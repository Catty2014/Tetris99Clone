//! 目标策略离线评测器：引擎直连、无网络、无节流。
//! 同一批 bot 阵容下跑若干局，对比不同强制目标策略的淘汰/交付效率。
//! 仅使用内置 basic/advanced/expert/null，外部子进程太慢不纳入评测。

use t99_bots::BotAdapter;
use t99_core::engine::TargetPolicy;
use t99_core::{Match, MatchConfig, Ruleset, TimedKey, TMS};
use t99_proto::{hello_for, PlayerAdapter, ResultMsg};

fn parse_policy(s: &str) -> Option<TargetPolicy> {
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

fn policy_name(p: TargetPolicy) -> &'static str {
    match p {
        TargetPolicy::Random => "random",
        TargetPolicy::Kos => "kos",
        TargetPolicy::Badges => "badges",
        TargetPolicy::Spread => "spread",
        TargetPolicy::HuntWeak => "huntweak",
        TargetPolicy::AvoidAttackers => "avoidattackers",
        TargetPolicy::SpreadSafe => "spreadsafe",
        TargetPolicy::SpreadWeak => "spreadweak",
        TargetPolicy::RandomTop => "randomtop",
        TargetPolicy::Attackers => "attackers",
    }
}

/// 默认阵容：按玩家数均分三档（basic/advanced/expert）。
fn default_tiers(players: usize) -> Vec<String> {
    (0..players)
        .map(|pid| match pid * 3 / players {
            0 => "basic".to_string(),
            1 => "advanced".to_string(),
            _ => "expert".to_string(),
        })
        .collect()
}

/// 同档交付计数：按 pid 对应的 bot 档位分组，统计 sent_to 矩阵落在同档的行数。
fn same_tier_counts(m: &Match, tiers: &[String]) -> (u64, u64) {
    let mut same = 0u64;
    let mut total = 0u64;
    for pid in 0..tiers.len() {
        for to in 0..tiers.len() {
            let n = m.players[pid].sent_to[to] as u64;
            total += n;
            if tiers[pid] == tiers[to] {
                same += n;
            }
        }
    }
    (same, total)
}

fn pct(same: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        same as f64 * 100.0 / total as f64
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut policy = TargetPolicy::Random;
    let mut players = 99usize;
    let mut matches = 1usize;
    let mut time_limit_secs = 60u64;
    let mut seed = 2026u64;
    let mut bot_specs: Option<Vec<String>> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--policy" => {
                let v = args.next().unwrap_or_default();
                policy = parse_policy(&v).unwrap_or_else(|| {
                    eprintln!(
                        "[policy] 未知策略 `{v}`；可选 random|kos|badges|spread|huntweak|avoidattackers|spreadsafe|spreadweak|randomtop|attackers"
                    );
                    std::process::exit(2);
                });
            }
            "--players" => players = args.next().and_then(|v| v.parse().ok()).unwrap_or(players),
            "--matches" => matches = args.next().and_then(|v| v.parse().ok()).unwrap_or(matches),
            "--time-limit-secs" => {
                time_limit_secs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(time_limit_secs)
            }
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(seed),
            "--bots" => {
                bot_specs = Some(
                    args.next()
                        .unwrap_or_default()
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            _ => {}
        }
    }
    assert!(players > 0, "players 必须大于 0");

    // 指定梯子时按列表顺序把席位均分到各档；否则默认三档均分。
    let tiers: Vec<String> = match bot_specs {
        Some(list) if !list.is_empty() => (0..players)
            .map(|pid| list[pid * list.len() / players].clone())
            .collect(),
        _ => default_tiers(players),
    };
    for t in &tiers {
        if !matches!(t.as_str(), "basic" | "advanced" | "expert" | "null") {
            eprintln!("[bots] 不支持 `{t}`；仅限 basic/advanced/expert/null");
            std::process::exit(2);
        }
    }

    let max_frames = if time_limit_secs > 0 {
        time_limit_secs.saturating_mul(60)
    } else {
        54_000
    };

    let mut sum_elim = 0u64;
    let mut sum_delivered = 0u64;
    let mut sum_inserted = 0u64;
    let mut sum_frames = 0u64;
    let mut sum_same = 0u64;
    let mut sum_total_sent = 0u64;

    for match_no in 1..=matches {
        let seed = seed.wrapping_add((match_no as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut ruleset = Ruleset::default();
        if time_limit_secs > 0 {
            ruleset.match_ticks = (time_limit_secs as i64) * 1000 * TMS;
        }
        let mut m = Match::new(MatchConfig {
            players,
            seed,
            ruleset,
        });
        m.force_target_policy(policy);

        let mut bots: Vec<BotAdapter> = tiers.iter().map(|t| BotAdapter::new(t)).collect();
        for (pid, b) in bots.iter_mut().enumerate() {
            if let Err(e) = b.setup(&hello_for(pid, &format!("seat{pid}"), players, 6)) {
                eprintln!("[seat {pid}] {} setup 失败: {e:?}", b.name());
                m.kill(pid, "setup_failed");
            }
        }

        let mut frames = 0u64;
        while m.over.is_none() && frames < max_frames {
            let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; players];
            for pid in 0..players {
                if m.players[pid].alive {
                    inputs[pid] = bots[pid].act(&m.snapshot(pid));
                }
            }
            m.frame(&inputs);
            frames += 1;
        }

        // 通知 bot 收尾，与服务器结算语义保持一致。
        for (pid, b) in bots.iter_mut().enumerate() {
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
                reason: m
                    .over
                    .as_ref()
                    .map(|o| o.reason.to_string())
                    .unwrap_or_else(|| "time_limit".into()),
                alive: p.alive,
            };
            b.finish(&msg);
        }

        let elim = m.players.iter().filter(|p| !p.alive).count() as u64;
        let delivered: u64 = m.players.iter().map(|p| p.sent as u64).sum();
        let canceled: u64 = m.players.iter().map(|p| p.canceled as u64).sum();
        let inserted: u64 = m.players.iter().map(|p| p.inserted as u64).sum();
        let dropped: u64 = m.players.iter().map(|p| p.dropped as u64).sum();
        let (same, total_sent) = same_tier_counts(&m, &tiers);
        let pct = pct(same, total_sent);

        println!(
            "[match {match_no}] elim={elim} frames={} delivered={delivered} canceled={canceled} inserted={inserted} dropped={dropped} same_tier_pct={pct:.1}",
            m.frame_n
        );

        sum_elim += elim;
        sum_delivered += delivered;
        sum_inserted += inserted;
        sum_frames += m.frame_n;
        sum_same += same;
        sum_total_sent += total_sent;
    }

    let n = matches.max(1) as f64;
    println!(
        "[summary] policy={} matches={} avg_elim={:.1} avg_inserted={:.1} avg_delivered={:.1} avg_frames={:.1} same_tier_pct={:.1}",
        policy_name(policy),
        matches,
        sum_elim as f64 / n,
        sum_inserted as f64 / n,
        sum_delivered as f64 / n,
        sum_frames as f64 / n,
        pct(sum_same, sum_total_sent)
    );
}
