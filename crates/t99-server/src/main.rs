//! t99-server：单局权威服务器（C/S 与 B/S 的 S 端）。
//!
//! - 一台服务器 = 一局对局；客户端用 ID+Key 准入（LLM Arena 中 ID 即测试者 ID）。
//! - 权威模拟在本机：人类席位经 WebSocket 上报输入，Bot 席位进程内驱动。
//! - 对客户端只广播权威事件流（hello/tick/result）——客户端本地镜像重演（确定性）。
//! - 观战：role=observer&spectate=N，与游玩客户端同协议、无输入权。
//! - 对局结束后所有人类席位请求 restart 或 8s 超时即开新局（新种子）。

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::handshake::server::{Request, Response};
use tungstenite::Message;

use t99_bots::BotAdapter;
use t99_core::engine::TargetPolicy;
use t99_core::{Key, Match, MatchConfig, Ruleset, TMS, TimedKey, Ticks, RULESET_ID};
use t99_proto::{hello_for, parse_seat, PlayerAdapter, ResultMsg, SeatProgram, SubprocessAdapter, FRAME_DEADLINE, MAX_CONSECUTIVE_MISS, PROTOCOL};

/// 逻辑帧墙钟周期由 ruleset.tick_hz 决定；引擎时基本身是 frame_ticks，这里只负责节流。
/// 读线程断开时投给写线程的收尾信号（JSON 文本帧不含 NUL，不会与业务消息冲突）
const MSG_CLOSE: &str = "\u{0}close";

#[derive(Clone)]
struct Args {
    address: String,
    port: u16,
    key: String,
    players: usize,
    bots: Vec<String>,
    seed: u64,
    max_frames: u64,
    /// 对局预算秒（0 = 按人数自动：4p 5min → 99p 15min）
    time_limit_secs: u64,
    /// 只读规则集；CLI 的 time-limit/stall-accel 仍可覆盖
    ruleset: Ruleset,
    /// 0 杀加速燃烧（展览机制；正式 Arena 关闭）
    stall_accel: bool,
    /// lobby 模式：席位由网络玩家（外部程序/提交程序/人类）接入，人数到齐自动开局；开局后加入无效。
    /// 与 --bots 同用时为混合局：前 human_seats 个席位由网络玩家随机占用，其余席位进程内/子进程 bot
    lobby: bool,
    /// 可选 PlayerID allowlist（逗号分隔）；不是席位顺序。缺省接受任意非空 ID。
    ids: Option<String>,
    loglevel: String,
    debug: bool,
    lobby_timeout: Duration,
    restart_timeout: Duration,
    write_timeout: Duration,
    auth_check: bool,
    auth_case_sensitive: bool,
    auth_files: Vec<String>,
    /// 结算时向 stderr 输出逐对送出/抵消矩阵
    dump_matrix: bool,
    /// 参考局：强制所有席位随机锁敌（忽略客户端目标键）
    force_random_target: bool,
    /// 评测：强制目标策略（优先于 force_random_target）
    force_target: Option<TargetPolicy>,
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
    // 布尔开关：`--x` 或 `--x 1` 开启，`--x 0`/`--x false` 关闭
    let flag = |name: &str| -> bool {
        match get(name) {
            Some(v) => v != "0" && v != "false",
            None => args.iter().any(|a| a == name),
        }
    };
    let cfg = t99_config::load_server(
        get("--config").as_deref().map(std::path::Path::new),
    )
    .unwrap_or_else(|e| {
        eprintln!("[config] {e}");
        std::process::exit(2);
    });
    let players: usize = get("--players")
        .and_then(|s| s.parse().ok())
        .unwrap_or(cfg.match_section.players);
    let no_lobby = args.iter().any(|a| a == "--no-lobby");
    let lobby = !no_lobby && (args.iter().any(|a| a == "--lobby") || cfg.match_section.lobby);
    // 缺省 bot 阵容只用于非 lobby 局；lobby 不给 --bots 时即纯网络局（全部席位网络玩家），
    // 显式给 --bots 时保留，形成混合局（前 human_seats 席网络，其余进程内/子进程 bot）
    let bots: Vec<String> = get("--bots")
        .unwrap_or_else(|| if lobby { String::new() } else { "advanced,advanced".into() })
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut seed = get("--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(cfg.match_section.seed);
    if seed == 0 {
        seed = random_seed();
        eprintln!("[seed] {seed}");
    }
    let rc = t99_config::load_ruleset(Some(std::path::Path::new(&cfg.ruleset_file)))
        .unwrap_or_else(|e| {
            eprintln!("[ruleset] {e}");
            std::process::exit(2);
        });
    if rc.ruleset.id != RULESET_ID {
        eprintln!(
            "[ruleset] id 不匹配: file={} compiled={}；修改规则集后必须同步重编 RULESET_ID",
            rc.ruleset.id, RULESET_ID
        );
        std::process::exit(2);
    }
    let ruleset = rc.to_ruleset().unwrap_or_else(|e| {
        eprintln!("[ruleset] {e}");
        std::process::exit(2);
    });
    let force_target = get("--force-target").map(|s| {
        parse_target_policy(&s).unwrap_or_else(|| {
            eprintln!(
                "[force-target] 未知策略 `{s}`；可选 random|kos|badges|spread|huntweak|avoidattackers|spreadsafe|spreadweak|randomtop|attackers"
            );
            std::process::exit(2);
        })
    });
    Args {
        address: get("--address").unwrap_or(cfg.server.address),
        port: get("--port").and_then(|s| s.parse().ok()).unwrap_or(cfg.server.port),
        key: get("--key").unwrap_or_else(|| "t99".into()),
        // Bot 数即席位下限：bot 覆盖全部席位时为纯 Bot 局（人类 0 席）
        players: players.max(bots.len()),
        bots,
        seed,
        max_frames: get("--max-frames").and_then(|s| s.parse().ok()).unwrap_or(54_000),
        time_limit_secs: get("--time-limit-secs").and_then(|s| s.parse().ok()).unwrap_or(0),
        ruleset,
        stall_accel: get("--stall-accel")
            .map(|s| s != "0" && s != "false")
            .unwrap_or(cfg.server.stalemate_accel),
        lobby,
        ids: get("--ids"),
        loglevel: get("--loglevel").unwrap_or(cfg.server.loglevel),
        debug: args.iter().any(|a| a == "--debug") || cfg.server.debug,
        lobby_timeout: Duration::from_secs(cfg.server.lobby_timeout_secs),
        restart_timeout: Duration::from_secs(cfg.server.restart_timeout_secs),
        write_timeout: Duration::from_secs(cfg.server.write_timeout_secs),
        auth_check: cfg.auth.check_player_id,
        auth_case_sensitive: cfg.auth.case_sensitive,
        auth_files: cfg.auth.id_files,
        dump_matrix: flag("--dump-matrix"),
        force_random_target: flag("--force-random-target"),
        force_target,
    }
}

fn parse_target_policy(s: &str) -> Option<TargetPolicy> {
    match s.to_ascii_lowercase().as_str() {
        "random" => Some(TargetPolicy::Random),
        "kos" => Some(TargetPolicy::Kos),
        "badges" => Some(TargetPolicy::Badges),
        "spread" => Some(TargetPolicy::Spread),
        "huntweak" => Some(TargetPolicy::HuntWeak),
        "avoidattackers" => Some(TargetPolicy::AvoidAttackers),
        "spreadsafe" => Some(TargetPolicy::SpreadSafe),
        "spreadweak" => Some(TargetPolicy::SpreadWeak),
        "randomtop" => Some(TargetPolicy::RandomTop),
        "attackers" => Some(TargetPolicy::Attackers),
        _ => None,
    }
}

fn random_seed() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    now ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

struct IdFilter {
    patterns: Vec<String>,
    case_sensitive: bool,
}

impl IdFilter {
    fn allows(&self, id: &str) -> bool {
        let id = if self.case_sensitive { id.to_string() } else { id.to_ascii_lowercase() };
        self.patterns.iter().any(|p| {
            let p = if self.case_sensitive { p.clone() } else { p.to_ascii_lowercase() };
            glob_match(p.as_bytes(), id.as_bytes())
        })
    }
}

fn glob_match(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        b'*' => glob_match(&p[1..], s) || (!s.is_empty() && glob_match(p, &s[1..])),
        b'?' => !s.is_empty() && glob_match(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob_match(&p[1..], &s[1..]),
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

/// 从空席位中随机选一个；PlayerID 只用于去重，不再决定席位顺序。
fn pick_seat(free: &[usize], salt: &str) -> usize {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h = now ^ 0x9e37_79b9_7f4a_7c15;
    for b in salt.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x1000_0000_01b3);
    }
    free[(h as usize) % free.len()]
}

/// 每连接的写线程：阻塞在 writer_rx 上逐条写出（含 hello/tick/result/pong）。
/// 退出条件：通道关闭（大厅登记与 NetPlayerAdapter 都 drop 了 tx）、收到 MSG_CLOSE、
/// 或写失败（对端消失/写超时）。
fn writer_loop(mut ws: tungstenite::WebSocket<std::net::TcpStream>, rx: mpsc::Receiver<String>) {
    while let Ok(msg) = rx.recv() {
        if msg == MSG_CLOSE {
            break;
        }
        if ws.send(Message::Text(msg.into())).is_err() {
            break;
        }
    }
    // 尽力关闭：对端已不读时 close/flush 可能失败，写超时兜底
    let _ = ws.close(None);
    let _ = ws.flush();
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
        let tick_hz = args.ruleset.tick_hz.max(1) as u64;
        let frame_ns = 1_000_000_000 / tick_hz;
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
        if match_no == 1 && human_seats > 0 {
            while lobby_start.elapsed() < Duration::from_secs(30) {
                let any = hall.lock().unwrap().clients.iter().flatten().next().is_some();
                if any {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        while lobby_start.elapsed() < args.lobby_timeout {
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
                                "type": "hello", "match": match_no, "match_id": match_id, "protocol": PROTOCOL, "ruleset": RULESET_ID, "seed": seed, "players": players, "tick_hz": tick_hz,
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
                    "type": "hello", "match": match_no, "match_id": match_id, "protocol": PROTOCOL, "ruleset": RULESET_ID, "seed": seed, "players": players, "tick_hz": tick_hz,
                    "seat": c.seat.map(|s| s as i64).unwrap_or(-1),
                    "player_id": c.id.clone(),
                    "role": if c.seat.is_some() { "player" } else { "observer" },
                    "time_limit_secs": args.time_limit_secs,
                    "stall_accel": args.stall_accel,
                });
                let _ = c.tx.send(hello.to_string());
            }
        }

        let mut ruleset = args.ruleset.clone();
        if args.time_limit_secs > 0 {
            ruleset.match_ticks = (args.time_limit_secs as Ticks) * 1000 * TMS;
        }
        ruleset.stall_accel = args.stall_accel;
        let mut engine = Match::new(MatchConfig { players, seed, ruleset });
        if let Some(policy) = args.force_target {
            engine.force_target_policy(policy);
        } else if args.force_random_target {
            engine.force_random_targets();
        }
        // 结算/握手用：seat -> PlayerID；bot 席位生成稳定占位 ID。
        let seat_ids: Vec<String> = {
            let h = hall.lock().unwrap();
            (0..players)
                .map(|pid| {
                    h.clients
                        .iter()
                        .flatten()
                        .find(|c| c.seat == Some(pid))
                        .map(|c| c.id.clone())
                        .unwrap_or_else(|| format!("bot-{pid}"))
                })
                .collect()
        };
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
                if let Err(e) = b.setup(&hello_for(pid, &seat_ids[pid], players, 6)) {
                    eprintln!("[seat {pid}] {} setup 失败: {e:?}", b.name());
                    engine.kill(pid, "setup_failed");
                }
            }
        }

        let mut next_inputs: HashMap<usize, Vec<TimedKey>> = HashMap::new();
        // 输入去重告警：每席位累计、限频输出
        let mut last_dup = vec![0u32; players];
        let mut dup_acc = vec![0u32; players];
        let mut last_warn: Vec<Option<Instant>> = vec![None; players];
        // ARRCheck 告警
        let mut last_arr = vec![0u32; players];
        let mut arr_acc = vec![0u32; players];
        let mut last_arr_warn: Vec<Option<Instant>> = vec![None; players];
        // 当前局的全部 tick（供中途加入的观战端回放）
        let mut history: Vec<String> = Vec::new();
        let mut in_match = true;
        // 墙钟节拍：绝对 deadline，避免 sleep 误差累积；逻辑帧本身仍是 16.6667ms
        let mut sched_origin = Instant::now();
        let mut frame_idx: u64 = 0;
        while in_match {
            let deadline = sched_origin
                + Duration::from_nanos(frame_idx.saturating_mul(1_000_000_000) / tick_hz);
            let now = Instant::now();
            if now < deadline {
                std::thread::sleep(deadline - now);
            } else if now.duration_since(deadline) > Duration::from_nanos(2 * frame_ns) {
                // 落后超过 2 帧：重置调度原点，避免追帧风暴；引擎仍只推进一帧
                sched_origin = now - Duration::from_nanos(frame_idx.saturating_mul(1_000_000_000) / tick_hz);
            }
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
                            "type": "hello", "match": match_no, "match_id": match_id, "protocol": PROTOCOL, "ruleset": RULESET_ID, "seed": seed, "players": players, "tick_hz": tick_hz,
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
            // 阶段一：所有异步席位先发本帧快照；进程内 bot 仍同步 act。
            let mut pending: Vec<usize> = Vec::new();
            for pid in 0..players {
                if !engine.players[pid].alive {
                    continue;
                }
                if let Some(b) = bots[pid].as_mut() {
                    let snap = engine.snapshot(pid);
                    if b.begin_frame(&snap) {
                        pending.push(pid);
                    } else {
                        inputs[pid] = b.act(&snap);
                    }
                } else if let Some(ev) = next_inputs.remove(&pid) {
                    inputs[pid] = ev;
                }
            }
            // 阶段二：共享 deadline 收集，等最慢的一席，而不是逐个等 RTT。
            if !pending.is_empty() {
                let deadline = Instant::now() + FRAME_DEADLINE;
                loop {
                    let mut still = Vec::with_capacity(pending.len());
                    for pid in pending.drain(..) {
                        let b = bots[pid].as_mut().expect("pending 席位必有 adapter");
                        match b.try_input() {
                            Some(ev) => inputs[pid] = ev,
                            None => still.push(pid),
                        }
                    }
                    pending = still;
                    if pending.is_empty() || Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
                for pid in pending.drain(..) {
                    inputs[pid] = bots[pid].as_mut().expect("pending 席位必有 adapter").end_frame();
                }
            }
            // 外部程序死亡/持续不应答：宿主强制判负
            for pid in 0..players {
                if let Some(b) = bots[pid].as_mut() {
                    if b.is_dead() || b.consecutive_misses() > MAX_CONSECUTIVE_MISS {
                        engine.kill(pid, "program_dead");
                    }
                }
            }
            engine.frame(&inputs);
            // 重复动作被 sanitize 丢弃时警告该席位（每席位最多 1 秒一次）
            for pid in 0..players {
                let total = engine.players[pid].dup_dropped;
                if total > last_dup[pid] {
                    dup_acc[pid] += total - last_dup[pid];
                    last_dup[pid] = total;
                }
                let due = last_warn[pid]
                    .map(|t: Instant| t.elapsed() >= Duration::from_secs(1))
                    .unwrap_or(true);
                if due && dup_acc[pid] > 0 {
                    let dropped = dup_acc[pid];
                    dup_acc[pid] = 0;
                    last_warn[pid] = Some(Instant::now());
                    eprintln!("[input] seat={pid} duplicate_actions dropped={dropped} total={total}");
                    let warn = serde_json::json!({
                        "type": "warn", "code": "duplicate_action", "dropped": dropped,
                    })
                    .to_string();
                    let h = hall.lock().unwrap();
                    if let Some(c) = h.clients.iter().flatten().find(|c| c.seat == Some(pid)) {
                        let _ = c.tx.send(warn);
                    }
                }
                // ARRCheck：同方向移动间隔不足
                let arr_total = engine.players[pid].arr_violations;
                if arr_total > last_arr[pid] {
                    arr_acc[pid] += arr_total - last_arr[pid];
                    last_arr[pid] = arr_total;
                }
                let arr_due = last_arr_warn[pid]
                    .map(|t: Instant| t.elapsed() >= Duration::from_secs(1))
                    .unwrap_or(true);
                if arr_due && arr_acc[pid] > 0 {
                    let dropped = arr_acc[pid];
                    arr_acc[pid] = 0;
                    last_arr_warn[pid] = Some(Instant::now());
                    eprintln!("[arr] seat={pid} dropped={dropped} total={arr_total}");
                    let warn = serde_json::json!({
                        "type": "warn", "code": "arr_violation", "dropped": dropped,
                    })
                    .to_string();
                    let h = hall.lock().unwrap();
                    if let Some(c) = h.clients.iter().flatten().find(|c| c.seat == Some(pid)) {
                        let _ = c.tx.send(warn);
                    }
                }
            }
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
            frame_idx += 1;
        }

        // 结算广播
        let reason = engine
            .over
            .as_ref()
            .map(|o| o.reason.to_string())
            .unwrap_or_else(|| "time_limit".into());
        let ranking = engine.final_ranking();
        let result = serde_json::json!({
            "type": "result", "frame": engine.frame_n, "reason": reason, "hash": engine.hash_state(),
            "places": ranking.iter().map(|r| serde_json::json!([r.pid, r.rank])).collect::<Vec<_>>(),
        });
        hall.lock().unwrap().broadcast(&result.to_string());

        // 服务器结算：stdout 一行 JSON，机器可读；stderr 保留人类日志。
        let out = serde_json::json!({
            "type": "match_result",
            "match_id": match_id,
            "seed": seed,
            "reason": reason,
            "protocol": PROTOCOL,
            "ruleset": RULESET_ID,
            "ranking": ranking.iter().map(|r| serde_json::json!({
                "player_id": seat_ids[r.pid],
                "seat": r.pid,
                "rank": r.rank,
                "badges": r.badges,
                "alive": r.alive,
                "ko": r.ko,
                "lines": r.lines,
                "sent": r.sent,
                "input_dropped": r.dup_dropped,
                "arr_violations": r.arr_violations,
                "dropped": r.dropped,
                "canceled": r.canceled,
                "inserted": r.inserted,
                "peak_pending": r.peak_pending,
                "pieces": r.pieces,
                "eliminated_at_frame": r.death_frame,
            })).collect::<Vec<_>>(),
        });
        // 逐对送出/抵消矩阵（stderr，仅观测；match_result 仍只写 stdout 一行）
        if args.dump_matrix {
            for (pid, p) in engine.players.iter().enumerate() {
                for (rid, &rows) in p.sent_to.iter().enumerate() {
                    if rows > 0 {
                        eprintln!("[matrix-sent] from={pid} to={rid} rows={rows}");
                    }
                }
                for (src, &rows) in p.canceled_from.iter().enumerate() {
                    if rows > 0 {
                        eprintln!("[matrix-cancel] by={pid} from={src} rows={rows}");
                    }
                }
            }
        }

        println!("{out}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        // 结算下发 + 外部子进程收尾（等待退出，超时击杀）
        for (pid, b) in bots.iter_mut().enumerate() {
            if let Some(b) = b {
                let p = &engine.players[pid];
                let row = ranking.iter().find(|r| r.pid == pid);
                let msg = ResultMsg {
                    kind: "result",
                    n: engine.frame_n,
                    seat: pid,
                    player_id: seat_ids[pid].clone(),
                    place: p.place,
                    rank: row.map(|r| r.rank).unwrap_or(0),
                    badges: p.badges,
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

        // 单局驻留：不再开新局；观战端晚到仍可补史看到最终局面。
        eprintln!("[server] 单局结束，服务器驻留现场（保持连接与补史；Ctrl-C 退出）");
        loop {
            while let Ok(m) = rx.try_recv() {
                match m {
                    ToGame::ObserverJoin { idx } => {
                        let hello = serde_json::json!({
                            "type": "hello", "match": match_no, "match_id": match_id, "protocol": PROTOCOL, "ruleset": RULESET_ID, "seed": seed, "players": players, "tick_hz": tick_hz,
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
        "t99-server: {}:{} loglevel={} debug={} key={} 席位 {}（人类 {} + Bot {}）种子 {} 预算 {} 0杀加速 {} restart_timeout={}s{}",
        args.address,
        args.port,
        args.loglevel,
        args.debug,
        args.key,
        args.players,
        human_seats,
        args.bots.len(),
        args.seed,
        if args.time_limit_secs > 0 { format!("{}s", args.time_limit_secs) } else { "auto".into() },
        if args.stall_accel { "开" } else { "关" },
        args.restart_timeout.as_secs(),
        if args.lobby { " [lobby]" } else { "" },
    );
    // 可选 allowlist；没有名册顺序：席位随机分配，PlayerID 只负责唯一性。
    let mut patterns: Vec<String> = args
        .ids
        .as_ref()
        .map(|list| {
            list.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if args.lobby && patterns.is_empty() && args.auth_check {
        for f in &args.auth_files {
            let text = std::fs::read_to_string(f).unwrap_or_else(|e| {
                eprintln!("[auth] 读取 {f} 失败: {e}");
                std::process::exit(2);
            });
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                patterns.push(line.to_string());
            }
        }
        if patterns.is_empty() {
            eprintln!("[auth] check_player_id=true 但没有有效 pattern；拒绝启动");
            std::process::exit(2);
        }
    }
    let allowed: Option<Arc<IdFilter>> = if args.lobby && !patterns.is_empty() {
        Some(Arc::new(IdFilter {
            patterns,
            case_sensitive: args.auth_case_sensitive,
        }))
    } else {
        None
    };
    if args.lobby {
        match &allowed {
            Some(a) => eprintln!("[auth] {} patterns；席位随机分配", a.patterns.len()),
            None => eprintln!("[auth] 无 allowlist，接受任意非空 PlayerID；席位随机分配"),
        }
    }
    let hall: Arc<Mutex<Hall>> = Arc::new(Mutex::new(Hall::default()));
    let (to_game_tx, to_game_rx) = mpsc::channel::<ToGame>();
    let next_client = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicBool::new(false)); // 对局进行中（限制中途加入）

    {
        let hall = hall.clone();
        let args = args.clone();
        let running = running.clone();
        std::thread::spawn(move || {
            let hs = args.players - args.bots.len();
            game_loop(args, hall, to_game_rx, hs, running);
        });
    }

    let listener = TcpListener::bind((args.address.as_str(), args.port)).expect("绑定端口失败");
    let write_timeout = args.write_timeout;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let hall = hall.clone();
        let to_game = to_game_tx.clone();
        let next_client = next_client.clone();
        let key = args.key.clone();
        let human_seats = human_seats;
        let running = running.clone();
        let lobby = args.lobby;
        let allowed = allowed.clone();
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
                if let Some(a) = &allowed {
                    if !a.allows(&id) {
                        err(&mut ws, "id_invalid");
                        eprintln!("[auth] 拒绝不在 allowlist 的 ID: {id}");
                        return;
                    }
                }
            }
            if role != "observer" && running.load(Ordering::SeqCst) {
                err(&mut ws, "match_running");
                return;
            }
            let mut inbound_tx: Option<mpsc::Sender<String>> = None;
            // 注册 + 席位分配必须原子（否则并发连接会分到同一席位）
            let (tx, writer_rx) = mpsc::channel::<String>();
            // 读线程自用的发送端（Pong 回显 + 断开时的收尾信号都走写线程）
            let tx_reader = tx.clone();
            let want_restart = Arc::new(AtomicBool::new(false));
            let (idx, seat) = {
                let mut h = hall.lock().unwrap();
                if h.clients.iter().flatten().any(|c| c.id == id) {
                    drop(h);
                    err(&mut ws, "id_duplicate");
                    eprintln!("[auth] 拒绝重复 PlayerID: {id}");
                    return;
                }
                let seat = if role == "observer" {
                    None
                } else {
                    let taken: Vec<usize> = h.clients.iter().flatten().filter_map(|c| c.seat).collect();
                    let free: Vec<usize> = (0..human_seats).filter(|s| !taken.contains(s)).collect();
                    if free.is_empty() {
                        None
                    } else {
                        Some(pick_seat(&free, &id))
                    }
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
                    serde_json::json!({"type":"seat","seat":s,"player_id":id.clone(),"spectate":spectate}).to_string().into(),
                ));
            } else {
                let _ = ws.send(Message::Text(
                    serde_json::json!({"type":"seat","seat":-1,"spectate":spectate}).to_string().into(),
                ));
            }
            // 读/写分线程：读线程阻塞在 ws.read()（无读超时），写线程独占克隆 socket 的
            // WebSocket 并阻塞在通道 recv 上。旧实现靠 2ms 读超时轮询顺带排空写队列，
            // 99 连接 ≈ 5 万次/秒唤醒，纯空转吃 CPU，却换不来吞吐（TPS 瓶颈是
            // NetPlayerAdapter::act 的串行等待，不是这里）。
            let writer_ws = match ws.get_ref().try_clone() {
                Ok(s) => {
                    // 对端不读时 send 不无限阻塞（写超时兜底写线程收尾）
                    s.set_write_timeout(Some(write_timeout)).ok();
                    tungstenite::WebSocket::from_raw_socket(
                        s,
                        tungstenite::protocol::Role::Server,
                        None,
                    )
                }
                Err(e) => {
                    eprintln!("[conn] {id} 写线程建流失败: {e}");
                    if let Some(slot) = hall.lock().unwrap().clients.get_mut(idx) {
                        *slot = None;
                    }
                    let _ = to_game.send(ToGame::Left { idx });
                    return;
                }
            };
            std::thread::spawn(move || writer_loop(writer_ws, writer_rx));
            // 读线程：只读上行，下行（帧/pong）一律投给写线程，避免两条线程写同一 socket
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
                            // ping 仍回显（低延迟测量），但走写线程
                            if v["type"] == "ping" {
                                let _ = tx_reader.send(
                                    serde_json::json!({"type": "pong", "t": v["t"]}).to_string(),
                                );
                            }
                            continue;
                        }
                        match v["type"].as_str().unwrap_or("") {
                            "ping" => {
                                let _ = tx_reader.send(
                                    serde_json::json!({"type": "pong", "t": v["t"]}).to_string(),
                                );
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
                    Err(_) => break,
                }
            }
            // 断开：先摘除大厅登记（drop 广播 tx）再通知写线程收尾，避免线程滞留
            if let Some(slot) = hall.lock().unwrap().clients.get_mut(idx) {
                *slot = None;
            }
            let _ = tx_reader.send(MSG_CLOSE.to_string());
            let _ = to_game.send(ToGame::Left { idx });
            eprintln!("[disc] {id}");
        });
    }
}
