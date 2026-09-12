//! 本地专属 Tauri 命令（上游无此文件 → 与上游合并零冲突）。
//!
//! 只放本项目新增的命令：账号发现 / 补录、数据对齐。`commands.rs` 保持上游原貌，
//! 新命令在 `lib.rs` 的 `invoke_handler` 里以 `commands_local::xxx` 注册。

use serde_json::{json, Value};

use wb_switch_core::modules::{account, align, discover};

/// GET /api/accounts/discover —— 识别本机曾登录/留有数据的账号（对照在册）。
#[tauri::command]
pub fn discover_known_accounts() -> Value {
    discover::discover_known_accounts()
}

/// POST /api/accounts/adopt —— 用最新 auth 历史备份补录指定 uid 进账号库。
#[tauri::command(rename_all = "camelCase")]
pub fn adopt_account(uid: String) -> Result<Value, String> {
    if uid.trim().is_empty() {
        return Err("缺少 uid".to_string());
    }
    discover::adopt_account(&uid).map(|meta| json!({ "ok": true, "account": meta }))
}

/// POST /api/automations/align —— 不切号，把当前自动化归属立即对齐到指定账号
/// （适用于：先用旧版切完号、补做归属对齐的场景）。需先完全退出 WorkBuddy。
#[tauri::command(rename_all = "camelCase")]
pub async fn align_automations(account_id: String) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&account_id).ok_or("账号不存在")?;
        let uid = align::account_uid(&target);
        if uid.is_empty() {
            return Err("该账号缺少 uid，无法对齐".to_string());
        }
        align::align_automations_owner(&uid).ok_or_else(|| "workbuddy.db 不存在".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST /api/align/data —— 多账号数据全量对齐（L1 备份 + L3 归属 + L4 文件 + L5 合并），
/// 不切号不写凭据。需先完全退出 WorkBuddy（dryRun=true 时只预览不落盘）。
#[tauri::command(rename_all = "camelCase")]
pub async fn align_data(
    account_id: String,
    align_automations: Option<bool>,
    align_sessions: Option<bool>,
    align_files: Option<bool>,
    dry_run: Option<bool>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let opts = align::AlignOptions {
        align_automations: align_automations.unwrap_or(true),
        align_sessions: align_sessions.unwrap_or(false),
        align_files: align_files.unwrap_or(false),
        dry_run: dry_run.unwrap_or(false),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&account_id).ok_or("账号不存在")?;
        let uid = align::account_uid(&target);
        if uid.is_empty() {
            return Err("该账号缺少 uid，无法对齐".to_string());
        }
        Ok(align::align_data(&uid, None, &opts))
    })
    .await
    .map_err(|e| e.to_string())?
}
