//! 模型限额台账：从本机日志还原「账号 × 模型」的频率限制（6004）与官方解锁时刻。
//!
//! 双源（只读）：
//! - SDK 日志  `<logs>/<日期>/sdk/conversations/<会话UUID>.log`：行首时间戳 = **UTC**，文件名 = 会话 id
//! - 业务日志  `<logs>/<日期>/<工作区>__<hash>.log`：行首时间戳 = **本地**，事件行内含 `(32hex/会话UUID)`
//!
//! 归因：
//! - 账号：会话 UUID → `sessions.user_id` → `accounts.json` 昵称。首次归因固化进
//!   `~/.wb-switch/limits_index.json`（`sessions.user_id` 会被 L3 归属移动改写，不回读覆盖历史）。
//! - 模型：触发时刻前、同会话最近一次 `sendPrompt` 的 modelId（`-f` 后缀归一化）。
//!
//! 去重三步：① (会话,解锁时刻) ② 无会话 id 的行按解锁时刻 ≤15s 合并 ③ 归因后按
//! (账号,模型,解锁时刻) ≤60s 合并（同账号两个会话几乎同时撞限 = 一次）。
//!
//! ⚠️ 解锁时刻来自服务端 429 原文，是**权威值**；"剩余次数"不可得（日志不暴露配额，容量与模型相关）。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::{account, config, session};

const MERGE_WINDOW_SECS: i64 = 15;
const SAME_KEY_WINDOW_SECS: i64 = 60;
/// 冷启动（无索引）时只读最近这么多小时被写过的文件 —— 否则首跑要啃全量 3GB+。
const COLD_WINDOW_HOURS: i64 = 24;
/// 解锁时刻前缀：国内版「将在 … 重置」、国际版「will reset at …」——两套都要认，
/// 否则国际版一条事件都读不到（两者都以 `UTC+8` 结尾，解析逻辑共用）。
const RESET_MARKS: [&str; 2] = ["将在 ", "will reset at "];
const RESET_ZONE: &str = "UTC+8";
/// 限流标志：国内版中文 / 国际版英文。缺一组 = 整类漏读。
const LIMIT_MARKS: [&str; 2] = ["使用量已超出频率限制", "usage exceeds frequency limit"];
/// SDK 日志里 code 嵌在转义 JSON 中，两种形态都收。
const LIMIT_JSON_MARKS: [&str; 2] = ["\"code\":6004", "\\\"code\\\":6004"];
/// 沙箱会把自己命令的 stdout 回写进工作区日志，内容里若含 reset 文案就是**自污染**。
const ECHO_MARKS: [&str; 3] = ["[SandboxShell]", "[SandboxPipeHandle]", "ProcessOutput"];

fn bj() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("+08:00")
}

fn fmt_bj(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&bj()).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 归一化模型名：`-f` 是变体后缀，聚合前去掉。
pub fn normalize_model(model: &str) -> String {
    model.trim().trim_end_matches("-f").to_string()
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        };
        if !ok {
            return false;
        }
    }
    true
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

fn find_after<'a>(line: &'a str, needle: &str) -> Option<&'a str> {
    line.find(needle).map(|i| &line[i + needle.len()..])
}

/// SDK 日志行首 UTC 时间戳（`2026-09-12T11:00:36.981Z`）。
fn parse_sdk_ts(line: &str) -> Option<DateTime<Utc>> {
    let stamp = line.get(..19)?;
    if line.as_bytes().get(19) != Some(&b'.') {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// 业务日志行首本地时间戳（`[2026/9/9 10:01:17.098]`）。按本机时区转 UTC。
fn parse_biz_ts(line: &str) -> Option<DateTime<Utc>> {
    let inner = line.strip_prefix('[')?;
    let body = inner.get(..inner.find(']')?)?;
    let (date, time) = body.split_once(' ')?;
    let time = time.split('.').next()?;
    let mut d = date.split('/');
    let (y, mo, da) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, s) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let naive = NaiveDate::from_ymd_opt(y, mo, da)?.and_hms_opt(h, mi, s)?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

/// 解锁时刻：`将在 2026-09-14 01:44:40 UTC+8 重置`。文案自带 UTC+8 → 固定按 +8 解释，不做本地换算。
fn parse_reset(line: &str) -> Option<DateTime<Utc>> {
    for mark in RESET_MARKS {
        let Some(rest) = find_after(line, mark) else { continue };
        let (Some(stamp), Some(tail)) = (rest.get(..19), rest.get(19..)) else { continue };
        if !tail.trim_start().starts_with(RESET_ZONE) {
            continue;
        }
        let naive = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").ok()?;
        return bj().from_local_datetime(&naive).single().map(|dt| dt.with_timezone(&Utc));
    }
    None
}

/// 事件行里的 `(requestId/会话UUID)` —— **两个都是有用信息**：
/// 32hex 是模型归因的首选键（`[ModelProvider]` 行可精确匹到 model），
/// UUID 才是会话 id（用于查账号）。早期版本把 32hex 当账号用过，别再取错。
fn parse_ids(line: &str) -> Option<(String, String)> {
    for (idx, _) in line.match_indices('(') {
        let Some(seg) = line.get(idx + 1..) else { continue };
        let Some(inner) = seg.get(..seg.find(')')?) else { continue };
        if let Some((hex, uuid)) = inner.split_once('/') {
            if is_hex32(hex) && is_uuid(uuid) {
                return Some((hex.to_string(), uuid.to_string()));
            }
        }
    }
    None
}

/// 会话 id：`(32hex/会话UUID)` 的**斜杠之后**。回退到 `sessionId=<UUID>`。
fn parse_session(line: &str) -> Option<String> {
    if let Some((_, uuid)) = parse_ids(line) {
        return Some(uuid);
    }
    find_after(line, "sessionId=")
        .and_then(|s| s.get(..36))
        .filter(|s| is_uuid(s))
        .map(|s| s.to_string())
}

/// `(requestId/会话UUID)` 的**斜杠之前** —— 模型归因首选键。
fn parse_request_id(line: &str) -> Option<String> {
    parse_ids(line).map(|(hex, _)| hex)
}

/// `[ModelProvider] Sending request: agent=cli, model=X, requestId=<32hex>, stream=true`
/// —— 用 requestId 精确匹出**触发这次限流的模型**。两个键的先后不保证 → 各取各的。
fn parse_provider_model(line: &str) -> Option<(String, String)> {
    if !line.contains("[ModelProvider]") || !line.contains("Sending request") {
        return None;
    }
    let rid = find_after(line, "requestId=")?.get(..32)?;
    if !is_hex32(rid) {
        return None;
    }
    let rest = find_after(line, "model=")?;
    let end = rest.find([',', ' ', '"']).unwrap_or(rest.len());
    let model = normalize_model(&rest[..end]);
    if model.is_empty() {
        return None;
    }
    Some((rid.to_string(), model))
}

/// `sendPrompt` 行里的 modelId（JSON 子串解析，不引 regex 依赖）。
fn parse_model(line: &str) -> Option<String> {
    let rest = find_after(line, "\"modelId\"")?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(normalize_model(&rest[..end]))
}

fn is_limit_line(line: &str) -> bool {
    LIMIT_MARKS.iter().any(|m| line.contains(m))
        || LIMIT_JSON_MARKS.iter().any(|m| line.contains(m))
}

#[derive(Clone, Debug)]
struct Raw {
    src: &'static str,
    ts: Option<DateTime<Utc>>,
    session: Option<String>,
    /// 事件行里的 requestId —— 模型归因首选键（精确匹配 `[ModelProvider]` 行）
    request_id: Option<String>,
    model: Option<String>,
    unlock: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitEvent {
    /// 触发时刻（北京时间字符串）
    pub ts: String,
    pub ts_epoch_ms: i64,
    pub source: String,
    pub session: Option<String>,
    /// 账号展示名（昵称）
    pub account: Option<String>,
    /// 账号 uid 前 8 位 —— **前端按这个匹配**（昵称可改、可重复，uid 稳定）
    pub account_uid: Option<String>,
    pub model: Option<String>,
    /// 官方给出的解锁时刻（北京时间字符串）
    pub unlock: String,
    pub unlock_epoch_ms: i64,
    pub confidence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitState {
    pub account: Option<String>,
    pub account_uid: Option<String>,
    pub model: String,
    pub unlock: String,
    pub unlock_epoch_ms: i64,
    pub limited: bool,
    pub remaining_secs: i64,
}

/// 日志根：国内版 `~/.workbuddy/logs`、国际版 `~/.workbuddy-ai/logs`。
/// 两边都列出来——只装了其中一个时另一个 `read_dir` 直接失败，无害。
fn logs_roots() -> Vec<PathBuf> {
    let home = config::home_dir();
    vec![
        home.join(".workbuddy").join("logs"),
        home.join(".workbuddy-ai").join("logs"),
    ]
}

fn read_lossy(path: &std::path::Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // 业务日志含 NUL 字节 → 用 lossy 转字符串，别整文件丢弃
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn day_dirs(cutoff_day: Option<NaiveDate>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in logs_roots() {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(day) = NaiveDate::parse_from_str(&name, "%Y-%m-%d") else { continue };
            if let Some(cut) = cutoff_day {
                if day < cut {
                    continue;
                }
            }
            out.push(e.path());
        }
    }
    out.sort();
    out
}

/// 本次是否要读该文件 + 要登记的状态（纯 IO 判定，与解析无关）。
///
/// - 尺寸与 mtime 都没变 → 跳过（**增量核心**：老日志不再重读）
/// - 冷启动（无索引）且文件早于 `cold_cutoff_ms` → 跳过但登记，避免首跑啃全量 3GB+
fn plan_file(idx: &Index, cold: bool, cold_cutoff_ms: i64, path: &std::path::Path) -> (bool, Option<FileState>) {
    let Ok(md) = fs::metadata(path) else { return (false, None) };
    let size = md.len();
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let key = path.to_string_lossy().into_owned();
    let state = FileState { size, mtime_ms };
    if should_skip(idx.files.get(&key), size, mtime_ms) {
        return (false, Some(state));
    }
    if cold && mtime_ms < cold_cutoff_ms {
        return (false, Some(state));
    }
    (true, Some(state))
}

/// 扫日志：同一次调用里同时收 `sendPrompt` 序列（模型归因用）与 6004 事件行。
/// 返回 (原始事件, 本次登记的文件状态)。
fn scan_raw(idx: &Index) -> (Vec<Raw>, BTreeMap<String, FileState>) {
    let cold = idx.files.is_empty();
    let cold_cutoff_ms = (Utc::now() - chrono::Duration::hours(COLD_WINDOW_HOURS)).timestamp_millis();
    let mut files: BTreeMap<String, FileState> = BTreeMap::new();
    let mut raw: Vec<Raw> = Vec::new();
    let mut sends: HashMap<String, Vec<(DateTime<Utc>, String)>> = HashMap::new();
    // requestId → model（`[ModelProvider]` 行累积，跨文件也安全：requestId 全局唯一）
    let mut rid_models: HashMap<String, String> = HashMap::new();

    for day in day_dirs(None) {
        // A 主源：sdk/conversations/*.log（文件名 = 会话 id，时间戳 UTC）
        let sdk_dir = day.join("sdk").join("conversations");
        if let Ok(entries) = fs::read_dir(&sdk_dir) {
            for e in entries.flatten() {
                let path = e.path();
                let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
                if !is_uuid(&stem) {
                    continue;
                }
                let (read, state) = plan_file(idx, cold, cold_cutoff_ms, &path);
                if let Some(state) = state {
                    files.insert(path.to_string_lossy().into_owned(), state);
                }
                if !read {
                    continue;
                }
                let Some(text) = read_lossy(&path) else { continue };
                let seq = sends.entry(stem.clone()).or_default();
                for line in text.lines() {
                    if line.contains("method:sendPrompt") {
                        if let (Some(ts), Some(model)) = (parse_sdk_ts(line), parse_model(line)) {
                            seq.push((ts, model));
                        }
                    }
                    if is_limit_line(line) {
                        raw.push(Raw {
                            src: "sdk",
                            ts: parse_sdk_ts(line),
                            session: Some(stem.clone()),
                            request_id: None,
                            model: None,
                            unlock: parse_reset(line),
                        });
                    }
                }
            }
        }

        // B 辅源：<日期>/*.log（时间戳本地；会话 id 在事件行内）
        if let Ok(entries) = fs::read_dir(&day) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().map(|x| x != "log").unwrap_or(true) {
                    continue;
                }
                let (read, state) = plan_file(idx, cold, cold_cutoff_ms, &path);
                if let Some(state) = state {
                    files.insert(path.to_string_lossy().into_owned(), state);
                }
                if !read {
                    continue;
                }
                let Some(text) = read_lossy(&path) else { continue };
                for line in text.lines() {
                    if ECHO_MARKS.iter().any(|m| line.contains(m)) {
                        continue;
                    }
                    // 先攒模型索引（该行与限流行同文件，扫到即用）
                    if let Some((rid, model)) = parse_provider_model(line) {
                        rid_models.insert(rid, model);
                        continue;
                    }
                    if !is_limit_line(line) {
                        continue;
                    }
                    raw.push(Raw {
                        src: "biz",
                        ts: parse_biz_ts(line),
                        session: parse_session(line),
                        request_id: parse_request_id(line),
                        model: None,
                        unlock: parse_reset(line),
                    });
                }
            }
        }
    }

    for seq in sends.values_mut() {
        seq.sort();
    }
    // 模型归因：① requestId 精确匹配（最准：它就是触发这次限流的那次请求，实测 118/118）
    // ② 触发前同会话最近一次 sendPrompt（SDK 源 / 拿不到 requestId 时）
    // ③ sessions.model（最后兜底，见 resolve_models）
    for r in raw.iter_mut() {
        if let Some(rid) = r.request_id.as_deref() {
            if let Some(m) = rid_models.get(rid) {
                r.model = Some(m.clone());
                continue;
            }
        }
        let (Some(sid), Some(ts)) = (r.session.clone(), r.ts) else { continue };
        if let Some(seq) = sends.get(&sid) {
            let mut best = None;
            for (sts, model) in seq {
                if *sts <= ts {
                    best = Some(model.clone());
                } else {
                    break;
                }
            }
            r.model = best;
        }
    }
    (raw, files)
}

/// 去重①②：先按 (会话, 解锁时刻)，再让无会话 id 的行与同解锁时刻的已见事件按 ≤15s 合并。
fn dedup(raw: Vec<Raw>) -> Vec<Raw> {
    let mut out: Vec<Raw> = Vec::new();
    let mut seen: Vec<(String, DateTime<Utc>)> = Vec::new();
    let mut times_by_unlock: HashMap<DateTime<Utc>, Vec<DateTime<Utc>>> = HashMap::new();

    for r in raw {
        let (Some(ts), Some(unlock)) = (r.ts, r.unlock) else { continue };
        match &r.session {
            Some(sid) => {
                let key = (sid.clone(), unlock);
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
            }
            None => {
                let near = times_by_unlock
                    .get(&unlock)
                    .map(|ts_list| ts_list.iter().any(|o| (*o - ts).num_seconds().abs() <= MERGE_WINDOW_SECS))
                    .unwrap_or(false);
                if near {
                    continue;
                }
            }
        }
        times_by_unlock.entry(unlock).or_default().push(ts);
        out.push(r);
    }
    out.sort_by_key(|r| r.ts);
    out
}

/// uid 前 8 位 → 昵称。
fn uid_names() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for acc in account::load_accounts() {
        let Some(uid) = acc.get("uid").and_then(|v| v.as_str()) else { continue };
        let name = acc
            .get("nickname")
            .and_then(|v| v.as_str())
            .or_else(|| acc.get("email").and_then(|v| v.as_str()))
            .unwrap_or(uid);
        out.insert(uid.chars().take(8).collect::<String>(), name.to_string());
    }
    out
}

fn resolve_owners(sids: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if sids.is_empty() {
        return out;
    }
    let Some(conn) = session::open_db(&session::workbuddy_db_path(crate::modules::variant::WbVariant::Cn), true) else { return out };
    for sid in sids {
        let uid: Option<String> = conn
            .query_row("SELECT user_id FROM sessions WHERE id = ?1", [sid], |row| row.get(0))
            .unwrap_or(None);
        if let Some(uid) = uid {
            out.insert(sid.clone(), uid.chars().take(8).collect());
        }
    }
    out
}

/// 会话 id → `sessions.model`（会话"当前所用模型"）。
/// **只作最后兜底**：被限后用户常立刻切模型，DB 记的是切换后的模型，
/// 会错归（实测：ds 被限 → 切 glm → DB 记 glm）。
fn resolve_models(sids: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if sids.is_empty() {
        return out;
    }
    let Some(conn) = session::open_db(&session::workbuddy_db_path(crate::modules::variant::WbVariant::Cn), true) else { return out };
    for sid in sids {
        let model: Option<String> = conn
            .query_row("SELECT model FROM sessions WHERE id = ?1", [sid], |row| row.get(0))
            .unwrap_or(None);
        if let Some(m) = model {
            if !m.is_empty() {
                out.insert(sid.clone(), m);
            }
        }
    }
    out
}

fn index_path() -> PathBuf {
    config::store_dir().join("limits_index.json")
}

/// 上次扫该文件时的 (尺寸, mtime)。**都没变 → 整个文件跳过不读**（增量核心）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileState {
    size: u64,
    mtime_ms: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    version: u32,
    /// 绝对路径 → 上次读到的 (size, mtime)
    files: BTreeMap<String, FileState>,
    /// 累积的 6004 事件（去重键 = 会话|解锁时刻）
    events: Vec<LimitEvent>,
}

/// 增量判据（纯函数，便于单测）：尺寸与 mtime 都没变 → 无需读。
fn should_skip(prev: Option<&FileState>, size: u64, mtime_ms: i64) -> bool {
    matches!(prev, Some(f) if f.size == size && f.mtime_ms == mtime_ms)
}

fn load_index() -> Index {
    let Ok(text) = fs::read_to_string(index_path()) else { return Index::default() };
    let Ok(mut idx) = serde_json::from_str::<Index>(&text) else { return Index::default() };
    if idx.version != 2 {
        return Index::default();
    }
    idx.events.sort_by_key(|e| e.ts_epoch_ms);
    idx
}

fn save_index(idx: &Index) {
    let path = index_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(body) = serde_json::to_string(idx) else { return };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

/// 索引内的事件去重键：`<会话>|<解锁时刻(北京字符串)>`（与索引里持久化的形态一致）。
fn index_key(session: Option<&str>, unlock_bj: &str) -> String {
    format!("{}|{}", session.unwrap_or(""), unlock_bj)
}

/// 归因：把去重后的原始命中组装成事件。
///
/// `frozen` = 索引里已固化的归因（**不回读 DB 覆盖**，sessions.user_id 会被 L3 归属移动改写）；
/// `owners` = 本次新查到的 session → uid；`names` = uid → 昵称。
fn build_events(
    raw: Vec<Raw>,
    frozen: &HashMap<String, Option<String>>,
    owners: &HashMap<String, String>,
    names: &HashMap<String, String>,
) -> Vec<LimitEvent> {
    raw.into_iter()
        .map(|r| {
            let ts = r.ts.expect("dedup 已过滤无时间戳");
            let unlock = r.unlock.expect("dedup 已过滤无解锁时刻");
            let key = index_key(r.session.as_deref(), &fmt_bj(unlock));
            let uid = frozen
                .get(&key)
                .cloned()
                .flatten()
                .or_else(|| r.session.as_ref().and_then(|sid| owners.get(sid)).cloned());
            let account = uid
                .as_ref()
                .and_then(|u| names.get(u).cloned())
                .or_else(|| uid.clone());
            LimitEvent {
                ts: fmt_bj(ts),
                ts_epoch_ms: ts.timestamp_millis(),
                source: r.src.to_string(),
                session: r.session,
                account,
                account_uid: uid,
                model: r.model,
                unlock: fmt_bj(unlock),
                unlock_epoch_ms: unlock.timestamp_millis(),
                confidence: "low".to_string(),
            }
        })
        .collect()
}

/// ③ 归因后按 (账号, 模型, 解锁时刻) ≤60s 再并一次，并按归因完整度定置信度。
fn merge_and_score(events: Vec<LimitEvent>) -> Vec<LimitEvent> {
    let mut merged: Vec<LimitEvent> = Vec::new();
    for e in events {
        let dup = merged.iter().rev().any(|p| {
            p.unlock == e.unlock
                && p.account_uid == e.account_uid
                && p.account == e.account
                && p.model == e.model
                && (p.ts_epoch_ms - e.ts_epoch_ms).abs() / 1000 <= SAME_KEY_WINDOW_SECS
        });
        if !dup {
            merged.push(e);
        }
    }
    for e in merged.iter_mut() {
        e.confidence = if e.account.is_some() && e.session.is_some() {
            "high".to_string()
        } else {
            "low".to_string()
        };
    }
    merged
}

/// 把新事件累积进索引（按 key 去重）+ 回写文件状态，下次即可跳过未变文件。
fn accumulate_into_index(
    idx: &mut Index,
    merged: &[LimitEvent],
    files: BTreeMap<String, FileState>,
) {
    let mut known: std::collections::HashSet<String> = idx
        .events
        .iter()
        .map(|e| index_key(e.session.as_deref(), &e.unlock))
        .collect();
    for e in merged {
        if known.insert(index_key(e.session.as_deref(), &e.unlock)) {
            idx.events.push(e.clone());
        }
    }
    idx.version = 2;

    for (path, state) in files {
        idx.files.insert(path, state);
    }
    idx.events.sort_by_key(|e| e.ts_epoch_ms);
    const MAX_EVENTS: usize = 1000;
    if idx.events.len() > MAX_EVENTS {
        let drop = idx.events.len() - MAX_EVENTS;
        idx.events.drain(0..drop);
    }
    save_index(idx);
}

/// 模型兜底：①② 都没取到时填 `sessions.model`。
///
/// 它代表"会话当前模型"，被限后用户常立刻切模型会错归 → **只填 None，不覆盖已有归因**。
/// 仅处理近 48h 事件（滑动窗 W ≤ 24h，更早的归因不影响当前 chip），省一次全量 DB 查询。
fn apply_model_fallback(idx: &mut Index) {
    let cutoff_48h = Utc::now().timestamp_millis() - 48 * 3600 * 1000;
    let mut all_sids: Vec<String> = idx
        .events
        .iter()
        .filter(|e| e.ts_epoch_ms >= cutoff_48h && e.model.is_none())
        .filter_map(|e| e.session.clone())
        .collect();
    all_sids.sort();
    all_sids.dedup();
    let fb_models = resolve_models(&all_sids);
    for e in idx.events.iter_mut() {
        if e.ts_epoch_ms < cutoff_48h || e.model.is_some() {
            continue;
        }
        let Some(sid) = e.session.clone() else { continue };
        if let Some(m) = fb_models.get(&sid) {
            e.model = Some(normalize_model(m));
        }
    }
}

/// 采集限额事件：**增量扫日志**（尺寸+mtime 未变的文件整份跳过）→ 归因 → 累积进索引
/// `~/.wb-switch/limits_index.json`（含文件状态，供下次跳过）。
///
/// 只做编排：扫 → 去重 → 归因 → 合并 → 落索引 → 模型兜底 → 按天过滤。
pub fn collect(limit_days: u32) -> Vec<LimitEvent> {
    let mut idx = load_index();
    // 已有事件里的归因结果即"固化值"：**不回读 DB 覆盖**（sessions.user_id 会被 L3 归属移动改写）
    let frozen: HashMap<String, Option<String>> = idx
        .events
        .iter()
        .map(|e| (index_key(e.session.as_deref(), &e.unlock), e.account_uid.clone()))
        .collect();

    let (raw, files) = scan_raw(&idx);
    let raw = dedup(raw);

    // 只查索引里还没有答案的会话，避免每次全量查库
    let names = uid_names();
    let need: Vec<String> = raw
        .iter()
        .filter(|r| {
            r.session
                .as_ref()
                .map(|sid| {
                    let key = index_key(Some(sid), &fmt_bj(r.unlock.expect("dedup 已过滤无解锁时刻")));
                    !frozen.contains_key(&key)
                })
                .unwrap_or(false)
        })
        .filter_map(|r| r.session.clone())
        .collect();
    let owners = resolve_owners(&need);

    let events = build_events(raw, &frozen, &owners, &names);
    let merged = merge_and_score(events);

    accumulate_into_index(&mut idx, &merged, files);
    apply_model_fallback(&mut idx);

    let cutoff_ms = if limit_days == 0 {
        i64::MIN
    } else {
        (Utc::now() - chrono::Duration::days(limit_days as i64)).timestamp_millis()
    };
    idx.events.retain(|e| e.ts_epoch_ms >= cutoff_ms);
    idx.events
}
/// 派生解锁状态：每个 (账号, 模型) 取**最近一次**事件的解锁时刻（滑动窗下会前移，不能取旧值）。
pub fn states(events: &[LimitEvent]) -> Vec<LimitState> {
    let now_ms = Utc::now().timestamp_millis();
    // 分组键优先用 uid（稳定）；无 uid 时退回昵称/未归因
    let mut best: BTreeMap<(String, String), (i64, String, Option<String>)> = BTreeMap::new();
    for e in events {
        let group = e
            .account_uid
            .clone()
            .or_else(|| e.account.clone())
            .unwrap_or_else(|| "未归因".to_string());
        let model = e.model.clone().unwrap_or_else(|| "(未知)".to_string());
        let entry = best
            .entry((group, model))
            .or_insert((e.unlock_epoch_ms, e.unlock.clone(), e.account.clone()));
        if e.unlock_epoch_ms > entry.0 {
            *entry = (e.unlock_epoch_ms, e.unlock.clone(), e.account.clone());
        }
    }
    let mut out: Vec<LimitState> = best
        .into_iter()
        .map(|((group, model), (ms, unlock, account))| LimitState {
            account: account.or(Some(group.clone())),
            account_uid: if group == "未归因" { None } else { Some(group) },
            model,
            unlock,
            unlock_epoch_ms: ms,
            limited: ms > now_ms,
            remaining_secs: (ms - now_ms).max(0) / 1000,
        })
        .collect();
    out.sort_by_key(|s| s.unlock_epoch_ms);
    out
}

/// 前端可直接渲染的快照。
pub fn snapshot(limit_days: u32) -> Value {
    let events = collect(limit_days);
    let states = states(&events);
    let limited = states.iter().filter(|s| s.limited).count();
    json!({
        "generatedAt": fmt_bj(Utc::now()),
        "limitedCount": limited,
        "states": states,
        "events": events,
        "note": "解锁时刻来自服务端 429 原文（权威）；剩余次数不可得。",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIZ_LINE: &str = "[2026/9/9 10:01:17.098] [Info] [pid=14148] [Interruption] Catch block entered, error: 429 您的使用量已超出频率限制，将在 2026-09-09 13:54:23 UTC+8 重置，您也可以切换其他模型继续使用。 (0123456789abcdef0123456789abcdef/aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee)";
    const SDK_EVENT_LINE: &str = "2026-09-13T04:53:35.100Z runtime.applyStopReason {\"event\":\"TURN_ERROR\",\"errorMessageMetaPreview\":\"{\\\"code\\\":6004,\\\"message\\\":\\\"Quota exceeded: 429 您的使用量已超出频率限制，将在 2026-09-14 01:44:40 UTC+8 重置\\\"}\"}";
    const SEND_LINE: &str = "2026-09-12T17:50:13.490Z method:sendPrompt {\"modelId\":\"deepseek-v4.1-flash-f\"}";
    /// 国际版：限流文案是英文，但位置/格式/时区后缀完全一致
    const INTL_LINE: &str = "[2026/9/13 20:07:05.164] [Info] [pid=18232] [Interruption] Catch block entered, error: 429 usage exceeds frequency limit, but don't worry, your usage will reset at 2026-09-14 10:59:21 UTC+8, alternatively, you can switch to the other models to continue using it. (0123456789abcdef0123456789abcdef/aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee)";
    const PROVIDER_LINE: &str = "[2026/9/9 08:54:24.654] [Info] [pid=13604] [ModelProvider]  [ModelProvider] Sending request: agent=cli, model=hy4-preview, requestId=0123456789abcdef0123456789abcdef, stream=true, url=https://copilot.tencent.com/v2/chat/";

    #[test]
    fn parses_international_english_line() {
        // 只认中文关键词时这一整类会被漏读
        assert!(is_limit_line(INTL_LINE));
        assert_eq!(fmt_bj(parse_reset(INTL_LINE).unwrap()), "2026-09-14 10:59:21");
        assert_eq!(
            parse_session(INTL_LINE).as_deref(),
            Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
        );
    }

    #[test]
    fn request_id_is_the_hex_before_slash() {
        assert_eq!(
            parse_request_id(BIZ_LINE).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(parse_request_id(INTL_LINE), parse_request_id(BIZ_LINE));
    }

    #[test]
    fn provider_line_yields_request_id_to_model() {
        let (rid, model) = parse_provider_model(PROVIDER_LINE).expect("provider");
        assert_eq!(rid, "0123456789abcdef0123456789abcdef");
        assert_eq!(model, "hy4-preview");
        // 同一 requestId 就是限流行里那次请求 → 精确归因
        assert_eq!(parse_request_id(BIZ_LINE).as_deref(), Some(rid.as_str()));
        // 非 ModelProvider 行不参与索引
        assert!(parse_provider_model(BIZ_LINE).is_none());
        assert!(parse_provider_model(SEND_LINE).is_none());
    }

    #[test]
    fn parses_business_line_fully() {
        assert!(is_limit_line(BIZ_LINE));
        let ts = parse_biz_ts(BIZ_LINE).expect("biz ts");
        let reset = parse_reset(BIZ_LINE).expect("reset");
        // 触发 10:01:17 +08 → 02:01:17 UTC
        assert_eq!(ts.format("%H:%M:%S").to_string(), "02:01:17");
        assert_eq!(fmt_bj(reset), "2026-09-09 13:54:23");
        assert_eq!(
            parse_session(BIZ_LINE).as_deref(),
            Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
        );
    }

    #[test]
    fn session_comes_after_slash_not_the_hex() {
        // 32hex 是请求相关性 ID：取错分组会把 hex 当会话 id
        let sid = parse_session(BIZ_LINE).unwrap();
        assert!(is_uuid(&sid));
        assert!(!is_hex32(&sid));
    }

    #[test]
    fn parses_sdk_line_and_model() {
        let ts = parse_sdk_ts(SDK_EVENT_LINE).expect("sdk ts");
        assert_eq!(fmt_bj(ts), "2026-09-13 12:53:35");
        assert_eq!(fmt_bj(parse_reset(SDK_EVENT_LINE).unwrap()), "2026-09-14 01:44:40");
        assert_eq!(parse_model(SEND_LINE).as_deref(), Some("deepseek-v4.1-flash"));
    }

    #[test]
    fn rejects_line_without_reset() {
        assert!(parse_reset("[2026/9/9 10:01:17.098] [Info] 仅提到 429").is_none());
        assert!(parse_reset("将在 2026-09-09 13:54:23 UTC+9 重置").is_none());
    }

    /// 性能自检（默认忽略）：`cargo test --release -p wb-switch-core bench_snapshot -- --ignored --nocapture`
    /// 第 1 次 = 冷启动（建索引），第 2 次 = 增量（应当只有毫秒级）。
    #[test]
    #[ignore]
    fn bench_snapshot() {
        for round in 0..2 {
            let start = std::time::Instant::now();
            let snap = snapshot(30);
            println!(
                "round{} elapsed={:?} events={} states={} limited={}",
                round,
                start.elapsed(),
                snap["events"].as_array().map(|a| a.len()).unwrap_or(0),
                snap["states"].as_array().map(|a| a.len()).unwrap_or(0),
                snap["limitedCount"],
            );
        }
    }

    #[test]
    fn skip_decision_uses_size_and_mtime() {
        let prev = FileState { size: 100, mtime_ms: 1_000 };
        // 尺寸与 mtime 都没变 → 跳过（增量核心）
        assert!(should_skip(Some(&prev), 100, 1_000));
        // 追加了内容 → 必读
        assert!(!should_skip(Some(&prev), 120, 1_000));
        // mtime 变了（被重写）→ 必读
        assert!(!should_skip(Some(&prev), 100, 2_000));
        // 新文件（索引里没有）→ 必读
        assert!(!should_skip(None, 100, 1_000));
    }

    #[test]
    fn model_normalization_strips_f_suffix() {
        assert_eq!(normalize_model("hy4-preview-f"), "hy4-preview");
        assert_eq!(normalize_model("hy3"), "hy3");
    }

    fn raw(src: &'static str, ts: &str, sid: Option<&str>, unlock: &str, model: Option<&str>) -> Raw {
        let parse = |s: &str| bj().from_local_datetime(&NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()).unwrap().with_timezone(&Utc);
        Raw {
            src,
            ts: Some(parse(ts)),
            session: sid.map(|s| s.to_string()),
            request_id: None,
            model: model.map(|s| s.to_string()),
            unlock: Some(parse(unlock)),
        }
    }

    #[test]
    fn dedup_merges_sdk_and_business_of_same_event() {
        let raw = vec![
            raw("sdk", "2026-09-09 10:01:17", Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"), "2026-09-09 13:54:23", Some("hy4-preview")),
            // 同一事件的业务日志行（无会话 id）→ 应被 15s 规则并掉
            raw("biz", "2026-09-09 10:01:17", None, "2026-09-09 13:54:23", None),
            raw("biz", "2026-09-09 10:01:18", None, "2026-09-09 13:54:23", None),
            // 另一事件：不同解锁时刻 → 保留
            raw("sdk", "2026-09-10 11:24:01", Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"), "2026-09-10 14:50:12", Some("hy4-preview")),
        ];
        assert_eq!(dedup(raw).len(), 2);
    }

    #[test]
    fn dedup_keeps_distinct_sessions_even_with_same_unlock() {
        let raw = vec![
            raw("sdk", "2026-09-13 16:53:06", Some("aaaaaaaa-0000-0000-0000-00000000000a"), "2026-09-14 14:35:05", Some("deepseek-v4.1-flash")),
            raw("sdk", "2026-09-13 16:53:42", Some("bbbbbbbb-0000-0000-0000-00000000000b"), "2026-09-14 14:35:05", Some("deepseek-v4.1-flash")),
        ];
        assert_eq!(dedup(raw).len(), 2, "不同会话先各自保留，交给第三步按账号合并");
    }

    #[test]
    fn state_takes_latest_unlock_and_marks_limited() {
        let mk = |unlock: &str, ms: i64| LimitEvent {
            ts: "2026-09-13 10:55:33".into(),
            ts_epoch_ms: 0,
            source: "sdk".into(),
            session: None,
            account: Some("测试账号".into()),
            account_uid: Some("abcd1234".into()),
            model: Some("deepseek-v4.1-flash".into()),
            unlock: unlock.into(),
            unlock_epoch_ms: ms,
            confidence: "high".into(),
        };
        let events = vec![
            mk("2026-09-14 01:50:14", 1_800_000_000_000),
            mk("2026-09-12 20:56:45", 1_700_000_000_000),
        ];
        let st = states(&events);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].unlock, "2026-09-14 01:50:14");
        assert_eq!(st[0].account_uid.as_deref(), Some("abcd1234"));
        assert!(st[0].limited, "解锁时刻在未来 → 受限");
    }
}
