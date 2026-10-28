//! 线协议（v2：压放式按键 + 引擎侧 DAS/ARR）与参与方适配器。
//!
//! 消息为单行 JSON（UTF-8，`\n` 结束）。与草稿 protocol.md 的差异：
//! - 键型由离散 tap 改为 press/release（left_down/left_up/...），DAS/ARR 由引擎统一执行；
//! - 事件偏移用整数微秒 `t_us`（0..=16666）而非毫秒小数；
//! - 快照新增 `das`（自身输入状态，供程序预测移动）。
//!
//! v6：frame 增 `targeted_by`（正在瞄准本方的存活 pid 列表，反攻决策）、
//! `time_secs_left`（对局倒计时）、`burn_x10`（倒计时燃烧倍率）。
//! v7：frame 增 `badges`（全员徽章数组；徽章随 KO 转移，围猎徽章领先者的依据）。
//! 读取方须容忍字段缺席（serde default）。

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use t99_core::{Key, Snapshot, TimedKey};

pub const PROTOCOL: u32 = 7;
pub const FRAME_DEADLINE: Duration = Duration::from_millis(16);
pub const READY_DEADLINE: Duration = Duration::from_millis(2000);
pub const MAX_CONSECUTIVE_MISS: u32 = 600;

#[derive(Serialize, Debug)]
pub struct Hello {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub protocol: u32,
    pub frame_hz: u32,
    pub player_id: usize,
    pub players: usize,
    pub board: BoardSpec,
    pub preview: usize,
    pub keys: Vec<&'static str>,
}

#[derive(Serialize, Debug)]
pub struct BoardSpec {
    pub w: usize,
    pub h: usize,
}

pub fn hello_for(pid: usize, players: usize, preview: usize) -> Hello {
    Hello {
        kind: "hello",
        protocol: PROTOCOL,
        frame_hz: 60,
        player_id: pid,
        players,
        board: BoardSpec { w: t99_core::W, h: t99_core::H },
        preview,
        keys: vec![
            "left_down", "left_up", "right_down", "right_up", "soft_down", "soft_up", "cw", "ccw",
            "flip", "hard", "hold", "target_badges", "target_kos", "target_attackers",
            "target_random", "target_player", "noop",
        ],
    }
}

#[derive(Serialize, Debug)]
pub struct FrameMsg {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub n: u64,
    pub player_id: usize,
    pub board: Vec<String>,
    pub active: Option<ActiveMsg>,
    /// 玩家相位：play / clear / are
    pub phase: PhaseMsg,
    pub next: Vec<char>,
    pub hold: Option<char>,
    pub hold_used: bool,
    pub combo: i64,
    pub b2b: bool,
    pub garbage: GarbageMsg,
    /// 消行延迟中闪烁的行（可见区行号）
    pub clearing: Vec<usize>,
    pub badges: u32,
    pub badge_units: u32,
    pub target: Option<usize>,
    pub target_mode: String,
    pub alive: Vec<usize>,
    pub gravity: i64,
    pub soft: bool,
    pub lock: LockMsg,
    pub das: DasMsg,
    pub ko: u32,
    pub lines: u32,
    pub sent: u32,
    pub pieces: u32,
    /// v6：正在瞄准本方的存活玩家 pid 列表（反攻决策依据）
    #[serde(default)]
    pub targeted_by: Vec<usize>,
    /// v7：全员徽章数组（index=pid，含已死者；徽章随 KO 转移）
    #[serde(default)]
    pub badges_all: Vec<u32>,
    /// v6：对局倒计时剩余秒
    #[serde(default)]
    pub time_secs_left: u32,
    /// v6：倒计时燃烧倍率 ×10（10 = 正常）
    #[serde(default)]
    pub burn_x10: u16,
}

#[derive(Serialize, Debug)]
pub struct ActiveMsg {
    pub t: char,
    pub r: u8,
    pub x: i32,
    /// 相对可见区顶部的毫格坐标（可为负）
    pub y_m: i64,
}

#[derive(Serialize, Debug)]
pub struct GarbageMsg {
    /// 红：即将进场
    pub red: u32,
    /// 黄：计时中
    pub yellow: u32,
    /// 灰：排队
    pub gray: u32,
}

#[derive(Serialize, Debug)]
pub struct PhaseMsg {
    pub kind: &'static str,
    pub ms_left: i64,
}

#[derive(Serialize, Debug)]
pub struct LockMsg {
    pub ms_left: i64,
    pub resets: u32,
}

#[derive(Serialize, Debug)]
pub struct DasMsg {
    pub left: bool,
    pub right: bool,
    pub dir: i64,
    pub das_ms: i64,
    pub arr_ms: i64,
}

fn board_rows(s: &Snapshot) -> Vec<String> {
    s.board
        .iter()
        .map(|row| {
            row.iter()
                .map(|&c| match c {
                    0 => '.',
                    t99_core::CELL_GARBAGE => 'G',
                    _ => t99_core::Kind::from_id(c).map(|k| k.ch()).unwrap_or('?'),
                })
                .collect()
        })
        .collect()
}

impl FrameMsg {
    pub fn from_snapshot(s: &Snapshot, pid: usize) -> Self {
        FrameMsg {
            kind: "frame",
            n: s.n,
            player_id: pid,
            board: board_rows(s),
            active: s.active.map(|a| ActiveMsg { t: a.kind.ch(), r: a.rot, x: a.x, y_m: a.y_milli }),
            phase: PhaseMsg { kind: s.phase.tag(), ms_left: s.phase.ms_left() },
            next: s.next.iter().map(|k| k.ch()).collect(),
            hold: s.hold.map(|k| k.ch()),
            hold_used: s.hold_used,
            combo: s.combo as i64,
            b2b: s.b2b,
            garbage: GarbageMsg { red: s.garbage_red, yellow: s.garbage_yellow, gray: s.garbage_gray },
            clearing: s.clearing.clone(),
            badges: s.badges,
            badge_units: s.badge_units,
            target: s.target,
            target_mode: s.target_mode.name().to_string(),
            alive: s.alive.clone(),
            gravity: s.gravity_milli,
            soft: s.soft,
            lock: LockMsg { ms_left: s.lock_ms_left, resets: s.lock_resets },
            das: DasMsg {
                left: s.das.left_held,
                right: s.das.right_held,
                dir: s.das.active_dir.map(|d| d as i64).unwrap_or(0),
                das_ms: s.das.das_ms,
                arr_ms: s.das.arr_ms,
            },
            ko: s.ko,
            lines: s.lines,
            sent: s.sent,
            pieces: s.pieces,
            targeted_by: s.targeted_by.clone(),
            badges_all: s.badges_all.clone(),
            time_secs_left: s.match_secs_left,
            burn_x10: s.burn_x10,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct InputMsg {
    #[serde(rename = "type")]
    pub kind: String,
    pub n: u64,
    #[serde(default)]
    pub events: Vec<EventIn>,
}

#[derive(Deserialize, Debug)]
pub struct EventIn {
    pub t_us: u32,
    pub k: String,
    /// 载荷（仅 target_player 使用）
    #[serde(default)]
    pub v: Option<u16>,
}

impl InputMsg {
    pub fn to_timed(&self) -> Option<Vec<TimedKey>> {
        if self.kind != "input" {
            return None;
        }
        let mut out = Vec::with_capacity(self.events.len());
        for e in &self.events {
            let key = if e.k == "target_player" {
                Key::TargetPlayer(e.v?)
            } else {
                Key::from_name(&e.k)?
            };
            out.push(TimedKey { t_us: e.t_us, key });
        }
        Some(out)
    }
}

#[derive(Deserialize, Debug)]
pub struct ReadyMsg {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Serialize, Debug)]
pub struct ResultMsg {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub n: u64,
    pub place: Option<u32>,
    pub players: usize,
    pub ko: u32,
    pub lines: u32,
    pub sent: u32,
    pub pieces: u32,
    pub reason: String,
    pub alive: bool,
}

#[derive(Debug)]
pub enum AdapterError {
    Timeout,
    Dead,
    Protocol(String),
}

/// 席位程序描述：内建 bot 档位，或外部可执行程序（与参赛模型提交同通道）。
/// 语法：`tier` 或 `ext:label=path`（path 为可执行文件）。
#[derive(Clone, Debug)]
pub enum SeatProgram {
    BuiltIn(String),
    Ext { label: String, path: String },
}

pub fn parse_seat(s: &str) -> SeatProgram {
    if let Some(rest) = s.strip_prefix("ext:") {
        let (label, path) = rest.split_once('=').unwrap_or((rest, rest));
        SeatProgram::Ext { label: label.to_string(), path: path.to_string() }
    } else {
        SeatProgram::BuiltIn(s.to_string())
    }
}

/// 参赛方统一接口：子进程程序与进程内 Bot 都实现它。
pub trait PlayerAdapter {
    fn name(&self) -> String;
    fn setup(&mut self, hello: &Hello) -> Result<(), AdapterError>;
    /// 返回该帧原始事件（引擎内部会再 sanitize）
    fn act(&mut self, snap: &Snapshot) -> Vec<TimedKey>;
    fn consecutive_misses(&self) -> u32;
    fn is_dead(&self) -> bool;
    fn finish(&mut self, result: &ResultMsg);
}

/// 子进程参与方：stdin/stdout 单行 JSON。
pub struct SubprocessAdapter {
    pub label: String,
    cmd: Vec<String>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Option<Receiver<String>>,
    misses: u32,
    dead: bool,
    last_n: u64,
}

impl SubprocessAdapter {
    pub fn new(label: impl Into<String>, cmd: Vec<String>) -> Self {
        SubprocessAdapter {
            label: label.into(),
            cmd,
            child: None,
            stdin: None,
            rx: None,
            misses: 0,
            dead: false,
            last_n: 0,
        }
    }

    fn spawn(&mut self) -> Result<(), AdapterError> {
        if self.cmd.is_empty() {
            return Err(AdapterError::Protocol("empty command".into()));
        }
        let mut child = Command::new(&self.cmd[0])
            .args(&self.cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| AdapterError::Protocol(format!("spawn {}: {}", self.cmd[0], e)))?;
        let stdin = child.stdin.take().ok_or_else(|| AdapterError::Protocol("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| AdapterError::Protocol("no stdout".into()))?;
        let rx = {
            let (tx, rx) = mpsc::channel::<String>();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(l) => {
                            if tx.send(l).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
            rx
        };
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.rx = Some(rx);
        Ok(())
    }

    fn send(&mut self, s: &str) -> bool {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin.write_all(s.as_bytes()).is_ok() && stdin.write_all(b"\n").is_ok() && stdin.flush().is_ok()
        } else {
            false
        }
    }

    fn recv(&self, timeout: Duration) -> Option<String> {
        match self.rx.as_ref()?.recv_timeout(timeout) {
            Ok(line) => Some(line),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(String::new()), // 管道关闭 → 空行标记死亡
        }
    }

    fn check_exit(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            matches!(child.try_wait(), Ok(Some(_)))
        } else {
            true
        }
    }
}

impl PlayerAdapter for SubprocessAdapter {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn setup(&mut self, hello: &Hello) -> Result<(), AdapterError> {
        self.spawn()?;
        let msg = serde_json::to_string(hello).map_err(|e| AdapterError::Protocol(e.to_string()))?;
        if !self.send(&msg) {
            self.dead = true;
            return Err(AdapterError::Dead);
        }
        match self.recv(READY_DEADLINE) {
            Some(line) => {
                if line.is_empty() {
                    self.dead = true;
                    return Err(AdapterError::Dead);
                }
                match serde_json::from_str::<ReadyMsg>(&line) {
                    Ok(r) if r.kind == "ready" => Ok(()),
                    _ => Err(AdapterError::Protocol("bad ready".into())),
                }
            }
            None => {
                self.dead = true;
                Err(AdapterError::Timeout)
            }
        }
    }

    fn act(&mut self, snap: &Snapshot) -> Vec<TimedKey> {
        if self.dead || self.child.is_none() {
            self.misses += 1;
            return vec![];
        }
        let frame = FrameMsg::from_snapshot(snap, 0);
        self.last_n = frame.n;
        let msg = serde_json::to_string(&frame).unwrap_or_default();
        if !self.send(&msg) {
            self.dead = true;
            self.misses += 1;
            return vec![];
        }
        // 在帧预算内读到「当前帧」的应答为止：调度抖动导致的迟到应答（上帧超时后才到）
        // 不再造成永久错位——读到的旧帧应答直接丢弃，继续等当前帧的。
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break;
            }
            match self.recv(remain) {
                Some(line) => {
                    if line.is_empty() {
                        self.dead = true;
                        self.misses += 1;
                        return vec![];
                    }
                    match serde_json::from_str::<InputMsg>(&line) {
                        Ok(m) if m.n == self.last_n => match m.to_timed() {
                            Some(events) => {
                                self.misses = 0;
                                return events;
                            }
                            None => {
                                self.misses += 1;
                                return vec![];
                            }
                        },
                        // 过期应答（n 落后）：丢弃，继续读
                        Ok(_) => continue,
                        Err(_) => {
                            self.misses += 1;
                            return vec![];
                        }
                    }
                }
                None => break,
            }
        }
        if self.check_exit() {
            self.dead = true;
        }
        self.misses += 1;
        vec![]
    }

    fn consecutive_misses(&self) -> u32 {
        self.misses
    }

    fn is_dead(&self) -> bool {
        self.dead
    }

    fn finish(&mut self, result: &ResultMsg) {
        if let Ok(msg) = serde_json::to_string(result) {
            self.send(&msg);
        }
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        self.child = None;
    }
}

/// 网络玩家适配器（lobby 模式）：外部程序/提交程序经 WebSocket 以玩家身份接入，
/// 与子进程适配器同 PlayerAdapter 语义（hello/ready → frame/input → result）。
pub struct NetPlayerAdapter {
    pub label: String,
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<String>,
    misses: u32,
    dead: bool,
    last_n: u64,
}

impl NetPlayerAdapter {
    pub fn new(label: impl Into<String>, tx: mpsc::Sender<String>, rx: mpsc::Receiver<String>) -> Self {
        NetPlayerAdapter { label: label.into(), tx, rx, misses: 0, dead: false, last_n: 0 }
    }

    fn send(&self, s: &str) -> bool {
        self.tx.send(s.to_string()).is_ok()
    }

    /// 读一行；None = 超时，Some("") = 断开
    fn recv(&self, timeout: Duration) -> Option<String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Some(line),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(String::new()),
        }
    }
}

impl PlayerAdapter for NetPlayerAdapter {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn setup(&mut self, hello: &Hello) -> Result<(), AdapterError> {
        let msg = serde_json::to_string(hello).map_err(|e| AdapterError::Protocol(e.to_string()))?;
        if !self.send(&msg) {
            self.dead = true;
            return Err(AdapterError::Dead);
        }
        match self.recv(READY_DEADLINE) {
            Some(line) => {
                if line.is_empty() {
                    self.dead = true;
                    return Err(AdapterError::Dead);
                }
                match serde_json::from_str::<ReadyMsg>(&line) {
                    Ok(r) if r.kind == "ready" => Ok(()),
                    _ => Err(AdapterError::Protocol("bad ready".into())),
                }
            }
            None => {
                self.dead = true;
                Err(AdapterError::Timeout)
            }
        }
    }

    fn act(&mut self, snap: &Snapshot) -> Vec<TimedKey> {
        if self.dead {
            self.misses += 1;
            return vec![];
        }
        let frame = FrameMsg::from_snapshot(snap, 0);
        self.last_n = frame.n;
        let msg = serde_json::to_string(&frame).unwrap_or_default();
        let dbg = std::env::var("T99_NET_DEBUG").is_ok();
        if dbg && self.last_n < 5 {
            eprintln!("[net:{}->] frame n={}", self.label, self.last_n);
        }
        if !self.send(&msg) {
            self.dead = true;
            self.misses += 1;
            return vec![];
        }
        // 帧预算内读到当前帧应答为止（过期应答丢弃，防永久错位）
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break;
            }
            match self.recv(remain) {
                Some(line) => {
                    if line.is_empty() {
                        self.dead = true;
                        self.misses += 1;
                        return vec![];
                    }
                    if dbg && self.last_n < 5 {
                        eprintln!("[net:{}<-] {}", self.label, &line[..line.len().min(120)]);
                    }
                    match serde_json::from_str::<InputMsg>(&line) {
                        Ok(m) if m.n == self.last_n => match m.to_timed() {
                            Some(events) => {
                                self.misses = 0;
                                if dbg && self.last_n < 5 {
                                    eprintln!("[net:{}] 应用 {} 事件", self.label, events.len());
                                }
                                return events;
                            }
                            None => {
                                self.misses += 1;
                                return vec![];
                            }
                        },
                        Ok(_) => continue,
                        Err(_) => {
                            self.misses += 1;
                            if dbg && self.last_n < 5 {
                                eprintln!("[net:{}] 解析失败", self.label);
                            }
                            return vec![];
                        }
                    }
                }
                None => break,
            }
        }
        self.misses += 1;
        vec![]
    }

    fn consecutive_misses(&self) -> u32 {
        self.misses
    }

    fn is_dead(&self) -> bool {
        self.dead
    }

    fn finish(&mut self, result: &ResultMsg) {
        if let Ok(msg) = serde_json::to_string(result) {
            self.send(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_msg_roundtrip() {
        let raw = r#"{"type":"input","n":42,"events":[{"t_us":0,"k":"left_down"},{"t_us":900,"k":"left_up"},{"t_us":5000,"k":"hard"}]}"#;
        let m: InputMsg = serde_json::from_str(raw).unwrap();
        assert_eq!(m.n, 42);
        let ev = m.to_timed().unwrap();
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[2].key, Key::HardDrop);
        let bad = r#"{"type":"input","n":1,"events":[{"t_us":0,"k":"warp"}]}"#;
        let m: InputMsg = serde_json::from_str(bad).unwrap();
        assert!(m.to_timed().is_none());
    }

    #[test]
    fn hello_json_shape() {
        let h = hello_for(3, 4, 6);
        let s = serde_json::to_string(&h).unwrap();
        assert!(s.contains(r#""type":"hello""#));
        assert!(s.contains(r#""player_id":3"#));
    }
}
