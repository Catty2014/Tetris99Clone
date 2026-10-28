use t99_bot_m::*;
fn main() {
    let mut g: Grid = [[0; 10]; 24];
    for c in 0..10 {
        if c != 4 { g[23][c] = 1; }
        if !(3..=5).contains(&c) { g[22][c] = 1; }
    }
    g[21][2] = 1;
    g[21][5] = 1;
    let mut board = Vec::new();
    for r in 4..24 {
        let s: String = (0..10).map(|c| if g[r][c] != 0 { '#' } else { '.' }).collect();
        board.push(s);
    }
    let fr: FrameIn = serde_json::from_value(serde_json::json!({
        "type":"frame","n":0,"player_id":0,"board":board,
        "active":{"t":"T","r":0,"x":3,"y_m":0},
        "next":["L"],"combo":-1,"b2b":false,
        "garbage":{"red":0,"yellow":0,"gray":0},
        "lock":{"ms_left":0,"resets":0},"lines":0,"sent":0,"pieces":0,
    })).unwrap();
    let d = choose(&g, &fr, &gm_profile());
    match d {
        Some(Decision::Place(c)) => println!("Place spin={} cl={} x={} y={} score={:.1}", c.spin, c.cleared, c.fin_x, c.fin_y, c.score),
        Some(Decision::Hold) => println!("Hold"),
        None => println!("None"),
    }
    // 根层 T 候选 top6 静态分
    let (danger, redline) = danger_state(&g, &fr, &gm_profile());
    let mut cands = candidates(&g, 'T', &fr, &gm_profile(), danger, redline);
    cands.sort_by(|a,b| b.score.partial_cmp(&a.score).unwrap());
    for c in cands.iter().take(6) {
        println!("  T cand spin={} cl={} x={} y={} sc={:.1}", c.spin, c.cleared, c.fin_x, c.fin_y, c.score);
    }
    // 决策耗时基准（随机堆盘面 × 200 次平均）
    {
        use std::time::Instant;
        let mut g2 = g;
        // 造一个中局盘面（半满随机）
        let mut seed = 12345u64;
        let mut rnd = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1); seed >> 33 };
        for r in 14..24 { for c in 0..10 { if rnd() % 100 < 55 { g2[r][c] = 1; } } }
        let fr2 = serde_json::from_value::<FrameIn>(serde_json::json!({
            "type":"frame","n":0,"player_id":0,
            "board": (0..20).map(|r| (0..10).map(|c| if g2[4+r][c] != 0 {"#"} else {"."}).collect::<String>()).collect::<Vec<_>>(),
            "active":{"t":"T","r":0,"x":3,"y_m":0},
            "next":["L","J","I","S","Z","T"],"combo":-1,"b2b":false,
            "garbage":{"red":0,"yellow":0,"gray":0},
            "lock":{"ms_left":0,"resets":0},"lines":0,"sent":0,"pieces":0,
        })).unwrap();
        let t0 = Instant::now();
        let n = 200;
        let mut picks = 0;
        for _ in 0..n { if let Some(Decision::Place(_)) = choose(&g2, &fr2, &gm_profile()) { picks += 1; } }
        let dt = t0.elapsed();
        println!("decide: {:.2}ms/次（{} 次有效）", dt.as_secs_f64()*1000.0/n as f64, picks);
    }
    // hold 分支（L）最优
    let mut cands = candidates(&g, 'L', &fr, &gm_profile(), danger, redline);
    cands.sort_by(|a,b| b.score.partial_cmp(&a.score).unwrap());
    if let Some(c) = cands.first() { println!("  L best sc={:.1}", c.score); }
}
