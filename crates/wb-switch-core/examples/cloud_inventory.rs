//! 诊断用：按账号跑云端全账巡检（瘦身报告里的 `cloud.inventory`，**只读、永不删**）。
//!
//! 直调项目实现，不在脚本里复刻口径（见 dump_stats 同款纪律）。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example cloud_inventory
//!   cargo run -p wb-switch-core --example cloud_inventory -- <uid 前缀>   # 只跑某个账号
//!
//! 输出列：uid 前 8 位 + inventory JSON（enabled / cloud / aligned / stale / foreign / localOnly）。

use std::collections::HashSet;
use std::path::Path;

use wb_switch_core::modules::{account, align, cloud_conv, cloud_reconcile, session};

/// 读本机 sessions 的存活 / 软删 id 集合（只读，与 projects_anchor 内口径一致）。
fn read_local(db: &Path) -> Result<(HashSet<String>, HashSet<String>), String> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开 workbuddy.db 失败: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT id, deleted_at FROM sessions")
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut alive = HashSet::new();
    let mut deleted = HashSet::new();
    for row in rows {
        let (id, del) = row.map_err(|e| e.to_string())?;
        if del.is_some() {
            deleted.insert(id);
        } else {
            alive.insert(id);
        }
    }
    Ok((alive, deleted))
}

fn main() {
    // 参数：uid 前缀（8 位即可，只跑某个账号）；--notoken 强制无凭证，验降级分支
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force_no_token = args.iter().any(|a| a == "--notoken");
    let only = args.iter().find(|a| !a.starts_with("--")).cloned();
    let db = session::workbuddy_db_path();
    let (alive, deleted) = match read_local(&db) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    println!("本机 sessions: 存活 {} / 软删 {}", alive.len(), deleted.len());

    for acc in account::load_accounts() {
        let uid = align::account_uid(&acc);
        if uid.is_empty() {
            continue;
        }
        if let Some(p) = &only {
            if !uid.starts_with(p.as_str()) {
                continue;
            }
        }
        let token = if force_no_token {
            None
        } else {
            cloud_conv::token_of(&uid)
        };
        let inv = cloud_reconcile::inventory(
            token.as_deref(),
            &alive,
            &deleted,
            cloud_conv::list_conversation_ids,
        );
        let short: String = uid.chars().take(8).collect();
        println!("{short}\t{inv}");
    }
}
