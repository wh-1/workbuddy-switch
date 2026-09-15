//! 项目锚点同步——切号后侧栏项目跟随 + 会话瘦身（本地专属，上游零冲突）。
//!
//! 背景：WorkBuddy 侧栏「项目」不是一等实体，是会话按 cwd 分组的派生视图——
//! 某账号下某 cwd 的存活会话数归零，该项目条目就从侧栏消失。
//!
//! 语义（主人 2026-09-12 定稿）：切号 A→B 时，B 的存活会话 cwd 集合对齐到 A：
//!   - 缺的项目 → 插 1 行空白占位会话（新 uuid，附最小空 JSONL）
//!   - 多的项目 → 该项目下 B 的全部存活会话软删（deleted_at 打标，JSONL 保留）
//!   - 删项目的正确姿势：在当前账号删光该项目会话，其余账号切号时自动跟删
//!   - 级联误删防护：源清单为空或较上次快照骤减 ≥30% 时中断（force 可跳过）
//!
//! 会话瘦身（独立入口）：每账号每 cwd 保留 updated_at 最新 keep 条，其余软删。
//!
//! 为什么不用复制/归属改写：复制会产生重复 JSONL（Token 重复统计，上游 #32）
//! 与侧栏副本雪球（上游 #9 被拒）；归属改写抹平历史归属。占位行零正文、
//! 零 usage，统计器天然不重复计数。
//!
//! 数据事实（2026-09-12 实测）：
//!   - sessions 必填列仅 id/cwd/user_id/created_at/updated_at（status 有默认值）
//!   - JSONL workspace 目录名 = cwd 盘符小写 + `:` 删除 + `\`/`/` → `-`
//!     （如 `C:\Users\Name\WorkBuddy\GIS` → `c-Users-Name-WorkBuddy-GIS`）
//!   - 当前库无「有行无 JSONL」先例 → 占位行附最小空 JSONL 保 WB 打开不报错

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::modules::config::{backup_dir, home_dir, now_ms, utc_iso};
use crate::modules::session::{backup_workbuddy_db, open_db, table_exists, workbuddy_db_path};

/// 遗留触发器（早期版本装的 user_id 自动统一触发器，会干扰占位行写入）。
/// 必须与 align.rs 的 LEGACY_TRIGGER 保持一致。
const LEGACY_TRIGGER: &str = "trg_unify_session_uid";

/// 上次同步的项目清单快照（级联误删防护的比对基准）。
fn snapshot_path() -> PathBuf {
    home_dir().join(".wb-switch").join("project_set_snapshot.json")
}

/// 读快照中某账号的项目清单（path 注入以便测试隔离）。
fn load_snapshot_from(path: &Path, uid: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v.get(uid)
        .and_then(|u| u.get("cwds"))
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 写快照（仅真实执行成功后调用）。
fn save_snapshot_to(path: &Path, uid: &str, cwds: &BTreeSet<String>) {
    let mut root = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    root.insert(
        uid.to_string(),
        json!({ "ts": now_ms(), "cwds": cwds.iter().collect::<Vec<_>>() }),
    );
    if let Ok(out) = serde_json::to_string_pretty(&Value::Object(root)) {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(path, out);
    }
}

/// projects 根目录。
fn projects_root() -> PathBuf {
    home_dir().join(".workbuddy").join("projects")
}

/// cwd → JSONL workspace 目录名。
/// 规则（33 个现役目录反推验证）：首字符（盘符）小写，`:` 删除，`\`/`/` → `-`。
pub(crate) fn workspace_dir_name(cwd: &str) -> String {
    let mut s = String::with_capacity(cwd.len() + 4);
    for (i, ch) in cwd.chars().enumerate() {
        match ch {
            ':' => {}
            '\\' | '/' => s.push('-'),
            _ if i == 0 => s.extend(ch.to_lowercase()),
            _ => s.push(ch),
        }
    }
    s
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(iter) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    let names: Vec<String> = iter.flatten().collect();
    names.iter().any(|name| name == column)
}

/// 若遗留触发器存在则删除，返回是否发生了删除。
pub(crate) fn drop_legacy_trigger(conn: &Connection) -> bool {
    let has: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1)",
            [LEGACY_TRIGGER],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 1;
    if has {
        let _ = conn.execute(&format!("DROP TRIGGER {LEGACY_TRIGGER}"), []);
    }
    has
}

/// 某账号的存活会话 cwd 去重集合（排序稳定）。
pub(crate) fn project_cwds(conn: &Connection, uid: &str) -> BTreeSet<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT cwd FROM sessions \
         WHERE deleted_at IS NULL AND user_id = ?1 AND cwd IS NOT NULL AND cwd != ''",
    ) {
        Ok(s) => s,
        Err(_) => return BTreeSet::new(),
    };
    let rows = stmt
        .query_map([uid], |r| r.get::<_, String>(0))
        .map(|iter| iter.flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    rows.into_iter().collect()
}

/// 读上次快照中某账号的项目清单（真实路径包装，供外部诊断用）。
#[allow(dead_code)]
fn load_snapshot(uid: &str) -> Vec<String> {
    load_snapshot_from(&snapshot_path(), uid)
}

/// 给目标账号补一个项目的占位会话：INSERT sessions 行 + 最小空 JSONL。
/// 返回新会话 id。列集合按 PRAGMA 动态适配（sessions 是动态列表）。
fn insert_anchor_session(conn: &Connection, projects: &Path, uid: &str, cwd: &str) -> Result<String, String> {
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();

    // JSONL：workspace 目录 = cwd 编码；空文件即可（新对话在首条消息前无正文）。
    let ws = projects.join(workspace_dir_name(cwd));
    if !ws.is_dir() {
        std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    }
    let jsonl = ws.join(format!("{new_id}.jsonl"));
    if !jsonl.exists() {
        std::fs::write(&jsonl, "").map_err(|e| e.to_string())?;
    }

    // 动态列：必填 id/cwd/user_id/title/created_at/updated_at + 存在的可选列。
    let mut cols: Vec<&str> = vec!["id", "cwd", "user_id", "title", "created_at", "updated_at"];
    for extra in ["last_activity_at", "status", "is_playground", "source_mode", "mode"] {
        if column_exists(conn, "sessions", extra) {
            cols.push(extra);
        }
    }
    // 可选列取值：占位行统一「已完成、非 playground、普通工作模式」。
    let optional_value = |col: &str| -> rusqlite::types::Value {
        use rusqlite::types::Value as V;
        match col {
            "status" => V::Text("completed".into()),
            "is_playground" => V::Integer(0),
            "source_mode" => V::Text("working".into()),
            "mode" => V::Text("craft".into()),
            _ => V::Integer(now), // last_activity_at
        }
    };
    let mut params: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(new_id.clone()),
        rusqlite::types::Value::Text(cwd.to_string()),
        rusqlite::types::Value::Text(uid.to_string()),
        rusqlite::types::Value::Text("（项目锚点）".into()),
        rusqlite::types::Value::Integer(now),
        rusqlite::types::Value::Integer(now),
    ];
    for c in cols.iter().skip(6) {
        params.push(optional_value(c));
    }
    let col_sql = cols.join(", ");
    let placeholders = (1..=params.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO sessions ({col_sql}) VALUES ({placeholders})");
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(|e| e.to_string())?;
    Ok(new_id)
}

/// 核心：目标账号项目集合对齐到源账号（补缺 + 删多），dry_run 优先。
pub(crate) fn sync_project_set_in_db(
    db_path: &Path,
    projects: &Path,
    snapshot: &Path,
    src_uid: &str,
    dst_uid: &str,
    dry_run: bool,
    force: bool,
) -> Result<Value, String> {
    if src_uid == dst_uid {
        return Ok(json!({ "skipped": "source == target" }));
    }
    let Some(conn) = open_db(db_path, false) else {
        return Err("无法打开 workbuddy.db".into());
    };
    if !table_exists(&conn, "sessions") {
        return Ok(json!({ "skipped": "no sessions table" }));
    }

    let src_set = project_cwds(&conn, src_uid);
    let dst_set = project_cwds(&conn, dst_uid);

    // —— 级联误删防护：源清单清零 / 骤减 ≥30%（对照上次快照）时中断 ——
    let snap = load_snapshot_from(snapshot, src_uid);
    if !force && !snap.is_empty() {
        if src_set.is_empty() {
            return Err(format!(
                "防护触发：源账号 {src_uid} 项目清单为空（上次快照 {} 个项目）。若确认要清空请 force。",
                snap.len()
            ));
        }
        if (snap.len() as f64) * 0.7 > src_set.len() as f64 {
            return Err(format!(
                "防护触发：源账号项目清单 {} → {}（骤减超 30%）。若确认请 force。",
                snap.len(),
                src_set.len()
            ));
        }
    }

    let to_add: Vec<&String> = src_set.difference(&dst_set).collect();
    let to_remove: Vec<&String> = dst_set.difference(&src_set).collect();

    // 删多：目标账号多余项目下的全部存活会话软删（dry-run 只统计不落盘）。
    let mut removed: Vec<Value> = Vec::new();
    let mut removed_count = 0usize;
    if !dry_run && !to_remove.is_empty() {
        for cwd in &to_remove {
            let n = conn
                .execute(
                    "UPDATE sessions SET deleted_at = ?2, updated_at = ?3 \
                     WHERE deleted_at IS NULL AND user_id = ?1 AND cwd = ?4",
                    rusqlite::params![dst_uid, now_ms(), now_ms(), cwd],
                )
                .map_err(|e| e.to_string())?;
            if n > 0 {
                removed_count += n as usize;
                removed.push(json!({ "cwd": cwd, "sessions": n }));
            }
        }
    } else if dry_run {
        for cwd in &to_remove {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sessions \
                     WHERE deleted_at IS NULL AND user_id = ?1 AND cwd = ?2",
                    rusqlite::params![dst_uid, cwd],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if n > 0 {
                removed_count += n as usize;
                removed.push(json!({ "cwd": cwd, "sessions": n, "planned": true }));
            }
        }
    }

    // 补缺：每项目 1 行占位会话（真实执行时才写 JSONL）。
    let mut added: Vec<Value> = Vec::new();
    if !dry_run {
        drop_legacy_trigger(&conn);
        for cwd in &to_add {
            let id = insert_anchor_session(&conn, projects, dst_uid, cwd)?;
            added.push(json!({ "cwd": cwd, "sessionId": id }));
        }
    } else {
        for cwd in &to_add {
            added.push(json!({ "cwd": cwd, "planned": true }));
        }
    }

    let report = json!({
        "source": src_uid,
        "target": dst_uid,
        "sourceProjects": src_set.len(),
        "targetProjects": dst_set.len(),
        "added": added,
        "addedCount": added.len(),
        "removedProjects": removed,
        "removedCount": removed_count,
        "dryRun": dry_run,
    });

    if !dry_run {
        save_snapshot_to(snapshot, src_uid, &src_set);
    }
    Ok(report)
}

/// 会话瘦身：每账号每 cwd 保留 updated_at 最新 keep 条，其余软删。
///
/// `exclude` = 本次切号刚复制过来的会话 id —— 它们既不被删、也不占用保留名额，
/// 否则「复制多条同项目会话 + 瘦身 keep=1」会让用户只看到 1 条（复制体互相挤掉）。
/// 云端删除上下文（`None` = 只做本地软删，保持旧行为）。
///
/// 归属判据 = `<configDir>/edge-sync-mapping-v4.db` 的 `msg_channel`；
/// 只有 `convmsg:<uid>`（本次瘦身的目标账号）才动云端 —— 其余（无映射 / 归别的账号）
/// 一律**只本地软删**，避免用错账号 token 触发 403 白跑、或误伤他人会话。
pub struct SlimCloudCtx {
    /// 本次瘦身的目标账号 uid。
    pub uid: String,
    /// 该账号的 `access_token`（取不到则整个云端环节跳过）。
    pub token: Option<String>,
    /// `sid → msg_channel` 全量映射。
    pub channels: std::collections::HashMap<String, String>,
}

impl SlimCloudCtx {
    /// 该 sid 是否「云端归属本次目标账号」⇒ 可以放心删云端。
    fn owns(&self, sid: &str) -> bool {
        self.channels
            .get(sid)
            .map(|ch| crate::modules::cloud_conv::channel_uid(ch) == self.uid)
            .unwrap_or(false)
    }
}

/// 会话瘦身的本地实现（不含云端）。旧签名保留，供既有调用与测试使用。
#[allow(dead_code)] // 仅测试与旧调用点用；生产路径统一走 _cloud 版
pub(crate) fn slim_sessions_in_db(
    db_path: &Path,
    uid: &str,
    keep: i64,
    dry_run: bool,
    exclude: &[String],
) -> Result<Value, String> {
    slim_sessions_in_db_cloud(db_path, uid, keep, dry_run, exclude, None)
}

/// 会话瘦身（可带云端删除）。
///
/// 每条 victim 的处理顺序（**先云端、后本地**，保证失败可回退）：
///   `ch == convmsg:<uid>` 且有 token → 调云端删除
///        · 成功 / 404  → 本地软删，计 `cloudDeleted`
///        · 403         → 归属与映射不符（映射记错）⇒ **放弃云端**，本地照常软删，计 `cloudForbidden`
///        · 其它失败    → **本地不软删**（留到下次重试），计 `cloudFailed`
///   `ch` 缺失                                    → 本地软删，计 `cloudNoMapping`
///   `ch` 归别的账号                              → 本地软删，计 `cloudForeign`
/// dry_run 只统计「将调用云端几条」，不发起请求。
pub(crate) fn slim_sessions_in_db_cloud(
    db_path: &Path,
    uid: &str,
    keep: i64,
    dry_run: bool,
    exclude: &[String],
    cloud: Option<&SlimCloudCtx>,
) -> Result<Value, String> {
    let keep = keep.max(1);
    let Some(conn) = open_db(db_path, false) else {
        return Err("无法打开 workbuddy.db".into());
    };
    if !table_exists(&conn, "sessions") {
        return Ok(json!({ "skipped": "no sessions table" }));
    }

    // 每组：cwd 分组，按 updated_at 倒序，第 keep 条之后的全软删。
    // 排除集（本次复制体）：外层保证不被删，子查询里保证不占保留名额。
    let excl: Vec<&String> = exclude.iter().filter(|s| !s.is_empty()).collect();
    let excl_sql = |alias: &str| -> String {
        if excl.is_empty() {
            return String::new();
        }
        let marks = (0..excl.len())
            .map(|i| format!("?{}", i + 3)) // ?1=uid, ?2=keep
            .collect::<Vec<_>>()
            .join(",");
        format!(" AND {alias}id NOT IN ({marks})")
    };
    let sql = format!(
        "SELECT id, cwd FROM sessions s \
         WHERE deleted_at IS NULL AND user_id = ?1{} AND id NOT IN (\
           SELECT id FROM sessions \
           WHERE deleted_at IS NULL AND user_id = ?1 AND cwd = s.cwd{} \
           ORDER BY updated_at DESC LIMIT ?2\
         )",
        excl_sql("s."),
        excl_sql(""),
    );
    let mut params: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(uid.to_string()),
        rusqlite::types::Value::Integer(keep),
    ];
    for e in &excl {
        params.push(rusqlite::types::Value::Text((*e).clone()));
    }
    let victims: Vec<(String, String)> = {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };

    let mut deleted = 0usize;
    // 云端侧统计
    // `deleted` = 合计（200 + 404）；`removed` = 真被这次请求删掉（200）；
    // `already_gone` = 云端本来就没有（404）。**只有 200 才能证明删成功** ——
    // 两个数字混在一起时，报告无法自证「到底真删了没」（2026-09-15 实测教训）。
    let mut cloud_deleted = 0usize;
    let mut cloud_removed = 0usize;
    let mut cloud_already_gone = 0usize;
    let mut cloud_forbidden = 0usize;
    let mut cloud_failed = 0usize;
    let mut cloud_no_mapping = 0usize;
    let mut cloud_no_token = 0usize;
    let mut cloud_foreign = 0usize;
    let mut cloud_planned = 0usize; // dry_run 下「将调用云端」条数
    let mut cloud_kept: Vec<String> = Vec::new(); // 因云端失败而保留未删的 sid

    let want_cloud = cloud.is_some();
    // 云端开启但没有 token ⇒ 退化为「只本地」，避免误报为 foreign
    let cloud_token_ok = cloud.map(|c| c.token.is_some()).unwrap_or(false);

    for (id, _cwd) in &victims {
        let mut allow_local = true;
        if want_cloud {
            let c = cloud.expect("checked by want_cloud");
            if c.owns(id) {
                if !cloud_token_ok {
                    // 归属对得上但没凭证 ⇒ 云端这轮跳过，本地照常软删
                    cloud_no_token += 1;
                } else if dry_run {
                    cloud_planned += 1;
                } else {
                    let token = c.token.as_deref().unwrap_or("");
                    let outcome = crate::modules::cloud_conv::delete_conversation(token, id);
                    match &outcome {
                        crate::modules::cloud_conv::CloudDelete::Deleted => {
                            cloud_deleted += 1;
                            cloud_removed += 1;
                        }
                        crate::modules::cloud_conv::CloudDelete::AlreadyGone => {
                            cloud_deleted += 1;
                            cloud_already_gone += 1;
                        }
                        crate::modules::cloud_conv::CloudDelete::Forbidden => {
                            cloud_forbidden += 1;
                        }
                        crate::modules::cloud_conv::CloudDelete::Failed(msg) => {
                            cloud_failed += 1;
                            cloud_kept.push(format!("{id}（{msg}）"));
                            // 铁律 3：云端没删掉 ⇒ 本地保留，下次切号再试
                            allow_local = false;
                        }
                    }
                    append_cloud_delete_audit(uid, id, &outcome);
                }
            } else if c.channels.contains_key(id.as_str()) {
                cloud_foreign += 1;
            } else {
                cloud_no_mapping += 1;
            }
        }

        if !dry_run && allow_local {
            let n = conn
                .execute(
                    "UPDATE sessions SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                    rusqlite::params![id, now_ms()],
                )
                .map_err(|e| e.to_string())?;
            deleted += n;
        }
    }

    let mut groups: std::collections::BTreeMap<&str, i64> = Default::default();
    for (_id, cwd) in &victims {
        *groups.entry(cwd.as_str()).or_insert(0) += 1;
    }
    let mut report = json!({
        "uid": uid,
        "keep": keep,
        "excluded": excl.len(),
        "planned": victims.len(),
        "deleted": deleted,
        "groups": groups.iter().map(|(c, n)| json!({ "cwd": c, "count": n })).collect::<Vec<_>>(),
        "dryRun": dry_run,
    });
    if want_cloud {
        report["cloud"] = json!({
            "enabled": true,
            "tokenReady": cloud_token_ok,
            "planned": cloud_planned,
            "deleted": cloud_deleted,
            "removed": cloud_removed,
            "alreadyGone": cloud_already_gone,
            "forbidden": cloud_forbidden,
            "failed": cloud_failed,
            "keptLocal": cloud_kept.len(),
            "samples": cloud_kept.iter().take(5).cloned().collect::<Vec<_>>(),
            "noMapping": cloud_no_mapping,
            "noToken": cloud_no_token,
            "foreign": cloud_foreign,
        });
    }
    Ok(report)
}

/// 逐条云端删除审计 → `~/.wb-switch/cloud_delete_log.jsonl`（一行一条 JSON）。
///
/// 为什么单独留这个文件：统计报告里 `deleted` 是 200 与 404 的**合计**，
/// 事后无法回答「这次到底真删了几条」。App 侧日志也不记录我们直连的请求
/// （我们走 `POST /console/as/conversations/{sid}/delete`，不经 App 的
/// `syncDeleteConversation`）⇒ 逐条落盘是**唯一**可事后核验的证据源。
///
/// 只记录真实执行（非 dry_run），写失败不影响业务。
fn append_cloud_delete_audit(uid: &str, sid: &str, outcome: &crate::modules::cloud_conv::CloudDelete) {
    use std::io::Write;
    let path = home_dir().join(".wb-switch").join("cloud_delete_log.jsonl");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 简易轮转：超过 2 MiB 直接换名覆盖，避免无限增长（审计只需近期）。
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 2 * 1024 * 1024 {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.old"));
    }
    let (result, detail) = match outcome {
        crate::modules::cloud_conv::CloudDelete::Deleted => ("removed", "http 200"),
        crate::modules::cloud_conv::CloudDelete::AlreadyGone => ("already_gone", "http 404"),
        crate::modules::cloud_conv::CloudDelete::Forbidden => ("forbidden", "http 403"),
        crate::modules::cloud_conv::CloudDelete::Failed(m) => ("failed", m.as_str()),
    };
    let line = json!({
        "at": now_ms(),
        "ts": utc_iso(),
        "uid": uid,
        "sid": sid,
        "result": result,
        "detail": detail,
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// 备份目录时间戳：毫秒级。
///
/// 不能用 `utc_iso()`（只到秒）——一次切号里项目侧栏同步与会话瘦身会连续各备份一次，
/// 秒级目录名重名导致后一次覆盖前一次，**pre-同步的快照丢失**，回滚点被后移。
fn backup_stamp() -> String {
    format!("{}-{}Z", utc_iso().trim_end_matches('Z'), now_ms())
}

/// 真实路径包装：同步项目侧栏（含 db 备份）。
pub fn sync_project_set(src_uid: &str, dst_uid: &str, dry_run: bool, force: bool) -> Result<Value, String> {
    if !dry_run {
        let root = backup_dir().join("projects_anchor").join(backup_stamp());
        backup_workbuddy_db(&root);
    }
    sync_project_set_in_db(
        &workbuddy_db_path(),
        &projects_root(),
        &snapshot_path(),
        src_uid,
        dst_uid,
        dry_run,
        force,
    )
}

/// 真实路径包装：会话瘦身（含 db 备份）。`exclude` = 不参与瘦身的会话 id（本次复制体）。
///
/// 云端删除是瘦身的默认组成部分（不再单独设开关）：装配云端上下文——读映射表 +
/// 取该账号 token；任何一步缺失都不致命，会在报告的 `slim.cloud.tokenReady` 里
/// 如实反映，流程退化为「只本地瘦身」。删除按 `cloud_conv` 的三条铁律走。
///
/// 末尾追加**对账阶段**（`cloud.reconcile`）：映射行全集 × 本机 sessions，
/// 清「本机已软删但云端还在」的残留；映射行有、本机无行的 Unknown 只计数不删
/// （可能是其他设备的活会话）。映射库只读，绝不写删。
pub fn slim_sessions(
    uid: &str,
    keep: i64,
    dry_run: bool,
    exclude: &[String],
) -> Result<Value, String> {
    if !dry_run {
        let root = backup_dir().join("projects_anchor").join(backup_stamp());
        backup_workbuddy_db(&root);
    }
    let ctx = Some(SlimCloudCtx {
        uid: uid.to_string(),
        token: crate::modules::cloud_conv::token_of(uid),
        channels: crate::modules::cloud_conv::mapping_channels(),
    });
    let mut report = slim_sessions_in_db_cloud(
        &workbuddy_db_path(),
        uid,
        keep,
        dry_run,
        exclude,
        ctx.as_ref(),
    )?;
    if let Some(cloud) = report.get_mut("cloud") {
        cloud["reconcile"] = reconcile_cloud_stage(uid, dry_run);
        cloud["inventory"] = inventory_cloud_stage(uid);
    }
    Ok(report)
}

/// 对账阶段：映射行全集 × 本机 sessions → 清「本机已软删但云端还在」。
/// 任何一步失败都不致命（返回 error 对象，不影响瘦身主流程）。
fn reconcile_cloud_stage(uid: &str, dry_run: bool) -> Value {
    let rows = crate::modules::cloud_conv::mapping_rows();
    if rows.is_empty() {
        return json!({ "skipped": "no mapping db" });
    }
    let (alive, deleted) = match read_local_alive_deleted(&workbuddy_db_path()) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e }),
    };
    let token = crate::modules::cloud_conv::token_of(uid);
    crate::modules::cloud_reconcile::sweep_stale_mappings(
        &rows,
        &alive,
        &deleted,
        token.as_deref(),
        dry_run,
        crate::modules::cloud_conv::delete_conversation,
    )
}

/// 全账巡检阶段（**只读，永不删**）：云端全账 × 本机 sessions → 分类计数。
///
/// 吃 `GET /v2/as/conversations/?type=all`（跨设备全账，2026-09-15 打通）——
/// 补上 `reconcile_cloud_stage` 的盲区：映射行那本账里**没有**的云端会话
/// （他机创建 / 云端自动化）它根本看不见。
///
/// 只报数，不做任何删除：`foreign`（本机无痕迹）多半是别的设备的活会话，
/// 删了就伤到别人。dry_run 与否都照跑（无副作用）。
fn inventory_cloud_stage(uid: &str) -> Value {
    let (alive, deleted) = match read_local_alive_deleted(&workbuddy_db_path()) {
        Ok(v) => v,
        Err(e) => return json!({ "error": e }),
    };
    let token = crate::modules::cloud_conv::token_of(uid);
    crate::modules::cloud_reconcile::inventory(
        token.as_deref(),
        &alive,
        &deleted,
        crate::modules::cloud_conv::list_conversation_ids,
    )
}

/// 读本机 sessions 的存活/软删 id 集合（对账用，只读）。
fn read_local_alive_deleted(
    db: &std::path::Path,
) -> Result<(std::collections::HashSet<String>, std::collections::HashSet<String>), String> {
    use std::collections::HashSet;
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("打开 workbuddy.db 失败: {e}"))?;
    let mut alive = HashSet::new();
    let mut deleted = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT id, deleted_at FROM sessions")
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    for row in rows {
        let (id, deleted_at) = row.map_err(|e| format!("读取 sessions 行失败: {e}"))?;
        if deleted_at.is_none() {
            alive.insert(id);
        } else {
            deleted.insert(id);
        }
    }
    Ok((alive, deleted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 备份戳必须带毫秒：一次切号内连续两次备份若重名，后一次会覆盖前一次的快照。
    #[test]
    fn backup_stamp_is_unique_per_millisecond() {
        let a = backup_stamp();
        assert!(a.ends_with('Z'), "保持与 utc_iso 一致的 Z 后缀: {a}");
        let sec_len = "2026-09-13T20-04-43Z".len();
        assert!(a.len() > sec_len, "毫秒后缀不能丢: {a}");
        // 连续两次至少不因「格式不含毫秒」而重名
        let millis = a.trim_end_matches('Z').rsplit('-').next().unwrap_or("");
        assert!(
            millis.chars().all(|c| c.is_ascii_digit()) && millis.len() >= 12,
            "毫秒段应为 now_ms 的数值: {a}"
        );
    }

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb_anchor_test_{}_{name}.db",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn temp_projects(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wb_anchor_prj_{}_{name}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn setup(db: &Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                user_id TEXT NOT NULL,
                title TEXT,
                status TEXT DEFAULT 'Pending',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                last_activity_at INTEGER,
                deleted_at INTEGER,
                is_playground INTEGER DEFAULT 0,
                source_mode TEXT,
                mode TEXT
            );",
        )
        .unwrap();
        for (id, cwd, uid, upd) in [
            ("s1", "D:\\p1", "uid-a", 1000),
            ("s2", "D:\\p1", "uid-a", 2000), // p1 两条，瘦身应删 s1（旧）
            ("s3", "D:\\p2", "uid-a", 1000),
            ("s4", "D:\\p3", "uid-b", 1000), // b 独有 → 删多目标
            ("s5", "D:\\p2", "uid-b", 1000), // b 在 p2 也有 → 不补
        ] {
            conn.execute(
                "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 't', 1, ?4)",
                rusqlite::params![id, cwd, uid, upd],
            )
            .unwrap();
        }
    }

    #[test]
    fn workspace_dir_name_encodes_cwd() {
        assert_eq!(workspace_dir_name("C:\\Users\\Name\\WorkBuddy\\GIS"), "c-Users-Name-WorkBuddy-GIS");
        assert_eq!(workspace_dir_name("D:\\work\\common\\repo-discipline"), "d-work-common-repo-discipline");
        assert_eq!(workspace_dir_name("C:\\Users\\Name\\WorkBuddy\\创业"), "c-Users-Name-WorkBuddy-创业");
    }

    #[test]
    fn sync_adds_missing_and_removes_extra() {
        let db = temp_db("sync");
        setup(&db);
        let prj = temp_projects("sync");
        // a: {p1,p2}；b: {p3,p2} → b 补 p1，删 p3
        let snap = temp_db("snapshot");
        let rep = sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", false, false).unwrap();
        assert_eq!(rep["addedCount"], 1);
        assert_eq!(rep["removedCount"], 1);

        let conn = Connection::open(&db).unwrap();
        let cwds = project_cwds(&conn, "uid-b");
        assert_eq!(cwds, BTreeSet::from(["D:\\p1".to_string(), "D:\\p2".to_string()]));
        // s4 已软删
        let s4: Option<i64> = conn
            .query_row("SELECT deleted_at FROM sessions WHERE id='s4'", [], |r| r.get(0))
            .unwrap();
        assert!(s4.is_some(), "多余项目会话被软删");
        // 占位行存在且无正文 JSONL 创建
        let placeholder: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE user_id='uid-b' AND cwd='D:\\p1' AND title='（项目锚点）'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(placeholder, 1);
        let jsonls: Vec<_> = std::fs::read_dir(prj.join("d-p1")).unwrap().flatten().collect();
        assert_eq!(jsonls.len(), 1, "占位 JSONL 已创建");
    }

    #[test]
    fn sync_dry_run_writes_nothing() {
        let db = temp_db("sync_dry");
        setup(&db);
        let prj = temp_projects("sync_dry");
        let snap = temp_db("snapshot");
        let rep = sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", true, false).unwrap();
        assert_eq!(rep["dryRun"], true);
        assert_eq!(rep["addedCount"], 1);
        let conn = Connection::open(&db).unwrap();
        let cwds = project_cwds(&conn, "uid-b");
        assert_eq!(cwds.len(), 2, "dry-run 不落盘");
        assert!(!prj.join("d-p1").exists(), "dry-run 不建 JSONL");
    }

    #[test]
    fn sync_guard_blocks_on_empty_source() {
        let db = temp_db("guard");
        setup(&db);
        let prj = temp_projects("guard");
        // 先建立快照：a → {p1,p2}
        let snap = temp_db("snapshot");
        sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", false, false).unwrap();
        // 清空 a 的会话 → 快照非空但清单空 → 阻断
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE sessions SET deleted_at=999 WHERE user_id='uid-a'", []).unwrap();
        drop(conn);
        let err = sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", false, false).unwrap_err();
        assert!(err.contains("防护触发"), "应触发级联防护: {err}");
        // force 放行
        let rep = sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", false, true).unwrap();
        assert_eq!(rep["removedCount"], 2, "force 后 b 的 p1/p2 占位与会话被清");
    }

    #[test]
    fn slim_keeps_latest_per_cwd() {
        let db = temp_db("slim");
        setup(&db);
        let rep = slim_sessions_in_db(&db, "uid-a", 1, false, &[]).unwrap();
        assert_eq!(rep["deleted"], 1, "p1 两条留最新，删 1");
        let conn = Connection::open(&db).unwrap();
        let alive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE user_id='uid-a' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive, 2, "p1 留 s2、p2 留 s3");
        let kept: String = conn
            .query_row("SELECT id FROM sessions WHERE user_id='uid-a' AND cwd='D:\\p1' AND deleted_at IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, "s2", "保留 updated_at 最新的");
    }

    /// 云端连带删除的**归属分流**：无映射 / 归本账号（无 token）/ 归别的账号 三类必须分得清，
    /// 且本用例**全程不联网**（token 传 `None` ⇒ 一律不发请求）。
    #[test]
    fn slim_cloud_routes_by_ownership_without_network() {
        let db = temp_db("slim_cloud");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, cwd TEXT, user_id TEXT, title TEXT,
                    created_at INTEGER, updated_at INTEGER, deleted_at INTEGER);",
            )
            .unwrap();
            // 每个 cwd 两条：旧的会被淘汰（keep=1 留最新）
            for (cwd, pfx) in [("D:\\p9", "none"), ("D:\\p8", "own"), ("D:\\p7", "foreign")] {
                for (suf, upd) in [("a", 100), ("b", 200)] {
                    conn.execute(
                        "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                         VALUES (?1, ?2, 'uid-a', 't', 1, ?3)",
                        rusqlite::params![format!("{pfx}_{suf}"), cwd, upd],
                    )
                    .unwrap();
                }
            }
        }
        // 只给 own_* 与 foreign_* 配映射；none_* 故意不配
        let mut channels = std::collections::HashMap::new();
        channels.insert("own_a".to_string(), "convmsg:uid-a".to_string());
        channels.insert("foreign_a".to_string(), "convmsg:uid-b".to_string());
        let ctx = SlimCloudCtx {
            uid: "uid-a".into(),
            token: None,
            channels,
        };

        // ① dry-run：只统计「将调云端」条数，不落盘、不发请求
        let rep = slim_sessions_in_db_cloud(&db, "uid-a", 1, true, &[], Some(&ctx)).unwrap();
        assert_eq!(rep["planned"], 3, "三个 cwd 各淘汰 1 条");
        assert_eq!(rep["deleted"], 0, "dry-run 不落盘");
        assert_eq!(rep["cloud"]["enabled"], true);
        assert_eq!(rep["cloud"]["tokenReady"], false);

        // ② 真执行但无 token ⇒ 三类都只本地软删，一条云端请求都不发
        let rep2 = slim_sessions_in_db_cloud(&db, "uid-a", 1, false, &[], Some(&ctx)).unwrap();
        assert_eq!(rep2["deleted"], 3, "三类都应完成本地软删");
        assert_eq!(rep2["cloud"]["deleted"], 0);
        assert_eq!(
            rep2["cloud"]["removed"], 0,
            "真删数（200）与 alreadyGone（404）必须分开报，否则事后无法自证删成功"
        );
        assert_eq!(rep2["cloud"]["alreadyGone"], 0);
        assert_eq!(rep2["cloud"]["failed"], 0, "无 token 不算失败，本地不被卡住");
        assert_eq!(rep2["cloud"]["noToken"], 1, "own_a：归属对但没凭证");
        assert_eq!(rep2["cloud"]["foreign"], 1, "foreign_a：云端归别的账号，不碰");
        assert_eq!(rep2["cloud"]["noMapping"], 1, "none_a：本机没有映射");

        // ③ 不勾云端 ⇒ 报告里根本没有 cloud 段（旧行为不变）
        let rep3 = slim_sessions_in_db_cloud(&db, "uid-b", 1, true, &[], None).unwrap();
        assert!(rep3.get("cloud").is_none(), "未开启时不应出现 cloud 字段");
    }

    /// 回归：「复制多条同项目会话 + 瘦身 keep=1」——复制体必须全部存活，且不挤掉原有保留名额。
    #[test]
    fn slim_protects_copied_sessions() {
        let db = temp_db("slim_protect");
        setup(&db);
        {
            let conn = Connection::open(&db).unwrap();
            // 模拟本次切号复制到 uid-a 的两条同项目会话（时间戳最新）
            for (id, upd) in [("c1", 3000), ("c2", 4000)] {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                     VALUES (?1, 'D:\\p1', 'uid-a', 'copied', 1, ?2)",
                    rusqlite::params![id, upd],
                )
                .unwrap();
            }
        }
        let alive = |db: &Path| -> Vec<String> {
            let conn = Connection::open(db).unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE cwd='D:\\p1' AND deleted_at IS NULL ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
                .collect()
        };

        // 无保护：keep=1 只留最新的 c2，复制体互相挤掉（用户只看到 1 条）
        let rep = slim_sessions_in_db(&db, "uid-a", 1, false, &[]).unwrap();
        let kept = alive(&db);
        assert_eq!(kept, vec!["c2".to_string()], "无保护时只剩最新一条: {kept:?}");
        assert_eq!(rep["deleted"], 3);

        // 有保护：c1/c2 都留，且 p1 原有的最新一条 s2 也留（复制体不占名额）
        let db2 = temp_db("slim_protect2");
        setup(&db2);
        {
            let conn = Connection::open(&db2).unwrap();
            for (id, upd) in [("c1", 3000), ("c2", 4000)] {
                conn.execute(
                    "INSERT INTO sessions (id, cwd, user_id, title, created_at, updated_at)
                     VALUES (?1, 'D:\\p1', 'uid-a', 'copied', 1, ?2)",
                    rusqlite::params![id, upd],
                )
                .unwrap();
            }
        }
        let rep2 = slim_sessions_in_db(
            &db2,
            "uid-a",
            1,
            false,
            &["c1".to_string(), "c2".to_string()],
        )
        .unwrap();
        let kept2 = alive(&db2);
        assert_eq!(
            kept2,
            vec!["c1".to_string(), "c2".to_string(), "s2".to_string()],
            "复制体受保护 + 原有最新一条: {kept2:?}"
        );
        assert_eq!(rep2["deleted"], 1, "只删旧的 s1");
        assert_eq!(rep2["excluded"], 2);
    }

    #[test]
    fn snapshot_roundtrip() {
        let db = temp_db("snap");
        setup(&db);
        let prj = temp_projects("snap");
        let snap = temp_db("snapshot");
        sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", true, false).unwrap();
        assert!(load_snapshot_from(&snap, "uid-a").is_empty(), "dry-run 不写快照");
        sync_project_set_in_db(&db, &prj, &snap, "uid-a", "uid-b", false, false).unwrap();
        let saved = load_snapshot_from(&snap, "uid-a");
        assert_eq!(saved.len(), 2);
    }
}
