//! 诊断/运维用：立即跑一轮活跃地图 cycle（复刻 GUI 启动时的调用），观察是否落盘缓存。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example activity_cycle_once          # force=false（同 GUI 启动）
//!   cargo run -p wb-switch-core --example activity_cycle_once -- force  # 跳过开关/时点/当日幂等
//!
//! 用途：定位「活跃地图当日缓存不更新」这类问题——那通常意味着
//! `~/.wb-switch/activity_cache.json` 从没被写出来（cycle 没跑完或没跑到写盘）。
//! ⚠️ 本 example 是**独立进程**，绕过 GUI 进程内的 `ACTIVITY_RUNNING` 守卫；
//! 若 GUI 侧该守卫被上一轮永久占用（请求挂起不返回），只有新进程能补跑成功。

use wb_switch_core::modules::activity::run_activity_cycle;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let force = std::env::args().any(|arg| arg == "force");
    println!("[activity] force={force} 开始…");
    let started = std::time::Instant::now();
    // 180s 硬超时：卡住时也能定位（GUI 侧卡死正是因为无超时 + 守卫不释放）
    match tokio::time::timeout(std::time::Duration::from_secs(180), run_activity_cycle(force)).await
    {
        Ok(result) => {
            println!(
                "[activity] 用时 {:?} → status={}",
                started.elapsed(),
                result.get("status").and_then(|v| v.as_str()).unwrap_or("?")
            );
            if let Some(reason) = result.get("reason").and_then(|v| v.as_str()) {
                println!("[activity] reason={reason}");
            }
            if let Some(results) = result.get("results").and_then(|v| v.as_array()) {
                for item in results {
                    println!(
                        "  {} → {}",
                        item.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                        serde_json::to_string(item.get("result").unwrap_or(&serde_json::Value::Null))
                            .unwrap_or_default()
                            .chars()
                            .take(200)
                            .collect::<String>()
                    );
                }
            }
        }
        Err(_) => {
            println!(
                "[activity] ⏱ 180s 超时未返回 —— 有请求挂起（无超时保护），缓存未落盘"
            );
        }
    }
}
