//! Master / GrandMaster 大脑：只讲参赛线协议（stdin/stdout 单行 JSON）的外部程序。
//!
//! 与内建 bot 的区别：
//! - 独立可执行文件，与参赛模型提交程序同一准入通道（SubprocessAdapter），同 16ms 帧预算；
//! - 不依赖 t99-core/t99-proto：方块、SRS 踢墙、T-Spin 三角判定、攻击表全部按公开规则复刻；
//! - 2 层束搜索（当前块 + 下一块，含 Hold 分支）、踢墙落点（软降贴地→旋转→硬降的 T-Spin 进槽）、
//!   攻击/抵消/连击/B2B/全消期望收益、危险降权、阶段化目标策略。
//!
//! 档位：Master 稳健攻守平衡；GrandMaster 进攻权重显著更高（攻击/连击/T-Spin 蓄势），
//! 且更早进入徽章收割。两档只差 `Profile`。

use serde::Deserialize;
use std::io::{BufRead, Write};

pub const W: usize = 10;
pub const HID: usize = 4;
pub const ROWS: usize = 24;
pub const MILLI: i64 = 1000;

/// 服务器 hello.protocol
pub const PROTOCOL_MIN: u32 = 5;
pub const PROTOCOL_MAX: u32 = 6;

// ———— 线协议（参赛方视角）———

#[derive(Deserialize, Debug)]
pub struct HelloIn {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub protocol: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FrameIn {
    #[serde(rename = "type")]
    pub kind: String,
    pub n: u64,
    #[serde(default)]
    pub board: Vec<String>,
    pub active: Option<ActiveIn>,
    #[serde(default)]
    pub next: Vec<char>,
    #[serde(default)]
    pub hold: Option<char>,
    #[serde(default)]
    pub hold_used: bool,
    #[serde(default)]
    pub combo: i64,
    #[serde(default)]
    pub b2b: bool,
    #[serde(default)]
    pub garbage: GarbageIn,
    #[serde(default)]
    pub badges: u32,
    #[serde(default)]
    pub target_mode: String,
    #[serde(default)]
    pub lock: LockIn,
    #[serde(default)]
    pub lines: u32,
    #[serde(default)]
    pub sent: u32,
    #[serde(default)]
    pub pieces: u32,
    // v6（缺席时缺省）
    #[serde(default)]
    pub targeted_by: Vec<usize>,
    #[serde(default)]
    pub time_secs_left: u32,
    #[serde(default)]
    pub burn_x10: u16,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ActiveIn {
    pub t: char,
    pub r: u8,
    pub x: i32,
    /// 相对可见区顶部的毫格坐标（可为负）
    pub y_m: i64,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct GarbageIn {
    #[serde(default)]
    pub red: u32,
    #[serde(default)]
    pub yellow: u32,
    #[serde(default)]
    pub gray: u32,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct LockIn {
    #[serde(default)]
    pub ms_left: i64,
    #[serde(default)]
    pub resets: u32,
}

impl FrameIn {
    pub fn pending(&self) -> u32 {
        self.garbage.red + self.garbage.yellow + self.garbage.gray
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ev {
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
    SoftDown,
    SoftUp,
    Cw,
    Ccw,
    Hard,
    Hold,
    TargetBadges,
    TargetKos,
    TargetAttackers,
}

impl Ev {
    fn name(self) -> &'static str {
        match self {
            Ev::LeftDown => "left_down",
            Ev::LeftUp => "left_up",
            Ev::RightDown => "right_down",
            Ev::RightUp => "right_up",
            Ev::SoftDown => "soft_down",
            Ev::SoftUp => "soft_up",
            Ev::Cw => "cw",
            Ev::Ccw => "ccw",
            Ev::Hard => "hard",
            Ev::Hold => "hold",
            Ev::TargetBadges => "target_badges",
            Ev::TargetKos => "target_kos",
            Ev::TargetAttackers => "target_attackers",
        }
    }
}

/// 组一帧输入：同帧键型不得重复、t_us 严格递增 ≤16666（非法整帧作废，务必在此保证）
pub fn pack(n: u64, mut evs: Vec<(u32, Ev)>) -> String {
    evs.sort_by_key(|(t, _)| *t);
    let items: Vec<String> = evs.iter().map(|(t, e)| format!(r#"{{"t_us":{t},"k":"{}"}}"#, e.name())).collect();
    format!(r#"{{"type":"input","n":{n},"events":[{}]}}"#, items.join(","))
}

// ———— 规则复刻：方块 / 踢墙 / 碰撞 / 自旋 ————

pub type Grid = [[u8; W]; ROWS];

fn cells(kind: char, rot: u8) -> [(i32, i32); 4] {
    let base: [(i32, i32); 4] = match kind {
        'I' => [(0, 1), (1, 1), (2, 1), (3, 1)],
        'O' => [(0, 0), (1, 0), (0, 1), (1, 1)],
        'T' => [(1, 0), (0, 1), (1, 1), (2, 1)],
        'S' => [(1, 0), (2, 0), (0, 1), (1, 1)],
        'Z' => [(0, 0), (1, 0), (1, 1), (2, 1)],
        'J' => [(0, 0), (0, 1), (1, 1), (2, 1)],
        _ => [(2, 0), (0, 1), (1, 1), (2, 1)], // L
    };
    let s = if kind == 'I' { 4 } else if kind == 'O' { 2 } else { 3 } as i32;
    let mut out = base;
    for _ in 0..(rot % 4) {
        for c in &mut out {
            *c = (s - 1 - c.1, c.0);
        }
    }
    out
}

/// SRS 踢墙表（屏幕坐标 y 向下），与服务器 t99_core::kicks 完全一致
fn kicks(kind: char, from: u8, to: u8) -> &'static [(i32, i32)] {
    const K0: [(i32, i32); 5] = [(0, 0), (-1, 0), (-1, -1), (0, 2), (-1, 2)];
    const K1: [(i32, i32); 5] = [(0, 0), (1, 0), (1, 1), (0, -2), (1, -2)];
    const K2: [(i32, i32); 5] = [(0, 0), (1, 0), (1, -1), (0, 2), (1, 2)];
    const K3: [(i32, i32); 5] = [(0, 0), (-1, 0), (-1, 1), (0, -2), (-1, -2)];
    const IK0: [(i32, i32); 5] = [(0, 0), (-2, 0), (1, 0), (-2, 1), (1, -2)];
    const IK1: [(i32, i32); 5] = [(0, 0), (2, 0), (-1, 0), (2, -1), (-1, 2)];
    const IK2: [(i32, i32); 5] = [(0, 0), (-1, 0), (2, 0), (-1, -2), (2, 1)];
    const IK3: [(i32, i32); 5] = [(0, 0), (1, 0), (-2, 0), (1, 2), (-2, -1)];
    if kind == 'O' {
        return &[(0, 0)];
    }
    let d = (to + 4 - from % 4) % 4;
    if d == 0 || d == 2 {
        return &[(0, 0)];
    }
    if kind == 'I' {
        match (from % 4, d) {
            (0, 1) | (3, 3) => &IK0,
            (1, 3) | (2, 1) => &IK1,
            (0, 3) | (1, 1) => &IK2,
            _ => &IK3,
        }
    } else {
        match (from % 4, d) {
            (0, 1) | (2, 3) => &K0,
            (1, 1) | (1, 3) => &K1,
            (0, 3) | (2, 1) => &K2,
            _ => &K3,
        }
    }
}

fn collides(g: &Grid, kind: char, rot: u8, x: i32, y: i64) -> bool {
    for (dx, dy) in cells(kind, rot) {
        let col = x + dx;
        if col < 0 || col >= W as i32 {
            return true;
        }
        let bottom = y + dy as i64 * MILLI;
        if bottom + MILLI > (ROWS as i64) * MILLI {
            return true;
        }
        let (r_lo, r_hi) = (bottom.div_euclid(MILLI), (bottom + MILLI - 1).div_euclid(MILLI));
        let mut r = r_lo;
        while r <= r_hi {
            if r >= 0 && (r as usize) < ROWS && g[r as usize][col as usize] != 0 {
                return true;
            }
            r += 1;
        }
    }
    false
}

/// 直落静息位（毫格）。地面夹持语义与服务器一致：活动块永不低于此位。
fn rest_y(g: &Grid, kind: char, rot: u8, x: i32) -> i64 {
    let mut y_max = i64::MAX;
    for (dx, dy) in cells(kind, rot) {
        let col = x + dx;
        let mut top = (ROWS as i64) * MILLI;
        if col >= 0 && (col as usize) < W {
            for r in 0..ROWS {
                if g[r][col as usize] != 0 {
                    top = r as i64 * MILLI;
                    break;
                }
            }
        }
        y_max = y_max.min(top - (dy as i64 + 1) * MILLI);
    }
    y_max
}

/// T-Spin 三角判定（与服务器一致：墙面/地面视为填充，踢 4 升格全旋）
/// 返回 0 无 / 1 mini / 2 全旋
fn spin_at(g: &Grid, rot: u8, x: i32, y: i64, kick_idx: u8) -> u8 {
    let yr = y.div_euclid(MILLI);
    let filled = |cx: i32, cy: i32| -> bool {
        let col = x + cx;
        let row = yr + cy as i64;
        if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
            return true;
        }
        g[row as usize][col as usize] != 0
    };
    let (f0, f1, b0, b1) = match rot % 4 {
        0 => ((0, 0), (2, 0), (0, 2), (2, 2)),
        1 => ((2, 0), (2, 2), (0, 0), (0, 2)),
        2 => ((0, 2), (2, 2), (0, 0), (2, 0)),
        _ => ((0, 0), (0, 2), (2, 0), (2, 2)),
    };
    let f = filled(f0.0, f0.1) as u32 + filled(f1.0, f1.1) as u32;
    let b = filled(b0.0, b0.1) as u32 + filled(b1.0, b1.1) as u32;
    if f == 2 && b >= 1 {
        2
    } else if f >= 1 && b == 2 {
        if kick_idx == 4 {
            2
        } else {
            1
        }
    } else {
        0
    }
}

/// 落章 + 消行（与服务器 lock_piece 的落章一致：越界格跳过；全隐藏判负由调用方过滤）
fn stamp_and_clear(g: &Grid, kind: char, rot: u8, x: i32, y: i64) -> Option<(Grid, u32, u32, bool)> {
    let mut out = *g;
    let mut piece_rows = [0usize; 4];
    let mut n_cells = 0usize;
    let mut visible = false;
    for (dx, dy) in cells(kind, rot) {
        let col = x + dx;
        let row = (y + dy as i64 * MILLI).div_euclid(MILLI);
        if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
            continue;
        }
        if row >= HID as i64 {
            visible = true;
        }
        out[row as usize][col as usize] = 1;
        piece_rows[n_cells] = row as usize;
        n_cells += 1;
    }
    if !visible {
        return None; // 锁定区全在隐藏区 → 服务器判负，候选不可取
    }
    let mut kept: Vec<[u8; W]> = Vec::with_capacity(ROWS);
    let mut cleared = 0u32;
    let mut eroded_rows = 0u32;
    for r in 0..ROWS {
        if out[r].iter().all(|&c| c != 0) {
            cleared += 1;
            if piece_rows[..n_cells].contains(&r) {
                eroded_rows += 1;
            }
        } else {
            kept.push(out[r]);
        }
    }
    let mut g2 = [[0u8; W]; ROWS];
    let base = ROWS - kept.len();
    for (i, row) in kept.iter().enumerate() {
        g2[base + i] = *row;
    }
    // 全消：非满行全空
    let allclear = kept.iter().all(|row| row.iter().all(|&c| c == 0));
    Some((g2, cleared, eroded_rows, allclear))
}

/// 候选落点
#[derive(Clone, Debug)]
pub struct Cand {
    /// 执行前置态（贴地旋转计划需先到 (pre_x, pre_rot) 贴地）
    pub pre_rot: u8,
    pub pre_x: i32,
    /// true = 需要先软降贴地再旋转（踢墙进槽）；false = 空中旋转后硬降即可
    pub floor_rot: bool,
    pub fin_rot: u8,
    pub fin_x: i32,
    pub fin_y: i64,
    pub spin: u8,
    pub grid: Grid,
    pub cleared: u32,
    pub eroded: u32,
    pub allclear: bool,
    pub sent: u32,
    pub score: f64,
}

/// 生成全部候选（直落 + T 踢墙进槽），并按模式上下文评估静态分
pub fn candidates(g: &Grid, kind: char, fr: &FrameIn, p: &Profile, ctx: &ModeCtx) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    let mut seen: Vec<(u8, i32, i64)> = Vec::new();
    let rot_n = if kind == 'O' { 1 } else { 4 };

    // 直落：空中旋转 → 硬降（末次操作是旋转 → T 落进凹槽即成立自旋）
    for rot in 0..rot_n {
        let cs = cells(kind, rot);
        let min_dx = cs.iter().map(|c| c.0).min().unwrap();
        let max_dx = cs.iter().map(|c| c.0).max().unwrap();
        for x in (-min_dx)..=(W as i32 - 1 - max_dx) {
            let y = rest_y(g, kind, rot, x);
            if y >= i64::MAX / 2 {
                continue;
            }
            let key = (rot, x, y);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            push_cand(&mut out, g, kind, fr, p, ctx, rot, x, false, rot, x, y, 0, 255);
        }
    }

    // T 踢墙进槽：贴地旋转（软降→旋转→硬降）。进入悬挑下凹槽只有这一条路。
    if kind == 'T' {
        for pre_rot in 0..4 {
            let cs = cells(kind, pre_rot);
            let min_dx = cs.iter().map(|c| c.0).min().unwrap();
            let max_dx = cs.iter().map(|c| c.0).max().unwrap();
            for pre_x in (-min_dx)..=(W as i32 - 1 - max_dx) {
                let pre_y = rest_y(g, kind, pre_rot, pre_x);
                if pre_y >= i64::MAX / 2 {
                    continue;
                }
                for &dir in &[1u8, 3u8] {
                    let to = (pre_rot + dir) % 4;
                    for (ki, (kx, ky)) in kicks(kind, pre_rot, to).iter().enumerate() {
                        let (x, y) = (pre_x + kx, pre_y + *ky as i64 * MILLI);
                        if collides(g, kind, to, x, y) {
                            continue;
                        }
                        let key = (to, x, y);
                        if seen.contains(&key) {
                            continue;
                        }
                        seen.push(key);
                        // 直落可达的终位已由上面覆盖；踢墙候选只要终位比直落深或成立自旋
                        let straight = rest_y(g, kind, to, x);
                        let spin = spin_at(g, to, x, y, ki as u8);
                        if y > straight || spin > 0 {
                            push_cand(&mut out, g, kind, fr, p, ctx, pre_rot, pre_x, true, to, x, y, spin, ki as u8);
                        }
                    }
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_cand(
    out: &mut Vec<Cand>,
    g: &Grid,
    kind: char,
    fr: &FrameIn,
    p: &Profile,
    ctx: &ModeCtx,
    pre_rot: u8,
    pre_x: i32,
    floor_rot: bool,
    fin_rot: u8,
    fin_x: i32,
    fin_y: i64,
    spin_hint: u8,
    kick_idx: u8,
) {
    let spin = if kind == 'T' {
        if spin_hint != 255 {
            spin_hint
        } else {
            spin_at(g, fin_rot, fin_x, fin_y, kick_idx)
        }
    } else {
        0
    };
    let Some((grid, cleared, eroded, allclear)) = stamp_and_clear(g, kind, fin_rot, fin_x, fin_y) else {
        return;
    };
    let sent = sent_estimate(fr, spin, cleared, allclear);
    let score = evaluate(p, ctx, &grid, fr, fin_y, cleared, eroded, spin, sent, allclear);
    out.push(Cand {
        pre_rot,
        pre_x,
        floor_rot,
        fin_rot,
        fin_x,
        fin_y,
        spin,
        grid,
        cleared,
        eroded,
        allclear,
        sent,
        score,
    });
}

/// 攻击期望：base + B2B + 连击 + 全消（观战不可知的 target_bonus/徽章倍率不计）
fn sent_estimate(fr: &FrameIn, spin: u8, cleared: u32, allclear: bool) -> u32 {
    let base: u32 = match (spin, cleared) {
        (2, 1) => 2,
        (2, 2) => 4,
        (2, 3) => 6,
        (2, _) => 4,
        (1, 2) => 1,
        (1, _) => 0,
        (0, 2) => 1,
        (0, 3) => 2,
        (0, 4) => 4,
        _ => 0,
    };
    let qualifies = spin != 0 || cleared == 4;
    let b2b = if qualifies && fr.b2b { 1 } else { 0 };
    let combo_next = if cleared > 0 && fr.combo + 1 >= 1 {
        COMBO_BONUS[(fr.combo + 1).clamp(0, 10) as usize]
    } else {
        0
    };
    let ac = if allclear { 4 } else { 0 };
    base + b2b + combo_next + ac
}

pub const COMBO_BONUS: [u32; 11] = [0, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5];

// ———— 评估 ————

#[derive(Clone, Copy, Debug)]
pub struct Weights {
    /// 落点高度（行，越高越罚）；直接以负值相加
    pub landing: f64,
    /// 消行侵蚀（eroded_rows）
    pub eroded: f64,
    /// 惩罚量级：行/列转置、洞、洞深、起伏、井、总高
    pub row_t: f64,
    pub col_t: f64,
    pub holes: f64,
    pub hole_depth: f64,
    pub bump: f64,
    pub wells: f64,
    pub agg: f64,
    /// 收益量级：攻击、抵消、井保持、T 槽蓄势、自旋
    pub attack: f64,
    pub cancel: f64,
    /// 负值：断 B2B 链的罚（直接相加）
    pub b2b_break: f64,
    pub well_keep: f64,
    pub tslot: f64,
    pub spin_extra: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub name: &'static str,
    pub beam: usize,
    /// max_h 超过此可见行数 → 生存权重
    pub danger_h: usize,
    /// 待进垃圾 ≥ 此行数 → 生存权重
    pub danger_pending: u32,
    /// 徽章 ≥ 此数且攻势成型时切徽章收割
    pub harvest_badges: u32,
    /// I 柱偏好列（Tetris 井）
    pub well_col: usize,
    pub w: Weights,
    pub w_danger: Weights,
    /// 态势模式表：BUILD/PRESSURE/HUNT/DIG/ENDGAME 各一套权重 + 生存红线
    pub modes: ModeTable,
}

// ———— 态势自适应攻击：模式状态机 ————

/// 态势模式（状态机节点）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 开局/安全：堆叠质量优先，攒 B2B/T 槽资源
    Build,
    /// 被瞄准（targeted_by 非空）：cancel 权重拉满 + TargetAttackers 反击
    Pressure,
    /// 攻势成型（b2b 且 combo≥1，或 badges≥2）：攻击拉满，TargetBadges 收割
    Hunt,
    /// 危险（堆高/待进垃圾超阈）：生存权重，攻击归零
    Dig,
    /// 终局（剩余时间少或烧牌加速）：全力进攻，生存次之（流局按徽章排名）
    Endgame,
}

pub const N_MODES: usize = 5;

impl Mode {
    pub fn idx(self) -> usize {
        match self {
            Mode::Build => 0,
            Mode::Pressure => 1,
            Mode::Hunt => 2,
            Mode::Dig => 3,
            Mode::Endgame => 4,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Build => "BUILD",
            Mode::Pressure => "PRESSURE",
            Mode::Hunt => "HUNT",
            Mode::Dig => "DIG",
            Mode::Endgame => "ENDGAME",
        }
    }
}

/// 评估上下文：一套权重 + 生存红线行数（超过每行额外罚 2）
#[derive(Clone, Copy)]
pub struct ModeCtx<'a> {
    pub w: &'a Weights,
    pub redline: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct ModeTable {
    pub w: [Weights; N_MODES],
    pub redline: [i64; N_MODES],
}

impl ModeTable {
    fn ctx(&self, m: Mode) -> ModeCtx<'_> {
        ModeCtx { w: &self.w[m.idx()], redline: self.redline[m.idx()] }
    }
}

/// 滞回（防抖）参数：模式解除所需的连续帧数
/// PRESSURE 解除：连续 ~0.75s 无人瞄准
pub const PRESSURE_RELEASE_FRAMES: u32 = 45;
/// HUNT 解除：攻势条件消失后再保持 ~0.33s
pub const HUNT_RELEASE_FRAMES: u32 = 20;
/// ENDGAME（终局）下 DIG 进入阈值放宽：全力进攻，更晚承认危险
pub const ENDGAME_DIG_H: i64 = 16;
pub const ENDGAME_DIG_PENDING: u32 = 6;

/// Master：稳健攻守。井策略保 Tetris 弹药，机会性 T-Spin，被瞄反攻，大劣势挖坑求生。
pub fn master_profile() -> Profile {
    let w_m = Weights {
        landing: -4.5,
        eroded: 3.4,
        row_t: 3.217,
        col_t: 9.348,
        holes: 7.899,
        hole_depth: 0.0,
        bump: 0.0,
        wells: 3.385,
        agg: 0.0,
        attack: 2.0,
        cancel: 2.5,
        b2b_break: -5.0,
        well_keep: 0.0,
        tslot: 0.0,
        spin_extra: 2.0,
    };
    let w_dg = Weights {
        landing: -0.8,
        eroded: 6.0,
        row_t: 1.5,
        col_t: 6.0,
        holes: 14.0,
        hole_depth: 0.8,
        bump: 0.5,
        wells: 1.0,
        agg: 0.35,
        attack: 0.8,
        cancel: 4.0,
        b2b_break: -1.0,
        well_keep: 0.0,
        tslot: 0.0,
        spin_extra: 1.0,
    };
    Profile {
        name: "master",
        beam: 8,
        danger_h: 13,
        danger_pending: 4,
        harvest_badges: 4,
        well_col: 9,
        w: w_m,
        w_danger: w_dg,
        // 旧两态语义：非危险一律 normal 权重，危险一律 danger 权重
        modes: ModeTable { w: [w_m, w_m, w_m, w_dg, w_m], redline: [13, 13, 13, 13, 13] },
    }
}

/// GrandMaster：压迫进攻的激进 Master。定位（谱系）：
/// - 「可以不活，其余参赛者必须死」——连击/T-Spin/全清/B2B 全都要发出去尽可能多的行；
/// - 极限危险兜底不弱于 Master 且严格更强（w_danger 同档或更狠），但触发阈值更高——
///   它更晚才承认自己危险，所以实际上未必能活，很大程度依赖其他 GM/M 分摊火力。
pub fn gm_profile() -> Profile {
    let w_m = Weights {
        landing: -4.0,
        eroded: 3.0,
        row_t: 2.6,
        col_t: 7.5,
        holes: 6.0,
        hole_depth: 0.0,
        bump: 0.0,
        wells: 2.5,
        agg: 0.0,
        attack: 5.0,
        cancel: 1.6,
        b2b_break: -6.0,
        well_keep: 0.0,
        tslot: 1.0,
        spin_extra: 4.0,
    };
    let w_dg = Weights {
        // 兜底 ≥ master 的 w_danger：挖行优先、抵消加码、攻击归零
        landing: -0.8,
        eroded: 6.5,
        row_t: 1.5,
        col_t: 6.0,
        holes: 14.0,
        hole_depth: 0.8,
        bump: 0.5,
        wells: 1.0,
        agg: 0.3,
        attack: 0.6,
        cancel: 4.5,
        b2b_break: -1.0,
        well_keep: 0.0,
        tslot: 0.0,
        spin_extra: 1.0,
    };
    Profile {
        name: "gm",
        beam: 8,
        danger_h: 16,
        danger_pending: 8,
        harvest_badges: 2,
        well_col: 9,
        w: w_m,
        w_danger: w_dg,
        modes: ModeTable { w: [w_m, w_m, w_m, w_dg, w_m], redline: [16, 16, 16, 16, 16] },
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    p: &Profile,
    ctx: &ModeCtx,
    g: &Grid,
    fr: &FrameIn,
    fin_y: i64,
    cleared: u32,
    eroded: u32,
    spin: u8,
    sent: u32,
    _allclear: bool,
) -> f64 {
    let w = ctx.w;
    let mut heights = [0i64; W];
    let mut holes = 0i64;
    let mut hole_depth = 0i64;
    let mut row_t = 0i64;
    let mut col_t = 0i64;
    let mut wells = 0i64;
    for c in 0..W {
        let mut top = ROWS as i64;
        for r in 0..ROWS {
            if g[r][c] != 0 {
                top = r as i64;
                break;
            }
        }
        heights[c] = ROWS as i64 - top;
        let mut seen = false;
        let mut depth = 0i64;
        for r in 0..ROWS {
            if g[r][c] != 0 {
                seen = true;
                depth += 1;
            } else if seen {
                holes += 1;
                hole_depth += depth;
            }
        }
        let mut prev = false;
        for r in 0..ROWS {
            let cur = g[r][c] != 0;
            if cur != prev {
                col_t += 1;
            }
            prev = cur;
        }
        if !prev {
            col_t += 1;
        }
    }
    for r in 0..ROWS {
        let mut prev = true;
        for c in 0..W {
            let cur = g[r][c] != 0;
            if cur != prev {
                row_t += 1;
            }
            prev = cur;
        }
        if !prev {
            row_t += 1;
        }
    }
    for c in 0..W {
        let mut run = 0i64;
        for r in 0..ROWS {
            let empty = g[r][c] == 0;
            let lw = c == 0 || g[r][c - 1] != 0;
            let rw = c == W - 1 || g[r][c + 1] != 0;
            if empty && lw && rw {
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
    let landing = ((ROWS as i64) * MILLI - fin_y) as f64 / 1000.0;

    // T 槽蓄势：成品 T 槽（某空格四角三实）数量——给后续 T 存弹药
    let tslot = t_slots(g) as f64;
    // I 井：偏好列比其余最矮列深多少（封顶 8）——Tetris 弹药
    let rim = heights
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != p.well_col)
        .map(|(_, h)| *h)
        .min()
        .unwrap_or(0);
    let well_depth = (rim - heights[p.well_col]).clamp(0, 8);

    let mut sc = w.landing * landing
        + w.eroded * eroded as f64
        - w.row_t * row_t as f64
        - w.col_t * col_t as f64
        - w.holes * holes as f64
        - w.hole_depth * hole_depth as f64
        - w.bump * bump as f64
        - w.wells * wells as f64
        - w.agg * agg as f64;
    // 攻防收益
    sc += w.attack * sent as f64;
    if fr.pending() > 0 {
        sc += w.cancel * cleared as f64; // 消行 1:1 抵消待进垃圾
    }
    if cleared > 0 {
        let qualifies = spin != 0 || cleared == 4;
        if fr.b2b && !qualifies {
            sc += w.b2b_break; // 负权重：断 B2B 链
        }
    }
    sc += w.well_keep * well_depth as f64;
    if spin > 0 {
        sc += w.spin_extra;
    }
    sc += w.tslot * tslot;
    // 生存红线：超过红线行数时额外压制堆高（红线随模式变化）
    if max_h as i64 > ctx.redline {
        sc -= 2.0 * (max_h as i64 - ctx.redline) as f64;
    }
    sc
}

/// 成品 T 槽计数：空格的相邻四角 ≥3 实（可旋转进 T 的凹槽）
fn t_slots(g: &Grid) -> u32 {
    let mut n = 0;
    for r in (HID + 1)..ROWS {
        for c in 0..W {
            if g[r][c] != 0 {
                continue;
            }
            let ri = r as i64;
            let f = |rr: i64, cc: i64| -> bool {
                if cc < 0 || cc >= W as i64 || rr < 0 || rr >= ROWS as i64 {
                    return true;
                }
                g[rr as usize][cc as usize] != 0
            };
            let corners =
                f(ri - 1, c as i64 - 1) as u32 + f(ri - 1, c as i64 + 1) as u32 + f(ri + 1, c as i64 - 1) as u32 + f(ri + 1, c as i64 + 1) as u32;
            if corners >= 3 {
                n += 1;
            }
        }
    }
    n
}

fn max_h(g: &Grid) -> i64 {
    let mut m = 0;
    for c in 0..W {
        for r in 0..ROWS {
            if g[r][c] != 0 {
                m = m.max((ROWS - r) as i64);
                break;
            }
        }
    }
    m
}

// ———— 搜索：root（当前 + Hold）× 束 × 下一块 ————

pub enum Decision {
    Hold,
    Place(Cand),
}

pub fn choose(g: &Grid, fr: &FrameIn, p: &Profile) -> Option<Decision> {
    choose_blocked(g, fr, p, &[])
}

/// skip：本块内已证不可达的 (fin_rot, fin_x) 候选
pub fn choose_blocked(g: &Grid, fr: &FrameIn, p: &Profile, skip: &[(u8, i32)]) -> Option<Decision> {
    // 旧两态语义：danger 一刀切
    let danger = max_h(g) >= p.danger_h as i64 || fr.pending() >= p.danger_pending;
    let ctx = if danger {
        ModeCtx { w: &p.w_danger, redline: p.danger_h as i64 }
    } else {
        ModeCtx { w: &p.w, redline: p.danger_h as i64 }
    };
    choose_ctx(g, fr, p, skip, &ctx)
}

/// 态势模式版搜索：调用方（Brain）按状态机选定 ModeCtx
pub fn choose_ctx(g: &Grid, fr: &FrameIn, p: &Profile, skip: &[(u8, i32)], ctx: &ModeCtx) -> Option<Decision> {
    fr.active.as_ref()?;
    let mut best: Option<(f64, bool, Cand)> = None; // (总分, 是 hold 分支, 候选)

    // root 集合：当前块 + hold 分支（hold 空 → 换入 next[0]）
    let mut roots: Vec<(char, bool)> = Vec::new();
    if let Some(a) = fr.active.as_ref() {
        roots.push((a.t, false));
    }
    if !fr.hold_used {
        if let Some(h) = fr.hold {
            roots.push((h, true));
        } else if let Some(&n0) = fr.next.first() {
            roots.push((n0, true));
        }
    }
    for (k0, is_hold) in roots {
        let mut sorted = candidates(g, k0, fr, p, ctx);
        sorted.retain(|c| !skip.contains(&(c.fin_rot % 4, c.fin_x)));
        if sorted.is_empty() {
            continue;
        }
        sorted.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
        // 尾随块：hold 空换入后，队列首下移一位（尾随用 next[1]）
        let tail_kind = if !is_hold || fr.hold.is_some() {
            fr.next.first().copied()
        } else {
            fr.next.get(1).copied()
        };
        for c in sorted.iter().take(p.beam) {
            let tail = tail_kind
                .map(|tk| {
                    let fr2 = frame_after(fr, c);
                    candidates(&c.grid, tk, &fr2, p, ctx)
                        .iter()
                        .map(|t| t.score)
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .unwrap_or(f64::NEG_INFINITY);
            let combined = 0.35 * c.score + 0.65 * tail.max(c.score * 0.9);
            if best.as_ref().map(|(b, _, _)| combined > *b).unwrap_or(true) {
                best = Some((combined, is_hold, c.clone()));
            }
        }
    }
    let (_, is_hold, c) = best?;
    if is_hold {
        Some(Decision::Hold)
    } else {
        Some(Decision::Place(c))
    }
}

/// 尾随评估用的前瞻帧状态（combo/b2b/pending 随候选更新）
fn frame_after(fr: &FrameIn, c: &Cand) -> FrameIn {
    let mut f = fr.clone();
    if c.cleared > 0 {
        f.combo = fr.combo + 1;
        let qualifies = c.spin != 0 || c.cleared == 4;
        f.b2b = qualifies;
    } else {
        f.combo = -1;
    }
    // 粗略：本块消行先抵消待进垃圾
    let pend = fr.pending().saturating_sub(c.cleared);
    f.garbage.red = 0;
    f.garbage.yellow = pend.min(fr.garbage.yellow);
    f.garbage.gray = pend.saturating_sub(fr.garbage.yellow);
    f
}

// ———— 执行器：(x, rot) 状态 BFS——精确复刻引擎的平移/踢墙转移，逐步从实时状态走到目标态 ————

#[derive(Clone, Debug)]
pub struct Plan {
    pub sig: (u64, char, u8, bool),
    pub cand: Cand,
    pub age: u32,
}

/// BFS 单步动作
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mv {
    L,
    R,
    Cw,
    Ccw,
}

pub struct Brain {
    pub profile: Profile,
    plan: Option<Plan>,
    soft_held: bool,
    last_mode: Option<Ev>,
    frames: u64,
    pub t_spins: u32,
    /// 本块内已证不可达的候选（fin_rot, fin_x）——BFS 失败后拉黑并重规划
    blocked: Vec<(u8, i32)>,
    blocked_sig: (u64, char, u8, bool),
    // ———— 态势状态机 ————
    /// 当前模式
    pub mode: Mode,
    /// 连续未被瞄准的帧数（PRESSURE 滞回）
    untargeted: u32,
    /// 攻势条件已连续消失的帧数（HUNT 滞回）
    hunt_missing: u32,
    /// 终局闩锁：时间/烧牌条件一旦满足便不再退出（时间只减不增）
    endgame_latched: bool,
    /// 模式刚切换：作废当前 plan、立即重选目标
    mode_dirty: bool,
    /// 各模式累计帧数（遥测）
    pub mode_frames: [u32; N_MODES],
}

pub struct Act {
    pub evs: Vec<(u32, Ev)>,
    /// 本帧结束后软降是否应保持按住
    pub soft_hold: bool,
}

/// 引擎等价的旋转转移：在高度 y 处按 kick 表顺序找首个无碰撞姿态（含 x 位移）
fn rotate_at(g: &Grid, kind: char, x: i32, rot: u8, to: u8, y: i64) -> Option<(i32, u8)> {
    for (kx, ky) in kicks(kind, rot, to) {
        let nx = x + kx;
        let ny = y + *ky as i64 * MILLI;
        if !collides(g, kind, to, nx, ny) {
            return Some((nx, to));
        }
    }
    None
}

/// 在高度 y 处的 (x,rot) 平面 BFS。goal 精确匹配 (to_x, to_rot)。
/// spin_last=true 时要求最后一步是旋转（T-Spin 语义：末次操作必须是旋转）。
fn bfs_path(g: &Grid, kind: char, y: i64, from: (i32, u8), to: (i32, u8), spin_last: bool) -> Option<Vec<Mv>> {
    use std::collections::VecDeque;
    if from == to {
        return Some(vec![]);
    }
    let coll_at = |x: i32, rot: u8| collides(g, kind, rot, x, y);
    #[derive(Clone)]
    struct Node {
        x: i32,
        r: u8,
        path: Vec<Mv>,
    }
    let mut seen = [[false; 4]; W + 6]; // x 偏移 -3..W+2 → 索引 x+3
    let idx = |x: i32| (x + 3) as usize;
    let mut q = VecDeque::new();
    seen[idx(from.0)][from.1 as usize] = true;
    q.push_back(Node { x: from.0, r: from.1, path: vec![] });
    while let Some(n) = q.pop_front() {
        let mut succ: Vec<(Mv, i32, u8)> = vec![];
        if !coll_at(n.x - 1, n.r) && n.x - 1 >= -3 {
            succ.push((Mv::L, n.x - 1, n.r));
        }
        if !coll_at(n.x + 1, n.r) && n.x + 1 <= W as i32 + 2 {
            succ.push((Mv::R, n.x + 1, n.r));
        }
        for (mv, dir) in [(Mv::Cw, 1u8), (Mv::Ccw, 3u8)] {
            if let Some((nx, nr)) = rotate_at(g, kind, n.x, n.r, (n.r + dir) % 4, y) {
                succ.push((mv, nx, nr));
            }
        }
        for (mv, nx, nr) in succ {
            let (ix, ir) = (idx(nx), nr as usize);
            if ix >= seen.len() || seen[ix][ir] {
                continue;
            }
            let mut path = n.path.clone();
            path.push(mv);
            let last_rot = matches!(mv, Mv::Cw | Mv::Ccw);
            if (nx, nr) == to {
                if spin_last && !last_rot {
                    // 末步非旋转：丢弃该路径（自旋候选要求末次操作为旋转）
                    continue;
                }
                return Some(path);
            }
            seen[ix][ir] = true;
            q.push_back(Node { x: nx, r: nr, path });
        }
    }
    None
}

impl Brain {
    pub fn new(profile: Profile) -> Self {
        Brain {
            profile,
            plan: None,
            soft_held: false,
            last_mode: None,
            frames: 0,
            t_spins: 0,
            blocked: vec![],
            blocked_sig: (0, '-', 0, false),
            mode: Mode::Build,
            untargeted: 0,
            hunt_missing: u32::MAX,
            endgame_latched: false,
            mode_dirty: true,
            mode_frames: [0; N_MODES],
        }
    }

    /// 态势状态机一步迁移（每帧调用；优先级 DIG > PRESSURE > ENDGAME > HUNT > BUILD）
    fn update_mode(&mut self, fr: &FrameIn, maxh: i64) {
        self.mode_frames[self.mode.idx()] += 1;
        let p = &self.profile;
        // PRESSURE：被瞄立即进入；解除需连续 N 帧无人瞄（防抖）
        let targeted = !fr.targeted_by.is_empty();
        self.untargeted = if targeted { 0 } else { self.untargeted.saturating_add(1) };
        let pressure = targeted || (self.mode == Mode::Pressure && self.untargeted < PRESSURE_RELEASE_FRAMES);

        // ENDGAME：倒计时加速期（剩余 <60s 或 burn 倍率 >1.0x）；单向闩锁
        if !self.endgame_latched && ((fr.time_secs_left > 0 && fr.time_secs_left < 60) || fr.burn_x10 > 10) {
            self.endgame_latched = true;
            self.mode_dirty = true;
        }

        // HUNT：攻势成型（b2b 且 combo≥1，或 badges≥2，或 combo≥2 连击进行中）；解除带滞回
        let hunt_raw = (fr.b2b && fr.combo >= 1) || fr.badges >= 2 || fr.combo >= 2;
        self.hunt_missing = if hunt_raw { 0 } else { self.hunt_missing.saturating_add(1) };
        let hunt = hunt_raw || (self.mode == Mode::Hunt && self.hunt_missing < HUNT_RELEASE_FRAMES);

        // DIG：堆高/待进垃圾双阈值，进出各一档（滞回）；终局下放宽进入阈值（全力进攻）
        let (dig_h, dig_pend) = if self.endgame_latched {
            (ENDGAME_DIG_H, ENDGAME_DIG_PENDING)
        } else {
            (p.danger_h as i64, p.danger_pending)
        };
        let dig = if self.mode == Mode::Dig {
            !(maxh <= dig_h - 2 && fr.pending() + 2 <= dig_pend)
        } else {
            maxh >= dig_h || fr.pending() >= dig_pend
        };

        let m = if dig {
            Mode::Dig
        } else if pressure {
            Mode::Pressure
        } else if self.endgame_latched {
            Mode::Endgame
        } else if hunt {
            Mode::Hunt
        } else {
            Mode::Build
        };
        if m != self.mode {
            self.mode = m;
            self.mode_dirty = true;
        }
    }

    /// 模式 → 目标策略
    fn target_for(&self, _fr: &FrameIn) -> Option<Ev> {
        match self.mode {
            // 被瞄：立即反打攻击者（cancel 1:1 + 反击）
            Mode::Pressure => Some(Ev::TargetAttackers),
            // 攻势成型：收割徽章主（杀人夺徽章，滚雪球）
            Mode::Hunt => Some(Ev::TargetBadges),
            // 挖坑/终局/常规：人头优先（流局按徽章排名，送出/人头决定名次）
            Mode::Dig | Mode::Endgame | Mode::Build => Some(Ev::TargetKos),
        }
    }

    pub fn decide(&mut self, fr: &FrameIn) -> Act {
        self.frames += 1;
        // 态势状态机：每帧迁移（需要堆高，先解析盘面）
        let g = self.grid(fr);
        let maxh = max_h(&g);
        self.update_mode(fr, maxh);
        if self.mode_dirty {
            self.plan = None; // 模式切换 → 按新模式的权重重规划本块
        }
        // 目标策略：模式切换立即生效；每 60 帧重申一次（不影响软降持键）
        if self.mode_dirty || self.frames % 60 == 0 {
            let want = self.target_for(fr);
            self.mode_dirty = false;
            if want != self.last_mode {
                self.last_mode = want;
                if let Some(k) = want {
                    return Act { evs: vec![(0, k)], soft_hold: self.soft_held };
                }
            }
        }
        let Some(a) = fr.active.as_ref() else {
            let act = Act { evs: if self.soft_held { vec![(0, Ev::SoftUp)] } else { vec![] }, soft_hold: false };
            self.soft_held = false;
            self.plan = None;
            return act;
        };

        let sig = (fr.pieces as u64, a.t, fr.hold.map(|k| k as u8).unwrap_or(0), fr.hold_used);
        if self.blocked_sig != sig {
            self.blocked.clear();
            self.blocked_sig = sig;
        }
        let need = match &self.plan {
            Some(p) => p.sig != sig,
            None => true,
        };
        if need {
            let decision = {
                let ctx = self.profile.modes.ctx(self.mode);
                choose_ctx(&g, fr, &self.profile, &self.blocked, &ctx)
            };
            match decision {
                Some(Decision::Hold) => {
                    let was_held = self.soft_held;
                    self.soft_held = false;
                    let evs = if fr.hold_used {
                        vec![] // hold 已用，本帧无操作
                    } else if was_held {
                        vec![(0, Ev::SoftUp), (1200, Ev::Hold)]
                    } else {
                        vec![(0, Ev::Hold)]
                    };
                    return Act { evs, soft_hold: false };
                }
                Some(Decision::Place(c)) => {
                    if c.spin > 0 {
                        self.t_spins += 1;
                    }
                    if std::env::var("T99_BOT_DEBUG").is_ok() {
                        let tops: Vec<String> = {
                            let ctx = self.profile.modes.ctx(self.mode);
                            let mut sorted = candidates(&g, a.t, fr, &self.profile, &ctx);
                            sorted.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
                            sorted
                                .iter()
                                .take(3)
                                .map(|c| format!("(spin{} f{} x{} y{} cl{} sc{:.1})", c.spin, c.fin_rot, c.fin_x, c.fin_y, c.cleared, c.score))
                                .collect()
                        };
                        eprintln!(
                            "[dbg] mode={} piece{} {} hold={:?} next={:?} top: {}",
                            self.mode.name(),
                            fr.pieces,
                            a.t,
                            fr.hold,
                            fr.next.iter().take(3).collect::<Vec<_>>(),
                            tops.join(" ")
                        );
                    }
                    self.plan = Some(Plan { sig, cand: c, age: 0 });
                }
                None => {
                    let was_held = self.soft_held;
                    self.soft_held = false;
                    self.plan = None;
                    // 无路可走：原地硬降（必要时先松软降）
                    return Act {
                        evs: if was_held { vec![(0, Ev::SoftUp), (2000, Ev::Hard)] } else { vec![(0, Ev::Hard)] },
                        soft_hold: false,
                    };
                }
            }
        }
        let Some(plan) = self.plan.as_mut() else { return Act { evs: vec![], soft_hold: false } };
        if plan.sig != sig {
            return Act { evs: vec![], soft_hold: self.soft_held };
        }
        plan.age += 1;
        if plan.age > 60 {
            // 卡死兜底：松软降 + 原地硬降（保证不无限挂起）
            self.plan = None;
            self.soft_held = false;
            return Act { evs: vec![(0, Ev::SoftUp), (2000, Ev::Hard)], soft_hold: false };
        }
        self.exec(fr, a, &g)
    }

    fn grid(&self, fr: &FrameIn) -> Grid {
        let mut g = [[0u8; W]; ROWS];
        for (i, row) in fr.board.iter().take(20).enumerate() {
            for (c, ch) in row.chars().take(W).enumerate() {
                if ch != '.' {
                    g[HID + i][c] = 1;
                }
            }
        }
        g
    }

    fn exec(&mut self, _fr: &FrameIn, a: &ActiveIn, g: &Grid) -> Act {
        let y = a.y_m + HID as i64 * MILLI;
        let cur = (a.x, a.r % 4);
        let (goal, is_spin) = {
            let p = self.plan.as_ref().unwrap();
            ((p.cand.fin_x, p.cand.fin_rot % 4), p.cand.spin > 0 && !p.cand.floor_rot)
        };

        if !self.plan.as_ref().unwrap().cand.floor_rot {
            // 直落计划：BFS 到目标态（自旋候选要求末步为旋转）
            let path = if cur == goal {
                Some(vec![])
            } else {
                bfs_path(g, a.t, y, cur, goal, is_spin)
            };
            match path {
                None => {
                    // 不可达：拉黑候选，下帧重规划
                    self.blocked.push((goal.1, goal.0));
                    self.plan = None;
                    return Act { evs: vec![], soft_hold: false };
                }
                Some(path) if path.is_empty() => {
                    self.plan = None;
                    return Act { evs: vec![(0, Ev::Hard)], soft_hold: false };
                }
                Some(path) => {
                    return self.emit_step(&path, true);
                }
            }
        }

        // 踢墙计划：BFS 到前置态 → 软降贴地 → 贴地旋转进槽 → 硬降
        let (pre, fin_rot) = {
            let p = self.plan.as_ref().unwrap();
            ((p.cand.pre_x, p.cand.pre_rot % 4), p.cand.fin_rot % 4)
        };
        let grounded = y >= rest_y(g, a.t, a.r, a.x);
        if cur != pre {
            let Some(path) = bfs_path(g, a.t, y, cur, pre, false) else {
                self.blocked.push((goal.1, goal.0));
                self.plan = None;
                return Act { evs: vec![], soft_hold: false };
            };
            if !path.is_empty() {
                return self.emit_step(&path, false);
            }
        }
        // 已在 pre 态：贴地 → 旋转 → 硬降
        if !grounded {
            return Act { evs: if self.soft_held { vec![] } else { vec![(0, Ev::SoftDown)] }, soft_hold: true };
        }
        let d = (fin_rot + 4 - cur.1) % 4;
        let rot_ev = if d == 3 { Ev::Ccw } else { Ev::Cw };
        self.plan = None;
        Act { evs: vec![(0, Ev::SoftUp), (1200, rot_ev), (2800, Ev::Hard)], soft_hold: false }
    }

    /// 执行路径首步；hard_on_goal 时该步恰抵达目标则同帧接硬降（踢墙前置阶段不得硬降）
    fn emit_step(&mut self, path: &[Mv], hard_on_goal: bool) -> Act {
        let first = path[0];
        let reaches_goal = path.len() == 1 && hard_on_goal;
        match first {
            Mv::L => {
                let mut evs = vec![(0, Ev::LeftDown), (700, Ev::LeftUp)];
                if reaches_goal {
                    evs.push((2000, Ev::Hard));
                    self.plan = None;
                }
                Act { evs, soft_hold: false }
            }
            Mv::R => {
                let mut evs = vec![(0, Ev::RightDown), (700, Ev::RightUp)];
                if reaches_goal {
                    evs.push((2000, Ev::Hard));
                    self.plan = None;
                }
                Act { evs, soft_hold: false }
            }
            Mv::Cw | Mv::Ccw => {
                let ev = if first == Mv::Cw { Ev::Cw } else { Ev::Ccw };
                let mut evs = vec![(0, ev)];
                if reaches_goal {
                    evs.push((1600, Ev::Hard));
                    self.plan = None;
                }
                Act { evs, soft_hold: false }
            }
        }
    }
}

// ———— 主循环 ————

pub fn run(profile: Profile) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut brain = Brain::new(profile);
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.is_empty() {
            break; // 管道关闭
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v["type"].as_str().unwrap_or("") {
            "hello" => {
                if let Ok(h) = serde_json::from_str::<HelloIn>(&line) {
                    if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&h.protocol) {
                        eprintln!("[t99-bot-{}] 协议不兼容: {}", profile.name, h.protocol);
                        std::process::exit(2);
                    }
                }
                let _ = writeln!(out, r#"{{"type":"ready"}}"#);
                let _ = out.flush();
            }
            "frame" => {
                let Ok(fr) = serde_json::from_str::<FrameIn>(&line) else { continue };
                let act = brain.decide(&fr);
                // 软降状态切换维护：上一帧按住、本帧不再按住 → 补 SoftUp
                let mut evs = act.evs;
                if brain.soft_held && !act.soft_hold && !evs.iter().any(|(_, e)| *e == Ev::SoftUp) {
                    evs.insert(0, (0, Ev::SoftUp));
                    brain.soft_held = false;
                }
                if std::env::var("T99_BOT_TRACE").is_ok() {
                    let names: Vec<&str> = evs.iter().map(|(_, e)| e.name()).collect();
                    eprintln!("[tr] f{} act={}x{}y{} hold={:?} send={:?}", fr.n, fr.active.as_ref().map(|a| a.t).unwrap_or('-'), fr.active.as_ref().map(|a| a.x).unwrap_or(-1), fr.active.as_ref().map(|a| a.y_m).unwrap_or(-1), fr.hold, names);
                }
                let _ = writeln!(out, "{}", pack(fr.n, evs));
                let _ = out.flush();
            }
            "result" => {
                let mf = brain.mode_frames;
                eprintln!(
                    "[t99-bot-{}] place={:?} lines={} sent={} pieces={} spin计划={} 模式帧 B/P/H/D/E={}/{}/{}/{}/{}",
                    profile.name,
                    v["place"].as_u64(),
                    v["lines"].as_u64().unwrap_or(0),
                    v["sent"].as_u64().unwrap_or(0),
                    v["pieces"].as_u64().unwrap_or(0),
                    brain.t_spins,
                    mf[0],
                    mf[1],
                    mf[2],
                    mf[3],
                    mf[4],
                );
                let _ = out.flush();
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Grid {
        [[0u8; W]; ROWS]
    }

    fn fr_of(g: &Grid) -> FrameIn {
        let mut board = Vec::new();
        for r in HID..ROWS {
            let s: String = (0..W).map(|c| if g[r][c] != 0 { '#' } else { '.' }).collect();
            board.push(s);
        }
        serde_json::from_value::<FrameIn>(serde_json::json!({
            "type": "frame", "n": 0, "board": board,
            "active": {"t": "T", "r": 0, "x": 3, "y_m": 0},
            "next": ["L"], "combo": -1, "b2b": false,
            "garbage": {"red": 0, "yellow": 0, "gray": 0},
            "lock": {"ms_left": 0, "resets": 0},
            "lines": 0, "sent": 0, "pieces": 0,
        }))
        .unwrap()
    }

    /// 经典 TSD 槽：row23 全满缺 col4（nub 孔），row22 为 3 宽坑道，row21 双侧悬挑 col2/col5。
    /// 唯一进入方式：T 竖条（rot1）在坑道口贴地，CW 旋转经踢 (1,1) 下沉入槽 → 全旋。
    fn tsd_stack() -> Grid {
        let mut g = empty();
        for c in 0..W {
            if c != 4 {
                g[23][c] = 1;
            }
            if !(3..=5).contains(&c) {
                g[22][c] = 1;
            }
        }
        g[21][2] = 1;
        g[21][5] = 1;
        g
    }

    fn ctx_of(p: &Profile, m: Mode) -> ModeCtx<'_> {
        p.modes.ctx(m)
    }

    /// TSD 槽（贴地踢墙进槽）：必须由踢墙候选被找到，且判为全旋
    #[test]
    fn finds_kick_tsd() {
        let g = tsd_stack();
        let fr = fr_of(&g);
        let p = master_profile();
        let cands = candidates(&g, 'T', &fr, &p, &ctx_of(&p, Mode::Build));
        let tsd = cands.iter().find(|c| c.spin == 2 && c.fin_x == 3 && c.fin_y == 21000 && c.fin_rot == 2);
        assert!(tsd.is_some(), "应找到踢墙进槽的 TSD 候选，实际候选 {} 个", cands.len());
        let c = tsd.unwrap();
        assert!(c.floor_rot, "TSD 候选必须走贴地旋转");
        assert_eq!(c.cleared, 2);
    }

    #[test]
    fn spin_corners_match_engine() {
        // 空场 T 直落至底，四角全空 → 无自旋；三面围死的凹槽 → 全旋
        let g = empty();
        assert_eq!(spin_at(&g, 2, 3, 22000, 0), 0);
        let mut g2 = empty();
        for c in 0..W {
            g2[23][c] = 1;
        }
        g2[23][4] = 0;
        g2[22][3] = 1;
        g2[22][5] = 1;
        // rot2 T at x=3，nub 下到 (4,23)（唯一空位），本体 (3..5,22)
        assert_eq!(spin_at(&g2, 2, 3, 22000, 0), 2);
    }

    #[test]
    fn tsd_sends_four_and_scores() {
        let g = tsd_stack();
        let fr = fr_of(&g);
        let p = master_profile();
        let cands = candidates(&g, 'T', &fr, &p, &ctx_of(&p, Mode::Build));
        let tsd = cands.iter().find(|c| c.spin == 2 && c.cleared == 2).unwrap();
        assert_eq!(tsd.sent, 4);
        // TSD 应显著优于普通直落堆放（攻防收益入账）
        let plain = cands
            .iter()
            .filter(|c| c.spin == 0 && c.cleared == 0)
            .map(|c| c.score)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(tsd.score > plain, "TSD({}) 应优于普通堆放({})", tsd.score, plain);
    }

    #[test]
    fn choose_prefers_tsd_when_available() {
        let g = tsd_stack();
        let mut fr = fr_of(&g);
        fr.active = Some(ActiveIn { t: 'T', r: 0, x: 3, y_m: 0 });
        let d = choose(&g, &fr, &gm_profile()).expect("应有决策");
        match d {
            Decision::Place(c) => assert!(
                c.spin == 2 && c.cleared == 2,
                "GM 应选 TSD（spin={} cleared={} fin=({},{},{})）",
                c.spin,
                c.cleared,
                c.fin_rot,
                c.fin_x,
                c.fin_y
            ),
            Decision::Hold => panic!("不应 Hold"),
        }
    }
}

// ———— 本实验策略：态势自适应攻击（多态状态机）———

/// 自适应攻击档：五态各一套权重。
/// 谱系：BUILD=Master 稳健堆叠（+T 槽蓄势）；PRESSURE=cancel 拉满快攻反击；
/// HUNT=GM 级攻击权重收割；DIG=Master 兜底挖行；ENDGAME=全力进攻（流局按徽章排名）。
pub fn adaptive_profile() -> Profile {
    let build = Weights {
        landing: -4.5,
        eroded: 3.6,
        row_t: 3.217,
        col_t: 9.348,
        holes: 7.899,
        hole_depth: 0.0,
        bump: 0.0,
        wells: 3.385,
        agg: 0.0,
        attack: 3.2, // R2：比 Master 更进攻——BUILD 是主态，火力从这里来
        cancel: 2.5,
        b2b_break: -5.0,
        well_keep: 0.0,
        tslot: 0.5, // 比 Master 多一点 T 槽蓄势：为 HUNT 备弹药
        spin_extra: 2.0,
    };
    let pressure = Weights {
        landing: -3.6,
        eroded: 4.5,
        row_t: 2.8,
        col_t: 8.5,
        holes: 8.5,
        hole_depth: 0.3,
        bump: 0.2,
        wells: 2.5,
        agg: 0.0,
        attack: 3.5, // R2：反击也要送出行（cancel 1:1 已由消行天然完成）
        cancel: 4.5, // R3：4.0→4.5——10p 焦火下抵消优先，保住存活率
        b2b_break: -3.0,
        well_keep: 0.0,
        tslot: 0.2,
        spin_extra: 3.0,
    };
    let hunt = Weights {
        landing: -4.0,
        eroded: 3.2,
        row_t: 2.6,
        col_t: 7.5,
        holes: 6.0,
        hole_depth: 0.0,
        bump: 0.0,
        wells: 2.5,
        agg: 0.0,
        attack: 5.5, // GM 级攻击
        cancel: 2.0,
        b2b_break: -6.0, // B2B 链就是火力，绝不轻易断
        well_keep: 0.0,
        tslot: 1.0,
        spin_extra: 4.0,
    };
    let dig = Weights {
        landing: -0.8,
        eroded: 6.0,
        row_t: 1.5,
        col_t: 6.0,
        holes: 14.0,
        hole_depth: 0.8,
        bump: 0.5,
        wells: 1.0,
        agg: 0.35,
        attack: 0.5, // 攻击近归零：活下来才有名次
        cancel: 5.0, // R3：4.5→5.0——挖掘期被瞄时全力抵消
        b2b_break: -1.0,
        well_keep: 0.0,
        tslot: 0.0,
        spin_extra: 1.0,
    };
    let endgame = Weights {
        landing: -3.5,
        eroded: 4.2,
        row_t: 2.2,
        col_t: 7.0,
        holes: 7.0,
        hole_depth: 0.2,
        bump: 0.2,
        wells: 2.0,
        agg: 0.0,
        attack: 6.0, // 全力进攻：送出/人头决定名次
        cancel: 3.0,
        b2b_break: -4.0,
        well_keep: 0.0,
        tslot: 0.5,
        spin_extra: 4.0,
    };
    let mut p = master_profile();
    p.name = "adaptive";
    p.danger_pending = 3; // R3：待进垃圾 ≥3 即挖（原 4）——10p 焦火下提前防御
    p.modes = ModeTable {
        w: [build, pressure, hunt, dig, endgame],
        redline: [13, 13, 14, 13, 14], // ENDGAME/HUNT 允许略高堆形（红线罚每行 2）
    };
    p
}

/// 状态机迁移单元测试（无盘面依赖，直接喂帧）
#[test]
fn mode_machine_transitions() {
    use serde_json::json;
    let mk = |extra: serde_json::Value| -> FrameIn {
        let mut v = json!({
            "type": "frame", "n": 0,
            "combo": -1, "b2b": false, "badges": 0,
            "garbage": {"red": 0, "yellow": 0, "gray": 0},
            "targeted_by": [], "time_secs_left": 200, "burn_x10": 10,
        });
        if let (Some(base), Some(add)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in add {
                base.insert(k.clone(), val.clone());
            }
        }
        serde_json::from_value(v).unwrap()
    };
    let mut b = Brain::new(adaptive_profile());
    let g = empty_grid();
    // 1) 常规 → BUILD
    let fr = mk(json!({}));
    b.update_mode(&fr, max_h(&g));
    assert_eq!(b.mode, Mode::Build);
    // 2) 被瞄 → PRESSURE
    let fr = mk(json!({"targeted_by": [1, 2]}));
    b.update_mode(&fr, max_h(&g));
    assert_eq!(b.mode, Mode::Pressure);
    // 3) 停瞄但未过滞回期 → 保持 PRESSURE
    for _ in 0..(PRESSURE_RELEASE_FRAMES - 1) {
        let fr = mk(json!({}));
        b.update_mode(&fr, max_h(&g));
    }
    assert_eq!(b.mode, Mode::Pressure, "滞回期内不得退出 PRESSURE");
    let fr = mk(json!({}));
    b.update_mode(&fr, max_h(&g));
    assert_eq!(b.mode, Mode::Build, "滞回期满应退出");
    // 4) b2b+combo → HUNT
    let fr = mk(json!({"b2b": true, "combo": 1}));
    b.update_mode(&fr, max_h(&g));
    assert_eq!(b.mode, Mode::Hunt);
    // 5) 堆高超阈 → DIG（优先级最高）
    let fr = mk(json!({"targeted_by": [1], "b2b": true, "combo": 1}));
    b.update_mode(&fr, 14);
    assert_eq!(b.mode, Mode::Dig);
    // 6) 终局闩锁：时间 <60s → ENDGAME（DIG 解除后）
    for _ in 0..(PRESSURE_RELEASE_FRAMES + HUNT_RELEASE_FRAMES + 1) {
        let fr = mk(json!({"time_secs_left": 50}));
        b.update_mode(&fr, 10);
    }
    assert_eq!(b.mode, Mode::Endgame);
    // 7) 终局下 DIG 阈值放宽：16 行才进 DIG
    let fr = mk(json!({"time_secs_left": 50}));
    b.update_mode(&fr, 14);
    assert_eq!(b.mode, Mode::Endgame, "终局 14 行不应进 DIG");
    b.update_mode(&fr, 16);
    assert_eq!(b.mode, Mode::Dig, "终局 16 行应进 DIG");
}

#[cfg(test)]
fn empty_grid() -> Grid {
    [[0u8; W]; ROWS]
}

/// 本实验策略的 profile 入口（改这里 + 需要的话改策略代码）
pub fn profile() -> Profile {
    adaptive_profile()
}
