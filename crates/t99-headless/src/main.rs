//! 无头主机：run（对局）/ replay（回放校验）/ calibrate（梯队校准矩阵）。

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use t99_bots::BotAdapter;
use t99_core::{Key, Match, MatchConfig, Ruleset, TimedKey};
use t99_proto::{hello_for, parse_seat, PlayerAdapter, ResultMsg, SeatProgram, SubprocessAdapter};

const DEFAULT_MAX_FRAMES: u64 = 54_000; // 15 分钟 @60Hz

struct Seat {
    adapter: Box<dyn PlayerAdapter>,
    killed: bool,
}

fn usage() {
    eprintln!("用法:");
    eprintln!("  t99-headless run --players N --bots basic,advanced [--candidate 'python3 bot.py'] \\");
    eprintln!("                    [--seed S] [--max-frames F] [--transcript PATH] [--realtime]");
    eprintln!("  t99-headless replay --transcript PATH");
    eprintln!("  t99-headless calibrate --tiers basic,advanced,expert [--games G] [--seed S]");
}

fn arg_val(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
    }
    None
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn parse_events(raw: &[serde_json::Value]) -> Vec<TimedKey> {
    raw.iter()
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
}

fn event_json(t: &TimedKey) -> serde_json::Value {
    match t.key {
        Key::TargetPlayer(p) => serde_json::json!([t.t_us, t.key.name(), p]),
        _ => serde_json::json!([t.t_us, t.key.name()]),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }
    let cmd = args[0].clone();
    let rest = &args[1..];
    match cmd.as_str() {
        "run" => cmd_run(rest),
        "replay" => cmd_replay(rest),
        "calibrate" => cmd_calibrate(rest),
        _ => {
            usage();
            std::process::exit(2);
        }
    }
}

fn build_seats(args: &[String], players: usize) -> (Vec<Seat>, Vec<String>, u64) {
    let seed: u64 = arg_val(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(42);
    let mut seat_specs: Vec<String> = Vec::new();
    let mut seats: Vec<Seat> = Vec::new();
    if let Some(cmdline) = arg_val(args, "--candidate") {
        let cmd: Vec<String> =
            cmdline.split_whitespace().map(|s| s.to_string()).collect();
        seat_specs.push(format!("candidate:{}", cmdline));
        let adapter = SubprocessAdapter::new(format!("candidate:{}", cmdline), cmd);
        seats.push(Seat { adapter: Box::new(adapter), killed: false });
    }
    let bots: Vec<String> = arg_val(args, "--bots")
        .unwrap_or_else(|| "basic".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for b in &bots {
        // 档位（内建）或 ext:label=path（外部程序，与提交程序同通道）
        match parse_seat(b) {
            SeatProgram::BuiltIn(tier) => {
                seat_specs.push(format!("bot:{tier}"));
                seats.push(Seat { adapter: Box::new(BotAdapter::new(&tier)), killed: false });
            }
            SeatProgram::Ext { label, path } => {
                seat_specs.push(format!("ext:{label}={path}"));
                let adapter =
                    SubprocessAdapter::new(format!("ext:{label}"), vec![path]);
                seats.push(Seat { adapter: Box::new(adapter), killed: false });
            }
        }
    }
    while seats.len() < players {
        seat_specs.push("bot:basic".into());
        seats.push(Seat { adapter: Box::new(BotAdapter::new("basic")), killed: false });
    }
    seats.truncate(players);
    seat_specs.truncate(players);
    (seats, seat_specs, seed)
}

fn cmd_run(args: &[String]) {
    let players: usize = arg_val(args, "--players").and_then(|s| s.parse().ok()).unwrap_or(2);
    let max_frames: u64 = arg_val(args, "--max-frames")
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_FRAMES);
    let realtime = has_flag(args, "--realtime");
    let transcript_path = arg_val(args, "--transcript");
    let (mut seats, seat_specs, seed) = build_seats(args, players);

    let ruleset = Ruleset::default();
    let mut engine = Match::new(MatchConfig { players, seed, ruleset: ruleset.clone() });

    let mut tr: Option<std::fs::File> = transcript_path.map(|p| {
        let mut f = std::fs::File::create(&p).expect("创建 transcript 失败");
        let cfg = serde_json::json!({
            "type": "cfg",
            "players": players,
            "seed": seed,
            "seats": seat_specs,
            "preview": ruleset.preview,
        });
        writeln!(f, "{}", cfg).unwrap();
        f
    });

    // 握手
    for pid in 0..seats.len() {
        let hello = hello_for(pid, &format!("seat{pid}"), players, ruleset.preview);
        if seats[pid].adapter.setup(&hello).is_err() {
            seats[pid].killed = true;
            engine.kill(pid, "setup_failed");
            if let Some(f) = tr.as_mut() {
                writeln!(f, "{}", serde_json::json!({"type":"kill","pid":pid,"why":"setup_failed"})).unwrap();
            }
        }
    }

    let t0 = Instant::now();
    let mut last_tick = Instant::now();
    let mut n: u64 = 0;
    while n < max_frames && engine.over.is_none() {
        // 致命错误与连续超时判负
        for pid in 0..seats.len() {
            if seats[pid].killed {
                continue;
            }
            let why = if seats[pid].adapter.is_dead() {
                Some("process_dead")
            } else if seats[pid].adapter.consecutive_misses() >= t99_proto::MAX_CONSECUTIVE_MISS {
                Some("timeout")
            } else {
                None
            };
            if let Some(why) = why {
                seats[pid].killed = true;
                engine.kill(pid, why);
                if let Some(f) = tr.as_mut() {
                    writeln!(f, "{}", serde_json::json!({"type":"kill","pid":pid,"why":why})).unwrap();
                }
            }
        }
        let mut inputs: Vec<Vec<TimedKey>> = Vec::with_capacity(players);
        for pid in 0..players {
            if !engine.players[pid].alive || seats[pid].killed {
                inputs.push(vec![]);
                continue;
            }
            let snap = engine.snapshot(pid);
            inputs.push(seats[pid].adapter.act(&snap));
        }
        if let Some(f) = tr.as_mut() {
            let per: Vec<Vec<serde_json::Value>> = inputs
                .iter()
                .map(|ev| ev.iter().map(event_json).collect())
                .collect();
            writeln!(f, "{}", serde_json::json!({"type":"f","n":n,"in":per})).unwrap();
        }
        engine.frame(&inputs);
        n += 1;
        if realtime {
            let target = Duration::from_secs_f64(n as f64 / 60.0);
            let elapsed = last_tick.elapsed() + Duration::ZERO;
            let _ = elapsed;
            let since_start = t0.elapsed();
            if since_start < target {
                std::thread::sleep(target - since_start);
            }
            last_tick = Instant::now();
        }
    }

    // 名次组装：死亡者已有 place，存活者按 ranking 补
    let table = engine.final_table();
    let mut place_map: Vec<Option<u32>> = vec![None; players];
    for row in &table {
        place_map[row.pid] = row.place;
    }
    let mut pl = 1u32;
    for &pid in engine.ranking().iter() {
        if place_map[pid].is_none() {
            place_map[pid] = Some(pl);
            pl += 1;
        }
    }
    let reason = engine.over.as_ref().map(|o| o.reason.to_string()).unwrap_or_else(|| "time_limit".to_string());
    let hash = engine.hash_state();
    let minutes = n as f64 / 3600.0;

    println!("对局结束: 原因={reason} 帧={n} 用时={:.1}s 玩家数={players}", t0.elapsed().as_secs_f32());
    println!("{:<6} {:<6} {:<28} {:>4} {:>6} {:>6} {:>7} {:>7} {:>7} {:<6} {:<12}", "名次", "pid", "座位", "KO", "消行", "送出", "块数", "APM", "TPM", "存活", "终局");
    let mut order: Vec<usize> = (0..players).collect();
    order.sort_by_key(|&pid| place_map[pid].unwrap_or(99));
    for pid in order {
        let row = &table[pid];
        let apm = if minutes > 0.0 { row.sent as f64 / minutes } else { 0.0 };
        let tpm = if minutes > 0.0 { row.pieces as f64 / minutes } else { 0.0 };
        let place = place_map[pid].map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "{:<6} {:<6} {:<28} {:>4} {:>6} {:>6} {:>7} {:>7.1} {:>7.1} {:<6} {:<12}",
            place, pid, seats[pid].adapter.name(), row.ko, row.lines, row.sent, row.pieces, apm, tpm,
            if row.alive { "是" } else { "否" },
            row.reason.clone().unwrap_or_default()
        );
    }

    // 发送 result 并收尾
    for pid in 0..players {
        let row = &table[pid];
        let msg = ResultMsg {
            kind: "result",
            n,
            seat: pid,
            player_id: format!("seat{pid}"),
            place: place_map[pid],
            rank: row.rank,
            badges: row.badges,
            players,
            ko: row.ko,
            lines: row.lines,
            sent: row.sent,
            pieces: row.pieces,
            reason: if row.alive { reason.clone() } else { row.reason.clone().unwrap_or_default() },
            alive: row.alive,
        };
        seats[pid].adapter.finish(&msg);
    }
    if let Some(mut f) = tr {
        writeln!(f, "{}", serde_json::json!({"type":"end","reason":reason,"hash":hash})).unwrap();
        eprintln!("transcript 已写入（{} 帧）", n);
    }
}

fn cmd_replay(args: &[String]) {
    let path = arg_val(args, "--transcript").expect("需要 --transcript PATH");
    let f = std::fs::File::open(&path).expect("打开 transcript 失败");
    let reader = BufReader::new(f);
    let mut engine: Option<Match> = None;
    let mut players = 0usize;
    let mut recorded_hash: Option<u64> = None;
    for line in reader.lines() {
        let line = line.expect("读取失败");
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).expect("transcript 行不是 JSON");
        match v["type"].as_str().unwrap_or("") {
            "cfg" => {
                players = v["players"].as_u64().unwrap() as usize;
                let seed = v["seed"].as_u64().unwrap();
                engine = Some(Match::new(MatchConfig { players, seed, ruleset: Ruleset::default() }));
            }
            "f" => {
                let inputs: Vec<Vec<TimedKey>> = v["in"]
                    .as_array()
                    .expect("in 缺失")
                    .iter()
                    .map(|p| parse_events(p.as_array().unwrap()))
                    .collect();
                engine.as_mut().expect("缺少 cfg").frame(&inputs);
            }
            "kill" => {
                let pid = v["pid"].as_u64().unwrap() as usize;
                let why = v["why"].as_str().unwrap_or("");
                engine.as_mut().expect("缺少 cfg").kill(pid, why);
            }
            "end" => {
                recorded_hash = v["hash"].as_u64();
            }
            _ => {}
        }
    }
    let engine = engine.expect("transcript 为空");
    let hash = engine.hash_state();
    match recorded_hash {
        Some(h) if h == hash => {
            println!("回放校验通过: hash={hash} 帧={} 名次={:?}", engine.frame_n, engine.places());
        }
        Some(h) => {
            println!("回放校验失败: 记录={h} 重放={hash}");
            std::process::exit(1);
        }
        None => {
            println!("回放完成（无 end 记录）: hash={hash} 帧={}", engine.frame_n);
        }
    }
}

fn cmd_calibrate(args: &[String]) {
    let tiers: Vec<String> = arg_val(args, "--tiers")
        .expect("需要 --tiers")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let games: u64 = arg_val(args, "--games").and_then(|s| s.parse().ok()).unwrap_or(20);
    let base_seed: u64 = arg_val(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(1000);
    eprintln!("校准: 档位={tiers:?} 每对{}局 基础种子={}", games, base_seed);

    // wins[i][j] = i 胜 j 的局数
    let mut wins = vec![vec![0u64; tiers.len()]; tiers.len()];
    let mut games_total = vec![vec![0u64; tiers.len()]; tiers.len()];
    let mut game_idx = 0u64;
    for i in 0..tiers.len() {
        for j in 0..tiers.len() {
            if i >= j {
                continue;
            }
            for g in 0..games {
                let seed = base_seed.wrapping_add(game_idx);
                game_idx += 1;
                // 交替座位，消除编号偏差
                let (a_first, seat_a, seat_b) = if g % 2 == 0 {
                    (true, 0usize, 1usize)
                } else {
                    (false, 1usize, 0usize)
                };
                let mut engine = Match::new(MatchConfig { players: 2, seed, ruleset: Ruleset::default() });
                let mut bot_a = BotAdapter::new(&tiers[i]);
                let mut bot_b = BotAdapter::new(&tiers[j]);
                let mut n: u64 = 0;
                while n < DEFAULT_MAX_FRAMES && engine.over.is_none() {
                    let mut inputs = vec![vec![], vec![]];
                    for (pid, adapter) in [(seat_a, &mut bot_a), (seat_b, &mut bot_b)] {
                        if engine.players[pid].alive {
                            let snap = engine.snapshot(pid);
                            inputs[pid] = adapter.act(&snap);
                        }
                    }
                    engine.frame(&inputs);
                    n += 1;
                }
                // 名次
                let row_a = engine.final_table()[seat_a].clone();
                let row_b = engine.final_table()[seat_b].clone();
                let pa = row_a.place.or(Some(1));
                let pb = row_b.place.or(Some(1));
                let a_wins = pa < pb || (pa == pb && row_a.sent > row_b.sent);
                if a_wins {
                    wins[i][j] += 1;
                } else {
                    wins[j][i] += 1;
                }
                games_total[i][j] += 1;
                games_total[j][i] += 1;
                let _ = a_first;
            }
        }
    }
    print!("{:<10}", "vs");
    for t in &tiers {
        print!("{:>10}", t);
    }
    println!();
    for (i, t) in tiers.iter().enumerate() {
        print!("{:<10}", t);
        for j in 0..tiers.len() {
            if i == j {
                print!("{:>10}", "-");
            } else {
                let w = wins[i][j];
                let total = games_total[i][j];
                print!("{:>9.0}%", (w as f64 / total as f64) * 100.0);
            }
        }
        println!();
    }
}
