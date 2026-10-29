//! 内置 Bot：与子进程程序同视角（只读自身快照）、同输入协议、同 DAS 限速。
//!
//! 档位（能力超集递进）：
//! - NullBot    什么都不做（地板校准）
//! - BasicBot   单块贪心（4 特征），静态随机目标，不用 Hold/预览
//! - AdvancedBot Dellacherie 6 特征 + Hold + next[0]，反击/残血目标切换，危险转平堆
//! - ExpertBot  2 深度束搜索 + Hold 分支 + 阶段化目标策略

use t99_core::{Key, Snapshot, TimedKey, Kind, W, H};
use t99_proto::{AdapterError, Hello, PlayerAdapter, ResultMsg};

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

fn grid_of(s: &Snapshot) -> Grid {
    s.board
}

fn max_height(g: &Grid) -> usize {
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

fn ev(t_us: u32, key: Key) -> TimedKey {
    TimedKey { t_us, key }
}

/// 攻防目标策略（徽章转移感知）：
/// 被瞄 → 集中反打徽章最肥的瞄准者（击杀吞徽章，双倍收益）；
/// 无人瞄 → 有徽章领先者（≥3）就围猎（制造转移链），否则抢人头。
/// 平摊式的 TargetAttackers 会让火力全被 1:1 抵消，谁也打不死谁。
/// 99 人基准实测：在“待进未满”的目标里均匀随机，优于 Kos/Badges/Spread 等集中策略。
/// 集中攻击会撞上 12 行待进上限，并被目标的消行抵消，实际进场反而更少。
pub fn desired_target(_s: &Snapshot) -> Key {
    Key::TargetRandom
}
// 注：反攻条件加 pending≥3（真正受威胁才反打），否则坚持围猎，避免「永远反攻」僵局。

/// 目标切换最小驻留（60Hz 下 0.5 秒）；紧急情况与 Random 目标不受此限。
pub const TARGET_DWELL_FRAMES: u64 = 30;

/// 是否允许本帧重新评估目标切换。`has_mode` = 已有既定目标；`urgent` = 受威胁且待进垃圾 ≥3。
pub fn switch_due(frames: u64, last_switch: u64, has_mode: bool, urgent: bool) -> bool {
    urgent || !has_mode || frames.saturating_sub(last_switch) >= TARGET_DWELL_FRAMES
}

// ———— NullBot ————

pub struct NullBot;

impl Bot for NullBot {
    fn name(&self) -> &'static str {
        "null"
    }
    fn decide(&mut self, _s: &Snapshot) -> Vec<TimedKey> {
        vec![]
    }
}

// ———— 计划执行骨架 ————

#[derive(Clone, Debug)]
struct Plan {
    sig_pieces: u64,
    sig_kind: u8,
    sig_hold: u8,
    sig_hold_used: bool,
    rot: u8,
    col: i32,
    want_hold: bool,
}

trait PlanBot {
    fn use_hold(&self) -> bool;
    fn use_panic(&self) -> bool;
    /// 返回 (rot, col) 或 None 表示想先 Hold
    fn search(&mut self, s: &Snapshot) -> Option<(u8, i32)>;
    fn plan_mut(&mut self) -> &mut Option<Plan>;
    fn frames_mut(&mut self) -> &mut u64;
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
            return vec![ev(0, Key::RightDown), ev(4000, Key::RightUp)];
        }
        if a.x > plan.col {
            return vec![ev(0, Key::LeftDown), ev(4000, Key::LeftUp)];
        }
        vec![ev(0, Key::HardDrop)]
    }
}

fn best_basic(grid: &Grid, kind: Kind, panic: bool) -> Option<(u8, i32)> {
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

fn best_dellach(grid: &Grid, kind: Kind, panic: bool) -> Option<(u8, i32, Grid, f64)> {
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

// ———— BasicBot ————

pub struct BasicBot {
    plan: Option<Plan>,
    frames: u64,
}

impl BasicBot {
    pub fn new() -> Self {
        BasicBot { plan: None, frames: 0 }
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
            return vec![ev(0, Key::RightDown), ev(4000, Key::RightUp)];
        }
        if a.x > plan.col {
            return vec![ev(0, Key::LeftDown), ev(4000, Key::LeftUp)];
        }
        vec![ev(0, Key::HardDrop)]
    }
}

// ———— AdvancedBot ————

pub struct AdvancedBot {
    plan: Option<Plan>,
    frames: u64,
    last_mode: Option<Key>,
    last_switch: u64,
}

impl AdvancedBot {
    pub fn new() -> Self {
        AdvancedBot { plan: None, frames: 0, last_mode: None, last_switch: 0 }
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
    fn target_policy(&mut self, s: &Snapshot) -> Option<Key> {
        let pending = s.garbage_red + s.garbage_yellow + s.garbage_gray;
        let urgent = !s.targeted_by.is_empty() && pending >= 3;
        let want = desired_target(s);
        // Random 目标不做驻留限制（仅 BasicBot 开局用，Advanced 预留）
        if !switch_due(self.frames, self.last_switch, self.last_mode.is_some(), urgent) && want != Key::TargetRandom {
            return None;
        }
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

// ———— ExpertBot ————

pub struct ExpertBot {
    plan: Option<Plan>,
    frames: u64,
    last_mode: Option<Key>,
    last_switch: u64,
}

impl ExpertBot {
    pub fn new() -> Self {
        ExpertBot { plan: None, frames: 0, last_mode: None, last_switch: 0 }
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
        let grid = grid_of(s);
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
    fn target_policy(&mut self, s: &Snapshot) -> Option<Key> {
        let pending = s.garbage_red + s.garbage_yellow + s.garbage_gray;
        let urgent = !s.targeted_by.is_empty() && pending >= 3;
        let want = desired_target(s);
        if !switch_due(self.frames, self.last_switch, self.last_mode.is_some(), urgent) && want != Key::TargetRandom {
            return None;
        }
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

// ———— 工厂与适配器 ————

pub fn make_bot(tier: &str) -> Box<dyn Bot> {
    match tier {
        "null" => Box::new(NullBot),
        "basic" => Box::new(BasicBot::new()),
        "advanced" => Box::new(AdvancedBot::new()),
        "expert" => Box::new(ExpertBot::new()),
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
}
