//! 常量、路径与通用工具函数（对照 server.py 常量区与工具区）

use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::modules::variant::WbVariant;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

// 以下三个常量是「国内版」档位的取值来源（档位取值统一见 modules/variant.rs）；
// 新增档位差异不要再新增同类常量。
pub const WORKBUDDY_API_ENDPOINT: &str = "https://www.codebuddy.cn";
pub const WORKBUDDY_API_PREFIX: &str = "/v2/plugin";
pub const WORKBUDDY_PLATFORM: &str = "workbuddy";

pub const OAUTH_TIMEOUT_SECONDS: i64 = 600;

pub const CHECKIN_API_PREFIX: &str = "/v2/billing/meter";
pub const CHECKIN_LOG_KEEP_DAYS: i64 = 30;
pub const CHECKIN_LOG_MAX_RECORDS: usize = 500;

/// 派猫猫旅行接口前缀（成长中心，非 /v2/plugin 体系，直接挂在 API 域名下）。
pub const TRAVEL_API_PREFIX: &str = "/activity/growth/buddy/travel";

static CHECKIN_LOG_WRITE_LOCK: Mutex<()> = Mutex::new(());
static TRAVEL_CACHE_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Serialize travel-cache read-modify-write across depart and claim cycles.
pub fn with_travel_cache_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = TRAVEL_CACHE_WRITE_LOCK.lock().unwrap();
    f()
}

pub const ROTATE_LOG_MAX_RECORDS: usize = 200;

/// 官网套餐页桌面 Chrome UA（plans-usage 捕获）。
pub const DEFAULT_HTTP_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 覆盖家目录的环境变量名。
///
/// 设置后 `home_dir()` 返回它的值，于是 `~/.wb-switch`、`~/.codebuddy`、
/// `~/.codebuddy-rotate` 全部落在指定目录下。用途：
/// - 集成测试把家目录沙箱化，避免读写真实账号与 CLI 配置；
/// - 自定义部署位置。
///
/// 未设置时行为与之前完全一致（`dirs::home_dir()`）。
pub const HOME_ENV_VAR: &str = "WB_SWITCH_HOME";

pub fn home_dir() -> PathBuf {
    if let Some(overridden) = std::env::var_os(HOME_ENV_VAR) {
        if !overridden.is_empty() {
            return PathBuf::from(overridden);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn store_dir() -> PathBuf {
    home_dir().join(".wb-switch")
}

pub fn accounts_file() -> PathBuf {
    store_dir().join("accounts.json")
}

pub fn backup_dir() -> PathBuf {
    store_dir().join("backups")
}

pub fn checkin_config_file() -> PathBuf {
    store_dir().join("auto_checkin_config.json")
}

pub fn checkin_logs_file() -> PathBuf {
    store_dir().join("auto_checkin_logs.json")
}

pub fn travel_config_file() -> PathBuf {
    store_dir().join("auto_travel_config.json")
}

pub fn travel_cache_file() -> PathBuf {
    store_dir().join("travel_cache.json")
}

pub fn credit_usage_snapshots_file() -> PathBuf {
    store_dir().join("credit_usage_snapshots.json")
}

pub fn rate_limit_config_file() -> PathBuf {
    store_dir().join("rate_limit_config.json")
}

pub fn official_usage_cache_file() -> PathBuf {
    store_dir().join("official_usage_cache.json")
}

pub fn auto_rotate_config_file() -> PathBuf {
    store_dir().join("auto_rotate_config.json")
}

pub fn auto_rotate_logs_file() -> PathBuf {
    store_dir().join("auto_rotate_logs.json")
}

pub fn workbuddy_exe_cache_file() -> PathBuf {
    store_dir().join("workbuddy_exe.json")
}

/// 旧格式（单 `exe` 字段）解析，CodeBuddy CN 应用缓存沿用该格式。
fn parse_workbuddy_exe_cache_json(text: &str) -> Option<PathBuf> {
    let v: Value = serde_json::from_str(text).ok()?;
    let exe = v.get("exe")?.as_str()?.trim();
    if exe.is_empty() {
        None
    } else {
        Some(PathBuf::from(exe))
    }
}

/// 按档位读缓存：新格式按档位分键，旧格式单键仅国内版认。
fn parse_workbuddy_exe_cache_json_for(text: &str, variant: WbVariant) -> Option<PathBuf> {
    let v: Value = serde_json::from_str(text).ok()?;
    let keyed = v.get(variant.exe_cache_key());
    let legacy = if variant == WbVariant::Cn {
        v.get("exe")
    } else {
        None
    };
    let exe = keyed.or(legacy)?.as_str()?.trim();
    if exe.is_empty() {
        None
    } else {
        Some(PathBuf::from(exe))
    }
}

/// 读取上次成功解析到的 WorkBuddy 应用路径（按档位分键）；损坏或空文件视为无缓存。
pub fn load_workbuddy_exe_cache(variant: WbVariant) -> Option<PathBuf> {
    let f = workbuddy_exe_cache_file();
    if !f.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&f).ok()?;
    parse_workbuddy_exe_cache_json_for(&text, variant)
}

/// 把某档位的路径并入缓存内容（旧格式单键按国内版迁移，写回新格式）。
fn upsert_workbuddy_exe_cache_json(text: &str, variant: WbVariant, exe: &Path) -> String {
    let mut root = serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = root.as_object_mut() {
        if let Some(Value::String(legacy)) = obj.remove("exe") {
            if !legacy.trim().is_empty() {
                obj.entry(WbVariant::Cn.exe_cache_key().to_string())
                    .or_insert(Value::String(legacy));
            }
        }
        obj.insert(
            variant.exe_cache_key().to_string(),
            json!(exe.to_string_lossy()),
        );
    }
    serde_json::to_string_pretty(&root).unwrap_or_default()
}

/// 记住已存在的 WorkBuddy 应用路径（按档位分键；旧格式单键在写回时升级）。
pub fn save_workbuddy_exe_cache(variant: WbVariant, exe: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let file = workbuddy_exe_cache_file();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    atomic_write(&file, &upsert_workbuddy_exe_cache_json(&text, variant, exe))
}

/// 清除某档位的缓存项；无其它档位残留则删除文件。
pub fn clear_workbuddy_exe_cache(variant: WbVariant) {
    let file = workbuddy_exe_cache_file();
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let Ok(mut root) = serde_json::from_str::<Value>(&text) else {
        let _ = std::fs::remove_file(&file);
        return;
    };
    let Some(obj) = root.as_object_mut() else {
        let _ = std::fs::remove_file(&file);
        return;
    };
    obj.remove(variant.exe_cache_key());
    if variant == WbVariant::Cn {
        obj.remove("exe");
    }
    if obj.is_empty() {
        let _ = std::fs::remove_file(&file);
        return;
    }
    let content = serde_json::to_string_pretty(&root).unwrap_or_default();
    let _ = atomic_write(&file, &content);
}

pub fn codebuddy_cn_app_cache_file() -> PathBuf {
    store_dir().join("codebuddy_cn_app.json")
}

fn parse_codebuddy_cn_app_cache_json(text: &str) -> Option<PathBuf> {
    parse_workbuddy_exe_cache_json(text)
}

/// 读取上次成功解析到的 CodeBuddy CN 应用路径；损坏或空文件视为无缓存。
pub fn load_codebuddy_cn_app_cache() -> Option<PathBuf> {
    let f = codebuddy_cn_app_cache_file();
    if !f.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&f).ok()?;
    parse_codebuddy_cn_app_cache_json(&text)
}

pub fn save_codebuddy_cn_app_cache(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let content =
        serde_json::to_string_pretty(&json!({ "exe": path.to_string_lossy() })).unwrap_or_default();
    atomic_write(&codebuddy_cn_app_cache_file(), &content)
}

pub fn clear_codebuddy_cn_app_cache() {
    let _ = std::fs::remove_file(codebuddy_cn_app_cache_file());
}

pub fn codebuddy_ide_app_cache_file() -> PathBuf {
    store_dir().join("codebuddy_ide_app.json")
}

fn parse_codebuddy_ide_app_cache_json(text: &str) -> Option<PathBuf> {
    parse_workbuddy_exe_cache_json(text)
}

pub fn load_codebuddy_ide_app_cache() -> Option<PathBuf> {
    let f = codebuddy_ide_app_cache_file();
    if !f.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&f).ok()?;
    parse_codebuddy_ide_app_cache_json(&text)
}

pub fn save_codebuddy_ide_app_cache(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let content =
        serde_json::to_string_pretty(&json!({ "exe": path.to_string_lossy() })).unwrap_or_default();
    atomic_write(&codebuddy_ide_app_cache_file(), &content)
}

pub fn clear_codebuddy_ide_app_cache() {
    let _ = std::fs::remove_file(codebuddy_ide_app_cache_file());
}

// ---------------------------------------------------------------------------
// 签到配置 / 日志（对照 server.py load/save_checkin_config / load/save/add_checkin_log）
// ---------------------------------------------------------------------------

/// 默认签到配置。旧时间窗口字段仅为配置文件兼容保留，调度不再读取。
///
/// `enabled` 默认关闭：全新安装需用户在设置页显式开启；已有用户在
/// [`load_checkin_config`] 中沿用历史默认（开启），升级不改其状态。
/// `checkin_start` / `checkin_end` 为空串 = 不限制签到时间段（与改动前行为一致）。
pub fn default_checkin_config() -> Value {
    checkin_config_with_enabled(false)
}

/// 历史默认签到配置（`enabled: true`）：已有用户没有显式值时沿用。
fn legacy_default_checkin_config() -> Value {
    checkin_config_with_enabled(true)
}

fn checkin_config_with_enabled(enabled: bool) -> Value {
    json!({
        "enabled": enabled,
        "excluded_account_ids": [],
        "checkin_start": "",
        "checkin_end": "",
        "start_hour": 6,
        "end_hour": 12,
        "keepalive_days": 0,
        "lazy_refresh_hours": 24,
    })
}

/// 解析 `"HH:MM"` 本地时钟（允许 1–2 位时/分，如 `"9:5"`）。
///
/// 空串、多余字符、越界（`"24:00"` / `"23:60"`）一律返回 `None`。
pub fn parse_clock(raw: &str) -> Option<(u32, u32)> {
    let (hour, minute) = raw.split_once(':')?;
    if !(1..=2).contains(&hour.len()) || !(1..=2).contains(&minute.len()) {
        return None;
    }
    if !hour.bytes().all(|b| b.is_ascii_digit()) || !minute.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

/// 把 `input` 中的已知字段覆盖到 `merged`；缺失 / 非法字段保持 `merged` 原值。
fn apply_checkin_config(merged: &mut Value, input: &Value) {
    let Some(map) = input.as_object() else {
        return;
    };
    if let Some(enabled) = map.get("enabled").and_then(Value::as_bool) {
        merged["enabled"] = json!(enabled);
    }
    // 仅用稳定账号 id 排除自动签到；忽略无效项并去重，旧配置默认全部参与。
    if let Some(ids) = map.get("excluded_account_ids").and_then(Value::as_array) {
        let mut seen = HashSet::new();
        let ids: Vec<&str> = ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.trim().is_empty() && seen.insert(*id))
            .collect();
        merged["excluded_account_ids"] = json!(ids);
    }
    for key in [
        "start_hour",
        "end_hour",
        "keepalive_days",
        "lazy_refresh_hours",
    ] {
        if let Some(value) = map.get(key).and_then(Value::as_i64) {
            merged[key] = json!(value);
        }
    }
    // 时间段保存归一化：合法值零填充后落盘，非法/非字符串归一为空串（= 不限制）。
    for key in ["checkin_start", "checkin_end"] {
        let normalized = map
            .get(key)
            .and_then(Value::as_str)
            .and_then(parse_clock)
            .map(|(hour, minute)| format!("{hour:02}:{minute:02}"))
            .unwrap_or_default();
        merged[key] = json!(normalized);
    }
}

/// 以新默认值为基线合并（保存路径）。
fn merge_checkin_config(input: &Value) -> Value {
    let mut merged = default_checkin_config();
    apply_checkin_config(&mut merged, input);
    merged
}

/// 读取签到配置（缺失/损坏时合并默认值）。
///
/// 配置只在用户显式保存时落盘，「无配置文件」无法区分新老安装：按使用痕迹（配置文件
/// 或签到日志）判定已有用户，没有显式值时沿用历史默认（开启）；全新安装默认关闭。
pub fn load_checkin_config() -> Value {
    load_checkin_config_at(&checkin_config_file(), &checkin_logs_file())
}

fn load_checkin_config_at(config_path: &Path, logs_path: &Path) -> Value {
    let input = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    resolve_checkin_config(input.as_ref(), config_path.exists() || logs_path.exists())
}

fn resolve_checkin_config(input: Option<&Value>, existing_install: bool) -> Value {
    let mut merged = if existing_install {
        legacy_default_checkin_config()
    } else {
        default_checkin_config()
    };
    if let Some(input) = input {
        apply_checkin_config(&mut merged, input);
    }
    merged
}

/// 保存签到配置（只保留已知字段）。
pub fn save_checkin_config(cfg: &Value) -> std::io::Result<()> {
    let merged = merge_checkin_config(cfg);
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(&checkin_config_file(), &content)
}

/// 读取签到日志。
pub fn load_checkin_logs() -> Vec<Value> {
    let f = checkin_logs_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&text) {
                return arr;
            }
        }
    }
    vec![]
}

fn save_checkin_logs_unlocked(logs: &[Value]) -> std::io::Result<()> {
    let kept = normalize_checkin_logs(logs, now_ms());
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&kept).unwrap_or_default();
    atomic_write(&checkin_logs_file(), &content)
}

/// 保存签到日志（30 天过滤 + 保留最近 500 条，保持插入顺序）。
pub fn save_checkin_logs(logs: &[Value]) -> std::io::Result<()> {
    let _guard = CHECKIN_LOG_WRITE_LOCK.lock().unwrap();
    save_checkin_logs_unlocked(logs)
}

fn checkin_log_local_date(ts_ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ts_ms)
        .single()
        .map(|date| date.format("%Y-%m-%d").to_string())
}

fn legacy_checkin_identity(entry: &Value) -> Option<String> {
    if let Some(account_id) = entry
        .get("accountId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(format!("account:{account_id}"));
    }

    // Old log rows predate accountId and only carried the display identity in
    // `email`. Keep this fallback namespaced so it can never merge with a
    // stable local account ID that happens to have the same text.
    entry
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|email| format!("legacy:{email}"))
}

/// Apply the persisted check-in log contract without changing the source file.
///
/// `success` and `error` entries retain their full multiplicity. Only repeated
/// legacy `already` rows are reduced to the latest timestamp for one account
/// and local calendar date.
fn normalize_checkin_logs(logs: &[Value], at_ms: i64) -> Vec<Value> {
    let cutoff = at_ms.saturating_sub(CHECKIN_LOG_KEEP_DAYS * 24 * 3600 * 1000);
    let retained: Vec<(usize, i64, &Value)> = logs
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let ts = norm_ts(entry.get("ts"))?;
            (ts >= cutoff).then_some((index, ts, entry))
        })
        .collect();

    let mut dedupable_already_indices = HashSet::new();
    let mut latest_already = HashMap::<(String, String), (i64, usize)>::new();
    for (index, ts, entry) in &retained {
        if entry.get("result").and_then(Value::as_str) != Some("already") {
            continue;
        }
        let Some(identity) = legacy_checkin_identity(entry) else {
            continue;
        };
        let Some(date) = checkin_log_local_date(*ts) else {
            continue;
        };
        dedupable_already_indices.insert(*index);
        let candidate = (*ts, *index);
        latest_already
            .entry((identity, date))
            .and_modify(|current| {
                if candidate >= *current {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }

    let winning_already_indices: HashSet<usize> = latest_already
        .into_values()
        .map(|(_, index)| index)
        .collect();
    let mut normalized: Vec<Value> = retained
        .into_iter()
        .filter(|(index, _, entry)| {
            entry.get("result").and_then(Value::as_str) != Some("already")
                || !dedupable_already_indices.contains(index)
                || winning_already_indices.contains(index)
        })
        .map(|(_, _, entry)| entry.clone())
        .collect();

    if normalized.len() > CHECKIN_LOG_MAX_RECORDS {
        normalized.drain(..normalized.len() - CHECKIN_LOG_MAX_RECORDS);
    }
    normalized
}

fn compact_checkin_logs_at(path: &Path, at_ms: i64) -> std::io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)?;
    let Ok(Value::Array(logs)) = serde_json::from_str::<Value>(&text) else {
        // Preserve unreadable user data rather than replacing it with an empty
        // file. Normal log loading keeps its existing tolerant behavior.
        return Ok(false);
    };
    let normalized = normalize_checkin_logs(&logs, at_ms);
    if normalized == logs {
        return Ok(false);
    }
    let content = serde_json::to_string_pretty(&normalized).unwrap_or_default();
    atomic_write(path, &content)?;
    Ok(true)
}

/// Compact legacy persisted check-in logs once during host startup.
///
/// Returns `true` only when the file was rewritten. Loading logs remains a
/// read-only operation; both hosts invoke this explicit migration before their
/// first automatic verification cycle.
pub fn compact_checkin_logs() -> std::io::Result<bool> {
    let _guard = CHECKIN_LOG_WRITE_LOCK.lock().unwrap();
    compact_checkin_logs_at(&checkin_logs_file(), now_ms())
}

/// 追加一条签到日志。
pub fn add_checkin_log(entry: &Value) {
    // Account-scoped check-in coordination permits unrelated accounts to run
    // concurrently. Serialize the file read-modify-write so neither entry is lost.
    let _guard = CHECKIN_LOG_WRITE_LOCK.lock().unwrap();
    let mut logs = load_checkin_logs();
    logs.push(entry.clone());
    let _ = save_checkin_logs_unlocked(&logs);
}

// ---------------------------------------------------------------------------
// 派猫猫旅行配置 / 缓存
// ---------------------------------------------------------------------------

/// 默认自动旅行配置（全新安装默认关闭）。
pub fn default_travel_config() -> Value {
    travel_config_with_enabled(false)
}

/// 历史默认自动旅行配置（`enabled: true`）：已有用户没有显式值时沿用。
fn legacy_default_travel_config() -> Value {
    travel_config_with_enabled(true)
}

fn travel_config_with_enabled(enabled: bool) -> Value {
    json!({ "enabled": enabled })
}

/// 读取自动旅行配置（缺失/损坏时合并默认值）。
///
/// 与签到同理：配置只在显式保存时落盘，按使用痕迹（配置文件或旅行缓存）判定已有用户，
/// 没有显式值时沿用历史默认（开启）；全新安装默认关闭。
pub fn load_travel_config() -> Value {
    load_travel_config_at(&travel_config_file(), &travel_cache_file())
}

fn load_travel_config_at(config_path: &Path, cache_path: &Path) -> Value {
    let input = std::fs::read_to_string(config_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    resolve_travel_config(input.as_ref(), config_path.exists() || cache_path.exists())
}

fn resolve_travel_config(input: Option<&Value>, existing_install: bool) -> Value {
    let mut cfg = if existing_install {
        legacy_default_travel_config()
    } else {
        default_travel_config()
    };
    if let Some(enabled) = input
        .and_then(Value::as_object)
        .and_then(|map| map.get("enabled"))
        .and_then(Value::as_bool)
    {
        cfg["enabled"] = json!(enabled);
    }
    cfg
}

/// 保存自动旅行配置（只保留已知字段）。
pub fn save_travel_config(cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_travel_config();
    if let Some(enabled) = cfg.get("enabled").and_then(Value::as_bool) {
        merged["enabled"] = json!(enabled);
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(&travel_config_file(), &content)
}

/// 读取旅行缓存（`{ date, completed, results: { accountId: {...} } }`）。
pub fn load_travel_cache() -> Value {
    let f = travel_cache_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                return Value::Object(map);
            }
        }
    }
    json!({})
}

/// 保存旅行缓存。
pub fn save_travel_cache(cache: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(cache).unwrap_or_default();
    atomic_write(&travel_cache_file(), &content)
}

// ---------------------------------------------------------------------------
// 活跃地图配置 / 缓存
// ---------------------------------------------------------------------------

/// 活跃地图默认上报时点（本地小时）：跨天后第一次检查就补一轮。
///
/// 2026-10-04 从 10 改为 0：原值是按「人已经开机上班」估的兜底点（迁移报告 §4
/// 「启动补跑已覆盖开机晚于 10 点的场景」），于是每天 00:00–10:00 缓存日期一翻篇
/// 就既不显示也不补跑 —— 空白窗。实测服务端成长中心日界就是 CST 零点
/// （heatmap 10-04 当天已有格子），提前上报不存在「记到昨天」的风险。
pub const ACTIVITY_DEFAULT_HOUR: i64 = 0;

/// `auto_activity_config.json` 的配置版本：v2 起默认时点 10 → 0。
const ACTIVITY_CONFIG_VERSION: i64 = 2;

/// v1（无 `config_version`）遗留配置里的旧默认时点。
const ACTIVITY_LEGACY_DEFAULT_HOUR: i64 = 10;

static ACTIVITY_CACHE_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 串行化活跃地图缓存的读-改-写（自动轮与手动触发可能并发）。
pub fn with_activity_cache_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ACTIVITY_CACHE_WRITE_LOCK.lock().unwrap();
    f()
}

/// 活跃地图自动执行配置（`~/.wb-switch/auto_activity_config.json`）。
pub fn activity_config_file() -> PathBuf {
    store_dir().join("auto_activity_config.json")
}

/// 活跃地图当日缓存（`~/.wb-switch/activity_cache.json`）。
pub fn activity_cache_file() -> PathBuf {
    store_dir().join("activity_cache.json")
}

/// 默认活跃地图配置：默认开启，跨天后第一次检查即上报。
pub fn default_activity_config() -> Value {
    json!({
        "enabled": true,
        "hour": ACTIVITY_DEFAULT_HOUR,
        "config_version": ACTIVITY_CONFIG_VERSION,
    })
}

/// 从指定路径读活跃地图配置（缺失/损坏回落默认；hour 越界忽略）。
///
/// 独立出 `_at` 版本供单测注入临时目录 —— 测试绝不写真实 `store_dir()`
/// （HANDOFF §5 坑 73：`save_*` 单测曾把真实配置改掉、连带把服务停掉）。
pub fn load_activity_config_at(path: &Path) -> Value {
    let mut cfg = default_activity_config();
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                if let Some(enabled) = map.get("enabled").and_then(Value::as_bool) {
                    cfg["enabled"] = json!(enabled);
                }
                if let Some(hour) = map.get("hour").and_then(Value::as_i64) {
                    if (0..=23).contains(&hour) {
                        cfg["hour"] = json!(hour);
                    }
                }
                // v1 → v2 迁移：老配置没有 `config_version`，其中的 hour=10 是旧默认值
                // 而不是主人自己挑的时点，落到新默认；非 10 的值视作显式选择，原样保留。
                // 只在读路径生效（幂等、不写盘），用户随后保存即固化新格式。
                if map.get("config_version").and_then(Value::as_i64)
                    != Some(ACTIVITY_CONFIG_VERSION)
                {
                    let legacy = map.get("hour").and_then(Value::as_i64);
                    if legacy.is_none() || legacy == Some(ACTIVITY_LEGACY_DEFAULT_HOUR) {
                        cfg["hour"] = json!(ACTIVITY_DEFAULT_HOUR);
                    }
                }
            }
        }
    }
    cfg
}

/// 读取活跃地图配置。
pub fn load_activity_config() -> Value {
    load_activity_config_at(&activity_config_file())
}

/// 保存活跃地图配置（只保留已知字段）。
pub fn save_activity_config(cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_activity_config();
    if let Some(enabled) = cfg.get("enabled").and_then(Value::as_bool) {
        merged["enabled"] = json!(enabled);
    }
    if let Some(hour) = cfg.get("hour").and_then(Value::as_i64) {
        if (0..=23).contains(&hour) {
            merged["hour"] = json!(hour);
        }
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(&activity_config_file(), &content)
}

/// 从指定路径读活跃地图缓存。
pub fn load_activity_cache_at(path: &Path) -> Value {
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                return Value::Object(map);
            }
        }
    }
    json!({})
}

/// 读取活跃地图缓存（`{ date, results: { accountId: {...} } }`）。
pub fn load_activity_cache() -> Value {
    load_activity_cache_at(&activity_cache_file())
}

/// 写入指定路径的活跃地图缓存。
pub fn save_activity_cache_at(path: &Path, cache: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(cache).unwrap_or_default();
    atomic_write(path, &content)
}

/// 保存活跃地图缓存。
pub fn save_activity_cache(cache: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(cache).unwrap_or_default();
    atomic_write(&activity_cache_file(), &content)
}

/// 跨模块互斥：growth 任务当日缓存的进程内写锁（与活动地图缓存锁同理，
/// 防止 cycle 并发写同一份 `growth_tasks_cache.json`）。
pub fn with_growth_tasks_cache_lock<T>(f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

// ---------------------------------------------------------------------------
// 成长任务自动执行（夜猫子 / 开学季；来源 2api scripts/task_runner.py + school_open_day_2026.py）
// ---------------------------------------------------------------------------

/// 成长任务自动执行配置（`~/.wb-switch/auto_tasks_config.json`）。
///
/// `enabled` 总开关；`schoolHour` 开学季执行点（CST 小时，默认 12，2api crontab 同款）。
/// 夜猫子不设时点——它按夜猫窗口（23:00–08:00 CST）判定，配置了固定 hour 反而会错过窗口。
pub fn tasks_config_file() -> PathBuf {
    store_dir().join("auto_tasks_config.json")
}

/// 成长任务当日缓存（`~/.wb-switch/growth_tasks_cache.json`，形状同活动地图缓存）。
pub fn growth_tasks_cache_file() -> PathBuf {
    store_dir().join("growth_tasks_cache.json")
}

/// 默认成长任务配置：默认开启。开学季默认 12 点执行。
pub fn default_tasks_config() -> Value {
    json!({ "enabled": true, "schoolHour": TASKS_DEFAULT_SCHOOL_HOUR })
}

pub const TASKS_DEFAULT_SCHOOL_HOUR: i64 = 12;

/// 从指定路径读成长任务配置（缺失/损坏回落默认；hour 越界忽略）。
pub fn load_tasks_config_at(path: &Path) -> Value {
    let mut cfg = default_tasks_config();
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                if let Some(enabled) = map.get("enabled").and_then(Value::as_bool) {
                    cfg["enabled"] = json!(enabled);
                }
                if let Some(hour) = map.get("schoolHour").and_then(Value::as_i64) {
                    if (0..=23).contains(&hour) {
                        cfg["schoolHour"] = json!(hour);
                    }
                }
            }
        }
    }
    cfg
}

/// 读取成长任务配置。
pub fn load_tasks_config() -> Value {
    load_tasks_config_at(&tasks_config_file())
}

/// 保存成长任务配置（只保留已知字段）。
pub fn save_tasks_config(cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_tasks_config();
    if let Some(enabled) = cfg.get("enabled").and_then(Value::as_bool) {
        merged["enabled"] = json!(enabled);
    }
    if let Some(hour) = cfg.get("schoolHour").and_then(Value::as_i64) {
        if (0..=23).contains(&hour) {
            merged["schoolHour"] = json!(hour);
        }
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(&tasks_config_file(), &content)
}

/// 从指定路径读成长任务缓存（形状与活动地图缓存一致）。
pub fn load_growth_tasks_cache_at(path: &Path) -> Value {
    load_activity_cache_at(path)
}

/// 读取成长任务缓存。
pub fn load_growth_tasks_cache() -> Value {
    load_growth_tasks_cache_at(&growth_tasks_cache_file())
}

/// 写入指定路径的成长任务缓存。
pub fn save_growth_tasks_cache_at(path: &Path, cache: &Value) -> std::io::Result<()> {
    save_activity_cache_at(path, cache)
}

/// 保存成长任务缓存。
pub fn save_growth_tasks_cache(cache: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(cache).unwrap_or_default();
    atomic_write(&growth_tasks_cache_file(), &content)
}

// ---------------------------------------------------------------------------
// 限额监听配置（hook 通路 + IDE 日志扫描的总开关）
// ---------------------------------------------------------------------------

/// 默认限额监听配置：默认开启（与改造前「账号页自动显示限额」的行为一致）。
///
/// `hookOptOut` = 用户点过「卸载 hook」→ 不再自动接入；默认 `false`（默认接入）。
/// `scanIdeLogs` = 是否扫描两个 CodeBuddy IDE 的日志；默认 `true`（IDE 的 429 不触发事件，
/// 日志是它唯一的数据源）。关闭只影响 IDE 两源，CLI / WorkBuddy 的 hook 通路与兜底扫描不变。
pub fn default_rate_limit_config() -> Value {
    json!({ "enabled": true, "hookOptOut": false, "scanIdeLogs": true })
}

/// 读取指定的限额监听配置文件（缺失/损坏时合并默认值）。
///
/// 与 `load_rate_limit_config` 分离只为注入路径：单测不得触碰真实 `~/.wb-switch`。
pub fn load_rate_limit_config_at(path: &Path) -> Value {
    let mut cfg = default_rate_limit_config();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
            for key in ["enabled", "hookOptOut", "scanIdeLogs"] {
                if let Some(value) = map.get(key).and_then(Value::as_bool) {
                    cfg[key] = json!(value);
                }
            }
        }
    }
    cfg
}

/// 读取限额监听配置（缺失/损坏时合并默认值）。
pub fn load_rate_limit_config() -> Value {
    load_rate_limit_config_at(&rate_limit_config_file())
}

/// 保存限额监听配置到指定路径（只保留已知字段）。
pub fn save_rate_limit_config_at(path: &Path, cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_rate_limit_config();
    for key in ["enabled", "hookOptOut", "scanIdeLogs"] {
        if let Some(value) = cfg.get(key).and_then(Value::as_bool) {
            merged[key] = json!(value);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(path, &content)
}

/// 只改写 `hookOptOut`（保留 `enabled` 等既有字段），返回写入后的完整配置。
///
/// 用户点「卸载 hook」置 `true`、「接入 hook」置 `false`；两处都不改动别的开关状态。
pub fn set_rate_limit_hook_opt_out_at(path: &Path, opt_out: bool) -> std::io::Result<Value> {
    let mut cfg = load_rate_limit_config_at(path);
    cfg["hookOptOut"] = json!(opt_out);
    save_rate_limit_config_at(path, &cfg)?;
    Ok(load_rate_limit_config_at(path))
}

// ---------------------------------------------------------------------------
// 自动轮换配置 / 日志（CodeBuddy CLI 账号轮换）
// ---------------------------------------------------------------------------

/// 默认自动轮换配置。
pub fn default_auto_rotate_config() -> Value {
    json!({
        "enabled": false,
        "check_interval_minutes": 5,
        "cooldown_minutes": 120,
        "min_gap_hours": 24,
        "min_urgency_hours": 72,
        "active_guard_minutes": 30,
        "min_remaining_credits": 0,
    })
}

/// 读取自动轮换配置（缺失/损坏时合并默认值）。
pub fn load_auto_rotate_config() -> Value {
    let mut cfg = default_auto_rotate_config();
    let f = auto_rotate_config_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
                for (k, v) in map {
                    cfg[k] = v;
                }
            }
        }
    }
    cfg
}

/// 保存自动轮换配置（只保留已知字段）。
pub fn save_auto_rotate_config(cfg: &Value) -> std::io::Result<()> {
    let mut merged = default_auto_rotate_config();
    let allowed: Vec<&str> = vec![
        "enabled",
        "check_interval_minutes",
        "cooldown_minutes",
        "min_gap_hours",
        "min_urgency_hours",
        "active_guard_minutes",
        "min_remaining_credits",
    ];
    for k in allowed {
        if let Some(v) = cfg.get(k) {
            merged[k] = v.clone();
        }
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&merged).unwrap_or_default();
    atomic_write(&auto_rotate_config_file(), &content)
}

/// 读取自动轮换日志。
pub fn load_rotate_logs() -> Vec<Value> {
    let f = auto_rotate_logs_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&text) {
                return arr;
            }
        }
    }
    vec![]
}

/// 保存自动轮换日志（保留最近 N 条，保持插入顺序）。
pub fn save_rotate_logs(logs: &[Value]) -> std::io::Result<()> {
    let mut kept: Vec<Value> = logs.to_vec();
    if kept.len() > ROTATE_LOG_MAX_RECORDS {
        kept.drain(..kept.len() - ROTATE_LOG_MAX_RECORDS);
    }
    std::fs::create_dir_all(store_dir())?;
    let content = serde_json::to_string_pretty(&kept).unwrap_or_default();
    atomic_write(&auto_rotate_logs_file(), &content)
}

/// 追加一条自动轮换日志。
pub fn add_rotate_log(entry: &Value) {
    let mut logs = load_rotate_logs();
    logs.push(entry.clone());
    let _ = save_rotate_logs(&logs);
}

// ---------------------------------------------------------------------------
// 轮换推迟提示预算（`~/.wb-switch/auto_rotate_notify.json`）
// ---------------------------------------------------------------------------

/// 提示预算文件名（`store_dir()/auto_rotate_notify.json`）。
const ROTATE_NOTIFY_FILE_NAME: &str = "auto_rotate_notify.json";

/// 同一自然日内最多提示几次；超出只写轮换日志，不再打扰用户。
pub const ROTATE_NOTIFY_DAILY_LIMIT: u32 = 5;

static ROTATE_NOTIFY_LOCK: Mutex<()> = Mutex::new(());

pub fn auto_rotate_notify_file() -> PathBuf {
    store_dir().join(ROTATE_NOTIFY_FILE_NAME)
}

/// 本地日期（`YYYY-MM-DD`）：提示预算的跨日重置口径（与签到日志同一套本地时间）。
fn local_date(at_ms: i64) -> String {
    Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 读当日已用次数：文件缺失 / 损坏 / 日期不是今天（跨日）一律按 0 计。
fn rotate_notify_count_at(path: &Path, today: &str) -> u32 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return 0;
    };
    if value.get("date").and_then(Value::as_str) != Some(today) {
        return 0;
    }
    value
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

/// 领取一次「轮换推迟提示」配额：`true` = 可以投递（并且已经计数）。
///
/// 同自然日上限 [`ROTATE_NOTIFY_DAILY_LIMIT`]，跨日按本地日期清零；读取失败/损坏视为 0，
/// 不阻塞轮换。预算文件写不进去时不投递——宁可少一条通知，也不要每轮都弹。
pub fn try_consume_rotate_notify(at_ms: i64) -> bool {
    let _guard = ROTATE_NOTIFY_LOCK.lock().unwrap();
    let path = auto_rotate_notify_file();
    try_consume_rotate_notify_at(&path, &local_date(at_ms))
}

fn try_consume_rotate_notify_at(path: &Path, today: &str) -> bool {
    if today.is_empty() {
        return false;
    }
    let used = rotate_notify_count_at(path, today);
    if used >= ROTATE_NOTIFY_DAILY_LIMIT {
        return false;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let content = serde_json::to_string_pretty(&json!({
        "date": today,
        "count": used + 1,
    }))
    .unwrap_or_default();
    atomic_write(path, &content).is_ok()
}

// ---------------------------------------------------------------------------
// 并发运行标志（替代 Python threading.Lock，Send 安全可跨 await）
// ---------------------------------------------------------------------------

/// RAII 运行标志：进入临界区置 true，Drop 时复位。
pub struct RunFlagGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> RunFlagGuard<'a> {
    /// 尝试获取标志；已被占用返回 None。
    pub fn try_acquire(flag: &'a AtomicBool) -> Option<Self> {
        if flag.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Self { flag })
        }
    }
}

impl Drop for RunFlagGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// 时间
// ---------------------------------------------------------------------------

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn utc_iso() -> String {
    // 对照 Python utc_iso：%Y-%m-%dT%H-%M-%S + "Z"
    format!("{}Z", chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S"))
}

// ---------------------------------------------------------------------------
// 文件
// ---------------------------------------------------------------------------

/// 原子写文件（临时文件 + rename），对照 Python atomic_write。
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    if let Err(e) = std::fs::write(&tmp, content) {
        eprintln!("[atomic] write tmp FAILED: {e}");
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        eprintln!("[atomic] rename FAILED: {e}");
        // rename 失败时清理临时文件，避免在目标目录残留 `<name>.tmp-*`。
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 时间戳归一化
// ---------------------------------------------------------------------------

/// 把秒/毫秒/字符串时间戳统一为毫秒；无效返回 None。对照 server.py `_norm_ts`。
pub fn norm_ts(v: Option<&Value>) -> Option<i64> {
    let mut ts: i64 = match v {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok()? as i64,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?,
        _ => return None,
    };
    if ts < 10_000_000_000 {
        ts *= 1000; // 秒 → 毫秒
    }
    Some(ts)
}

// ---------------------------------------------------------------------------
// HTTP 客户端（对照 Python http_request）
// ---------------------------------------------------------------------------

/// 响应是否为「该路径不存在」（HTTP 404）。
///
/// billing 路径候选回落**只允许由 404 触发**：401/403 是鉴权问题、10085 是网关
/// 客户端指纹拦截、`code=-1` 是传输错误，把它们误当成路径问题会掩盖真实原因，
/// 也会白白重试一遍并把错误码盖成 404（见 design D4）。
pub fn is_route_missing(response: &Value) -> bool {
    fn parse_code(value: &Value) -> Option<i64> {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    }
    response
        .get("code")
        .and_then(parse_code)
        .or_else(|| response.get("data")?.get("code").and_then(parse_code))
        == Some(404)
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(DEFAULT_HTTP_USER_AGENT)
}

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        http_client_builder()
            .build()
            .expect("failed to build reqwest client")
    })
}

/// 通用 HTTP 请求，返回解析后的 JSON。
///
/// 行为对齐 Python 版：
/// - 2xx：解析 body 为 JSON；
/// - HTTP 错误：body 可解析则返回其 JSON，否则 `{"code": <status>, "message": <body 前 500 字符>}`；
/// - 网络错误：`{"code": -1, "message": <原因>}`。
pub async fn http_request(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
) -> Value {
    http_request_with_proxy(url, method, body, headers, None).await
}

/// 通用 HTTP 请求，可为单次请求显式指定 HTTP/HTTPS 代理。
pub async fn http_request_with_proxy(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
) -> Value {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => match http_client_builder()
            .proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return json!({"code": -1, "message": format!("代理地址无效: {e}")}),
            })
            .build()
        {
            Ok(client) => client,
            Err(e) => return json!({"code": -1, "message": format!("代理客户端创建失败: {e}")}),
        },
        None => http_client().clone(),
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                serde_json::from_str(&text).unwrap_or(Value::Null)
            } else {
                serde_json::from_str(&text).unwrap_or_else(|_| {
                    json!({
                        "code": status.as_u16(),
                        "message": normalize_error_body(&text),
                    })
                })
            }
        }
        Err(e) => json!({"code": -1, "message": e.to_string()}),
    }
}

/// 判定 `http_request` 的失败是否属于**网络层**（连不上 / 代理未起 / DNS / TLS / 超时），
/// 而不是服务端明确拒绝。
///
/// 必须与上面的约定配套读：`code = -1` 表示请求根本没发出去，与 401 / 12153
/// `invalid_grant` 这类「服务端收了并拒绝」混在同一个返回值里。调用方若不区分，
/// 一次网络抖动就会被当成认证失败 —— 2026-10-03 实证：开机早于代理就绪的那轮
/// 保活把 7/7 账号全标成 `needs_relogin`，而 token 实际还剩 45 天。
///
/// 放在本文件是因为约定（`code = -1`）写在这里，判定要贴着约定走。
pub fn is_network_error(code: i64, message: &str) -> bool {
    code == -1 && is_network_message(message)
}

/// 只看文案判网络层失败：供「上游已把 code 吞掉、只剩一句错误文案」的场景复用
/// （活跃地图的 `status=error` 只带 `message`，拿不到 `code`）。
///
/// 拆成两个函数是为了让 `is_network_error` 保持「code 门槛 + 文案」两层判定不变，
/// 避免调用方为了判网络错去伪造一个 `code`。
pub fn is_network_message(message: &str) -> bool {
    // reqwest::Error 的 Display 前缀固定为 "error sending request for url (..)"，
    // 其余 hint 覆盖 connect / proxy / dns / tls / timeout 各子类。
    const NET_HINTS: [&str; 8] = [
        "error sending request",
        "connection",
        "timed out",
        "timeout",
        "dns",
        "proxy",
        "certificate",
        "tls",
    ];
    let lower = message.to_ascii_lowercase();
    NET_HINTS.iter().any(|hint| lower.contains(hint))
}

/// 无 rand 依赖的抖动源：时间 ⊕ 自增计数混乘（同毫秒调用也不重样）。
///
/// 由 growth_tasks 提到此处，供批量链路（保活 / 活跃地图）共用——三处各写一份
/// 恒温代码没意义，而抖动口径必须统一才谈得上「不像脚本」。
pub fn jitter_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    now_ms() as u64 ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// 多账号批量动作之间的抖动间隔（2~8s）。
///
/// 为什么必须有：7 个账号不加间隔时，全程只差若干个网络 RTT（实测 2.03s / 7 号，
/// 单间隔恒定在 270~387ms）。「同一 IP + 秒级完成 + 恒定间隔 + 固定表序」四项
/// 凑齐就是一份现成的行为指纹。growth_tasks 早对高频写动作做了同款防护
/// （800~2500ms 抖动 + 每号洗牌），低频的保活 / 活跃当时漏了，这里补齐。
///
/// 低频动作可以大方一点，故区间比 `growth_tasks::write_gap` 更宽。
pub async fn batch_gap() {
    let ms = 2_000 + jitter_u64() % 6_000;
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// 按种子确定性洗牌（Fisher-Yates）。
///
/// 多账号若每轮都按账号库表序连发，服务端拿到的是一串相同的 uid 先后关系，
/// 同样是可聚类特征；固定顺序 + 恒定间隔一起出现时尤其明显。
pub fn shuffle_by_seed<T>(items: &mut [T], mut seed: u64) {
    if items.len() < 2 {
        return;
    }
    for i in (1..items.len()).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = ((seed >> 33) as usize) % (i + 1);
        items.swap(i, j);
    }
}

/// 非 JSON 错误响应体归一化：网关（openresty / APISIX 等）的 401/5xx 常返回
/// 整页 HTML，原样截断会把 `<html>…` 整段塞进通知与界面卡片（issue #94）。
/// HTML 提取 `<title>` 作为可读信息；其余保持原有的 500 字符截断。
fn normalize_error_body(text: &str) -> String {
    if text.trim_start().starts_with('<') {
        let title = text
            .split_once("<title>")
            .and_then(|(_, rest)| rest.split_once("</title>"))
            .map(|(title, _)| title.trim())
            .unwrap_or_default();
        return if title.is_empty() {
            "服务端返回 HTML 错误页（无标题）".to_string()
        } else {
            format!("服务端返回 HTML 错误页：{title}")
        };
    }
    text.chars().take(500).collect::<String>()
}

/// 通用 HTTP 请求，返回原始响应（状态码 + 响应头 + 响应体），可选是否跟随重定向。
///
/// 供需要读取响应头（如 302 的 `Location`）或自行处理非 JSON 响应的场景使用；
/// 其余场景优先用 [`http_request_with_proxy`]。失败（网络错误 / 代理配置错误）
/// 返回 `(0, HashMap::new(), 错误信息)`，由调用方根据 status 判断。
pub async fn http_request_raw(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
    follow_redirects: bool,
) -> (u16, HashMap<String, String>, String) {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => {
            let mut builder = http_client_builder().proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return (0, HashMap::new(), format!("代理地址无效: {e}")),
            });
            if !follow_redirects {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
            match builder.build() {
                Ok(client) => client,
                Err(e) => return (0, HashMap::new(), format!("代理客户端创建失败: {e}")),
            }
        }
        None => {
            if follow_redirects {
                http_client().clone()
            } else {
                match http_client_builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                {
                    Ok(client) => client,
                    Err(e) => return (0, HashMap::new(), format!("客户端创建失败: {e}")),
                }
            }
        }
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut resp_headers = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(vs) = v.to_str() {
                    resp_headers.insert(k.as_str().to_string(), vs.to_string());
                }
            }
            let text = resp.text().await.unwrap_or_default();
            (status, resp_headers, text)
        }
        Err(e) => (0, HashMap::new(), e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归 issue #94：网关 401 返回的整页 HTML 要归一化为可读信息，
    /// 不能把 `<html>…` 原样塞进通知与界面卡片。
    #[test]
    fn normalize_error_body_extracts_html_title() {
        let html = "<html>\n<head><title>401 Authorization Required</title></head>\n\
                    <body>\n<center><h1>401 Authorization Required</h1></center>\n\
                    <hr><center>openresty</center>\n</body>\n</html>\n";
        assert_eq!(
            normalize_error_body(html),
            "服务端返回 HTML 错误页：401 Authorization Required"
        );

        assert_eq!(
            normalize_error_body("<!DOCTYPE html><html><body>boom</body></html>"),
            "服务端返回 HTML 错误页（无标题）"
        );

        // 非 HTML 错误体保持原有截断行为。
        let plain = "plain gateway error";
        assert_eq!(normalize_error_body(plain), plain);
        let long = "x".repeat(600);
        assert_eq!(normalize_error_body(&long).chars().count(), 500);
    }

    fn local_timestamp_ms(year: i32, month: u32, day: u32, hour: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("test timestamp must be unambiguous")
            .timestamp_millis()
    }

    #[test]
    fn auto_checkin_defaults_disabled_and_preserves_legacy_fields() {
        let cfg = default_checkin_config();
        assert_eq!(cfg.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(cfg["excluded_account_ids"], json!([]));
        assert_eq!(cfg.get("start_hour").and_then(Value::as_i64), Some(6));
        assert_eq!(cfg.get("end_hour").and_then(Value::as_i64), Some(12));

        // 历史默认仍保留开启，供已有用户在读取路径上沿用。
        let legacy = legacy_default_checkin_config();
        assert_eq!(legacy.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            legacy.get("lazy_refresh_hours").and_then(Value::as_i64),
            Some(24)
        );
    }

    #[test]
    fn auto_checkin_exclusions_survive_config_roundtrip_and_global_toggle() {
        let cfg = merge_checkin_config(&json!({
            "enabled": true,
            "excluded_account_ids": ["account-b", "account-a", "account-b", "", "  ", null, 42],
            "checkin_start": "9:5",
            "checkin_end": "12:00",
            "keepalive_days": 7
        }));
        assert_eq!(
            cfg["excluded_account_ids"],
            json!(["account-b", "account-a"])
        );
        let serialized = serde_json::to_string(&cfg).unwrap();
        let mut reloaded: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(merge_checkin_config(&reloaded), cfg);
        reloaded["enabled"] = json!(false);
        let disabled = merge_checkin_config(&reloaded);
        assert_eq!(
            disabled["excluded_account_ids"],
            cfg["excluded_account_ids"]
        );
        assert_eq!(disabled["checkin_start"], "09:05");
        assert_eq!(disabled["keepalive_days"], 7);
    }

    #[test]
    fn auto_checkin_legacy_or_invalid_exclusions_default_to_empty() {
        assert_eq!(
            merge_checkin_config(&json!({}))["excluded_account_ids"],
            json!([])
        );
        for invalid in [
            json!(null),
            json!(true),
            json!("account-a"),
            json!({"id": "account-a"}),
        ] {
            assert_eq!(
                merge_checkin_config(&json!({"excluded_account_ids": invalid}))
                    ["excluded_account_ids"],
                json!([])
            );
        }
    }

    #[test]
    fn auto_checkin_exclusions_survive_usage_trace_defaults() {
        let input = json!({"excluded_account_ids": ["account-a", "account-a", null]});
        for existing_install in [false, true] {
            let resolved = resolve_checkin_config(Some(&input), existing_install);
            assert_eq!(resolved["enabled"], json!(existing_install));
            assert_eq!(resolved["excluded_account_ids"], json!(["account-a"]));
            // 保存已解析配置后，新老安装均保留开关状态和账号排除列表。
            assert_eq!(merge_checkin_config(&resolved), resolved);
        }
    }

    #[test]
    fn checkin_default_follows_usage_trace() {
        // 全新安装（无配置文件、无签到日志）：默认关闭。
        let fresh = resolve_checkin_config(None, false);
        assert_eq!(fresh.get("enabled").and_then(Value::as_bool), Some(false));

        // 已有用户（签到日志即使用痕迹）：沿用历史默认开启，升级不改状态。
        let existing = resolve_checkin_config(None, true);
        assert_eq!(existing.get("enabled").and_then(Value::as_bool), Some(true));
        // 痕迹只影响 enabled，其余字段仍与默认一致。
        assert_eq!(
            existing.get("lazy_refresh_hours").and_then(Value::as_i64),
            Some(24)
        );
        assert_eq!(existing.get("checkin_start"), Some(&json!("")));

        // 显式值优先于痕迹。
        for existing_install in [false, true] {
            let off = resolve_checkin_config(Some(&json!({"enabled": false})), existing_install);
            assert_eq!(off.get("enabled").and_then(Value::as_bool), Some(false));
            let on = resolve_checkin_config(Some(&json!({"enabled": true})), existing_install);
            assert_eq!(on.get("enabled").and_then(Value::as_bool), Some(true));
        }
    }

    #[test]
    fn load_checkin_config_uses_usage_trace_files() {
        let dir =
            std::env::temp_dir().join(format!("wb-switch-checkin-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("auto_checkin_config.json");
        let logs_path = dir.join("auto_checkin_logs.json");

        // 两者皆无 → 全新安装，默认关闭。
        assert_eq!(
            load_checkin_config_at(&config_path, &logs_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );

        // 只有签到日志（使用痕迹）→ 已有用户，保持开启。
        std::fs::write(&logs_path, "[]").unwrap();
        assert_eq!(
            load_checkin_config_at(&config_path, &logs_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );

        // 显式保存的 false 覆盖痕迹。
        std::fs::write(&config_path, "{\"enabled\": false}").unwrap();
        assert_eq!(
            load_checkin_config_at(&config_path, &logs_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );

        // 配置文件损坏但存在 → 仍按已有用户处理（不因损坏而改状态）。
        std::fs::remove_file(&logs_path).unwrap();
        std::fs::write(&config_path, "not-json").unwrap();
        assert_eq!(
            load_checkin_config_at(&config_path, &logs_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn travel_default_follows_usage_trace() {
        // 全新安装默认关闭，已有用户（旅行缓存痕迹）沿用开启。
        assert_eq!(
            resolve_travel_config(None, false)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            resolve_travel_config(None, true)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );

        // 显式值优先；非法值回落基线。
        assert_eq!(
            resolve_travel_config(Some(&json!({"enabled": false})), true)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            resolve_travel_config(Some(&json!({"enabled": true})), false)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            resolve_travel_config(Some(&json!({"enabled": "no"})), false)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn load_travel_config_uses_usage_trace_files() {
        let dir =
            std::env::temp_dir().join(format!("wb-switch-travel-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("auto_travel_config.json");
        let cache_path = dir.join("travel_cache.json");

        // 两者皆无 → 全新安装，默认关闭。
        assert_eq!(
            load_travel_config_at(&config_path, &cache_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );

        // 只有旅行缓存（使用痕迹）→ 已有用户，保持开启。
        std::fs::write(&cache_path, "{}").unwrap();
        assert_eq!(
            load_travel_config_at(&config_path, &cache_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );

        // 显式保存的 false 覆盖痕迹。
        std::fs::write(&config_path, "{\"enabled\": false}").unwrap();
        assert_eq!(
            load_travel_config_at(&config_path, &cache_path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(false)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_checkin_explicit_value_wins_and_invalid_value_uses_default() {
        // 保存路径（以新默认值为基线）：显式值原样保留，缺失/非法才回落默认。
        let enabled = merge_checkin_config(&json!({"enabled": true, "keepalive_days": 7}));
        assert_eq!(enabled.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            enabled.get("keepalive_days").and_then(Value::as_i64),
            Some(7)
        );

        let disabled = merge_checkin_config(&json!({"enabled": false, "keepalive_days": 7}));
        assert_eq!(
            disabled.get("enabled").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            disabled.get("keepalive_days").and_then(Value::as_i64),
            Some(7)
        );

        let corrupt = merge_checkin_config(&json!({"enabled": "no", "lazy_refresh_hours": null}));
        assert_eq!(corrupt.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(
            corrupt.get("lazy_refresh_hours").and_then(Value::as_i64),
            Some(24)
        );
    }

    #[test]
    fn parse_clock_accepts_short_forms_and_rejects_malformed_values() {
        assert_eq!(parse_clock("22:00"), Some((22, 0)));
        assert_eq!(parse_clock("9:5"), Some((9, 5)));
        assert_eq!(parse_clock("00:00"), Some((0, 0)));
        assert_eq!(parse_clock("23:59"), Some((23, 59)));

        for raw in [
            "24:00",     // 小时越界
            "23:60",     // 分钟越界
            "22",        // 缺分钟
            "",          // 空串
            "abc",       // 非数字
            "22:00:00",  // 多余字符
            "22:",       // 分钟为空
            ":00",       // 小时为空
            "-1:00",     // 符号
            "+1:00",     // 符号
            " 22:00",    // 前导空格
            "22:00 ",    // 尾随空格
            "０１:００", // 全角数字
        ] {
            assert_eq!(parse_clock(raw), None, "必须拒绝 {raw:?}");
        }
    }

    #[test]
    fn checkin_window_defaults_to_unset_and_normalizes_on_save() {
        let defaults = default_checkin_config();
        assert_eq!(defaults.get("checkin_start"), Some(&json!("")));
        assert_eq!(defaults.get("checkin_end"), Some(&json!("")));

        // 合法值零填充后落盘。
        let merged = merge_checkin_config(&json!({
            "checkin_start": "9:5",
            "checkin_end": "23:30"
        }));
        assert_eq!(merged.get("checkin_start"), Some(&json!("09:05")));
        assert_eq!(merged.get("checkin_end"), Some(&json!("23:30")));
    }

    #[test]
    fn checkin_window_invalid_values_normalize_to_empty_and_keep_other_fields() {
        // 缺失 → 空串。
        let missing = merge_checkin_config(&json!({}));
        assert_eq!(missing.get("checkin_start"), Some(&json!("")));
        assert_eq!(missing.get("checkin_end"), Some(&json!("")));

        // 非字符串 / 越界 / 格式错误 → 空串，不落盘未知值。
        for bad in [json!(1234), json!(null), json!(true), json!("25:00")] {
            let merged = merge_checkin_config(&json!({"checkin_start": bad}));
            assert_eq!(
                merged.get("checkin_start"),
                Some(&json!("")),
                "非法取值必须归一为空串: {bad:?}"
            );
        }

        // 其它字段与旧时间窗口字段不受影响。
        let merged = merge_checkin_config(&json!({
            "checkin_start": "25:00",
            "checkin_end": 1234,
            "enabled": false,
            "keepalive_days": 7,
            "start_hour": 3,
            "end_hour": 9
        }));
        assert_eq!(merged.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(
            merged.get("keepalive_days").and_then(Value::as_i64),
            Some(7)
        );
        assert_eq!(merged.get("start_hour").and_then(Value::as_i64), Some(3));
        assert_eq!(merged.get("end_hour").and_then(Value::as_i64), Some(9));
        assert_eq!(
            merged.get("lazy_refresh_hours").and_then(Value::as_i64),
            Some(24)
        );
    }

    #[test]
    fn checkin_log_normalization_keeps_latest_already_per_identity_and_local_date() {
        let day = local_timestamp_ms(2026, 8, 20, 12);
        let logs = vec![
            json!({"accountId": "a", "email": "same", "result": "already", "ts": day + 1, "marker": "a-old"}),
            json!({"accountId": "b", "email": "same", "result": "already", "ts": day + 2, "marker": "b"}),
            json!({"accountId": "a", "email": "same", "result": "already", "ts": day + 3, "marker": "a-new"}),
            json!({"email": "legacy@example.com", "result": "already", "ts": day + 4, "marker": "legacy-old"}),
            json!({"email": "legacy@example.com", "result": "already", "ts": day + 5, "marker": "legacy-new"}),
            json!({"result": "already", "ts": day + 6, "marker": "no-identity"}),
        ];

        let normalized = normalize_checkin_logs(&logs, day + 10);
        let markers: Vec<&str> = normalized
            .iter()
            .filter_map(|entry| entry.get("marker").and_then(Value::as_str))
            .collect();

        assert_eq!(markers, vec!["b", "a-new", "legacy-new", "no-identity"]);
    }

    #[test]
    fn checkin_log_identity_namespaces_stable_ids_and_legacy_email_fallbacks() {
        let day = local_timestamp_ms(2026, 8, 20, 12);
        let logs = vec![
            json!({"accountId": "same@example.com", "email": "display", "result": "already", "ts": day + 1, "marker": "stable-old"}),
            json!({"email": "same@example.com", "result": "already", "ts": day + 2, "marker": "legacy-old"}),
            json!({"accountId": "same@example.com", "email": "display", "result": "already", "ts": day + 3, "marker": "stable-new"}),
            json!({"email": "same@example.com", "result": "already", "ts": day + 4, "marker": "legacy-new"}),
        ];

        let normalized = normalize_checkin_logs(&logs, day + 10);
        let markers: Vec<&str> = normalized
            .iter()
            .filter_map(|entry| entry.get("marker").and_then(Value::as_str))
            .collect();

        assert_eq!(markers, vec!["stable-new", "legacy-new"]);
    }

    #[test]
    fn checkin_log_normalization_keeps_already_for_separate_dates() {
        let first_day = local_timestamp_ms(2026, 8, 19, 12);
        let second_day = local_timestamp_ms(2026, 8, 20, 12);
        let logs = vec![
            json!({"accountId": "a", "result": "already", "ts": first_day}),
            json!({"accountId": "a", "result": "already", "ts": second_day}),
        ];

        assert_eq!(normalize_checkin_logs(&logs, second_day).len(), 2);
    }

    #[test]
    fn checkin_log_normalization_preserves_success_and_error_multiplicity() {
        let day = local_timestamp_ms(2026, 8, 20, 12);
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": day + 1}),
            json!({"accountId": "a", "result": "success", "ts": day + 2}),
            json!({"accountId": "a", "result": "error", "ts": day + 3}),
            json!({"accountId": "a", "result": "error", "ts": day + 4}),
        ];

        assert_eq!(normalize_checkin_logs(&logs, day + 10), logs);
    }

    #[test]
    fn checkin_log_normalization_applies_retention_and_record_cap() {
        let now = local_timestamp_ms(2026, 8, 20, 12);
        let cutoff = now - CHECKIN_LOG_KEEP_DAYS * 24 * 3600 * 1000;
        let mut logs = vec![json!({
            "accountId": "old",
            "result": "success",
            "ts": cutoff - 1,
            "marker": -1,
        })];
        logs.extend((0..505).map(|marker| {
            json!({
                "accountId": "a",
                "result": "success",
                "ts": now,
                "marker": marker,
            })
        }));

        let normalized = normalize_checkin_logs(&logs, now);
        assert_eq!(normalized.len(), CHECKIN_LOG_MAX_RECORDS);
        assert_eq!(normalized[0]["marker"], 5);
        assert_eq!(normalized.last().unwrap()["marker"], 504);
    }

    #[test]
    fn checkin_log_normalization_deduplicates_before_taking_final_500() {
        let now = local_timestamp_ms(2026, 8, 20, 12);
        let mut logs = vec![
            json!({"accountId": "duplicate", "result": "already", "ts": now - 2, "marker": "duplicate-old"}),
            json!({"accountId": "duplicate", "result": "already", "ts": now - 1, "marker": "duplicate-new"}),
        ];
        logs.extend((0..500).map(|marker| {
            json!({
                "accountId": "a",
                "result": "success",
                "ts": now,
                "marker": marker,
            })
        }));

        let normalized = normalize_checkin_logs(&logs, now);
        assert_eq!(normalized.len(), CHECKIN_LOG_MAX_RECORDS);
        assert_eq!(normalized[0]["marker"], 0);
        assert_eq!(normalized.last().unwrap()["marker"], 499);
        assert!(normalized
            .iter()
            .all(|entry| entry["marker"] != "duplicate-old"));
    }

    #[test]
    fn checkin_log_normalization_is_idempotent() {
        let day = local_timestamp_ms(2026, 8, 20, 12);
        let logs = vec![
            json!({"accountId": "a", "result": "already", "ts": day + 1}),
            json!({"accountId": "a", "result": "already", "ts": day + 2}),
            json!({"accountId": "a", "result": "success", "ts": day + 3}),
        ];

        let once = normalize_checkin_logs(&logs, day + 10);
        assert_eq!(normalize_checkin_logs(&once, day + 10), once);
    }

    #[test]
    fn persisted_checkin_log_compaction_writes_only_when_changed() {
        let day = local_timestamp_ms(2026, 8, 20, 12);
        let dir = std::env::temp_dir().join(format!(
            "wb-switch-checkin-log-compaction-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("logs.json");
        let logs = json!([
            {"accountId": "a", "result": "already", "ts": day + 1},
            {"accountId": "a", "result": "already", "ts": day + 2}
        ]);
        std::fs::write(&path, serde_json::to_string_pretty(&logs).unwrap()).unwrap();

        assert!(compact_checkin_logs_at(&path, day + 10).unwrap());
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert!(!compact_checkin_logs_at(&path, day + 10).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);

        let missing = dir.join("missing.json");
        assert!(!compact_checkin_logs_at(&missing, day + 10).unwrap());
        assert!(!missing.exists());

        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, "not-json").unwrap();
        assert!(!compact_checkin_logs_at(&corrupt, day + 10).unwrap());
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "not-json");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parse_workbuddy_exe_cache_json_reads_exe() {
        let path = parse_workbuddy_exe_cache_json(
            r#"{ "exe": "D:\\Users\\Zhou\\AppData\\Local\\Programs\\WorkBuddy\\WorkBuddy.exe" }"#,
        )
        .expect("valid cache");
        assert_eq!(
            path.to_string_lossy(),
            r"D:\Users\Zhou\AppData\Local\Programs\WorkBuddy\WorkBuddy.exe"
        );
    }

    #[test]
    fn parse_workbuddy_exe_cache_json_ignores_corrupt_and_empty() {
        assert!(parse_workbuddy_exe_cache_json("not-json").is_none());
        assert!(parse_workbuddy_exe_cache_json(r#"{ "exe": "  " }"#).is_none());
        assert!(parse_workbuddy_exe_cache_json("{}").is_none());
    }

    #[test]
    fn workbuddy_exe_cache_reads_new_format_per_variant() {
        let text = r#"{
  "cn": "C:\\Programs\\WorkBuddy\\WorkBuddy.exe",
  "ai": "C:\\Programs\\WorkBuddyAI\\WorkBuddyAI.exe"
}"#;
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(text, WbVariant::Cn)
                .unwrap()
                .to_string_lossy(),
            r"C:\Programs\WorkBuddy\WorkBuddy.exe"
        );
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(text, WbVariant::Ai)
                .unwrap()
                .to_string_lossy(),
            r"C:\Programs\WorkBuddyAI\WorkBuddyAI.exe"
        );
    }

    #[test]
    fn workbuddy_exe_cache_reads_legacy_single_key_as_cn_only() {
        let text = r#"{ "exe": "C:\\Programs\\WorkBuddy\\WorkBuddy.exe" }"#;
        assert!(parse_workbuddy_exe_cache_json_for(text, WbVariant::Cn).is_some());
        assert!(parse_workbuddy_exe_cache_json_for(text, WbVariant::Ai).is_none());
        assert!(parse_workbuddy_exe_cache_json_for(r#"{ "exe": "  " }"#, WbVariant::Cn).is_none());
        assert!(parse_workbuddy_exe_cache_json_for("not-json", WbVariant::Cn).is_none());
    }

    #[test]
    fn workbuddy_exe_cache_write_upgrades_legacy_and_keeps_both_keys() {
        // 旧格式写入国际版 → 升级为新格式，且国内版旧值迁到 cn 键
        let migrated = upsert_workbuddy_exe_cache_json(
            r#"{ "exe": "/Applications/WorkBuddy.app" }"#,
            WbVariant::Ai,
            Path::new("/Applications/WorkBuddy AI.app"),
        );
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(&migrated, WbVariant::Cn)
                .unwrap()
                .to_string_lossy(),
            "/Applications/WorkBuddy.app"
        );
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(&migrated, WbVariant::Ai)
                .unwrap()
                .to_string_lossy(),
            "/Applications/WorkBuddy AI.app"
        );
        assert!(!migrated.contains("\"exe\""), "写回新格式: {migrated}");

        // 再写国内版：两档位互不覆盖
        let both = upsert_workbuddy_exe_cache_json(
            &migrated,
            WbVariant::Cn,
            Path::new("/Applications/CodeBuddy.app"),
        );
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(&both, WbVariant::Cn)
                .unwrap()
                .to_string_lossy(),
            "/Applications/CodeBuddy.app"
        );
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(&both, WbVariant::Ai)
                .unwrap()
                .to_string_lossy(),
            "/Applications/WorkBuddy AI.app"
        );

        // 损坏内容不从零继承，直接重建
        let recovered =
            upsert_workbuddy_exe_cache_json("not-json", WbVariant::Ai, Path::new("/x/a"));
        assert_eq!(
            parse_workbuddy_exe_cache_json_for(&recovered, WbVariant::Ai)
                .unwrap()
                .to_string_lossy(),
            "/x/a"
        );
    }

    #[test]
    fn parse_codebuddy_cn_app_cache_json_reads_exe() {
        let path =
            parse_codebuddy_cn_app_cache_json(r#"{ "exe": "/Applications/CodeBuddy CN.app" }"#)
                .expect("valid cache");
        assert_eq!(path.to_string_lossy(), "/Applications/CodeBuddy CN.app");
    }

    #[test]
    fn parse_codebuddy_cn_app_cache_json_ignores_corrupt_and_empty() {
        assert!(parse_codebuddy_cn_app_cache_json("not-json").is_none());
        assert!(parse_codebuddy_cn_app_cache_json(r#"{ "exe": "  " }"#).is_none());
        assert!(parse_codebuddy_cn_app_cache_json("{}").is_none());
    }

    #[test]
    fn codebuddy_cn_app_cache_file_is_not_workbuddy_exe_cache() {
        assert_ne!(codebuddy_cn_app_cache_file(), workbuddy_exe_cache_file());
        assert!(codebuddy_cn_app_cache_file()
            .file_name()
            .is_some_and(|n| n == "codebuddy_cn_app.json"));
        assert_ne!(
            codebuddy_ide_app_cache_file(),
            codebuddy_cn_app_cache_file()
        );
        assert!(codebuddy_ide_app_cache_file()
            .file_name()
            .is_some_and(|n| n == "codebuddy_ide_app.json"));
    }

    #[test]
    fn default_http_user_agent_matches_official_chrome_desktop() {
        assert_eq!(
            DEFAULT_HTTP_USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36"
        );
        let _ = http_client_builder();
    }

    // 坑（2026-10-03）：`code = -1` 是「请求没发出去」，与服务端拒绝同走
    // `code != 0` 分支，不区分就会把一次网络抖动判成认证失败。下面两组断言
    // 把两类错误钉死在 http_request 的约定旁。
    #[test]
    fn transport_failure_is_network_error() {
        let msg =
            "error sending request for url (https://www.codebuddy.cn/v2/plugin/auth/token/refresh)";
        assert!(is_network_error(-1, msg));
        assert!(is_network_error(-1, "connection refused (os error 10061)"));
        assert!(is_network_error(-1, "operation timed out"));
    }

    #[test]
    fn auth_failure_is_not_network_error() {
        // 服务端明确拒绝 → 调用方必须按认证失败处理（写 needs_relogin）。
        assert!(!is_network_error(12153, "invalid_grant"));
        assert!(!is_network_error(401, "Unauthorized"));
        assert!(!is_network_error(403, "forbidden"));
        // 反向验证：非 -1 码即便 message 撞上网络 hint 也不算网络错，
        // 否则 5xx 页里带 "connection" 字样就会被误判成可自愈。
        assert!(!is_network_error(500, "connection reset by peer"));
    }

    #[test]
    fn shuffle_by_seed_changes_order_and_is_reproducible() {
        let base = ["a", "b", "c", "d", "e", "f", "g"];
        // 反向验证：若洗牌退化成恒等（等于没防护），changed 会是 0 —— 这里必须 FAIL。
        let mut changed = 0;
        for seed in 0..20u64 {
            let mut v = base.to_vec();
            shuffle_by_seed(&mut v, seed);
            if v != base.to_vec() {
                changed += 1;
            }
        }
        assert!(
            changed >= 15,
            "洗牌必须真打散顺序，恒等即等于没防护（实测仅 {changed}/20 变序）"
        );

        // 同种子可复现（排障时要能重放同一轮顺序）
        let mut x = base.to_vec();
        let mut y = base.to_vec();
        shuffle_by_seed(&mut x, 7);
        shuffle_by_seed(&mut y, 7);
        assert_eq!(x, y);

        // 不得丢元素，也不得对不足 2 个元素的集合 panic
        let mut z = base.to_vec();
        shuffle_by_seed(&mut z, 1);
        let mut got = z.clone();
        got.sort_unstable();
        let mut want = base.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
        let mut one = ["a"];
        shuffle_by_seed(&mut one, 1);
        assert_eq!(one, ["a"]);
        let mut none: [&str; 0] = [];
        shuffle_by_seed(&mut none, 1);
    }

    #[test]
    fn route_missing_is_404_only() {
        assert!(is_route_missing(
            &json!({"code": 404, "message": "not found"})
        ));
        assert!(is_route_missing(&json!({"code": "404"})));
        assert!(is_route_missing(&json!({"data": {"code": 404}})));
        // 非 404 一律不得当作路径问题回落。
        assert!(!is_route_missing(
            &json!({"code": 401, "message": "unauthorized"})
        ));
        assert!(!is_route_missing(&json!({"code": 403})));
        assert!(!is_route_missing(
            &json!({"code": 10085, "msg": "请求不合法"})
        ));
        assert!(!is_route_missing(
            &json!({"code": -1, "message": "error sending request"})
        ));
        assert!(!is_route_missing(&json!({"code": 0, "data": {}})));
        assert!(!is_route_missing(&Value::Null));
    }

    /// 限额监听配置：默认开启、显式 false 生效、损坏/缺字段回默认，且只写已知字段。
    #[test]
    fn rate_limit_config_defaults_to_enabled_and_keeps_only_known_fields() {
        let dir = std::env::temp_dir().join(format!(
            "wb-switch-rate-limit-config-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rate_limit_config.json");

        // 文件缺失 → 默认开启、未卸载过、IDE 日志扫描开启。
        let defaults = load_rate_limit_config_at(&path);
        assert_eq!(defaults.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            defaults.get("hookOptOut").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            defaults.get("scanIdeLogs").and_then(Value::as_bool),
            Some(true)
        );

        // 显式关闭 → 生效。
        save_rate_limit_config_at(
            &path,
            &json!({"enabled": false, "scanIdeLogs": false, "extra": 1}),
        )
        .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(
            saved.get("scanIdeLogs").and_then(Value::as_bool),
            Some(false),
            "scanIdeLogs 显式 false 必须落盘"
        );
        assert!(saved.get("extra").is_none(), "只保留已知字段: {saved}");
        assert_eq!(
            saved.as_object().unwrap().len(),
            3,
            "只有 enabled + hookOptOut + scanIdeLogs"
        );
        assert_eq!(
            load_rate_limit_config_at(&path)
                .get("scanIdeLogs")
                .and_then(Value::as_bool),
            Some(false),
            "读回仍是显式 false（不被默认值冲掉）"
        );

        // 显式 true 与显式 false 都如实往返（默认值不覆盖显式值）。
        save_rate_limit_config_at(&path, &json!({"scanIdeLogs": true})).unwrap();
        let round_trip = load_rate_limit_config_at(&path);
        assert_eq!(
            round_trip.get("scanIdeLogs").and_then(Value::as_bool),
            Some(true)
        );

        // 损坏内容 / 类型不符 → 回默认，不报错。
        std::fs::write(&path, "not-json").unwrap();
        assert_eq!(
            load_rate_limit_config_at(&path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            load_rate_limit_config_at(&path)
                .get("scanIdeLogs")
                .and_then(Value::as_bool),
            Some(true)
        );
        std::fs::write(&path, json!({"enabled": "no"}).to_string()).unwrap();
        assert_eq!(
            load_rate_limit_config_at(&path)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(rate_limit_config_file().ends_with("rate_limit_config.json"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `hookOptOut` 与 `enabled` 并列：只保留已知字段，且单字段改写不动另一个开关。
    #[test]
    fn rate_limit_hook_opt_out_survives_known_field_merge() {
        let dir = std::env::temp_dir().join(format!(
            "wb-switch-rate-limit-opt-out-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rate_limit_config.json");

        // 只保留已知字段：多余键不落盘。
        save_rate_limit_config_at(
            &path,
            &json!({"enabled": false, "hookOptOut": true, "scanIdeLogs": false, "unknown": "x"}),
        )
        .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.get("hookOptOut").and_then(Value::as_bool), Some(true));
        assert_eq!(saved.get("enabled").and_then(Value::as_bool), Some(false));
        assert!(saved.get("unknown").is_none(), "只保留已知字段: {saved}");

        // 单字段改写：置 true / 置 false 都不动 `enabled` 与 `scanIdeLogs`。
        let after_opt_out = set_rate_limit_hook_opt_out_at(&path, true).unwrap();
        assert_eq!(
            after_opt_out.get("hookOptOut").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            after_opt_out.get("enabled").and_then(Value::as_bool),
            Some(false),
            "改写 hookOptOut 不得重置限额监听开关"
        );
        assert_eq!(
            after_opt_out.get("scanIdeLogs").and_then(Value::as_bool),
            Some(false),
            "改写 hookOptOut 不得重置 IDE 日志扫描开关"
        );
        let cleared = set_rate_limit_hook_opt_out_at(&path, false).unwrap();
        assert_eq!(
            cleared.get("hookOptOut").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(cleared.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(
            cleared.get("scanIdeLogs").and_then(Value::as_bool),
            Some(false)
        );
        // 配置缺失时也能写入（首次卸载 / 首次接入）。
        let fresh = dir.join("fresh.json");
        assert_eq!(
            set_rate_limit_hook_opt_out_at(&fresh, true)
                .unwrap()
                .get("hookOptOut")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            load_rate_limit_config_at(&fresh)
                .get("enabled")
                .and_then(Value::as_bool),
            Some(true)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 轮换推迟提示预算：同日第 1..5 次放行、第 6 次拒绝；跨日重置；损坏回退 0。
    #[test]
    fn rotate_notify_budget_caps_per_local_day_and_resets_on_a_new_day() {
        let dir =
            std::env::temp_dir().join(format!("wb-switch-rotate-notify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ROTATE_NOTIFY_FILE_NAME);
        let today = "2026-09-18";

        // 文件不存在 → 从 0 开始，前 5 次都放行。
        for expected_count in 1..=ROTATE_NOTIFY_DAILY_LIMIT {
            assert!(
                try_consume_rotate_notify_at(&path, today),
                "第 {expected_count} 次应放行"
            );
            let saved: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved["count"], json!(expected_count));
            assert_eq!(saved["date"], json!(today));
        }
        // 第 6 次：拒绝，且预算文件不再被改写（仍停在 5）。
        assert!(!try_consume_rotate_notify_at(&path, today));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["count"], json!(ROTATE_NOTIFY_DAILY_LIMIT));

        // 跨日：日期变化即清零，重新放行。
        assert!(try_consume_rotate_notify_at(&path, "2026-09-19"));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["date"], json!("2026-09-19"));
        assert_eq!(saved["count"], json!(1));

        // 损坏 / 字段缺失 / 类型不符 → 按 0 计（不阻塞轮换）。
        for broken in [
            "not-json",
            "{}",
            r#"{"date":"2026-09-20","count":"many"}"#,
            "[]",
        ] {
            std::fs::write(&path, broken).unwrap();
            assert!(
                try_consume_rotate_notify_at(&path, today),
                "损坏内容应按 0 计: {broken}"
            );
            let saved: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved["count"], json!(1), "损坏后从 1 重新起算: {broken}");
            assert_eq!(saved["date"], json!(today));
        }

        // 空日期视为不可用（不写坏文件）。
        std::fs::remove_file(&path).unwrap();
        assert!(!try_consume_rotate_notify_at(&path, ""));
        assert!(!path.exists());
        assert!(auto_rotate_notify_file().ends_with(ROTATE_NOTIFY_FILE_NAME));
        std::fs::remove_dir_all(&dir).ok();
    }
}
