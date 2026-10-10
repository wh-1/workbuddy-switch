//! 只读探针：mp 口径（X-Client-Platform: miniprogram）拉成长任务列表，
//! 对比 web 口径差异，确认小程序限定任务（Sequential_Tasks_1 / school_season）下发状态。
//! 不发任何 accept/report/claim 写请求。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example mp_probe

use serde_json::Value;
use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{list_growth_tasks, list_growth_tasks_mp, tasks_capable_accounts};

fn kind(task: &Value) -> String {
    match task.get("task_type").and_then(Value::as_str) {
        Some("single") => "一次性".into(),
        Some("recurring") => "每日".into(),
        Some(o) => format!("?{o}"),
        None => "无字段".into(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let capable = tasks_capable_accounts(load_accounts());
    println!("可跑成长任务的账号数：{}", capable.len());
    for account in &capable {
        let uid = account.get("uid").and_then(Value::as_str).unwrap_or("?");
        let name = account.get("name").and_then(Value::as_str).unwrap_or("");
        println!("\n== {name} ({}) ==", &uid[..uid.len().min(8)]);
        let web = list_growth_tasks(account).await.unwrap_or_default();
        let web_codes: Vec<&str> = web
            .iter()
            .filter_map(|t| t.get("task_code").and_then(Value::as_str))
            .collect();
        match list_growth_tasks_mp(account).await {
            Ok(mp) => {
                println!("  mp 口径 {} 项 / web 口径 {} 项", mp.len(), web.len());
                for task in &mp {
                    let code = task.get("task_code").and_then(Value::as_str).unwrap_or("?");
                    if web_codes.contains(&code) {
                        continue; // 只列 mp 限定差异项
                    }
                    let status = task.get("accept_status").and_then(Value::as_str).unwrap_or("?");
                    let prog = task.get("progress");
                    let cur = prog.and_then(|p| p.get("current")).and_then(Value::as_i64).unwrap_or(0);
                    let tgt = prog.and_then(|p| p.get("target")).and_then(Value::as_i64).unwrap_or(0);
                    let reward = task.get("reward_credit").and_then(Value::as_i64).unwrap_or(0);
                    let desc = task.get("task_desc").and_then(Value::as_str).unwrap_or("");
                    println!(
                        "  ★ mp 限定 {code:<24} [{:<4}] accept={status:<14} {cur}/{tgt} reward={reward} | {desc}",
                        kind(task)
                    );
                }
                if let Some(st) = mp.iter().find(|t| {
                    t.get("task_code").and_then(Value::as_str) == Some("Sequential_Tasks_1")
                }) {
                    println!(
                        "  Sequential_Tasks_1 原始字段：{}",
                        serde_json::to_string(st).unwrap_or_default()
                    );
                }
            }
            Err(e) => println!("  ❌ mp 拉取失败：{e}"),
        }
    }
}
