//! 账号切换：备份 → 关进程 → 复制会话/数据对齐（可选）→ 写认证 → 启动。
//! 账号切换：备份 → 关进程 → 恢复/复制会话（可选）→ 写认证 → 启动。
//!
//! 对照 server.py `switch_account`。切换过程中通过进度回调向前端推送实时进度，
//! 避免界面长时间无反馈被误认为卡死。core 不依赖 Tauri，进度回调由宿主适配
//! （桌面端转发为 `switch-progress` 事件，HTTP 端写入轮询/SSE）。
//!
//! dry_run=true 为预览模式：只统计将发生的对齐变更，不关 App、不写库、不写凭据。
//! 顺序要点（design §1）：互斥 → 关进程 → 恢复/复制/同步会话 → 写认证 → 启动。
//! 复制与同步顺序调用、共用同一把档位操作锁，且在「关闭 App + 恢复未完成写入」之后
//! 才执行；会话写入一律在 WorkBuddy 停止写入之后。普通会话操作失败不阻止认证切换，
//! 只有「无法安全恢复的中间产物」才暂停切换并明确报告恢复需求。
//!
//! 暂停启动的判定有两次，口径相同（design §5 / R5）：
//!   1. 写入之前——切换开头恢复未完成写入后仍有不可安全恢复的中间产物；
//!   2. 写入之后——本次复制/同步新留下的未完成操作（正文/数据库/组表任一阶段失败都会
//!      保留操作记录），不能带着不一致的会话内容写认证并启动 App。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::account;
use crate::modules::align::{self, AlignOptions};
use crate::modules::auth_file;
use crate::modules::oplog;
use crate::modules::process::{close_workbuddy, launch_workbuddy};
use crate::modules::session::{self, SessionPaths};
use crate::modules::session_link::{
    self, Operation, RecoveryReport, LOCK_BUSY_MESSAGE_PREFIX, LOCK_UNAVAILABLE_MESSAGE_PREFIX,
};
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

/// 切换账号。
///
/// 薄包装：调用 [`switch_account_inner`] 做实际切换，并把结果留痕到
/// `~/.wb-switch/switch_logs.json`（见 `oplog` 模块）。留痕失败不影响切换结果。
/// `restart=false` 携带会话写入意图时的拒绝文案（复制/同步各自一条）。
pub const COPY_WITHOUT_RESTART_MESSAGE: &str =
    "本次切换未重启 WorkBuddy（restart=false），已拒绝会话复制请求；如需复制请勾选重启切换";
pub const SYNC_WITHOUT_RESTART_MESSAGE: &str =
    "本次切换未重启 WorkBuddy（restart=false），已拒绝会话同步请求；如需同步请勾选重启切换";

/// 恢复报告里是否存在阻碍启动的问题：未恢复一致的中间产物必须先处理。
///
/// 拿不到档位锁、或中间产物被改动/丢失都属此类（design §4 / §5）：此时继续写认证并
/// 启动 App 会让 Official App 在最坏状态下打开会话，因此暂停切换与启动。
/// `retryable=true` 沿用复制侧「可延后重试且不阻断启动」的约定；同步恢复失败必须
/// 返回 false，即使故障只是暂时的，也不能带着半完成的正文/数据库/基线启动。
pub fn recovery_blocks_startup(report: &RecoveryReport) -> bool {
    report.needs_recovery.iter().any(|issue| !issue.retryable)
}

/// 阻碍启动的原因汇总（用于错误文案）。
///
/// 必须带操作标识：阻断分支只返回 Err 字符串，前端 catch 不能只看到原因。
pub fn recovery_blocking_detail(report: &RecoveryReport) -> String {
    report
        .needs_recovery
        .iter()
        .filter(|issue| !issue.retryable)
        .map(|issue| format!("{}：{}", issue.operation_id, issue.reason))
        .collect::<Vec<String>>()
        .join("；")
}

/// 恢复报告的前端投影（与复制/同步报告同形，便于展示恢复信息）。
pub fn recovery_report_json(report: &RecoveryReport) -> Value {
    json!({
        "recovered": report.recovered.len(),
        "abandoned": report.abandoned.len(),
        "needsRecovery": report
            .needs_recovery
            .iter()
            .map(|issue| json!({
                "operationId": issue.operation_id,
                "reason": issue.reason,
                "retryable": issue.retryable,
            }))
            .collect::<Vec<Value>>(),
        // 待清理/待恢复的临时备份残留：与复制/同步报告同一结构。
        "temporaryFiles": report.temporary_files,
    })
}

/// 某档位当前未完成操作的 id 集合。
///
/// 用于区分「本次新产生的未完成写入」与「切换开头已尝试恢复的历史残留」：
/// 后者若是可重试类（例如映射库暂不可用），按既有口径不阻断切换。
fn pending_operation_ids(paths: &SessionPaths, variant: WbVariant) -> BTreeSet<String> {
    session_link::pending_operations(paths, variant)
        .into_iter()
        .map(|operation| operation.operation_id)
        .collect()
}

/// 本次会话写入新留下的未完成操作（按创建顺序，便于稳定输出）。
fn newly_unfinished_writes(before: &BTreeSet<String>, after: Vec<Operation>) -> Vec<Operation> {
    let mut created: Vec<Operation> = after
        .into_iter()
        .filter(|operation| !before.contains(&operation.operation_id))
        .collect();
    created.sort_by_key(|operation| operation.created_at);
    created
}

/// 未完成写入的说明文案：会话标识 + 操作标识 + 原因。
fn unfinished_writes_detail(writes: &[Operation]) -> String {
    writes
        .iter()
        .map(|operation| {
            let reason = operation
                .last_error
                .clone()
                .unwrap_or_else(|| "尚未完成".to_string());
            format!(
                "{}（操作 {}）：{reason}",
                operation.target.session_id, operation.operation_id
            )
        })
        .collect::<Vec<String>>()
        .join("；")
}

/// restart=false 时携带写入意图：显式拒绝对应的会话操作（不静默丢弃，design §4.1）。
fn reject_session_writes_without_restart(
    has_copy: bool,
    has_sync: bool,
) -> (Option<Value>, Option<Value>) {
    let copy = has_copy.then(|| json!({ "error": COPY_WITHOUT_RESTART_MESSAGE }));
    let sync = has_sync.then(|| {
        json!({
            "synced": [],
            "skipped": [],
            "errors": [{ "error": SYNC_WITHOUT_RESTART_MESSAGE }],
        })
    });
    (copy, sync)
}

/// 切换账号。
///
/// `copy_session_ids` 非空时按路径 B 复制勾选会话（新 id，云端可同步）；
/// `sync_selections` 非空时把来源账号的新增内容同步到目标账号的关联会话（保留目标
/// sessionId、标题与自定义标题）。两者共用同一把档位操作锁，顺序执行、不重复写入。
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
    let mut sync_report: Option<Value> = None;
    let mut recovery_report: Option<Value> = None;
    if opts.restart {
        progress("正在关闭 WorkBuddy…");
        close_workbuddy(variant, 20)?;
        // 关进程后先恢复未完成的会话写入：恢复成功或复制侧可安全延后重试的失败
        // 不阻断切换；同步仍未完成、拿不到锁或中间产物异常则暂停启动（design §4 / §5）。
        let recovery = match session::recover_pending_session_operations(variant) {
            Ok(report) => report,
            Err(error) => {
                return Err(format!(
                    "无法恢复未完成的会话写入（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                ));
            }
        };
        let blocking = recovery_blocks_startup(&recovery);
        if !recovery.is_empty() {
            recovery_report = Some(recovery_report_json(&recovery));
        }
        if blocking {
            let detail = recovery_blocking_detail(&recovery);
            return Err(format!(
                "检测到无法安全恢复的会话写入（{detail}），已暂停切换与启动 WorkBuddy；请先处理该会话后再试"
            ));
        }
        // 本次是否请求了会话写入；同时记下写入前已存在的未完成记录，供写入后比对。
        let ops_paths = SessionPaths::for_variant(variant);
        let attempts_session_writes =
            !opts.copy_session_ids.is_empty() || !opts.sync_selections.is_empty();
        let pending_before = if attempts_session_writes {
            pending_operation_ids(&ops_paths, variant)
        } else {
            BTreeSet::new()
        };
        // 只有重启场景才做会话/数据操作（数据库在运行中不宜写入）
        // 能力探测只对国际版生效：国内版数据根与改造前同构，探测会把「从未用过
        // 会话」的国内版机器判成不支持并整段跳过（A1 零回归）。
        let copy_available = variant != WbVariant::Ai || variant.supports_session_copy();
        if !opts.copy_session_ids.is_empty() && copy_available {
            progress("正在复制会话到目标账号…");
            // 复制失败本身只记进报告、不阻断切换；但若本次留下了未完成的写入，
            // 会在复制与同步都跑完后统一暂停切换与启动（见下方 pending 差集检查）。
            copy_report = Some(
                match session::copy_sessions_for_switch(&acc, &opts.copy_session_ids) {
                    Ok(report) => report,
                    // 档位锁被占用或无法建立互斥：此时可能另有会话写入正在进行，
                    // 不能当成普通复制失败后继续写认证并启动 App。锁失败文案前缀
                    // 由 session_link 提供，不在这里嗅探整句错误文案。
                    Err(error)
                        if error.starts_with(LOCK_BUSY_MESSAGE_PREFIX)
                            || error.starts_with(LOCK_UNAVAILABLE_MESSAGE_PREFIX) =>
                    {
                        return Err(format!(
                            "无法独占会话操作（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                        ));
                    }
                    Err(error) => json!({"error": error}),
                },
            );
        }
        if !opts.sync_selections.is_empty() {
            // 同步与复制顺序执行（同一把档位锁），不重复写入。
            progress("正在同步会话到目标账号…");
            sync_report = Some(
                match session::sync_sessions_for_switch(&acc, &opts.sync_selections) {
                    Ok(report) => report,
                    Err(error)
                        if error.starts_with(LOCK_BUSY_MESSAGE_PREFIX)
                            || error.starts_with(LOCK_UNAVAILABLE_MESSAGE_PREFIX) =>
                    {
                        return Err(format!(
                            "无法独占会话操作（{error}），已暂停切换与启动 WorkBuddy；请稍后重试"
                        ));
                    }
                    // 同步失败不阻断切换：契约与成功路径同形，错误挂在 errors 里。
                    Err(error) => json!({
                        "synced": [],
                        "skipped": [],
                        "errors": [{ "error": error }],
                    }),
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
                        &session::SessionPaths::for_variant(variant),
                        &crate::modules::config::backup_dir()
                            .join("auto_link")
                            .join(crate::modules::config::utc_iso()),
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
        // 本次复制/同步新留下的未完成操作：不带着不一致的会话内容写认证并启动 App
        // （design §5 / R5）。未完成写入也不在这里被报告成成功——错误已在各自报告里，
        // 这里只决定「暂停」。未开始写入的失败（预览过期、源会话被删等）不产生操作记录，
        // 因此仍然只是跳过该项、继续切号。
        if attempts_session_writes {
            let created = newly_unfinished_writes(
                &pending_before,
                session_link::pending_operations(&ops_paths, variant),
            );
            if !created.is_empty() {
                return Err(format!(
                    "本次会话写入未完成（{}），已暂停切换与启动 WorkBuddy；请重试，下次切号会先完成恢复",
                    unfinished_writes_detail(&created)
                ));
            }
        }
    } else {
        // restart=false 表示本次不做任何会话写入；携带写入意图时显式拒绝该会话操作，
        // 不能静默丢弃（design §4.1）。
        let (copy, sync) = reject_session_writes_without_restart(
            !opts.copy_session_ids.is_empty(),
            !opts.sync_selections.is_empty(),
        );
        copy_report = copy;
        sync_report = sync;
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
    if let Some(s) = sync_report {
        result["sessionSync"] = s;
    }
    if let Some(r) = recovery_report {
        result["sessionRecovery"] = r;
    }
    // gateway(私有) —— 跟随模式：同步「跟随源当前号」（CLI 优先，内部判定 + 指纹幂等）。
    // App 切号目标 ≠ 跟随源时（四产品账号互不联动），这里只会幂等刷新 CLI 当前号，
    // 不会把 App 的号顶进网关。失败不阻断切号（进 pending 补偿）。
    result["gatewaySync"] =
        crate::modules::gateway_sync::sync_gateway_credentials_for(None, "switch");
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
    // 编排层可注入纯函数的单测：切换全流程（关进程/写认证/启动）需真机验收，
    // 这里覆盖「不可安全恢复必须暂停启动」「本次新留下的未完成写入必须暂停启动」
    // 与「restart=false 拒绝写入意图」三条判定。

    use crate::modules::session_link::{
        save_operation, OpPhase, OperationMember, RecoveryIssue, TemporaryFileIssue,
        OPERATION_VERSION,
    };

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wb_switch_orchestrate_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn paths(&self) -> SessionPaths {
            SessionPaths {
                store_root: self.0.join("store"),
                data_root: self.0.join("data"),
                link_namespace: crate::modules::session::LinkNamespace::WorkBuddy,
                auth_file: self.0.join("auth.info"),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 测试用操作记录：只关心 id、阶段与失败原因。
    fn operation(id: &str, phase: OpPhase, error: Option<&str>, created_at: i64) -> Operation {
        Operation {
            version: OPERATION_VERSION,
            operation_id: id.to_string(),
            kind: "sync".to_string(),
            variant: WbVariant::Cn,
            group_id: "g-1".to_string(),
            source: OperationMember {
                account_id: None,
                uid: "uid-a".to_string(),
                session_id: "sess-1".to_string(),
            },
            target: OperationMember {
                account_id: None,
                uid: "uid-b".to_string(),
                session_id: "sess-b".to_string(),
            },
            expected_content_digest: "digest".to_string(),
            expected_record_count: 1,
            phase,
            backup: None,
            lifecycle_version: None,
            cleanup_state: None,
            last_error: error.map(str::to_string),
            created_at,
            updated_at: created_at,
        }
    }

    fn report_with(retryable: bool) -> RecoveryReport {
        RecoveryReport {
            recovered: Vec::new(),
            abandoned: Vec::new(),
            temporary_files: Vec::new(),
            needs_recovery: vec![RecoveryIssue {
                operation_id: "op-1".to_string(),
                reason: "目标内容与操作记录不一致，已停止恢复".to_string(),
                retryable,
            }],
        }
    }

    /// 中间产物被改动/丢失（不可重试）→ 暂停启动；可重试的失败不阻断。
    #[test]
    fn recovery_blocks_startup_only_for_unrecoverable_issues() {
        assert!(recovery_blocks_startup(&report_with(false)));
        assert!(!recovery_blocks_startup(&report_with(true)));
        assert!(!recovery_blocks_startup(&RecoveryReport::default()));
        let blocking = recovery_blocking_detail(&report_with(false));
        assert!(blocking.contains("op-1"), "{blocking}");
        assert!(blocking.contains("已停止恢复"), "{blocking}");
        assert!(recovery_blocking_detail(&report_with(true)).is_empty());
    }

    /// 恢复报告投影：三类结果与不可重试标记都要能被前端看到。
    #[test]
    fn recovery_report_projection_carries_all_categories() {
        let mut report = report_with(false);
        report.recovered.push("op-2".to_string());
        report.abandoned.push("op-3".to_string());
        let value = recovery_report_json(&report);
        assert_eq!(value["recovered"], 1);
        assert_eq!(value["abandoned"], 1);
        assert_eq!(value["needsRecovery"][0]["operationId"], "op-1");
        assert_eq!(value["needsRecovery"][0]["retryable"], false);
        assert_eq!(value["temporaryFiles"], json!([]));
    }

    /// 只有临时备份残留时也不能当成「恢复什么都没做」：宿主必须把 sessionRecovery 带给前端。
    #[test]
    fn recovery_is_not_empty_when_only_temporary_files_remain() {
        let mut report = RecoveryReport::default();
        report
            .temporary_files
            .push(TemporaryFileIssue::cleanup_pending(
                "op-cleanup".to_string(),
                Some("sess-1".to_string()),
                Some("标题".to_string()),
                "临时目录删除失败：权限不足".to_string(),
            ));
        assert!(!report.is_empty());
        assert!(report.is_clean(), "待清理不得变成启动阻断");
        let value = recovery_report_json(&report);
        assert_eq!(value["temporaryFiles"][0]["operationId"], "op-cleanup");
        assert_eq!(value["temporaryFiles"][0]["state"], "cleanupPending");
        assert_eq!(value["temporaryFiles"][0]["sessionId"], "sess-1");
        assert_eq!(value["temporaryFiles"][0]["title"], "标题");
    }

    /// 本次新留下的未完成写入必须被识别出来：历史残留不算在本次头上，已完成的也不算。
    #[test]
    fn newly_unfinished_writes_only_counts_this_switch() {
        let dir = TempDir::new("pending-writes");
        let paths = dir.paths();
        // 历史残留（切换开头已尝试恢复，可重试类不阻断）。
        save_operation(
            &paths,
            &operation("op-old", OpPhase::BodyWritten, Some("映射库暂不可用"), 1),
        )
        .unwrap();
        let before = pending_operation_ids(&paths, WbVariant::Cn);
        assert_eq!(before.len(), 1);

        // 本次复制/同步新产生一条未完成操作。
        save_operation(
            &paths,
            &operation("op-new", OpPhase::DbWritten, Some("目标会话记录更新失败"), 2),
        )
        .unwrap();
        let created = newly_unfinished_writes(
            &before,
            session_link::pending_operations(&paths, WbVariant::Cn),
        );
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].operation_id, "op-new");
        let detail = unfinished_writes_detail(&created);
        assert!(detail.contains("目标会话记录更新失败"), "{detail}");

        // 已完成的写入不算未完成；没有失败原因时退化为操作 id。
        save_operation(&paths, &operation("op-done", OpPhase::Completed, None, 3)).unwrap();
        let created = newly_unfinished_writes(
            &BTreeSet::new(),
            session_link::pending_operations(&paths, WbVariant::Cn),
        );
        assert_eq!(
            created
                .iter()
                .map(|operation| operation.operation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["op-old", "op-new"],
            "按创建时间排序，且不含已完成操作"
        );
        assert!(
            unfinished_writes_detail(&[operation("op-bare", OpPhase::Prepared, None, 4)])
                .contains("op-bare")
        );

        // 别的档位的未完成写入不算在本档位头上。
        let mut other = operation("op-ai", OpPhase::Prepared, Some("待恢复"), 5);
        other.variant = WbVariant::Ai;
        save_operation(&paths, &other).unwrap();
        assert!(!pending_operation_ids(&paths, WbVariant::Cn).contains("op-ai"));
    }

    /// restart=false 携带同步意图 → 与复制同样显式拒绝，且契约与成功路径同形。
    #[test]
    fn session_writes_without_restart_are_rejected_explicitly() {
        let (copy, sync) = reject_session_writes_without_restart(false, true);
        assert!(copy.is_none());
        let sync = sync.expect("携带同步意图必须给出拒绝报告");
        assert_eq!(sync["errors"][0]["error"], SYNC_WITHOUT_RESTART_MESSAGE);
        assert!(sync["errors"][0]["error"]
            .as_str()
            .unwrap()
            .contains("restart=false"));
        assert!(sync["synced"].as_array().unwrap().is_empty());
        assert!(sync["skipped"].as_array().unwrap().is_empty());

        // 两种意图同时存在：各自拒绝，互不吞掉。
        let (copy, sync) = reject_session_writes_without_restart(true, true);
        assert_eq!(copy.unwrap()["error"], COPY_WITHOUT_RESTART_MESSAGE);
        assert_eq!(
            sync.unwrap()["errors"][0]["error"],
            SYNC_WITHOUT_RESTART_MESSAGE
        );

        // 都没有写入意图：不产生任何报告（保持原有语义）。
        let (copy, sync) = reject_session_writes_without_restart(false, false);
        assert!(copy.is_none() && sync.is_none());
    }
}
