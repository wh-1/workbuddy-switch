//! 切换流程的本地私有编排：oplog 留痕 / dry_run 预览 / autoLink 增量共享 /
//! 项目对齐与瘦身 / 共享 sid 改写。
//!
//! 设计目标（2026-09-30 最小上游足迹定稿）：[`super::switch`] 相对上游只保留
//! 「签名收拢成 [`SwitchOptions`] + 少量 hook 调用」，全部本地专属逻辑住在本模块。
//! 上游演进 switch_account 主体时不会与本文件冲突，合并面积最小化。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

use crate::modules::account;
use crate::modules::align::{self, AlignOptions};
use crate::modules::config;
use crate::modules::oplog;
use crate::modules::session::{self, SessionPaths};
use crate::modules::session_share;
use crate::modules::variant::WbVariant;

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
    /// 设置同步（multi_sync L4/L5：settings/storage/画像/my-files/主题跟随）。
    #[serde(default)]
    pub align_files: bool,
    /// 会话瘦身：每项目保留最近 N 条存活会话（0 = 关闭）。
    /// 共享链路下 N 同时是**统一保留名单**的维度（名单 = 源∪目标合并、每项目最新 N 条）。
    #[serde(default)]
    pub slim_keep: i64,
    /// 增量硬链接共享：源账号「目标还没有」的会话零拷贝共享过去（2026-09-16 定稿，默认开）。
    #[serde(default = "default_true")]
    pub auto_link: bool,
    /// 预览模式：只统计变更，不落盘。
    #[serde(default)]
    pub dry_run: bool,
    /// 关联组同步选择（#59：[{groupId, previewToken, mode}]）。
    /// SyncSelection 手动 parse（拒绝未知 mode），不走 serde：HTTP/命令层解析后填入。
    #[serde(skip, default)]
    pub sync_selections: Vec<session::SyncSelection>,
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
            align_files: false,
            slim_keep: 0,
            auto_link: true,
            dry_run: false,
            sync_selections: Vec::new(),
        }
    }
}

/// 把一次切换的结果留痕到 `~/.wb-switch/switch_logs.json`（见 `oplog` 模块）。
/// 留痕失败不影响切换结果；dry_run 单独标记类别，便于台账区分预览与真实切换。
pub fn log_switch_result(account_id: &str, opts: &SwitchOptions, outcome: &Result<Value, String>) {
    let from_uid = session::current_user_uid(WbVariant::Cn);
    let to_uid = account::find_account(account_id)
        .map(|acc| align::account_uid(&acc))
        .unwrap_or_default();
    let log_options = json!({
        "copySessions": opts.copy_session_ids.len(),
        "alignAutomations": opts.align_automations,
        "alignFiles": opts.align_files,
        "slimKeep": opts.slim_keep,
        "autoLink": opts.auto_link,
        "restart": opts.restart,
    });
    match outcome {
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
}

/// 从复制/共享报告里取出新会话 id（瘦身时用作保护名单，避免复制体互相挤掉）。
pub fn copied_session_ids(report: &Option<Value>) -> Vec<String> {
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

/// dry_run 预览：统计将对齐的数据，不关 App、不写库、不写凭据。
///
/// ⚠️ 备份必须放在本分支**之后**（真实执行路径）：放这儿会让每次预览都 fs::copy
/// 一份 auth 备份（纯垃圾）。
pub fn dry_run_preview(
    progress: &dyn Fn(&str),
    acc: &Value,
    variant: WbVariant,
    opts: &SwitchOptions,
) -> Result<Value, String> {
    progress("预览模式：统计将对齐的数据…");
    // 保留名单先算（autoLink dry_run 报告里带 keepTargetSids），瘦身预览用同一份名单
    let mut preview_link: Option<Value> = None;
    if opts.auto_link {
        if let Some(src) = session::current_user_uid(variant) {
            let dst = align::account_uid(acc);
            if src != dst {
                preview_link =
                    Some(session_share::link_missing_sessions(&src, &dst, &session_share::link_scope_from_keep(opts.slim_keep), true));
            }
        }
    }
    let keep_sids: Vec<String> = preview_link
        .as_ref()
        .and_then(|r| r.get("keepTargetSids"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let align_opts = AlignOptions {
        align_automations: opts.align_automations,
        align_files: opts.align_files,
        slim_keep: opts.slim_keep,
        // 无感切号：身份四文件全部跟随（坑 82）；关掉则只带 MEMORY.md + USER.md
        align_persona: true,
        dry_run: true,
    };
    // 预览不真复制/真共享，但要把「本次会搬过去的会话」传进去，用于量化瘦身的抵消条数：
    // 手动勾选复制（`copy_session_ids`）+ 共享待搬（`autoLink` dry_run 的 `copied[]`，即计划硬链接的源会话）。
    // ⚠️ 只传前者时，共享场景拿不到 `copyPlanned`，预览会退化成没有条数的兜底提示。
    let mut planned_move_ids: Vec<String> = opts.copy_session_ids.clone();
    if let Some(arr) = preview_link
        .as_ref()
        .and_then(|r| r.get("copied"))
        .and_then(|v| v.as_array())
    {
        for item in arr {
            // ⚠️ `copied[]` 的元素是对象（含 `id` / `newId` / `planned`），不是裸字符串
            // —— 直接 `as_str()` 会全部过滤掉，共享场景永远拿不到 `copyPlanned`。
            let sid = item
                .as_str()
                .or_else(|| item.get("id").and_then(|v| v.as_str()))
                .map(str::to_string);
            if let Some(sid) = sid {
                if !planned_move_ids.iter().any(|s| s.as_str() == sid) {
                    planned_move_ids.push(sid);
                }
            }
        }
    }
    let align_data = align::preview_sync(acc, &align_opts, &planned_move_ids, &keep_sids)
        .unwrap_or_else(|| {
            json!({ "dryRun": true, "noop": true, "targetUid": align::account_uid(acc) })
        });
    let mut result = json!({
        "ok": true,
        "dryRun": true,
        "account": account::account_display_name(acc),
        "alignData": align_data,
    });
    if let Some(l) = preview_link {
        result["autoLink"] = l;
    }
    if opts.auto_link {
        if let Some(src) = session::current_user_uid(variant) {
            let dst = align::account_uid(acc);
            if src != dst {
                // 预览共享身份改写计划（不落盘）
                result["sidRewrite"] =
                    session_share::rewrite_shared_session_sids(&dst, true);
            }
        }
    }
    Ok(result)
}

/// 关进程后的本地专属编排产物（三份报告，由 [`post_close_extras`] 填充）。
pub struct PostCloseReports {
    pub auto_link: Option<Value>,
    pub sid_rewrite: Option<Value>,
    pub align: Option<Value>,
}

/// 关进程后的本地专属编排（App 已关、auth 未写的窗口期内执行）：
/// ①autoLink 增量共享 → ②统一保留名单计算 → ③项目对齐/瘦身 → ④共享 sid 改写。
///
/// ⚠️ 时序铁律：sid 改写必须**在瘦身之后**执行——「共享+清理」模式下被清理的旧族其
/// 目标 sid 已不在存活列表，族自动跳过不误改；放瘦身前会白改将被清理的族。
/// 且必须保持 App 关态（无运行时占用正文文件）。
pub fn post_close_extras(
    progress: &dyn Fn(&str),
    acc: &Value,
    variant: WbVariant,
    opts: &SwitchOptions,
    copy_report: &Option<Value>,
) -> PostCloseReports {
    let mut auto_link_report: Option<Value> = None;
    let mut sid_rewrite_report: Option<Value> = None;
    // 增量硬链接共享（①复制→②项目对齐→③瘦身 的第①步）：
    // 源账号「目标还没有」的存活会话，零拷贝共享过去（inode 判重，天然防重复防膨胀）。
    // 备份只在批量入口做一次（设计 §3.6：25 条逐条备份 = 24.5 MB 冗余）。
    let target_uid = align::account_uid(acc);
    if opts.auto_link {
        if let Some(src) = session::current_user_uid(variant) {
            if src != target_uid {
                progress("正在增量共享会话到目标账号…");
                let db_backup = session::backup_workbuddy_db(
                    &SessionPaths::for_variant(variant),
                    &config::backup_dir()
                        .join("auto_link")
                        .join(config::utc_iso()),
                )
                .ok()
                .map(|p| p.to_string_lossy().to_string());
                let mut rep =
                    session_share::link_missing_sessions(&src, &target_uid, &session_share::link_scope_from_keep(opts.slim_keep), false);
                rep["backupDb"] = json!(db_backup);
                auto_link_report = Some(rep);
            }
        }
    }
    // 设置同步 + 主题跟随 + 项目侧栏同步（本地专属逻辑在 align::post_close_sync，switch.rs 保持薄）
    // 瘦身保护名单只含**手动勾选复制体**（用户明确动作）；
    // autoLink 共享副本不再保护——统一保留名单（keepTargetSids）之外的都删（2026-09-16 主人定稿），
    // 否则每切一次号堆一批副本，目标账号会话数失控（H 账号 76 条的教训）。
    // ⚠️ 名单计算时点早于会话创建 ⇒ **本轮共享产物（newId）必须补进名单**，
    // 否则 keepList 模式会把刚共享的会话当「名单外」删掉（22:12 实测踩坑）。
    let protected = copied_session_ids(copy_report);
    let mut keep_sids: Vec<String> = auto_link_report
        .as_ref()
        .and_then(|r| r.get("keepTargetSids"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    keep_sids.extend(copied_session_ids(&auto_link_report));
    let align_report = align::post_close_sync(acc, &AlignOptions {
        align_automations: opts.align_automations,
        align_files: opts.align_files,
        slim_keep: opts.slim_keep,
        // 无感切号：身份四文件全部跟随（坑 82）；关掉则只带 MEMORY.md + USER.md
        align_persona: true,
        dry_run: false,
    }, &protected, &keep_sids);
    if align_report.is_some() {
        progress("正在同步设置与文件…");
    }
    // 共享会话身份改写（2026-09-18 实验定稿，主人拍板实施）：把共享族正文内嵌 sid
    // 全量等长替换为目标账号 sid，记账/频控键随活跃账号走（根治共享会话 429 串号）。
    // 字节级 r+b 原地写 ⇒ inode 不变 ⇒ 硬链接保持；幂等；失败只计报告不阻断切号。
    // 「只共享」模式（keep=0）无清理，全部族照常改写。
    if opts.auto_link {
        progress("正在对齐共享会话身份（改写内嵌 sid）…");
        sid_rewrite_report =
            Some(session_share::rewrite_shared_session_sids(&target_uid, false));
    }
    PostCloseReports {
        auto_link: auto_link_report,
        sid_rewrite: sid_rewrite_report,
        align: align_report,
    }
}

/// 渲染层会话缓存目录（WorkBuddy userData 下的 Session Storage，Chromium leveldb）。
/// 与 ui_theme 的 Local Storage 同级但用途不同：这里存的是**会话级** UI 态（含
/// `agents_user_auth_action_logged_in_uid` 登录账号缓存），预写它即让重启后直接是新号。
/// （目录常量复用 ui_theme，避免两处重复定义。）

/// 切号后刷新渲染层界面（**无条件执行**；2026-10-08 定稿，原独立开关已移除）。
///
/// WorkBuddy 5.7.6 的渲染层账号资料是懒加载的：只换 auth 文件重启会显示源号
/// （左下角徽章/侧栏），需一次交互才刷新。本 hook 做三件事——
/// ① 清空 Session Storage：Chromium 重启后 map id 重新分配，预写 `map-0-…logged_in_uid`
///    会指向旧 map 而失效（实测）⇒ 只能清空让渲染层重新走 auth，刷新到目标号；
/// ② Local Storage `agent-ui-sidebar-expanded` = `true`：CDP 快照 diff 实证该键即主侧栏
///    展开态，而 Local Storage 不被清 ⇒ 预写保留，侧栏保持展开；
/// ③ （由 `switch.rs` 在启动后异步执行）经 CDP 触发一次账号重拉，见 `renderer_refresh`。
///
/// 清空前备份整个 Session Storage 到 `~/.wb-switch/backups/renderer-store/<ts>/`；
/// 失败不阻断切号。草稿不受影响（草稿在 Local Storage）。
pub fn refresh_renderer_ui_hook(progress: &dyn Fn(&str)) -> Option<Value> {
    let wb_root = config::home_dir().join(".workbuddy");
    let backup_root = config::backup_dir().join("renderer-store").join(config::utc_iso());
    let mut report = json!({ "prewritten": [], "errors": [] });

    // ① 清空 Session Storage ⇒ 重启后渲染层重新 auth，账号刷新到目标号
    {
        progress("正在准备账号界面（重置登录态，重启后刷新到目标账号）…");
        let dir = wb_root.join(crate::modules::ui_theme::SESSION_STORAGE_SUBDIR
            .trim_end_matches("/leveldb"));
        if dir.is_dir() {
            let backup_dir = backup_root.join("session-storage").join(config::utc_iso());
            let backup = match std::fs::create_dir_all(&backup_dir) {
                Ok(()) => match copy_dir_contents(&dir, &backup_dir) {
                    Ok(()) => Some(backup_dir.to_string_lossy().to_string()),
                    Err(error) => {
                        report["errors"].as_array_mut().expect("array").push(
                            json!(format!("sessionStorage 备份失败，已跳过清空: {error}")),
                        );
                        None
                    }
                },
                Err(error) => {
                    report["errors"].as_array_mut().expect("array").push(
                        json!(format!("备份目录创建失败，已跳过清空: {error}")),
                    );
                    None
                }
            };
            if backup.is_some() {
                let (removed, _bytes, errors) = clear_dir_contents(&dir);
                report["cleared"] = json!({ "removed": removed, "backup": backup });
                for e in errors {
                    report["errors"].as_array_mut().expect("array").push(json!(e));
                }
            }
        }
    }

    // ①.5 窗口状态修正（无条件）：window-state.json 记录的窄尺寸（实测 608×456）会让
    // 5.7.6 响应式布局在启动时把侧栏初始化为收起（先按 bounds 建窗、再应用最大化也救不回）。
    // 把 bounds 抬到宽屏尺寸 ⇒ 启动即展开。备份在前；文件缺失/损坏跳过。
    {
        let ws_path = wb_root.join("app/window-state.json");
        if ws_path.is_file() {
            let backup_ws = backup_root.join("window-state.json");
            let _ = std::fs::copy(&ws_path, &backup_ws);
            if let Ok(mut val) = serde_json::from_str::<Value>(
                &std::fs::read_to_string(&ws_path).unwrap_or_default(),
            ) {
                if let Some(b) = val.get_mut("bounds") {
                    let w = b.get("width").and_then(|v| v.as_i64()).unwrap_or(0);
                    if w < 1200 {
                        b["width"] = json!(1600);
                        b["height"] = json!(900);
                        if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                            match std::fs::write(&ws_path, pretty) {
                                Ok(()) => report["prewritten"]
                                    .as_array_mut()
                                    .expect("array")
                                    .push(json!("window-state:bounds-widened")),
                                Err(error) => report["errors"]
                                    .as_array_mut()
                                    .expect("array")
                                    .push(json!(format!("window-state: {error}"))),
                            }
                        }
                    }
                }
            }
        }
    }

    // ② Local Storage：侧栏展开态 → true（Local Storage 不被清 ⇒ 预写保留生效）
    {
        let local_dir = wb_root.join("app/session/Local Storage/leveldb");
        if local_dir.is_dir() {
            progress("正在预写侧栏展开状态…");
            match crate::modules::ui_theme::prewrite_record(
                &local_dir,
                &crate::modules::ui_theme::local_storage_key("agent-ui-sidebar-expanded"),
                &crate::modules::ui_theme::local_storage_text_value("true"),
                &backup_root,
            ) {
                Ok(_) => report["prewritten"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!("localStorage:agent-ui-sidebar-expanded")),
                Err(error) => report["errors"]
                    .as_array_mut()
                    .expect("array")
                    .push(json!(format!("localStorage: {error}"))),
            }
        }
    }

    Some(report)
}

/// 递归复制目录内容（备份用）。
fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_contents(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 清空目录内容但保留目录本身；返回 (删除条数, 释放字节, 错误列表)。
fn clear_dir_contents(dir: &Path) -> (usize, u64, Vec<String>) {
    let mut removed = 0usize;
    let mut bytes = 0u64;
    let mut errors: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0, vec![format!("读取目录失败: {}", dir.display())]);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let size = entry
            .metadata()
            .map(|m| if m.is_dir() { 0 } else { m.len() })
            .unwrap_or(0);
        let is_dir = path.is_dir();
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                removed += 1;
                bytes += size;
            }
            Err(error) => errors.push(format!(
                "{}: {error}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string())
            )),
        }
    }
    (removed, bytes, errors)
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
                "alignAutomations": true,
                "alignFiles": true,
                "dryRun": true
            }))
            .expect("camelCase body 应可反序列化");
        assert!(opts.align_automations);
        assert!(opts.align_files);
        assert!(opts.dry_run);
        // snake_case 旧口径不再被接受（字段忽略后落 default ⇒ align_automations 的 default 为 true）
        let legacy: SwitchOptions =
            serde_json::from_value(json!({ "align_automations": false })).unwrap();
        assert!(legacy.align_automations);
    }


    /// 预写编码：Local Storage 键/值、Session Storage 键/值（UTF-16LE）逐字节校验。
    #[test]
    fn renderer_store_key_value_encodings() {
        use crate::modules::ui_theme::{
            local_storage_key, local_storage_text_value, session_storage_key,
            session_storage_text_value,
        };
        // Local Storage：`_file://\0\x01` + 键名；值 = 0x01 + UTF-8
        assert_eq!(
            local_storage_key("agent-ui-sidebar-expanded"),
            b"_file://\x00\x01agent-ui-sidebar-expanded".to_vec()
        );
        assert_eq!(
            local_storage_text_value("true"),
            vec![0x01, b't', b'r', b'u', b'e']
        );
        // Session Storage：`map-0-` + 键名；值 = UTF-16LE（无前缀）
        assert_eq!(
            session_storage_key("agents_user_auth_action_logged_in_uid"),
            b"map-0-agents_user_auth_action_logged_in_uid".to_vec()
        );
        let v = session_storage_text_value("2a");
        assert_eq!(v, vec![0x32, 0x00, 0x61, 0x00]);
    }

    /// 预写记录可被 leveldb 追加（用临时目录模拟），且写入后 .log 增长、备份存在。
    #[test]
    fn prewrite_record_appends_and_backs_up() {
        use crate::modules::ui_theme::{
            local_storage_key, local_storage_text_value, prewrite_record,
        };
        let tmp = std::env::temp_dir().join(format!("wb-switch-prewrite-{}", std::process::id()));
        let db_dir = tmp.join("leveldb");
        let backup_root = tmp.join("backups");
        std::fs::create_dir_all(&db_dir).unwrap();
        // 先造一个 .log（leveldb 只追加到编号最大的 .log）
        let log = db_dir.join("000003.log");
        std::fs::write(&log, b"seed").unwrap();
        let before = std::fs::metadata(&log).unwrap().len();

        let backup = prewrite_record(
            &db_dir,
            &local_storage_key("agent-ui-sidebar-expanded"),
            &local_storage_text_value("true"),
            &backup_root,
        )
        .expect("预写应成功");
        // .log 增长（追加了一条 record）
        let after = std::fs::metadata(&log).unwrap().len();
        assert!(after > before, "追加后 .log 应变长");
        // 备份存在且保留原始内容（seed）
        assert!(backup.exists(), "备份文件应存在");
        assert_eq!(std::fs::read(&backup).unwrap(), b"seed");
        // 追加内容含键名（明文可检索）
        let written = std::fs::read(&log).unwrap();
        assert!(
            written
                .windows(b"agent-ui-sidebar-expanded".len())
                .any(|w| w == b"agent-ui-sidebar-expanded"),
            "写入内容应含键名"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
