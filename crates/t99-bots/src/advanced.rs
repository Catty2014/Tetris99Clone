//! AdvancedBot：Dellacherie 6 特征 + Hold 分支 + next[0]，加权目标策略，危险转平堆。

use super::{best_dellach, grid_of, max_height, Bot, Plan, PlanBot};
use t99_core::{Key, Snapshot, TimedKey};

/// advanced 的加权目标：Random / KOs / Badges 各 1；反击 `min(9, 被瞄) / 3`。
/// 每次成功送出后重抽。集中攻击会撞上 12 行待进上限并被消行抵消，
/// 99 人基准实测在「待进未满」的目标里随机摊开优于集中策略。
fn advanced_weighted(s: &Snapshot, rng: &mut u64) -> Key {
    let w_att = s.targeted_by.len().min(9) as f64 / 3.0;
    let weights = [
        (Key::TargetRandom, 1.0),
        (Key::TargetKos, 1.0),
        (Key::TargetBadges, 1.0),
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

pub struct AdvancedBot {
    plan: Option<Plan>,
    frames: u64,
    last_mode: Option<Key>,
    last_switch: u64,
    last_move_frame: u64,
    last_sent: u32,
    weighted_mode: Option<Key>,
    rng: u64,
}

impl AdvancedBot {
    pub fn new() -> Self {
        AdvancedBot {
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

impl Default for AdvancedBot {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanBot for AdvancedBot {
    fn use_hold(&self) -> bool {
        true
    }
    fn use_panic(&self) -> bool {
        true
    }
    fn search(&mut self, s: &Snapshot) -> Option<(u8, i32)> {
        let grid = grid_of(s);
        let panic = self.use_panic() && max_height(&grid) >= 15;
        let a = s.active?;
        let cur = best_dellach(&grid, a.kind, panic);
        if !self.use_hold() || s.hold_used {
            return cur.map(|(r, c, _, _)| (r, c));
        }
        // Hold 分支：hold 空则换入 next[0]，否则换入 hold
        let swap_kind = match s.hold {
            Some(k) => Some(k),
            None => s.next.first().copied(),
        };
        if let Some(k) = swap_kind {
            let alt = best_dellach(&grid, k, panic);
            let better = match (&cur, &alt) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some((_, _, _, s1)), Some((_, _, _, s2))) => *s2 > *s1,
            };
            if better {
                return None; // 触发 Hold
            }
        }
        cur.map(|(r, c, _, _)| (r, c))
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
            self.weighted_mode = Some(advanced_weighted(s, &mut self.rng));
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

impl Bot for AdvancedBot {
    fn name(&self) -> &'static str {
        "advanced"
    }
    fn decide(&mut self, s: &Snapshot) -> Vec<TimedKey> {
        PlanBot::decide(self, s)
    }
}
