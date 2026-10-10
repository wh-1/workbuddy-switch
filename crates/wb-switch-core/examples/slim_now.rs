//! 诊断/运维用：对指定账号立即执行一次会话瘦身（走 session_slim::slim_sessions
//! 全流程——db 备份 + 归一化项目键分组 + 云删对账），与 #62 清理框同一代码路径。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example slim_now -- <uid> [keep] [--dry-run]
//!
//! 参数：
//!   <uid>        目标账号 uid（必填）
//!   [keep]       每项目保留条数，默认 1（min 1，不得全清）
//!   --dry-run    只预览不执行（报告里 victims 与云端动作均不落盘）
//!
//! 例：
//!   cargo run -p wb-switch-core --example slim_now -- 27dd5ebd-180c-4416-a3b2-180f53c9922b 1 --dry-run

use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut uid: Option<String> = None;
    let mut keep: i64 = 1;
    let mut dry_run = false;
    for a in &args {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            _ if uid.is_none() => uid = Some(a.clone()),
            _ => match a.parse::<i64>() {
                Ok(n) if n >= 1 => keep = n,
                _ => eprintln!("忽略非法参数: {a}"),
            },
        }
    }
    let Some(uid) = uid else {
        eprintln!("用法: slim_now <uid> [keep] [--dry-run]");
        std::process::exit(2);
    };
    eprintln!("slim_now uid={uid} keep={keep} dry_run={dry_run}");
    // 探针：核实解析到的 db 与该 uid 的存活会话数（防沙箱 HOME 漂移 / 看错库）
    let db_path = wb_switch_core::modules::session::workbuddy_db_path(
        wb_switch_core::modules::variant::WbVariant::Cn,
    );
    eprintln!("db = {}", db_path.display());
    match std::fs::metadata(&db_path) {
        Ok(m) => eprintln!("db exists, size={} bytes", m.len()),
        Err(e) => eprintln!("db MISSING: {e}"),
    }
    match wb_switch_core::modules::session_slim::slim_sessions(&uid, keep, dry_run, &[], None) {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            summarize(&report);
        }
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

fn summarize(report: &Value) {
    let mut n_deleted = 0usize;
    let mut n_kept = 0usize;
    if let Some(groups) = report.get("groups").and_then(|g| g.as_array()) {
        for g in groups {
            n_deleted += g.get("deleted").and_then(|d| d.as_array()).map(|a| a.len()).unwrap_or(0);
            n_kept += g.get("kept").and_then(|k| k.as_array()).map(|a| a.len()).unwrap_or(0);
        }
    }
    // 兼容不同报告形状：顶层 deleted/kept 计数兜底
    if n_deleted == 0 {
        n_deleted = report
            .get("deleted")
            .and_then(|d| d.as_array())
            .map(|a| a.len())
            .or_else(|| report.get("deletedCount").and_then(|d| d.as_i64()).map(|v| v as usize))
            .map(|v| v as usize)
            .unwrap_or(0);
    }
    eprintln!("== 摘要: deleted={n_deleted} kept={n_kept} ==");
    if let Some(cloud) = report.get("cloud") {
        eprintln!(
            "cloud: tokenReady={} reconcile={}",
            cloud.get("tokenReady").and_then(|t| t.as_bool()).unwrap_or(false),
            cloud.get("reconcile").map(|r| r.to_string()).unwrap_or_default()
        );
    }
}
