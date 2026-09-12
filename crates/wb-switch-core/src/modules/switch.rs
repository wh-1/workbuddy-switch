//! 账号切换：备份 → 关进程 → 复制会话/数据对齐（可选）→ 写认证 → 启动。
//!
//! 对照 server.py `switch_account`。切换过程中通过进度回调向前端推送实时进度，
//! 避免界面长时间无反馈被误认为卡死。core 不依赖 Tauri，进度回调由宿主适配
//! （桌面端转发为 `switch-progress` 事件，HTTP 端写入轮询/SSE）。
//!
//! dry_run=true 为预览模式：只统计将发生的对齐变更，不关 App、不写库、不写凭据。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::account;
use crate::modules::align::{self, AlignOptions};
use crate::modules::auth_file;
use crate::modules::oplog;
use crate::modules::process::{close_workbuddy, launch_workbuddy};
use crate::modules::session;

/// 切换进度回调（宿主注入，如 Tauri `app.emit` 或 HTTP 进度缓存）。
pub type ProgressFn = Box<dyn Fn(&str) + Send + Sync>;

/// 切换选项。
///
/// serde 必须用 camelCase：HTTP api（api_switch）直接把前端扁平 JSON 反序列化成
/// 本结构，字段名对不上会被当未知字段忽略、静默落回 default（勾选全部失效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SwitchOptions {
    #[serde(default = "default_true")]
    pub restart: bool,
    #[serde(default)]
    pub share_sessions: bool,
    #[serde(default)]
    pub copy_session_ids: Vec<String>,
    /// 自动化归属对齐（multi_sync L3）。
    #[serde(default = "default_true")]
    pub align_automations: bool,
    /// 会话归属对齐（multi_sync L3，切号后本地列表全量可见）。
    #[serde(default)]
    pub align_sessions: bool,
    /// 设置同步（multi_sync L4/L5：settings/storage/画像/my-files/主题跟随）。
    #[serde(default)]
    pub align_files: bool,
    /// 同步项目侧栏：目标账号项目集合对齐到源账号（补缺占位 + 多余软删）。
    #[serde(default)]
    pub sync_projects: bool,
    /// 会话瘦身：每项目保留最近 N 条存活会话（0 = 关闭）。
    #[serde(default)]
    pub slim_keep: i64,
    /// 预览模式：只统计变更，不落盘。
    #[serde(default)]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

impl Default for SwitchOptions {
    fn default() -> Self {
        Self {
            restart: true,
            share_sessions: false,
            copy_session_ids: Vec::new(),
            align_automations: true,
            align_sessions: false,
            align_files: false,
            sync_projects: false,
            slim_keep: 0,
            dry_run: false,
        }
    }
}

/// 切换账号。
///
/// 薄包装：调用 [`switch_account_inner`] 做实际切换，并把结果留痕到
/// `~/.wb-switch/switch_logs.json`（见 `oplog` 模块）。留痕失败不影响切换结果。
pub fn switch_account(
    progress_fn: Option<&ProgressFn>,
    account_id: &str,
    opts: &SwitchOptions,
) -> Result<Value, String> {
    let from_uid = session::current_user_uid();
    let to_uid = account::find_account(account_id)
        .map(|acc| align::account_uid(&acc))
        .unwrap_or_default();
    let outcome = switch_account_inner(progress_fn, account_id, opts);
    let log_options = json!({
        "copySessions": opts.copy_session_ids.len(),
        "alignAutomations": opts.align_automations,
        "alignSessions": opts.align_sessions,
        "alignFiles": opts.align_files,
        "syncProjects": opts.sync_projects,
        "slimKeep": opts.slim_keep,
        "restart": opts.restart,
    });
    match &outcome {
        Ok(result) => oplog::add_switch_log(&oplog::switch_log_entry(
            if opts.dry_run { "dry-run" } else { "switch" },
            from_uid.as_deref(),
            &to_uid,
            &log_options,
            result,
        )),
        Err(err) => oplog::add_switch_log(&oplog::switch_log_entry(
            "error",
            from_uid.as_deref(),
            &to_uid,
            &log_options,
            &json!({ "ok": false, "error": err }),
        )),
    }
    outcome
}

/// 从复制报告里取出新会话 id（瘦身时用作保护名单，避免复制体互相挤掉）。
fn copied_session_ids(report: &Option<Value>) -> Vec<String> {
    report
        .as_ref()
        .and_then(|r| r.get("copied"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("newId").and_then(|v| v.as_str()))
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn switch_account_inner(
    progress_fn: Option<&ProgressFn>,
    account_id: &str,
    opts: &SwitchOptions,
) -> Result<Value, String> {
    let progress = |message: &str| {
        eprintln!("[switch] progress: {message}");
        if let Some(p) = progress_fn {
            p(message);
        }
    };

    progress("开始切换账号…");
    let acc =
        account::find_account(account_id).ok_or_else(|| format!("账号不存在: {account_id}"))?;
    let backup = auth_file::backup_auth_file();

    // 预览模式：不关 App、不写库、不写凭据，只算对齐计划
    // （含项目侧栏同步与会话瘦身——这两项是破坏性的，必须先可预览）
    if opts.dry_run {
        progress("预览模式：统计将对齐的数据…");
        let align_opts = AlignOptions {
            align_automations: opts.align_automations,
            align_sessions: opts.align_sessions,
            align_files: opts.align_files,
            sync_projects: opts.sync_projects,
            slim_keep: opts.slim_keep,
            dry_run: true,
        };
        let align_data = align::preview_sync(&acc, &align_opts).unwrap_or_else(|| {
            json!({ "dryRun": true, "noop": true, "targetUid": align::account_uid(&acc) })
        });
        return Ok(json!({
            "ok": true,
            "dryRun": true,
            "account": account::account_display_name(&acc),
            "alignData": align_data,
        }));
    }

    let mut copy_report: Option<Value> = None;
    let mut session_report: Option<Value> = None;
    let mut align_report: Option<Value> = None;
    if opts.restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(20)?;
        // 只有重启场景才做会话/数据操作（数据库在运行中不宜写入）
        if !opts.copy_session_ids.is_empty() {
            progress("正在复制会话到目标账号…");
            copy_report = session::copy_sessions_for_switch(&acc, &opts.copy_session_ids);
        }
        // 设置同步 + 主题跟随 + 项目侧栏同步（本地专属逻辑在 align::post_close_sync，switch.rs 保持薄）
        // 复制体受保护：瘦身不删它们、也不让它们占用每项目保留名额。
        let protected = copied_session_ids(&copy_report);
        align_report = align::post_close_sync(&acc, &AlignOptions {
            align_automations: opts.align_automations,
            align_sessions: opts.align_sessions,
            align_files: opts.align_files,
            sync_projects: opts.sync_projects,
            slim_keep: opts.slim_keep,
            dry_run: false,
        }, &protected);
        if align_report.is_some() {
            progress("正在执行设置同步与项目侧栏同步…");
        }
        if opts.share_sessions {
            // 旧的「全体转移」兼容路径（默认关闭），Rust 版暂未实现
            session_report = Some(json!({"error": "share_sessions 兼容路径暂未在 Rust 版实现"}));
        }
    }
    progress("正在写入认证文件…");
    auth_file::write_account_to_auth_file(&acc)?;
    if opts.restart {
        progress("正在启动 WorkBuddy…");
        launch_workbuddy(Some(&progress))?;
    }
    progress("切换完成");

    let mut result = json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "backup": backup.map(|p| p.to_string_lossy().to_string()),
    });
    if let Some(c) = copy_report {
        result["sessionCopy"] = c;
    }
    if let Some(s) = session_report {
        result["sessionShare"] = s;
    }
    if let Some(a) = align_report {
        result["alignData"] = a;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：HTTP api 直接把前端扁平 camelCase JSON 反序列化成本结构。
    /// 曾因缺少 rename_all=camelCase 导致勾选全部被忽略、静默落回 default。
    #[test]
    fn switch_options_deserializes_camel_case_body() {
        let opts: SwitchOptions =
            serde_json::from_value(json!({
                "accountId": "a-1",
                "alignAutomations": false,
                "alignSessions": true,
                "alignFiles": true,
                "dryRun": true
            }))
            .expect("camelCase body 应可反序列化");
        assert!(opts.align_sessions);
        assert!(opts.align_files);
        assert!(opts.dry_run);
        assert!(!opts.align_automations);
        // snake_case 旧口径不再被接受（字段忽略后落 default）
        let legacy: SwitchOptions =
            serde_json::from_value(json!({ "align_sessions": true })).unwrap();
        assert!(!legacy.align_sessions);
    }
}
