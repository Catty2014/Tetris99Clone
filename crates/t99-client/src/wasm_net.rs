// Code in this module PARTIALLY or FULLY utilized AI Coding Agent
//! 浏览器端 WebSocket 传输。
//!
//! 与 native 的 `net_thread` 共用同一套消息处理（`handle_server_msg`）和本地镜像重演，
//! 只把「阻塞读线程 + mpsc」换成「WebSocket 回调 + 每帧 pump」。
//! 浏览器是单线程：回调不能跨线程，所以状态放在 `Rc` + `thread_local` 里。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, WebSocket};

use crate::{epoch_ms, handle_server_msg, net_cmd_json, version_ok, MirrorState, NetCmd, NetStats, PROTOCOL};

pub struct Ctx {
    pub ws: RefCell<Option<WebSocket>>,
    pub mirror: Arc<Mutex<MirrorState>>,
    pub status: Arc<Mutex<String>>,
    pub stats: Arc<Mutex<NetStats>>,
    pub base_ruleset: t99_core::Ruleset,
    pub spectate: usize,
    pub observer: bool,
    pub url: String,
    pub expected: RefCell<Option<u64>>,
    /// 断线/失步后置位；observer 由 pump 负责重连补史
    pub reconnect: Cell<bool>,
    /// 已接收未处理的服务器消息。onmessage 只入队，pump 每帧一次排空——
    /// 浏览器把回调与 Bevy 帧放在同一事件循环里，刷新慢时回调会排队，
    /// 让画面落后于实时；排空（while 一次性处理完积压）保证 UI 等待计算，
    /// 计算永远在渲染之前追平到最新帧。
    pub mq: RefCell<VecDeque<String>>,
}

struct Pump {
    ctx: Rc<Ctx>,
    rx: std::sync::mpsc::Receiver<NetCmd>,
    last_ping: crate::Instant,
    retry_at: Option<crate::Instant>,
}

thread_local! {
    static PUMP: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

fn send_text(ctx: &Ctx, text: &str) {
    if let Some(ws) = ctx.ws.borrow().as_ref() {
        let _ = ws.send_with_str(text);
    }
}

fn close(ctx: &Ctx) {
    if let Some(ws) = ctx.ws.borrow().as_ref() {
        let _ = ws.close();
    }
}

fn connect(ctx: &Rc<Ctx>) {
    // 重连换连接：旧连接排队中的消息已无意义，清掉避免与重建的镜像错位。
    ctx.mq.borrow_mut().clear();
    let ws = match WebSocket::new(&ctx.url) {
        Ok(w) => w,
        Err(_) => {
            *ctx.status.lock().unwrap() = "WebSocket 创建失败".into();
            ctx.reconnect.set(true);
            return;
        }
    };

    let c = ctx.clone();
    let onmsg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let Some(text) = e.data().as_string() else { return };
        // hello 是版本闸门：protocol / ruleset 任一不匹配，立即拒绝并关闭。
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if v["type"] == "hello" {
                let p = v["protocol"].as_u64().unwrap_or(0);
                let r = v["ruleset"].as_str().unwrap_or("");
                if !version_ok(p, r) {
                    *c.status.lock().unwrap() = format!(
                        "版本不匹配: 服务器 protocol={p} ruleset={r}，本机 protocol={PROTOCOL} ruleset={}",
                        t99_core::RULESET_ID
                    );
                    c.stats.lock().unwrap().connected = false;
                    send_text(
                        &c,
                        &serde_json::json!({
                            "type": "error",
                            "code": "unsupported_version",
                            "protocol": PROTOCOL,
                            "ruleset": t99_core::RULESET_ID,
                        })
                        .to_string(),
                    );
                    close(&c);
                    return;
                }
                // 私有 hello（带 frame_hz）是 NetPlayerAdapter 的 setup；不交给镜像。
                if v.get("frame_hz").is_some() {
                    send_text(&c, r#"{"type":"ready"}"#);
                    return;
                }
            }
        }
        // 只入队，不在浏览器回调里推进镜像：回调与 Bevy 帧同事件循环，
        // 积压的推进推迟到 pump，由单帧 while 排空追平，慢刷新下 UI 才等得动计算。
        c.mq.borrow_mut().push_back(text);
    });
    ws.set_onmessage(Some(onmsg.as_ref().unchecked_ref()));
    onmsg.forget();

    let c = ctx.clone();
    let onopen = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        *c.status.lock().unwrap() = "已连接，等待对局…".into();
        c.stats.lock().unwrap().connected = true;
    });
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    let c = ctx.clone();
    let onclose = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        *c.status.lock().unwrap() = "连接断开".into();
        c.stats.lock().unwrap().connected = false;
        c.reconnect.set(true);
    });
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    *ctx.ws.borrow_mut() = Some(ws);
}

pub fn start(ctx: Rc<Ctx>, rx: std::sync::mpsc::Receiver<NetCmd>) {
    connect(&ctx);
    PUMP.with(|p| {
        *p.borrow_mut() = Some(Pump {
            ctx,
            rx,
            last_ping: crate::Instant::now(),
            retry_at: None,
        });
    });
}

/// 每帧由 Bevy 系统驱动：发送输入、每秒 ping、observer 断线重连补史。
pub fn pump_frame() {
    PUMP.with(|cell| {
        let mut opt = cell.borrow_mut();
        let Some(p) = opt.as_mut() else { return };

        if p.ctx.reconnect.get() {
            // 与 native 一致：观战端重连补史；玩家端无法中途入场，保持最终画面
            if p.ctx.observer {
                let now = crate::Instant::now();
                if p.retry_at.map_or(true, |t| now >= t) {
                    p.ctx.reconnect.set(false);
                    p.retry_at = Some(now + std::time::Duration::from_millis(400));
                    p.ctx.ws.borrow_mut().take();
                    connect(&p.ctx);
                }
            }
        } else {
            p.retry_at = None;
        }

        // —— 单帧追平：一次性排空本帧积压的全部服务器消息 ——
        // 每条消息立即推进镜像引擎，帧内 CPU 尽量给重演；排空后再渲染，
        // 画面即当前实时帧。失步（序号断裂）走重连补史，属于失真而非变慢。
        let mut exp = p.ctx.expected.borrow_mut();
        let retry = loop {
            let Some(text) = p.ctx.mq.borrow_mut().pop_front() else { break false };
            if !handle_server_msg(
                &text,
                &p.ctx.mirror,
                &p.ctx.status,
                &p.ctx.stats,
                &p.ctx.base_ruleset,
                p.ctx.spectate,
                &mut exp,
            ) {
                // 失步/版本不符：与旧回调行为一致，断开并重连；旧连接剩余消息作废。
                *p.ctx.status.lock().unwrap() = "镜像失步，重连补史…".into();
                p.ctx.mq.borrow_mut().clear();
                close(&p.ctx);
                break true;
            }
        };
        drop(exp);
        if retry {
            p.ctx.reconnect.set(true);
        }

        // —— 发送侧：本帧积压的输入指令一次发完 ——
        while let Ok(cmd) = p.rx.try_recv() {
            let msg = net_cmd_json(&cmd);
            let mut failed = false;
            if let Some(ws) = p.ctx.ws.borrow().as_ref() {
                failed = ws.send_with_str(&msg).is_err();
            }
            if failed {
                *p.ctx.status.lock().unwrap() = "发送失败，连接断开".into();
                p.ctx.reconnect.set(true);
            }
        }

        if p.last_ping.elapsed().as_millis() >= 1000 {
            p.last_ping = crate::Instant::now();
            {
                let mut st = p.ctx.stats.lock().unwrap();
                st.pings += 1;
                st.ping_sent_at = Some(crate::Instant::now());
                st.refresh_loss();
            }
            let msg = serde_json::json!({"type": "ping", "t": epoch_ms()}).to_string();
            if let Some(ws) = p.ctx.ws.borrow().as_ref() {
                let _ = ws.send_with_str(&msg);
            }
        }
    });
}
