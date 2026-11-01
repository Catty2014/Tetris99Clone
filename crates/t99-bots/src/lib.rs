//! 内置 Bot：与子进程程序同视角（只读自身快照）、同输入协议、同 DAS 限速。
//!
//! 档位按文件解耦（能力超集递进）：
//! - `null.rs`     NullBot    什么都不做（地板校准）
//! - `basic.rs`    BasicBot   单块贪心（4 特征），静态随机目标，不用 Hold/预览
//! - `advanced.rs` AdvancedBot Dellacherie 6 特征 + Hold + next[0]，加权目标策略，危险转平堆
//! - `expert.rs`   ExpertBot  internal 专有档（能力描述与实现见 expert.rs，不随公开文档展开）
//!
//! `expert.rs` 挂 feature `expert`（默认启用）。分发源码剔除该文件并关闭
//! feature 后，本 crate 仍可独立构建与对局（expert 档位以闭源形式内建在
//! 分发的二进制中）。

use t99_core::{Key, Snapshot, TimedKey, Kind, W, H};
use t99_proto::{AdapterError, Hello, PlayerAdapter, ResultMsg};

pub mod advanced;
pub mod basic;
pub mod null;
#[cfg(feature = "expert")]
pub mod expert;

pub use advanced::AdvancedBot;
pub use basic::BasicBot;
pub use null::NullBot;
#[cfg(feature = "expert")]
pub use expert::ExpertBot;

pub type Grid = [[u8; W]; H];

pub trait Bot: Send + Sync {
    fn name(&self) -> &'static str;
    fn ready(&mut self, pid: usize, players: usize, preview: usize) {
        let _ = (pid, players, preview);
    }
    fn decide(&mut self, s: &Snapshot) -> Vec<TimedKey>;
}

// ———— 评估特征 ————

#[derive(Clone, Debug, Default)]
pub struct Features {
    pub agg_height: f64,
    pub holes: f64,
    pub bump: f64,
    pub cleared: f64,
    pub landing_height: f64,
    pub eroded: f64,
    pub row_trans: f64,
    pub col_trans: f64,
    pub wells: f64,
    pub max_height: f64,
    pub overflow: f64,
}

fn features_after(grid: &Grid, cleared: u32, piece_rows: &[usize], piece_in_cleared: u32, eroded_rows: u32) -> Features {
    let mut heights = [0i64; W];
    let mut holes = 0i64;
    let mut row_trans = 0i64;
    let mut col_trans = 0i64;
    let mut wells = 0i64;
    for c in 0..W {
        let mut top = H as i64;
        for r in 0..H {
            if grid[r][c] != 0 {
                top = r as i64;
                break;
            }
        }
        heights[c] = H as i64 - top;
        let mut seen = false;
        for r in 0..H {
            if grid[r][c] != 0 {
                seen = true;
            } else if seen {
                holes += 1;
            }
        }
        // 列转移（上边界空、下边界实）
        let mut prev = false;
        for r in 0..H {
            let cur = grid[r][c] != 0;
            if cur != prev {
                col_trans += 1;
            }
            prev = cur;
        }
        if !prev {
            col_trans += 1;
        }
    }
    for r in 0..H {
        let mut prev = true; // 左墙实
        for c in 0..W {
            let cur = grid[r][c] != 0;
            if cur != prev {
                row_trans += 1;
            }
            prev = cur;
        }
        if !prev {
            row_trans += 1; // 右墙实
        }
    }
    for c in 0..W {
        let mut run = 0i64;
        for r in 0..H {
            let empty = grid[r][c] == 0;
            let lwall = c == 0 || grid[r][c - 1] != 0;
            let rwall = c == W - 1 || grid[r][c + 1] != 0;
            if empty && lwall && rwall {
                run += 1;
                wells += run;
            } else {
                run = 0;
            }
        }
    }
    let agg: i64 = heights.iter().sum();
    let bump: i64 = (0..W - 1).map(|i| (heights[i] - heights[i + 1]).abs()).sum();
    let max_h = *heights.iter().max().unwrap();
    let avg_row = if piece_rows.is_empty() {
        0.0
    } else {
        piece_rows.iter().sum::<usize>() as f64 / piece_rows.len() as f64
    };
    Features {
        agg_height: agg as f64,
        holes: holes as f64,
        bump: bump as f64,
        cleared: cleared as f64,
        landing_height: H as f64 - avg_row,
        eroded: eroded_rows as f64 * piece_in_cleared as f64,
        row_trans: row_trans as f64,
        col_trans: col_trans as f64,
        wells: wells as f64,
        max_height: max_h as f64,
        overflow: 0.0,
    }
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub rot: u8,
    pub col: i32,
    pub grid: Grid,
    pub f: Features,
    pub score_basic: f64,
    pub score_dellach: f64,
}

fn stamp_and_clear(grid: &Grid, kind: Kind, rot: u8, x: i32, y: i32) -> Option<(Grid, u32, Vec<usize>, u32, u32)> {
    let mut g = *grid;
    let mut piece_rows = Vec::new();
    for (dx, dy) in kind.cells(rot) {
        let col = x + dx;
        let row = y + dy;
        if col < 0 || col >= W as i32 || row < 0 || row >= H as i32 {
            return None; // 越界/顶部溢出：直落近似里视为不可行
        }
        g[row as usize][col as usize] = kind.id();
        piece_rows.push(row as usize);
    }
    // 消行（自底向上保持顺序）
    let mut kept: Vec<[u8; W]> = Vec::with_capacity(H);
    let mut cleared = 0u32;
    let mut eroded_rows = 0u32;
    let mut piece_in_cleared = 0u32;
    for r in 0..H {
        if g[r].iter().all(|&c| c != 0) {
            cleared += 1;
            if piece_rows.contains(&r) {
                eroded_rows += 1;
                piece_in_cleared += kind
                    .cells(rot)
                    .iter()
                    .filter(|&&(_, dy)| (y + dy) as usize == r)
                    .count() as u32;
            }
        } else {
            kept.push(g[r]);
        }
    }
    let mut g2 = [[0; W]; H];
    let base = H - kept.len();
    for (i, row) in kept.iter().enumerate() {
        g2[base + i] = *row;
    }
    Some((g2, cleared, piece_rows, piece_in_cleared, eroded_rows))
}

/// 直落可行落点（不模拟 SRS 进路，贪心近似）。
pub fn placements(grid: &Grid, kind: Kind) -> Vec<Placement> {
    let mut out = Vec::new();
    let rot_count = if kind == Kind::O { 1 } else { 4 };
    for rot in 0..rot_count {
        let cells = kind.cells(rot);
        let min_dx = cells.iter().map(|c| c.0).min().unwrap();
        let max_dx = cells.iter().map(|c| c.0).max().unwrap();
        let x_lo = -min_dx;
        let x_hi = W as i32 - 1 - max_dx;
        for x in x_lo..=x_hi {
            // 直落静息盒行
            let mut y = i32::MAX;
            for &(dx, dy) in &cells {
                let col = (x + dx) as usize;
                let mut top = H as i32;
                for r in 0..H {
                    if grid[r][col] != 0 {
                        top = r as i32;
                        break;
                    }
                }
                y = y.min(top - 1 - dy);
            }
            if let Some((g, cleared, piece_rows, piece_in_cleared, eroded_rows)) = stamp_and_clear(grid, kind, rot, x, y) {
                let f = features_after(&g, cleared, &piece_rows, piece_in_cleared, eroded_rows);
                let score_basic = -0.510066 * f.agg_height + 0.760666 * f.cleared - 0.35663 * f.holes - 0.184483 * f.bump;
                let score_dellach = -4.500158825082766 * f.landing_height
                    + 3.4181268101392694 * f.eroded
                    - 3.2178882868487753 * f.row_trans
                    - 9.348695305445199 * f.col_trans
                    - 7.899265427351652 * f.holes
                    - 3.3855972247263626 * f.wells;
                let pen = -1000.0 * f.overflow;
                out.push(Placement { rot, col: x, grid: g, f, score_basic: score_basic + pen, score_dellach: score_dellach + pen });
            }
        }
    }
    out
}

pub(crate) fn grid_of(s: &Snapshot) -> Grid {
    s.board
}

pub(crate) fn max_height(g: &Grid) -> usize {
    let mut m = 0;
    for c in 0..W {
        for r in 0..H {
            if g[r][c] != 0 {
                m = m.max(H - r);
                break;
            }
        }
    }
    m
}

pub(crate) fn ev(t_us: u32, key: Key) -> TimedKey {
    TimedKey { t_us, key }
}

/// 目标切换最小驻留（60Hz 下 0.5 秒）；紧急情况与 Random 目标不受此限。
pub const TARGET_DWELL_FRAMES: u64 = 30;

/// 服务端 ARRCheck 要求同方向横向移动的最小间隔（帧），对应 ruleset 的 `arr_ticks`
/// （默认 2，见 t99-core `Ruleset::arr_ticks`）。左右共用一个计时：反向也多等一帧，
/// 比按方向各记一份更简单且同样不会触发违规。
const ARR_FRAMES: u64 = 2;

/// 横向移动节流：距上次移动不足 `ARR_FRAMES` 帧则本帧不发移动。
/// 返回 true 表示放行并已把本帧记为「上次移动帧」。
pub(crate) fn allow_move(frames: u64, last: &mut u64) -> bool {
    if frames.saturating_sub(*last) < ARR_FRAMES {
        return false;
    }
    *last = frames;
    true
}

/// 是否允许本帧重新评估目标切换。`has_mode` = 已有既定目标；`urgent` = 受威胁且待进垃圾 ≥3。
pub fn switch_due(frames: u64, last_switch: u64, has_mode: bool, urgent: bool) -> bool {
    urgent || !has_mode || frames.saturating_sub(last_switch) >= TARGET_DWELL_FRAMES
}

// ———— 计划执行骨架 ————

#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) sig_pieces: u64,
    pub(crate) sig_kind: u8,
    pub(crate) sig_hold: u8,
    pub(crate) sig_hold_used: bool,
    pub(crate) rot: u8,
    pub(crate) col: i32,
    pub(crate) want_hold: bool,
}

pub(crate) trait PlanBot {
    fn use_hold(&self) -> bool;
    fn use_panic(&self) -> bool;
    /// 返回 (rot, col) 或 None 表示想先 Hold
    fn search(&mut self, s: &Snapshot) -> Option<(u8, i32)>;
    fn plan_mut(&mut self) -> &mut Option<Plan>;
    fn frames_mut(&mut self) -> &mut u64;
    fn last_move_frame_mut(&mut self) -> &mut u64;
    fn target_policy(&mut self, s: &Snapshot) -> Option<Key>;

    fn decide(&mut self, s: &Snapshot) -> Vec<TimedKey> {
        if !s.alive_self {
            return vec![];
        }
        *self.frames_mut() += 1;
        if let Some(k) = self.target_policy(s) {
            return vec![ev(0, k)];
        }
        let Some(a) = s.active else { return vec![] };
        let sig = (
            s.pieces as u64,
            a.kind.id(),
            s.hold.map(|k| k.id()).unwrap_or(0),
            s.hold_used,
        );
        let need = match self.plan_mut() {
            Some(p) => {
                (p.sig_pieces, p.sig_kind, p.sig_hold, p.sig_hold_used) != sig
            }
            None => true,
        };
        if need {
            let plan = match self.search(s) {
                Some((rot, col)) => Plan {
                    sig_pieces: sig.0,
                    sig_kind: sig.1,
                    sig_hold: sig.2,
                    sig_hold_used: sig.3,
                    rot,
                    col,
                    want_hold: false,
                },
                None => Plan {
                    sig_pieces: sig.0,
                    sig_kind: sig.1,
                    sig_hold: sig.2,
                    sig_hold_used: sig.3,
                    rot: 0,
                    col: 0,
                    want_hold: true,
                },
            };
            *self.plan_mut() = Some(plan);
        }
        let plan = match self.plan_mut() {
            Some(p) if (p.sig_pieces, p.sig_kind, p.sig_hold, p.sig_hold_used) == sig => p.clone(),
            _ => return vec![],
        };
        if plan.want_hold {
            return if s.hold_used { vec![] } else { vec![ev(0, Key::Hold)] };
        }
        let diff = (plan.rot as i32 + 4 - (a.rot as i32 % 4)) % 4;
        if diff == 3 {
            return vec![ev(0, Key::RotCcw)];
        }
        if diff != 0 {
            return vec![ev(0, Key::RotCw)];
        }
        if a.x < plan.col {
            let frames = *self.frames_mut();
            if !allow_move(frames, self.last_move_frame_mut()) {
                return vec![];
            }
            return vec![ev(0, Key::RightDown), ev(4000, Key::RightUp)];
        }
        if a.x > plan.col {
            let frames = *self.frames_mut();
            if !allow_move(frames, self.last_move_frame_mut()) {
                return vec![];
            }
            return vec![ev(0, Key::LeftDown), ev(4000, Key::LeftUp)];
        }
        vec![ev(0, Key::HardDrop)]
    }
}

pub(crate) fn best_basic(grid: &Grid, kind: Kind, panic: bool) -> Option<(u8, i32)> {
    let mut best: Option<(f64, u8, i32)> = None;
    for p in placements(grid, kind) {
        let mut sc = p.score_basic;
        if panic {
            sc += p.f.cleared * 8.0;
        }
        if best.map(|(b, _, _)| sc > b).unwrap_or(true) {
            best = Some((sc, p.rot, p.col));
        }
    }
    best.map(|(_, r, c)| (r, c))
}

pub(crate) fn best_dellach(grid: &Grid, kind: Kind, panic: bool) -> Option<(u8, i32, Grid, f64)> {
    let mut best: Option<(f64, u8, i32, Grid)> = None;
    for p in placements(grid, kind) {
        let mut sc = p.score_dellach;
        if panic {
            sc += p.f.cleared * 25.0;
        }
        if best.as_ref().map(|(b, _, _, _)| sc > *b).unwrap_or(true) {
            best = Some((sc, p.rot, p.col, p.grid));
        }
    }
    best.map(|(s, r, c, g)| (r, c, g, s))
}

// ———— 工厂与适配器 ————

pub fn make_bot(tier: &str) -> Box<dyn Bot> {
    match tier {
        "null" => Box::new(NullBot),
        "basic" => Box::new(BasicBot::new()),
        "advanced" => Box::new(AdvancedBot::new()),
        #[cfg(feature = "expert")]
        "expert" => Box::new(ExpertBot::new()),
        #[cfg(not(feature = "expert"))]
        "expert" => panic!("expert: 本源码分发不含 expert 档位（feature `expert` 关闭；二进制内建闭源实现）"),
        _ => panic!("unknown tier: {tier}"),
    }
}

pub struct BotAdapter {
    pub label: String,
    bot: Box<dyn Bot>,
}

impl BotAdapter {
    pub fn new(tier: &str) -> Self {
        BotAdapter { label: tier.to_string(), bot: make_bot(tier) }
    }
}

impl PlayerAdapter for BotAdapter {
    fn name(&self) -> String {
        format!("bot:{}", self.label)
    }
    fn setup(&mut self, hello: &Hello) -> Result<(), AdapterError> {
        self.bot.ready(hello.seat, hello.players, hello.preview);
        let _ = AdapterError::Timeout;
        Ok(())
    }
    fn act(&mut self, snap: &Snapshot) -> Vec<TimedKey> {
        self.bot.decide(snap)
    }
    fn consecutive_misses(&self) -> u32 {
        0
    }
    fn is_dead(&self) -> bool {
        false
    }
    fn finish(&mut self, _result: &ResultMsg) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use t99_core::{MatchConfig, Ruleset};

    #[test]
    fn i_piece_placement_count() {
        let empty: Grid = [[0; W]; H];
        assert_eq!(placements(&empty, Kind::I).len(), 34);
        assert_eq!(placements(&empty, Kind::O).len(), 9);
        assert_eq!(placements(&empty, Kind::T).len(), 34);
    }

    fn run_match(tiers: &[&str], frames: u64, seed: u64) -> t99_core::Match {
        let mut m = t99_core::Match::new(MatchConfig {
            players: tiers.len(),
            seed,
            ruleset: Ruleset::default(),
        });
        let mut bots: Vec<BotAdapter> = tiers.iter().map(|t| BotAdapter::new(t)).collect();
        for _ in 0..frames {
            let inputs: Vec<Vec<TimedKey>> = (0..tiers.len())
                .map(|pid| {
                    let snap = m.snapshot(pid);
                    bots[pid].act(&snap)
                })
                .collect();
            m.frame(&inputs);
            if m.over.is_some() {
                break;
            }
        }
        m
    }

    #[test]
    fn switch_due_dwell() {
        // 初始（无既定目标）立即允许
        assert!(switch_due(1, 0, false, false));
        // 刚切换，29 帧内禁止
        assert!(!switch_due(1 + 29, 1, true, false));
        // 满 30 帧允许
        assert!(switch_due(1 + 30, 1, true, false));
        // 紧急立即允许
        assert!(switch_due(1, 1, true, true));
    }

    #[test]
    fn basic_survives_solo_long() {
        let m = run_match(&["basic"], 10000, 5);
        assert!(m.over.is_none());
        assert!(m.players[0].alive);
    }

    #[test]
    fn basic_beats_null() {
        let m = run_match(&["basic", "null"], 30000, 11);
        assert!(m.over.is_some());
        assert!(m.players[0].alive, "basic 应存活");
        assert!(!m.players[1].alive);
    }

    #[test]
    fn bot_matches_are_deterministic() {
        let a = run_match(&["basic", "advanced"], 9000, 77);
        let b = run_match(&["basic", "advanced"], 9000, 77);
        assert_eq!(a.hash_state(), b.hash_state());
    }

    #[cfg(feature = "expert")]
    #[test]
    fn expert_survives_solo_long() {
        let m = run_match(&["expert"], 10000, 5);
        assert!(m.over.is_none());
        assert!(m.players[0].alive);
    }
}
