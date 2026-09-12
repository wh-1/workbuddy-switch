//! 多账号数据对齐：把 multi_sync 的 L1/L3/L4/L5 能力内置进 switch。
//!
//! 设计原则（对照 multi_sync.py，规则保持一致）：
//! - 归属层（L3）：「移动」而非「复制」，UPDATE 既有行的 owner/user_id，天然无双跑
//! - 文件层（L4）：settings.json claw.users 深合并，SECRET_KEYS 命中即跳过防串号；
//!   storage/user-* 与画像缓存做「目标缺失才补」的单向对齐
//! - 合并层（L5）：my-files.json 全账号并集，禁止单向覆盖（防丢 favoriteIds 键）
//! - 备份层（L1）：任何落盘前先备份 workbuddy.db + settings.json
//! - dry_run：只统计将要发生的变更，不落盘、不关 App、不写凭据

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, backup_dir, home_dir, now_ms, utc_iso};
use crate::modules::session::{backup_workbuddy_db, open_db, table_exists, workbuddy_db_path};

/// 身份凭据关键词：命中即跳过，绝不跨账号复制（含渠道身份，防止串号）。
const SECRET_KEYS: &[&str] = &[
    "bottoken", "accountid", "token", "credential", "secret", "master.key",
    "appid", "appsecret", "corpid", "apikey", "accesstoken", "refreshtoken",
    "password", "sessionkey", "privatekey", "signature",
    "channelid", "userid", "openid", "unionid", "chatid", "groupid", "roomid",
    "webhook", "robotid", "wxid", "imuid", "ownerid",
];
const SECRET_FILENAMES: &[&str] = &["master.key", "credential", "credentials"];
/// 这些文件必须走并集合并，禁止单向覆盖。
const MERGE_ONLY_FILES: &[&str] = &["my-files.json"];
/// multi_sync v1 时代的遗留触发器，发现即清除。
const LEGACY_TRIGGER: &str = "trg_unify_session_uid";

pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_lowercase().replace(['_', '-'], "");
    SECRET_KEYS.iter().any(|s| k.contains(&s.replace('.', "")))
}

fn is_secret_file(name: &str) -> bool {
    let low = name.to_lowercase();
    SECRET_FILENAMES.iter().any(|s| low.contains(s))
}

fn is_merge_only(name: &str) -> bool {
    MERGE_ONLY_FILES.iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// 对齐选项（dry_run=true 时只统计不落盘）。
///
/// 命名（2026-09-12 定稿）：align_automations = 「定时任务迁入」；
/// align_sessions = 「会话归属对齐」（已下线，代码保留）；align_files = 「设置同步」
/// （含 settings 深合并 / storage 补齐 / 画像 / my-files / 主题跟随）；
/// sync_projects = 「同步项目侧栏」；slim_keep = 「会话瘦身」保留条数（0=关）。
#[derive(Debug, Clone, Default)]
pub struct AlignOptions {
    pub align_automations: bool,
    pub align_sessions: bool,
    pub align_files: bool,
    /// 同步项目侧栏：目标账号项目集合对齐到源账号（补缺占位 + 多余软删）。
    pub sync_projects: bool,
    /// 会话瘦身：每项目保留最近 N 条存活会话（0 或缺省 = 关闭）。
    pub slim_keep: i64,
    pub dry_run: bool,
}

fn workbuddy_root() -> PathBuf {
    home_dir().join(".workbuddy")
}

fn settings_path() -> PathBuf {
    workbuddy_root().join("settings.json")
}

fn memory_dir() -> PathBuf {
    workbuddy_root().join("memory")
}

fn storage_dir() -> PathBuf {
    workbuddy_root().join("storage")
}

/// 从账号 JSON 里取 uid（空串表示账号缺 uid，调用方应跳过对齐）。
pub fn account_uid(acc: &Value) -> String {
    acc.get("uid")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 备份 settings.json（P1 全量备份的一半；db 备份复用 session::backup_workbuddy_db）。
pub fn backup_settings() -> Option<PathBuf> {
    let src = settings_path();
    if !src.is_file() {
        return None;
    }
    let root = backup_dir().join("settings").join(utc_iso());
    fs::create_dir_all(&root).ok()?;
    let dst = root.join("settings.json");
    fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// L1：备份 workbuddy.db + settings.json，返回描述。
fn backup_all() -> Value {
    let db = backup_workbuddy_db(&backup_dir().join("db").join(utc_iso()))
        .map(|p| p.to_string_lossy().to_string());
    let settings = backup_settings().map(|p| p.to_string_lossy().to_string());
    json!({ "db": db, "settings": settings })
}

// ---------------------------------------------------------------------------
// 账号发现 / 对齐源选择
// ---------------------------------------------------------------------------

/// 账号清单 = settings claw.users 键 ∪ storage/user-<uid> 目录 ∪ 画像缓存文件。
pub fn discover_accounts() -> Vec<String> {
    let mut accs: Vec<String> = Vec::new();
    if let Ok(text) = fs::read_to_string(settings_path()) {
        if let Ok(data) = serde_json::from_str::<Value>(&text) {
            if let Some(users) = data
                .get("claw")
                .and_then(|c| c.get("users"))
                .and_then(|u| u.as_object())
            {
                accs.extend(users.keys().cloned());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(storage_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uid) = name.strip_prefix("user-") {
                if !uid.ends_with("-personal") {
                    accs.push(uid.to_string());
                }
            }
        }
    }
    if let Ok(entries) = fs::read_dir(memory_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uid) = name.strip_suffix("_memory.md") {
                accs.push(uid.to_string());
            }
        }
    }
    accs.sort();
    accs.dedup();
    accs
}

fn latest_mtime_dir(p: &Path) -> i64 {
    let mut latest = 0i64;
    if let Ok(entries) = fs::read_dir(p) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                latest = latest.max(latest_mtime_dir(&path));
            } else if let Ok(meta) = e.metadata() {
                if let Ok(m) = meta.modified() {
                    if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                        latest = latest.max(d.as_millis() as i64);
                    }
                }
            }
        }
    }
    latest
}

/// 缺省对齐源 = 除目标外最近活跃的账号（按 user-<uid>-personal 最新 mtime）。
pub fn pick_source(target: &str) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for uid in discover_accounts() {
        if uid == target {
            continue;
        }
        let dir = storage_dir().join(format!("user-{uid}-personal"));
        let m = if dir.is_dir() { latest_mtime_dir(&dir) } else { 0 };
        if best.as_ref().is_none_or(|(bm, _)| m > *bm) {
            best = Some((m, uid));
        }
    }
    best.map(|(_, uid)| uid)
}

// ---------------------------------------------------------------------------
// L3 归属层（DB）
// ---------------------------------------------------------------------------

/// 把未删除自动化的 owner 对齐到目标账号（含备份）。db 不存在返回 None。
pub fn align_automations_owner(target_uid: &str) -> Option<Value> {
    let db = workbuddy_db_path();
    if !db.is_file() {
        return None;
    }
    let backup = backup_workbuddy_db(&backup_dir().join("automations").join(utc_iso()))
        .map(|p| p.to_string_lossy().to_string());
    let (automations, outbox) =
        align_automations_owner_in_db(&db, target_uid, false).unwrap_or((0, 0));
    Some(json!({
        "targetUid": target_uid,
        "automationsUpdated": automations,
        "outboxUpdated": outbox,
        "backup": backup,
    }))
}

/// 低层：对齐 automations + outbox 的 owner，返回 (automations 行数, outbox 行数)。
fn align_automations_owner_in_db(
    db_path: &Path,
    target_uid: &str,
    dry_run: bool,
) -> Result<(usize, usize), String> {
    if !db_path.is_file() {
        return Ok((0, 0));
    }
    let Some(conn) = open_db(db_path, false) else {
        return Ok((0, 0));
    };

    let mut automations_updated = 0usize;
    if table_exists(&conn, "automations") {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM automations \
                 WHERE deleted_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n > 0 && !dry_run {
            conn.execute(
                "UPDATE automations SET owner_user_id = ?1, updated_at = ?2 \
                 WHERE deleted_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        }
        automations_updated = n as usize;
    }

    let mut outbox_updated = 0usize;
    if table_exists(&conn, "automation_delivery_outbox") {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM automation_delivery_outbox \
                 WHERE finished_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if n > 0 && !dry_run {
            conn.execute(
                "UPDATE automation_delivery_outbox SET owner_user_id = ?1, updated_at = ?2 \
                 WHERE finished_at IS NULL AND (owner_user_id IS NULL OR owner_user_id != ?1)",
                rusqlite::params![target_uid, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        }
        outbox_updated = n as usize;
    }

    Ok((automations_updated, outbox_updated))
}

/// 低层：对齐 sessions.user_id + 清理遗留触发器，返回 (sessions 行数, 是否移除触发器)。
///
/// 与 multi_sync L3 口径一致：user_id 非空且 != 目标的**存活**行对齐；软删行（deleted_at 非空）
/// 不改写——已删除对话不参与任何上下文/统计展示，改写归属无意义，且保留原值可作对齐痕迹回溯。
fn align_sessions_owner_in_db(
    db_path: &Path,
    target_uid: &str,
    dry_run: bool,
) -> Result<(usize, bool), String> {
    if !db_path.is_file() {
        return Ok((0, false));
    }
    let Some(conn) = open_db(db_path, false) else {
        return Ok((0, false));
    };
    if !table_exists(&conn, "sessions") {
        return Ok((0, false));
    }

    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions \
             WHERE deleted_at IS NULL AND user_id IS NOT NULL AND user_id != ?1",
            rusqlite::params![target_uid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if n > 0 && !dry_run {
        conn.execute(
            "UPDATE sessions SET user_id = ?1, updated_at = ?2 \
             WHERE deleted_at IS NULL AND user_id IS NOT NULL AND user_id != ?1",
            rusqlite::params![target_uid, now_ms()],
        )
        .map_err(|e| e.to_string())?;
    }

    let has_trigger: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1)",
            [LEGACY_TRIGGER],
            |r| r.get(0),
        )
        .unwrap_or(0)
        == 1;
    let mut trigger_removed = false;
    if has_trigger && !dry_run {
        conn.execute(&format!("DROP TRIGGER {LEGACY_TRIGGER}"), [])
            .map_err(|e| e.to_string())?;
        trigger_removed = true;
    }

    Ok((n as usize, trigger_removed || (has_trigger && dry_run)))
}

// ---------------------------------------------------------------------------
// L4 文件层
// ---------------------------------------------------------------------------

/// settings.json claw.users 深合并（源补齐到目标），SECRET_KEYS 命中的键整棵跳过。
fn align_settings_claw_users(src: &str, dst: &str, dry_run: bool) -> Value {
    let mut changed: Vec<String> = Vec::new();
    let mut skipped = false;
    'outer: {
        let Ok(text) = fs::read_to_string(settings_path()) else {
            skipped = true;
            break 'outer;
        };
        let Ok(mut data) = serde_json::from_str::<Value>(&text) else {
            skipped = true;
            break 'outer;
        };
        let Some(users) = data
            .get_mut("claw")
            .and_then(|c| c.get_mut("users"))
            .and_then(|u| u.as_object_mut())
        else {
            skipped = true;
            break 'outer;
        };
        let Some(src_node) = users.get(src).cloned() else {
            skipped = true;
            break 'outer;
        };
        let Some(dst_node) = users.get_mut(dst) else {
            skipped = true;
            break 'outer;
        };
        if let (Value::Object(s), Value::Object(d)) = (&src_node, dst_node) {
            let mut s = s.clone();
            merge_walk(&mut s, d, "", &mut changed);
        }
        if !changed.is_empty() && !dry_run {
            if let Ok(out) = serde_json::to_string_pretty(&data) {
                if let Err(e) = atomic_write(&settings_path(), &out) {
                    return json!({ "changed": 0, "skipped": true, "error": e.to_string() });
                }
            }
        }
    }
    json!({
        "changed": changed.len(),
        "keys": changed.iter().take(8).collect::<Vec<_>>(),
        "skipped": skipped,
    })
}

/// 深合并：src 的键补齐/更新到 dst（secret 键跳过），记录变更路径。
fn merge_walk(
    src: &Map<String, Value>,
    dst: &mut Map<String, Value>,
    path: &str,
    changed: &mut Vec<String>,
) {
    for (k, v) in src {
        if is_secret_key(k) {
            continue;
        }
        let full = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
        match dst.get_mut(k) {
            None => {
                changed.push(format!("+{full}"));
                dst.insert(k.clone(), v.clone());
            }
            Some(Value::Object(d2)) if v.is_object() => {
                if let Value::Object(s2) = v {
                    merge_walk(s2, d2, &full, changed);
                }
            }
            Some(cur) if cur != v => {
                changed.push(format!("~{full}"));
                *cur = v.clone();
            }
            _ => {}
        }
    }
}

/// 递归目录对齐：目标缺失或内容不同才复制；跳过 .bak / 凭据文件；my-files.json 延迟到 L5。
fn align_storage_dir(
    src: &Path,
    dst: &Path,
    dry_run: bool,
    copied: &mut usize,
    skipped: &mut usize,
    deferred: &mut usize,
    samples: &mut Vec<String>,
) {
    if !src.is_dir() {
        return;
    }
    if !dst.exists() && !dry_run {
        let _ = fs::create_dir_all(dst);
    }
    let Ok(entries) = fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let sp = entry.path();
        let dp = dst.join(entry.file_name());
        if sp.is_dir() {
            align_storage_dir(&sp, &dp, dry_run, copied, skipped, deferred, samples);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".bak") || is_secret_file(&name) {
            *skipped += 1;
            continue;
        }
        if is_merge_only(&name) {
            *deferred += 1;
            continue;
        }
        let need = match (fs::read(&sp), fs::read(&dp)) {
            (Ok(a), Ok(b)) => a != b,
            (Ok(_), Err(_)) => true,
            _ => false,
        };
        if !need {
            continue;
        }
        *copied += 1;
        if samples.len() < 6 {
            samples.push(name);
        }
        if !dry_run {
            if let Some(parent) = dp.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::copy(&sp, &dp).is_err() {
                *copied -= 1;
                *skipped += 1;
            }
        }
    }
}

/// 云端画像缓存对齐：memory/<uid>_memory.md 源 -> 目标。
fn align_memory_profile(src: &str, dst: &str, dry_run: bool) -> Value {
    let sa = memory_dir().join(format!("{src}_memory.md"));
    let sb = memory_dir().join(format!("{dst}_memory.md"));
    if !sa.is_file() {
        return json!({ "changed": false, "skipped": true });
    }
    let same = match (fs::read(&sa), fs::read(&sb)) {
        (Ok(a), Ok(b)) => a == b,
        (Ok(_), Err(_)) => false,
        _ => true,
    };
    if same {
        return json!({ "changed": false });
    }
    let bytes = fs::metadata(&sa).map(|m| m.len()).unwrap_or(0);
    if !dry_run {
        if sb.is_file() {
            let root = backup_dir().join("memory").join(utc_iso());
            let _ = fs::create_dir_all(&root);
            let _ = fs::copy(&sb, root.join(format!("{dst}_memory.md")));
        }
        let _ = fs::copy(&sa, &sb);
    }
    json!({ "changed": true, "bytes": bytes })
}

/// L5：my-files.json 全账号并集合并（列表取并集去重，其余键首个非空值生效）。
fn merge_my_files_all(accounts: &[String], dry_run: bool) -> Value {
    let mut files: Vec<PathBuf> = Vec::new();
    for uid in accounts {
        let scoped = storage_dir()
            .join(format!("user-{uid}-personal"))
            .join("scoped");
        let Ok(entries) = fs::read_dir(&scoped) else {
            continue;
        };
        for scope in entries.flatten() {
            let f = scope.path().join("my-files.json");
            if f.is_file() {
                files.push(f);
            }
        }
    }
    if files.len() < 2 {
        return json!({ "files": files.len(), "changed": 0 });
    }

    let mut merged = Map::new();
    let mut parsed: Vec<(PathBuf, Map<String, Value>)> = Vec::new();
    for f in &files {
        let parsed_one = fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
        if let Ok(Value::Object(m)) = parsed_one {
            parsed.push((f.clone(), m));
        }
    }
    for (_, m) in &parsed {
        for (k, v) in m {
            match merged.get_mut(k) {
                None => {
                    merged.insert(k.clone(), v.clone());
                }
                Some(Value::Array(dst_list)) if v.is_array() => {
                    if let Value::Array(src_list) = v {
                        let mut all: Vec<String> = dst_list
                            .iter()
                            .chain(src_list.iter())
                            .map(|x| x.to_string())
                            .collect();
                        all.sort();
                        all.dedup();
                        *dst_list = all
                            .iter()
                            .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.clone())))
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }

    let mut changed = 0usize;
    if !dry_run {
        for (f, m) in &parsed {
            if *m != merged {
                if let Ok(out) = serde_json::to_string_pretty(&Value::Object(merged.clone())) {
                    let root = backup_dir().join("my-files").join(utc_iso());
                    let _ = fs::create_dir_all(&root);
                    let _ = fs::copy(f, root.join("my-files.json"));
                    if atomic_write(f, &out).is_ok() {
                        changed += 1;
                    }
                }
            }
        }
    } else {
        changed = parsed.iter().filter(|(_, m)| *m != merged).count();
    }
    json!({ "files": files.len(), "changed": changed, "keys": merged.len() })
}

// ---------------------------------------------------------------------------
// 总入口
// ---------------------------------------------------------------------------

/// 多账号对齐总入口（L1 备份 + L3 归属 + L4 文件 + L5 合并）。
///
/// dry_run=true 时只统计计划变更；任一开关都未打开时返回 noop。
pub fn align_data(target_uid: &str, source_uid: Option<&str>, opts: &AlignOptions) -> Value {
    let mut report = json!({
        "targetUid": target_uid,
        "dryRun": opts.dry_run,
        "automations": { "updated": 0, "outbox": 0 },
        "sessions": { "updated": 0, "triggerRemoved": false },
        "settings": {
            "claw": { "changed": 0, "skipped": true },
            "storage": { "copied": 0, "skipped": 0, "deferred": 0, "samples": [] },
            "memory": { "changed": false },
            "myFiles": { "files": 0, "changed": 0, "keys": 0 },
        },
    });
    let any = opts.align_automations || opts.align_sessions || opts.align_files;
    if !any || target_uid.is_empty() {
        report["noop"] = json!(true);
        return report;
    }

    let db = workbuddy_db_path();
    if !db.is_file() {
        report["error"] = json!("workbuddy.db 不存在");
        return report;
    }

    if !opts.dry_run {
        report["backup"] = backup_all();
    }

    if opts.align_automations {
        if let Ok((a, o)) = align_automations_owner_in_db(&db, target_uid, opts.dry_run) {
            report["automations"] = json!({ "updated": a, "outbox": o });
        }
    }

    if opts.align_sessions {
        match align_sessions_owner_in_db(&db, target_uid, opts.dry_run) {
            Ok((n, t)) => report["sessions"] = json!({ "updated": n, "triggerRemoved": t }),
            Err(e) => report["sessions"] = json!({ "error": e }),
        }
    }

    if opts.align_files {
        let source = source_uid
            .map(|s| s.to_string())
            .or_else(|| pick_source(target_uid));
        report["settings"]["sourceUid"] = json!(source);
        if let Some(src) = source {
            if src != target_uid {
                report["settings"]["claw"] =
                    align_settings_claw_users(&src, target_uid, opts.dry_run);

                let mut copied = 0usize;
                let mut skipped = 0usize;
                let mut deferred = 0usize;
                let mut samples: Vec<String> = Vec::new();
                for suffix in ["", "-personal"] {
                    let s = storage_dir().join(format!("user-{src}{suffix}"));
                    let d = storage_dir().join(format!("user-{target_uid}{suffix}"));
                    align_storage_dir(
                        &s, &d, opts.dry_run, &mut copied, &mut skipped, &mut deferred, &mut samples,
                    );
                }
                report["settings"]["storage"] = json!({
                    "copied": copied, "skipped": skipped, "deferred": deferred, "samples": samples,
                });

                report["settings"]["memory"] = align_memory_profile(&src, target_uid, opts.dry_run);
            }
        }
        let accounts = discover_accounts();
        report["settings"]["myFiles"] = merge_my_files_all(&accounts, opts.dry_run);
    }

    report
}

/// 项目侧栏同步 + 会话瘦身，追加进报告（真实执行与预览共用）。
///
/// 顺序固定：先补占位/删多余，再瘦身（占位行也纳入瘦身统计）。
/// `protected_ids` = 本次复制体，瘦身时跳过（不删、也不占保留名额）。
fn append_project_and_slim(
    report: &mut Value,
    target_uid: &str,
    source_uid: Option<&str>,
    opts: &AlignOptions,
    dry_run: bool,
    protected_ids: &[String],
) {
    if opts.sync_projects {
        if let Some(src) = source_uid {
            match crate::modules::projects_anchor::sync_project_set(src, target_uid, dry_run, false) {
                Ok(r) => report["projects"] = r,
                Err(e) => report["projects"] = json!({ "error": e }),
            }
        }
    }
    if opts.slim_keep > 0 {
        match crate::modules::projects_anchor::slim_sessions(
            target_uid,
            opts.slim_keep,
            dry_run,
            protected_ids,
        ) {
            Ok(r) => report["slim"] = r,
            Err(e) => report["slim"] = json!({ "error": e }),
        }
    }
}

/// 切号预览（dry_run）：统计「对齐 + 项目侧栏 + 会话瘦身」将发生的变更。
///
/// 与 [`post_close_sync`] 的区别：**不写库、不写快照、不写云端**。
/// 主题跟随只给出提示占位——`sync_theme_for_switch` 会写 leveldb 与云端主题，
/// 预览阶段绝不能调用。
pub fn preview_sync(target_acc: &Value, opts: &AlignOptions) -> Option<Value> {
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty()
        || !(opts.align_automations || opts.align_sessions || opts.align_files || opts.sync_projects || opts.slim_keep > 0)
    {
        return None;
    }
    let source_uid = crate::modules::session::current_user_uid();
    let mut dry_opts = opts.clone();
    dry_opts.dry_run = true;
    let mut report = align_data(&target_uid, source_uid.as_deref(), &dry_opts);
    report["dryRun"] = json!(true);

    append_project_and_slim(
        &mut report,
        &target_uid,
        source_uid.as_deref(),
        opts,
        true,
        &[], // 预览不复制，无保护名单；真实执行时会跳过复制体，实际删除数可能更少
    );

    if opts.align_files {
        report["settings"]["theme"] = json!({ "planned": true });
    }
    Some(report)
}

/// 切号「关进程之后」的数据后置同步：归属/文件对齐 + 界面主题跟随。
///
/// 从 `switch.rs` 内联块下沉到这里（本地专属文件），使 switch.rs 的本地改动保持最小。
/// 返回 `align_report`；主题跟随（原独立 theme_report）已并入报告的 `settings.theme`
/// 子项——主题即账号外观设置，归入「设置同步」呈现（2026-09-12 定稿）。
/// `protected_ids` = 本次切号刚复制到目标账号的会话 id：瘦身时必须跳过，
/// 否则「复制多条同项目会话 + 瘦身」会让复制体互相挤掉，用户只看到 1 条。
pub fn post_close_sync(
    target_acc: &Value,
    opts: &AlignOptions,
    protected_ids: &[String],
) -> Option<Value> {
    let target_uid = account_uid(target_acc);
    if target_uid.is_empty()
        || !(opts.align_automations || opts.align_sessions || opts.align_files || opts.sync_projects || opts.slim_keep > 0)
    {
        return None;
    }
    let source_uid = crate::modules::session::current_user_uid();
    let mut full_opts = opts.clone();
    full_opts.dry_run = false;
    let mut report = align_data(&target_uid, source_uid.as_deref(), &full_opts);

    // 项目侧栏同步 + 会话瘦身（真实执行与预览共用，见 append_project_and_slim）。
    append_project_and_slim(
        &mut report,
        &target_uid,
        source_uid.as_deref(),
        opts,
        false,
        protected_ids,
    );

    // 主题跟随账号：本地继承（leveldb 注入，启动瞬间生效）+ 云端继承
    // （把 target 的云端外观选择改成 source 的值，根治回跳）。并入「设置同步」。
    let source_acc = source_uid.as_deref().and_then(|uid| {
        crate::modules::account::load_accounts()
            .into_iter()
            .find(|a| account_uid(a) == uid)
    });
    let source_token = source_acc
        .as_ref()
        .and_then(|a| crate::modules::account::get_str(a, "access_token"));
    let target_token = crate::modules::account::get_str(target_acc, "access_token");
    report["settings"]["theme"] = crate::modules::ui_theme::sync_theme_for_switch(
        source_uid.as_deref(),
        &target_uid,
        source_token.as_deref(),
        target_token.as_deref(),
    );
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_switch_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn setup(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE automations (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                owner_user_id TEXT,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER
            );
            CREATE TABLE automation_delivery_outbox (
                id TEXT PRIMARY KEY,
                automation_id TEXT NOT NULL,
                owner_user_id TEXT,
                status TEXT NOT NULL,
                finished_at INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT,
                title TEXT,
                created_at INTEGER,
                updated_at INTEGER,
                deleted_at INTEGER
            );",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO automations (id, name, owner_user_id, created_at, updated_at, deleted_at)
             VALUES
               ('a-1', '旧账号的自动化', 'uid-a', 1, 1, NULL),
               ('a-2', '已删除的自动化', 'uid-a', 1, 1, 100),
               ('a-3', '已是目标账号',    'uid-b', 1, 1, NULL),
               ('a-4', '无归属 legacy',   NULL,    1, 1, NULL);
             INSERT INTO automation_delivery_outbox
               (id, automation_id, owner_user_id, status, finished_at, created_at, updated_at)
             VALUES
               ('o-1', 'a-1', 'uid-a', 'pending', NULL, 1, 1),
               ('o-2', 'a-1', 'uid-a', 'finished', 999, 1, 1);
             INSERT INTO sessions (id, user_id, title, created_at, updated_at, deleted_at)
             VALUES
               ('s-1', 'uid-a', '会话A', 1, 1, NULL),
               ('s-2', 'uid-b', '会话B', 1, 1, NULL);",
        )
        .unwrap();
    }

    #[test]
    fn align_automations_moves_live_rows_to_target() {
        let db = temp_db("align_auto");
        setup(&db);

        let (n, o) = align_automations_owner_in_db(&db, "uid-b", false).unwrap();
        assert_eq!(n, 2, "a-1(owner 不同)/a-4(NULL) 两行；a-2 已软删、a-3 已是目标，均不动");
        assert_eq!(o, 1, "只有未投递完成的 o-1 会被对齐");

        let conn = Connection::open(&db).unwrap();
        let get = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT owner_user_id FROM automations WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .ok()
        };
        assert_eq!(get("a-1").as_deref(), Some("uid-b"));
        assert_eq!(get("a-2").as_deref(), Some("uid-a"), "软删行保留原归属");
        assert_eq!(get("a-3").as_deref(), Some("uid-b"));
        assert_eq!(get("a-4").as_deref(), Some("uid-b"), "legacy 无归属行一并接管");

        let outbox_owner: String = conn
            .query_row(
                "SELECT owner_user_id FROM automation_delivery_outbox WHERE id = 'o-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outbox_owner, "uid-a", "已完成的投递行不动");
    }

    #[test]
    fn align_automations_dry_run_does_not_write() {
        let db = temp_db("align_auto_dry");
        setup(&db);
        let (n, _o) = align_automations_owner_in_db(&db, "uid-b", true).unwrap();
        assert_eq!(n, 2, "dry-run 也要统计计划行数");
        let owner: String = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT owner_user_id FROM automations WHERE id='a-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner, "uid-a", "dry-run 不落盘");
    }

    #[test]
    fn align_sessions_moves_rows_and_dry_run_noop() {
        let db = temp_db("align_sessions");
        setup(&db);
        let (n, _t) = align_sessions_owner_in_db(&db, "uid-b", false).unwrap();
        assert_eq!(n, 1, "只有 s-1 需要 move");
        let owner: String = Connection::open(&db)
            .unwrap()
            .query_row("SELECT user_id FROM sessions WHERE id='s-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "uid-b");

        let db2 = temp_db("align_sessions_dry");
        setup(&db2);
        let (n2, _) = align_sessions_owner_in_db(&db2, "uid-b", true).unwrap();
        assert_eq!(n2, 1);
        let owner2: String = Connection::open(&db2)
            .unwrap()
            .query_row("SELECT user_id FROM sessions WHERE id='s-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner2, "uid-a", "dry-run 不落盘");
    }

    #[test]
    fn align_missing_db_is_noop() {
        let db = temp_db("missing");
        assert_eq!(
            align_automations_owner_in_db(&db, "uid-b", false).unwrap(),
            (0, 0)
        );
        assert_eq!(
            align_sessions_owner_in_db(&db, "uid-b", false).unwrap(),
            (0, false)
        );
    }

    #[test]
    fn secret_key_rules_match_multi_sync() {
        assert!(is_secret_key("botToken"));
        assert!(is_secret_key("channel_id"));
        assert!(is_secret_key("ownerId"));
        // multi_sync 原逻辑：输入键名不剥点、规则键才剥点，故 "master.key" 键名匹配不上
        // （文件名场景由 SECRET_FILENAMES 兜底）——保持口径一致
        assert!(!is_secret_key("master.key"));
        assert!(!is_secret_key("nickname"));
        assert!(!is_secret_key("themeColor"));
        assert!(is_secret_file("master.key"));
        assert!(!is_secret_file("expert.json"));
        assert!(is_merge_only("my-files.json"));
    }
}
