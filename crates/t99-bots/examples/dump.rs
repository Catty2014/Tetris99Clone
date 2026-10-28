use t99_bots::BotAdapter;
use t99_core::{Match, MatchConfig, Ruleset};
use t99_proto::PlayerAdapter;

fn main() {
    let mut m = Match::new(MatchConfig { players: 1, seed: 5, ruleset: Ruleset::default() });
    let mut bot = BotAdapter::new("basic");
    for n in 0..10000u64 {
        let inputs = vec![bot.act(&m.snapshot(0))];
        m.frame(&inputs);
        if m.over.is_some() {
            println!("DIED at frame {n}");
            let s = m.snapshot(0);
            for r in 0..20 {
                println!("{}", s.board[r].iter().map(|&c| if c == 0 { '.' } else { '#' }).collect::<String>());
            }
            break;
        }
        if n == 100 || n == 150 || n == 180 {
            let s = m.snapshot(0);
            println!("== frame {n} pieces={} lines={}", s.pieces, s.lines);
            for r in 0..20 {
                if s.board[r].iter().any(|&c| c != 0) {
                    println!("{}", s.board[r].iter().map(|&c| if c == 0 { '.' } else { '#' }).collect::<String>());
                }
            }
        }
    }
}
