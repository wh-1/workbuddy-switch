//! 只读探针：拉取全部可跑成长任务的账号，逐号列出成长任务现状
//! （task_code / **task_type 周期** / accept_status / progress / 状态），不发任何写请求。
//!
//! 用途：①回答「成长任务全家族还有多少没领」②**判定每日 vs 一次性**——服务端自带
//! `task_type`：`single`=一次性 / `recurring`=每日重置（2api `school_open_day_2026.py`
//! 同款映射），别靠 target 猜。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example tasks_probe
//!
//! 输出：每账号一段任务表 + 汇总（可领/已完成未领/未接单），原始 JSON 落 `reports/`。

use serde_json::{json, Value};
use wb_switch_core::modules::account::load_accounts;
use wb_switch_core::modules::growth_tasks::{
    fetch_school_tasks, list_growth_tasks, tasks_capable_accounts,
};

/// 服务端周期字段：`single`=一次性，`recurring`=每日；未知原样回显（不猜）。
fn task_kind(task: &Value) -> String {
    match task.get("task_type").and_then(Value::as_str) {
        Some("single") => "一次性".to_string(),
        Some("recurring") => "每日".to_string(),
        Some(other) => format!("?{other}"),
        None => "无字段".to_string(),
    }
}

/// 任务当前状态字段（服务端下发，completed = 可领奖 / claimed = 已入账）。
fn task_status(task: &serde_json::Value) -> String {
    task.get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?")
        .to_string()
}

fn progress(task: &serde_json::Value) -> String {
    let p = task.get("progress");
    match p {
        Some(p) => format!(
            "{}/{}",
            p.get("current").and_then(serde_json::Value::as_i64).unwrap_or(0),
            p.get("target").and_then(serde_json::Value::as_i64).unwrap_or(0)
        ),
        None => "-".to_string(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let capable = tasks_capable_accounts(load_accounts());
    println!("可跑成长任务的账号数：{}", capable.len());
    let mut total_claimable = 0usize;
    let mut total_claimed = 0usize;
    let mut total_todo = 0usize;
    let mut raw = Vec::<Value>::new();
    for account in &capable {
        let uid = account
            .get("uid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let name = account
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        println!("\n== {name} ({}) ==", &uid[..uid.len().min(8)]);
        match list_growth_tasks(account).await {
            Ok(tasks) if tasks.is_empty() => println!("  （服务端未下发任何任务）"),
            Ok(tasks) => {
                for task in &tasks {
                    let code = task
                        .get("task_code")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let status = task_status(task);
                    let accept = task
                        .get("accept_status")
                        .and_then(Value::as_str)
                        .unwrap_or("not_accepted");
                    println!(
                        "  {code:<28} [{:<4}] status={status:<12} accept={accept:<14} progress={}",
                        task_kind(task),
                        progress(task)
                    );
                    match accept {
                        "claimed" => total_claimed += 1,
                        "completed" => total_claimable += 1,
                        _ => total_todo += 1,
                    }
                }
                // uid 只存前 8 位：原始表要能入库私仓当证据，别带完整标识。
                raw.push(json!({"scope": "growth", "uid": &uid[..uid.len().min(8)], "tasks": tasks}));
            }
            Err(e) => println!("  ❌ 拉取失败：{e}"),
        }
        // 活动任务域（/portal/activity/school）：同端点未来同类活动复用，一并探周期。
        match fetch_school_tasks(account).await {
            Ok((tasks, in_period)) => {
                println!("  -- 活动任务域 in_period={in_period} tasks={}", tasks.len());
                for task in &tasks {
                    let code = task
                        .get("task_code")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    println!(
                        "     {code:<28} [{:<4}] status={:<10}",
                        task_kind(task),
                        task_status(task)
                    );
                }
                raw.push(json!({"scope": "school", "uid": &uid[..uid.len().min(8)], "in_period": in_period, "tasks": tasks}));
            }
            Err(e) => println!("  （活动任务域拉取失败：{e}）"),
        }
    }
    // 原始 JSON 落盘：分类口径以后要复核时不用重新拉服务端。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let out = format!("reports/tasks_probe_raw_{stamp}.json");
    if let Ok(text) = serde_json::to_string_pretty(&raw) {
        if std::fs::write(&out, text).is_ok() {
            println!("\n原始任务表已落盘：{out}");
        }
    }
    println!(
        "\n汇总：已完成可领 {} 项 · 已入账 {} 项 · 进行中/未达标 {} 项",
        total_claimable, total_claimed, total_todo
    );
}
