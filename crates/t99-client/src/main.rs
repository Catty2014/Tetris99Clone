//! t99 人类客户端（T99 版 UI）：内嵌引擎，Bevy 只做表现与输入。
//!
//! 输入（与程序/Bot 同物理，客户端侧 DAS/ARR；采样打点见 poll_input）：
//!   A/D 移动（按住连发）  S 软降  空格 硬降  X/J 顺时针  Z/K 逆时针  C/Shift Hold
//!   Tab 循环目标模式；方向键 = 副操作：导航对手小窗并手动锁定目标
//!
//! UI 对齐 T99：左右缩放小窗（框色=危险度，死亡=灰框+冻结棋盘+名次），
//! 中央棋盘 + 左侧垃圾条 + 右侧 Next6 + 下方菱形目标指示。

use bevy::prelude::*;
use bevy::text::{Font, TextLayoutInfo, TextSpan};
use bevy::window::PresentMode;
use std::collections::VecDeque;
use std::time::Duration;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use t99_bots::BotAdapter;
use t99_core::engine::{ActiveView, AttackEvent};
use t99_core::{Cell, Key, Match, MatchConfig, PlayerPhase, Ruleset, Snapshot, TargetMode, TimedKey, TMS, W, H};
use t99_proto::{PlayerAdapter, PROTOCOL};

const CELL: f32 = 26.0;
const MINI: f32 = 4.0;
const FRAME_US: u64 = 16_666;
const DEFAULT_TICK_HZ: u32 = 60;
/// ARR=0 时一次补发的最大格数（棋盘宽 10）。
const MAX_REPEAT_PER_FRAME: usize = 10;
const THUMB_HZ: f64 = 10.0;
const TW: f32 = 44.0; // 小窗宽（含边框）
const TH: f32 = 88.0;
/// 侧栏网格定位：网格外缘恒收在窗口内（半宽 800 留 12px 边距），且不贴死棋盘框
fn grid_half_for(n: usize) -> f32 {
    n as f32 * (TW + 6.0) / 2.0
}

fn side_center_x(n: usize, half_w: f32) -> f32 {
    let grid_half = grid_half_for(n);
    // 内缘贴主棋盘：右侧预览列右缘 227 + 16px。窗口放不下时向内收缩（保持 12px 外边距）。
    let ideal = 243.0 + grid_half;
    ideal.min((half_w - 12.0 - grid_half).max(148.0 + grid_half))
}

fn kind_color(id: Cell) -> Color {
    match id {
        1 => Color::srgb(0.20, 0.85, 0.95),
        2 => Color::srgb(0.95, 0.85, 0.20),
        3 => Color::srgb(0.75, 0.35, 0.90),
        4 => Color::srgb(0.30, 0.90, 0.40),
        5 => Color::srgb(0.95, 0.35, 0.35),
        6 => Color::srgb(0.35, 0.50, 0.95),
        7 => Color::srgb(0.95, 0.60, 0.25),
        8 => Color::srgb(0.55, 0.55, 0.58),
        // 红闪暗相位：仍是方块，只是转暗
        9 => Color::srgb(0.42, 0.10, 0.10),
        _ => Color::BLACK,
    }
}

fn danger_color(h: usize, dead: bool) -> Color {
    if dead {
        return Color::srgb(0.35, 0.35, 0.38);
    }
    match h {
        // 低危险度底色压暗一档：提亮方块本身的可读性（底色越亮，方块越糊）
        0..=7 => Color::srgb(0.66, 0.70, 0.78),
        8..=11 => Color::srgb(0.72, 0.62, 0.18),
        12..=14 => Color::srgb(0.92, 0.5, 0.12),
        _ => Color::srgb(0.92, 0.18, 0.18),
    }
}

fn stack_height(s: &Snapshot) -> usize {
    for r in 0..H {
        if s.board[r].iter().any(|&c| c != 0) {
            return H - r;
        }
    }
    0
}

/// 开局最小可容网格（2N² ≥ 存活对手数），2..=7 —— 8 人局开局为 2×2
fn fit_grid(alive_others: usize) -> usize {
    let mut n = 2usize;
    while n < 7 && 2 * n * n < alive_others {
        n += 1;
    }
    n
}

/// 网络指令（Bevy → net 线程）
pub enum NetCmd {
    /// n = 服务器私有 frame 的帧号；服务器按同帧号匹配 input
    Input { n: u64, evs: Vec<TimedKey> },
    Restart,
}

/// 网络镜像：本地确定性重演权威事件流；座位以服务器 hello 为准
#[derive(Default)]
struct MirrorState {
    engine: Option<Match>,
    my_seat: usize,
    /// 服务器逻辑 tick 速率；DAS/ARR tick 转 time 与 FixedUpdate 都依赖它
    tick_hz: u32,
    /// 最近一次私有 frame 的帧号；用于给 input 填 n
    pending_frame_n: u64,
    /// 已回复过的 frame 帧号，避免同一帧重复发 input
    last_input_n: u64,
}

/// 连接诊断（调试 HUD 用）：对局 ID、RTT、丢包率、连接状态
#[derive(Resource, Default, Clone)]
struct NetStats {
    player_id: String,
    match_id: u64,
    rtt_ms: u32,
    loss_pct: u32,
    connected: bool,
    /// 服务器 tick 速率（EMA，tick/s）
    server_tps: f32,
    /// 内部：上一 tick 的序号与到达时刻
    last_tick_n: u64,
    last_tick_at: Option<Instant>,
    /// 内部：已发 ping 数 / 收到 pong 数
    pings: u32,
    pongs: u32,
}

#[derive(Resource)]
struct HudMetrics {
    /// 当前刷新窗口内的最小 FPS
    fps_min: f32,
    fps_frames: u32,
    /// 已刷新的 FPS
    shown_fps: f32,
    /// 当前刷新窗口内的最小 TPS
    tps_min: f32,
    tps_ticks: u32,
    shown_tps: f32,
    last_tick_n: u64,
}

impl Default for HudMetrics {
    fn default() -> Self {
        Self {
            fps_min: f32::MAX,
            fps_frames: 0,
            shown_fps: 0.0,
            tps_min: f32::MAX,
            tps_ticks: 0,
            shown_tps: 0.0,
            last_tick_n: 0,
        }
    }
}

/// HUD 指标刷新率（M）：每 K 帧刷新一次，K = floor(R / M)
const DISPLAY_HZ: f32 = 5.0;
const HUD_DEFAULT: Color = Color::srgb(0.65, 0.70, 0.80);
const HUD_YELLOW: Color = Color::srgb(0.95, 0.85, 0.20);
const HUD_ORANGE: Color = Color::srgb(0.95, 0.60, 0.20);
const HUD_RED: Color = Color::srgb(0.95, 0.35, 0.35);

fn lat_color(ms: u32) -> Color {
    if ms > 33 {
        HUD_RED
    } else if ms > 16 {
        HUD_YELLOW
    } else {
        HUD_DEFAULT
    }
}

fn loss_color(pct: u32) -> Color {
    if pct > 1 {
        HUD_RED
    } else if pct > 0 {
        HUD_YELLOW
    } else {
        HUD_DEFAULT
    }
}

fn tps_color(tps: f32) -> Color {
    if tps < 40.0 {
        HUD_RED
    } else if tps < 50.0 {
        HUD_ORANGE
    } else if tps < 55.0 {
        HUD_YELLOW
    } else {
        HUD_DEFAULT
    }
}

fn fps_color(fps: f32) -> Color {
    if fps < 30.0 {
        HUD_RED
    } else if fps < 50.0 {
        HUD_ORANGE
    } else if fps < 60.0 {
        HUD_YELLOW
    } else {
        HUD_DEFAULT
    }
}

#[derive(Resource)]
enum Session {
    Local { engine: Match, bots: Vec<Option<BotAdapter>> },
    Net {
        mirror: std::sync::Arc<std::sync::Mutex<MirrorState>>,
        tx: std::sync::mpsc::Sender<NetCmd>,
        observer: bool,
        status: std::sync::Arc<std::sync::Mutex<String>>,
        stats: std::sync::Arc<std::sync::Mutex<NetStats>>,
    },
}

impl Session {
    fn with_engine<R>(&self, f: impl FnOnce(&Match, usize) -> R) -> Option<R> {
        match self {
            Session::Local { engine, .. } => Some(f(engine, 0)),
            Session::Net { mirror, .. } => {
                let g = mirror.lock().unwrap();
                g.engine.as_ref().map(|e| f(e, g.my_seat))
            }
        }
    }
}

#[derive(Resource, Default)]
struct InputQueue {
    entries: VecDeque<(Instant, Key)>,
    /// 方向键导航游标（shown 列表索引）
    cursor: Option<usize>,
    target_idx: usize,
}

#[derive(Default, Clone, Copy)]
struct DirState {
    held: bool,
    charged: bool,
    next: Option<Instant>,
}

/// 有头客户端的 DAS/ARR 状态：引擎不再按住自动重复。
/// 每方向独立保存 charge；active 决定当前发哪个方向的脉冲。
#[derive(Resource, Default)]
struct RepeatState {
    left: DirState,
    right: DirState,
    active: i8,
    dual_charge: bool,
}

#[derive(Resource, Clone, Copy)]
struct InputTiming {
    das_ticks: u32,
    arr_ticks: u32,
    dual_charge: bool,
}

#[allow(dead_code)]
#[derive(Resource, Clone, Copy)]
struct DisplaySettings {
    ghost: bool,
    shadow_locked: bool,
    opponent_framerate: u32,
    result_hold_secs: u64,
}

#[derive(Resource)]
struct Views {
    cells: Vec<Entity>,
    active: Vec<Entity>,
    ghost: Vec<Entity>,
    ghost_enabled: bool,
    hold: Vec<Entity>,
    next: Vec<Entity>,
    meter: Vec<Entity>,
    hud: Vec<Entity>,
    diamond: [Entity; 4],
    diamond_center: Entity,
    judge: Entity,
    /// 判定种类文字后的白色最终攻击力（Text2d 根 + 子 span 着色）
    judge_num: Entity,
    judge_sub: Entity,
    /// COMBO 行的白色连击数（子 span）
    judge_sub_num: Entity,
    /// 中央棋盘死亡蒙版（与缩略图同款灰蒙，只盖棋盘本体）
    death_mask: Entity,
    death_rank: Entity,
    /// 调试 HUD：左上一行（玩家/对局码），左下一行（连接状态行）。bg = 深色背衬条（文字底）
    dbg_top: Entity,
    dbg_top_bg: Entity,
    dbg_bot: Entity,
    /// 左下行拆段：状态 + Lat/Loss/TPS/FPS 各自独立着色
    dbg_bot_status: Entity,
    dbg_lat: Entity,
    dbg_loss: Entity,
    dbg_tps: Entity,
    dbg_fps: Entity,
    dbg_bot_bg: Entity,
    /// Next6 下方信息面板：徽章/击杀/档位/攻击加成/存活（逐帧 set_text）
    info: [Entity; 5],
    /// 被瞄警示框：与棋盘框同几何的红色描边，targeted_by 非空时亮起
    board_alert: Vec<Entity>,
}

struct ThumbView {
    pid: usize,
    origin: Vec2,
    /// 背景填充（危险度/死亡灰）
    bg: Entity,
    /// 边缘线（瞄准关系：淡蓝/红/紫）
    edge: Entity,
    mask: Entity,
    cells: Vec<Entity>,
    active: Vec<Entity>,
    meter: Vec<Entity>,
    badge: Entity,
    id: Entity,
    rank: Entity,
}

/// 攻击光点动画状态：来自最近一个引擎 tick 的 recent_attacks。
/// 槽位与光点池一一对应，None = 空闲；无事件时全部隐藏，每帧只复用实体。
struct DotFx {
    from: Vec2,
    to: Vec2,
    born: Instant,
    rows: u32,
    /// 颜色随方向（来袭红 / 自发金 / 旁观灰），透明度勿再推算
    color: Color,
    /// 基础亮度：旁观事件明显更暗（~0.15）
    base: f32,
}

#[derive(Resource, Default)]
struct Thumbs {
    n: usize,
    views: Vec<ThumbView>,
    all_ents: Vec<Entity>,
    last_refresh: Option<Instant>,
    /// 上一帧的局内帧号（倒退 = 新局，网格等级重置）
    last_frame: u64,
    /// 攻击连线池（被瞄黄/瞄准蓝）：z=6 缩略图之上、调试 HUD 之下
    atk_lines: Vec<Entity>,
    /// 攻击光点池：z=5，每 tick 的 recent_attacks 生成 0.3s 飞行淡出光点
    atk_dots: Vec<Entity>,
    /// 光点动画槽位（与 atk_dots 一一对应，None = 空闲）
    slots: Vec<Option<DotFx>>,
    /// 已消费的引擎 tick 号：同一 tick 在多个渲染帧间只采一次事件
    last_tick: u64,
}

#[derive(Resource)]
struct PrevPiece {
    pieces: u32,
    y_milli: i64,
}

// ———— 布局辅助 ————

fn board_pos(col: i64, row: i64) -> Vec2 {
    Vec2::new(
        (col as f32 - (W as f32 - 1.0) / 2.0) * CELL,
        ((H as f32 - 1.0) / 2.0 - row as f32) * CELL,
    )
}

fn spawn_grid(cmd: &mut Commands, origin: Vec2, size: f32, cols: usize, rows: usize, z: f32) -> Vec<Entity> {
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let x = origin.x + (c as f32 - (cols as f32 - 1.0) / 2.0) * size;
            let y = origin.y - (r as f32 - (rows as f32 - 1.0) / 2.0) * size;
            out.push(
                cmd.spawn((
                    Sprite::from_color(Color::WHITE, Vec2::splat(size - 1.0)),
                    Transform::from_xyz(x, y, z),
                    Visibility::Hidden,
                ))
                .id(),
            );
        }
    }
    out
}

fn text(cmd: &mut Commands, font: &Handle<Font>, s: &str, size: f32, color: Color, pos: Vec2) -> Entity {
    cmd.spawn((
        Text2d::new(s),
        TextFont { font: font.clone(), font_size: size, ..default() },
        TextColor(color),
        Transform::from_xyz(pos.x, pos.y, 3.0),
    ))
    .id()
}

/// 区域描边框（4 条 2px 边）
fn frame_rect(cmd: &mut Commands, center: Vec2, w: f32, h: f32, color: Color, z: f32) {
    let t = 2.0;
    let segs = [
        (Vec2::new(center.x, center.y + h / 2.0 - t / 2.0), Vec2::new(w, t)),
        (Vec2::new(center.x, center.y - h / 2.0 + t / 2.0), Vec2::new(w, t)),
        (Vec2::new(center.x - w / 2.0 + t / 2.0, center.y), Vec2::new(t, h)),
        (Vec2::new(center.x + w / 2.0 - t / 2.0, center.y), Vec2::new(t, h)),
    ];
    for (pos, size) in segs {
        cmd.spawn((Sprite::from_color(color, size), Transform::from_xyz(pos.x, pos.y, z)));
    }
}

/// 内嵌 CJK 字体：不依赖 assets 目录或运行工作目录。
#[derive(Resource)]
struct UiFont(Handle<Font>);

fn load_ui_font(mut cmd: Commands, mut fonts: ResMut<Assets<Font>>) {
    let font = Font::try_from_bytes(include_bytes!("../assets/fonts/CJK.otf").to_vec())
        .expect("内置 CJK 字体损坏");
    cmd.insert_resource(UiFont(fonts.add(font)));
}

fn setup(mut cmd: Commands, ui_font: Res<UiFont>, display: Res<DisplaySettings>) {
    let font: Handle<Font> = ui_font.0.clone();
    cmd.spawn(Camera2d);
    cmd.spawn((
        Sprite::from_color(Color::srgb(0.06, 0.07, 0.10), Vec2::new(W as f32 * CELL + 8.0, H as f32 * CELL + 8.0)),
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));
    let cells = spawn_grid(&mut cmd, Vec2::ZERO, CELL, W, H, 0.0);
    let active = spawn_grid(&mut cmd, Vec2::ZERO, CELL, 1, 4, 2.0);
    let ghost = spawn_grid(&mut cmd, Vec2::ZERO, CELL, 1, 4, 1.0);
    // 保存（Hold）框：整体右移，右缘与下方垃圾列(-174..-144)对齐组成左侧紧凑列；
    // 距棋盘框（-142..-140）4px，不与任何框重叠。棋盘/Next/HUD 不动。
    let hold_x = -196.0;
    cmd.spawn((
        Sprite::from_color(Color::srgb(0.08, 0.09, 0.13), Vec2::new(4.0 * 20.0 + 8.0, 2.0 * 20.0 + 8.0)),
        Transform::from_xyz(hold_x, 240.0, -0.5),
    ));
    let hold = spawn_grid(&mut cmd, Vec2::new(hold_x, 240.0), 20.0, 4, 2, 0.0);
    let next = spawn_grid(&mut cmd, Vec2::new(190.0, 119.0), 16.0, 4, 18, 0.0);
    // 垃圾条单列堆叠：红(即将进场)在下，黄(计时)其上，灰(排队)顶层。
    // 预铺 72 格：sync 按 garbage_batches 分组摆位（批间 3 倍行距），超高时压缩行距。
    let meter = spawn_grid(&mut cmd, Vec2::new(-159.0, -4.0), 20.0, 1, 72, 0.0);

    // 分区框与标题（界定区域 + 辅助文字）。
    // 对齐：保存/对战盘/预览上沿取齐（+270）；垃圾下沿与对战盘下沿取齐（-270）；
    // 攻击盘位于对战盘下方，留 16px 空隙。保存框右缘距棋盘框 4px。
    let fc = Color::srgb(0.30, 0.34, 0.42);
    let lc = Color::srgb(0.45, 0.50, 0.60);
    // 分区底衬：Next 列 / 垃圾列 / 攻击盘 / 信息面板统一加一层略深的背衬，
    // 与棋盘/保存的深底一致，分区和 99 缩略图拉开明暗层次。
    let panel_c = Color::srgb(0.07, 0.08, 0.12);
    frame_rect(&mut cmd, Vec2::new(0.0, 0.0), W as f32 * CELL + 20.0, H as f32 * CELL + 20.0, fc, -0.7);
    frame_rect(&mut cmd, Vec2::new(hold_x, 240.0), 96.0, 60.0, fc, -0.7);
    frame_rect(&mut cmd, Vec2::new(190.0, 119.0), 74.0, 302.0, fc, -0.7);
    frame_rect(&mut cmd, Vec2::new(-159.0, -4.0), 30.0, H as f32 * 20.0 + 12.0, fc, -0.7);
    frame_rect(&mut cmd, Vec2::new(0.0, -344.0), 176.0, 116.0, fc, -0.7);
    for (pos, size) in [
        (Vec2::new(190.0, 119.0), Vec2::new(70.0, 298.0)),
        (Vec2::new(-159.0, -4.0), Vec2::new(26.0, H as f32 * 20.0 + 8.0)),
        (Vec2::new(0.0, -344.0), Vec2::new(172.0, 112.0)),
    ] {
        cmd.spawn((Sprite::from_color(panel_c, size), Transform::from_xyz(pos.x, pos.y, -0.6)));
    }
    // 被瞄警示框：与棋盘框同几何，红色描边（z= -0.65 压在灰框 -0.7 之上）；
    // 平时隐藏，targeted_by 非空时亮起并慢脉冲。
    let bw = W as f32 * CELL + 20.0;
    let bh = H as f32 * CELL + 20.0;
    let mut board_alert = Vec::new();
    for (pos, size) in [
        (Vec2::new(0.0, bh / 2.0 - 1.0), Vec2::new(bw, 2.0)),
        (Vec2::new(0.0, -bh / 2.0 + 1.0), Vec2::new(bw, 2.0)),
        (Vec2::new(-bw / 2.0 + 1.0, 0.0), Vec2::new(2.0, bh)),
        (Vec2::new(bw / 2.0 - 1.0, 0.0), Vec2::new(2.0, bh)),
    ] {
        board_alert.push(
            cmd.spawn((
                Sprite::from_color(Color::srgba(0.95, 0.30, 0.30, 0.0), size),
                Transform::from_xyz(pos.x, pos.y, -0.65),
                Visibility::Hidden,
            ))
            .id(),
        );
    }
    text(&mut cmd, &font, "对战盘", 13.0, lc, Vec2::new(-95.0, 283.0));
    text(&mut cmd, &font, "保存", 13.0, lc, Vec2::new(hold_x, 282.0));
    text(&mut cmd, &font, "预览", 13.0, lc, Vec2::new(190.0, 282.0));
    text(&mut cmd, &font, "垃圾", 13.0, lc, Vec2::new(-159.0, -282.0));
    text(&mut cmd, &font, "攻击", 13.0, lc, Vec2::new(0.0, -414.0));
    // —— Next6 下方信息面板（x≈190，y -48…-172）：徽章/击杀/档位/攻击加成/存活。
    // 74px 宽与 Next 列框同宽对齐；上距 Next 框 16px；距 99p 右侧网格内缘(243) ≥16px；
    // 距窗口右下调试 HUD（y < -400）远，不重叠。
    frame_rect(&mut cmd, Vec2::new(190.0, -110.0), 74.0, 124.0, fc, -0.7);
    cmd.spawn((
        Sprite::from_color(panel_c, Vec2::new(70.0, 120.0)),
        Transform::from_xyz(190.0, -110.0, -0.6),
    ));
    text(&mut cmd, &font, "状态", 13.0, lc, Vec2::new(190.0, -70.0));
    let info = [
        text(&mut cmd, &font, "", 12.0, Color::srgb(0.85, 0.88, 0.95), Vec2::new(190.0, -92.0)),
        text(&mut cmd, &font, "", 12.0, Color::srgb(0.85, 0.88, 0.95), Vec2::new(190.0, -110.0)),
        text(&mut cmd, &font, "", 12.0, Color::srgb(0.85, 0.88, 0.95), Vec2::new(190.0, -128.0)),
        text(&mut cmd, &font, "", 12.0, Color::srgb(0.85, 0.88, 0.95), Vec2::new(190.0, -146.0)),
        text(&mut cmd, &font, "", 12.0, Color::srgb(0.85, 0.88, 0.95), Vec2::new(190.0, -164.0)),
    ];

    let mut hud = Vec::new();
    let stats: [(&str, f32); 5] = [
        ("存活", 300.0),
        ("徽章", 270.0),
        ("消行", 240.0),
        ("目标", 210.0),
        ("相位", 180.0),
    ];
    for (s, y) in stats {
        let e = text(&mut cmd, &font, s, 16.0, Color::srgb(0.8, 0.83, 0.9), Vec2::new(345.0, y));
        cmd.entity(e).insert(bevy::sprite::Anchor::CenterRight);
        hud.push(e);
    }
    hud.push({
        let e = text(&mut cmd, &font, "", 30.0, Color::srgb(0.95, 0.8, 0.3), Vec2::new(345.0, 340.0));
        cmd.entity(e).insert(bevy::sprite::Anchor::CenterRight);
        e
    });
    hud.push({
        // 对局倒计时：主棋盘正上方水平居中（sync 逐帧摆位），默认 Center 锚
        let e = text(&mut cmd, &font, "", 24.0, Color::srgb(0.65, 0.7, 0.8), Vec2::new(0.0, 342.0));
        e
    });
    hud.push({
        let e = text(&mut cmd, &font, "", 13.0, Color::srgb(0.5, 0.55, 0.62), Vec2::new(345.0, 150.0));
        cmd.entity(e).insert(bevy::sprite::Anchor::BottomRight);
        e
    });
    hud.push(text(
        &mut cmd,
        &font,
        "A/D 移动  S 软降  空格 硬降  X/Z 旋转  C Hold  Tab 模式  方向键选敌",
        15.0,
        Color::srgb(0.5, 0.55, 0.62),
        Vec2::new(0.0, 428.0),
    ));
    // 菱形目标指示：上随机 下反攻 左击倒 右徽章（位于攻击盘中心）
    let dd = 36.0;
    let dm = Color::srgb(0.45, 0.5, 0.58);
    let ds = Color::srgb(0.98, 0.8, 0.25);
    let diamond = [
        text(&mut cmd, &font, "随机", 16.0, dm, Vec2::new(0.0, -344.0 - dd)),
        text(&mut cmd, &font, "反攻", 16.0, dm, Vec2::new(0.0, -344.0 + dd)),
        text(&mut cmd, &font, "击倒", 16.0, dm, Vec2::new(-dd, -344.0)),
        text(&mut cmd, &font, "徽章", 16.0, dm, Vec2::new(dd, -344.0)),
    ];
    let diamond_center = text(&mut cmd, &font, "◆", 20.0, dm, Vec2::new(0.0, -344.0));
    let _ = ds;
    // 判定弹出文字（消行判定 + COMBO）：种类后跟白色最终攻击力 / 白色连击数。
    // 根 span 颜色照旧，子 span（数字）固定白色；Text2d 根 + TextSpan 子节点。
    let judge = text(&mut cmd, &font, "", 26.0, Color::srgb(0.98, 0.85, 0.35), Vec2::new(0.0, 300.0));
    let judge_num = cmd
        .spawn((
            TextSpan::new(""),
            TextFont { font: font.clone(), font_size: 26.0, ..default() },
            TextColor(Color::WHITE),
        ))
        .id();
    cmd.entity(judge).add_children(&[judge_num]);
    let judge_sub = text(&mut cmd, &font, "", 16.0, Color::srgb(0.95, 0.6, 0.3), Vec2::new(0.0, 268.0));
    let judge_sub_num = cmd
        .spawn((
            TextSpan::new(""),
            TextFont { font: font.clone(), font_size: 16.0, ..default() },
            TextColor(Color::WHITE),
        ))
        .id();
    cmd.entity(judge_sub).add_children(&[judge_sub_num]);
    // 中央棋盘死亡蒙版：与缩略图同款灰蒙，只盖棋盘本体（不盖垃圾条/Next/目标面板/调试 HUD）
    let death_mask = cmd
        .spawn((
            Sprite::from_color(
                Color::srgba(0.45, 0.45, 0.5, 0.65),
                Vec2::new(W as f32 * CELL + 10.0, H as f32 * CELL + 10.0),
            ),
            Transform::from_xyz(0.0, 0.0, 2.5),
            Visibility::Hidden,
        ))
        .id();
    // 名次 #N 显示在蒙版中央（z=3 压在蒙版 z=2.5 之上）
    let death_rank = text(&mut cmd, &font, "", 44.0, Color::srgb(0.92, 0.92, 0.95), Vec2::new(0.0, 0.0));
    // 调试 HUD：左上一行（玩家/对局码）+ 左下一行（连接状态），深色半透明背衬条压条
    // 文本 z=51、背衬 z=50，压在 99p 小窗网格（z≤3）之上；不折行，紧贴窗口边角
    let strip_bg = |cmd: &mut Commands| {
        cmd.spawn((
            Sprite::from_color(Color::srgba(0.03, 0.04, 0.06, 0.70), Vec2::new(240.0, 20.0)),
            Transform::from_xyz(0.0, 0.0, 50.0),
        ))
        .id()
    };
    let dbg_top_bg = strip_bg(&mut cmd);
    let dbg_bot_bg = strip_bg(&mut cmd);
    let dbg_top = cmd
        .spawn((
            Text2d::new(""),
            TextFont { font: font.clone(), font_size: 15.0, ..default() },
            TextColor(Color::srgb(0.75, 0.78, 0.82)),
            Transform::from_xyz(0.0, 0.0, 51.0),
            bevy::sprite::Anchor::TopLeft,
        ))
        .id();
    // 左下行 = 根（空）+ 5 个 span：状态 + Lat/Loss/TPS/FPS（数值各自着色）
    let dbg_bot = cmd
        .spawn((
            Text2d::new(""),
            TextFont { font: font.clone(), font_size: 15.0, ..default() },
            TextColor(Color::srgb(0.75, 0.78, 0.82)),
            Transform::from_xyz(0.0, 0.0, 51.0),
            bevy::sprite::Anchor::BottomLeft,
        ))
        .id();
    let dbg_span = |cmd: &mut Commands, text: &str| {
        cmd.spawn((
            TextSpan::new(text),
            TextFont { font: font.clone(), font_size: 15.0, ..default() },
            TextColor(HUD_DEFAULT),
        ))
        .id()
    };
    let dbg_bot_status = cmd
        .spawn((
            TextSpan::new(""),
            TextFont { font: font.clone(), font_size: 15.0, ..default() },
            TextColor(Color::srgb(0.30, 0.90, 0.45)),
        ))
        .id();
    let dbg_lat = dbg_span(&mut cmd, "");
    let dbg_loss = dbg_span(&mut cmd, "");
    let dbg_tps = dbg_span(&mut cmd, "");
    let dbg_fps = dbg_span(&mut cmd, "");
    cmd.entity(dbg_bot)
        .add_children(&[dbg_bot_status, dbg_lat, dbg_loss, dbg_tps, dbg_fps]);
    // 攻击连线池（14 条够覆盖 targeted_by≤10+瞄准 1，留余量）与光点池（多批多点同 tick）
    let mut atk_lines = Vec::new();
    for _ in 0..14 {
        atk_lines.push(
            cmd.spawn((
                Sprite::from_color(Color::WHITE, Vec2::new(1.0, 2.6)),
                Transform::from_xyz(0.0, 0.0, 6.0),
                Visibility::Hidden,
            ))
            .id(),
        );
    }
    let mut atk_dots = Vec::new();
    for _ in 0..24 {
        atk_dots.push(
            cmd.spawn((
                Sprite::from_color(Color::WHITE, Vec2::splat(10.0)),
                Transform::from_xyz(0.0, 0.0, 5.0),
                Visibility::Hidden,
            ))
            .id(),
        );
    }
    cmd.insert_resource(Views {
        cells, active, ghost, ghost_enabled: display.ghost, hold, next, meter, hud, diamond, diamond_center, judge,
        judge_num, judge_sub, judge_sub_num, death_mask, death_rank,
        dbg_top, dbg_top_bg, dbg_bot, dbg_bot_status, dbg_lat, dbg_loss, dbg_tps, dbg_fps, dbg_bot_bg,
        info, board_alert,
    });
    cmd.insert_resource(Thumbs {
        atk_lines,
        slots: atk_dots.iter().map(|_| None).collect(),
        atk_dots,
        ..Default::default()
    });
    cmd.insert_resource(PrevPiece { pieces: 0, y_milli: 0 });
}

// ———— 输入采集（Update 节奏 = 渲染循环，不锁帧时远高于 60Hz） ————

fn push_key(q: &mut InputQueue, k: Key, now: Instant) {
    q.entries.push_back((now, k));
}

#[derive(Resource, Clone)]
struct Keymap {
    left: Vec<KeyCode>,
    right: Vec<KeyCode>,
    soft: Vec<KeyCode>,
    hard: Vec<KeyCode>,
    rot_cw: Vec<KeyCode>,
    rot_ccw: Vec<KeyCode>,
    hold: Vec<KeyCode>,
    attack: Vec<KeyCode>,
    nav_l: Vec<KeyCode>,
    nav_r: Vec<KeyCode>,
    nav_u: Vec<KeyCode>,
    nav_d: Vec<KeyCode>,
}

impl Keymap {
    fn from_control(c: &t99_config::ControlSection) -> Result<Self, String> {
        let nav = match c.manual.as_str() {
            "arrow" => (
                vec![KeyCode::ArrowLeft],
                vec![KeyCode::ArrowRight],
                vec![KeyCode::ArrowUp],
                vec![KeyCode::ArrowDown],
            ),
            "wasd" => (
                vec![KeyCode::KeyA],
                vec![KeyCode::KeyD],
                vec![KeyCode::KeyW],
                vec![KeyCode::KeyS],
            ),
            other => return Err(format!("manual={other:?}，只支持 arrow / wasd")),
        };
        Ok(Self {
            left: parse_keys(&[c.left.clone()])?,
            right: parse_keys(&[c.right.clone()])?,
            soft: parse_keys(&[c.softdrop.clone()])?,
            hard: parse_keys(&[c.harddrop.clone()])?,
            rot_cw: parse_keys(&c.rotate)?,
            rot_ccw: parse_keys(&c.rotate_c)?,
            hold: parse_keys(&c.hold)?,
            attack: parse_keys(&[c.attack.clone()])?,
            nav_l: nav.0,
            nav_r: nav.1,
            nav_u: nav.2,
            nav_d: nav.3,
        })
    }

    fn preset(name: &str) -> Result<Self, String> {
        let mut c = t99_config::ControlSection::default();
        if name == "arrows" {
            c.left = "<LEFT>".into();
            c.right = "<RIGHT>".into();
            c.softdrop = "<DOWN>".into();
            c.rotate = vec!["<UP>".into(), "X".into()];
            c.manual = "wasd".into();
        } else if name != "wasd" {
            return Err(format!("未知 controls preset: {name}"));
        }
        Self::from_control(&c)
    }
}

fn parse_keys(raw: &[String]) -> Result<Vec<KeyCode>, String> {
    if raw.is_empty() {
        return Err("空键位列表".into());
    }
    raw.iter()
        .map(|s| parse_key(s).ok_or_else(|| format!("未知键位: {s:?}")))
        .collect()
}

fn parse_key(s: &str) -> Option<KeyCode> {
    let k = match s {
        "<TAB>" => KeyCode::Tab,
        "<SP>" => KeyCode::Space,
        "<CR>" => KeyCode::Enter,
        "<BS>" => KeyCode::Backspace,
        "<DEL>" => KeyCode::Delete,
        "<INS>" => KeyCode::Insert,
        "<PGUP>" => KeyCode::PageUp,
        "<PGDN>" => KeyCode::PageDown,
        "<HOME>" => KeyCode::Home,
        "<END>" => KeyCode::End,
        "<UP>" => KeyCode::ArrowUp,
        "<DOWN>" => KeyCode::ArrowDown,
        "<LEFT>" => KeyCode::ArrowLeft,
        "<RIGHT>" => KeyCode::ArrowRight,
        "<L-S>" => KeyCode::ShiftLeft,
        "<R-S>" => KeyCode::ShiftRight,
        "<L-C>" => KeyCode::ControlLeft,
        "<R-C>" => KeyCode::ControlRight,
        "<L-A>" => KeyCode::AltLeft,
        "<R-A>" => KeyCode::AltRight,
        _ => {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            match c {
                'A' => KeyCode::KeyA,
                'B' => KeyCode::KeyB,
                'C' => KeyCode::KeyC,
                'D' => KeyCode::KeyD,
                'E' => KeyCode::KeyE,
                'F' => KeyCode::KeyF,
                'G' => KeyCode::KeyG,
                'H' => KeyCode::KeyH,
                'I' => KeyCode::KeyI,
                'J' => KeyCode::KeyJ,
                'K' => KeyCode::KeyK,
                'L' => KeyCode::KeyL,
                'M' => KeyCode::KeyM,
                'N' => KeyCode::KeyN,
                'O' => KeyCode::KeyO,
                'P' => KeyCode::KeyP,
                'Q' => KeyCode::KeyQ,
                'R' => KeyCode::KeyR,
                'S' => KeyCode::KeyS,
                'T' => KeyCode::KeyT,
                'U' => KeyCode::KeyU,
                'V' => KeyCode::KeyV,
                'W' => KeyCode::KeyW,
                'X' => KeyCode::KeyX,
                'Y' => KeyCode::KeyY,
                'Z' => KeyCode::KeyZ,
                '0' => KeyCode::Digit0,
                '1' => KeyCode::Digit1,
                '2' => KeyCode::Digit2,
                '3' => KeyCode::Digit3,
                '4' => KeyCode::Digit4,
                '5' => KeyCode::Digit5,
                '6' => KeyCode::Digit6,
                '7' => KeyCode::Digit7,
                '8' => KeyCode::Digit8,
                '9' => KeyCode::Digit9,
                ',' => KeyCode::Comma,
                '.' => KeyCode::Period,
                '/' => KeyCode::Slash,
                ';' => KeyCode::Semicolon,
                '\'' => KeyCode::Quote,
                '[' => KeyCode::BracketLeft,
                ']' => KeyCode::BracketRight,
                '\\' => KeyCode::Backslash,
                '-' => KeyCode::Minus,
                '=' => KeyCode::Equal,
                '`' => KeyCode::Backquote,
                _ => return None,
            }
        }
    };
    Some(k)
}

fn session_tick_hz(session: &Session) -> u32 {
    match session {
        Session::Net { mirror, .. } => {
            let h = mirror.lock().unwrap().tick_hz;
            if h == 0 {
                DEFAULT_TICK_HZ
            } else {
                h
            }
        }
        Session::Local { engine, .. } => engine.cfg.ruleset.tick_hz.max(1),
    }
}

fn sync_tick_rate(
    session: Res<Session>,
    mut fixed: ResMut<Time<Fixed>>,
    mut last: Local<u32>,
) {
    let hz = session_tick_hz(&session);
    if *last != hz {
        fixed.set_timestep_hz(hz as f64);
        *last = hz;
    }
}

fn poll_input(
    mut queue: ResMut<InputQueue>,
    mut repeat: ResMut<RepeatState>,
    timing: Res<InputTiming>,
    keys: Res<ButtonInput<KeyCode>>,
    session: Res<Session>,
    thumbs: Res<Thumbs>,
    keymap: Res<Keymap>,
) {
    let now = Instant::now();
    let jp = |c: &[KeyCode]| c.iter().any(|c| keys.just_pressed(*c));
    let jr = |c: &[KeyCode]| c.iter().any(|c| keys.just_released(*c));
    let tick_hz = session_tick_hz(&session);
    let das = Duration::from_secs_f64(timing.das_ticks as f64 / tick_hz as f64);
    let arr = Duration::from_secs_f64(timing.arr_ticks as f64 / tick_hz as f64);
    repeat.dual_charge = timing.dual_charge;
    // 先发已到期的 DAS/ARR 脉冲，再处理本帧新按键；保证打断时 DAS 在前。
    for key in repeat_keys(&mut repeat, now, das, arr) {
        push_key(&mut queue, key, now);
    }
    // 主操作：移动/软降。横向 DAS/ARR 在这里展开，引擎只收离散事件。
    let (ml, mr, sf) = (
        keymap.left.as_slice(),
        keymap.right.as_slice(),
        keymap.soft.as_slice(),
    );
    if jp(&ml) {
        push_key(&mut queue, Key::LeftDown, now);
        repeat.left.held = true;
        repeat.active = -1;
        if repeat.left.charged {
            repeat.left.next = Some(now);
        } else if repeat.left.next.is_none() {
            repeat.left.next = Some(now + das);
        }
    }
    if jr(&ml) {
        push_key(&mut queue, Key::LeftUp, now);
        repeat.left.held = false;
        if repeat.active == -1 {
            if repeat.right.held {
                repeat.active = 1;
                if repeat.right.charged {
                    repeat.right.next = Some(now);
                }
            } else {
                repeat.active = 0;
            }
        }
    }
    if jp(&mr) {
        push_key(&mut queue, Key::RightDown, now);
        repeat.right.held = true;
        repeat.active = 1;
        if repeat.right.charged {
            repeat.right.next = Some(now);
        } else if repeat.right.next.is_none() {
            repeat.right.next = Some(now + das);
        }
    }
    if jr(&mr) {
        push_key(&mut queue, Key::RightUp, now);
        repeat.right.held = false;
        if repeat.active == 1 {
            if repeat.left.held {
                repeat.active = -1;
                if repeat.left.charged {
                    repeat.left.next = Some(now);
                }
            } else {
                repeat.active = 0;
            }
        }
    }
    if jp(&sf) {
        push_key(&mut queue, Key::SoftDown, now);
    }
    if jr(&sf) {
        push_key(&mut queue, Key::SoftUp, now);
    }
    if jp(&keymap.hard) {
        push_key(&mut queue, Key::HardDrop, now);
    }
    if jp(&keymap.rot_cw) {
        push_key(&mut queue, Key::RotCwDown, now);
    }
    if jr(&keymap.rot_cw) {
        push_key(&mut queue, Key::RotCwUp, now);
    }
    if jp(&keymap.rot_ccw) {
        push_key(&mut queue, Key::RotCcwDown, now);
    }
    if jr(&keymap.rot_ccw) {
        push_key(&mut queue, Key::RotCcwUp, now);
    }
    if jp(&keymap.hold) {
        push_key(&mut queue, Key::HoldDown, now);
    }
    if jr(&keymap.hold) {
        push_key(&mut queue, Key::HoldUp, now);
    }
    if jp(&[KeyCode::KeyR]) {
        if let Session::Net { tx, .. } = &*session {
            let _ = tx.send(NetCmd::Restart);
        }
    }
    if jp(&keymap.attack) {
        let cycle = [Key::TargetKos, Key::TargetBadges, Key::TargetAttackers, Key::TargetRandom];
        let k = cycle[queue.target_idx % cycle.len()];
        queue.target_idx += 1;
        push_key(&mut queue, k, now);
    }
    // 副操作：导航对手（shown 列表线性导航，上下按网格行宽跳）
    let shown = session.with_engine(|e, _| e.players.len()).unwrap_or(1) - 1;
    if shown > 0 {
        let mut delta = 0isize;
        let row = thumbs.n.max(1) as isize;
        if jp(&keymap.nav_l) {
            delta = -1;
        }
        if jp(&keymap.nav_r) {
            delta = 1;
        }
        if jp(&keymap.nav_u) {
            delta = -row;
        }
        if jp(&keymap.nav_d) {
            delta = row;
        }
        if delta != 0 {
            let cur = queue.cursor.map(|c| c as isize).unwrap_or(0);
            let next = (cur + delta).rem_euclid(shown as isize) as usize;
            queue.cursor = Some(next);
            let pid = thumbs.views.get(next).map(|t| t.pid).unwrap_or(next + 1);
            push_key(&mut queue, Key::TargetPlayer(pid as u16), now);
        }
    }
}

/// 返回本帧应补发的离散方向事件；不直接碰 Bevy，便于单测。
fn repeat_keys(
    state: &mut RepeatState,
    now: Instant,
    das: Duration,
    arr: Duration,
) -> Vec<Key> {
    let mut out = Vec::new();
    let dual = state.dual_charge;
    let active = state.active;
    if active == -1 {
        advance_dir(&mut state.left, now, Key::LeftDown, das, arr, true, &mut out);
    } else if active == 1 {
        advance_dir(&mut state.right, now, Key::RightDown, das, arr, true, &mut out);
    }
    if dual {
        if active != -1 {
            advance_dir(&mut state.left, now, Key::LeftDown, das, arr, false, &mut out);
        }
        if active != 1 {
            advance_dir(&mut state.right, now, Key::RightDown, das, arr, false, &mut out);
        }
    }
    out
}

fn advance_dir(
    d: &mut DirState,
    now: Instant,
    key: Key,
    das: Duration,
    arr: Duration,
    emit: bool,
    out: &mut Vec<Key>,
) {
    if !d.held {
        return;
    }
    let mut next = match d.next {
        Some(n) => n,
        None => {
            let n = now + das;
            d.next = Some(n);
            n
        }
    };
    if !d.charged {
        if now < next {
            return;
        }
        d.charged = true;
        if emit {
            out.push(key);
        }
        d.next = Some(now + arr);
        return;
    }
    if arr.is_zero() {
        if emit {
            for _ in 0..MAX_REPEAT_PER_FRAME {
                out.push(key);
            }
        }
        d.next = Some(now + Duration::from_millis(16));
        return;
    }
    let mut budget = 16;
    while now >= next && budget > 0 {
        if emit {
            out.push(key);
        }
        next += arr;
        budget -= 1;
    }
    d.next = Some(next);
}

#[cfg(test)]
mod repeat_tests {
    use super::*;
    use t99_core::Kind;

    #[test]
    fn embedded_cjk_font_parses() {
        let bytes = include_bytes!("../assets/fonts/CJK.otf").to_vec();
        assert!(Font::try_from_bytes(bytes).is_ok());
    }

    #[test]
    fn das_then_arr_emits_one_step_per_interval() {
        let t0 = Instant::now();
        let das = Duration::from_millis(167);
        let arr = Duration::from_millis(33);
        let mut s = RepeatState::default();
        s.left.held = true;
        s.active = -1;
        s.left.next = Some(t0 + das);
        assert!(repeat_keys(&mut s, t0, das, arr).is_empty());
        assert_eq!(
            repeat_keys(&mut s, t0 + das, das, arr),
            vec![Key::LeftDown]
        );
        assert_eq!(
            repeat_keys(&mut s, t0 + das + arr, das, arr),
            vec![Key::LeftDown]
        );
    }

    #[test]
    fn arr_backlog_emits_multiple_steps() {
        let t0 = Instant::now();
        let das = Duration::from_millis(167);
        let arr = Duration::from_millis(33);
        let mut s = RepeatState::default();
        s.right.held = true;
        s.active = 1;
        s.right.charged = true;
        s.right.next = Some(t0);
        let ev = repeat_keys(&mut s, t0 + arr + Duration::from_millis(1), das, arr);
        assert_eq!(ev, vec![Key::RightDown, Key::RightDown]);
    }

    #[test]
    fn keymap_parses_default_control_section() {
        let c = t99_config::ControlSection::default();
        let m = Keymap::from_control(&c).unwrap();
        assert_eq!(m.left, vec![KeyCode::KeyA]);
        assert_eq!(m.right, vec![KeyCode::KeyD]);
        assert_eq!(m.hard, vec![KeyCode::Space]);
        assert_eq!(m.rot_ccw, vec![KeyCode::KeyZ, KeyCode::ControlLeft]);
        assert_eq!(m.hold, vec![KeyCode::KeyC, KeyCode::ShiftLeft]);
        assert_eq!(m.nav_l, vec![KeyCode::ArrowLeft]);
        assert_eq!(parse_key("<SP>"), Some(KeyCode::Space));
        assert_eq!(parse_key("<TAB>"), Some(KeyCode::Tab));
        assert_eq!(parse_key("a"), None);
    }

    #[test]
    fn das_interruption_preserves_other_charge() {
        let t0 = Instant::now();
        let das = Duration::from_millis(167);
        let arr = Duration::from_millis(33);
        let mut s = RepeatState::default();
        s.right = DirState {
            held: true,
            charged: true,
            next: Some(t0),
        };
        s.active = 1;
        // 按左：right 仍然 held + charged，只切 active
        s.left.held = true;
        s.active = -1;
        s.left.next = Some(t0 + das);
        // 松左：right 恢复 active
        s.left.held = false;
        s.active = 1;
        s.right.next = Some(t0);
        assert_eq!(repeat_keys(&mut s, t0, das, arr), vec![Key::RightDown]);
        assert!(s.right.charged);
    }

    #[test]
    fn dual_charge_advances_inactive_without_emitting() {
        let t0 = Instant::now();
        let das = Duration::from_millis(167);
        let arr = Duration::from_millis(33);
        let mut s = RepeatState {
            left: DirState { held: true, charged: false, next: Some(t0 + das) },
            right: DirState { held: true, charged: false, next: Some(t0 + das) },
            active: -1,
            dual_charge: true,
        };
        let ev = repeat_keys(&mut s, t0 + das, das, arr);
        assert_eq!(ev, vec![Key::LeftDown]);
        assert!(s.left.charged);
        assert!(s.right.charged);
    }

    #[test]
    fn version_gate_rejects_mismatch() {
        assert!(version_ok(PROTOCOL as u64, t99_core::RULESET_ID));
        assert!(!version_ok(PROTOCOL as u64 + 1, t99_core::RULESET_ID));
        assert!(!version_ok(PROTOCOL as u64, "deadbeef"));
    }

    #[test]
    fn ghost_lands_on_floor() {
        let board = [[0; W]; H];
        let a = ActiveView { kind: Kind::T, rot: 0, x: 3, y_milli: 0 };
        let cells = ghost_cells_for(&board, a);
        assert!(cells.contains(&(18, 4)), "T top cell: {cells:?}");
        assert!(cells.contains(&(19, 3)), "T bottom left: {cells:?}");
        assert!(cells.contains(&(19, 4)), "T bottom middle: {cells:?}");
        assert!(cells.contains(&(19, 5)), "T bottom right: {cells:?}");
    }

    #[test]
    fn ghost_rests_one_row_above_stack() {
        let mut board = [[0; W]; H];
        board[10][4] = 1;
        let a = ActiveView { kind: Kind::T, rot: 0, x: 3, y_milli: 0 };
        let cells = ghost_cells_for(&board, a);
        assert!(cells.contains(&(8, 4)), "T top cell: {cells:?}");
        assert!(cells.contains(&(9, 4)), "T bottom middle: {cells:?}");
        assert!(!cells.contains(&(10, 4)), "must not overlap stack: {cells:?}");
    }
}

fn drain_input(queue: &mut InputQueue) -> Vec<TimedKey> {
    let now = Instant::now();
    let mut out: Vec<TimedKey> = Vec::new();
    let mut last_t: i64 = -1;
    while let Some((ts, key)) = queue.entries.front().copied() {
        if ts > now {
            break;
        }
        queue.entries.pop_front();
        let elapsed_us = (now - ts).as_micros() as u64;
        let mut t_us = FRAME_US.saturating_sub(elapsed_us).min(FRAME_US - 1) as i64;
        if t_us <= last_t {
            t_us = last_t + 1;
        }
        if t_us >= FRAME_US as i64 {
            continue; // 放不下，丢弃（下一帧重排）
        }
        last_t = t_us;
        out.push(TimedKey { t_us: t_us as u32, key });
    }
    out
}

// ———— 逻辑节拍 ————

fn tick(mut session: ResMut<Session>, mut queue: ResMut<InputQueue>) {
    match &mut *session {
        Session::Local { engine, bots } => {
            if engine.over.is_some() {
                return;
            }
            let n = engine.players.len();
            let mut inputs: Vec<Vec<TimedKey>> = Vec::with_capacity(n);
            for pid in 0..n {
                if !engine.players[pid].alive {
                    inputs.push(vec![]);
                    continue;
                }
                if pid == 0 {
                    inputs.push(drain_input(&mut queue));
                } else {
                    let mut bot = bots[pid].take();
                    let ev = match bot.as_mut() {
                        Some(b) => {
                            let snap = engine.snapshot(pid);
                            b.act(&snap)
                        }
                        None => vec![],
                    };
                    bots[pid] = bot;
                    inputs.push(ev);
                }
            }
            engine.frame(&inputs);
        }
        Session::Net { mirror, tx, observer, .. } => {
            // 客户端只上报输入；对局由服务端权威推进，镜像本地重演。
            // 每收到一个新的私有 frame 回一次 input；空事件也回，保持席位存活。
            if !*observer {
                let (frame_n, alive, sent_n) = {
                    let g = mirror.lock().unwrap();
                    let alive = g
                        .engine
                        .as_ref()
                        .map(|e| e.players[g.my_seat].alive)
                        .unwrap_or(false);
                    (g.pending_frame_n, alive, g.last_input_n)
                };
                if alive && frame_n > sent_n {
                    let ev = drain_input(&mut queue);
                    let _ = tx.send(NetCmd::Input { n: frame_n, evs: ev });
                    mirror.lock().unwrap().last_input_n = frame_n;
                }
            }
        }
    }
}

// ———— 渲染同步 ————

fn ghost_cells(s: &Snapshot) -> Vec<(i64, i64)> {
    match s.active {
        Some(a) => ghost_cells_for(&s.board, a),
        None => vec![],
    }
}

fn ghost_cells_for(board: &[[Cell; W]; H], a: ActiveView) -> Vec<(i64, i64)> {
    let mut drop = i64::MAX; // 各格所需下落量的最小值 = 最先触底的格
    for &(dx, dy) in a.kind.cells(a.rot).iter() {
        let col = a.x as i64 + dx as i64;
        if col < 0 || col >= W as i64 {
            return vec![];
        }
        let mut top = H as i64;
        for r in 0..H {
            if board[r][col as usize] != 0 {
                top = r as i64;
                break;
            }
        }
        // y 已经包含该格的 dy；这里只能减一次，否则 ghost 会整体上移一格。
        let y = (a.y_milli + dy as i64 * 1000).div_euclid(1000);
        drop = drop.min(top - 1 - y);
    }
    if drop == i64::MAX {
        return vec![];
    }
    a.kind
        .cells(a.rot)
        .iter()
        .map(|&(dx, dy)| {
            let y = (a.y_milli + dy as i64 * 1000).div_euclid(1000);
            (y + drop, a.x as i64 + dx as i64)
        })
        .collect()
}

fn show_grid(
    q: &mut Query<(&mut Visibility, &mut Sprite, &mut Transform)>,
    ents: &[Entity],
    cols: usize,
    pred: &dyn Fn(usize, usize) -> Option<Cell>,
) {
    for (i, &e) in ents.iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        if let Ok((mut v, mut s, _)) = q.get_mut(e) {
            match pred(r, c) {
                Some(id) => {
                    *v = Visibility::Visible;
                    s.color = kind_color(id);
                }
                None => *v = Visibility::Hidden,
            }
        }
    }
}

fn hide(q: &mut Query<(&mut Visibility, &mut Sprite, &mut Transform)>, ents: &[Entity]) {
    for &e in ents {
        if let Ok((mut v, _, _)) = q.get_mut(e) {
            *v = Visibility::Hidden;
        }
    }
}

fn place(q: &mut Query<(&mut Visibility, &mut Sprite, &mut Transform)>, e: Entity, pos: Vec2, color: Color, alpha: f32) {
    if let Ok((mut v, mut s, mut tr)) = q.get_mut(e) {
        *v = Visibility::Visible;
        s.color = color.with_alpha(alpha);
        tr.translation.x = pos.x;
        tr.translation.y = pos.y;
    }
}

fn set_text(q: &mut Query<&mut Text2d>, e: Entity, s: String) {
    if let Ok(mut t) = q.get_mut(e) {
        *t = Text2d::new(s);
    }
}

fn set_span(q: &mut Query<&mut TextSpan>, e: Entity, s: String) {
    if let Ok(mut sp) = q.get_mut(e) {
        *sp = TextSpan::new(s);
    }
}

/// 调试 HUD 文本：左上一行「玩家 ID | 对局码」，左下一行「状态(Build值) + 延迟/丢包/TPS/FPS」。
/// 状态单独着色（Connected 绿 / Disconnected 红 / 本地 Local 哑色），其余恒为哑色。
fn set_debug_hud_text(
    session: &Session,
    shown_fps: f32,
    shown_tps: f32,
    text_q: &mut Query<&mut Text2d>,
    span_q: &mut Query<&mut TextSpan>,
    color_q: &mut Query<&mut TextColor>,
    views: &Views,
) {
    let stat = match &*session {
        Session::Net { stats, .. } => Some(stats.lock().unwrap().clone()),
        Session::Local { .. } => None,
    };
    let set = |span_q: &mut Query<&mut TextSpan>,
               color_q: &mut Query<&mut TextColor>,
               e: Entity,
               s: String,
               c: Color| {
        set_span(span_q, e, s);
        if let Ok(mut tc) = color_q.get_mut(e) {
            tc.0 = c;
        }
    };
    set_text(text_q, views.dbg_bot, String::new());
    match stat {
        Some(st) => {
            let pid = if st.player_id.is_empty() { "(observer)".to_string() } else { st.player_id };
            let mid = if st.match_id == 0 { "-".to_string() } else { format!("{:x}", st.match_id) };
            set_text(text_q, views.dbg_top, format!("PlayerID {pid} | MatchID {mid}"));
            let status = if st.connected { "Connected" } else { "Disconnected" };
            let status_col = if st.connected { Color::srgb(0.30, 0.90, 0.45) } else { HUD_RED };
            if let Ok(mut tc) = color_q.get_mut(views.dbg_bot_status) {
                tc.0 = status_col;
            }
            set_span(span_q, views.dbg_bot_status, status.to_string());
            if st.connected {
                set(span_q, color_q, views.dbg_lat, format!(" | Lat {}ms", st.rtt_ms), lat_color(st.rtt_ms));
                set(span_q, color_q, views.dbg_loss, format!(" | Loss {}%", st.loss_pct), loss_color(st.loss_pct));
                set(span_q, color_q, views.dbg_tps, format!(" | TPS {:.2}", shown_tps), tps_color(shown_tps));
                set(span_q, color_q, views.dbg_fps, format!(" | FPS {:.0}", shown_fps), fps_color(shown_fps));
            } else {
                set(span_q, color_q, views.dbg_lat, " | Lat - | Loss - | TPS -".into(), HUD_DEFAULT);
                set(span_q, color_q, views.dbg_loss, String::new(), HUD_DEFAULT);
                set(span_q, color_q, views.dbg_tps, String::new(), HUD_DEFAULT);
                set(span_q, color_q, views.dbg_fps, String::new(), HUD_DEFAULT);
            }
        }
        None => {
            set_text(text_q, views.dbg_top, "PlayerID local | MatchID -".to_string());
            set_span(span_q, views.dbg_bot_status, "Local".to_string());
            if let Ok(mut tc) = color_q.get_mut(views.dbg_bot_status) {
                tc.0 = Color::srgb(0.55, 0.60, 0.70);
            }
            set(span_q, color_q, views.dbg_lat, String::new(), HUD_DEFAULT);
            set(span_q, color_q, views.dbg_loss, String::new(), HUD_DEFAULT);
            set(span_q, color_q, views.dbg_tps, String::new(), HUD_DEFAULT);
            set(span_q, color_q, views.dbg_fps, format!(" | FPS {:.0}", shown_fps), fps_color(shown_fps));
        }
    }
}

/// 调试 HUD 逐帧摆位：文本锚定窗口左上/左下角（一行式，不折行不撑主棋盘），
/// 背衬条按文本测量宽度自适应压在文字底下（z=50，文本 z=51，压在 99p 小窗网格 z≤3 之上）。
fn place_debug_hud(
    views: &Views,
    vq: &mut Query<(&mut Visibility, &mut Sprite, &mut Transform)>,
    text_tr: &mut Query<&mut Transform, (With<Text2d>, Without<Sprite>)>,
    layout: &Query<&TextLayoutInfo, With<Text2d>>,
    half_w: f32,
    half_h: f32,
) {
    let mx = 14.0;
    for (te, be, top) in [(views.dbg_top, views.dbg_top_bg, true), (views.dbg_bot, views.dbg_bot_bg, false)] {
        // 未测量（首帧/字体未加载）时给保守默认宽
        let size = layout.get(te).map(|i| i.size).unwrap_or(Vec2::new(240.0, 18.0));
        let y = if top { half_h - 10.0 } else { -half_h + 10.0 };
        // TopLeft/BottomLeft 锚：文本框自锚点向下/向上展开，背衬条取框体竖直中点
        let cy = if top { y - size.y / 2.0 } else { y + size.y / 2.0 };
        if let Ok(mut tr) = text_tr.get_mut(te) {
            tr.translation = Vec3::new(-half_w + mx, y, 51.0);
        }
        if let Ok((_, mut sp, mut tr)) = vq.get_mut(be) {
            let w = (size.x + 18.0).max(190.0);
            let h = (size.y + 5.0).max(20.0);
            sp.custom_size = Some(Vec2::new(w, h));
            tr.translation = Vec3::new(-half_w + mx - 9.0 + w / 2.0, cy, 50.0);
        }
    }
}

fn rebuild_thumbs(
    cmd: &mut Commands,
    font: &Handle<Font>,
    session: &Session,
    thumbs: &mut Thumbs,
    half_w: f32,
) {
    for e in thumbs.all_ents.drain(..) {
        cmd.entity(e).despawn();
    }
    thumbs.views.clear();
    let Some((others, _my_seat)) = session.with_engine(|e, seat| {
        ((0..e.players.len()).filter(|&p| p != seat).collect::<Vec<_>>(), seat)
    }) else {
        thumbs.n = 0;
        return;
    };
    if others.is_empty() {
        thumbs.n = 0;
        return;
    }
    // 网格等级由 sync 决定（开局最小可容 + 淘汰降级），此处只消费
    let n = thumbs.n.max(2);
    let cap = 2 * n * n;
    let mut shown: Vec<usize> = session
        .with_engine(|e, _| others.iter().copied().filter(|&p| e.players[p].alive).collect())
        .unwrap_or_default();
    // 死者按名次升序补位（最近淘汰在前）：网格缩小时从尾部移除最早淘汰者
    let dead_order: Vec<usize> = session
        .with_engine(|e, _| {
            let mut d: Vec<(u32, usize)> = others
                .iter()
                .copied()
                .filter(|&p| !e.players[p].alive)
                .map(|p| (e.players[p].place.unwrap_or(u32::MAX), p))
                .collect();
            d.sort_by_key(|&(pl, _)| pl);
            d.into_iter().map(|(_, p)| p).collect()
        })
        .unwrap_or_default();
    for p in dead_order {
        if !shown.contains(&p) && shown.len() < cap {
            shown.push(p);
        }
    }
    shown.truncate(cap);
    let per_side = (shown.len() + 1) / 2;
    for (i, &pid) in shown.iter().enumerate() {
        let (side, idx) = if i < per_side { (0usize, i) } else { (1usize, i - per_side) };
        let (col, row) = (idx % n, idx / n);
        let grid_w = n as f32 * (TW + 6.0);
        let grid_h = n as f32 * (TH + 18.0);
        let cx = if side == 0 {
            let ideal = 290.0 + grid_half_for(n);
            -ideal.min((half_w - 12.0 - grid_half_for(n)).max(148.0 + grid_half_for(n)))
        } else {
            side_center_x(n, half_w)
        };
        let x = cx - grid_w / 2.0 + col as f32 * (TW + 6.0) + TW / 2.0;
        let y = grid_h / 2.0 - row as f32 * (TH + 18.0) - TH / 2.0;
        // 背景填充 = 危险度（白/黄/橙/红）或死亡灰
        let bg = cmd
            .spawn((
                Sprite::from_color(Color::srgb(0.2, 0.2, 0.24), Vec2::new(TW, TH)),
                Transform::from_xyz(x, y, -0.5),
            ))
            .id();
        // 边缘线 = 瞄准关系（淡蓝=被本方瞄准 红=瞄准本方 紫=互瞄），无关系时隐藏
        let edge = cmd
            .spawn((
                Sprite::from_color(Color::srgb(0.45, 0.75, 1.0), Vec2::new(TW + 4.5, TH + 4.5)),
                Transform::from_xyz(x, y, -0.55),
                Visibility::Hidden,
            ))
            .id();
        let mask = cmd
            .spawn((
                Sprite::from_color(Color::srgb(0.5, 0.5, 0.55), Vec2::new(TW - 4.0, TH - 4.0)),
                Transform::from_xyz(x, y, 0.4),
                Visibility::Hidden,
            ))
            .id();
        let origin = Vec2::new(x, y);
        let cells = spawn_grid(cmd, origin, MINI, W, H, 0.3);
        let active = spawn_grid(cmd, origin, MINI, 1, 4, 0.35);
        // 小窗垃圾条（单列堆叠，同主界面）
        let meter = spawn_grid(cmd, Vec2::new(x - TW / 2.0 - 4.0, y), MINI, 1, H, 0.35);
        // 击杀数（★）：贴框体上沿内侧显示
        let badge = text(cmd, font, "", 12.0, Color::srgb(0.95, 0.85, 0.3), Vec2::new(x, y + TH / 2.0 - 11.0));
        let id = text(cmd, font, &format!("{}", pid + 1), 12.0, Color::srgb(0.7, 0.75, 0.8), Vec2::new(x, y - TH / 2.0 - 9.0));
        let rank = text(cmd, font, "", 14.0, Color::srgb(0.85, 0.85, 0.88), Vec2::new(x, y));
        thumbs.all_ents.push(bg);
        thumbs.all_ents.push(edge);
        thumbs.all_ents.push(mask);
        thumbs.all_ents.extend(cells.iter().copied());
        thumbs.all_ents.extend(active.iter().copied());
        thumbs.all_ents.extend(meter.iter().copied());
        thumbs.all_ents.push(badge);
        thumbs.all_ents.push(id);
        thumbs.all_ents.push(rank);
        thumbs.views.push(ThumbView { pid, origin, bg, edge, mask, cells, active, meter, badge, id, rank });
    }
    eprintln!(
        "[grid] n={n} views={} per_side={per_side} side_x=±{:.0} shown0={}",
        thumbs.views.len(),
        side_center_x(n, half_w),
        shown.first().copied().unwrap_or(999)
    );
}

fn sync(
    mut cmd: Commands,
    session: Res<Session>,
    views: Res<Views>,
    font_assets: Res<UiFont>,
    mut thumbs: ResMut<Thumbs>,
    queue: Res<InputQueue>,
    mut prev: ResMut<PrevPiece>,
    fixed: Res<Time<Fixed>>,
    time: Res<Time>,
    mut hud: ResMut<HudMetrics>,
    mut vq: Query<(&mut Visibility, &mut Sprite, &mut Transform)>,
    mut text_q: Query<&mut Text2d>,
    mut span_q: Query<&mut TextSpan>,
    mut color_q: Query<&mut TextColor>,
    // 文本变换 + 逐字符测量宽信息：合并为一组参数（Bevy 系统参数元组上限 16）
    mut dbg_tr: (Query<&mut Transform, (With<Text2d>, Without<Sprite>)>, Query<&TextLayoutInfo, With<Text2d>>),
    windows: Query<&Window>,
) {
    let half_w = windows.single().map(|w| w.width() / 2.0).unwrap_or(800.0);
    let half_h = windows.single().map(|w| w.height() / 2.0).unwrap_or(450.0);
    let inst_fps = 1.0 / time.delta_secs().max(1e-6);
    // FPS：K = floor(R / M)，窗口内取最小，满 K 帧刷新一次
    if hud.shown_fps <= 0.0 {
        hud.shown_fps = inst_fps;
        hud.fps_min = inst_fps;
    }
    hud.fps_min = hud.fps_min.min(inst_fps);
    hud.fps_frames += 1;
    let k_fps = ((inst_fps / DISPLAY_HZ).floor() as u32).max(1);
    if hud.fps_frames >= k_fps {
        hud.shown_fps = hud.fps_min;
        hud.fps_min = inst_fps;
        hud.fps_frames = 0;
    }
    let Some(s) = session.with_engine(|e, seat| e.snapshot(seat)) else {
        // 网络模式未就绪：显示连接状态
        if let Session::Net { status, .. } = &*session {
            let st = status.lock().unwrap().clone();
            if let Some(e) = views.hud.get(5) {
                set_text(&mut text_q, *e, st);
            }
        }
        // 调试 HUD 在等待/断线期同样可见（Disconnected + 横杠占位）
        set_debug_hud_text(&session, hud.shown_fps, hud.shown_tps, &mut text_q, &mut span_q, &mut color_q, &views);
        place_debug_hud(&views, &mut vq, &mut dbg_tr.0, &dbg_tr.1, half_w, half_h);
        return;
    };
    // TPS：只在收到新 tick 时采样，K = floor(tick_hz / M)，窗口内取最小后刷新
    let tick_hz = session_tick_hz(&session);
    let tps_now = match &*session {
        Session::Net { stats, .. } => stats.lock().unwrap().server_tps,
        _ => 0.0,
    };
    if s.n != hud.last_tick_n {
        hud.last_tick_n = s.n;
        if hud.shown_tps <= 0.0 {
            hud.shown_tps = tps_now;
            hud.tps_min = tps_now;
        }
        hud.tps_min = hud.tps_min.min(tps_now);
        hud.tps_ticks += 1;
        let k_tps = ((tick_hz as f32 / DISPLAY_HZ).floor() as u32).max(1);
        if hud.tps_ticks >= k_tps {
            hud.shown_tps = hud.tps_min;
            hud.tps_min = tps_now;
            hud.tps_ticks = 0;
        }
    }
    // 反攻模式：被本方瞄准的是全部攻击者（引擎把完整攻击分别打给每个攻击者，最多 10 人）
    let attackers_mode = s.target_mode == TargetMode::Attackers;
    // 本方座位：连线/光点端点用它区分菱形与缩略图
    let seat = session.with_engine(|_, st| st).unwrap_or(0);
    // —— 己方棋盘 ——
    show_grid(&mut vq, &views.cells, W, &|r, c| if s.board[r][c] != 0 { Some(s.board[r][c]) } else { None });
    // 消行闪烁：Clear 相位中被消的行以白色脉冲覆盖
    if !s.clearing.is_empty() {
        let blink = (s.phase.ms_left() % 200) < 100;
        for &row in &s.clearing {
            if row < H {
                for c in 0..W {
                    if let Ok((mut v, mut sp, _)) = vq.get_mut(views.cells[row * W + c]) {
                        *v = Visibility::Visible;
                        sp.color = if blink {
                            Color::srgb(1.0, 1.0, 1.0)
                        } else {
                            Color::srgb(0.85, 0.9, 1.0)
                        };
                    }
                }
            }
        }
    }
    hide(&mut vq, &views.active);
    hide(&mut vq, &views.ghost);
    if let Some(a) = s.active {
        // 外推插值：恒速重力下渲染平滑
        let alpha = fixed.overstep_fraction();
        let dy = if prev.pieces == s.pieces { (a.y_milli - prev.y_milli) as f32 } else { 0.0 };
        let y_draw = a.y_milli as f32 + dy * alpha;
        for (i, &(dx, dy)) in a.kind.cells(a.rot).iter().enumerate() {
            let row = ((y_draw + dy as f32 * 1000.0) / 1000.0).floor() as i64;
            let col = a.x as i64 + dx as i64;
            if (0..H as i64).contains(&row) && (0..W as i64).contains(&col) {
                place(&mut vq, views.active[i], board_pos(col, row), kind_color(a.kind.id()), 1.0);
            }
        }
        if views.ghost_enabled {
            for (i, &(row, col)) in ghost_cells(&s).iter().enumerate() {
                if i < views.ghost.len() && (0..H as i64).contains(&row) && (0..W as i64).contains(&col) {
                    place(&mut vq, views.ghost[i], board_pos(col, row), kind_color(a.kind.id()), 0.25);
                }
            }
        }
    }
    prev.pieces = s.pieces;
    prev.y_milli = s.active.map(|a| a.y_milli).unwrap_or(0);
    // Hold / Next / 垃圾条
    show_grid(&mut vq, &views.hold, 4, &|r, c| match s.hold {
        Some(k) => k.cells(0).iter().find(|&&(dx, dy)| dx as usize == c && dy as usize == r).map(|_| k.id()),
        None => None,
    });
    show_grid(&mut vq, &views.next, 4, &|r, c| {
        let idx = r / 3;
        let rr = r % 3;
        if rr == 2 {
            return None;
        }
        match s.next.get(idx) {
            Some(&k) => k.cells(0).iter().find(|&&(dx, dy)| dx as usize == c && dy as usize == rr).map(|_| k.id()),
            None => None,
        }
    });
    // 垃圾条单列：红在下（即将进场，T99 红闪：以黄转红的相同间隔闪烁）、黄其上（计时中）、灰顶层（排队）
    let red_on = session
        .with_engine(|e, _| {
            let stage_ms = e.cfg.ruleset.garbage_stage(e.phase_idx) / TMS;
            let cycle = (2 * stage_ms).max(1);
            (s.n as i64 * 16666 / 1000) % cycle < stage_ms
        })
        .unwrap_or(true);
    // 垃圾条按批次分组摆位：同一 sender 的连续条目相邻（普通行距），
    // 不同批之间的空档 = 3 倍行距；队首（红，即将进场）贴棋盘在下，灰(排队)在顶。
    // 批次总高超出列框时整体等比压缩行距（方块随之缩小），全部保持可见。
    {
        let batches = &s.garbage_batches;
        let rows_total: u32 = batches.iter().map(|g| g.rows).sum();
        let gaps = batches
            .iter()
            .zip(batches.iter().skip(1))
            .filter(|(a, b)| a.sender != b.sender)
            .count();
        let base = 20.0; // 普通行距（与方块间距一致）
        // 行距数量：批内每格 1 个，批界因 3 倍空档多出 2 个；列框内可用 398px（y∈[-198, 200]）
        let need_units = (rows_total.max(1) as f32 - 1.0) + 2.0 * gaps as f32;
        let scale = ((398.0 / base) / need_units.max(1.0)).min(1.0);
        let pitch = base * scale;
        let cell_size = (base * scale - 1.0).max(5.0);
        let mut y = -198.0;
        let mut prev_sender: Option<usize> = None;
        let mut used = 0usize;
        for g in batches.iter() {
            if let Some(ps) = prev_sender {
                if ps != g.sender {
                    // 批界：多出 2 个“方块间隙”，总间距 = 3 × 普通行间隙（不是 3 × 方块宽）
                    let gap = (pitch - cell_size).max(0.0);
                    y += 2.0 * gap;
                }
            }
            prev_sender = Some(g.sender);
            let col = match g.state {
                2 => {
                    // 红闪：与旧 red/yellow 相同的亮暗交替
                    if red_on {
                        5
                    } else {
                        9
                    }
                }
                1 => 2,
                _ => 8,
            };
            for _ in 0..g.rows {
                if used >= views.meter.len() {
                    break;
                }
                if let Ok((mut v, mut sp, mut tr)) = vq.get_mut(views.meter[used]) {
                    *v = Visibility::Visible;
                    sp.color = kind_color(col);
                    sp.custom_size = Some(Vec2::splat(cell_size));
                    tr.translation = Vec3::new(-159.0, y, 0.0);
                }
                used += 1;
                y += pitch;
            }
            if used >= views.meter.len() {
                break;
            }
        }
        hide(&mut vq, &views.meter[used..]);
    }
    // —— HUD ——
    set_text(&mut text_q, views.hud[0], format!("存活 {}/{} 相位{}", s.alive.len(), session.with_engine(|e, _| e.players.len()).unwrap_or(0), session.with_engine(|e, _| e.phase_idx + 1).unwrap_or(1)));
    set_text(&mut text_q, views.hud[1], format!("徽章{} KO{} 块{}", s.badge_units, s.ko, s.pieces));
    set_text(
        &mut text_q,
        views.hud[2],
        format!("消行{} 送出{} APM{:.0}", s.lines, s.sent, if s.n > 0 { s.sent as f64 * 3600.0 / s.n as f64 } else { 0.0 }),
    );
    let tgt = match s.target_mode {
        TargetMode::Manual(t) => format!("手动→{}", t + 1),
        m => m.name().to_string(),
    };
    let tgt_to = if attackers_mode {
        if s.targeted_by.is_empty() {
            "随机".to_string()
        } else {
            format!("全部×{}", s.targeted_by.len())
        }
    } else {
        s.target.map(|t| t + 1).map(|x| x.to_string()).unwrap_or("-".into())
    };
    set_text(
        &mut text_q,
        views.hud[3],
        format!("目标 {} → {} 被瞄{}", tgt, tgt_to, s.targeted_by.len()),
    );
    set_text(
        &mut text_q,
        views.hud[4],
        format!(
            "combo{} b2b{} {:.2}G {}",
            s.combo,
            s.b2b,
            s.gravity_milli as f64 / 60000.0,
            match s.phase {
                PlayerPhase::Play => "".into(),
                PlayerPhase::Clear(_) => format!("消行{}ms", s.phase.ms_left()),
                PlayerPhase::Are(_) => format!("ARE{}ms", s.phase.ms_left()),
            }
        ),
    );
    let place_str = if !s.alive_self {
        format!("第 {} 名", s.place.unwrap_or(0))
    } else if session.with_engine(|e, _| e.over.is_some()).unwrap_or(false) {
        "第一名！".to_string()
    } else {
        String::new()
    };
    set_text(&mut text_q, views.hud[5], place_str);
    // 被瞄警示框：与棋盘框同几何的红色描边；被瞄（本方存活）时慢脉冲亮起，一眼可见危险信号
    if s.alive_self && !s.targeted_by.is_empty() {
        let pulse = 0.55 + 0.25 * ((s.n as f32 * 0.08).sin() * 0.5 + 0.5);
        for &e in &views.board_alert {
            if let Ok((mut v, mut sp, _)) = vq.get_mut(e) {
                *v = Visibility::Visible;
                sp.color = Color::srgba(0.95, 0.30, 0.30, pulse);
            }
        }
    } else {
        for &e in &views.board_alert {
            if let Ok((mut v, _, _)) = vq.get_mut(e) {
                *v = Visibility::Hidden;
            }
        }
    }
    // 中央棋盘死亡蒙版 + 名次（蒙版只盖棋盘本体；垃圾条/Next/目标面板/调试 HUD 均在外）
    if let Ok((mut v, _, _)) = vq.get_mut(views.death_mask) {
        *v = if s.alive_self { Visibility::Hidden } else { Visibility::Visible };
    }
    set_text(
        &mut text_q,
        views.death_rank,
        if s.alive_self {
            String::new()
        } else {
            s.place.map(|p| format!("#{p}")).unwrap_or_default()
        },
    );
    // 对局倒计时（0 杀加速时显示倍率并变红）
    let cd = if s.burn_x10 > 10 {
        format!("{}:{:02} ×{:.1}", s.match_secs_left / 60, s.match_secs_left % 60, s.burn_x10 as f32 / 10.0)
    } else {
        format!("{}:{:02}", s.match_secs_left / 60, s.match_secs_left % 60)
    };
    set_text(&mut text_q, views.hud[6], cd);
    let mut color_q_state = color_q;
    if let Ok(mut tc) = color_q_state.get_mut(views.hud[6]) {
        tc.0 = if s.burn_x10 > 10 { Color::srgb(0.95, 0.35, 0.35) } else { Color::srgb(0.65, 0.7, 0.8) };
    }
    // —— Next6 下方信息面板：徽章单元/击杀/档位 Lv/攻击加成(+25%×档)/剩余人数 ——
    {
        let rows = [
            format!("徽章 {}", s.badge_units),
            format!("击杀 {}", s.ko),
            format!("档位 Lv.{}", s.badges.clamp(0, 4)),
            format!("攻击 +{}%", s.badges * 25),
            format!("存活 {}", s.alive.len()),
        ];
        for (i, r) in rows.iter().enumerate() {
            set_text(&mut text_q, views.info[i], r.clone());
        }
        // 有加成时攻击行点亮金色，与菱形选中色一致
        if let Ok(mut tc) = color_q_state.get_mut(views.info[3]) {
            tc.0 = if s.badges > 0 {
                Color::srgb(0.98, 0.8, 0.25)
            } else {
                Color::srgb(0.85, 0.88, 0.95)
            };
        }
    }
    // 当前模式参数（相位规格 + 垃圾延迟）
    let params = session
        .with_engine(|e, _| {
            let rules = &e.cfg.ruleset;
            let ph = &rules.phases[e.phase_idx];
            format!(
                "P{} G{:.2}→{:.2} ARE{} 消{} 垃{}",
                e.phase_idx + 1,
                ph.gravity_base as f64 / 60000.0,
                rules.gravity_hold(e.phase_idx) as f64 / 60000.0,
                ph.are / TMS,
                ph.clear_delay / TMS,
                rules.garbage_stage(e.phase_idx) / TMS
            )
        })
        .unwrap_or_default();
    set_text(&mut text_q, views.hud[7], params);
    // 判定弹出：最近消行 1s 内显示（按 last_clear.frame 过滤），COMBO 在下方；新判定直接替换。
    // 普通消 1（无旋转/全消/B2B 且无连击）不弹。种类后的数字 = 最终攻击力（已加算/徽章/cap20）。
    let mut judge_txt = String::new();
    let mut judge_num_txt = String::new();
    let mut combo_txt = String::new();
    let mut combo_num_txt = String::new();
    if let Some(lc) = &s.last_clear {
        if s.n.saturating_sub(lc.frame) < 60 {
            let plain_single = lc.spin == 0 && lc.lines == 1 && !lc.all_clear && !lc.b2b && s.combo < 1;
            if !plain_single {
                let name = match (lc.spin, lc.lines) {
                    (2, 1) => "T-SPIN SINGLE".to_string(),
                    (2, 2) => "T-SPIN DOUBLE".to_string(),
                    (2, 3) => "T-SPIN TRIPLE".to_string(),
                    (2, _) => "T-SPIN".to_string(),
                    (1, 1) => "MINI SINGLE".to_string(),
                    (1, 2) => "MINI DOUBLE".to_string(),
                    (1, _) => "MINI".to_string(),
                    (_, 1) => "SINGLE".to_string(),
                    (_, 2) => "DOUBLE".to_string(),
                    (_, 3) => "TRIPLE".to_string(),
                    (_, 4) => "TETRIS".to_string(),
                    (_, k) => format!("消{k}"),
                };
                let name = if lc.all_clear { "PERFECT CLEAR".to_string() } else { name };
                // 尾缀空格：白色攻击力数字以子 span 接在种类后
                judge_txt = if lc.b2b { format!("B2B {name} ") } else { format!("{name} ") };
                judge_num_txt = lc.attack.to_string();
                if s.combo >= 1 {
                    combo_txt = "COMBO ".to_string();
                    combo_num_txt = s.combo.to_string();
                }
            }
        }
    }
    set_text(&mut text_q, views.judge, judge_txt);
    set_span(&mut span_q, views.judge_num, judge_num_txt);
    set_text(&mut text_q, views.judge_sub, combo_txt);
    set_span(&mut span_q, views.judge_sub_num, combo_num_txt);
    // HUD 列：贴窗口右缘（窗口自适应）；调试堆移右下角
    {
        let hud_x = half_w - 14.0;
        let spots = [
            (views.hud[0], hud_x, 300.0), // 存活/相位
            (views.hud[1], hud_x, 270.0), // 徽章/KO/块
            (views.hud[2], hud_x, 240.0), // 消行/送出/APM
            (views.hud[5], hud_x, 368.0), // 名次
            // 倒计时：主棋盘正上方水平居中（不与 judge/judge_sub/标题重叠）
            (views.hud[6], 0.0, 342.0),
            // 调试堆：目标 / combo-b2b-G / 相位参数 → 右下角（BottomRight 锚定）
            (views.hud[3], hud_x, -half_h + 40.0),
            (views.hud[4], hud_x, -half_h + 64.0),
            (views.hud[7], hud_x, -half_h + 86.0),
        ];
        for (e, x, y) in spots {
            if let Ok(mut tr) = dbg_tr.0.get_mut(e) {
                tr.translation = Vec3::new(x, y, 0.0);
            }
        }
    }
    // 调试 HUD：左上一行「玩家 ID | 对局码」，左下一行「连接状态 + 延迟/丢包/TPS/FPS」，
    // z=50/51 压在小窗网格之上，背衬条保证其在 99p 7×7 网格盖上仍可读
    set_debug_hud_text(&session, hud.shown_fps, hud.shown_tps, &mut text_q, &mut span_q, &mut color_q_state, &views);
    place_debug_hud(&views, &mut vq, &mut dbg_tr.0, &dbg_tr.1, half_w, half_h);
    // 菱形高亮：当前模式金色，其余灰
    let sel = match s.target_mode {
        TargetMode::Random => 0,
        TargetMode::Attackers => 1,
        TargetMode::Kos => 2,
        TargetMode::Badges => 3,
        TargetMode::Manual(_) => 4,
    };
    let hot = Color::srgb(0.98, 0.8, 0.25);
    let dim = Color::srgb(0.45, 0.5, 0.58);
    for (i, &e) in views.diamond.iter().enumerate() {
        let want = if sel == i { hot } else { dim };
        if let Ok(mut tc) = color_q_state.get_mut(e) {
            tc.0 = want;
        }
    }
    if let Ok(mut tc) = color_q_state.get_mut(views.diamond_center) {
        tc.0 = if sel == 4 { hot } else { dim };
    }
    // —— 小窗层（节流） ——
    let font: Handle<Font> = font_assets.0.clone();
    let alive_others = session
        .with_engine(|e, seat| (0..e.players.len()).filter(|&p| p != seat && e.players[p].alive).count())
        .unwrap_or(0);
    // 新局（帧号倒退）→ 网格重置为开局最小可容（8 人局 = 2×2）
    if s.n < thumbs.last_frame {
        thumbs.n = 0;
    }
    thumbs.last_frame = s.n;
    let mut want_n = if thumbs.n == 0 { fit_grid(alive_others.max(1)) } else { thumbs.n };
    // 淘汰降级：跌出 K-2 级容量 2(K-2)² → 降一级（恒 ≥ 2×2）
    while want_n > 2 && alive_others < 2 * (want_n - 2) * (want_n - 2) {
        want_n -= 1;
    }
    if want_n != thumbs.n {
        thumbs.n = want_n;
        rebuild_thumbs(&mut cmd, &font, &session, &mut thumbs, half_w);
        thumbs.last_refresh = None;
    }
    let due = match thumbs.last_refresh {
        None => true,
        Some(t) => t.elapsed().as_secs_f64() >= 1.0 / THUMB_HZ,
    };
    if due {
        thumbs.last_refresh = Some(Instant::now());
        let cursor_pid = queue.cursor.and_then(|c| thumbs.views.get(c)).map(|tv| tv.pid);
        for tv in thumbs.views.iter() {
            let Some(snap) = session.with_engine(|e, _| e.snapshot(tv.pid)) else { continue };
            let dead = !snap.alive_self;
            let h = stack_height(&snap);
    let is_target = if attackers_mode {
        s.targeted_by.contains(&tv.pid)
    } else {
        s.target == Some(tv.pid)
    };
    let is_targeted = s.targeted_by.contains(&tv.pid);            // 背景填充 = 危险度/死亡灰；边缘线 = 瞄准关系（淡蓝/红/紫）
            if let Ok((_, mut sp, _)) = vq.get_mut(tv.bg) {
                sp.color = danger_color(h, dead);
            }
            if let Ok((mut v, mut sp, _)) = vq.get_mut(tv.edge) {
                if dead {
                    *v = Visibility::Hidden;
                } else if is_target && is_targeted {
                    *v = Visibility::Visible;
                    sp.color = Color::srgb(0.75, 0.35, 1.0);
                } else if is_target {
                    *v = Visibility::Visible;
                    sp.color = Color::srgb(0.45, 0.75, 1.0);
                } else if is_targeted {
                    *v = Visibility::Visible;
                    sp.color = Color::srgb(1.0, 0.32, 0.32);
                } else {
                    *v = Visibility::Hidden;
                }
            }
            // 棋盘（死亡冻结：快照即死亡瞬间状态）
            show_grid(&mut vq, &tv.cells, W, &|r, c| if snap.board[r][c] != 0 { Some(snap.board[r][c]) } else { None });
            // 活动块 + 垃圾条（单列堆叠）
            hide(&mut vq, &tv.active);
            hide(&mut vq, &tv.meter);
            if let Some(a) = snap.active {
                for (i, &(dx, dy)) in a.kind.cells(a.rot).iter().enumerate() {
                    let Some(&e) = tv.active.get(i) else { continue };
                    let row = a.y_milli.div_euclid(1000) + dy as i64;
                    let col = a.x as i64 + dx as i64;
                    if (0..H as i64).contains(&row) && (0..W as i64).contains(&col) {
                        let px = tv.origin.x + (col as f32 - (W as f32 - 1.0) / 2.0) * MINI;
                        let py = tv.origin.y - (row as f32 - (H as f32 - 1.0) / 2.0) * MINI;
                        place(&mut vq, e, Vec2::new(px, py), kind_color(a.kind.id()), 1.0);
                    }
                }
            }
            if !dead {
                show_grid(&mut vq, &tv.meter, 1, &|r, _| {
                    let idx = (H - 1 - r) as u32;
                    if idx < snap.garbage_red {
                        // 同主棋盘：暗相位转暗红而非隐藏
                        Some(if red_on { 5 } else { 9 })
                    } else if idx < snap.garbage_red + snap.garbage_yellow {
                        Some(2)
                    } else if idx < snap.garbage_red + snap.garbage_yellow + snap.garbage_gray {
                        Some(8)
                    } else {
                        None
                    }
                });
            }
            // 灰蒙版 + 名次
            if let Ok((mut v, mut sp, _)) = vq.get_mut(tv.mask) {
                *v = if dead { Visibility::Visible } else { Visibility::Hidden };
                sp.color = Color::srgba(0.45, 0.45, 0.5, 0.65);
            }
            // 击杀数：>3 用「数字+★」避免一排星星溢出小窗
            set_text(
                &mut text_q,
                tv.badge,
                if snap.badge_units == 0 {
                    "".into()
                } else if snap.badge_units > 3 {
                    format!("{}★", snap.badge_units)
                } else {
                    "★".repeat(snap.badge_units as usize)
                },
            );
            set_text(&mut text_q, tv.rank, if dead { format!("#{}", snap.place.unwrap_or(0)) } else { "".into() });
            // 副操作光标：ID 标签高亮
            if let Ok(mut tc) = color_q_state.get_mut(tv.id) {
                tc.0 = if cursor_pid == Some(tv.pid) {
                    Color::srgb(1.0, 0.9, 0.4)
                } else {
                    Color::srgb(0.7, 0.75, 0.8)
                };
            }
        }
    }
    // —— 攻击连线：被瞄(targeted_by → 菱形 黄) + 本方瞄准(菱形 → target 蓝) ——
    // z=6：99p 缩略图（z≤3）之上、调试 HUD（z=50）之下；池化复用，多余槽隐藏。
    // 本方已出局时菱形被攻击盘蒙灰接管，不再画线。
    if s.alive_self {
        let dia = Vec2::new(0.0, -344.0);
        let mut segs: Vec<(Vec2, Vec2, Color)> = Vec::new();
        for &pid in &s.targeted_by {
            if let Some(p) = nail_pos(&thumbs, seat, pid) {
                segs.push((p, dia, Color::srgba(1.0, 0.85, 0.25, 0.30)));
            }
        }
        if let Some(t) = s.target {
            if t != seat {
                if let Some(p) = nail_pos(&thumbs, seat, t) {
                    segs.push((dia, p, Color::srgba(0.35, 0.60, 1.0, 0.30)));
                }
            }
        }
        segs.truncate(thumbs.atk_lines.len());
        for (i, &e) in thumbs.atk_lines.iter().enumerate() {
            let Some(&(p0, p1, c)) = segs.get(i) else {
                if let Ok((mut v, _, _)) = vq.get_mut(e) {
                    *v = Visibility::Hidden;
                }
                continue;
            };
            let d = p1 - p0;
            let len = d.length();
            if len < 8.0 {
                if let Ok((mut v, _, _)) = vq.get_mut(e) {
                    *v = Visibility::Hidden;
                }
                continue;
            }
            if let Ok((mut v, mut sp, mut tr)) = vq.get_mut(e) {
                *v = Visibility::Visible;
                sp.custom_size = Some(Vec2::new(len, 2.6));
                sp.color = c;
                tr.translation = Vec3::new((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0, 6.0);
                tr.rotation = Quat::from_rotation_z(d.y.atan2(d.x));
            }
        }
    }
    // —— 攻击光点：每 tick 的 recent_attacks 生成 0.3s 飞行淡出光点；按 tick 号去重，
    // 池化复用（不 spawn/despawn），无事件时槽位保持隐藏。——
    {
        let now = Instant::now();
        let evs: Vec<AttackEvent> =
            session.with_engine(|e, _| e.recent_attacks().to_vec()).unwrap_or_default();
        if !evs.is_empty() && s.n != thumbs.last_tick {
            thumbs.last_tick = s.n;
            for ev in &evs {
                let (Some(p0), Some(p1)) = (nail_pos(&thumbs, seat, ev.from), nail_pos(&thumbs, seat, ev.to))
                else {
                    continue;
                };
                if (p1 - p0).length() < 8.0 {
                    continue;
                }
                let Some(slot) = thumbs.slots.iter_mut().find(|s| s.is_none()) else {
                    break; // 池满：丢最后续的事件（1 tick 最多 ~10 发）
                };
                // 来袭（to=本方）红、自发（from=本方）金、旁观（双方均非本方）灰且明显更暗
                let (color, base) = if ev.from != seat && ev.to != seat {
                    (Color::srgb(0.72, 0.75, 0.82), 0.15)
                } else if ev.to == seat {
                    (Color::srgb(1.0, 0.42, 0.30), 0.30 + ev.rows.min(20) as f32 / 20.0 * 0.30)
                } else {
                    (Color::srgb(1.0, 0.78, 0.28), 0.30 + ev.rows.min(20) as f32 / 20.0 * 0.30)
                };
                *slot = Some(DotFx {
                    from: p0,
                    to: p1,
                    born: now,
                    rows: ev.rows,
                    color,
                    base,
                });
            }
        }
        let atk_dots = thumbs.atk_dots.clone();
        for (i, slot) in thumbs.slots.iter_mut().enumerate() {
            let Some(&e) = atk_dots.get(i) else { continue };
            let Some(d) = slot else { continue }; // 空闲槽：释放时已隐藏，保持原状即可
            let el = (now.duration_since(d.born).as_secs_f32() / 0.3).min(1.0);
            if el >= 1.0 {
                if let Ok((mut v, _, _)) = vq.get_mut(e) {
                    *v = Visibility::Hidden;
                }
                *slot = None;
                continue;
            }
            let eased = el * (2.0 - el); // 缓出：起步快、落点稳
            let p = d.from + (d.to - d.from) * eased;
            let dir = d.to - d.from;
            let r = (4.5 + d.rows as f32 * 0.4).min(11.0); // 亮度/大小随攻击力(1..=20)增加
            if let Ok((mut v, mut sp, mut tr)) = vq.get_mut(e) {
                *v = Visibility::Visible;
                sp.custom_size = Some(Vec2::new(r * 2.4, r * 1.9));
                sp.color = d.color.with_alpha(d.base * (1.0 - el) * (1.0 - el));
                tr.translation = Vec3::new(p.x, p.y, 5.0);
                tr.rotation = Quat::from_rotation_z(dir.y.atan2(dir.x));
            }
        }
    }
}

/// pid → 连线/光点端点：本方 = 攻击面板中心菱形 (0,-344)；其他 = 缩略图中心（无缩略图则 None）。
fn nail_pos(thumbs: &Thumbs, seat: usize, pid: usize) -> Option<Vec2> {
    if pid == seat {
        Some(Vec2::new(0.0, -344.0))
    } else {
        thumbs.views.iter().find(|t| t.pid == pid).map(|t| t.origin)
    }
}

/// 网络线程：连接服务器，维护本地镜像（事件流重演），转发输入/重启
#[cfg(not(target_arch = "wasm32"))]
fn version_ok(protocol: u64, ruleset: &str) -> bool {
    protocol == PROTOCOL as u64 && ruleset == t99_core::RULESET_ID
}

fn net_thread(
    url: String,
    id: String,
    key: String,
    observer: bool,
    spectate: usize,
    base_ruleset: Ruleset,
    mirror: std::sync::Arc<std::sync::Mutex<MirrorState>>,
    status: std::sync::Arc<std::sync::Mutex<String>>,
    rx: std::sync::mpsc::Receiver<NetCmd>,
    stats: std::sync::Arc<std::sync::Mutex<NetStats>>,
) {
    use tungstenite::{client::IntoClientRequest, handshake::client::Response, Message, WebSocket};
    let role = if observer { "observer" } else { "player" };
    let full = format!("{url}/ws?id={id}&key={key}&role={role}&spectate={spectate}");
    let req = match full.into_client_request() {
        Ok(r) => r,
        Err(e) => {
            *status.lock().unwrap() = format!("URL 无效: {e}");
            return;
        }
    };
    let (mut ws, _resp): (WebSocket<_>, Response) = match tungstenite::connect(req) {
        Ok(x) => x,
        Err(e) => {
            *status.lock().unwrap() = format!("连接失败: {e}");
            return;
        }
    };
    *status.lock().unwrap() = "已连接，等待对局…".into();
    stats.lock().unwrap().connected = true;
    {
        let mut st = stats.lock().unwrap();
        st.player_id = id.clone();
    }
    if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() {
        // 2ms 只是为了让 rx 里的 input 及时写回私有 frame；P1 的 session core 会用
        // 独立写线程替掉这里的读超时轮询。服务端已经没有这个轮询。
        let _ = tcp.set_read_timeout(Some(std::time::Duration::from_millis(2)));
    }
    let handle_msg = |t: &str, mirror: &std::sync::Arc<std::sync::Mutex<MirrorState>>, status: &std::sync::Arc<std::sync::Mutex<String>>, expected: &mut Option<u64>| -> bool {
        let v: serde_json::Value = match serde_json::from_str(&t) { Ok(v) => v, Err(_) => return true };
        match v["type"].as_str().unwrap_or("") {
            "hello" => {
                *expected = None;
                {
                    let mut st = stats.lock().unwrap();
                    st.match_id = v["match_id"].as_u64().unwrap_or(0);
                    st.connected = true;
                    st.server_tps = 0.0;
                    st.last_tick_n = 0;
                    st.last_tick_at = None;
                }
                let players = v["players"].as_u64().unwrap_or(2) as usize;
                let seed = v["seed"].as_u64().unwrap_or(0);
                let seat = v["seat"].as_i64().unwrap_or(-1);
                // 规则参数随 hello 下发，镜像与权威引擎必然一致
                let mut ruleset = base_ruleset.clone();
                let tick_hz = v["tick_hz"]
                    .as_u64()
                    .or_else(|| v["frame_hz"].as_u64())
                    .unwrap_or(DEFAULT_TICK_HZ as u64) as u32;
                // tick 速率以服务器 hello 为准，镜像引擎的帧长必须跟着服务器
                ruleset.tick_hz = tick_hz.max(1);
                let tl = v["time_limit_secs"].as_u64().unwrap_or(0);
                if tl > 0 {
                    ruleset.match_ticks = (tl as i64) * 1000 * TMS;
                }
                ruleset.stall_accel = v["stall_accel"].as_bool().unwrap_or(true);
                let mut g = mirror.lock().unwrap();
                g.engine = Some(Match::new(MatchConfig { players, seed, ruleset }));
                g.my_seat = if seat >= 0 { seat as usize } else { spectate };
                g.tick_hz = tick_hz;
                g.pending_frame_n = 0;
                g.last_input_n = 0;
                drop(g);
                *status.lock().unwrap() = format!("对局 #{} 进行中", v["match"].as_u64().unwrap_or(0));
            }
            "frame" => {
                // 私有帧只用于输入时序：镜像仍按 tick 推进，n 原样回填到 input
                if let Some(n) = v["n"].as_u64() {
                    mirror.lock().unwrap().pending_frame_n = n;
                }
            }
            "tick" => {
                // 服务器 tick 速率：按 tick 序号差 / 到达时间差做 EMA
                let tn = v["n"].as_u64();
                if let Some(tn) = tn {
                    let now = Instant::now();
                    let mut st = stats.lock().unwrap();
                    if st.last_tick_n > 0 && tn > st.last_tick_n {
                        if let Some(prev) = st.last_tick_at {
                            let dt = now.duration_since(prev).as_secs_f32();
                            if dt > 0.0 {
                                let inst = (tn - st.last_tick_n) as f32 / dt;
                                st.server_tps = if st.server_tps <= 0.0 {
                                    inst
                                } else {
                                    st.server_tps * 0.8 + inst * 0.2
                                };
                            }
                        }
                    }
                    st.last_tick_n = tn;
                    st.last_tick_at = Some(now);
                }
                // 序号连续性校验：重复/缺帧即失步（镜像多跑帧会导致垃圾提前灌满等假象）
                if let (Some(tn), Some(exp)) = (tn, *expected) {
                    if tn != exp {
                        eprintln!("[mirror] 失步: 收到 tick n={tn} 期望 {exp}（本地镜像将重建）");
                        *status.lock().unwrap() = "镜像失步，重连补史…".into();
                        return false;
                    }
                }
                let mut g = mirror.lock().unwrap();
                if let Some(e) = g.engine.as_mut() {
                    let n = e.players.len();
                    let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; n];
                    if let Some(arr) = v["in"].as_array() {
                        for pair in arr {
                            let pid = pair[0].as_u64().unwrap_or(0) as usize;
                            if pid >= n { continue; }
                            if let Some(evs) = pair[1].as_array() {
                                inputs[pid] = evs.iter().filter_map(|e| {
                                    let t = e.get(0)?.as_u64()? as u32;
                                    let k = e.get(1)?.as_str()?;
                                    let key = if k == "target_player" {
                                        Key::TargetPlayer(e.get(2)?.as_u64()? as u16)
                                    } else {
                                        Key::from_name(k)?
                                    };
                                    Some(TimedKey { t_us: t, key })
                                }).collect();
                            }
                        }
                    }
                    e.frame(&inputs);
                }
                *expected = Some(tn.unwrap_or(0) + 1);
            }
            "warn" => {
                let code = v["code"].as_str().unwrap_or("?");
                let dropped = v["dropped"].as_u64().unwrap_or(0);
                eprintln!("[warn] {code} dropped={dropped}");
            }
            "result" => {
                let reason = v["reason"].as_str().unwrap_or("?");
                let mut st = format!("对局结束[{reason}]：");
                if let Some(arr) = v["places"].as_array() {
                    st.push_str(&arr.iter().map(|p| format!("#{}→{}", p[0], p[1])).collect::<Vec<_>>().join(" "));
                }
                st.push_str("（按 R 重开）");
                *status.lock().unwrap() = st;
            }
            "pong" => {
                let t = v["t"].as_u64().unwrap_or(0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let mut st = stats.lock().unwrap();
                if now >= t && t > 0 {
                    st.rtt_ms = (now - t) as u32;
                }
                st.pongs += 1;
                st.loss_pct = if st.pings == 0 { 0 } else { (st.pings - st.pongs) * 100 / st.pings };
                st.connected = true;
            }
            "error" => {
                *status.lock().unwrap() = format!("服务器错误: {}", v["code"].as_str().unwrap_or("?"));
            }
            _ => {}
        }
        true
    };
    let mut expected: Option<u64> = None;
    let mut last_ping = Instant::now();
    'conn: loop {
        if last_ping.elapsed().as_millis() >= 1000 {
            last_ping = Instant::now();
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            stats.lock().unwrap().pings += 1;
            let _ = ws.send(Message::Text(serde_json::json!({"type": "ping", "t": t}).to_string().into()));
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                let text = t.to_string();
                // hello 是版本闸门：protocol / ruleset 任一不匹配，立即拒绝并关闭。
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if v["type"] == "hello" {
                        let p = v["protocol"].as_u64().unwrap_or(0);
                        let r = v["ruleset"].as_str().unwrap_or("");
                        if !version_ok(p, r) {
                            let msg = serde_json::json!({
                                "type": "error",
                                "code": "unsupported_version",
                                "protocol": PROTOCOL,
                                "ruleset": t99_core::RULESET_ID,
                            });
                            let _ = ws.send(Message::Text(msg.to_string().into()));
                            *status.lock().unwrap() = format!(
                                "版本不匹配: 服务器 protocol={p} ruleset={r}，本机 protocol={PROTOCOL} ruleset={}",
                                t99_core::RULESET_ID
                            );
                            stats.lock().unwrap().connected = false;
                            return;
                        }
                        // 私有 hello（带 frame_hz）是 NetPlayerAdapter 的 setup；
                        // 它不含 match/seed，不能交给 mirror 的 hello 分支。
                        if v.get("frame_hz").is_some() {
                            let _ = ws.send(Message::Text(r#"{"type":"ready"}"#.into()));
                            continue;
                        }
                    }
                }
                if !handle_msg(&text, &mirror, &status, &mut expected) {
                    break 'conn; // 失步 → 触发重连
                }
            }
            Ok(Message::Ping(d)) => { let _ = ws.send(Message::Pong(d)); }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                *status.lock().unwrap() = format!("连接断开: {e}");
                stats.lock().unwrap().connected = false;
                break 'conn;
            }
        }
        while let Ok(cmd) = rx.try_recv() {
            let msg = match cmd {
                NetCmd::Input { n, evs } => serde_json::json!({
                    "type": "input",
                    "n": n,
                    "events": evs.iter().map(|t| match t.key {
                        Key::TargetPlayer(v) => serde_json::json!([t.t_us, t.key.name(), v]),
                        _ => serde_json::json!([t.t_us, t.key.name()]),
                    }).collect::<Vec<_>>()
                }),
                NetCmd::Restart => serde_json::json!({"type": "restart"}),
            };
            if ws.send(Message::Text(msg.to_string().into())).is_err() {
                *status.lock().unwrap() = "发送失败，连接断开".into();
                break 'conn;
            }
        }
    }
    // —— 失步/断线后：观战端重连补史自愈；玩家端无法中途入场，放弃并保持最终画面 ——
    if observer {
        *status.lock().unwrap() = "重连中…".into();
        std::thread::sleep(std::time::Duration::from_millis(400));
        net_thread(
            url,
            id,
            key,
            observer,
            spectate,
            base_ruleset.clone(),
            mirror,
            status,
            rx,
            Default::default(),
        );
    } else {
        *status.lock().unwrap() = "已断开（保持最终画面）".into();
    }
}

fn main() {
    let mut seed = 42u64;
    let mut n_bots = 3usize;
    let mut tier = "advanced".to_string();
    let mut controls_override: Option<String> = None;
    let mut server: Option<String> = None;
    let mut my_id = "player1".to_string();
    let mut my_key = "k".to_string();
    let mut observer = false;
    let mut observer_explicit = false;
    let mut spectate = 0usize;
    let mut spectate_explicit = false;
    let mut config_path: Option<String> = None;
    let mut local = false;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => { seed = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(seed); i += 1; }
            "--bots" => { n_bots = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(n_bots); i += 1; }
            "--tier" => { tier = args.get(i + 1).cloned().unwrap_or(tier); i += 1; }
            "--controls" => {
                controls_override = args.get(i + 1).cloned();
                i += 1;
            }
            "--server" => { server = args.get(i + 1).cloned(); i += 1; }
            "--id" => { my_id = args.get(i + 1).cloned().unwrap_or(my_id); i += 1; }
            "--key" => { my_key = args.get(i + 1).cloned().unwrap_or(my_key); i += 1; }
            "--observer" => { observer = true; observer_explicit = true; }
            "--spectate" => { spectate = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(spectate); spectate_explicit = true; i += 1; }
            "--config" => { config_path = args.get(i + 1).cloned(); i += 1; }
            "--local" => { local = true; }
            _ => {}
        }
        i += 1;
    }
    let cfg = t99_config::load_client(config_path.as_deref().map(std::path::Path::new))
        .unwrap_or_else(|e| {
            eprintln!("[config] {e}");
            std::process::exit(2);
        });
    if !observer_explicit {
        observer = cfg.client.observer;
    }
    if !spectate_explicit {
        spectate = cfg.client.spectate;
    }
    if local {
        server = None;
    } else if server.is_none() && !cfg.client.server.is_empty() {
        server = Some(cfg.client.server.clone());
    }
    if cfg.display.framerate > 0 {
        eprintln!(
            "[config] framerate={} 尚未实现 limiter；当前仍不限制 FPS",
            cfg.display.framerate
        );
    }
    let rc = t99_config::load_ruleset(Some(std::path::Path::new(&cfg.ruleset_file)))
        .unwrap_or_else(|e| {
            eprintln!("[ruleset] {e}");
            std::process::exit(2);
        });
    if rc.ruleset.id != t99_core::RULESET_ID {
        eprintln!(
            "[ruleset] id 不匹配: file={} compiled={}；修改规则集后必须同步重编 RULESET_ID",
            rc.ruleset.id,
            t99_core::RULESET_ID
        );
        std::process::exit(2);
    }
    let base_ruleset = rc.to_ruleset().unwrap_or_else(|e| {
        eprintln!("[ruleset] {e}");
        std::process::exit(2);
    });
    let timing = InputTiming {
        das_ticks: cfg.control.das_ticks,
        arr_ticks: cfg.control.arr_ticks,
        dual_charge: cfg.control.dual_charge,
    };
    let display = DisplaySettings {
        ghost: cfg.display.ghost,
        shadow_locked: cfg.display.shadow_locked,
        opponent_framerate: cfg.display.opponent_framerate,
        result_hold_secs: cfg.display.result_hold_secs,
    };
    let keymap = match controls_override.as_deref() {
        Some(name) => Keymap::preset(name),
        None => Keymap::from_control(&cfg.control),
    }
    .unwrap_or_else(|e| {
        eprintln!("[config] {e}");
        std::process::exit(2);
    });
    let session = if let Some(url) = server {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mirror: std::sync::Arc<std::sync::Mutex<MirrorState>> = Default::default();
            let status: std::sync::Arc<std::sync::Mutex<String>> = std::sync::Arc::new(std::sync::Mutex::new("连接中…".into()));
            let (tx, rx) = std::sync::mpsc::channel();
            let (m2, st2, url2, id2, key2) = (mirror.clone(), status.clone(), url.clone(), my_id.clone(), my_key.clone());
            let stats: std::sync::Arc<std::sync::Mutex<NetStats>> = Default::default();
            let stats2 = stats.clone();
            let ruleset2 = base_ruleset.clone();
            std::thread::spawn(move || {
                net_thread(url2, id2, key2, observer, spectate, ruleset2, m2, st2, rx, stats2)
            });
            Session::Net { mirror, tx, observer, status, stats }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (my_id, my_key, observer, spectate);
            eprintln!("wasm 构建暂不支持 --server（本地模式）");
            let n_bots = n_bots.clamp(1, 98);
            let mut bots: Vec<Option<BotAdapter>> = vec![None];
            for _ in 0..n_bots { bots.push(Some(BotAdapter::new(&tier))); }
            Session::Local { engine: Match::new(MatchConfig { players: 1 + n_bots, seed, ruleset: base_ruleset.clone() }), bots }
        }
    } else {
        let n_bots = n_bots.clamp(1, 98);
        let mut bots: Vec<Option<BotAdapter>> = vec![None];
        for _ in 0..n_bots {
            bots.push(Some(BotAdapter::new(&tier)));
        }
        Session::Local { engine: Match::new(MatchConfig { players: 1 + n_bots, seed, ruleset: base_ruleset.clone() }), bots }
    };
    let mut window = Window {
        title: "t99 · Arena Tetris".into(),
        resolution: (1600.0, 900.0).into(),
        ..default()
    };
    if !cfg!(target_arch = "wasm32") {
        // 默认不锁帧；vsync=true 时交给显示器刷新率。
        window.present_mode = if cfg.display.vsync {
            PresentMode::AutoVsync
        } else {
            PresentMode::Immediate
        };
    }
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(window),
            ..default()
        }))
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .init_resource::<HudMetrics>()
        .insert_resource(session)
        .insert_resource(InputQueue::default())
        .insert_resource(RepeatState::default())
        .insert_resource(timing)
        .insert_resource(display)
        .insert_resource(keymap)
        .add_systems(Startup, (load_ui_font, setup).chain())
        .add_systems(Update, (sync_tick_rate, poll_input, sync).chain())
        .add_systems(FixedUpdate, tick)
        .run();
}
