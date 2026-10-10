//! 诊断/运维用：对指定账号立即执行一次成长任务家族（纯上报/领取类），
//! 走 `growth_tasks::run_task_family` 全流程（accept → 补上报 → 回读 → claim）。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example family_run -- <uid前缀>
//!
//! 例：
//!   cargo run -p wb-switch-core --example family_run -- 9b0f1e5a
//!
//! 只对命中的单个账号执行；写动作间 ≥1s 频控，不可伪造任务永不触碰。

use serde_json::Value;

use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{run_task_family, tasks_capable_accounts};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let prefix = std::env::args().nth(1).unwrap_or_default();
    if prefix.is_empty() {
        eprintln!("用法: family_run <uid前缀>");
        std::process::exit(2);
    }
    let capable = tasks_capable_accounts(load_accounts());
    let matched: Vec<&Value> = capable
        .iter()
        .filter(|account| {
            account
                .get("uid")
                .and_then(Value::as_str)
                .is_some_and(|uid| uid.starts_with(&prefix))
        })
        .collect();
    match matched.len() {
        0 => {
            eprintln!("未找到 uid 前缀为 {prefix} 的可跑账号");
            std::process::exit(1);
        }
        1 => {}
        n => {
            eprintln!("前缀 {prefix} 命中 {n} 个账号，请加长前缀");
            std::process::exit(1);
        }
    }
    let account = matched[0];
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("?");
    eprintln!("family_run uid={uid} 开始…");
    // 手动运维口不读当日缓存 ⇒ 熔断 prior 传空（同任务连错也不会被跳过）
    let prior_tasks = serde_json::json!({});
    let outcome = run_task_family(account, &prior_tasks).await;
    println!("{}", serde_json::to_string_pretty(&outcome).unwrap_or_default());
}
