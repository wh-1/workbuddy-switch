//! 诊断用：对指定账号做一次 **mp 口径** accept 并打印原始响应 + 回读 accept_status。
//! 用途：Sequential 阶梯（mp 限定）accept 失败排查——web 口径列表里没有这些任务。
//! 用法：cargo run -p wb-switch-core --example mp_accept_debug -- <uid前缀> [task_code]
//! （task_code 默认 Sequential_Tasks_3）

use serde_json::Value;

use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{mp_accept_once_raw, list_growth_tasks_mp, tasks_capable_accounts};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prefix = args.first().cloned().unwrap_or_default();
    let code = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "Sequential_Tasks_3".to_string());
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
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("?");
    println!("uid: {uid}  task: {code}");
    let tasks = list_growth_tasks_mp(account).await.unwrap_or_default();
    match tasks
        .iter()
        .find(|t| t.get("task_code").and_then(Value::as_str) == Some(code.as_str()))
    {
        Some(task) => println!(
            "accept 前: status={} progress={}",
            task.get("accept_status").and_then(Value::as_str).unwrap_or("?"),
            task.get("progress").map(|p| p.to_string()).unwrap_or("null".into())
        ),
        None => println!("⚠️ {code} 不在 mp 下发列表（{}/{} 项）", tasks.len(), tasks.len()),
    }
    println!("---- mp accept 原始响应 ----");
    let resp = mp_accept_once_raw(account, &code).await;
    println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    match list_growth_tasks_mp(account).await {
        Ok(tasks2) => match tasks2
            .iter()
            .find(|t| t.get("task_code").and_then(Value::as_str) == Some(code.as_str()))
        {
            Some(task2) => println!(
                "回读 accept_status: {}  progress={}",
                task2.get("accept_status").and_then(Value::as_str).unwrap_or("?"),
                task2.get("progress").map(|p| p.to_string()).unwrap_or("null".into())
            ),
            None => println!("回读：{code} 不在列表"),
        },
        Err(e) => println!("回读失败：{e}"),
    }
}
