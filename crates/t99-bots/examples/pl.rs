fn main() {
    let empty = [[0u8; 10]; 20];
    for k in ["I","O"] {
        let kind = t99_core::Kind::from_ch(k.chars().next().unwrap()).unwrap();
        let ps = t99_bots::placements(&empty, kind);
        println!("{}: {} placements", k, ps.len());
        for r in 0..4 {
            let c: Vec<i32> = ps.iter().filter(|p| p.rot == r).map(|p| p.col).collect();
            if !c.is_empty() { println!("  rot{} cols {:?}", r, c); }
        }
    }
}
