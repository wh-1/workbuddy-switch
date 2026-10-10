//! 诊断用：对指定账号做一次 accept 并打印原始响应 + 回读 accept_status。
//! 用法：cargo run -p wb-switch-core --example accept_debug -- <uid前缀> [task_code]

use serde_json::{json, Value};

use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{accept_once_raw, list_growth_tasks, tasks_capable_accounts};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prefix = args.first().cloned().unwrap_or_default();
    let code = args.get(2).cloned().unwrap_or_else(|| "skill_1".to_string());
    let capable = tasks_capable_accounts(load_accounts());
    let Some(account) = capable.iter().find(|account| {
        account
            .get("uid")
            .and_then(Value::as_str)
            .is_some_and(|uid| uid.starts_with(&prefix))
    }) else {
        eprintln!("未找到账号 {prefix}");
        std::process::exit(1);
    };
    // 直接复用核心私有路径：借用 run family 的公开面做不了，这里走最小重建——
    // 通过 family_run 同款域请求。为只读诊断，直接调 list + 打印账号形状。
    // `_uid`：仅回显用占位（CI 的 -D warnings 会把未使用变量升级为 error，2026-09-25）
    let _uid = account.get("uid").and_then(Value::as_str).unwrap_or("?");
    println!("uid: {_uid}");
    println!("账号形状 keys: {:?}", account.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()));
    println!("variant: {:?}", account.get("variant"));
    let tasks = list_growth_tasks(account).await.unwrap_or_default();
    println!("任务数: {}", tasks.len());
    if let Some(task) = tasks.iter().find(|t| t.get("task_code").and_then(Value::as_str) == Some(code.as_str())) {
        println!("{code} 原始: {}", serde_json::to_string_pretty(task).unwrap_or_default());
    } else {
        println!("{code} 不在下发列表");
    }
    println!("---- accept 原始响应 ----");
    let resp = accept_once_raw(account, &code).await;
    println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
    let tasks2 = list_growth_tasks(account).await.unwrap_or_default();
    if let Some(task2) = tasks2.iter().find(|t| t.get("task_code").and_then(Value::as_str) == Some(code.as_str())) {
        println!("回读 accept_status: {}", task2.get("accept_status").and_then(Value::as_str).unwrap_or("?"));
    }
    let _ = json!({});
}
