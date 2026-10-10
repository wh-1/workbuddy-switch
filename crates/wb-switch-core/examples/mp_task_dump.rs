//! 一次性诊断：抓指定 mp 任务的完整原始 JSON（desc / progress / accept_status）。
//! 用法：cargo run -p wb-switch-core --example mp_task_dump -- [task_code] [uid前缀]

use serde_json::Value;

use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{list_growth_tasks_mp, tasks_capable_accounts};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = args
        .first()
        .cloned()
        .unwrap_or_else(|| "Sequential_Tasks_5".to_string());
    let prefix = args.get(1).cloned().unwrap_or_default();
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
    let tasks = list_growth_tasks_mp(account).await.unwrap_or_default();
    match tasks
        .iter()
        .find(|t| t.get("task_code").and_then(Value::as_str) == Some(code.as_str()))
    {
        Some(task) => println!(
            "{}",
            serde_json::to_string_pretty(task).unwrap_or_default()
        ),
        None => {
            println!("未找到 {code}；当前 mp 列表：");
            for t in &tasks {
                println!(
                    "  {} accept={} progress={}",
                    t.get("task_code").and_then(Value::as_str).unwrap_or("?"),
                    t.get("accept_status").and_then(Value::as_str).unwrap_or("?"),
                    t.get("progress").map(|p| p.to_string()).unwrap_or_default()
                );
            }
        }
    }
}
