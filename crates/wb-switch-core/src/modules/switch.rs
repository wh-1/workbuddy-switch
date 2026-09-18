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
use crate::modules::session_share;
use crate::modules::variant::WbVariant;

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
    let from_uid = session::current_user_uid(crate::modules::variant::WbVariant::Cn);
    let to_uid = account::find_account(account_id)
        .map(|acc| align::account_uid(&acc))
        .unwrap_or_default();
    let outcome = switch_account_inner(progress_fn, account_id, opts);
    let log_options = json!({
        "copySessions": opts.copy_session_ids.len(),
        "alignAutomations": opts.align_automations,
        "alignFiles": opts.align_files,
        "slimKeep": opts.slim_keep,
        "autoLink": opts.auto_link,
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

/// 从复制/共享报告里取出新会话 id（瘦身时用作保护名单，避免复制体互相挤掉）。
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

fn link_scope(opts: &SwitchOptions) -> session_share::LinkScope {
    session_share::LinkScope { keep: opts.slim_keep.max(0) as usize }
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
    // 档位以账号自身为准：签名里的参数无法表达「用 A 档位操作 B 档位账号」。
    let variant = account::variant_of(&acc);

    // 预览模式：不关 App、不写库、不写凭据，只算对齐计划
    // （含项目侧栏同步与会话瘦身——这两项是破坏性的，必须先可预览）
    // ⚠️ 备份必须放在本分支**之后**：放这儿会让每次预览都 fs::copy 一份 auth 备份（纯垃圾）。
    if opts.dry_run {
        progress("预览模式：统计将对齐的数据…");
        // 保留名单先算（autoLink dry_run 报告里带 keepTargetSids），瘦身预览用同一份名单
        let mut preview_link: Option<Value> = None;
        if opts.auto_link {
            if let Some(src) = session::current_user_uid(variant) {
                let dst = align::account_uid(&acc);
                if src != dst {
                    preview_link =
                        Some(session_share::link_missing_sessions(&src, &dst, &link_scope(opts), true));
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
        let align_data = align::preview_sync(&acc, &align_opts, &planned_move_ids, &keep_sids)
            .unwrap_or_else(|| {
                json!({ "dryRun": true, "noop": true, "targetUid": align::account_uid(&acc) })
            });
        let mut result = json!({
            "ok": true,
            "dryRun": true,
            "account": account::account_display_name(&acc),
            "alignData": align_data,
        });
        if let Some(l) = preview_link {
            result["autoLink"] = l;
        }
        if opts.auto_link {
            if let Some(src) = session::current_user_uid(variant) {
                let dst = align::account_uid(&acc);
                if src != dst {
                    // 预览共享身份改写计划（不落盘）
                    result["sidRewrite"] =
                        session_share::rewrite_shared_session_sids(&dst, true);
                }
            }
        }
        return Ok(result);
    }

    // 真实执行才备份 auth（预览零写盘，时点仍在关 App 之前）。
    let backup = auth_file::backup_auth_file(variant);

    let mut copy_report: Option<Value> = None;
    let mut auto_link_report: Option<Value> = None;
    let mut sid_rewrite_report: Option<Value> = None;
    let mut session_report: Option<Value> = None;
    let mut align_report: Option<Value> = None;
    if opts.restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(variant, 20)?;
        // 只有重启场景才做会话/数据操作（数据库在运行中不宜写入）
        // 能力探测只对国际版生效：国内版数据根与改造前同构，探测会把「从未用过
        // 会话」的国内版机器判成不支持并整段跳过（A1 零回归）。
        let copy_available = variant != WbVariant::Ai || variant.supports_session_copy();
        if !opts.copy_session_ids.is_empty() && copy_available {
            progress("正在复制会话到目标账号…");
            // 复制失败不阻断切换：报告里带上错误，切换本身仍然继续。
            copy_report = Some(
                match session::copy_sessions_for_switch(&acc, &opts.copy_session_ids) {
                    Ok(report) => report,
                    Err(error) => json!({"error": error}),
                },
            );
        }
        // 增量硬链接共享（①复制→②项目对齐→③瘦身 的第①步）：
        // 源账号「目标还没有」的存活会话，零拷贝共享过去（inode 判重，天然防重复防膨胀）。
        // 备份只在批量入口做一次（设计 §3.6：25 条逐条备份 = 24.5 MB 冗余）。
        let target_uid = align::account_uid(&acc);
        if opts.auto_link {
            if let Some(src) = session::current_user_uid(variant) {
                if src != target_uid {
                    progress("正在增量共享会话到目标账号…");
                    let db_backup = session::backup_workbuddy_db(
                        variant,
                        &crate::modules::config::backup_dir()
                            .join("auto_link")
                            .join(crate::modules::config::utc_iso()),
                    )
                    .map(|p| p.to_string_lossy().to_string());
                    let mut rep =
                        session_share::link_missing_sessions(&src, &target_uid, &link_scope(opts), false);
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
        let protected = copied_session_ids(&copy_report);
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
        align_report = align::post_close_sync(&acc, &AlignOptions {
            align_automations: opts.align_automations,
            align_files: opts.align_files,
            slim_keep: opts.slim_keep,
            dry_run: false,
        }, &protected, &keep_sids);
        if align_report.is_some() {
            progress("正在同步设置与文件…");
        }
        // 共享会话身份改写（2026-09-18 实验定稿，主人拍板实施）：把共享族正文内嵌 sid
        // 全量等长替换为目标账号 sid，记账/频控键随活跃账号走（根治共享会话 429 串号）。
        // 字节级 r+b 原地写 ⇒ inode 不变 ⇒ 硬链接保持；幂等；失败只计报告不阻断切号。
        // ⚠️ 时序铁律：必须**在瘦身之后**执行——「共享+清理」模式下被清理的旧族其目标
        // sid 已不在存活列表，族自动跳过不误改（正文留给源账号，切回时自动翻回源 sid）；
        // 「只共享」模式（keep=0）无清理，全部族照常改写。放瘦身前会白改将被清理的族。
        // 且必须保持 App 关态（无运行时占用正文文件），故仍在 restart 块内、auth 写入前。
        if opts.auto_link {
            progress("正在对齐共享会话身份（改写内嵌 sid）…");
            sid_rewrite_report =
                Some(session_share::rewrite_shared_session_sids(&target_uid, false));
        }
        if opts.share_sessions {
            // 旧的「全体转移」兼容路径（默认关闭），Rust 版暂未实现
            session_report = Some(json!({"error": "share_sessions 兼容路径暂未在 Rust 版实现"}));
        }
    }
    progress("正在写入认证文件…");
    auth_file::write_account_to_auth_file(&acc, variant)?;
    if opts.restart {
        progress("正在启动 WorkBuddy…");
        launch_workbuddy(variant, Some(&progress))?;
    }
    progress("切换完成");

    let mut result = json!({
        "ok": true,
        "account": account::account_display_name(&acc),
        "variant": variant.as_str(),
        "backup": backup.map(|p| p.to_string_lossy().to_string()),
    });
    if let Some(c) = copy_report {
        result["sessionCopy"] = c;
    }
    if let Some(a) = auto_link_report {
        result["autoLink"] = a;
    }
    if let Some(r) = sid_rewrite_report {
        result["sidRewrite"] = r;
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
}
