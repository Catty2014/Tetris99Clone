//! 快速对战工作台：引擎直连（无 60Hz 节流）驱动内建/外部席位，
//! 用于 bot 分层统计与调参。席位语法与服务器一致：
//!   tier                 内建档位（basic/advanced/expert/…）
//!   ext:label=path       外部程序（如 Master/GrandMaster）
//!
//! 输出：每局结束表 + 汇总（平均名次/存活帧/行/送出/块效率）。

use std::collections::HashMap;

use t99_bots::BotAdapter;
use t99_core::{Match, MatchConfig, Ruleset, TMS, TimedKey};
use t99_proto::{hello_for, parse_seat, PlayerAdapter, ResultMsg, SeatProgram, SubprocessAdapter, MAX_CONSECUTIVE_MISS};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut players = 4usize;
    let mut bots: Vec<String> = vec![];
    let mut seed = 42u64;
    let mut matches = 8usize;
    let mut time_limit = 0u64;
    let mut accel = true;
    let mut max_frames = 54_000u64;
    let mut quiet = false;
    let mut snaps: Vec<(u64, usize)> = vec![];
    {
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            if a == "--dump" {
                if let Some(v) = it.next() {
                    let mut parts = v.splitn(2, ':');
                    let f: u64 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let pid: usize = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    snaps.push((f, pid));
                }
            }
        }
        snaps.sort();
    }
    while let Some(a) = args.next() {
        match a.as_str() {
            "--players" => players = args.next().and_then(|v| v.parse().ok()).unwrap_or(players),
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
            "--max-frames" => max_frames = args.next().and_then(|v| v.parse().ok()).unwrap_or(max_frames),
            "--quiet" => quiet = true,
            _ => {}
        }
    }
    let short = |s: &str| match parse_seat(s) {
        SeatProgram::BuiltIn(t) => t,
        SeatProgram::Ext { label, .. } => format!("ext:{label}"),
    };

    let mut stats: HashMap<String, (u64, u64, u64, u64, u64, u64, u64, u64)> = HashMap::new(); // placeΣ, n, 存活局, linesΣ, sentΣ, piecesΣ, koΣ, badgesΣ
    for match_no in 1..=matches {
        let mut seed_m = seed;
        seed_m = seed_m.wrapping_mul(6364136223846793005).wrapping_add(match_no as u64 | 1);
        let mut ruleset = Ruleset::default();
        ruleset.match_ticks = if time_limit > 0 { (time_limit as i64) * 1000 * TMS } else { 0 };
        ruleset.stall_accel = accel;
        let mut m = Match::new(MatchConfig { players, seed: seed_m, ruleset });
        let mut adapters: Vec<Option<Box<dyn PlayerAdapter>>> = (0..players)
            .map(|pid| {
                bots.get(pid).map(|spec| -> Box<dyn PlayerAdapter> {
                    if let SeatProgram::Ext { label, path } = parse_seat(spec) {
                        Box::new(SubprocessAdapter::new(format!("ext:{label}"), vec![path]))
                    } else {
                        Box::new(BotAdapter::new(spec))
                    }
                })
            })
            .collect();
        for (pid, b) in adapters.iter_mut().enumerate() {
            if let Some(b) = b {
                if let Err(e) = b.setup(&hello_for(pid, &format!("seat{pid}"), players, 6)) {
                    eprintln!("[seat {pid}] {} setup 失败: {e:?}", b.name());
                    m.kill(pid, "setup_failed");
                }
            }
        }
        let mut frame: u64 = 0;
        let mut prev_boards: Vec<Option<Vec<[u8; 10]>>> = vec![None; players];
        let audit = std::env::var("DUEL_AUDIT").map(|v| v == "1").unwrap_or(false);
        while m.over.is_none() && frame < max_frames {
            let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; players];
            for pid in 0..players {
                if !m.players[pid].alive {
                    continue;
                }
                if let Some(b) = adapters[pid].as_mut() {
                    inputs[pid] = b.act(&m.snapshot(pid));
                    if b.is_dead() || b.consecutive_misses() > MAX_CONSECUTIVE_MISS {
                        m.kill(pid, "program_dead");
                    }
                }
            }
            m.frame(&inputs);
            frame += 1;
            if audit {
                for pid in 0..players {
                    let s = m.snapshot(pid);
                    let rows: Vec<[u8; 10]> = s.board.to_vec();
                    let pc = s.pieces;
                    let changed = match &prev_boards[pid] {
                        None => false,
                        Some(pb) => pb.as_slice() != rows.as_slice(),
                    };
                    if changed {
                        // 找出新增格（近似落点）
                        let mut cells = vec![];
                        if let Some(pb) = &prev_boards[pid] {
                            for r in 0..20 {
                                for c in 0..10 {
                                    if rows[r][c] != 0 && pb[r][c] == 0 {
                                        cells.push((r, c));
                                    }
                                }
                            }
                        }
                        let ymin = cells.iter().map(|c| c.0).min();
                        let ymax = cells.iter().map(|c| c.0).max();
                        let xmin = cells.iter().map(|c| c.1).min();
                        let xmax = cells.iter().map(|c| c.1).max();
                        println!("[audit] p{pid} f{frame} piece{pc} lock cells={cells:?} bbox=rows{:?}-{:?} cols{:?}-{:?}", ymin, ymax, xmin, xmax);
                    }
                    prev_boards[pid] = Some(rows);
                }
            }
            for (sf, sp) in &snaps {
                if *sf == frame {
                    let s = m.snapshot(*sp);
                    println!("---- f{frame} p{sp} 行{} 块{} ----", s.lines, s.pieces);
                    for (r, row) in s.board.iter().enumerate() {
                        let line: String = row.iter().map(|&c| if c == 0 { '.' } else { '#' }).collect();
                        println!("  │{line}│{r}");
                    }
                }
            }
        }
        // 流局排名兜底
        if m.over.is_none() {
            let mut surv: Vec<usize> = (0..players).filter(|&p| m.players[p].alive).collect();
            surv.sort_by(|&a, &b| m.players[b].badges.cmp(&m.players[a].badges).then(a.cmp(&b)));
            for (i, &pid) in surv.iter().enumerate() {
                if m.players[pid].place.is_none() {
                    m.players[pid].place = Some(i as u32 + 1);
                }
            }
        }
        if !quiet {
            let deaths: Vec<String> = (0..players)
                .filter(|&p| !m.players[p].alive)
                .map(|p| format!("p{p}(ko{})", m.players[p].badges))
                .collect();
            println!(
                "  ● match {match_no} f={} reason={} 死亡{} places={}",
                m.frame_n,
                m.over.as_ref().map(|o| o.reason).unwrap_or("time_limit"),
                if deaths.is_empty() { "无".into() } else { deaths.join(",") },
                (0..players).filter_map(|p| m.players[p].place.map(|pl| format!("{p}→{pl}"))).collect::<Vec<_>>().join(" ")
            );
        }
        for (pid, b) in adapters.iter_mut().enumerate() {
            if let Some(b) = b {
                let p = &m.players[pid];
                let msg = ResultMsg {
                    kind: "result",
                    n: m.frame_n,
                    seat: pid,
                    player_id: format!("seat{pid}"),
                    place: p.place,
                    rank: p.place.unwrap_or(0),
                    badges: p.badges,
                    players,
                    ko: p.ko,
                    lines: p.lines,
                    sent: p.sent,
                    pieces: p.pieces,
                    reason: m.over.as_ref().map(|o| o.reason.to_string()).unwrap_or_else(|| "time_limit".into()),
                    alive: p.alive,
                };
                b.finish(&msg);
                let e = stats.entry(short(&bots[pid])).or_insert((0, 0, 0, 0, 0, 0, 0, 0));
                e.0 += p.place.unwrap_or(players as u32) as u64;
                e.1 += 1;
                if p.alive {
                    e.2 += 1; // 存活局数
                }
                e.3 += p.lines as u64;
                e.4 += p.sent as u64;
                e.5 += p.pieces as u64;
                e.6 += p.ko as u64;
                e.7 += p.badges as u64;
            }
        }
    }
    println!("=== 汇总（{matches} 局）===");
    let mut rows: Vec<(String, f64, f64, f64, f64, f64, f64)> = stats
        .into_iter()
        .map(|(k, (ps, n, surv, ln, st, pc, ko, bd))| {
            (
                k,
                ps as f64 / n as f64,
                ln as f64 / pc.max(1) as f64,
                st as f64 / n as f64,
                pc as f64 / n.max(1) as f64 * 60.0 / 6236.0, // 粗略 PPS（按场均帧）
                surv as f64 / n.max(1) as f64,
                ko as f64 / n.max(1) as f64,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    println!("  {:<14} {:>8} {:>8} {:>8} {:>6} {:>6} {:>5}", "席位", "平均名次", "行/块", "送出/局", "PPS", "存活率", "KO/局");
    for (k, avg, lp, sent, pps, surv, ko) in rows {
        println!("  {k:<14} {avg:>8.2} {lp:>8.3} {sent:>8.1} {pps:>6.2} {surv:>6.2} {ko:>5.2}");
    }
}
