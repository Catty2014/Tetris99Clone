use t99_bots::BotAdapter;
use t99_core::{Match, MatchConfig, Ruleset};
use t99_proto::PlayerAdapter;

fn main() {
    let mut m = Match::new(MatchConfig { players: 1, seed: 5, ruleset: Ruleset::default() });
    let mut bot = BotAdapter::new("basic");
    for n in 0..30u64 {
        let snap = m.snapshot(0);
        let a = snap.active;
        let inputs = vec![bot.act(&snap)];
        println!("frame {n}: active={:?} -> events={:?}", a.map(|x| (x.kind.ch().to_string(), x.rot, x.x, x.y_milli)),
            inputs[0].iter().map(|t| (t.t_us, t.key.name())).collect::<Vec<_>>());
        m.frame(&inputs);
    }
}
