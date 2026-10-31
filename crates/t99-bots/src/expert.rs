//! ExpertBot：2 深度束搜索 + Hold 分支 + 加权目标策略。
//!
//! internal 专有档（feature `expert`，默认启用）；分发源码剔除本文件。

use super::{
    best_dellach, grid_of, max_height, placements, Bot, Grid, Plan, PlanBot,
};
use t99_core::{Key, Snapshot, TimedKey, Kind};

/// expert 的加权目标：Random 100；Badges (max-own)/max*100；KOs (max_stack-10)*10；Attackers min(aim,12)/6*100。
fn weighted_target(s: &Snapshot, rng: &mut u64) -> Key {
    let own = s.badge_units as f64;
    let max_b = s
        .alive
        .iter()
        .filter(|&&p| p != s.pid)
        .map(|&p| s.badges_all.get(p).copied().unwrap_or(0))
        .max()
        .unwrap_or(0) as f64;
    let w_badges = if max_b > 0.0 {
        ((max_b - own).max(0.0) / max_b) * 100.0
    } else {
        0.0
    };
    let w_kos = ((s.max_stack as f64 - 10.0) * 10.0).max(0.0);
    let w_att = s.targeted_by.len().min(12) as f64 / 6.0 * 100.0;
    let weights = [
        (Key::TargetRandom, 100.0),
        (Key::TargetBadges, w_badges),
        (Key::TargetKos, w_kos),
        (Key::TargetAttackers, w_att),
    ];
    let sum: f64 = weights.iter().map(|(_, w)| *w).sum();
    *rng = rng
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let mut roll = ((*rng >> 11) as f64 / (1u64 << 53) as f64) * sum;
    for (k, w) in weights {
        if roll < w {
            return k;
        }
        roll -= w;
    }
    Key::TargetRandom
}

pub struct ExpertBot {
    plan: Option<Plan>,
    frames: u64,
    last_mode: Option<Key>,
    last_switch: u64,
    last_move_frame: u64,
    last_sent: u32,
    weighted_mode: Option<Key>,
    rng: u64,
}

impl ExpertBot {
    pub fn new() -> Self {
        ExpertBot {
            plan: None,
            frames: 0,
            last_mode: None,
            last_switch: 0,
            last_move_frame: 0,
            last_sent: 0,
            weighted_mode: None,
            rng: 0,
        }
    }
}

impl Default for ExpertBot {
    fn default() -> Self {
        Self::new()
    }
}

const EXPERT_BEAM: usize = 12;

impl PlanBot for ExpertBot {
    fn use_hold(&self) -> bool {
        true
    }
    fn use_panic(&self) -> bool {
        true
    }
    fn search(&mut self, s: &Snapshot) -> Option<(u8, i32)> {
        let grid: Grid = grid_of(s);
        let panic = self.use_panic() && max_height(&grid) >= 15;
        let a = s.active?;
        let next = s.next.first().copied();
        let beam_root = |kind: Kind| -> Option<(f64, u8, i32)> {
            // 深度 2：root + next，按 root 分数截断束宽，终局分数主导
            let mut cands: Vec<(f64, f64, u8, i32, Grid)> = placements(&grid, kind)
                .into_iter()
                .map(|p| {
                    let mut sc = p.score_dellach;
                    if panic {
                        sc += p.f.cleared * 25.0;
                    }
                    (sc, 0.0, p.rot, p.col, p.grid)
                })
                .collect();
            if cands.is_empty() {
                return None;
            }
            cands.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
            cands.truncate(EXPERT_BEAM);
            let mut best: Option<(f64, u8, i32)> = None;
            for (root_sc, _, rot, col, g) in cands {
                let mut tail_best = root_sc;
                if let Some(nk) = next {
                    if let Some(b) = placements(&g, nk)
                        .into_iter()
                        .map(|p| {
                            let mut sc = p.score_dellach;
                            if panic {
                                sc += p.f.cleared * 25.0;
                            }
                            sc
                        })
                        .max_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
                    {
                        tail_best = tail_best * 0.3 + b * 0.7;
                    }
                }
                if best.as_ref().map(|(b, _, _)| tail_best > *b).unwrap_or(true) {
                    best = Some((tail_best, rot, col));
                }
            }
            best
        };
        let cur = beam_root(a.kind).map(|(_, r, c)| (r, c));
        if !self.use_hold() || s.hold_used {
            return cur;
        }
        let swap_kind = match s.hold {
            Some(k) => Some(k),
            None => s.next.first().copied(),
        };
        if let Some(k) = swap_kind {
            // Hold 决策：两种棋型在同一局面下的单块最优评估公平比较
            let grid = grid_of(s);
            let panic = self.use_panic() && max_height(&grid) >= 15;
            let cur_sc = best_dellach(&grid, a.kind, panic).map(|x| x.3);
            let alt_sc = best_dellach(&grid, k, panic).map(|x| x.3);
            if alt_sc > cur_sc {
                return None;
            }
        }
        cur
    }
    fn plan_mut(&mut self) -> &mut Option<Plan> {
        &mut self.plan
    }
    fn frames_mut(&mut self) -> &mut u64 {
        &mut self.frames
    }
    fn last_move_frame_mut(&mut self) -> &mut u64 {
        &mut self.last_move_frame
    }
    fn target_policy(&mut self, s: &Snapshot) -> Option<Key> {
        if self.rng == 0 {
            self.rng = 0x9E37_79B9_7F4A_7C15 ^ (s.pid as u64 + 1);
        }
        if s.sent != self.last_sent || self.weighted_mode.is_none() {
            self.last_sent = s.sent;
            self.weighted_mode = Some(weighted_target(s, &mut self.rng));
        }
        let want = match self.weighted_mode {
            Some(k) => k,
            None => Key::TargetRandom,
        };
        if Some(want) != self.last_mode {
            self.last_mode = Some(want);
            self.last_switch = self.frames;
            return Some(want);
        }
        None
    }
}

impl Bot for ExpertBot {
    fn name(&self) -> &'static str {
        "expert"
    }
    fn decide(&mut self, s: &Snapshot) -> Vec<TimedKey> {
        PlanBot::decide(self, s)
    }
}
