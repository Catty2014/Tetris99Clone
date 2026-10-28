//! 离线重放分析器：复刻 t99-server 的对局循环（同种子同 bot），逐帧采样快照，
//! 输出每次死亡的完整现场与周期性压力表，用于诊断「低堆叠死亡 / 无进展误判」。
//! 席位语法与服务器一致：`tier`（内建）或 `ext:label=path`（外部程序，如 M/GM）。

use t99_bots::BotAdapter;
use t99_core::{Match, MatchConfig, Ruleset, TMS, TimedKey};
use t99_proto::{hello_for, parse_seat, PlayerAdapter, ResultMsg, SeatProgram, SubprocessAdapter, MAX_CONSECUTIVE_MISS};

#[derive(Clone)]
struct Sample {
    alive: bool,
    stack_rows: usize,   // 可见区占用行数（自底向上连续非空计数）
    top_gap: usize,      // 顶行到最高块的距离（0 = 顶到可见区顶部）
    pending: usize,      // 待进垃圾总行数
    pieces: u32,
    lines: u32,
}

fn sample(m: &Match, pid: usize) -> Sample {
    let s = m.snapshot(pid);
    let mut stack_rows = 0;
    let mut top_gap = 20;
    for r in 0..20 {
        if s.board[r].iter().any(|&c| c != 0) {
            if top_gap == 20 {
                top_gap = r;
            }
            stack_rows += 1;
        }
    }
    Sample {
        alive: s.alive_self,
        stack_rows,
        top_gap,
        pending: (s.garbage_red + s.garbage_yellow + s.garbage_gray) as usize,
        pieces: s.pieces,
        lines: s.lines,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut players = 8usize;
    let mut bots: Vec<String> = vec![];
    let mut seed = 2026u64;
    let mut matches = 6usize;
    let mut time_limit = 0u64;
    let mut accel = true;
    let mut dump: Option<usize> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--players" => players = args.next().and_then(|v| v.parse().ok()).unwrap_or(players),
            "--dump" => dump = args.next().and_then(|v| v.parse().ok()),
            "--bots" => {
                bots = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(seed),
            "--matches" => matches = args.next().and_then(|v| v.parse().ok()).unwrap_or(matches),
            "--time-limit-secs" => time_limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(time_limit),
            "--stall-accel" => accel = args.next().map(|v| v != "0" && v != "false").unwrap_or(accel),
            _ => {}
        }
    }

    let mut seed = seed;
    // 分层统计：label → (名次累计, 参赛场次)
    let mut tier_stats: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
    for match_no in 1..=matches {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(match_no as u64 | 1);
        let mut ruleset = Ruleset::default();
        ruleset.match_ticks = if time_limit > 0 { (time_limit as i64) * 1000 * TMS } else { 0 };
        ruleset.stall_accel = accel;
        let mut m = Match::new(MatchConfig { players, seed, ruleset });
        let mut bots_v: Vec<Option<Box<dyn PlayerAdapter>>> = (0..players)
            .map(|pid| {
                bots.get(pid).map(|spec| -> Box<dyn PlayerAdapter> {
                    let label = match parse_seat(spec) {
                        SeatProgram::BuiltIn(tier) => {
                            let _ = &tier;
                            tier
                        }
                        SeatProgram::Ext { label, path } => {
                            return Box::new(SubprocessAdapter::new(format!("ext:{label}"), vec![path]));
                        }
                    };
                    Box::new(BotAdapter::new(&label))
                })
            })
            .collect();
        for (pid, b) in bots_v.iter_mut().enumerate() {
            if let Some(b) = b {
                if let Err(e) = b.setup(&hello_for(pid, players, 6)) {
                    eprintln!("[seat {pid}] {} setup 失败: {e:?}", b.name());
                    m.kill(pid, "setup_failed");
                }
            }
        }
        let mut prev: Vec<Sample> = (0..players).map(|pid| sample(&m, pid)).collect();
        println!("=== match {match_no} seed {seed} players {players} bots {:?} ===", bots);
        let mut frame: u64 = 0;
        while m.over.is_none() && frame < 54_000 {
            let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; players];
            for pid in 0..players {
                if !m.players[pid].alive {
                    continue;
                }
                if let Some(b) = bots_v[pid].as_mut() {
                    inputs[pid] = b.act(&m.snapshot(pid));
                    if b.is_dead() || b.consecutive_misses() > MAX_CONSECUTIVE_MISS {
                        m.kill(pid, "program_dead");
                    }
                }
            }
            m.frame(&inputs);
            frame += 1;
            // 每 600 帧（10s）输出压力表
            if frame % 600 == 0 {
                let row: Vec<String> = (0..players)
                    .map(|pid| {
                        let s = sample(&m, pid);
                        if !s.alive {
                            format!("p{pid}:✝")
                        } else {
                            format!("p{pid}:堆{}隙{}待{}", s.stack_rows, s.top_gap, s.pending)
                        }
                    })
                    .collect();
                println!("[f{frame} 相位{}] {}", m.phase_idx + 1, row.join(" "));
            }
            // 死亡现场
            for pid in 0..players {
                let cur = sample(&m, pid);
                if prev[pid].alive && !cur.alive {
                    println!(
                        "  ☠ f{frame} p{pid} place={:?} 堆叠{}行 顶部空隙{} 待进{} 块{} 行{}",
                        m.players[pid].place, cur.stack_rows, cur.top_gap, cur.pending, cur.pieces, cur.lines
                    );
                    if dump == Some(pid) {
                        let s = m.snapshot(pid);
                        println!("  ┌──────────┐");
                        for (r, row) in s.board.iter().enumerate() {
                            let line: String = row.iter().map(|&c| if c == 0 { '.' } else { '#' }).collect();
                            println!("  │{line}│ {r}");
                        }
                        println!("  └──────────┘");
                    }
                }
                prev[pid] = cur;
            }
        }
        let r = m.over.as_ref().map(|o| o.reason.to_string()).unwrap_or("time_limit".into());
        println!(
            "  ● match {match_no} 结束 f={} reason={r} places={:?}",
            m.frame_n,
            m.over.as_ref().map(|o| o.places.clone()).unwrap_or_default()
        );
        // 终局时各存活者状态
        for pid in 0..players {
            let s = sample(&m, pid);
            if s.alive {
                println!("  存活 p{pid}: 堆叠{}行 待进{} 块{} 行{} 送出{}", s.stack_rows, s.pending, s.pieces, s.lines, m.players[pid].sent);
            }
        }
        // 结算：外部子进程收尾 + 分层统计
        let fallback_reason = "time_limit".to_string();
        if m.over.is_none() {
            let mut surv: Vec<usize> = (0..players).filter(|&p| m.players[p].alive).collect();
            surv.sort_by(|&a, &b| m.players[b].badges.cmp(&m.players[a].badges).then(a.cmp(&b)));
            for (i, &pid) in surv.iter().enumerate() {
                if m.players[pid].place.is_none() {
                    m.players[pid].place = Some(i as u32 + 1);
                }
            }
        }
        for (pid, b) in bots_v.iter_mut().enumerate() {
            if let Some(b) = b {
                let p = &m.players[pid];
                let msg = ResultMsg {
                    kind: "result",
                    n: m.frame_n,
                    place: p.place,
                    players,
                    ko: p.ko,
                    lines: p.lines,
                    sent: p.sent,
                    pieces: p.pieces,
                    reason: m.over.as_ref().map(|o| o.reason.to_string()).unwrap_or_else(|| fallback_reason.clone()),
                    alive: p.alive,
                };
                b.finish(&msg);
                let label = bots.get(pid).cloned().unwrap_or_default();
                let place = p.place.unwrap_or(players as u32) as u64;
                let e = tier_stats.entry(label).or_insert((0, 0));
                e.0 += place;
                e.1 += 1;
            }
        }
    }
    // 分层汇总：平均名次升序（越小越强）
    if !tier_stats.is_empty() {
        println!("=== 分层汇总（{} 局平均名次）===", matches);
        let mut rows: Vec<(String, f64, u64)> = tier_stats
            .into_iter()
            .map(|(k, (s, n))| (k, s as f64 / n.max(1) as f64, n))
            .collect();
        rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (k, avg, n) in rows {
            println!("  {k:<16} 平均名次 {avg:.2}  ({} 局)", n);
        }
    }
}
