//! 诊断用：直接调用 token_stats::get_statistics 输出项目口径的统计。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example dump_stats -- 30
//!   cargo run -p wb-switch-core --example dump_stats -- 30 --sessions
//!   cargo run -p wb-switch-core --example dump_stats -- 30 --json out.json
//!   cargo run -p wb-switch-core --example dump_stats -- --today --sessions
//!   cargo run -p wb-switch-core --example dump_stats -- --since 1757520000000
//!
//! 参数：
//!   [days]          7 / 30 / 90，省略则全量
//!   --today         本地当日 00:00 起（白名单外的自定义窗口）
//!   --since <ms>    自定义起点（含），与 --today 二选一
//!   --sessions      额外按会话明细输出（token / 命中率）
//!   --json <path>   把完整统计（含 sessions 数组）写入 JSON 文件

use serde_json::Value;

fn fmt(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        format!("{n}")
    }
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let days = args.iter().find_map(|a| a.parse::<i64>().ok());
    let show_sessions = args.iter().any(|a| a == "--sessions");
    let today = args.iter().any(|a| a == "--today");
    let since = args
        .iter()
        .position(|a| a == "--since")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<i64>().ok());
    let json_out = args
        .iter()
        .position(|a| a == "--json")
        .and_then(|i| args.get(i + 1));

    let data = if today {
        // 本地当日 00:00 的毫秒时间戳（白名单外窗口，走 since 入口）
        use chrono::{Local, TimeZone};
        let midnight = Local
            .from_local_datetime(&Local::now().date_naive().and_hms_opt(0, 0, 0).unwrap())
            .single()
            .map(|dt| dt.timestamp_millis());
        wb_switch_core::modules::token_stats::get_statistics_since(midnight)
    } else if let Some(ms) = since {
        wb_switch_core::modules::token_stats::get_statistics_since(Some(ms))
    } else {
        wb_switch_core::modules::token_stats::get_statistics(days)
    };

    if let Some(path) = json_out {
        match serde_json::to_string_pretty(&data) {
            Ok(text) => match std::fs::write(path, text) {
                Ok(()) => println!("[json] 已写入 {path}"),
                Err(e) => eprintln!("[json] 写入失败: {e}"),
            },
            Err(e) => eprintln!("[json] 序列化失败: {e}"),
        }
        return;
    }

    let sources = data.get("sources").and_then(Value::as_array).cloned().unwrap_or_default();
    for source in sources {
        let name = source.get("source").and_then(Value::as_str).unwrap_or("?");
        let summary = source.get("summary").cloned().unwrap_or(Value::Null);
        let total = u(&summary, "total");
        let input = u(&summary, "input");
        let output = u(&summary, "output");
        let cache_read = u(&summary, "cacheRead");
        let cache_write = u(&summary, "cacheWrite");
        let uncached = u(&summary, "uncachedInput");
        let records = u(&summary, "records");
        let rate = summary
            .get("cacheHitRate")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        println!("================ source: {name} ================");
        println!("  Total        {:<10} 记录/调用 {}", fmt(total), records);
        println!("  Input        {:<10}", fmt(input));
        println!("  Output       {:<10}", fmt(output));
        println!("  缓存读        {:<10}", fmt(cache_read));
        println!("  缓存写        {:<10}", fmt(cache_write));
        println!("  未命中输入    {:<10}", fmt(uncached));
        println!("  缓存命中率    {:.1}%", rate * 100.0);
        println!("  文件数        {}", u(&source, "filesScanned"));

        let sessions = source.get("sessions").and_then(Value::as_array);
        if let Some(list) = sessions {
            println!("  会话数        {}", list.len());
            if show_sessions {
                println!();
                println!("  {:<44} {:>9} {:>9} {:>8} {:>6}", "会话", "Total", "Input", "命中率", "记录");
                for s in list.iter().take(40) {
                    let title = s
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                        .unwrap_or("(无标题)");
                    let proj = s.get("project").and_then(Value::as_str).unwrap_or("");
                    let sid = s.get("sessionId").and_then(Value::as_str).unwrap_or("");
                    let label = format!("{} · {} · {}", title, proj, &sid[..sid.len().min(8)]);
                    let r = s.get("cacheHitRate").and_then(Value::as_f64).unwrap_or(0.0);
                    println!(
                        "  {:<44} {:>9} {:>9} {:>7.1}% {:>6}",
                        if label.chars().count() > 44 {
                            label.chars().take(42).collect::<String>() + ".."
                        } else {
                            label
                        },
                        fmt(u(s, "total")),
                        fmt(u(s, "input")),
                        r * 100.0,
                        u(s, "records"),
                    );
                }
            }
        }
        println!();
    }
}
