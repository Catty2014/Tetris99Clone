//! t99-core: 确定性 Tetris-99 风格对局内核。
//!
//! 全整数运算：时间单位为 1/3 纳秒（帧 = 5e7 拍、微秒 = 3000 拍，全部整除），
//! 位置单位为毫格（1/1000 格）。内核不接触墙钟与浮点，
//! 对局是 `(seed, 事件流)` 的纯函数，可跨平台逐位重放。

pub mod engine;

pub use engine::{Match, MatchConfig, MatchResult, PlayerPhase, Snapshot, TableRow};

pub type Ticks = i64;
/// ticks per second (1/3 ns)
pub const TPS: Ticks = 3_000_000_000;
pub const TMS: Ticks = 3_000_000;
pub const TUS: Ticks = 3_000;
pub const FRAME_TICKS: Ticks = TPS / 60; // 50_000_000
/// 积分子步长：1ms。锁定时序精度为 1ms，位置精度由累加器保证精确。
pub const SUB_TICKS: Ticks = TMS;
pub const MS_PER_FRAME: u32 = 16; // 整毫秒部分；帧尾余 2/3 ms 由 FRAME_TICKS 精确处理
pub const MAX_T_US: u32 = 16_666;

pub type Milli = i64;
pub const MILLI_CELL: Milli = 1000;

pub const W: usize = 10;
pub const HIDDEN: usize = 4;
pub const H: usize = 20;
pub const ROWS: usize = HIDDEN + H;

pub type Cell = u8; // 0 空, 1..=7 方块, 8 垃圾
pub const CELL_GARBAGE: Cell = 8;

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    I,
    O,
    T,
    S,
    Z,
    J,
    L,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::I, Kind::O, Kind::T, Kind::S, Kind::Z, Kind::J, Kind::L];

    pub fn id(self) -> u8 {
        self as u8 + 1
    }

    pub fn from_id(id: u8) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.id() == id)
    }

    pub fn ch(self) -> char {
        match self {
            Kind::I => 'I',
            Kind::O => 'O',
            Kind::T => 'T',
            Kind::S => 'S',
            Kind::Z => 'Z',
            Kind::J => 'J',
            Kind::L => 'L',
        }
    }

    pub fn from_ch(c: char) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.ch() == c)
    }

    pub fn box_size(self) -> usize {
        match self {
            Kind::I => 4,
            Kind::O => 2,
            _ => 3,
        }
    }

    pub fn spawn_x(self) -> i32 {
        match self {
            Kind::O => 4,
            _ => 3,
        }
    }

    /// rot0（出生态）方块格，随盒内顺时针旋转 k 次得到 rot=k。
    pub fn cells(self, rot: u8) -> [(i32, i32); 4] {
        let base: [(i32, i32); 4] = match self {
            Kind::I => [(0, 1), (1, 1), (2, 1), (3, 1)],
            Kind::O => [(0, 0), (1, 0), (0, 1), (1, 1)],
            Kind::T => [(1, 0), (0, 1), (1, 1), (2, 1)],
            Kind::S => [(1, 0), (2, 0), (0, 1), (1, 1)],
            Kind::Z => [(0, 0), (1, 0), (1, 1), (2, 1)],
            Kind::J => [(0, 0), (0, 1), (1, 1), (2, 1)],
            Kind::L => [(2, 0), (0, 1), (1, 1), (2, 1)],
        };
        let s = self.box_size() as i32;
        let mut out = base;
        for _ in 0..(rot % 4) {
            for c in &mut out {
                *c = (s - 1 - c.1, c.0);
            }
        }
        out
    }
}

/// 屏幕坐标（y 向下）下的 SRS 踢墙表。O 无踢墙；180° 仅占位 [(0,0)]（默认禁用）。
pub fn kicks(kind: Kind, from: u8, to: u8) -> &'static [(i32, i32)] {
    const K0: [(i32, i32); 5] = [(0, 0), (-1, 0), (-1, -1), (0, 2), (-1, 2)];
    const K1: [(i32, i32); 5] = [(0, 0), (1, 0), (1, 1), (0, -2), (1, -2)];
    const K2: [(i32, i32); 5] = [(0, 0), (1, 0), (1, -1), (0, 2), (1, 2)];
    const K3: [(i32, i32); 5] = [(0, 0), (-1, 0), (-1, 1), (0, -2), (-1, -2)];
    const NONE: [(i32, i32); 1] = [(0, 0)];
    const IK0: [(i32, i32); 5] = [(0, 0), (-2, 0), (1, 0), (-2, 1), (1, -2)];
    const IK1: [(i32, i32); 5] = [(0, 0), (2, 0), (-1, 0), (2, -1), (-1, 2)];
    const IK2: [(i32, i32); 5] = [(0, 0), (-1, 0), (2, 0), (-1, -2), (2, 1)];
    const IK3: [(i32, i32); 5] = [(0, 0), (1, 0), (-2, 0), (1, 2), (-2, -1)];
    if kind == Kind::O {
        return &NONE;
    }
    let d = (to + 4 - from % 4) % 4;
    if d == 0 {
        return &NONE;
    }
    if d == 2 {
        return &NONE; // 180° 简化踢墙（默认禁用 180）
    }
    // d: 1=顺时针, 3=逆时针。按 (起始态, 方向) 选表。
    if kind == Kind::I {
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

/// splitmix64 流；由 (seed, pid, salt) 派生，保证可重放。
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64, pid: usize, salt: u64) -> Self {
        let mut s = seed
            ^ (pid as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ salt.rotate_left(32);
        let r = Rng(s);
        let mut r = r;
        r.0 = r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Key {
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
    SoftDown,
    SoftUp,
    RotCw,
    RotCcw,
    Rot180,
    HardDrop,
    Hold,
    TargetBadges,
    TargetKos,
    TargetAttackers,
    TargetRandom,
    /// 手动指定目标（协议载荷键）
    TargetPlayer(u16),
    Noop,
}

impl Key {
    /// 键类别编号：同帧去重按类别（target_player 不同目标亦视为同类）
    pub fn kind(&self) -> u8 {
        match self {
            Key::LeftDown => 1,
            Key::LeftUp => 2,
            Key::RightDown => 3,
            Key::RightUp => 4,
            Key::SoftDown => 5,
            Key::SoftUp => 6,
            Key::RotCw => 7,
            Key::RotCcw => 8,
            Key::Rot180 => 9,
            Key::HardDrop => 10,
            Key::Hold => 11,
            Key::TargetBadges => 12,
            Key::TargetKos => 13,
            Key::TargetAttackers => 14,
            Key::TargetRandom => 15,
            Key::TargetPlayer(_) => 16,
            Key::Noop => 17,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Key::LeftDown => "left_down",
            Key::LeftUp => "left_up",
            Key::RightDown => "right_down",
            Key::RightUp => "right_up",
            Key::SoftDown => "soft_down",
            Key::SoftUp => "soft_up",
            Key::RotCw => "cw",
            Key::RotCcw => "ccw",
            Key::Rot180 => "flip",
            Key::HardDrop => "hard",
            Key::Hold => "hold",
            Key::TargetBadges => "target_badges",
            Key::TargetKos => "target_kos",
            Key::TargetAttackers => "target_attackers",
            Key::TargetRandom => "target_random",
            Key::TargetPlayer(_) => "target_player",
            Key::Noop => "noop",
        }
    }

    pub fn from_name(s: &str) -> Option<Key> {
        Some(match s {
            "left_down" => Key::LeftDown,
            "left_up" => Key::LeftUp,
            "right_down" => Key::RightDown,
            "right_up" => Key::RightUp,
            "soft_down" => Key::SoftDown,
            "soft_up" => Key::SoftUp,
            "cw" => Key::RotCw,
            "ccw" => Key::RotCcw,
            "flip" => Key::Rot180,
            "hard" => Key::HardDrop,
            "hold" => Key::Hold,
            "target_badges" => Key::TargetBadges,
            "target_kos" => Key::TargetKos,
            "target_attackers" => Key::TargetAttackers,
            "target_random" => Key::TargetRandom,
            "noop" => Key::Noop,
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct TimedKey {
    pub t_us: u32,
    pub key: Key,
}

/// 事件帧内校验：≤16 个、t 严格递增且 ≤ 16666µs、同帧不得重复同键。
/// 非法整帧作废（返回 None → 空帧）。
pub fn sanitize_events(raw: &[TimedKey]) -> Option<Vec<TimedKey>> {
    if raw.len() > 16 {
        return None;
    }
    let mut prev_t: i64 = -1;
    for (i, e) in raw.iter().enumerate() {
        if e.t_us > MAX_T_US || (e.t_us as i64) <= prev_t {
            return None;
        }
        prev_t = e.t_us as i64;
        if raw[..i].iter().any(|p| p.key.kind() == e.key.kind()) {
            return None;
        }
    }
    Some(raw.to_vec())
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TargetMode {
    Badges,
    Kos,
    Attackers,
    Random,
    /// 手动指定单个玩家
    Manual(usize),
}

impl TargetMode {
    pub fn name(self) -> &'static str {
        match self {
            TargetMode::Badges => "badges",
            TargetMode::Kos => "kos",
            TargetMode::Attackers => "attackers",
            TargetMode::Random => "random",
            TargetMode::Manual(_) => "manual",
        }
    }
}

/// 节奏相位（ADR-018）：按存活比例切换；相位内参数缓升，相位间跳变。
#[derive(Clone, Debug)]
pub struct PhaseSpec {
    /// 进入该相位所需最低存活比例（千分比，alive*1000/start_players）
    pub ratio_min_pm: u32,
    pub gravity_base: Milli,
    /// 相位内每分钟重力增量（毫格/秒）
    pub gravity_ramp_per_min: Milli,
    pub gravity_cap: Milli,
    pub clear_delay: Ticks,
    pub are: Ticks,
    /// 垃圾黄/红各自阶段的时长（总排队 ≈ 2×此值）
    pub garbage_stage: Ticks,
}

/// 规则集：全部数值集中于此，考试档与公开档在此分叉。
#[derive(Clone, Debug)]
pub struct Ruleset {
    pub preview: usize,
    pub lock_ticks: Ticks,
    pub lock_resets_max: u32,
    pub das_ticks: Ticks,
    pub arr_ticks: Ticks,
    pub soft_milli_per_sec: Milli,
    /// 节奏相位表，按 ratio_min_pm 降序
    pub phases: Vec<PhaseSpec>,
    pub pending_cap_rows: u32,
    pub send_cap_rows: u32,
    /// combo 计数（第 2 次连续消行 = 1）→ 追加行数，索引上限 10
    pub combo_bonus: [u32; 11],
    /// 被瞄准人数(2..=6+) → 追加
    pub target_bonus: [u32; 7],
    /// 徽章档位所需徽章数（KO 转移：击倒者获得被击倒者全部徽章+1）
    pub badge_tier_units: [u32; 4],
    /// 垃圾行同洞概率（百分比）
    pub p_same_hole_pct: u32,
    /// 对局倒计时预算（tick）；0 = 按人数自动：4p 5min → 99p 15min 线性插值
    pub match_ticks: Ticks,
    /// 0 杀加速燃烧：最后一次击杀 T 秒后（T>60）倒计时以 T/60 倍速消耗（展览模式；
    /// 正式 Arena 关闭，难度由相位参数自然上升）
    pub stall_accel: bool,
    pub allow_rot180: bool,
    pub spawn_y_milli: Milli,
}

/// 按参与方数推导对局预算：4p=300s，99p=900s，线性插值，两端截断
pub fn match_ticks_for(players: usize) -> Ticks {
    let secs = if players <= 4 {
        300.0f64
    } else {
        (300.0 + 600.0 * (players.min(99) - 4) as f64 / 95.0).min(900.0)
    };
    (secs * 1000.0).round() as Ticks * TMS
}

impl Default for Ruleset {
    fn default() -> Self {
        Ruleset {
            preview: 6,
            lock_ticks: 500 * TMS,
            lock_resets_max: 15,
            das_ticks: 10 * FRAME_TICKS,
            arr_ticks: 2 * FRAME_TICKS,
            soft_milli_per_sec: 60_000,
            phases: vec![
                // P1：重力跟随 T99/准则 L1（0.8s/行 ≈ 0.0125G；自然流速待实机校准，ADR-014）；
                // 垃圾总排队 2.4s 给足抵消窗口（T99 满厅约 2.5-3.5s），消行 300ms ≈ T99 动画
                PhaseSpec {
                    ratio_min_pm: 500,
                    gravity_base: 750,
                    gravity_ramp_per_min: 120,
                    gravity_cap: 1125,
                    clear_delay: 300 * TMS,
                    are: 100 * TMS,
                    garbage_stage: 1200 * TMS,
                },
                PhaseSpec {
                    ratio_min_pm: 300,
                    gravity_base: 1500,
                    gravity_ramp_per_min: 400,
                    gravity_cap: 2250,
                    clear_delay: 250 * TMS,
                    are: 100 * TMS,
                    garbage_stage: 900 * TMS,
                },
                PhaseSpec {
                    ratio_min_pm: 100,
                    gravity_base: 3000,
                    gravity_ramp_per_min: 1000,
                    gravity_cap: 4500,
                    clear_delay: 200 * TMS,
                    are: 80 * TMS,
                    garbage_stage: 650 * TMS,
                },
                // P4 末相位：无下一阶梯，向 1G 爬坡（T99 终盘高速）；垃圾 0.9s 进场
                PhaseSpec {
                    ratio_min_pm: 0,
                    gravity_base: 6000,
                    gravity_ramp_per_min: 2000,
                    gravity_cap: 60_000,
                    clear_delay: 150 * TMS,
                    are: 60 * TMS,
                    garbage_stage: 450 * TMS,
                },
            ],
            pending_cap_rows: 12,
            send_cap_rows: 20,
            combo_bonus: [0, 1, 1, 2, 2, 3, 3, 4, 4, 4, 5],
            target_bonus: [0, 0, 1, 3, 5, 7, 9],
            badge_tier_units: [2, 6, 14, 30],
            p_same_hole_pct: 70,
            match_ticks: 0,
            stall_accel: true,
            allow_rot180: false,
            // 可见首行出生：T99 观感——方块从场顶立即出现（而非隐藏缓冲区）
            spawn_y_milli: 4000,
        }
    }
}

impl Ruleset {
    pub fn phase_of(&self, alive: usize, start: usize) -> usize {
        let ratio = ((alive.max(1) * 1000) / start.max(1)) as u32;
        for (i, p) in self.phases.iter().enumerate() {
            if ratio >= p.ratio_min_pm {
                return i;
            }
        }
        self.phases.len() - 1
    }

    pub fn gravity(&self, phase_idx: usize, phase_ticks: Ticks) -> Milli {
        let i = phase_idx.min(self.phases.len() - 1);
        let p = &self.phases[i];
        // 千分之一分钟粒度，保证缓升平滑且全整数
        let milli_minutes = (phase_ticks.max(0) as i128 * 1000 / MINUTES_TICKS as i128) as Milli;
        // 相位内线性爬坡，但封顶在到下一相位阶梯的一半（末相位爬到自身 cap）
        let hold = if i + 1 < self.phases.len() {
            p.gravity_base + (self.phases[i + 1].gravity_base - p.gravity_base) / 2
        } else {
            p.gravity_cap
        };
        (p.gravity_base + p.gravity_ramp_per_min * milli_minutes / 1000).min(hold)
    }

    /// 相位内重力爬坡的封顶值（显示用，与 gravity() 一致）
    pub fn gravity_hold(&self, phase_idx: usize) -> Milli {
        let i = phase_idx.min(self.phases.len() - 1);
        let p = &self.phases[i];
        if i + 1 < self.phases.len() {
            p.gravity_base + (self.phases[i + 1].gravity_base - p.gravity_base) / 2
        } else {
            p.gravity_cap
        }
    }

    pub fn clear_delay(&self, phase_idx: usize) -> Ticks {
        self.phases[phase_idx.min(self.phases.len() - 1)].clear_delay
    }

    pub fn are(&self, phase_idx: usize) -> Ticks {
        self.phases[phase_idx.min(self.phases.len() - 1)].are
    }

    /// 垃圾黄/红各阶段的时长（按相位；总排队 ≈ 2×此值）
    pub fn garbage_stage(&self, phase_idx: usize) -> Ticks {
        self.phases[phase_idx.min(self.phases.len() - 1)].garbage_stage
    }
}

pub const MINUTES_TICKS: Ticks = TPS * 60;
