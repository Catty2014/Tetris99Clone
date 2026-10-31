//! NullBot：什么都不做（地板校准）。

use super::Bot;
use t99_core::{Snapshot, TimedKey};

pub struct NullBot;

impl Bot for NullBot {
    fn name(&self) -> &'static str {
        "null"
    }
    fn decide(&mut self, _s: &Snapshot) -> Vec<TimedKey> {
        vec![]
    }
}
