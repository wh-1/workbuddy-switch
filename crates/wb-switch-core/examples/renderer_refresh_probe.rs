//! 渲染层刷新探针（排障/回归用）：对**已在运行**的 WorkBuddy 调试端口执行一次刷新流程。
//!
//! 用法: cargo run -p wb-switch-core --example renderer_refresh_probe -- <port> <expectName>
//!
//! 例：`-- 44143 Elaine` ⇒ 徽章已是 Elaine，期望输出 `skipped`；
//!     `-- 44143 绝不存在的名字` ⇒ 期望 3 次点击后输出 `not-refreshed`（证明点击/判定链路通且不假成功）。

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(44143);
    let expect = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "Elaine".to_string());
    println!("[probe] port={port} expect={expect}");
    match wb_switch_core::modules::renderer_refresh::refresh_renderer_account(
        port,
        &expect,
        Duration::from_secs(5),
    ) {
        Ok(s) => println!("[probe] OK {s}"),
        Err(e) => println!("[probe] ERR {e}"),
    }
}
