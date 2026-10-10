//! 诊断/运维用：立即跑一轮成长任务 cycle（复刻 GUI 启动时的调用），观察是否落盘缓存。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example tasks_cycle_once          # force=false（同 GUI 启动）
//!   cargo run -p wb-switch-core --example tasks_cycle_once -- force  # 跳过开关/门控/当日幂等
//!
//! 用途：定位「成长任务 chip 只有活跃地图一段」这类问题的根因——那通常意味着
//! `~/.wb-switch/growth_tasks_cache.json` 从没被写出来（cycle 没跑完或没跑到写盘）。

use wb_switch_core::modules::growth_tasks::run_tasks_cycle;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let force = std::env::args().any(|arg| arg == "force");
    println!("[cycle] force={force} 开始…");
    let started = std::time::Instant::now();
    let result = run_tasks_cycle(force).await;
    println!(
        "[cycle] 用时 {:?} → status={}",
        started.elapsed(),
        result.get("status").and_then(|v| v.as_str()).unwrap_or("?")
    );
    if let Some(reason) = result.get("reason").and_then(|v| v.as_str()) {
        println!("[cycle] reason={reason}");
    }
    if let Some(results) = result.get("results").and_then(|v| v.as_array()) {
        for item in results {
            println!(
                "  {} → {}",
                item.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                serde_json::to_string(item.get("result").unwrap_or(&serde_json::Value::Null))
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect::<String>()
            );
        }
    }
}
