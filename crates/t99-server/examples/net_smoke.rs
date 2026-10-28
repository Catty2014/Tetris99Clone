//! 联机冒烟：两个 WebSocket 玩家客户端连入 t99-server，
//! 各自本地镜像重演，结束时校验镜像结算与服务器广播一致。

use std::io::ErrorKind;
use std::sync::Arc;

use t99_core::{Key, Match, MatchConfig, Ruleset, TimedKey};
use tungstenite::Message;

fn run_client(url: &str, id: &str, key: &str, n: usize, stop: Arc<std::sync::atomic::AtomicBool>) -> (u64, Vec<(usize, u32)>) {
    let full = format!("{url}?id={id}&key={key}&role=player&spectate=0");
    let (mut ws, _) = tungstenite::connect(full).expect("连接失败");
    if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() {
        tcp.set_read_timeout(Some(std::time::Duration::from_millis(20))).ok();
    }
    let mut mirror: Option<Match> = None;
    let mut my_seat = 0usize;
    let mut frame_counter = 0u64;
    let mut places: Vec<(usize, u32)> = vec![];
    loop {
        let mut got = false;
        match ws.read() {
            Ok(Message::Text(t)) => {
                got = true;
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                match v["type"].as_str().unwrap_or("") {
                    "hello" => {
                        my_seat = v["seat"].as_i64().unwrap_or(0).max(0) as usize;
                        mirror = Some(Match::new(MatchConfig {
                            players: v["players"].as_u64().unwrap_or(2) as usize,
                            seed: v["seed"].as_u64().unwrap_or(0),
                            ruleset: Ruleset::default(),
                        }));
                        println!("[{id}] 入局 seat={my_seat}");
                    }
                    "tick" => {
                        if let Some(e) = mirror.as_mut() {
                            let players = e.players.len();
                            let mut inputs: Vec<Vec<TimedKey>> = vec![vec![]; players];
                            if let Some(arr) = v["in"].as_array() {
                                for pair in arr {
                                    let pid = pair[0].as_u64().unwrap_or(0) as usize;
                                    if pid >= players {
                                        continue;
                                    }
                                    inputs[pid] = pair[1]
                                        .as_array()
                                        .map(|evs| {
                                            evs.iter()
                                                .filter_map(|e| {
                                                    let t = e.get(0)?.as_u64()? as u32;
                                                    let k = e.get(1)?.as_str()?;
                                                    Some(TimedKey { t_us: t, key: Key::from_name(k)? })
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                }
                            }
                            e.frame(&inputs);
                        }
                        frame_counter = v["n"].as_u64().unwrap_or(0);
                        // 每 3 帧硬降一次（简单玩家）
                        if frame_counter % 3 == 0 {
                            let msg = serde_json::json!({"type":"input","events":[[0u32,"hard"]]});
                            let _ = ws.send(Message::Text(msg.to_string().into()));
                        }
                    }
                    "result" => {
                        places = v["places"]
                            .as_array()
                            .map(|a| a.iter().map(|p| (p[0].as_u64().unwrap_or(0) as usize, p[1].as_u64().unwrap_or(0) as u32)).collect())
                            .unwrap_or_default();
                        // 确定性校验：镜像状态哈希 == 服务器广播哈希
                        // （time_limit 的存活者名次由服务器 ranking 补全，不在镜像里比对）
                        let srv_hash = v["hash"].as_u64().unwrap_or(0);
                        if let Some(e) = mirror.as_ref() {
                            let my_hash = e.hash_state();
                            let dead_ok = {
                                let mut mplace = vec![None; e.players.len()];
                                for row in e.final_table() {
                                    mplace[row.pid] = row.place;
                                }
                                places.iter().all(|(pid, pl)| mplace[*pid] != Some(*pl) || e.players[*pid].alive == false)
                            };
                            println!(
                                "[{id}] 结束 frame={frame_counter} places={places:?} hash {}一致",
                                if my_hash == srv_hash { "" } else { "不" }
                            );
                            assert_eq!(my_hash, srv_hash, "[{id}] 镜像状态与服务器不一致");
                            assert!(dead_ok, "[{id}] 死亡名次不一致");
                        }
                        return (frame_counter, places);
                    }
                    "error" => {
                        panic!("[{id}] 服务器拒绝: {}", v["code"].as_str().unwrap_or("?"));
                    }
                    _ => {}
                }
            }
            Ok(Message::Ping(d)) => {
                let _ = ws.send(Message::Pong(d));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => panic!("[{id}] 连接错误: {e}"),
        }
        if !got && stop.load(std::sync::atomic::Ordering::SeqCst) {
            return (frame_counter, places);
        }
        let _ = n;
    }
}

fn main() {
    let url = std::env::args().nth(1).unwrap_or_else(|| "ws://127.0.0.1:8138/ws".into());
    let key = std::env::args().nth(2).unwrap_or_else(|| "t99".into());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx1, rx1) = std::sync::mpsc::channel();
    let (tx2, rx2) = std::sync::mpsc::channel();
    let url1 = url.clone();
    let key1 = key.clone();
    let stop1 = stop.clone();
    let t1 = std::thread::spawn(move || {
        let r = run_client(&url1, "p1", &key1, 1, stop1);
        let _ = tx1.send(r);
    });
    let stop2 = stop.clone();
    let t2 = std::thread::spawn(move || {
        let r = run_client(&url, "p2", &key, 2, stop2);
        let _ = tx2.send(r);
    });
    let (f1, p1r) = rx1.recv().expect("p1 未返回");
    let (f2, p2r) = rx2.recv().expect("p2 未返回");
    t1.join().ok();
    t2.join().ok();
    assert_eq!(p1r, p2r, "两端结算应一致");
    println!("冒烟通过: frame={f1}/{f2} places={p1r:?}");
}
