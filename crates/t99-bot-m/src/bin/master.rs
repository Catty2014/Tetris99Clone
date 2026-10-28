//! t99-bot-master：Master 隐藏档外部程序（稳健攻守）。
//! 用法：由 t99-server/t99-headless 以 `ext:master=<path>` 席位启动，stdin/stdout 线协议。
fn main() {
    t99_bot_m::main_entry(t99_bot_m::master_profile());
}
