//! t99-core: 确定性 Tetris-99 风格对局内核。
//!
//! 全整数运算：时间单位为 1/3 纳秒（帧 = 5e7 拍、微秒 = 3000 拍，全部整除），
//! 位置单位为毫格（1/1000 格）。内核不接触墙钟与浮点，
//! 对局是 `(seed, 事件流)` 的纯函数，可跨平台逐位重放。

pub mod engine;
pub mod score;

pub use engine::{Match, MatchConfig, MatchResult, PlayerPhase, Snapshot, TableRow};
pub use score::{match_points, placement_points};

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

/// 消行延迟结束后的出生间隔（ruleset v9：恒为 6 帧 @60Hz）
pub const ARE_FRAMES: u32 = 6;

/// 引擎语义版本：任何影响模拟结果的引擎改动都必须递增（镜像失步的语义类防护——
/// 客户端二进制落后于服务器时，tick 流本身无缺口、序号校验抓不到，只能靠版本号）
pub const ENGINE_SEMANTICS: u32 = 4;
/// 规则集身份；与 ruleset.toml 的 `id` 和服务器 hello 一致。
pub const RULESET_ID: &str = "9f3c1a7e";

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
    /// 有状态的按下/释放，用于 I*S、KeyState 与 DAS 中断
    RotCwDown,
    RotCwUp,
    RotCcwDown,
    RotCcwUp,
    HoldDown,
    HoldUp,
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
            Key::RotCwDown => 18,
            Key::RotCwUp => 19,
            Key::RotCcwDown => 20,
            Key::RotCcwUp => 21,
            Key::HoldDown => 22,
            Key::HoldUp => 23,
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
            Key::RotCwDown => "cw_down",
            Key::RotCwUp => "cw_up",
            Key::RotCcwDown => "ccw_down",
            Key::RotCcwUp => "ccw_up",
            Key::HoldDown => "hold_down",
            Key::HoldUp => "hold_up",
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
            "cw_down" => Key::RotCwDown,
            "cw_up" => Key::RotCwUp,
            "ccw_down" => Key::RotCcwDown,
            "ccw_up" => Key::RotCcwUp,
            "hold_down" => Key::HoldDown,
            "hold_up" => Key::HoldUp,
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

/// 已净化的帧输入：`events` 为实际执行的事件，`dropped` 为被去重/拒绝的数量。
pub struct Sanitized {
    pub events: Vec<TimedKey>,
    pub dropped: u32,
}

/// 事件帧内校验与去重：≤16 个、t 严格递增且 ≤ 16666µs。
/// 同一动作（同 `Key::kind`）一帧只响应第一次，后续重复丢弃并计数；
/// 软降重复无收益，保留；目标选择按顺序生效；180 旋转未实现，直接丢弃。
pub fn sanitize_events(raw: &[TimedKey]) -> Option<Sanitized> {
    if raw.len() > 16 {
        return None;
    }
    let mut prev_t: i64 = -1;
    for e in raw {
        if e.t_us > MAX_T_US || (e.t_us as i64) <= prev_t {
            return None;
        }
        prev_t = e.t_us as i64;
    }
    let mut seen = [false; 24];
    let mut events = Vec::with_capacity(raw.len());
    let mut dropped = 0u32;
    for e in raw {
        // 未实现的 180 旋转不允许出现在输入里
        if matches!(e.key, Key::Rot180) {
            dropped += 1;
            continue;
        }
        // 软降重复无收益；目标选择按顺序生效；Noop 无副作用
        let exempt = matches!(
            e.key,
            Key::SoftDown
                | Key::SoftUp
                | Key::TargetBadges
                | Key::TargetKos
                | Key::TargetAttackers
                | Key::TargetRandom
                | Key::TargetPlayer(_)
                | Key::Noop
        );
        let kind = e.key.kind() as usize;
        if !exempt && seen.get(kind).copied().unwrap_or(false) {
            dropped += 1;
            continue;
        }
        if let Some(s) = seen.get_mut(kind) {
            *s = true;
        }
        events.push(*e);
    }
    Some(Sanitized { events, dropped })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialTrigger {
    /// 窗口内出现 0->1
    Edge,
    /// 窗口结束时仍按住
    Held,
    /// 任一满足
    Either,
}

/// 阶段（ruleset v9，见 docs/rules/ruleset-v9.md）：按存活比例选择，
/// 阶段内按剩余时间比例向下一阶段的一半过渡。
#[derive(Clone, Debug)]
pub struct PhaseSpec {
    /// 进入该阶段所需最低存活比例（千分比）；末项为终态，只作过渡终点
    pub ratio_min_pm: u32,
    /// 自然下落一格所需帧数（60Hz），倒数为每帧格数
    pub g_frames: u32,
    /// 垃圾倒计时总帧数（60Hz）
    pub r_frames: u32,
    /// 锁定后到下一块出生前的帧数，消行延迟与 ARE 合并
    pub d_frames: u32,
    /// 向下一阶段一半过渡的窗口，占总局时长的千分比
    pub window_pm: u32,
}

/// 规则集：全部数值集中于此，考试档与公开档在此分叉。
#[derive(Clone, Debug)]
pub struct Ruleset {
    pub preview: usize,
    /// 逻辑 tick 速率；规则时长仍是绝对时间，120Hz 只改变每帧步长
    pub tick_hz: u32,
    /// 横向移动同一方向的最小间隔（帧）；ARRCheck 用，必须 >= 1
    pub arr_ticks: u32,
    pub irs: bool,
    pub ihs: bool,
    pub initial_trigger: InitialTrigger,
    pub lock_ticks: Ticks,
    pub lock_resets_max: u32,
    /// 软降速度；横向 DAS/ARR 属于有头客户端输入时序，不在规则集内。
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

/// 按参与方数推导对局预算：4p=300s，99p=1200s，线性插值，两端截断。
/// 正式对局 99 人为 20 分钟；人数越少预算越短。
pub fn match_ticks_for(players: usize) -> Ticks {
    let secs = if players <= 4 {
        300.0f64
    } else {
        (300.0 + 900.0 * (players.min(99) - 4) as f64 / 95.0).min(1200.0)
    };
    (secs * 1000.0).round() as Ticks * TMS
}

impl Default for Ruleset {
    fn default() -> Self {
        Ruleset {
            preview: 6,
            tick_hz: 60,
            arr_ticks: 2,
            irs: true,
            ihs: true,
            initial_trigger: InitialTrigger::Either,
            lock_ticks: 500 * TMS,
            lock_resets_max: 15,
            soft_milli_per_sec: 60_000,
            phases: vec![
                // v9：阶段 1~3 为可选阶段，第 4 项是终态（只作阶段 3 的过渡终点）。
                PhaseSpec {
                    ratio_min_pm: 500,
                    g_frames: 60,
                    r_frames: 136,
                    d_frames: 33,
                    window_pm: 400,
                },
                PhaseSpec {
                    ratio_min_pm: 100,
                    g_frames: 30,
                    r_frames: 90,
                    d_frames: 17,
                    window_pm: 200,
                },
                PhaseSpec {
                    ratio_min_pm: 1,
                    g_frames: 10,
                    r_frames: 30,
                    d_frames: 7,
                    window_pm: 100,
                },
                PhaseSpec {
                    ratio_min_pm: 0,
                    g_frames: 1,
                    r_frames: 30,
                    d_frames: 7,
                    window_pm: 0,
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
    pub fn frame_ticks(&self) -> Ticks {
        TPS / self.tick_hz.max(1) as Ticks
    }

    /// 阶段选择（v9）：T ≥ 75% 时无条件回到阶段 1；否则按存活比例。
    /// 阈值取严格不等：P = 50% 属于阶段 2，P = 10% 属于阶段 3。
    pub fn phase_of(&self, alive: usize, start: usize, ticks_left: Ticks, total: Ticks) -> usize {
        if total > 0 && ticks_left * 4 >= total * 3 {
            return 0;
        }
        let alive = alive.max(1) as u128;
        let start = start.max(1) as u128;
        if alive * 2 > start {
            0
        } else if alive * 10 > start {
            1
        } else {
            2
        }
    }

    /// 阶段参数，单位为帧：g 的「帧/格」、垃圾倒计时 r、锁定延迟 d。
    /// enter_left 是进入当前阶段时的剩余 tick；阶段 1 的过渡起点固定为 75%。
    /// 规则见 docs/rules/ruleset-v9.md §3、§4。
    pub fn pacing(
        &self,
        phase: usize,
        enter_left: Ticks,
        left: Ticks,
        total: Ticks,
    ) -> (u32, u32, u32) {
        let i = phase.min(self.phases.len() - 1);
        let base = &self.phases[i];
        let base_v = (base.g_frames, base.r_frames, base.d_frames);
        let next_i = (i + 1).min(self.phases.len() - 1);
        let next = &self.phases[next_i];
        let full = (next.g_frames, next.r_frames, next.d_frames);
        // 一半在帧域取算术平均；阶段 3 直接推到终态整值
        let half = if i + 2 >= self.phases.len() {
            full
        } else {
            (
                (base.g_frames + next.g_frames) / 2,
                (base.r_frames + next.r_frames) / 2,
                (base.d_frames + next.d_frames) / 2,
            )
        };

        let win = total * base.window_pm as Ticks / 1000;
        let start = if i == 0 {
            total * 3 / 4
        } else {
            enter_left.clamp(0, total)
        };
        if win <= 0 || left >= start {
            return base_v;
        }
        let end = start - win;
        let p40 = total * 2 / 5;

        let lerp = |from: (u32, u32, u32),
                    to: (u32, u32, u32),
                    a: Ticks,
                    b: Ticks,
                    at: Ticks|
         -> (u32, u32, u32) {
            if b >= a {
                return to;
            }
            let den = (a - b) as i128;
            let num = (a - at).clamp(0, a - b) as i128;
            let f = |x: u32, y: u32| -> u32 {
                let x = x as i128;
                let y = y as i128;
                let v = (x * den + (y - x) * num).div_euclid(den);
                v.max(0) as u32
            };
            (f(from.0, to.0), f(from.1, to.1), f(from.2, to.2))
        };

        if p40 >= start {
            // 窗口起点已在 40% 之后：整段以整档为目标
            return lerp(base_v, full, start, end, left);
        }
        if p40 > end {
            // 40% 落在窗口内部：先把已走过的部分按一半走，再在剩余窗口内补齐整档
            if left >= p40 {
                return lerp(base_v, half, start, end, left);
            }
            let v40 = lerp(base_v, half, start, end, p40);
            return lerp(v40, full, p40, end, left);
        }
        // 窗口在 40% 之前结束
        if left > p40 {
            return lerp(base_v, half, start, end, left);
        }
        if end == p40 {
            full
        } else {
            half
        }
    }

    /// 帧域的落体速度折算为毫格/秒（整数除法，等价于向下取整）
    pub fn gravity_milli(g_frames: u32) -> Milli {
        60_000 / g_frames.max(1) as Milli
    }
}

/// 规则帧（60Hz）换算为引擎 tick；与引擎实际 tick_hz 无关
pub const fn frames_to_ticks(frames: u32) -> Ticks {
    frames as Ticks * FRAME_TICKS
}

pub const MINUTES_TICKS: Ticks = TPS * 60;
