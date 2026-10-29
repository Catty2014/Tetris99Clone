//! 对局引擎：单人 guideline 栈 + 多人战斗机制（垃圾/目标/徽章/名次）。

use crate::*;
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct MatchConfig {
    pub players: usize,
    pub seed: u64,
    pub ruleset: Ruleset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeathReason {
    TopOut,
    HostKill(String),
}

impl DeathReason {
    pub fn tag(&self) -> &'static str {
        match self {
            DeathReason::TopOut => "top_out",
            DeathReason::HostKill(_) => "host_kill",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SpinKind {
    None,
    Mini,
    Full,
}

/// 最近一次消行判定（客户端弹出判定文字用；spin: 0 无旋 1 mini 2 全旋）
#[derive(Copy, Clone, Debug)]
pub struct ClearView {
    pub lines: u32,
    pub spin: u8,
    pub all_clear: bool,
    pub b2b: bool,
    /// 判定发生的帧号（客户端按 <1s 显示窗口过滤）
    pub frame: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ActiveView {
    pub kind: Kind,
    pub rot: u8,
    pub x: i32,
    /// 相对可见区顶部的毫格坐标，可为负（生成区）
    pub y_milli: Milli,
}

#[derive(Clone, Debug)]
pub struct DasView {
    pub left_held: bool,
    pub right_held: bool,
    pub active_dir: Option<i8>,
    pub das_ms: i64,
    pub arr_ms: i64,
}

/// 单方视角快照：不含他方棋盘。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub n: u64,
    /// 本快照归属的玩家 pid
    pub pid: usize,
    pub board: [[Cell; W]; H],
    pub active: Option<ActiveView>,
    /// 玩家相位：play / clear / are（含剩余毫秒）
    pub phase: PlayerPhase,
    pub next: Vec<Kind>,
    pub hold: Option<Kind>,
    pub hold_used: bool,
    pub combo: i32,
    pub b2b: bool,
    pub garbage_red: u32,
    pub garbage_yellow: u32,
    pub garbage_gray: u32,
    /// 消行延迟中正在闪烁的行（可见区行号）
    pub clearing: Vec<usize>,
    pub badges: u32,
    pub badge_units: u32,
    pub target: Option<usize>,
    pub target_mode: TargetMode,
    pub alive: Vec<usize>,
    pub gravity_milli: Milli,
    pub soft: bool,
    pub lock_ms_left: i64,
    pub lock_resets: u32,
    pub das: DasView,
    pub ko: u32,
    pub lines: u32,
    pub sent: u32,
    pub pieces: u32,
    pub alive_self: bool,
    pub place: Option<u32>,
    /// 对局倒计时剩余秒（向上取整）
    pub match_secs_left: u32,
    /// 倒计时燃烧倍率 ×10（10 = 正常 1×；0 杀加速时 >10）
    pub burn_x10: u16,
    /// 正在瞄准本方（存活）的玩家 pid 列表
    pub targeted_by: Vec<usize>,
    /// 全员徽章数（index=pid；徽章随 KO 转移，围猎徽章领先者的决策依据）
    pub badges_all: Vec<u32>,
    /// 最近一次消行判定（1s 显示窗口由客户端按帧龄过滤）
    pub last_clear: Option<ClearView>,
}

#[derive(Copy, Clone, Debug)]
struct Active {
    kind: Kind,
    rot: u8,
    x: i32,
    y: Milli,
}

/// T99 垃圾队列条目三态（wiki：黄→红→红闪进场，灰色排队，连击阻挡）
#[derive(Clone, Debug)]
struct Garb {
    rows: u32,
    sender: usize,
    state: GarbState,
}

#[derive(Clone, Debug)]
enum GarbState {
    /// 灰：排队中，前序批次了结后才开始计时
    Gray,
    /// 黄：计时中（剩余 ticks）
    Yellow(Ticks),
    /// 红：即将进场（剩余 ticks，到 0 后等待连击窗口）
    Red(Ticks),
}

impl GarbState {
    fn tag(&self) -> u8 {
        match self {
            GarbState::Gray => 0,
            GarbState::Yellow(_) => 1,
            GarbState::Red(_) => 2,
        }
    }
}

#[derive(Clone, Debug)]
struct Das {
    left: bool,
    right: bool,
    dir: Option<i8>,
    das: Ticks,
    arr: Ticks,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PlayerPhase {
    Play,
    /// 消行延迟：clearing_rows 中的行尚未压实
    Clear(Ticks),
    /// 生成延迟
    Are(Ticks),
}

impl PlayerPhase {
    pub fn tag(&self) -> &'static str {
        match self {
            PlayerPhase::Play => "play",
            PlayerPhase::Clear(_) => "clear",
            PlayerPhase::Are(_) => "are",
        }
    }

    pub fn ms_left(&self) -> i64 {
        match self {
            PlayerPhase::Play => 0,
            PlayerPhase::Clear(t) | PlayerPhase::Are(t) => t / TMS,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Player {
    pub id: usize,
    board: [Cell; W * ROWS],
    active: Option<Active>,
    pub phase: PlayerPhase,
    clearing_rows: Vec<usize>,
    queue: VecDeque<Kind>,
    bag: Vec<Kind>,
    rng_bag: Rng,
    rng_misc: Rng,
    pub hold: Option<Kind>,
    hold_used: bool,
    soft: bool,
    /// 有状态的旋转/保持键，用于 I*S 的 held 触发
    held_cw: bool,
    held_ccw: bool,
    held_hold: bool,
    /// 窗口内出现过的 0->1（edge）
    initial_rot: Option<u8>,
    initial_hold: bool,
    rot_last_pressed: Option<u8>,
    rot_last_released: Option<u8>,
    /// 当前 I*S 窗口内发生过释放；用于“以后松开的为准”
    rot_released_in_window: bool,
    /// 本帧已发生锁定；后续 piece-action 不再执行
    locked_this_frame: bool,
    das: Das,
    lock_timer: Ticks,
    lock_resets: u32,
    grounded: bool,
    grav_rem: i128,
    last_rot_kick: Option<u8>,
    pub combo: i32,
    pub b2b: bool,
    pending: Vec<Garb>,
    pub ko: u32,
    pub badges: u32,
    pub lines: u32,
    pub sent: u32,
    pub pieces: u32,
    pub target_mode: TargetMode,
    pub last_target: Option<usize>,
    pub alive: bool,
    pub death_reason: Option<DeathReason>,
    pub place: Option<u32>,
    /// 死亡发生的逻辑帧；同帧淘汰共享名次
    pub death_frame: Option<u64>,
    last_garbage_from: Option<usize>,
    pub last_clear: Option<ClearView>,
}

/// 锁定后本帧不能再作用于下一块的输入；目标选择不算 piece-action。
fn is_piece_action(key: Key) -> bool {
    !matches!(
        key,
        Key::TargetBadges
            | Key::TargetKos
            | Key::TargetAttackers
            | Key::TargetRandom
            | Key::TargetPlayer(_)
            | Key::Noop
    )
}

fn collides(p: &Player, kind: Kind, rot: u8, x: i32, y: Milli) -> bool {
    for (dx, dy) in kind.cells(rot) {
        let col = x + dx;
        if col < 0 || col >= W as i32 {
            return true;
        }
        let bottom = y + dy as Milli * MILLI_CELL;
        if bottom + MILLI_CELL > (ROWS as Milli) * MILLI_CELL {
            return true;
        }
        let r_lo = bottom.div_euclid(MILLI_CELL);
        let r_hi = (bottom + MILLI_CELL - 1).div_euclid(MILLI_CELL);
        let mut r = r_lo;
        while r <= r_hi {
            if r >= 0 && (r as usize) < ROWS && p.board[r as usize * W + col as usize] != 0 {
                return true;
            }
            r += 1;
        }
    }
    false
}

/// 直落静息位（毫格）。不检查横向合法性（调用方保证列内）。
fn rest_y(p: &Player, kind: Kind, rot: u8, x: i32) -> Milli {
    let mut y_max = i64::MAX;
    for (dx, dy) in kind.cells(rot) {
        let col = x + dx;
        let mut top = (ROWS as Milli) * MILLI_CELL;
        if col >= 0 && (col as usize) < W {
            for r in 0..ROWS {
                if p.board[r * W + col as usize] != 0 {
                    top = (r as Milli) * MILLI_CELL;
                    break;
                }
            }
        }
        y_max = y_max.min(top - (dy as Milli + 1) * MILLI_CELL);
    }
    y_max
}

fn stack_height(p: &Player) -> usize {
    for r in 0..ROWS {
        if p.board[r * W..(r + 1) * W].iter().any(|&c| c != 0) {
            return ROWS - r;
        }
    }
    0
}

#[derive(Clone, Debug)]
pub struct MatchResult {
    pub reason: &'static str,
    pub frame: u64,
    /// (pid, 名次)，按名次排序
    pub places: Vec<(usize, u32)>,
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub pid: usize,
    pub place: Option<u32>,
    /// 最终名次；同徽章/同帧淘汰采用 competition 排名
    pub rank: u32,
    pub death_frame: Option<u64>,
    pub alive: bool,
    pub ko: u32,
    pub badges: u32,
    pub lines: u32,
    pub sent: u32,
    pub pieces: u32,
    pub reason: Option<String>,
}

pub struct Match {
    pub cfg: MatchConfig,
    pub players: Vec<Player>,
    pub frame_n: u64,
    pub over: Option<MatchResult>,
    alive_count: usize,
    /// 节奏相位（ADR-018）
    pub phase_idx: usize,
    phase_ticks: Ticks,
    /// 最近一次击倒的帧号（None = 尚无死亡）；0 杀加速燃烧的锚点
    last_death_frame: Option<u64>,
    /// 对局倒计时剩余（tick），归零按排序键终局
    match_ticks_left: Ticks,
}

impl Match {
    pub fn new(cfg: MatchConfig) -> Self {
        assert!(cfg.players >= 1);
        let mut players = Vec::with_capacity(cfg.players);
        for id in 0..cfg.players {
            players.push(Player {
                id,
                board: [0; W * ROWS],
                active: None,
                phase: PlayerPhase::Play,
                clearing_rows: Vec::new(),
                queue: VecDeque::new(),
                bag: Vec::new(),
                rng_bag: Rng::new(cfg.seed, id, 1),
                rng_misc: Rng::new(cfg.seed, id, 2),
                hold: None,
                hold_used: false,
                soft: false,
                held_cw: false,
                held_ccw: false,
                held_hold: false,
                initial_rot: None,
                initial_hold: false,
                rot_last_pressed: None,
                rot_last_released: None,
                rot_released_in_window: false,
                locked_this_frame: false,
                das: Das { left: false, right: false, dir: None, das: 0, arr: 0 },
                lock_timer: 0,
                lock_resets: 0,
                grounded: false,
                grav_rem: 0,
                last_rot_kick: None,
                combo: -1,
                b2b: false,
                pending: Vec::new(),
                ko: 0,
                badges: 0,
                lines: 0,
                sent: 0,
                pieces: 0,
                target_mode: TargetMode::Kos,
                last_target: None,
                alive: true,
                death_reason: None,
                place: None,
                death_frame: None,
                last_garbage_from: None,
                last_clear: None,
            });
        }
        let alive_count = cfg.players;
        let match_ticks_left = if cfg.ruleset.match_ticks == 0 {
            crate::match_ticks_for(cfg.players)
        } else {
            cfg.ruleset.match_ticks
        };
        let mut m = Match {
            cfg,
            players,
            frame_n: 0,
            over: None,
            alive_count,
            phase_idx: 0,
            phase_ticks: 0,
            last_death_frame: None,
            match_ticks_left,
        };
        for pid in 0..m.players.len() {
            m.spawn(pid, None, false);
            if m.over.is_some() {
                break;
            }
        }
        m
    }

    fn draw(&mut self, pid: usize) -> Kind {
        if self.players[pid].bag.is_empty() {
            let mut v: Vec<Kind> = Kind::ALL.to_vec();
            let p = &mut self.players[pid];
            for i in (1..v.len()).rev() {
                let j = p.rng_bag.below((i + 1) as u64) as usize;
                v.swap(i, j);
            }
            p.bag = v;
        }
        self.players[pid].bag.pop().unwrap()
    }

    fn spawn(&mut self, pid: usize, kind: Option<Kind>, from_hold: bool) {
        let preview = self.cfg.ruleset.preview;
        while self.players[pid].queue.len() <= preview {
            let k = self.draw(pid);
            self.players[pid].queue.push_back(k);
        }
        let k = match kind {
            Some(k) => k,
            None => self.players[pid].queue.pop_front().unwrap(),
        };
        let sy = self.cfg.ruleset.spawn_y_milli;
        let (sx, coll) = {
            let p = &self.players[pid];
            (k.spawn_x(), collides(p, k, 0, k.spawn_x(), sy))
        };
        if coll {
            self.players[pid].active = None;
            self.kill_player(pid, DeathReason::TopOut);
            return;
        }
        let p = &mut self.players[pid];
        p.active = Some(Active { kind: k, rot: 0, x: sx, y: sy });
        p.phase = PlayerPhase::Play;
        p.clearing_rows.clear();
        p.hold_used = from_hold;
        p.grav_rem = 0;
        p.lock_timer = 0;
        p.lock_resets = 0;
        p.grounded = false;
        p.last_rot_kick = None;
        self.apply_initial(pid, from_hold);
    }

    fn initial_triggered(&self, edge: bool, held: bool) -> bool {
        match self.cfg.ruleset.initial_trigger {
            InitialTrigger::Edge => edge,
            InitialTrigger::Held => held,
            InitialTrigger::Either => edge || held,
        }
    }

    fn apply_initial(&mut self, pid: usize, from_hold: bool) {
        let (edge_rot, edge_hold, held_hold, held_rot) = {
            let p = &self.players[pid];
            let held_rot = if p.held_cw && p.held_ccw {
                // 两边都还按住：以后按下的为准
                p.rot_last_pressed.or(p.initial_rot)
            } else if p.rot_released_in_window {
                // 有一边已经松开：以后松开的为准
                p.rot_last_released
            } else if p.held_cw {
                Some(1)
            } else if p.held_ccw {
                Some(3)
            } else {
                None
            };
            (p.initial_rot, p.initial_hold, p.held_hold, held_rot)
        };
        let want_hold = self.cfg.ruleset.ihs
            && !from_hold
            && self.initial_triggered(edge_hold, held_hold);
        let want_rot = self.cfg.ruleset.irs
            && self.initial_triggered(edge_rot.is_some(), held_rot.is_some());
        // held/释放规则优先于窗口内最后一次按下。
        let rot = held_rot.or(edge_rot);

        // IHS 先：交换后递归 spawn，from_hold=true 只再应用 IRS。
        if want_hold {
            self.players[pid].initial_hold = false;
            self.do_hold(pid);
            return;
        }
        if want_rot {
            if let (Some(d), Some(r)) = (rot, self.players[pid].active.map(|a| a.rot)) {
                self.try_rotate(pid, r + d);
            }
        }
        self.players[pid].initial_rot = None;
        self.players[pid].initial_hold = false;
        self.players[pid].rot_released_in_window = false;
    }

    /// 帧推进：sanitize 后按事件时刻精确积分。
    pub fn frame(&mut self, inputs: &[Vec<TimedKey>]) {
        if self.over.is_some() {
            return;
        }
        for p in &mut self.players {
            p.locked_this_frame = false;
        }
        let frame_ticks = self.cfg.ruleset.frame_ticks();
        let n = self.players.len();
        let mut per: Vec<Vec<TimedKey>> = (0..n)
            .map(|i| inputs.get(i).and_then(|v| sanitize_events(v)).unwrap_or_default())
            .collect();
        for pid in 0..n {
            if !self.players[pid].alive {
                continue;
            }
            let events = std::mem::take(&mut per[pid]);
            let mut cursor: Ticks = 0;
            let mut stop = false;
            for ev in events {
                let target = ev.t_us as Ticks * TUS;
                if target > cursor {
                    self.advance(pid, target - cursor);
                    cursor = target;
                }
                if !self.players[pid].alive || self.over.is_some() {
                    stop = true;
                    break;
                }
                self.apply_key(pid, ev.key);
                if !self.players[pid].alive || self.over.is_some() {
                    stop = true;
                    break;
                }
            }
            if !stop {
                self.advance(pid, frame_ticks - cursor);
            }
            if self.over.is_some() {
                break;
            }
        }
        self.frame_n += 1;
        self.phase_ticks += frame_ticks;
        // 对局倒计时：最后一次击杀 T 秒后（T>60）以 T/60 倍速燃烧（0 杀加速，展览用）；
        // 归零即按排序键终局（rule.md 8 排序键 / 9 时间上限）
        if self.over.is_none() {
            self.match_ticks_left = (self.match_ticks_left - self.burn_ticks()).max(0);
            if self.match_ticks_left == 0 {
                // 流局：幸存者仅按徽章数排名
                let ranking = self.ranking_badges();
                for (i, &pid) in ranking.iter().enumerate() {
                    if self.players[pid].place.is_none() {
                        self.players[pid].place = Some(i as u32 + 1);
                    }
                }
                self.over = Some(MatchResult {
                    reason: "time_limit",
                    frame: self.frame_n,
                    places: self.places(),
                });
            }
        }
    }

    /// 本帧倒计时燃烧量：正常一帧；0 杀超过 60s 后线性升至 T/60 倍
    fn burn_ticks(&self) -> Ticks {
        let frame_ticks = self.cfg.ruleset.frame_ticks();
        if !self.cfg.ruleset.stall_accel {
            return frame_ticks;
        }
        let gap = self.frame_n.saturating_sub(self.last_death_frame.unwrap_or(0));
        if gap <= 60 * 60 {
            return frame_ticks;
        }
        (frame_ticks as u128 * gap as u128 / 3600) as Ticks
    }

    fn advance(&mut self, pid: usize, mut dt: Ticks) {
        while dt > 0 && self.players[pid].alive {
            let step = dt.min(SUB_TICKS);
            self.step_player(pid, step);
            dt -= step;
        }
    }

    fn step_player(&mut self, pid: usize, dt: Ticks) {
        // 垃圾队列独立推进（任何相位；T99：黄→红→进场，连击阻挡）
        self.tick_garbage(pid, dt);
        // 相位推进
        match self.players[pid].phase {
            PlayerPhase::Clear(mut t) => {
                t -= dt;
                if t <= 0 {
                    self.finish_clear(pid);
                } else {
                    self.players[pid].phase = PlayerPhase::Clear(t);
                }
                return;
            }
            PlayerPhase::Are(mut t) => {
                t -= dt;
                if t <= 0 {
                    self.spawn(pid, None, false);
                } else {
                    self.players[pid].phase = PlayerPhase::Are(t);
                }
                return;
            }
            PlayerPhase::Play => {}
        }
        if self.players[pid].active.is_none() {
            return;
        }
        let rate = if self.players[pid].soft {
            self.cfg.ruleset.soft_milli_per_sec
        } else {
            self.cfg.ruleset.gravity(self.phase_idx, self.phase_ticks)
        };
        {
            let p = &mut self.players[pid];
            p.grav_rem += rate as i128 * dt as i128;
            let adv = (p.grav_rem / TPS as i128) as Milli;
            if adv > 0 {
                p.grav_rem %= TPS as i128;
                if let Some(a) = p.active.as_mut() {
                    a.y += adv;
                }
            }
        }
        // 夹到静息位
        {
            let (k, r, x) = {
                let a = self.players[pid].active.unwrap();
                (a.kind, a.rot, a.x)
            };
            let rest = rest_y(&self.players[pid], k, r, x);
            let p = &mut self.players[pid];
            if let Some(a) = p.active.as_mut() {
                if a.y > rest {
                    a.y = rest;
                }
            }
        }
        // grounded 与锁定计时
        let grounded_now = {
            let p = &self.players[pid];
            let a = p.active.unwrap();
            collides(p, a.kind, a.rot, a.x, a.y + 1)
        };
        self.players[pid].grounded = grounded_now;
        if grounded_now {
            let lock_ticks = self.cfg.ruleset.lock_ticks;
            let mut do_lock = false;
            {
                let p = &mut self.players[pid];
                p.lock_timer += dt;
                if p.lock_timer >= lock_ticks {
                    do_lock = true;
                }
            }
            if do_lock {
                self.lock_piece(pid);
                return;
            }
        } else {
            self.players[pid].lock_timer = 0;
        }
    }

    fn try_shift(&mut self, pid: usize, dir: i8) -> bool {
        let (k, r, x, y) = match self.players[pid].active {
            Some(a) => (a.kind, a.rot, a.x, a.y),
            None => return false,
        };
        let nx = x + dir as i32;
        if collides(&self.players[pid], k, r, nx, y) {
            return false;
        }
        let max_resets = self.cfg.ruleset.lock_resets_max;
        let p = &mut self.players[pid];
        if let Some(a) = p.active.as_mut() {
            a.x = nx;
        }
        p.last_rot_kick = None;
        if p.grounded && p.lock_resets < max_resets {
            p.lock_timer = 0;
            p.lock_resets += 1;
        }
        true
    }

    fn try_rotate(&mut self, pid: usize, to: u8) -> bool {
        let (k, r, x, y) = match self.players[pid].active {
            Some(a) => (a.kind, a.rot, a.x, a.y),
            None => return false,
        };
        let to = to % 4;
        if to == r % 4 {
            return false;
        }
        let max_resets = self.cfg.ruleset.lock_resets_max;
        if k == Kind::O {
            let p = &mut self.players[pid];
            if let Some(a) = p.active.as_mut() {
                a.rot = to;
            }
            p.last_rot_kick = Some(0);
            if p.grounded && p.lock_resets < max_resets {
                p.lock_timer = 0;
                p.lock_resets += 1;
            }
            return true;
        }
        let d = (to + 4 - r % 4) % 4;
        if d == 2 && !self.cfg.ruleset.allow_rot180 {
            return false;
        }
        for (i, (kx, ky)) in kicks(k, r, to).iter().enumerate() {
            let nx = x + *kx;
            let ny = y + *ky as Milli * MILLI_CELL;
            if !collides(&self.players[pid], k, to, nx, ny) {
                let p = &mut self.players[pid];
                if let Some(a) = p.active.as_mut() {
                    a.rot = to;
                    a.x = nx;
                    a.y = ny;
                }
                p.last_rot_kick = Some(i as u8);
                if p.grounded && p.lock_resets < max_resets {
                    p.lock_timer = 0;
                    p.lock_resets += 1;
                }
                return true;
            }
        }
        false
    }

    fn do_hold(&mut self, pid: usize) {
        let cur = match self.players[pid].active {
            Some(a) => a.kind,
            None => return,
        };
        if self.players[pid].hold_used {
            return;
        }
        self.players[pid].hold_used = true;
        self.players[pid].active = None;
        let swap = self.players[pid].hold.replace(cur);
        self.spawn(pid, swap, true);
    }

    fn apply_key(&mut self, pid: usize, key: Key) {
        if !self.players[pid].alive {
            return;
        }
        // 本帧已经锁定：后续 piece-action 不再影响下一块；目标事件仍允许。
        if self.players[pid].locked_this_frame && is_piece_action(key) {
            return;
        }
        // 无活动块（Clear/Are）期间：允许 DAS 充能、软降、目标与空操作；
        // 旋转/保持的 down/up 进入 I*S 窗口。
        if self.players[pid].active.is_none() {
            {
                let p = &mut self.players[pid];
                match key {
                    Key::RotCw => {
                        p.initial_rot = Some(1);
                        p.rot_last_pressed = Some(1);
                        return;
                    }
                    Key::RotCcw => {
                        p.initial_rot = Some(3);
                        p.rot_last_pressed = Some(3);
                        return;
                    }
                    Key::RotCwDown => {
                        p.held_cw = true;
                        p.initial_rot = Some(1);
                        p.rot_last_pressed = Some(1);
                        return;
                    }
                    Key::RotCwUp => {
                        p.held_cw = false;
                        p.rot_last_released = Some(1);
                        p.rot_released_in_window = true;
                        return;
                    }
                    Key::RotCcwDown => {
                        p.held_ccw = true;
                        p.initial_rot = Some(3);
                        p.rot_last_pressed = Some(3);
                        return;
                    }
                    Key::RotCcwUp => {
                        p.held_ccw = false;
                        p.rot_last_released = Some(3);
                        p.rot_released_in_window = true;
                        return;
                    }
                    Key::Hold => {
                        p.initial_hold = true;
                        return;
                    }
                    Key::HoldDown => {
                        p.held_hold = true;
                        p.initial_hold = true;
                        return;
                    }
                    Key::HoldUp => {
                        p.held_hold = false;
                        return;
                    }
                    _ => {}
                }
            }
            let gap_ok = matches!(
                key,
                Key::LeftDown
                    | Key::LeftUp
                    | Key::RightDown
                    | Key::RightUp
                    | Key::SoftDown
                    | Key::SoftUp
                    | Key::TargetBadges
                    | Key::TargetKos
                    | Key::TargetAttackers
                    | Key::TargetRandom
                    | Key::TargetPlayer(_)
                    | Key::Noop
            );
            if !gap_ok {
                return;
            }
        }
        match key {
            Key::LeftDown => {
                // 离散移动：每个 left_down 移一格；DAS/ARR 由客户端展开。
                self.players[pid].das.left = true;
                self.players[pid].das.dir = Some(-1);
                self.try_shift(pid, -1);
            }
            Key::LeftUp => {
                self.players[pid].das.left = false;
                if self.players[pid].das.dir == Some(-1) {
                    let right = self.players[pid].das.right;
                    self.players[pid].das.dir = if right { Some(1) } else { None };
                }
            }
            Key::RightDown => {
                self.players[pid].das.right = true;
                self.players[pid].das.dir = Some(1);
                self.try_shift(pid, 1);
            }
            Key::RightUp => {
                self.players[pid].das.right = false;
                if self.players[pid].das.dir == Some(1) {
                    let left = self.players[pid].das.left;
                    self.players[pid].das.dir = if left { Some(-1) } else { None };
                }
            }
            Key::SoftDown => self.players[pid].soft = true,
            Key::SoftUp => self.players[pid].soft = false,
            Key::RotCwDown => {
                self.players[pid].held_cw = true;
                self.players[pid].rot_last_pressed = Some(1);
                let r = self.players[pid].active.map(|a| a.rot);
                if let Some(r) = r {
                    self.try_rotate(pid, r + 1);
                }
            }
            Key::RotCwUp => {
                self.players[pid].held_cw = false;
                self.players[pid].rot_last_released = Some(1);
            }
            Key::RotCcwDown => {
                self.players[pid].held_ccw = true;
                self.players[pid].rot_last_pressed = Some(3);
                let r = self.players[pid].active.map(|a| a.rot);
                if let Some(r) = r {
                    self.try_rotate(pid, r + 3);
                }
            }
            Key::RotCcwUp => {
                self.players[pid].held_ccw = false;
                self.players[pid].rot_last_released = Some(3);
            }
            Key::HoldDown => {
                self.players[pid].held_hold = true;
                self.do_hold(pid);
            }
            Key::HoldUp => {
                self.players[pid].held_hold = false;
            }
            Key::RotCw => {
                let r = self.players[pid].active.map(|a| a.rot);
                if let Some(r) = r {
                    self.try_rotate(pid, r + 1);
                }
            }
            Key::RotCcw => {
                let r = self.players[pid].active.map(|a| a.rot);
                if let Some(r) = r {
                    self.try_rotate(pid, r + 3);
                }
            }
            Key::Rot180 => {
                if self.cfg.ruleset.allow_rot180 {
                    let r = self.players[pid].active.map(|a| a.rot);
                    if let Some(r) = r {
                        self.try_rotate(pid, r + 2);
                    }
                }
            }
            Key::HardDrop => {
                let (k, r, x) = match self.players[pid].active {
                    Some(a) => (a.kind, a.rot, a.x),
                    None => return,
                };
                let rest = rest_y(&self.players[pid], k, r, x);
                {
                    let p = &mut self.players[pid];
                    if let Some(a) = p.active.as_mut() {
                        a.y = rest;
                    }
                    // 硬降覆盖同帧软降，且不把软降状态带到下一块
                    p.soft = false;
                    // 保留 last_rot_kick：T99 语义——旋转后硬降仍成立 T-Spin（硬降不视为横向移动）
                }
                self.lock_piece(pid);
            }
            Key::Hold => self.do_hold(pid),
            Key::TargetBadges => self.set_target_mode(pid, TargetMode::Badges),
            Key::TargetKos => self.set_target_mode(pid, TargetMode::Kos),
            Key::TargetAttackers => self.set_target_mode(pid, TargetMode::Attackers),
            Key::TargetRandom => self.set_target_mode(pid, TargetMode::Random),
            Key::TargetPlayer(v) => {
                let t = v as usize;
                if t < self.players.len() && t != pid && self.players[t].alive {
                    self.players[pid].target_mode = TargetMode::Manual(t);
                    self.players[pid].last_target = Some(t);
                }
            }
            Key::Noop => {}
        }
    }

    fn set_target_mode(&mut self, pid: usize, mode: TargetMode) {
        self.players[pid].target_mode = mode;
        let others: Vec<usize> = self
            .players
            .iter()
            .filter(|p| p.alive && p.id != pid)
            .map(|p| p.id)
            .collect();
        self.players[pid].last_target = self.pick_target_ordered(pid, &others, mode);
    }

    fn pick_target_ordered(&mut self, pid: usize, others: &[usize], mode: TargetMode) -> Option<usize> {
        if others.is_empty() {
            return None;
        }
        match mode {
            TargetMode::Badges => {
                // 按徽章数（而非档位）排序：档位阈值稀疏（2/6/14/30），多数持有者挤在同档，
                // 同档按 pid 排序会把集火退化成「打最低 pid」，级联断链。
                let mut v = others.to_vec();
                v.sort_by(|&a, &b| {
                    self.players[b].badges.cmp(&self.players[a].badges).then(a.cmp(&b))
                });
                Some(v[0])
            }
            TargetMode::Kos => {
                // 「最接近死亡」= 堆高 + 待进垃圾（T99 的临近击倒语义：濒死含即将进场的垃圾）
                let near_death = |i: usize| -> i64 {
                    stack_height(&self.players[i]) as i64
                        + self.players[i].pending.iter().map(|g| g.rows as i64).sum::<i64>()
                };
                let mut v = others.to_vec();
                v.sort_by(|&a, &b| near_death(b).cmp(&near_death(a)).then(a.cmp(&b)));
                Some(v[0])
            }
            TargetMode::Random => {
                let idx = self.players[pid].rng_misc.below(others.len() as u64) as usize;
                Some(others[idx])
            }
            TargetMode::Attackers => {
                let mut atk: Vec<usize> = others
                    .iter()
                    .copied()
                    .filter(|&i| self.players[i].last_target == Some(pid))
                    .collect();
                atk.sort_unstable();
                if atk.is_empty() {
                    let idx = self.players[pid].rng_misc.below(others.len() as u64) as usize;
                    Some(others[idx])
                } else {
                    Some(atk[0])
                }
            }
            TargetMode::Manual(t) => {
                if others.contains(&t) {
                    Some(t)
                } else {
                    // 目标死亡或无效：退化为随机
                    let idx = self.players[pid].rng_misc.below(others.len() as u64) as usize;
                    Some(others[idx])
                }
            }
        }
    }

    fn resolve_receivers(&mut self, pid: usize, rows: u32) -> Vec<(usize, u32)> {
        let mode = self.players[pid].target_mode;
        let others: Vec<usize> = self
            .players
            .iter()
            .filter(|p| p.alive && p.id != pid)
            .map(|p| p.id)
            .collect();
        if others.is_empty() || rows == 0 {
            return vec![];
        }
        let out = match mode {
            TargetMode::Attackers => {
                let mut atk: Vec<usize> = others
                    .iter()
                    .copied()
                    .filter(|&i| self.players[i].last_target == Some(pid))
                    .collect();
                if atk.is_empty() {
                    let idx = self.players[pid].rng_misc.below(others.len() as u64) as usize;
                    vec![(others[idx], rows)]
                } else {
                    atk.sort_unstable();
                    let base = rows / atk.len() as u32;
                    let rem = rows % atk.len() as u32;
                    atk.iter()
                        .enumerate()
                        .map(|(i, &r)| (r, base + if (i as u32) < rem { 1 } else { 0 }))
                        .collect()
                }
            }
            m => {
                let t = self.pick_target_ordered(pid, &others, m);
                match t {
                    Some(t) => vec![(t, rows)],
                    None => vec![],
                }
            }
        };
        if let Some(&(first, _)) = out.first() {
            self.players[pid].last_target = Some(first);
        }
        out
    }

    /// 垃圾队列推进：仅队首计时；红阶段到点且无连击阻挡时进场。
    fn tick_garbage(&mut self, pid: usize, dt: Ticks) {
        loop {
            if !self.players[pid].alive || self.players[pid].pending.is_empty() {
                return;
            }
            let do_insert = {
                let stage = self.cfg.ruleset.garbage_stage(self.phase_idx);
                let p = &mut self.players[pid];
                match &mut p.pending[0].state {
                    GarbState::Gray => {
                        // 晋升为队首后开始计时
                        p.pending[0].state = GarbState::Yellow(stage);
                        false
                    }
                    GarbState::Yellow(t) => {
                        let nt = *t - dt;
                        p.pending[0].state = if nt <= 0 {
                            GarbState::Red(stage)
                        } else {
                            GarbState::Yellow(nt)
                        };
                        false
                    }
                    GarbState::Red(t) => {
                        let nt = *t - dt;
                        p.pending[0].state = GarbState::Red(nt.max(0));
                        // 连击进行中（combo >= 0）阻挡进场
                        nt <= 0 && p.combo < 0
                    }
                }
            };
            if do_insert {
                self.insert_first(pid);
            } else {
                return;
            }
        }
    }

    /// 队首垃圾进场：上移棋盘、活动块随之上推、完全推出场外判负。
    fn insert_first(&mut self, pid: usize) {
        let g = self.players[pid].pending.remove(0);
        let n = g.rows as usize;
        let last_sender = g.sender;
        let p_same = self.cfg.ruleset.p_same_hole_pct;
        {
            let p = &mut self.players[pid];
            p.board.copy_within(n * W..ROWS * W, 0);
            for i in 0..n {
                let row = ROWS - n + i;
                for c in 0..W {
                    p.board[row * W + c] = CELL_GARBAGE;
                }
            }
            let mut hole = p.rng_misc.below(W as u64) as usize;
            for i in 0..n {
                let row = ROWS - n + i;
                if i > 0 && p.rng_misc.below(100) >= p_same as u64 {
                    hole = p.rng_misc.below(W as u64) as usize;
                }
                p.board[row * W + hole] = 0;
            }
            if let Some(a) = p.active.as_mut() {
                a.y -= n as Milli * MILLI_CELL;
                // 被顶起后夹回场内：T99 语义——空中的块被顶不致死，块会从场顶重新落回；
                // 只有牌堆真被顶到场顶、夹回后无处容身时才判负（见下方挤压判定）
                let top = a
                    .kind
                    .cells(a.rot)
                    .iter()
                    .map(|&(_, dy)| (a.y + dy as Milli * MILLI_CELL).div_euclid(MILLI_CELL))
                    .min()
                    .unwrap_or(0);
                if top < 0 {
                    a.y += (-top) as Milli * MILLI_CELL;
                }
            }
            p.last_garbage_from = Some(last_sender);
        }
        // 挤压判定：夹回后仍与堆叠重叠 = 堆到顶无容身之处
        if let Some(a) = self.players[pid].active {
            if collides(&self.players[pid], a.kind, a.rot, a.x, a.y) {
                self.kill_player(pid, DeathReason::TopOut);
            }
        }
    }

    fn cancel_pending(&mut self, pid: usize, mut n: u32) {
        while n > 0 && !self.players[pid].pending.is_empty() {
            let take = n.min(self.players[pid].pending[0].rows);
            self.players[pid].pending[0].rows -= take;
            n -= take;
            if self.players[pid].pending[0].rows == 0 {
                self.players[pid].pending.remove(0);
            }
        }
    }

    /// 消行延迟结束：压实待消行、落盘就绪垃圾、进入 ARE。
    fn finish_clear(&mut self, pid: usize) {
        let rows = std::mem::take(&mut self.players[pid].clearing_rows);
        {
            let p = &mut self.players[pid];
            let mut kept: Vec<[Cell; W]> = Vec::with_capacity(ROWS);
            for r in 0..ROWS {
                if !rows.contains(&r) {
                    let mut row = [0; W];
                    row.copy_from_slice(&p.board[r * W..(r + 1) * W]);
                    kept.push(row);
                }
            }
            let base = ROWS - kept.len();
            for (i, row) in kept.iter().enumerate() {
                p.board[(base + i) * W..(base + i + 1) * W].copy_from_slice(row);
            }
            for r in 0..base {
                for c in 0..W {
                    p.board[r * W + c] = 0;
                }
            }
        }
        // 垃圾进场独立于锁定/消行（T99 语义），此处只压实与进入 ARE
        let are = self.cfg.ruleset.are(self.phase_idx);
        self.players[pid].phase = PlayerPhase::Are(are);
    }

    fn lock_piece(&mut self, pid: usize) {
        let (kind, rot, x, y) = match self.players[pid].active {
            Some(a) => (a.kind, a.rot, a.x, a.y),
            None => return,
        };
        // 本帧锁定后，剩余 piece-action 不再作用于下一块。
        self.players[pid].locked_this_frame = true;
        // 落章与锁定区判定
        let mut lockout = true;
        {
            let p = &mut self.players[pid];
            p.active = None;
            for (dx, dy) in kind.cells(rot) {
                let col = x + dx;
                let row = (y + dy as Milli * MILLI_CELL).div_euclid(MILLI_CELL);
                if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
                    continue;
                }
                p.board[row as usize * W + col as usize] = kind.id();
                if row >= HIDDEN as i64 {
                    lockout = false;
                }
            }
            p.pieces += 1;
        }
        if lockout {
            self.kill_player(pid, DeathReason::TopOut);
            return;
        }
        // T-Spin 判定（三角规则，踢墙索引 4 升格）
        let spin = if kind == Kind::T {
            match self.players[pid].last_rot_kick {
                Some(kick_idx) => {
                    let p = &self.players[pid];
                    let yr = y.div_euclid(MILLI_CELL);
                    let filled = |cx: i32, cy: i32| -> bool {
                        let col = x + cx;
                        let row = yr + cy as i64;
                        if col < 0 || col >= W as i32 || row < 0 || row >= ROWS as i64 {
                            return true;
                        }
                        p.board[row as usize * W + col as usize] != 0
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
                        SpinKind::Full
                    } else if f >= 1 && b == 2 {
                        if kick_idx == 4 {
                            SpinKind::Full
                        } else {
                            SpinKind::Mini
                        }
                    } else {
                        SpinKind::None
                    }
                }
                None => SpinKind::None,
            }
        } else {
            SpinKind::None
        };
        // 消行
        let mut full_rows: Vec<usize> = Vec::new();
        {
            let p = &self.players[pid];
            for r in 0..ROWS {
                if p.board[r * W..(r + 1) * W].iter().all(|&c| c != 0) {
                    full_rows.push(r);
                }
            }
        }
        let n = full_rows.len() as u32;
        // 全消判定（不改动棋盘：非满行全空即全消）
        let allclear = if n > 0 {
            let p = &self.players[pid];
            (0..ROWS).all(|r| {
                full_rows.contains(&r) || p.board[r * W..(r + 1) * W].iter().all(|&c| c == 0)
            })
        } else {
            false
        };
        {
            let p = &mut self.players[pid];
            p.lines += n;
            if n > 0 {
                p.combo += 1;
            } else {
                p.combo = -1;
            }
        }
        // 送出量计算
        let base: u32 = match (spin, n) {
            (SpinKind::Full, 1) => 2,
            (SpinKind::Full, 2) => 4,
            (SpinKind::Full, 3) => 6,
            (SpinKind::Full, _) => 4,
            (SpinKind::Mini, 2) => 1,
            (SpinKind::Mini, _) => 0,
            (SpinKind::None, 2) => 1,
            (SpinKind::None, 3) => 2,
            (SpinKind::None, 4) => 4,
            (SpinKind::None, _) => 0,
        };
        let qualifies = spin != SpinKind::None || n == 4;
        let mut b2b_bonus = 0u32;
        {
            let p = &mut self.players[pid];
            if n > 0 {
                if qualifies {
                    if p.b2b {
                        b2b_bonus = 1;
                    }
                    p.b2b = true;
                } else {
                    p.b2b = false;
                }
            }
        }
        let combo = self.players[pid].combo;
        let cb = if n > 0 && combo >= 1 {
            self.cfg.ruleset.combo_bonus[combo.min(10) as usize]
        } else {
            0
        };
        let attackers = self
            .players
            .iter()
            .filter(|q| q.alive && q.id != pid && q.last_target == Some(pid))
            .count();
        let tb = self.cfg.ruleset.target_bonus[attackers.min(6)];
        let ac = if allclear { 4 } else { 0 };
        let subtotal = base + b2b_bonus + cb + tb + ac;
        if n > 0 {
            let p = &mut self.players[pid];
            p.last_clear = Some(ClearView {
                lines: n,
                spin: spin as u8,
                all_clear: allclear,
                b2b: b2b_bonus > 0,
                frame: self.frame_n,
            });
        }
        let tier = self.badge_tier(pid);
        let cap = self.cfg.ruleset.send_cap_rows;
        let total = ((subtotal * (100 + 25 * tier)) / 100).min(cap);
        // T99 阻挡规则（wiki：有暂缓垃圾行时，攻击全部用于阻挡；只有没有暂缓垃圾时才能发送）：
        // 攻击值（含 B2B/连击/徽章倍率）先抵扣暂缓队列，队列清空后的剩余攻击才发给别人。
        let pending_rows: u32 = self.players[pid].pending.iter().map(|g| g.rows).sum();
        let mut sent_rows = total;
        if n > 0 && total > 0 && pending_rows > 0 {
            let blocked = total.min(pending_rows);
            self.cancel_pending(pid, blocked);
            sent_rows = total - blocked;
        }
        if n > 0 {
            let d = self.cfg.ruleset.clear_delay(self.phase_idx);
            let p = &mut self.players[pid];
            p.clearing_rows = full_rows;
            p.phase = PlayerPhase::Clear(d);
        } else {
            // 垃圾独立进场，无消行直接进 ARE
            let are = self.cfg.ruleset.are(self.phase_idx);
            self.players[pid].phase = PlayerPhase::Are(are);
        }
        if n > 0 && sent_rows > 0 && self.alive_count > 1 {
            let recv = self.resolve_receivers(pid, sent_rows);
            for (rid, rows) in recv {
                let room = self.cfg.ruleset.pending_cap_rows
                    - self.players[rid]
                        .pending
                        .iter()
                        .map(|g| g.rows)
                        .sum::<u32>();
                let rows = rows.min(room);
                if rows > 0 {
                    // 队列为空立即开始计时（黄），否则灰排队
                    let state = if self.players[rid].pending.is_empty() {
                        GarbState::Yellow(self.cfg.ruleset.garbage_stage(self.phase_idx))
                    } else {
                        GarbState::Gray
                    };
                    self.players[rid].pending.push(Garb { rows, sender: pid, state });
                }
            }
            self.players[pid].sent += sent_rows;
        }
    }

    fn badge_tier(&self, pid: usize) -> u32 {
        let ko = self.players[pid].badges;
        let t = &self.cfg.ruleset.badge_tier_units;
        let mut tier = 0;
        if ko >= t[3] {
            tier = 4;
        } else if ko >= t[2] {
            tier = 3;
        } else if ko >= t[1] {
            tier = 2;
        } else if ko >= t[0] {
            tier = 1;
        }
        tier
    }

    fn kill_player(&mut self, pid: usize, reason: DeathReason) {
        if !self.players[pid].alive {
            return;
        }
        if reason == DeathReason::TopOut {
            if let Some(k) = self.players[pid].last_garbage_from {
                if k != pid && self.players[k].alive {
                    // T99：击倒者获得被击倒者全部徽章并额外 +1
                    self.players[k].ko += 1;
                    let gained = self.players[pid].badges + 1;
                    self.players[k].badges += gained;
                }
            }
        }
        {
            let p = &mut self.players[pid];
            p.alive = false;
            p.active = None;
            p.death_reason = Some(reason);
            p.place = Some(self.alive_count as u32);
            p.death_frame = Some(self.frame_n);
            p.pending.clear();
        }
        self.alive_count -= 1;
        self.last_death_frame = Some(self.frame_n);
        // 节奏相位重算（ADR-018）
        let new_idx = self.cfg.ruleset.phase_of(self.alive_count.max(1), self.cfg.players);
        if new_idx != self.phase_idx {
            self.phase_idx = new_idx;
            self.phase_ticks = 0;
        }
        if self.cfg.players > 1 && self.alive_count <= 1 {
            for p in &mut self.players {
                if p.alive {
                    p.place = Some(1);
                }
            }
            self.over = Some(MatchResult {
                reason: "last_alive",
                frame: self.frame_n,
                places: self.places(),
            });
        } else if self.alive_count == 0 {
            self.over = Some(MatchResult {
                reason: "top_out",
                frame: self.frame_n,
                places: self.places(),
            });
        }
    }

    /// 宿主强制判负（超时/崩溃/OOM）
    pub fn kill(&mut self, pid: usize, why: &str) {
        self.kill_player(pid, DeathReason::HostKill(why.to_string()));
    }

    pub fn places(&self) -> Vec<(usize, u32)> {
        let mut v: Vec<(usize, u32)> = self
            .players
            .iter()
            .filter_map(|p| p.place.map(|pl| (p.id, pl)))
            .collect();
        v.sort_by_key(|&(_, pl)| pl);
        v
    }

    /// 排序键：存活 > 送出 > 消行 > 棋盘高度 > 编号。返回 pid 顺序（名次序）。
    pub fn ranking(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.players.iter().map(|p| p.id).collect();
        v.sort_by(|&a, &b| {
            let (pa, pb) = (&self.players[a], &self.players[b]);
            pb.alive
                .cmp(&pa.alive)
                .then(pb.sent.cmp(&pa.sent))
                .then(pb.lines.cmp(&pa.lines))
                .then(stack_height(pa).cmp(&stack_height(pb)))
                .then(a.cmp(&b))
        });
        v
    }

    /// 流局（倒计时耗尽）名次：幸存者仅按徽章数降序，平局按编号
    fn ranking_badges(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.players.len()).filter(|&p| self.players[p].alive).collect();
        v.sort_by(|&a, &b| self.players[b].badges.cmp(&self.players[a].badges).then(a.cmp(&b)));
        v
    }

    pub fn final_table(&self) -> Vec<TableRow> {
        self.players
            .iter()
            .map(|p| TableRow {
                pid: p.id,
                place: p.place,
                rank: 0,
                death_frame: p.death_frame,
                alive: p.alive,
                ko: p.ko,
                badges: p.badges,
                lines: p.lines,
                sent: p.sent,
                pieces: p.pieces,
                reason: p.death_reason.as_ref().map(|r| match r {
                    DeathReason::TopOut => "top_out".to_string(),
                    DeathReason::HostKill(s) => format!("host_kill:{}", s),
                }),
            })
            .collect()
    }

    /// 结算名次：正常局按淘汰序；流局幸存者按徽章，已淘汰者按淘汰序；
    /// 同徽章 / 同 death_frame 共享名次，下一组跳过该组人数。
    pub fn final_ranking(&self) -> Vec<TableRow> {
        let mut rows = self.final_table();
        let time_limit = self
            .over
            .as_ref()
            .map(|o| o.reason == "time_limit")
            .unwrap_or(false);

        let mut survivors: Vec<usize> = (0..self.players.len())
            .filter(|&p| self.players[p].alive)
            .collect();
        if time_limit {
            survivors.sort_by(|&a, &b| {
                self.players[b]
                    .badges
                    .cmp(&self.players[a].badges)
                    .then(a.cmp(&b))
            });
        } else {
            survivors.sort_unstable();
        }

        let mut rank = 1u32;
        let mut i = 0;
        while i < survivors.len() {
            let j = if time_limit {
                let badges = self.players[survivors[i]].badges;
                let mut j = i + 1;
                while j < survivors.len() && self.players[survivors[j]].badges == badges {
                    j += 1;
                }
                j
            } else {
                i + 1
            };
            for &pid in &survivors[i..j] {
                rows[pid].rank = rank;
            }
            rank += (j - i) as u32;
            i = j;
        }

        let mut dead: Vec<usize> = (0..self.players.len())
            .filter(|&p| !self.players[p].alive)
            .collect();
        dead.sort_by(|&a, &b| {
            self.players[b]
                .death_frame
                .cmp(&self.players[a].death_frame)
                .then(a.cmp(&b))
        });
        let mut i = 0;
        while i < dead.len() {
            let frame = self.players[dead[i]].death_frame;
            let mut j = i + 1;
            while j < dead.len() && self.players[dead[j]].death_frame == frame {
                j += 1;
            }
            for &pid in &dead[i..j] {
                rows[pid].rank = rank;
            }
            rank += (j - i) as u32;
            i = j;
        }

        rows.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.pid.cmp(&b.pid)));
        rows
    }

    pub fn alive_count(&self) -> usize {
        self.alive_count
    }

    pub fn snapshot(&self, pid: usize) -> Snapshot {
        let p = &self.players[pid];
        let pid_field = pid;
        let mut board = [[0; W]; H];
        for r in 0..H {
            for c in 0..W {
                board[r][c] = p.board[(HIDDEN + r) * W + c];
            }
        }
        let active = p.active.map(|a| ActiveView {
            kind: a.kind,
            rot: a.rot,
            x: a.x,
            y_milli: a.y - (HIDDEN as Milli) * MILLI_CELL,
        });
        let mut garbage_red = 0u32;
        let mut garbage_yellow = 0u32;
        let mut garbage_gray = 0u32;
        for (i, g) in p.pending.iter().enumerate() {
            // 队首 Gray 会在下一拍晋升，计入黄
            match (&g.state, i) {
                (GarbState::Red(_), _) => garbage_red += g.rows,
                (GarbState::Yellow(_), _) | (GarbState::Gray, 0) => garbage_yellow += g.rows,
                (GarbState::Gray, _) => garbage_gray += g.rows,
            }
        }
        Snapshot {
            n: self.frame_n,
            board,
            active,
            phase: p.phase,
            next: p.queue.iter().take(self.cfg.ruleset.preview).copied().collect(),
            hold: p.hold,
            hold_used: p.hold_used,
            combo: p.combo,
            b2b: p.b2b,
            garbage_red,
            garbage_yellow,
            garbage_gray,
            clearing: p.clearing_rows.iter().map(|&r| r as isize - HIDDEN as isize).filter(|&r| r >= 0).map(|r| r as usize).collect(),
            badges: self.badge_tier(pid),
            badge_units: p.badges,
            target: p.last_target.filter(|&t| self.players[t].alive),
            target_mode: p.target_mode,
            alive: self.players.iter().filter(|q| q.alive).map(|q| q.id).collect(),
            gravity_milli: self.cfg.ruleset.gravity(self.phase_idx, self.phase_ticks),
            soft: p.soft,
            lock_ms_left: ((self.cfg.ruleset.lock_ticks - p.lock_timer) / TMS).max(0),
            lock_resets: p.lock_resets,
            das: DasView {
                left_held: p.das.left,
                right_held: p.das.right,
                active_dir: p.das.dir,
                das_ms: p.das.das / TMS,
                arr_ms: p.das.arr / TMS,
            },
            ko: p.ko,
            lines: p.lines,
            sent: p.sent,
            pieces: p.pieces,
            alive_self: p.alive,
            place: p.place,
            match_secs_left: ((self.match_ticks_left + 1000 * TMS - 1) / (1000 * TMS)) as u32,
            burn_x10: ((self.burn_ticks() as u128 * 10 / self.cfg.ruleset.frame_ticks().max(1) as u128) as u16).max(1),
            pid: pid_field,
            badges_all: self.players.iter().map(|p| p.badges).collect(),
            targeted_by: (0..self.players.len())
                .filter(|&q| q != pid && self.players[q].alive && self.players[q].last_target == Some(pid))
                .collect(),
            last_clear: p.last_clear,
        }
    }

    pub fn hash_state(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        fn mix(h: &mut u64, b: u8) {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x100_0000_01b3);
        }
        fn mix_i(h: &mut u64, v: i64) {
            for b in v.to_le_bytes() {
                mix(h, b);
            }
        }
        mix_i(&mut h, self.frame_n as i64);
        mix_i(&mut h, self.over.is_some() as i64);
        mix_i(&mut h, self.phase_idx as i64);
        mix_i(&mut h, self.phase_ticks);
        for p in &self.players {
            mix_i(&mut h, p.id as i64);
            mix(&mut h, p.phase.tag().as_bytes()[0]);
            mix_i(&mut h, p.phase.ms_left());
            for &c in &p.board {
                mix(&mut h, c);
            }
            match p.active {
                Some(a) => {
                    mix(&mut h, a.kind.id());
                    mix(&mut h, a.rot);
                    mix_i(&mut h, a.x as i64);
                    mix_i(&mut h, a.y);
                }
                None => mix(&mut h, 255),
            }
            mix_i(&mut h, p.queue.len() as i64);
            for &k in &p.queue {
                mix(&mut h, k.id());
            }
            mix_i(&mut h, p.bag.len() as i64);
            for &k in &p.bag {
                mix(&mut h, k.id());
            }
            mix(&mut h, p.hold.map(|k| k.id()).unwrap_or(0));
            mix(&mut h, p.hold_used as u8);
            mix(&mut h, p.soft as u8);
            mix_i(&mut h, p.das.das);
            mix_i(&mut h, p.das.arr);
            mix(&mut h, p.das.dir.map(|d| d as i8 as u8).unwrap_or(255));
            mix_i(&mut h, p.lock_timer);
            mix_i(&mut h, p.lock_resets as i64);
            mix(&mut h, p.grounded as u8);
            mix(&mut h, p.last_rot_kick.map(|k| k + 1).unwrap_or(0));
            mix_i(&mut h, p.combo as i64);
            mix(&mut h, p.b2b as u8);
            mix_i(&mut h, p.pending.len() as i64);
            for g in &p.pending {
                mix_i(&mut h, g.rows as i64);
                mix_i(&mut h, g.sender as i64);
                mix(&mut h, g.state.tag());
                match g.state {
                    GarbState::Yellow(t) | GarbState::Red(t) => mix_i(&mut h, t),
                    GarbState::Gray => {}
                }
            }
            mix_i(&mut h, p.badges as i64);
            mix_i(&mut h, p.clearing_rows.len() as i64);
            mix_i(&mut h, p.ko as i64);
            mix_i(&mut h, p.lines as i64);
            mix_i(&mut h, p.sent as i64);
            mix_i(&mut h, p.pieces as i64);
            mix(&mut h, match p.target_mode {
                TargetMode::Badges => 1u8,
                TargetMode::Kos => 2,
                TargetMode::Attackers => 3,
                TargetMode::Random => 4,
                TargetMode::Manual(t) => (5u8.wrapping_add(t as u8)),
            });
            mix(&mut h, p.last_target.map(|t| t as u8 + 1).unwrap_or(0));
            mix(&mut h, p.alive as u8);
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solo() -> Match {
        Match::new(MatchConfig { players: 1, seed: 42, ruleset: Ruleset::default() })
    }

    fn hard_only(n_frames: u64) -> Vec<Vec<TimedKey>> {
        vec![vec![TimedKey { t_us: 0, key: Key::HardDrop }]; n_frames as usize]
    }

    #[test]
    fn first_seven_pieces_are_one_bag() {
        let m = solo();
        let p = &m.players[0];
        let mut kinds: Vec<u8> = p.queue.iter().map(|k| k.id()).collect();
        kinds.push(p.active.unwrap().kind.id());
        kinds.sort_unstable();
        let mut expect: Vec<u8> = Kind::ALL.iter().map(|k| k.id()).collect();
        expect.sort_unstable();
        assert_eq!(kinds, expect);
    }

    #[test]
    fn sanitize_rejects_bad() {
        use crate::sanitize_events;
        assert!(sanitize_events(&[TimedKey { t_us: 0, key: Key::HardDrop }]).is_some());
        // t 必须严格递增
        assert!(sanitize_events(&[
            TimedKey { t_us: 5, key: Key::HardDrop },
            TimedKey { t_us: 5, key: Key::RotCw }
        ]).is_none());
        // 同帧重复方向键合法（离散移动）；其他动作仍拒绝
        assert!(sanitize_events(&[
            TimedKey { t_us: 0, key: Key::LeftDown },
            TimedKey { t_us: 100, key: Key::LeftDown }
        ]).is_some());
        assert!(sanitize_events(&[
            TimedKey { t_us: 0, key: Key::RotCw },
            TimedKey { t_us: 100, key: Key::RotCw }
        ]).is_none());
        // 目标选择同帧可重复，按顺序后者生效
        assert!(sanitize_events(&[
            TimedKey { t_us: 0, key: Key::TargetPlayer(1) },
            TimedKey { t_us: 100, key: Key::TargetPlayer(2) }
        ]).is_some());
        // 越界 t
        assert!(sanitize_events(&[TimedKey { t_us: 16667, key: Key::HardDrop }]).is_none());
        // >16
        let many: Vec<TimedKey> = (0..17).map(|i| TimedKey { t_us: i * 900, key: Key::Noop }).collect();
        assert!(sanitize_events(&many).is_none());
        // 帧内 press+release 合法
        assert!(sanitize_events(&[
            TimedKey { t_us: 0, key: Key::LeftDown },
            TimedKey { t_us: 1000, key: Key::LeftUp }
        ]).is_some());
    }

    #[test]
    fn ranking_normal_and_same_frame_ties() {
        let cfg = MatchConfig { players: 4, seed: 1, ruleset: Ruleset::default() };
        let mut m = Match::new(cfg);
        m.frame_n = 10;
        m.kill_player(3, DeathReason::TopOut);
        m.kill_player(2, DeathReason::TopOut);
        m.frame_n = 20;
        m.kill_player(1, DeathReason::TopOut);
        let r = m.final_ranking();
        assert_eq!((r[0].pid, r[0].rank), (0, 1));
        assert_eq!((r[1].pid, r[1].rank), (1, 2));
        assert_eq!(r[2].rank, 3);
        assert_eq!(r[3].rank, 3);
        assert_eq!((r[2].pid, r[3].pid), (2, 3));
    }

    #[test]
    fn ranking_draw_uses_badges_then_elimination_order() {
        let cfg = MatchConfig { players: 4, seed: 1, ruleset: Ruleset::default() };
        let mut m = Match::new(cfg);
        m.frame_n = 10;
        m.kill_player(3, DeathReason::TopOut);
        m.players[0].badges = 5;
        m.players[1].badges = 5;
        m.players[2].badges = 2;
        m.players[3].badges = 9;
        m.over = Some(MatchResult { reason: "time_limit", frame: 100, places: vec![] });
        let r = m.final_ranking();
        assert_eq!((r[0].pid, r[0].rank), (0, 1));
        assert_eq!((r[1].pid, r[1].rank), (1, 1));
        assert_eq!((r[2].pid, r[2].rank), (2, 3));
        assert_eq!((r[3].pid, r[3].rank), (3, 4));
    }

    #[test]
    fn irs_edge_rotates_on_spawn() {
        let mut m = solo();
        m.players[0].active = None;
        m.apply_key(0, Key::RotCwDown);
        m.apply_key(0, Key::RotCwUp);
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().rot, 1);
    }

    #[test]
    fn irs_held_rotates_on_spawn() {
        let mut m = solo();
        m.cfg.ruleset.initial_trigger = InitialTrigger::Held;
        m.players[0].active = None;
        m.players[0].held_cw = true;
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().rot, 1);
    }

    #[test]
    fn initial_rotation_direction_rules() {
        let mut m = solo();
        m.players[0].active = None;
        m.apply_key(0, Key::RotCwDown);
        m.apply_key(0, Key::RotCcwDown);
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().rot, 3); // 两边按住：后按下者

        let mut m = solo();
        m.players[0].active = None;
        m.apply_key(0, Key::RotCwDown);
        m.apply_key(0, Key::RotCcwDown);
        m.apply_key(0, Key::RotCwUp);
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().rot, 1); // 已松开：后松开者
    }

    #[test]
    fn ihs_swaps_before_spawn() {
        let mut m = solo();
        m.players[0].active = None;
        m.players[0].hold = Some(Kind::I);
        m.apply_key(0, Key::HoldDown);
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().kind, Kind::I);
        assert_eq!(m.players[0].hold, Some(Kind::T));
    }

    #[test]
    fn lock_frame_ignores_later_piece_actions() {
        let mut m = solo();
        m.apply_key(0, Key::HardDrop);
        assert!(m.players[0].active.is_none());
        assert!(m.players[0].locked_this_frame);
        m.apply_key(0, Key::RotCwDown);
        m.spawn(0, Some(Kind::T), false);
        assert_eq!(m.players[0].active.unwrap().rot, 0);
    }

    #[test]
    fn discrete_direction_events_move_one_cell_each() {
        let mut m = solo();
        let x0 = m.players[0].active.expect("spawned piece").x;
        let evs = vec![
            TimedKey { t_us: 0, key: Key::LeftDown },
            TimedKey { t_us: 1000, key: Key::LeftDown },
            TimedKey { t_us: 2000, key: Key::LeftUp },
        ];
        m.frame(&[evs]);
        let x1 = m.players[0].active.expect("piece alive").x;
        assert_eq!(x1, x0 - 2, "each left_down must move exactly one cell");
    }

    #[test]
    fn i_piece_wall_kick() {
        let mut m = solo();
        // I 在盒 x=0、底部 y=22000（rot0 占 row23），顺时针旋转应通过第 4 个踢墙落到 (x=1, y=20000)
        m.players[0].active = Some(Active { kind: Kind::I, rot: 0, x: 0, y: 22000 });
        assert!(m.try_rotate(0, 1));
        let a = m.players[0].active.unwrap();
        assert_eq!((a.rot, a.x, a.y), (1, 1, 20000));
        assert_eq!(m.players[0].last_rot_kick, Some(4));
    }

    fn run_quiet(m: &mut Match, frames: u64) {
        let n = m.cfg.players;
        for _ in 0..frames {
            let inputs: Vec<Vec<TimedKey>> = (0..n).map(|_| vec![]).collect();
            m.frame(&inputs);
        }
    }

    #[test]
    fn stall_accel_burns_countdown_to_time_limit() {
        let mut m = Match::new(MatchConfig {
            players: 3,
            seed: 9,
            ruleset: Ruleset { stall_accel: true, match_ticks: 10 * 1000 * TMS, ..Default::default() },
        });
        assert_eq!(m.snapshot(0).match_secs_left, 10);
        // 锚点拨到 0 杀已过 70s：单帧燃烧 = T/60 = 70/60 倍
        m.frame_n = 70 * 60;
        m.last_death_frame = Some(0);
        let before = m.match_ticks_left;
        m.frame(&[vec![], vec![], vec![]]);
        // frame() 先自增 frame_n 再燃烧 → gap = 4201
        let frame_ticks = m.cfg.ruleset.frame_ticks();
        assert_eq!(m.match_ticks_left, before - (frame_ticks * 4201) / 3600);
        // 击杀复位 1×
        m.kill(1, "test");
        assert_eq!(m.last_death_frame, Some(m.frame_n));
        let before = m.match_ticks_left;
        m.frame(&[vec![], vec![], vec![]]);
        assert_eq!(m.match_ticks_left, before - frame_ticks);
        // 锚点留在帧 0（gap 已远超 60s，燃烧 >1× 且持续加速）→ 预算烧完即 time_limit 终局
        m.last_death_frame = Some(0);
        let mut guard = 0;
        while m.over.is_none() && guard < 100_000 {
            m.frame(&[vec![], vec![], vec![]]);
            guard += 1;
        }
        assert_eq!(m.over.as_ref().unwrap().reason, "time_limit");
        // 幸存者按排序键得名次
        assert!(m.over.unwrap().places.iter().all(|&(_, pl)| pl >= 1));
    }

    #[test]
    fn no_accel_ends_at_budget_linearly() {
        let mut m = Match::new(MatchConfig {
            players: 2,
            seed: 9,
            ruleset: Ruleset { stall_accel: false, match_ticks: 10 * 1000 * TMS, ..Default::default() },
        });
        assert_eq!(m.snapshot(0).match_secs_left, 10);
        run_quiet(&mut m, 300);
        assert!(m.over.is_none());
        assert_eq!(m.snapshot(0).match_secs_left, 5);
        run_quiet(&mut m, 301);
        // 无人死亡也能按排序键终局（time_limit）
        assert_eq!(m.over.as_ref().unwrap().reason, "time_limit");
        assert_eq!(m.alive_count(), 2);
    }

    #[test]
    fn targeted_by_lists_aiming_players() {
        let mut m = Match::new(MatchConfig { players: 3, seed: 9, ruleset: Ruleset::default() });
        assert!(m.snapshot(0).targeted_by.is_empty());
        m.players[1].last_target = Some(0);
        m.players[2].last_target = Some(0);
        assert_eq!(m.snapshot(0).targeted_by, vec![1, 2]);
        assert!(m.snapshot(1).targeted_by.is_empty());
    }

    #[test]
    fn gravity_ramps_only_half_step_and_records_clear() {
        let r = Ruleset::default();
        let minute = MINUTES_TICKS;
        // P1 内线性爬坡（750 + 120/min），封顶在到 P2 阶梯的一半：750 + 750/2 = 1125
        assert_eq!(r.gravity(0, minute), 870);
        assert_eq!(r.gravity(0, 100 * minute), 1125);
        // P2 内封顶 2250；P4 末相位爬到自身 cap（1G）
        assert_eq!(r.gravity(1, 100 * minute), 2250);
        assert_eq!(r.gravity(3, 100 * minute), 60_000);
        assert_eq!(r.gravity_hold(0), 1125);
    }

    #[test]
    fn gravity_accumulator_exact() {
        let mut m = solo(); // players=1 → 相位0
        let base = m.cfg.ruleset.phases[0].gravity_base;
        for _ in 0..3 {
            m.frame(&[]);
        }
        // 3 帧内未触底，位移应精确为 base*3/60 毫格
        let a = m.players[0].active.unwrap();
        assert_eq!(a.y, 4000 + base * 3 / 60);
    }

    #[test]
    fn clear_then_are_gates_spawn() {
        let mut m = solo();
        m.players[0].active = Some(Active { kind: Kind::O, rot: 0, x: 4, y: 22000 });
        m.lock_piece(0);
        // 无消行 → 直接 ARE，期间无活动块
        assert!(m.players[0].active.is_none());
        assert!(matches!(m.players[0].phase, PlayerPhase::Are(_)));
        let are_frames = m.cfg.ruleset.are(0) / m.cfg.ruleset.frame_ticks() + 2;
        run_quiet(&mut m, are_frames as u64);
        assert!(m.players[0].active.is_some());
        assert_eq!(m.players[0].phase, PlayerPhase::Play);
    }

    #[test]
    fn doubled_tick_rate_halves_frame_ticks_without_changing_absolute_timing() {
        let mut r = Ruleset::default();
        r.tick_hz = 120;
        assert_eq!(r.frame_ticks(), TPS / 120);
        // 规则时长是绝对时间，不随 tick_hz 变化
        assert_eq!(r.are(0), 100 * TMS);
        assert_eq!(r.lock_ticks, 500 * TMS);

        let mut m = Match::new(MatchConfig { players: 1, seed: 3, ruleset: r });
        m.players[0].active = Some(Active { kind: Kind::O, rot: 0, x: 4, y: 22000 });
        m.lock_piece(0);
        assert!(matches!(m.players[0].phase, PlayerPhase::Are(_)));
        // 120Hz 下 ARE 100ms = 12 帧
        run_quiet(&mut m, 11);
        assert!(m.players[0].active.is_none());
        run_quiet(&mut m, 2);
        assert!(m.players[0].active.is_some());
        assert_eq!(m.players[0].phase, PlayerPhase::Play);
    }

    #[test]
    fn phase_advances_on_deaths() {
        let mut m = Match::new(MatchConfig { players: 4, seed: 1, ruleset: Ruleset::default() });
        assert_eq!(m.phase_idx, 0);
        m.kill(1, "t");
        m.kill(2, "t");
        assert_eq!(m.phase_idx, 0); // 2/4 = 500‰
        m.kill(3, "t");
        assert_eq!(m.phase_idx, 2); // 1/4 = 250‰ → 跳过相位1
    }

    #[test]
    fn manual_target_locks_receiver() {
        let mut m = Match::new(MatchConfig { players: 3, seed: 5, ruleset: Ruleset::default() });
        m.apply_key(0, Key::TargetPlayer(2));
        assert_eq!(m.players[0].target_mode, TargetMode::Manual(2));
        assert_eq!(m.resolve_receivers(0, 4), vec![(2, 4)]);
        m.kill(2, "t");
        let recv = m.resolve_receivers(0, 4);
        assert_eq!(recv.len(), 1);
        assert_eq!(recv[0].0, 1); // 退化为存活他人
    }

    #[test]
    fn hard_drop_preserves_rot_flag_for_t_spin() {
        // T99 语义：旋转→硬降仍成立 T-Spin（旋转是最后一次有效操作，硬降不视为移动）
        let mut m = Match::new(MatchConfig { players: 2, seed: 7, ruleset: Ruleset::default() });
        let p = &mut m.players[0];
        // 构造 TSD 槽：row23 全满缺 col4；row22 全满缺 col3..5；row21 col3、col4 填充（悬挑）
        for c in 0..W {
            if c != 4 {
                p.board[23 * W + c] = 1;
            }
            if !(3..=5).contains(&c) {
                p.board[22 * W + c] = 1;
            }
        }
        p.board[21 * W + 3] = 1;
        p.board[21 * W + 4] = 1;
        // T 悬在槽口上方（rot2、x=3、y=17000），空中顺时针旋到 rot3，然后硬降
        m.players[0].active = Some(Active { kind: Kind::T, rot: 2, x: 3, y: 17000 });
        m.apply_key(0, Key::RotCw);
        assert_eq!(m.players[0].active.unwrap().rot, 3);
        assert_eq!(m.players[0].last_rot_kick, Some(0));
        m.apply_key(0, Key::HardDrop);
        assert!(
            m.players[0].last_rot_kick.is_some(),
            "硬降不得清除旋转标记，否则旋转后硬降永远无法成立 T-Spin"
        );
    }

    #[test]
    fn t_spin_double_sends_four() {
        let mut m = Match::new(MatchConfig { players: 2, seed: 7, ruleset: Ruleset::default() });
        let p = &mut m.players[0];
        // 构造 TSD 槽：row23 全满缺 col4；row22 全满缺 col3..5；row21 col3、col4 填充（悬挑）
        for c in 0..W {
            if c != 4 {
                p.board[23 * W + c] = 1;
            }
            if !(3..=5).contains(&c) {
                p.board[22 * W + c] = 1;
            }
        }
        p.board[21 * W + 3] = 1;
        p.board[21 * W + 4] = 1;
        p.active = Some(Active { kind: Kind::T, rot: 2, x: 3, y: 21000 });
        p.last_rot_kick = Some(0);
        m.lock_piece(0);
        let p0 = &m.players[0];
        assert_eq!(p0.lines, 2);
        assert_eq!(p0.sent, 4);
        // 对手立即入队 4 行（黄/灰计时中）
        let rows: u32 = m.players[1].pending.iter().map(|g| g.rows).sum();
        assert_eq!(rows, 4);
        // 消行经 Clear 延迟后压实：悬挑格沉底至 row23，其余清空
        run_quiet(&mut m, 60);
        let p0 = &m.players[0];
        assert!(p0.board[22 * W..23 * W].iter().all(|&c| c == 0));
        assert_eq!(p0.board[23 * W + 3], 1);
        assert_eq!(p0.board[23 * W + 4], 1);
        assert!(p0.board[23 * W + 5..24 * W].iter().all(|&c| c == 0));
        assert!(p0.board[0..22 * W].iter().all(|&c| c == 0));
    }

    #[test]
    fn garbage_inserts_independently_and_cancels() {
        let mut m = Match::new(MatchConfig { players: 2, seed: 9, ruleset: Ruleset::default() });
        // 2 人局 P1：黄/红各 1200ms，总排队 2.4s
        let stage = 1200 * TMS;
        m.players[0].pending.push(Garb { rows: 3, sender: 1, state: GarbState::Yellow(stage) });
        // 无消行（无阻挡）：约 145 帧后 3 行进场（独立于落块）
        run_quiet(&mut m, 150);
        let p = &m.players[0];
        assert!(p.pending.is_empty());
        for r in 22..ROWS {
            let garbage = (0..W).filter(|&c| p.board[r * W + c] == CELL_GARBAGE).count();
            assert_eq!(garbage, 9);
        }
        // T99 阻挡规则：单消（攻击值 0）不阻挡；有暂缓垃圾时攻击全部转为阻挡，不发送；
        // 队列清空后的剩余攻击才发送
        let mut m2 = Match::new(MatchConfig { players: 2, seed: 9, ruleset: Ruleset::default() });
        {
            let p = &mut m2.players[0];
            for c in 0..W {
                if c < 9 {
                    p.board[23 * W + c] = 1;
                }
            }
            p.active = Some(Active { kind: Kind::O, rot: 0, x: 8, y: 22000 });
        }
        m2.players[0].pending.push(Garb { rows: 3, sender: 1, state: GarbState::Yellow(stage) });
        m2.lock_piece(0); // 消 1 行（single，攻击值 0）→ 阻挡 0，剩 3，不发送
        let rows: u32 = m2.players[0].pending.iter().map(|g| g.rows).sum();
        assert_eq!(rows, 3, "单消攻击值为 0，不应阻挡");
        assert_eq!(m2.players[0].sent, 0, "有暂缓垃圾时不发送");
        // 双消（攻击值 1，留残块避免全消奖励）→ 阻挡 1，剩 2
        let mut m3 = Match::new(MatchConfig { players: 2, seed: 9, ruleset: Ruleset::default() });
        {
            let p = &mut m3.players[0];
            p.board[21 * W] = 1;
            for c in 0..W {
                if c < 8 {
                    p.board[22 * W + c] = 1;
                    p.board[23 * W + c] = 1;
                }
            }
            p.active = Some(Active { kind: Kind::O, rot: 0, x: 8, y: 22000 });
        }
        m3.players[0].pending.push(Garb { rows: 3, sender: 1, state: GarbState::Yellow(stage) });
        m3.lock_piece(0); // 消 2 行（double，攻击值 1）→ 阻挡 1，剩 2，不发送
        let rows: u32 = m3.players[0].pending.iter().map(|g| g.rows).sum();
        assert_eq!(rows, 2, "攻击值 1 应阻挡 1 行");
        assert_eq!(m3.players[0].sent, 0, "阻挡后仍有暂缓垃圾，剩余攻击不放行");
        // 连击中断（任一次无消行锁定会把 combo 打回 -1，此处直接置位）后，红段走完即进场
        {
            let p = &mut m3.players[0];
            p.combo = -1;
            if let Some(g) = p.pending.first_mut() {
                g.state = GarbState::Red(1);
            }
        }
        run_quiet(&mut m3, 3);
        assert!(m3.players[0].pending.is_empty(), "连击结束后垃圾应进场");
    }
    #[test]
    fn garbage_pushes_airborne_piece_without_death() {
        // T99 语义：空中块被垃圾顶起不致死，夹回场顶重新下落
        let mut m = Match::new(MatchConfig { players: 2, seed: 9, ruleset: Ruleset::default() });
        {
            let p = &mut m.players[0];
            for r in 20..24 {
                for c in 0..W {
                    p.board[r * W + c] = CELL_GARBAGE;
                }
            }
            // 活动块悬在半空（全板第 3~4 行），被 8 行垃圾顶起会完全出板
            p.active = Some(Active { kind: Kind::O, rot: 0, x: 4, y: 3000 });
            p.pending.push(Garb { rows: 8, sender: 1, state: GarbState::Yellow(1200 * TMS) });
        }
        run_quiet(&mut m, 150);
        let p = &m.players[0];
        assert!(p.alive, "空中块被顶不应判负");
        let a = p.active.unwrap();
        let top = a
            .kind
            .cells(a.rot)
            .iter()
            .map(|&(_, dy)| (a.y + dy as Milli * MILLI_CELL).div_euclid(MILLI_CELL))
            .min()
            .unwrap();
        assert!(top >= 0, "夹回后不应高于场顶");
        assert!(p.pending.is_empty());
    }

    #[test]
    fn garbage_squeeze_kills_when_stack_reaches_top() {
        // 牌堆顶到缓冲区、夹回无处容身 = 挤压判负
        let mut m = Match::new(MatchConfig { players: 2, seed: 9, ruleset: Ruleset::default() });
        {
            let p = &mut m.players[0];
            for r in 2..24 {
                for c in 0..W {
                    p.board[r * W + c] = 1;
                }
            }
            p.active = Some(Active { kind: Kind::O, rot: 0, x: 4, y: 0 });
            p.pending.push(Garb { rows: 4, sender: 1, state: GarbState::Yellow(1200 * TMS) });
        }
        run_quiet(&mut m, 150);
        assert!(!m.players[0].alive, "堆到顶后垃圾进场应挤压判负");
    }
    #[test]
    fn lock_reset_counts() {
        let mut m = solo();
        // T 落底后左右点动各重置一次
        {
            let p = &mut m.players[0];
            p.active = Some(Active { kind: Kind::T, rot: 0, x: 4, y: 22000 });
        }
        m.players[0].grounded = true;
        assert!(m.try_shift(0, -1));
        assert_eq!(m.players[0].lock_resets, 1);
        assert!(m.try_shift(0, 1));
        assert_eq!(m.players[0].lock_resets, 2);
        assert_eq!(m.players[0].lock_timer, 0);
    }

    #[test]
    fn determinism_same_seed_same_hash() {
        let run = |seed: u64| -> (u64, Option<MatchResult>) {
            let mut m = Match::new(MatchConfig { players: 4, seed, ruleset: Ruleset::default() });
            // 每帧全部硬降 + 随帧目标切换
            for n in 0..8000u64 {
                let inputs: Vec<Vec<TimedKey>> = (0..4)
                    .map(|pid| {
                        vec![
                            TimedKey { t_us: 0, key: Key::HardDrop },
                            TimedKey { t_us: 100, key: if (n + pid) % 4 == 0 { Key::TargetRandom } else { Key::Noop } },
                        ]
                    })
                    .collect();
                m.frame(&inputs);
                if m.over.is_some() {
                    break;
                }
            }
            (m.hash_state(), m.over.clone())
        };
        let a = run(123);
        let b = run(123);
        let c = run(124);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1.map(|r| (r.frame, r.places)), b.1.map(|r| (r.frame, r.places)));
        assert_ne!(a.0, c.0);
    }

    #[test]
    fn solo_hard_drop_ends_in_top_out() {
        let mut m = solo();
        for _ in 0..2000 {
            m.frame(&hard_only(1));
            if m.over.is_some() {
                break;
            }
        }
        assert!(m.over.is_some());
        assert!(!m.players[0].alive);
    }
}
