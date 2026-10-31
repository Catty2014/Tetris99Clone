//! BasicBot：单块贪心（4 特征），静态随机目标，不用 Hold/预览。
//!
//! Tetris 基线档：惩罚「什么都不做」，为更高档位提供强度下界。

use super::{allow_move, best_basic, ev, grid_of, max_height, Bot, Plan};
use t99_core::{Key, Snapshot, TimedKey};

pub struct BasicBot {
    plan: Option<Plan>,
    frames: u64,
    last_move_frame: u64,
}

impl BasicBot {
    pub fn new() -> Self {
        BasicBot { plan: None, frames: 0, last_move_frame: 0 }
    }
}

impl Default for BasicBot {
    fn default() -> Self {
        Self::new()
    }
}

impl Bot for BasicBot {
    fn name(&self) -> &'static str {
        "basic"
    }
    fn decide(&mut self, s: &Snapshot) -> Vec<TimedKey> {
        if !s.alive_self {
            return vec![];
        }
        if self.frames == 0 {
            self.frames += 1;
            return vec![ev(0, Key::TargetRandom)];
        }
        self.frames += 1;
        let Some(a) = s.active else { return vec![] };
        let sig = (s.pieces as u64, a.kind.id(), s.hold.map(|k| k.id()).unwrap_or(0), s.hold_used);
        let need = match &self.plan {
            Some(p) => (p.sig_pieces, p.sig_kind, p.sig_hold, p.sig_hold_used) != sig,
            None => true,
        };
        if need {
            let grid = grid_of(s);
            let panic = max_height(&grid) >= 15;
            let found = best_basic(&grid, a.kind, panic).map(|(r, c)| Plan {
                sig_pieces: sig.0,
                sig_kind: sig.1,
                sig_hold: sig.2,
                sig_hold_used: sig.3,
                rot: r,
                col: c,
                want_hold: false,
            });
            self.plan = found;
        }
        let plan = match &self.plan {
            Some(p) if (p.sig_pieces, p.sig_kind, p.sig_hold, p.sig_hold_used) == sig => p,
            _ => return vec![],
        };
        let diff = (plan.rot as i32 + 4 - (a.rot as i32 % 4)) % 4;
        if diff == 3 {
            return vec![ev(0, Key::RotCcw)];
        }
        if diff != 0 {
            return vec![ev(0, Key::RotCw)];
        }
        if a.x < plan.col {
            if !allow_move(self.frames, &mut self.last_move_frame) {
                return vec![];
            }
            return vec![ev(0, Key::RightDown), ev(4000, Key::RightUp)];
        }
        if a.x > plan.col {
            if !allow_move(self.frames, &mut self.last_move_frame) {
                return vec![];
            }
            return vec![ev(0, Key::LeftDown), ev(4000, Key::LeftUp)];
        }
        vec![ev(0, Key::HardDrop)]
    }
}
