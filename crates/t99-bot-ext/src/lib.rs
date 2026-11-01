//! 公开档外部程序运行壳：basic / advanced 的 external 变体。
//!
//! 两种接入方式（与参赛提交程序同通道）：
//! - 子进程 stdio：逐行 JSON（hello → ready → (frame → input)* → result）
//! - WebSocket：`--connect ws://host:port --id <player_id> --key <key>`（lobby）
//!
//! 决策核与进程内 Bot 是同一份代码：收到的 frame 经 `snapshot_from_frame`
//! 还原为内存快照后直接交给 `t99_bots` 的 Bot，因此与 internal 档构造上等价。

use std::io::{BufRead, Write};

use t99_bots::Bot;
use t99_proto::{snapshot_from_frame, FrameMsg};
use t99_core::TimedKey;

/// 容忍区间与全部 t99 外部程序一致：读取方对可选字段用 serde default。
pub const PROTOCOL_MIN: u32 = 5;
pub const PROTOCOL_MAX: u32 = 8;

const HELP: &str = r#"t99-bot-basic / t99-bot-advanced — 公开档外部程序（参考实现/可修改陪练）

两种接入方式（缺省无参数 = 子进程 stdio 模式）:

1. 子进程模式（被 t99-server / t99-headless 以 ext:label=path 启动）
     stdin/stdout 单行 JSON：hello → ready → (frame → input)* → result
     示例（服务器端指定席位）:
       t99-headless run --players 2 --bots "ext:basic=./t99-bot-basic,basic"

2. WebSocket 玩家模式（自主接入 lobby 服务器，与参赛提交程序同通道）
     --connect URL   服务器地址，如 ws://127.0.0.1:8138（自动拼接 /ws 路径）
     --id ID         PlayerID（需通过服务器的身份校验）
     --key KEY       房间准入 key
     示例:
       t99-bot-basic --connect ws://127.0.0.1:8138 --id my-bot --key t99

说明:
  - 本程序的决策核与进程内 Bot（t99-bots）为同一份代码，可自由修改。
  - 帧响应预算 16.666ms（实际判定 16ms），连续 600 帧无有效响应判负。
  - 协议 v8；可选字段缺席按默认处理（容忍 5..=8）。
"#;

const HELP_EN: &str = r#"t99-bot-basic / t99-bot-advanced — public-tier external programs (reference implementation / modifiable sparring partner)

Two ways to connect (no arguments = subprocess stdio mode):

1. Subprocess mode (spawned by t99-server / t99-headless via ext:label=path)
     stdin/stdout single-line JSON: hello → ready → (frame → input)* → result
     Example (server-side seat spec):
       t99-headless run --players 2 --bots "ext:basic=./t99-bot-basic,basic"

2. WebSocket player mode (connect to a lobby server yourself; same channel as submissions)
     --connect URL   server address, e.g. ws://127.0.0.1:8138 (/ws path appended automatically)
     --id ID         PlayerID (must pass the server's identity check)
     --key KEY       room admission key
     Example:
       t99-bot-basic --connect ws://127.0.0.1:8138 --id my-bot --key t99

Notes:
  - The decision core is the same code as the in-process bots (t99-bots); modify freely.
  - Frame response budget 16.666ms (enforced at 16ms); 600 consecutive unanswered frames → loss.
  - Protocol v8; absent optional fields fall back to defaults (tolerates 5..=8).
"#;

fn lang_is_zh() -> bool {
    for k in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            if v.starts_with("zh") {
                return true;
            }
        }
    }
    false
}

pub fn main_entry(make: fn() -> Box<dyn Bot>) {
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
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", if lang_is_zh() { HELP } else { HELP_EN });
        std::process::exit(0);
    }
    if let (Some(url), Some(id), Some(key)) = (get("--connect"), get("--id"), get("--key")) {
        run_ws(make, &url, &id, &key);
    } else {
        run_stdio(make);
    }
}

/// TimedKey → 单行 input JSON（与 t99-proto InputMsg 语义一致）。
fn pack(n: u64, evs: &[TimedKey]) -> String {
    let mut out = format!(r#"{{"type":"input","n":{n},"events":["#);
    for (i, e) in evs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match e.key {
            t99_core::Key::TargetPlayer(v) => {
                out.push_str(&format!(r#"{{"t_us":{},"k":"target_player","v":{}}}"#, e.t_us, v));
            }
            _ => {
                out.push_str(&format!(r#"{{"t_us":{},"k":"{}"}}"#, e.t_us, e.key.name()));
            }
        }
    }
    out.push_str("]}");
    out
}

fn check_protocol(proto: u32, tag: &str) {
    if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&proto) {
        eprintln!("[t99-bot-{tag}] 协议不兼容: {proto}（支持 {PROTOCOL_MIN}..={PROTOCOL_MAX}）");
        std::process::exit(2);
    }
}

fn on_frame(bot: &mut Box<dyn Bot>, line: &str, send: &mut dyn FnMut(String)) {
    let Ok(fr) = serde_json::from_str::<FrameMsg>(line) else { return };
    let snap = snapshot_from_frame(&fr, fr.seat);
    let evs = bot.decide(&snap);
    send(pack(fr.n, &evs));
}

fn on_result(tag: &str, v: &serde_json::Value) -> bool {
    // 广播 result（无 place 字段）先到：跳过，等逐玩家 result
    if v["place"].is_null() {
        return false;
    }
    eprintln!(
        "[t99-bot-{tag}] place={:?} lines={} sent={} pieces={} ko={}",
        v["place"].as_u64(),
        v["lines"].as_u64().unwrap_or(0),
        v["sent"].as_u64().unwrap_or(0),
        v["pieces"].as_u64().unwrap_or(0),
        v["ko"].as_u64().unwrap_or(0),
    );
    true
}

/// 子进程模式：stdin/stdout 单行 JSON。
fn run_stdio(make: fn() -> Box<dyn Bot>) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut bot = make();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.is_empty() {
            break; // 管道关闭
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v["type"].as_str().unwrap_or("") {
            "hello" => {
                check_protocol(v["protocol"].as_u64().unwrap_or(0) as u32, "ext");
                let _ = writeln!(out, r#"{{"type":"ready"}}"#);
                let _ = out.flush();
            }
            "frame" => {
                let mut send = |s: String| {
                    let _ = writeln!(out, "{s}");
                    let _ = out.flush();
                };
                on_frame(&mut bot, &line, &mut send);
            }
            "result" => {
                if on_result("ext", &v) {
                    let _ = out.flush();
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    }
}

/// WS 玩家模式：连接 lobby 服务器（/ws?id=&key=）。
fn run_ws(make: fn() -> Box<dyn Bot>, url: &str, id: &str, key: &str) {
    use tungstenite::Message;
    let full = format!("{}/ws?id={id}&key={key}", url.trim_end_matches('/'));
    let req = tungstenite::client::IntoClientRequest::into_client_request(full.as_str())
        .expect("无效 URL");
    let mut ws = tungstenite::connect(req).expect("连接服务器失败").0;
    let mut bot = make();
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let line = t.to_string();
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                match v["type"].as_str().unwrap_or("") {
                    "hello" if v.get("frame_hz").is_some() => {
                        check_protocol(v["protocol"].as_u64().unwrap_or(0) as u32, "ext");
                        let _ = ws.send(Message::Text(r#"{"type":"ready"}"#.into()));
                    }
                    "hello" => {
                        eprintln!(
                            "[t99-bot-ext] 对局 {}（seat {}）",
                            v["match_id"].as_u64().unwrap_or(0),
                            v["seat"].as_i64().unwrap_or(-1)
                        );
                    }
                    "frame" => {
                        let mut send = |s: String| {
                            let _ = ws.send(Message::Text(s.into()));
                        };
                        on_frame(&mut bot, &line, &mut send);
                    }
                    "result" => {
                        if on_result("ext", &v) {
                            std::process::exit(0);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Ping(d)) => {
                let _ = ws.send(Message::Pong(d));
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[t99-bot-ext] 连接断开: {e}");
                std::process::exit(1);
            }
        }
    }
}
