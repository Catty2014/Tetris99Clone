//! t99-server：单局权威服务器（C/S 与 B/S 的 S 端）。
//!
//! - 一台服务器 = 一局对局；客户端用 ID+Key 准入（LLM Arena 中 ID 即测试者 ID）。
//! - 权威模拟在本机：人类席位经 WebSocket 上报输入，Bot 席位进程内驱动。
//! - 对客户端只广播权威事件流（hello/tick/result）——客户端本地镜像重演（确定性）。
//! - 观战：role=observer&spectate=N，与游玩客户端同协议、无输入权。
//! - 对局结束后所有人类席位请求 restart 或 8s 超时即开新局（新种子）。

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::handshake::server::{Request, Response};
use tungstenite::Message;

use t99_bots::BotAdapter;
use t99_core::{Key, Match, MatchConfig, Ruleset, TMS, TimedKey, Ticks, ENGINE_SEMANTICS};
use t99_proto::{hello_for, parse_seat, PlayerAdapter, ResultMsg, SeatProgram, SubprocessAdapter, MAX_CONSECUTIVE_MISS};

const FRAME_MS: u64 = 16;
const RESTART_TIMEOUT: Duration = Duration::from_secs(8);
const LOBBY_TIMEOUT: Duration = Duration::from_secs(10);
/// 结算展示期：结果广播后至少保留此时长再开下一局
const RESULT_HOLD: Duration = Duration::from_secs(5);

struct Args {
    port: u16,
    key: String,
    players: usize,
    bots: Vec<String>,
    seed: u64,
    max_frames: u64,
    /// 对局预算秒（0 = 按人数自动：4p 5min → 99p 15min）
    time_limit_secs: u64,
    /// 0 杀加速燃烧（展览机制；正式 Arena 关闭）
    stall_accel: bool,
    /// lobby 模式：席位由网络玩家（外部程序/提交程序/人类）接入，人数到齐自动开局；开局后加入无效。
    /// 与 --bots 同用时为混合局：0..human_seats 席网络玩家，其余席位进程内/子进程 bot
    lobby: bool,
    /// lobby 名册（逗号分隔，长度=席位数）；缺省自动生成 p<i>-<hash>
    ids: Option<String>,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let get = |name: &str| -> Option<String> {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == name {
                return it.next().cloned();
            }
        }
        None
    };
    let players: usize = get("--players").and_then(|s| s.parse().ok()).unwrap_or(4);
    let lobby = args.iter().any(|a| a == "--lobby");
    // 缺省 bot 阵容只用于非 lobby 局；lobby 不给 --bots 时即纯网络局（全部席位网络玩家），
    // 显式给 --bots 时保留，形成混合局（前 human_seats 席网络，其余进程内/子进程 bot）
    let bots: Vec<String> = get("--bots")
        .unwrap_or_else(|| if lobby { String::new() } else { "advanced,advanced".into() })
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Args {
        port: get("--port").and_then(|s| s.parse().ok()).unwrap_or(8138),
        key: get("--key").unwrap_or_else(|| "t99".into()),
        // Bot 数即席位下限：bot 覆盖全部席位时为纯 Bot 局（人类 0 席）
        players: players.max(bots.len()),
        bots,
        seed: get("--seed").and_then(|s| s.parse().ok()).unwrap_or(42),
        max_frames: get("--max-frames").and_then(|s| s.parse().ok()).unwrap_or(54_000),
        time_limit_secs: get("--time-limit-secs").and_then(|s| s.parse().ok()).unwrap_or(0),
        stall_accel: get("--stall-accel").map(|s| s != "0" && s != "false").unwrap_or(true),
        lobby,
        ids: get("--ids"),
    }
}

struct Client {
    id: String,
    seat: Option<usize>,
    tx: mpsc::Sender<String>,
    want_restart: Arc<AtomicBool>,
    /// 观战端补史完成前不接收直播帧（game_loop 单线程回放后激活，保证时序）
    active: bool,
    /// lobby 玩家：原始上行通道（hello 应答/frame 应答），由 NetPlayerAdapter 消费
    inbound_rx: Option<mpsc::Receiver<String>>,
}

#[derive(Default)]
struct Hall {
    clients: Vec<Option<Client>>,
}

impl Hall {
    fn broadcast(&self, msg: &str) {
        for c in self.clients.iter().flatten() {
            if c.active {
                let _ = c.tx.send(msg.to_string());
            }
        }
    }

    fn player_count(&self) -> usize {
        self.clients.iter().flatten().filter(|c| c.seat.is_some()).count()
    }
}

enum ToGame {
    Input { seat: usize, events: Vec<TimedKey> },
    Restart { idx: usize },
    Left { idx: usize },
    /// 观战端中途加入：回放当前局 hello + 全部 tick 后激活
    ObserverJoin { idx: usize },
}

fn event_json(t: &TimedKey) -> serde_json::Value {
    match t.key {
        Key::TargetPlayer(v) => serde_json::json!([t.t_us, t.key.name(), v]),
        _ => serde_json::json!([t.t_us, t.key.name()]),
    }
}

fn game_loop(args: Args, hall: Arc<Mutex<Hall>>, rx: mpsc::Receiver<ToGame>, human_seats: usize, running: Arc<AtomicBool>) {
    use t99_proto::NetPlayerAdapter;
    // 单局服务器：一台服务器只服务一局（统计/调试口径唯一）；对局结束后驻留现场，
    // 保留 history 供观战端晚到补史，客户端保持最终画面。多局循环以后按需再做。
    let match_no: u64 = 1;
    let mut seed = args.seed;
    'match_one: loop {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(match_no | 1);
        let players = args.players;
        // 对局 ID（雪花式：毫秒时间戳 << 12 | 局号）
        let match_id = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
            << 12)
            | (match_no & 0xFFF);

        // 大厅：等人类席位坐满或超时
        let lobby_start = Instant::now();
        // 首局再等首个客户端（含观战）入场，至多 30s：观战端先就位再开局。
        // 注意：此处不可盲清空 rx——宽限期内加入的观战端依赖 ObserverJoin 激活补史。
        if match_no == 1 {
            while lobby_start.elapsed() < Duration::from_secs(30) {
                let any = hall.lock().unwrap().clients.iter().flatten().next().is_some();
                if any {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        while lobby_start.elapsed() < LOBBY_TIMEOUT {
            let filled = hall
                .lock()
                .unwrap()
                .clients
                .iter()
                .flatten()
                .filter(|c| c.seat.is_some())
                .count();
            if filled >= human_seats {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // lobby 模式：等待 human_seats 个网络席位坐满（开局后加入无效）；其余席位由 bot 填充
        let mut net_bots: Vec<Option<NetPlayerAdapter>> = vec![];
        if args.lobby {
            loop {
                {
                    let h = hall.lock().unwrap();
                    let seated = h.clients.iter().flatten().filter(|c| c.seat.is_some()).count();
                    if seated >= human_seats {
                        break;
                    }
                }
                // 等待期泵 rx：观战端中途加入照常补史（history 为空则直接激活）
                while let Ok(m) = rx.try_recv() {
                    match m {
                        ToGame::ObserverJoin { idx } => {
                            let hello = serde_json::json!({
                                "type": "hello", "match": match_no, "match_id": match_id, "engine_rev": ENGINE_SEMANTICS, "seed": seed, "players": players,
                                "seat": -1, "role": "observer",
                                "time_limit_secs": args.time_limit_secs, "stall_accel": args.stall_accel,
                            });
                            if let Some(Some(c)) = hall.lock().unwrap().clients.get(idx) {
                                let _ = c.tx.send(hello.to_string());
                            }
                            if let Some(Some(c)) = hall.lock().unwrap().clients.get_mut(idx) {
                                c.active = true;
                            }
                        }
                        ToGame::Left { idx } => {
                            if let Some(slot) = hall.lock().unwrap().clients.get_mut(idx) {
                                *slot = None;
                            }
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            running.store(true, Ordering::SeqCst);
            // 按席位构建网络玩家适配器（仅前 human_seats 席）
            net_bots = {
                let mut h = hall.lock().unwrap();
                (0..human_seats)
                    .map(|pid| {
                        let client = h.clients.iter_mut().flatten().find(|c| c.seat == Some(pid));
                        client.map(|c| {
                            let rx = c.inbound_rx.take().expect("lobby 席位必有 inbound");
                            NetPlayerAdapter::new(format!("net:{}", c.id), c.tx.clone(), rx)
                        })
                    })
                    .collect()
            };
        }
        running.store(true, Ordering::SeqCst);
        // 每客户端下发专属 hello
        {
            let h = hall.lock().unwrap();
            for c in h.clients.iter().flatten() {
                let hello = serde_json::json!({
                    "type": "hello", "match": match_no, "match_id": match_id, "engine_rev": ENGINE_SEMANTICS, "seed": seed, "players": players,
                    "seat": c.seat.map(|s| s as i64).unwrap_or(-1),
                    "role": if c.seat.is_some() { "player" } else { "observer" },
                    "time_limit_secs": args.time_limit_secs,
                    "stall_accel": args.stall_accel,
                });
                let _ = c.tx.send(hello.to_string());
            }
        }

        let mut ruleset = Ruleset::default();
        ruleset.match_ticks = if args.time_limit_secs > 0 { (args.time_limit_secs as Ticks) * 1000 * TMS } else { 0 };
        ruleset.stall_accel = args.stall_accel;
        let mut engine = Match::new(MatchConfig { players, seed, ruleset });
        // 席位适配器：前 human_seats 席 = 网络玩家（lobby），其余 = 内建 bot / 外部子进程
        let mut net_iter = net_bots.into_iter();
        let mut bots: Vec<Option<Box<dyn PlayerAdapter>>> = (0..players)
            .map(|pid| {
                if args.lobby && pid < human_seats {
                    net_iter.next().flatten().map(|b| Box::new(b) as Box<dyn PlayerAdapter>)
                } else if pid >= human_seats {
                    args.bots.get(pid - human_seats).map(|spec| -> Box<dyn PlayerAdapter> {
                        match parse_seat(spec) {
                            SeatProgram::BuiltIn(tier) => Box::new(BotAdapter::new(&tier)),
                            SeatProgram::Ext { label, path } => Box::new(SubprocessAdapter::new(format!("ext:{label}"), vec![path])),
                        }
                    })
                } else {
                    None
                }
            })
            .collect();
        // 外部程序握手：ready 超时/拒绝 → 直接判负（与正式 Arena 语义一致）
        for (pid, b) in bots.iter_mut().enumerate() {
            if let Some(b) = b {
                if let Err(e) = b.setup(&hello_for(pid, players, 6)) {
                    eprintln!("[seat {pid}] {} setup 失败: {e:?}", b.name());
                    engine.kill(pid, "setup_failed");
                }
            }
        }

        let mut next_inputs: HashMap<usize, Vec<TimedKey>> = HashMap::new();
        // 当前局的全部 tick（供中途加入的观战端回放）
        let mut history: Vec<String> = Vec::new();
        let mut in_match = true;
        while in_match {
            let t0 = Instant::now();
            // 消息并入下一帧
            while let Ok(m) = rx.try_recv() {
                match m {
                    ToGame::Input { seat, events } => {
                        next_inputs.entry(seat).or_default().extend(events);
                    }
                    ToGame::Restart { idx } => {
                        let h = hall.lock().unwrap();
                        if let Some(Some(c)) = h.clients.get(idx) {
                            c.want_restart.store(true, Ordering::SeqCst);
                        }
                    }
                    ToGame::Left { idx } => {
                        if let Some(slot) = hall.lock().unwrap().clients.get_mut(idx) {
                            *slot = None;
                        }
                    }
                    ToGame::ObserverJoin { idx } => {
                        // 单线程回放：先 hello + 全部历史 tick，再激活直播，时序必然正确
                        let hello = serde_json::json!({
                            "type": "hello", "match": match_no, "match_id": match_id, "engine_rev": ENGINE_SEMANTICS, "seed": seed, "players": players,
                            "seat": -1, "role": "observer",
                            "time_limit_secs": args.time_limit_secs, "stall_accel": args.stall_accel,
                        });
                        {
                            let h = hall.lock().unwrap();
                            if let Some(Some(c)) = h.clients.get(idx) {
                                let _ = c.tx.send(hello.to_string());
                                for t in &history {
                                    if c.tx.send(t.clone()).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(Some(c)) = hall.lock().unwrap().clients.get_mut(idx) {
                            c.active = true;
                        }
                        eprintln!("[backfill] idx={idx} frames={}", history.len());
                    }
                }
            }
            let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; players];
            for pid in 0..players {
                if !engine.players[pid].alive {
                    continue;
                }
                if let Some(b) = bots[pid].as_mut() {
                    let snap = engine.snapshot(pid);
                    inputs[pid] = b.act(&snap);
                    // 外部程序死亡/持续不应答：宿主强制判负
                    if b.is_dead() || b.consecutive_misses() > MAX_CONSECUTIVE_MISS {
                        engine.kill(pid, "program_dead");
                    }
                } else if let Some(ev) = next_inputs.remove(&pid) {
                    inputs[pid] = ev;
                }
            }
            engine.frame(&inputs);
            let tick = serde_json::json!({
                "type": "tick", "n": engine.frame_n,
                "in": inputs.iter().enumerate()
                    .filter(|(_, ev)| !ev.is_empty())
                    .map(|(pid, ev)| serde_json::json!([pid, ev.iter().map(event_json).collect::<Vec<_>>()]))
                    .collect::<Vec<_>>(),
            });
            let tick_str = tick.to_string();
            history.push(tick_str.clone());
            hall.lock().unwrap().broadcast(&tick_str);

            if engine.over.is_some() || engine.frame_n >= args.max_frames {
                in_match = false;
            }
            let spent = t0.elapsed();
            if spent < Duration::from_millis(FRAME_MS) {
                std::thread::sleep(Duration::from_millis(FRAME_MS) - spent);
            }
        }

        // 结算广播
        let reason = engine
            .over
            .as_ref()
            .map(|o| o.reason.to_string())
            .unwrap_or_else(|| "time_limit".into());
        let mut place_map = vec![None; players];
        for row in engine.final_table() {
            place_map[row.pid] = row.place;
        }
        if engine.over.is_none() {
            // 流局：幸存者仅按徽章数排名
            let mut surv: Vec<usize> = (0..players).filter(|&p| engine.players[p].alive).collect();
            surv.sort_by(|&a, &b| engine.players[b].badges.cmp(&engine.players[a].badges).then(a.cmp(&b)));
            for (i, &pid) in surv.iter().enumerate() {
                if place_map[pid].is_none() {
                    place_map[pid] = Some(i as u32 + 1);
                }
            }
        }
        let result = serde_json::json!({
            "type": "result", "frame": engine.frame_n, "reason": reason, "hash": engine.hash_state(),
            "places": (0..players).filter_map(|p| place_map[p].map(|pl| serde_json::json!([p, pl]))).collect::<Vec<_>>(),
        });
        hall.lock().unwrap().broadcast(&result.to_string());
        // 结算下发 + 外部子进程收尾（等待退出，超时击杀）
        for (pid, b) in bots.iter_mut().enumerate() {
            if let Some(b) = b {
                let p = &engine.players[pid];
                let msg = ResultMsg {
                    kind: "result",
                    n: engine.frame_n,
                    place: p.place,
                    players,
                    ko: p.ko,
                    lines: p.lines,
                    sent: p.sent,
                    pieces: p.pieces,
                    reason: reason.clone(),
                    alive: p.alive,
                };
                b.finish(&msg);
            }
        }
        running.store(false, Ordering::SeqCst);
        eprintln!(
            "[match {match_no}] 结束: {reason} @ 帧 {} places={}",
            engine.frame_n,
            result["places"].as_array().map(|a| a.iter().map(|p| format!("{}→{}", p[0], p[1])).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        );
        eprintln!(
            "[match {match_no}] 摘要: {}",
            (0..players.min(12))
                .map(|p| format!("p{}:块{}行{}送{}", p, engine.players[p].pieces, engine.players[p].lines, engine.players[p].sent))
                .collect::<Vec<_>>()
                .join(" ")
        );

        // 结算展示期后驻留：不再开新局；观战端晚到仍可补史看到最终局面
        std::thread::sleep(RESULT_HOLD);
        eprintln!("[server] 单局结束，服务器驻留现场（保持连接与补史；Ctrl-C 退出）");
        loop {
            while let Ok(m) = rx.try_recv() {
                match m {
                    ToGame::ObserverJoin { idx } => {
                        let hello = serde_json::json!({
                            "type": "hello", "match": match_no, "match_id": match_id, "engine_rev": ENGINE_SEMANTICS, "seed": seed, "players": players,
                            "seat": -1, "role": "observer",
                            "time_limit_secs": args.time_limit_secs, "stall_accel": args.stall_accel,
                        });
                        {
                            let h = hall.lock().unwrap();
                            if let Some(Some(c)) = h.clients.get(idx) {
                                let _ = c.tx.send(hello.to_string());
                                for t in &history {
                                    if c.tx.send(t.clone()).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(Some(c)) = hall.lock().unwrap().clients.get_mut(idx) {
                            c.active = true;
                        }
                        eprintln!("[backfill-final] idx={idx} frames={}", history.len());
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn main() {
    let args = parse_args();
    let human_seats = args.players - args.bots.len();
    eprintln!(
        "t99-server: 端口 {} key={} 席位 {}（人类 {} + Bot {}）种子 {} 预算 {} 0杀加速 {}{}",
        args.port,
        args.key,
        args.players,
        human_seats,
        args.bots.len(),
        args.seed,
        if args.time_limit_secs > 0 { format!("{}s", args.time_limit_secs) } else { "auto".into() },
        if args.stall_accel { "开" } else { "关" },
        if args.lobby { " [lobby]" } else { "" },
    );
    // lobby 名册：合法 ID 集合（连接时校验、一次性领取；计分周期的身份锚点）
    let roster: Arc<Mutex<std::collections::HashSet<String>>> = Arc::new(Mutex::new(
        if args.lobby {
            match &args.ids {
                Some(list) => list.split(',').map(|x| x.trim().to_string()).collect(),
                None => (0..human_seats)
                    .map(|i| {
                        let h = &format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as u64 + (i as u64) * 0x9E37);
                        format!("p{i}-{}", &h[..6.min(h.len())])
                    })
                    .collect(),
            }
        } else {
            Default::default()
        },
    ));
    if args.lobby {
        {
            let r = roster.lock().unwrap();
            let ids: Vec<&String> = r.iter().collect();
            eprintln!("[roster] key={} 席位数 {} 合法 ID：", args.key, ids.len());
            for id in ids {
                eprintln!("  {id}");
            }
        }
        // 名册长度必须等于网络席位数，否则永远等不齐（少）或有人领不到位（多）
        let n_ids = roster.lock().unwrap().len();
        if n_ids != human_seats {
            eprintln!("[roster] 警告：名册 {} 个 ID ≠ 网络席位 {}（human_seats），lobby 将无法开局", n_ids, human_seats);
        }
    }
    let hall: Arc<Mutex<Hall>> = Arc::new(Mutex::new(Hall::default()));
    let (to_game_tx, to_game_rx) = mpsc::channel::<ToGame>();
    let next_client = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicBool::new(false)); // 对局进行中（限制中途加入）

    {
        let hall = hall.clone();
        let args = Args { port: args.port, key: args.key.clone(), players: args.players, bots: args.bots.clone(), seed: args.seed, max_frames: args.max_frames, time_limit_secs: args.time_limit_secs, stall_accel: args.stall_accel, lobby: args.lobby, ids: args.ids.clone() };
        let running = running.clone();
        std::thread::spawn(move || {
            let hs = args.players - args.bots.len();
            game_loop(args, hall, to_game_rx, hs, running);
        });
    }

    let listener = TcpListener::bind(("0.0.0.0", args.port)).expect("绑定端口失败");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let hall = hall.clone();
        let to_game = to_game_tx.clone();
        let next_client = next_client.clone();
        let key = args.key.clone();
        let human_seats = human_seats;
        let running = running.clone();
        let lobby = args.lobby;
        let roster = roster.clone();
        std::thread::spawn(move || {
            stream.set_nodelay(true).ok();
            let mut query = String::new();
            let cb = |req: &Request, resp: Response| {
                query = req.uri().to_string();
                Ok(resp)
            };
            let mut ws = match tungstenite::accept_hdr(stream, cb) {
                Ok(w) => w,
                Err(_) => return,
            };
            // 解析 query: /ws?id=..&key=..&role=..&spectate=..
            let mut id = String::new();
            let mut qkey = String::new();
            let mut role = "player".to_string();
            let mut spectate = 0usize;
            for pair in query.split('?').nth(1).unwrap_or("").split('&') {
                let mut kv = pair.splitn(2, '=');
                let k = kv.next().unwrap_or("");
                let v = kv.next().unwrap_or("");
                let v = v.replace("%20", " ");
                match k {
                    "id" => id = v,
                    "key" => qkey = v,
                    "role" => role = v,
                    "spectate" => spectate = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let err = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, code: &str| {
                let _ = ws.send(Message::Text(format!(r#"{{"type":"error","code":"{code}"}}"#).into()));
            };
            // 观战端只需 key；玩家席位需要 key + 合法未领取的 ID（lobby 模式）
            if qkey != key || (role != "observer" && id.is_empty()) {
                err(&mut ws, "auth");
                return;
            }
            if role != "observer" && lobby {
                let mut r = roster.lock().unwrap();
                if !r.remove(&id) {
                    err(&mut ws, "id_invalid");
                    eprintln!("[roster] 拒绝非法/重复 ID: {id}");
                    return;
                }
                eprintln!("[roster] 领取: {id}");
            }
            if role != "observer" && running.load(Ordering::SeqCst) {
                err(&mut ws, "match_running");
                return;
            }
            let mut inbound_tx: Option<mpsc::Sender<String>> = None;
            // 注册 + 席位分配必须原子（否则并发连接会分到同一席位）
            let (tx, writer_rx) = mpsc::channel::<String>();
            let want_restart = Arc::new(AtomicBool::new(false));
            let (idx, seat) = {
                let mut h = hall.lock().unwrap();
                let seat = if role == "observer" {
                    None
                } else {
                    let taken: Vec<usize> = h.clients.iter().flatten().filter_map(|c| c.seat).collect();
                    (0..human_seats).find(|s| !taken.contains(s))
                };
                if role != "observer" && seat.is_none() {
                    drop(h);
                    err(&mut ws, "no_seat");
                    return;
                }
                let idx = next_client.fetch_add(1, Ordering::SeqCst);
                while h.clients.len() <= idx {
                    h.clients.push(None);
                }
                // lobby 玩家席位：建立原始上行通道（NetPlayerAdapter 消费）
                let inbound_rx = if seat.is_some() && lobby {
                    let (itx, irx) = mpsc::channel::<String>();
                    inbound_tx = Some(itx);
                    Some(irx)
                } else {
                    None
                };
                h.clients[idx] = Some(Client {
                    id: id.clone(),
                    seat,
                    tx,
                    want_restart: want_restart.clone(),
                    // 观战端等补史后再激活；玩家席位本就在开局前入场
                    active: seat.is_some(),
                    inbound_rx,
                });
                (idx, seat)
            };
            eprintln!("[conn] {id} seat={:?} spectate={spectate}", seat);
            if seat.is_none() {
                // 观战端：交给 game_loop 单线程回放当前局（hello + 历史 tick）后再接直播
                let _ = to_game.send(ToGame::ObserverJoin { idx });
            }
            if let Some(s) = seat {
                let _ = ws.send(Message::Text(
                    serde_json::json!({"type":"seat","seat":s,"spectate":spectate}).to_string().into(),
                ));
            } else {
                let _ = ws.send(Message::Text(
                    serde_json::json!({"type":"seat","seat":-1,"spectate":spectate}).to_string().into(),
                ));
            }
            // 读写轮询：读超时即写泵粒度——玩家席位 60Hz 帧下发要求 ≤2ms 排空
            ws.get_ref().set_read_timeout(Some(Duration::from_millis(2))).ok();
            loop {
                match ws.read() {
                    Ok(Message::Text(t)) => {
                        let v: serde_json::Value = match serde_json::from_str(&t) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        // lobby 玩家：上行全部原样交给 NetPlayerAdapter（ready/input/result）
                        if let Some(itx) = &inbound_tx {
                            let _ = itx.send(t.to_string());
                            if let Ok(_) = serde_json::from_str::<serde_json::Value>(&t) {
                                // ping 仍就地回显（低延迟测量）
                                if v["type"] == "ping" {
                                    let _ = ws.send(Message::Text(
                                        serde_json::json!({"type": "pong", "t": v["t"]}).to_string().into(),
                                    ));
                                }
                            }
                            continue;
                        }
                        match v["type"].as_str().unwrap_or("") {
                            "ping" => {
                                let _ = ws.send(Message::Text(
                                    serde_json::json!({"type": "pong", "t": v["t"]}).to_string().into(),
                                ));
                            }
                            "input" => {
                                if let Some(s) = seat {
                                    let evs: Vec<TimedKey> = v["events"]
                                        .as_array()
                                        .map(|a| {
                                            a.iter()
                                                .filter_map(|e| {
                                                    let t = e.get(0)?.as_u64()? as u32;
                                                    let k = e.get(1)?.as_str()?;
                                                    let key = if k == "target_player" {
                                                        Key::TargetPlayer(e.get(2)?.as_u64()? as u16)
                                                    } else {
                                                        Key::from_name(k)?
                                                    };
                                                    Some(TimedKey { t_us: t, key })
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    if to_game.send(ToGame::Input { seat: s, events: evs }).is_err() {
                                        return;
                                    }
                                }
                            }
                            "restart" => {
                                let _ = to_game.send(ToGame::Restart { idx });
                                want_restart.store(true, Ordering::SeqCst);
                            }
                            _ => {}
                        }
                    }
                    Ok(Message::Ping(d)) => {
                        let _ = ws.send(Message::Pong(d));
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
                while let Ok(msg) = writer_rx.try_recv() {
                    if ws.send(Message::Text(msg.into())).is_err() {
                        break;
                    }
                }
            }
            let _ = to_game.send(ToGame::Left { idx });
            eprintln!("[disc] {id}");
        });
    }
}
