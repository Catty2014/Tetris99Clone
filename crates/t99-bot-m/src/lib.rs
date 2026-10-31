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

/// 目标切换最小驻留（60Hz 下 0.5 秒）；紧急情况不受此限。
pub const TARGET_DWELL_FRAMES: u64 = 30;

/// 服务端 ARRCheck 要求同方向横向移动的最小间隔（帧），对应 ruleset 的 `arr_ticks`
/// （默认 2，见 t99-core `Ruleset::arr_ticks`）。左右共用一个计时，反向也多等一帧。
pub const ARR_FRAMES: u64 = 2;

/// 服务器 hello.protocol
pub const PROTOCOL_MIN: u32 = 5;
pub const PROTOCOL_MAX: u32 = 8;

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
    /// 本帧归属席位（v8 起与 player_id 字符串分开）
    #[serde(default, alias = "player_id")]
    pub seat: usize,
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
    /// v7：全员徽章数组（index=pid；徽章随 KO 转移）
    #[serde(default)]
    pub badges_all: Vec<u32>,
    /// 自己的徽章单位数（与 badges 的档位区分）
    #[serde(default)]
    pub badge_units: u32,
    /// 存活玩家 pid 列表
    #[serde(default)]
    pub alive: Vec<usize>,
    /// 场上存活他人最高可见堆（KOs 权重用）
    #[serde(default)]
    pub max_stack: u16,
    /// 全员待进垃圾行数（index=pid）；缺席时为空 → 不做饱和过滤
    #[serde(default)]
    pub pending_all: Vec<u32>,
    /// 待进队列上限（行）；0 表示未知
    #[serde(default)]
    pub garbage_cap: u32,
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

    /// 该玩家还能否收下攻击：待进 < 队列上限才有空间。
    /// 上限为 0（旧服务端未下发）或待进数组缺席时视为有空间。
    pub fn has_garbage_room(&self, pid: usize) -> bool {
        if self.garbage_cap == 0 {
            return true;
        }
        self.pending_all.get(pid).copied().unwrap_or(0) < self.garbage_cap
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
    TargetRandom,
    /// 手动锁定单个玩家（载荷 pid）
    TargetPlayer(u16),
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
            Ev::TargetRandom => "target_random",
            Ev::TargetPlayer(_) => "target_player",
        }
    }
}

/// 组一帧输入：同帧键型不得重复、t_us 严格递增 ≤16666（非法整帧作废，务必在此保证）
pub fn pack(n: u64, mut evs: Vec<(u32, Ev)>) -> String {
    evs.sort_by_key(|(t, _)| *t);
    let items: Vec<String> = evs
        .iter()
        .map(|(t, e)| match e {
            Ev::TargetPlayer(v) => format!(r#"{{"t_us":{t},"k":"target_player","v":{v}}}"#),
            _ => format!(r#"{{"t_us":{t},"k":"{}"}}"#, e.name()),
        })
        .collect();
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
    /// 本落点污染井列的格子数（井型模式：非 I 且井开放时计数）
    pub pollute: i64,
    /// 本落点填进成品 T 槽的格子数（非自旋落子）
    pub cav_fill: i64,
    pub score: f64,
}

/// 生成全部候选（直落 + T 踢墙进槽），并按 profile 评估静态分。
/// redline_h：生存红线（行）——超高时按超出量额外罚分；钓鱼时被抬高。
pub fn candidates(g: &Grid, kind: char, fr: &FrameIn, p: &Profile, danger: bool, redline_h: usize) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    let mut seen: Vec<(u8, i32, i64)> = Vec::new();
    let rot_n = if kind == 'O' { 1 } else { 4 };
    // T 槽只在 T 即将到手时才有意义：T 在 Hold、或 next 前 2 位。
    // 否则不奖励搭槽，避免为了远期收益先造一堆洞。
    let t_near = fr.hold == Some('T') || fr.next.iter().take(3).any(|&c| c == 'T');
    let pre_cav = if t_near && p.weights(danger).cavity_fill > 0.0 {
        let well = if p.well_mode && !danger { Some(p.well_col) } else { None };
        cavity_mask(g, well)
    } else {
        [[false; W]; ROWS]
    };

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
            push_cand(&mut out, g, kind, fr, p, danger, redline_h, rot, x, false, rot, x, y, 255, 255, t_near, &pre_cav);
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
                            push_cand(&mut out, g, kind, fr, p, danger, redline_h, pre_rot, pre_x, true, to, x, y, spin, ki as u8, t_near, &pre_cav);
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
    danger: bool,
    redline_h: usize,
    pre_rot: u8,
    pre_x: i32,
    floor_rot: bool,
    fin_rot: u8,
    fin_x: i32,
    fin_y: i64,
    spin_hint: u8,
    kick_idx: u8,
    t_near: bool,
    pre_cav: &[[bool; W]; ROWS],
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
    let pollute = well_pollution(p, g, kind, fin_rot, fin_x, danger, cleared);
    // 已有成品 T 槽被非自旋落子填掉：每格重罚；T 自旋进槽是正常消费。
    let cav_fill = if !t_near || (kind == 'T' && spin > 0) {
        0
    } else {
        cells(kind, fin_rot)
            .iter()
            .filter(|(dx, dy)| {
                let col = fin_x + dx;
                let row = (fin_y + *dy as i64 * MILLI).div_euclid(MILLI);
                if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
                    return false;
                }
                pre_cav[row as usize][col as usize]
            })
            .count() as i64
    };
    let score = evaluate(
        p, &grid, fr, fin_y, cleared, eroded, spin, sent, allclear, danger, redline_h, kind, pollute,
        cav_fill, t_near,
    );
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
        pollute,
        cav_fill,
        score,
    });
}

/// 井列高度
fn col_height(g: &Grid, c: usize) -> i64 {
    for r in 0..ROWS {
        if g[r][c] != 0 {
            return (ROWS - r) as i64;
        }
    }
    0
}

/// 非 I 块对井列的污染格数：井开放（相对其余最矮列有深度）且本块不消 4 行时，
/// 凡落入井列即计污染（含架桥——桥面同样挡住 I 的入口）。danger 态关闭（保命优先允许填井）。
fn well_pollution(p: &Profile, g: &Grid, kind: char, fin_rot: u8, fin_x: i32, danger: bool, cleared: u32) -> i64 {
    if !p.well_mode || danger || kind == 'I' || cleared >= 4 {
        return 0;
    }
    let rim = (0..W).filter(|&c| c != p.well_col).map(|c| col_height(g, c)).min().unwrap_or(0);
    if rim - col_height(g, p.well_col) < 1 {
        return 0; // 井未开放（与堆面齐平），无井可污染
    }
    cells(kind, fin_rot).iter().filter(|(dx, _)| fin_x + dx == p.well_col as i32).count() as i64
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
    /// T 槽空腔：T 在 2 块内时，已被屋顶覆盖且 T 落下能消 2+ 行的空格，每格奖励
    pub cavity_keep: f64,
    /// 非自旋落子填进成品 T 槽：每格罚
    pub cavity_fill: f64,
    /// 非 I 块污染井列：每个入井格的罚（仅 well_mode 且非 danger）
    pub pollute: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub name: &'static str,
    pub beam: usize,
    /// 预读搜索层数（含当前块，≤1+预览数；层内全局束 = beam）
    pub depth: usize,
    /// max_h 超过此可见行数 → 生存权重
    pub danger_h: usize,
    /// 待进垃圾 ≥ 此行数 → 生存权重
    pub danger_pending: u32,
    /// 徽章 ≥ 此数且攻势成型时切徽章收割
    pub harvest_badges: u32,
    /// I 柱偏好列（Tetris 井）
    pub well_col: usize,
    /// KO 钓鱼：被瞄准时维持攻势权重（消行=抵消+反打双赚），直到 feign_cap 行
    pub feign: bool,
    pub feign_cap: usize,
    /// true = 井型工业模式：井列从通用堆面指标中掩码，由 well_keep/pollute 单独管理
    pub well_mode: bool,
    /// true = 徽章收割需 b2b+combo 门槛（稳健档）；false = 达到徽章数即收割（激进档）
    pub harvest_gate: bool,
    pub w: Weights,
    pub w_danger: Weights,
}

impl Profile {
    fn weights(&self, danger: bool) -> &Weights {
        if danger {
            &self.w_danger
        } else {
            &self.w
        }
    }
}

/// Master（选秀冠军：lab/well B2B Tetris 井型工业）。
/// 右列(9)专用 I 井：其余 9 列快速平整堆放喂井，Hold 轮转等 I，
/// I 进井 = Tetris(4) + b2b(+1) = 5 行主攻击引擎；非 I 污染井列重罚（含架桥），
/// 井深≥4 时非 I 硬过滤禁入井列；危险切 master 兜底档（更早触发）。
pub fn master_profile() -> Profile {
    Profile {
        name: "master",
        beam: 6,
        depth: 6,
        danger_h: 12,
        danger_pending: 4,
        harvest_badges: 2,
        well_col: 9,
        feign: false,
        feign_cap: 21,
        well_mode: true,
        harvest_gate: true,
        w: Weights {
            landing: -3.0,
            eroded: 3.0,
            row_t: 2.8,
            col_t: 8.0,
            holes: 7.0,
            hole_depth: 0.6,
            bump: 1.2, // 平整喂井
            wells: 3.0,
            agg: 0.15, // 堆面整体高度温和压制
            attack: 6.0,
            cancel: 1.5,
            b2b_break: -9.0, // 链是引擎，断链重罚
            well_keep: 3.5,  // 每行井深 +3.5
            tslot: 0.5,
            spin_extra: 2.0,
            cavity_keep: 2.0,
            cavity_fill: 6.0,
            pollute: 10.0, // 每个入井格 -10（整块 -40 上下）
        },
        w_danger: Weights {
            landing: -0.8,
            eroded: 6.0,
            row_t: 1.5,
            col_t: 6.0,
            holes: 14.0,
            hole_depth: 0.8,
            bump: 1.0,
            wells: 1.0,
            agg: 0.35,
            attack: 0.8,
            cancel: 4.0,
            b2b_break: -1.0,
            well_keep: 0.0, // 危险时不奖励井（掩码也关闭，井洞按普通洞处理→自然填平）
            tslot: 0.0,
            spin_extra: 1.0,
            cavity_keep: 0.0,
            cavity_fill: 2.0,
            pollute: 0.0,
        },
    }
}

/// GrandMaster（选秀冠军：lab/berserk v7 极限激进档）。
/// 定位：「可以不活，其余参赛者必须死」——attack 20、危险阈值 20 行、待进垃圾容忍 16，
/// 徽章 ≥1 即收割（无 b2b/combo 门槛）；保留 GM 强兜底（w_danger 原样）但触发更晚——
/// 实际上未必能活，火力分摊依赖其他 GM/M。10 人实杀场送出 1.4-2× 于旧 GM。
pub fn gm_profile() -> Profile {
    berserk7()
}

/// Berserk 谱系公共骨架：GM 兜底不变（w_danger 原样保留），进攻档拉满。
/// - attack ≥8（GM 5.0）、危险阈值 ≥18 行（GM 16）、待进垃圾容忍 ≥10（GM 8）；
/// - 徽章 ≥1 即切 TargetBadges（去掉了 GM 的 b2b/combo 门槛）；
/// - feign=true：KO 钓鱼——被瞄准时保持攻势权重（消行=抵消+反打双赚），直到 feign_cap 行。
fn berserk(name: &'static str, attack: f64, danger_h: usize, danger_pending: u32, feign: bool) -> Profile {
    // 以旧 GM 档（稳健进攻）为基础拉满进攻：兜底 w_danger 原样保留
    let mut p = Profile {
        name: "gm",
        beam: 6,
        depth: 6,
        danger_h: 16,
        danger_pending: 8,
        harvest_badges: 2,
        well_col: 9,
        feign: false,
        feign_cap: 21,
        well_mode: false,
        harvest_gate: true,
        w: Weights {
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
            cavity_keep: 3.0,
            cavity_fill: 8.0,
            pollute: 0.0,
        },
        w_danger: Weights {
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
            cavity_keep: 0.0,
            cavity_fill: 2.0,
            pollute: 0.0,
        },
    };
    p.name = name;
    p.danger_h = danger_h;
    p.danger_pending = danger_pending;
    p.harvest_badges = 1;
    p.feign = feign;
    p.feign_cap = 21;
    p.harvest_gate = false;
    p.w.attack = attack;
    p.w.landing = -3.4;
    p.w.eroded = 2.6;
    p.w.row_t = 2.4;
    p.w.col_t = 7.0;
    p.w.holes = 5.0;
    p.w.wells = 2.0;
    p.w.cancel = 0.8;
    p.w.spin_extra = 5.0;
    p.w.tslot = 1.0;
    p
}

/// B1「激进」：attack 8、18 行/10 垃圾才转生存。
pub fn berserk1() -> Profile {
    berserk("berserk1", 8.0, 18, 10, false)
}

/// B2「极限」：attack 12、20 行/12 垃圾才转生存（仍保留 GM 强兜底）。
pub fn berserk2() -> Profile {
    berserk("berserk2", 12.0, 20, 12, false)
}

/// B3「极限+钓鱼」：B2 + 被瞄时钓鱼（保持攻势直到 21 行）。
pub fn berserk3() -> Profile {
    berserk("berserk3", 12.0, 20, 12, true)
}

/// B4「钓鱼对照」：与 B3 同参数但永不钓鱼——分离「钓鱼」变量的净收益。
pub fn berserk4() -> Profile {
    berserk("berserk4", 12.0, 21, 12, false)
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    p: &Profile,
    g: &Grid,
    fr: &FrameIn,
    fin_y: i64,
    cleared: u32,
    eroded: u32,
    spin: u8,
    sent: u32,
    _allclear: bool,
    danger: bool,
    redline_h: usize,
    _kind: char,
    pollute: i64,
    cav_fill: i64,
    t_near: bool,
) -> f64 {
    let w = p.weights(danger);
    // 井型工业：非 danger 时把专用井列从通用堆面指标中掩码（按「已填实」处理），
    // 井的健康度由 well_keep（深度奖励）+ pollute（污染罚）单独管理。
    let wcm = p.well_mode && !danger;
    let mut heights = [0i64; W];
    let mut holes = 0i64;
    let mut hole_depth = 0i64;
    let mut hole_m = [[false; W]; ROWS];
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
        if wcm && c == p.well_col {
            continue; // 井列不参与 holes/col_t
        }
        let mut seen = false;
        let mut depth = 0i64;
        for r in 0..ROWS {
            if g[r][c] != 0 {
                seen = true;
                depth += 1;
            } else if seen {
                holes += 1;
                hole_depth += depth;
                hole_m[r][c] = true;
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
            let cur = (wcm && c == p.well_col) || g[r][c] != 0; // 井列按填实计，消去跨井行转置
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
        if wcm && c == p.well_col {
            continue; // 专用井不算杂井
        }
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
    let agg: i64 = heights
        .iter()
        .enumerate()
        .filter(|(i, _)| !(wcm && *i == p.well_col))
        .map(|(_, h)| *h)
        .sum();
    let bump: i64 = (0..W - 1)
        .filter(|&i| !(wcm && (i == p.well_col || i + 1 == p.well_col))) // 井深差不算崎岖
        .map(|i| (heights[i] - heights[i + 1]).abs())
        .sum();
    let max_h = *heights.iter().max().unwrap();
    let landing = ((ROWS as i64) * MILLI - fin_y) as f64 / 1000.0;

    // T 槽蓄势：成品 T 槽（某空格四角三实）数量——给后续 T 存弹药
    let tslot = t_slots(g) as f64;
    // 成品 T 槽：T 落下即消 2+ 行、且被屋顶覆盖的空格。只在 T 即将到手时计入，
    // 并按槽位价值奖励、从普通洞罚中剔除，否则搭槽永远是「先亏洞」。
    let (cav_n, cav_holes) = if t_near {
        let well = if wcm { Some(p.well_col) } else { None };
        let cav = cavity_mask(g, well);
        let mut n = 0i64;
        let mut h = 0i64;
        for (r, row) in cav.iter().enumerate() {
            for (c, &is_cav) in row.iter().enumerate() {
                if !is_cav {
                    continue;
                }
                n += 1;
                if hole_m[r][c] {
                    h += 1;
                }
            }
        }
        (n, h)
    } else {
        (0, 0)
    };
    let holes_excess = (holes - cav_holes).max(0);
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
        - w.holes * holes_excess as f64
        - w.hole_depth * hole_depth as f64
        - w.bump * bump as f64
        - w.wells * wells as f64
        - w.agg * agg as f64;
    // 攻防收益（T99 阻挡规则）：有暂缓垃圾时攻击全部转为阻挡，不发送——
    // 真正发出去的才算进攻，阻挡量按攻击值计（大消行阻挡更多 → 鼓励拼 Tetris/TSD 挖洞）
    let block = fr.pending().min(sent);
    sc += w.attack * (sent - block) as f64;
    sc += w.cancel * block as f64;
    if cleared > 0 {
        let qualifies = spin != 0 || cleared == 4;
        if fr.b2b && !qualifies {
            sc += w.b2b_break; // 负权重：断 B2B 链
        }
    }
    sc += w.well_keep * well_depth as f64;
    sc -= w.pollute * pollute as f64; // 非 I 污染井列（含架桥）
    if spin > 0 && cleared > 0 {
        sc += w.spin_extra;
    }
    if !danger {
        sc += w.tslot * tslot;
    }
    if t_near {
        // 槽位价值 + 填槽惩罚（危险档 cavity_keep 为 0，只剩轻微填充罚）
        sc += w.cavity_keep * cav_n.min(12) as f64;
        sc -= w.cavity_fill * cav_fill as f64;
    }
    // 生存红线：超高时额外压制堆高（钓鱼时红线抬到 feign_cap）
    if max_h as usize > redline_h {
        sc -= 2.0 * (max_h - redline_h as i64) as f64;
    }
    sc
}

/// 成品 T 槽掩码：以「被屋顶覆盖的空格」为锚点反推 T 落位，
/// 只标记 T 落下即消 2 行以上（TSD/TST）的空腔。well：井列不算槽。
fn cavity_mask(g: &Grid, well: Option<usize>) -> [[bool; W]; ROWS] {
    let mut m = [[false; W]; ROWS];
    let filled = |r: i64, c: i64| -> bool {
        if r < 0 || r >= ROWS as i64 || c < 0 || c >= W as i64 {
            return true;
        }
        g[r as usize][c as usize] != 0
    };
    for r in HID..ROWS {
        for c in 0..W {
            if g[r][c] != 0 || !filled(r as i64 - 1, c as i64) {
                continue;
            }
            for rot in 0..4u8 {
                for (dx, dy) in cells('T', rot) {
                    let x = c as i32 - dx;
                    let y = r as i64 * MILLI - dy as i64 * MILLI;
                    if collides(g, 'T', rot, x, y) {
                        continue;
                    }
                    // 必须稳定：还能下落说明不是槽
                    if !collides(g, 'T', rot, x, y + MILLI) {
                        continue;
                    }
                    if spin_at(g, rot, x, y, 255) == 0 {
                        continue;
                    }
                    let Some((_, cleared, _, _)) = stamp_and_clear(g, 'T', rot, x, y) else {
                        continue;
                    };
                    if cleared < 2 {
                        continue;
                    }
                    for (mx, my) in cells('T', rot) {
                        let col = x + mx;
                        let row = (y + my as i64 * MILLI).div_euclid(MILLI);
                        if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
                            continue;
                        }
                        if Some(col as usize) == well {
                            continue;
                        }
                        m[row as usize][col as usize] = true;
                    }
                }
            }
        }
    }
    m
}

/// 可执行的成品 T 槽：与 cavity_mask 不同，这里要求 T 能从静息位直落或踢墙真正进入，
/// 否则会奖励“T 根本进不去的死洞”。当前未接入评分，留给模板式搭建使用。
#[allow(dead_code)]
fn reachable_tsd_mask(g: &Grid, well: Option<usize>) -> [[bool; W]; ROWS] {
    let mut m = [[false; W]; ROWS];
    let mut mark = |rot: u8, x: i32, y: i64| {
        for (dx, dy) in cells('T', rot) {
            let col = x + dx;
            let row = (y + dy as i64 * MILLI).div_euclid(MILLI);
            if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
                continue;
            }
            if Some(col as usize) == well {
                continue;
            }
            m[row as usize][col as usize] = true;
        }
    };
    let clears2 = |rot: u8, x: i32, y: i64| -> bool {
        matches!(stamp_and_clear(g, 'T', rot, x, y), Some((_, n, _, _)) if n >= 2)
    };
    // 直落：静息位 + 三角成立 + 消 2 行
    for rot in 0..4u8 {
        let cs = cells('T', rot);
        let min_dx = cs.iter().map(|c| c.0).min().unwrap();
        let max_dx = cs.iter().map(|c| c.0).max().unwrap();
        for x in (-min_dx)..=(W as i32 - 1 - max_dx) {
            let y = rest_y(g, 'T', rot, x);
            if y >= i64::MAX / 2 || collides(g, 'T', rot, x, y) {
                continue;
            }
            if spin_at(g, rot, x, y, 255) == 0 {
                continue;
            }
            if clears2(rot, x, y) {
                mark(rot, x, y);
            }
        }
    }
    // 踢墙：贴地静息后 CW/CCW 旋转进悬挑下的槽
    for pre_rot in 0..4u8 {
        let cs = cells('T', pre_rot);
        let min_dx = cs.iter().map(|c| c.0).min().unwrap();
        let max_dx = cs.iter().map(|c| c.0).max().unwrap();
        for pre_x in (-min_dx)..=(W as i32 - 1 - max_dx) {
            let pre_y = rest_y(g, 'T', pre_rot, pre_x);
            if pre_y >= i64::MAX / 2 {
                continue;
            }
            for &dir in &[1u8, 3u8] {
                let to = (pre_rot + dir) % 4;
                for (ki, (kx, ky)) in kicks('T', pre_rot, to).iter().enumerate() {
                    let (x, y) = (pre_x + kx, pre_y + *ky as i64 * MILLI);
                    if collides(g, 'T', to, x, y) {
                        continue;
                    }
                    if spin_at(g, to, x, y, ki as u8) == 0 {
                        continue;
                    }
                    if clears2(to, x, y) {
                        mark(to, x, y);
                    }
                }
            }
        }
    }
    m
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

/// 危险判定 + 生存红线（钓鱼：被瞄准且堆高未破 cap → 维持攻势权重，红线抬到 feign_cap）。
pub fn danger_state(g: &Grid, fr: &FrameIn, p: &Profile) -> (bool, usize) {
    let mh = max_h(g);
    let feigning = p.feign && !fr.targeted_by.is_empty() && mh < p.feign_cap as i64 && fr.pending() < p.danger_pending;
    if feigning {
        (false, p.feign_cap)
    } else {
        (mh >= p.danger_h as i64 || fr.pending() >= p.danger_pending, p.danger_h)
    }
}

/// skip：本块内已证不可达的 (fin_rot, fin_x) 候选
/// 多层束搜索：吃满预览队列（≤depth 层，层内全局束）。
/// 每节点累计分 = 自身静态分 + 0.5×父累计（兼顾即时攻击与后续布局）；
/// 返回最优路径的首步。Hold 只在根层分支。确定性：无墙钟、无随机。
pub fn choose_blocked(g: &Grid, fr: &FrameIn, p: &Profile, skip: &[(u8, i32)]) -> Option<Decision> {
    fr.active.as_ref()?;
    let (danger, redline) = danger_state(g, fr, p);
    let depth = p.depth.max(1).min(fr.next.len() + 1);

    #[derive(Clone)]
    struct Node {
        grid: Grid,
        fr2: FrameIn,
        sum: f64,
        levels: usize,
    }
    // 分支价值：已达层数的静态分和 + 剩余层数×平均层分（外推），消除变深偏置
    let value = |sum: f64, levels: usize| -> f64 {
        if levels == 0 {
            return sum;
        }
        sum + (depth - levels) as f64 * (sum / levels as f64)
    };

    // 根层：当前块 + hold 分支
    let mut roots: Vec<(char, bool, Vec<char>)> = Vec::new();
    if let Some(a) = fr.active.as_ref() {
        roots.push((a.t, false, fr.next.clone()));
    }
    if !fr.hold_used {
        if let Some(h) = fr.hold {
            roots.push((h, true, fr.next.clone()));
        } else if let Some(&n0) = fr.next.first() {
            roots.push((n0, true, fr.next.iter().skip(1).copied().collect()));
        }
    }

    let mut best: Option<(f64, bool, Cand)> = None; // (累计分, 是 hold 分支, 首步候选)
    for (k0, is_hold, queue) in roots {
        let mut level: Vec<(Node, Cand)> = Vec::new(); // (节点, 根首步)
        let mut sorted = candidates(g, k0, fr, p, danger, redline);
        sorted.retain(|c| !skip.contains(&(c.fin_rot % 4, c.fin_x)));
        // 井等 I：井深 ≥4（一发 Tetris 就绪）且非危险态时，非 I 禁止进入井列（硬过滤，空则回退全量）
        if p.well_mode && !danger && k0 != 'I' {
            let rim = (0..W).filter(|&c| c != p.well_col).map(|c| col_height(g, c)).min().unwrap_or(0);
            let well_depth = rim - col_height(g, p.well_col);
            if well_depth >= 4 {
                let filtered: Vec<Cand> = sorted.iter().filter(|c| c.pollute == 0).cloned().collect();
                if !filtered.is_empty() {
                    sorted = filtered;
                }
            }
        }
        if sorted.is_empty() {
            continue;
        }
        sorted.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
        for c in sorted.into_iter().take(p.beam) {
            let fr2 = frame_after(fr, &c);
            level.push((Node { grid: c.grid.clone(), fr2, sum: c.score, levels: 1 }, c));
        }
        // 逐层展开（层内按外推价值全局截束）
        for li in 0..(depth - 1) {
            let Some(&tk) = queue.get(li) else { break };
            let mut next_level: Vec<(Node, Cand)> = Vec::new();
            for (node, root) in &level {
                for c in candidates(&node.grid, tk, &node.fr2, p, danger, redline) {
                    next_level.push((
                        Node {
                            grid: c.grid.clone(),
                            fr2: frame_after(&node.fr2, &c),
                            sum: node.sum + c.score,
                            levels: node.levels + 1,
                        },
                        root.clone(),
                    ));
                }
            }
            if next_level.is_empty() {
                break;
            }
            next_level.sort_by(|a, b| {
                value(b.0.sum, b.0.levels)
                    .partial_cmp(&value(a.0.sum, a.0.levels))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            next_level.truncate(p.beam);
            level = next_level;
        }
        // 本根的最优分支（外推价值）
        if let Some((node, root)) = level
            .iter()
            .max_by(|a, b| value(b.0.sum, b.0.levels).partial_cmp(&value(a.0.sum, a.0.levels)).unwrap_or(std::cmp::Ordering::Equal))
        {
            let v = value(node.sum, node.levels);
            if best.as_ref().map(|(b, _, _)| v > *b).unwrap_or(true) {
                best = Some((v, is_hold, root.clone()));
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
    // 粗略：本块攻击先用于阻挡暂缓垃圾
    let pend = fr.pending().saturating_sub(c.sent.min(fr.pending()));
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
    /// 上次发出横向移动（LeftDown/RightDown）所在帧，ARR 节流用
    last_move_frame: u64,
    /// 上次目标切换（或首次评估）所在帧
    last_switch: u64,
    /// master 加权策略的当前模式 / 上次送出计数 / 随机源
    weighted_mode: Option<Ev>,
    last_sent: u32,
    rng: u64,
    pub t_spins: u32,
    /// 本块内已证不可达的候选（fin_rot, fin_x）——BFS 失败后拉黑并重规划
    blocked: Vec<(u8, i32)>,
    blocked_sig: (u64, char, u8, bool),
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
        // 自旋要求末次操作是旋转；已经在终点时无法再补一次旋转
        return if spin_last { None } else { Some(vec![]) };
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

/// master 的加权目标：Random 100；Badges (max-own)/max*100；KOs (max_stack-10)*10；Attackers min(aim,12)/6*100。
fn pick_weighted_mode(fr: &FrameIn, rng: &mut u64) -> Ev {
    let own = fr.badge_units as f64;
    let max_b = fr
        .alive
        .iter()
        .filter(|&&p| p != fr.seat)
        .map(|&p| fr.badges_all.get(p).copied().unwrap_or(0))
        .max()
        .unwrap_or(0) as f64;
    let w_badges = if max_b > 0.0 {
        ((max_b - own).max(0.0) / max_b) * 100.0
    } else {
        0.0
    };
    let w_kos = ((fr.max_stack as f64 - 10.0) * 10.0).max(0.0);
    let w_att = fr.targeted_by.len().min(12) as f64 / 6.0 * 100.0;
    let weights = [
        (Ev::TargetRandom, 100.0),
        (Ev::TargetBadges, w_badges),
        (Ev::TargetKos, w_kos),
        (Ev::TargetAttackers, w_att),
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
    Ev::TargetRandom
}

impl Brain {
    pub fn new(profile: Profile) -> Self {
        Brain {
            profile,
            plan: None,
            soft_held: false,
            last_mode: None,
            frames: 0,
            last_move_frame: 0,
            last_switch: 0,
            weighted_mode: None,
            last_sent: 0,
            rng: 0,
            t_spins: 0,
            blocked: vec![],
            blocked_sig: (0, '-', 0, false),
        }
    }

    pub fn decide(&mut self, fr: &FrameIn) -> Act {
        self.frames += 1;
        // 目标策略：最小驻留 30 帧；被瞄且待进垃圾 ≥3 时紧急豁免；首次评估立即执行
        let urgent = !fr.targeted_by.is_empty() && fr.pending() >= 3;
        let sent_changed = self.profile.name == "master" && fr.sent != self.last_sent;
        let gm_six = (self.profile.name == "gm" || self.profile.name.starts_with("berserk"))
            && fr.targeted_by.len() >= 6;
        if urgent
            || self.last_mode.is_none()
            || self.frames.saturating_sub(self.last_switch) >= TARGET_DWELL_FRAMES
            || sent_changed
            || gm_six
        {
            let want = self.target_mode(fr);
            if self.last_mode != want {
                self.last_mode = want;
                self.last_switch = self.frames;
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

        let g = self.grid(fr);
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
            match choose_blocked(&g, fr, &self.profile, &self.blocked) {
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
                        if std::env::var("T99_SPIN_TRACE").is_ok() {
                            eprintln!(
                                "[spin-plan] seat={} piece={} kind={} rot={} x={} y={} cleared={} floor={} pre=({},{})",
                                fr.seat, a.t, c.spin, c.fin_rot, c.fin_x, c.fin_y, c.cleared, c.floor_rot, c.pre_rot, c.pre_x
                            );
                        }
                    }
                    if std::env::var("T99_BOT_DEBUG").is_ok() {
                        let (dbg_danger, dbg_redline) = danger_state(&g, fr, &self.profile);
                        let mut sorted = candidates(&g, a.t, fr, &self.profile, dbg_danger, dbg_redline);
                        sorted.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal));
                        let tops: Vec<String> = sorted
                            .iter()
                            .take(3)
                            .map(|c| format!("(spin{} f{} x{} y{} cl{} sc{:.1})", c.spin, c.fin_rot, c.fin_x, c.fin_y, c.cleared, c.score))
                            .collect();
                        eprintln!("[dbg] piece{} {} hold={:?} next={:?} top: {}", fr.pieces, a.t, fr.hold, fr.next.iter().take(3).collect::<Vec<_>>(), tops.join(" "));
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

    fn target_mode(&mut self, fr: &FrameIn) -> Option<Ev> {
        // master：按奖励函数权重，每次成功送出后重抽目标模式。
        if self.profile.name == "master" {
            if self.rng == 0 {
                self.rng = 0x9E37_79B9_7F4A_7C15 ^ (fr.seat as u64 + 1);
            }
            if fr.sent != self.last_sent || self.weighted_mode.is_none() {
                self.last_sent = fr.sent;
                self.weighted_mode = Some(pick_weighted_mode(fr, &mut self.rng));
            }
            return self.weighted_mode;
        }
        // GM：被 6 人以上瞄准时无论待进多少，优先全线反击。
        if fr.targeted_by.len() >= 6 {
            return Some(Ev::TargetAttackers);
        }
        let dbg = std::env::var("T99_BOT_TARGET").is_ok();
        let trace = |m: &str| if dbg { eprintln!("[target:{} f{}] {}", self.profile.name, fr.n, m); };
        // 攻防策略（徽章转移感知）：
        // 被瞄 → 集中反打徽章最肥的瞄准者（击杀吞其全部徽章，双倍收益）；
        // 无人瞄 → 场上徽章领先者 ≥ harvest_badges 即围猎（TargetBadges，制造转移链）；
        // 否则抢人头。平摊式 TargetAttackers 会让火力全被抵消，谁也打不死谁。
        let p = &self.profile;
        let leader = fr
            .badges_all
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != fr.seat)
            .max_by_key(|&(_, &b)| b);
        // GM（「可以不活，其余参赛者必须死」）：围猎永远优先于反攻；
        // Master：仅当真正受威胁（待进垃圾 ≥3 行）才反攻，否则坚持围猎/进攻。
        let counter_first = if p.name == "gm" || p.name.starts_with("berserk") { false } else { fr.pending() >= 3 };
        if counter_first && !fr.targeted_by.is_empty() {
            // 待进已满的瞄准者收不下攻击，优先反打仍有余量者；全员饱和才退回原逻辑
            let roomy: Vec<usize> = fr
                .targeted_by
                .iter()
                .copied()
                .filter(|&p| fr.has_garbage_room(p))
                .collect();
            let pool: &[usize] = if roomy.is_empty() { &fr.targeted_by } else { &roomy };
            let fattest = pool
                .iter()
                .copied()
                .max_by_key(|&p| fr.badges_all.get(p).copied().unwrap_or(0))
                .unwrap_or(0);
            trace(&format!("被瞄{} 待进{} → 反打 pid{} (徽章{})", fr.targeted_by.len(), fr.pending(), fattest, fr.badges_all.get(fattest).copied().unwrap_or(0)));
            return Some(Ev::TargetPlayer(fattest as u16));
        }
        if let Some((lp, &b)) = leader {
            if b >= p.harvest_badges {
                trace(&format!("围猎领先者 pid{} (徽章{}) 被瞄{}", lp, b, fr.targeted_by.len()));
                return Some(Ev::TargetBadges);
            }
        }
        // 兜底：TargetKos——引擎会集火堆最高（最接近死亡）者，pack 击杀产生首枚徽章，
        // 徽章经济启动后由围猎接管（「永远反攻」只会让火力被抵消，谁也拿不到击杀）。
        Some(Ev::TargetKos)
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
            ((p.cand.fin_x, p.cand.fin_rot % 4), p.cand.spin > 0 && p.cand.cleared > 0 && !p.cand.floor_rot)
        };

        if !self.plan.as_ref().unwrap().cand.floor_rot {
            // 直落计划：BFS 到目标态（自旋候选要求末步为旋转）
            let path = if cur == goal {
                if is_spin {
                    // 已经在自旋终点：必须补一次原地旋转，下一帧再 BFS 转回终点。
                    let cw = rotate_at(g, a.t, cur.0, cur.1, (cur.1 + 1) % 4, y);
                    if cw.is_some() {
                        return Act { evs: vec![(0, Ev::Cw)], soft_hold: false };
                    }
                    let ccw = rotate_at(g, a.t, cur.0, cur.1, (cur.1 + 3) % 4, y);
                    if ccw.is_some() {
                        return Act { evs: vec![(0, Ev::Ccw)], soft_hold: false };
                    }
                    // 无法旋转：这次自旋不成立，撤销计数后直接硬降
                    self.t_spins = self.t_spins.saturating_sub(1);
                    self.plan = None;
                    return Act { evs: vec![(0, Ev::Hard)], soft_hold: false };
                }
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
                if std::env::var("T99_SPIN_TRACE").is_ok() {
                    eprintln!("[exec-fail] kick BFS cur=({},{}) pre=({},{}) y={}", cur.0, cur.1, pre.0, pre.1, y);
                }
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
            Mv::L | Mv::R => {
                // ARR 节流：同向（按简化口径，任意方向）移动间隔不足时本帧不发移动。
                // 不丢 plan、不碰 soft_hold 状态，下帧继续走同一步。
                if self.frames.saturating_sub(self.last_move_frame) < ARR_FRAMES {
                    return Act { evs: vec![], soft_hold: self.soft_held };
                }
                self.last_move_frame = self.frames;
                let (down, up) = if first == Mv::L {
                    (Ev::LeftDown, Ev::LeftUp)
                } else {
                    (Ev::RightDown, Ev::RightUp)
                };
                let mut evs = vec![(0, down), (700, up)];
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

/// 入口：--connect <url> --id <id> --key <key> → WS 玩家模式（lobby）；否则 stdin 子进程模式
pub fn main_entry(profile: Profile) {
    let args: Vec<String> = std::env::args().collect();
    let get = |name: &str| -> Option<String> {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == name {
                return it.next().cloned();
            }
        }
        None
    };
    if let (Some(url), Some(id), Some(key)) = (get("--connect"), get("--id"), get("--key")) {
        run_ws(profile, &url, &id, &key);
    } else {
        run(profile);
    }
}

/// WS 玩家模式：连接 lobby 服务器（/ws?id=&key=），与参赛提交程序完全同通道。
pub fn run_ws(profile: Profile, url: &str, id: &str, key: &str) {
    use tungstenite::Message;
    let full = format!("{}/ws?id={id}&key={key}", url.trim_end_matches('/'));
    let req = tungstenite::client::IntoClientRequest::into_client_request(full.as_str())
        .expect("无效 URL");
    let mut ws = tungstenite::connect(req).expect("连接服务器失败").0;
    let mut brain = Brain::new(profile);
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let line = t.to_string();
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                match v["type"].as_str().unwrap_or("") {
                    "hello" if v.get("frame_hz").is_some() => {
                        let proto = v["protocol"].as_u64().unwrap_or(0) as u32;
                        if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&proto) {
                            eprintln!("[t99-bot-{}] 协议不兼容: {}", profile.name, proto);
                            std::process::exit(2);
                        }
                        let _ = ws.send(Message::Text(r#"{"type":"ready"}"#.into()));
                    }
                    "hello" => {
                        eprintln!(
                            "[t99-bot-{}] 对局 {}（seat {}）",
                            profile.name,
                            v["match_id"].as_u64().unwrap_or(0),
                            v["seat"].as_i64().unwrap_or(-1)
                        );
                    }
                    "frame" => {
                        let Ok(fr) = serde_json::from_str::<FrameIn>(&line) else { continue };
                        let act = brain.decide(&fr);
                        let mut evs = act.evs;
                        if brain.soft_held && !act.soft_hold && !evs.iter().any(|(_, e)| *e == Ev::SoftUp) {
                            evs.insert(0, (0, Ev::SoftUp));
                            brain.soft_held = false;
                        }
                        let _ = ws.send(Message::Text(pack(fr.n, evs).into()));
                    }
                    "result" => {
                        // 广播 result（无 place 字段）先到：跳过，等逐玩家 result
                        if v["place"].is_null() {
                            continue;
                        }
                        eprintln!(
                            "[t99-bot-{}] place={:?} lines={} sent={} pieces={} spin计划={}",
                            profile.name,
                            v["place"].as_u64(),
                            v["lines"].as_u64().unwrap_or(0),
                            v["sent"].as_u64().unwrap_or(0),
                            v["pieces"].as_u64().unwrap_or(0),
                            brain.t_spins,
                        );
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
            Ok(Message::Ping(d)) => {
                let _ = ws.send(Message::Pong(d));
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[t99-bot-{}] 连接断开: {e}", profile.name);
                std::process::exit(1);
            }
        }
    }
}

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
                eprintln!(
                    "[t99-bot-{}] place={:?} lines={} sent={} pieces={} spin计划={}",
                    profile.name,
                    v["place"].as_u64(),
                    v["lines"].as_u64().unwrap_or(0),
                    v["sent"].as_u64().unwrap_or(0),
                    v["pieces"].as_u64().unwrap_or(0),
                    brain.t_spins,
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

    #[test]
    fn spin_path_requires_final_rotation() {
        let g = empty();
        // 已经在终点：自旋路径不能是空路径（否则会直接硬降，last_rot_kick 为空）
        assert!(bfs_path(&g, 'T', 0, (3, 0), (3, 0), true).is_none());
        let p = bfs_path(&g, 'T', 0, (3, 0), (3, 0), false);
        assert!(matches!(p, Some(ref v) if v.is_empty()));
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

    /// TSD 槽（贴地踢墙进槽）：必须由踢墙候选被找到，且判为全旋
    #[test]
    fn finds_kick_tsd() {
        let g = tsd_stack();
        let fr = fr_of(&g);
        let cands = candidates(&g, 'T', &fr, &master_profile(), false, master_profile().danger_h);
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
        let cands = candidates(&g, 'T', &fr, &master_profile(), false, master_profile().danger_h);
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

/// B5「超极限」：attack 16、容忍 14 行待进垃圾、更不在乎洞——检验激进是否还有正收益。
pub fn berserk5() -> Profile {
    let mut p = berserk("berserk5", 16.0, 20, 14, false);
    p.w.holes = 4.0;
    p.w.eroded = 2.2;
    p
}

/// B6「混合」：与 B2 同攻档但抵消权重回到 2.5（半攻半守），检验 cancel 在高攻档的价值。
pub fn berserk6() -> Profile {
    let mut p = berserk("berserk6", 12.0, 20, 12, false);
    p.w.cancel = 2.5;
    p
}

/// B7「v5+1 档」：attack 20、待进垃圾容忍 16——激进上限探测。
pub fn berserk7() -> Profile {
    let mut p = berserk("berserk7", 20.0, 20, 16, false);
    p.w.holes = 3.5;
    p.w.eroded = 2.0;
    p
}

/// 本实验策略的 profile 入口：T99_BERSERK 选档（v1 激进 / v2 极限 / v3 极限+钓鱼 / v4 钓鱼对照 /
/// v5 超极限 / v6 混合），缺省 v7（实测综合最优：attack 20 / 20 行 / 16 垃圾容忍）。
pub fn profile() -> Profile {
    match std::env::var("T99_BERSERK").as_deref() {
        Ok("v1") => berserk1(),
        Ok("v2") => berserk2(),
        Ok("v3") => berserk3(),
        Ok("v4") => berserk4(),
        Ok("v5") => berserk5(),
        Ok("v6") => berserk6(),
        _ => berserk7(),
    }
}
