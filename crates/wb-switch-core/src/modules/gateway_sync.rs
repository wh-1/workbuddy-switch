//! 网关凭证跟随同步（v3.2 跟随模式，设计稿 `reports/gateway-admin-ui-design-2026-09-21.md` §12.1/§12.2）。
//!
//! 语义：2api（workbuddy2api）网关与 switch **共用账号**——switch 当前是哪个号，
//! 网关池里就只有哪个号。switch 侧在「切号 / 登录 / 凭证刷新」三条路径末尾调用
//! [`sync_gateway_credentials_for`]，把当前账号凭证映射成 2api auths 嵌套形覆盖写
//! `workbuddy-current.json`，并清掉池内其他 `workbuddy-*.json`（跟随模式 = 单号池）。
//!
//! 可信跟随三件套（§12.2 P0）：
//! 1. **原子写**：temp + rename（复用 `config::atomic_write`），2api watcher 5s 轮询
//!    读不到写一半的 JSON，不会把号误踢出池。
//! 2. **失败补偿**：写入失败记 pending（内存），下次任何触发路径自动重试；
//!    `gateway_sync_status()` 把失败暴露给前端标红 + 一键重同步。
//! 3. **一致性校验**：status 对比本地当前 uid 与 auths 文件里的 uid，不一致由前端标红。
//! 4. **幂等防抖**：凭证指纹（uid+AT+RT 哈希）未变化直接跳过——后台高频刷新不会
//!    重复写盘。不另做 60s 时间窗：写入本身是亚毫秒本地操作，指纹幂等已消除高频写盘。
//!
//! 刷新权独占 switch：2api 侧配套 `refresh_on_request=false` 关闭请求路径预刷新；
//! 本模块每次写入都携带最新凭证，即使网关侧曾抢刷也天然纠漂。
//!
//! 字段映射（§12 实测核实）：switch 条目 `access_token / refresh_token / expiresAt(毫秒) /
//! domain / uid / nickname / enterpriseId` ⇄ 2api auths
//! `{auth:{accessToken, refreshToken, expiresAt(**Unix 秒**), domain, realm},
//! account:{uid, nickname, enterpriseId}}`。realm 按 domain 后缀推断：
//! `.workbuddy.ai` → "global"，否则 "cn"（与 2api `Auth::Realm()` 回落规则一致）。

use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::modules::account;
use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::variant::WbVariant;

/// 覆盖式单文件名：2api 池里的「当前号」凭证。
pub const CURRENT_FILE_NAME: &str = "workbuddy-current.json";

/// 网关同步配置文件（enabled + authsDir）。
const CONFIG_FILE_NAME: &str = "gateway_config.json";

/// pending 重试的内存态：上次写入失败的 uid（下次触发路径重试）。
static PENDING_UID: Mutex<Option<String>> = Mutex::new(None);

/// 上次成功同步的指纹 + 时刻 + 结果（幂等防抖 + status 透出）。
static LAST_SYNC: Mutex<Option<LastSync>> = Mutex::new(None);

#[derive(Clone)]
struct LastSync {
    fingerprint: u64,
    at_ms: i64,
    reason: String,
}

/// 读取网关同步配置；缺失/损坏返回默认（关闭 + 空 authsDir）。
pub fn load_gateway_config() -> Value {
    load_gateway_config_at(&config_path())
}

fn load_gateway_config_at(path: &Path) -> Value {
    let default = json!({ "enabled": false, "authsDir": "" });
    let Ok(text) = std::fs::read_to_string(path) else {
        return default;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return default;
    };
    // 只保留已知字段，类型归一。
    json!({
        "enabled": value.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        "authsDir": value
            .get("authsDir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string(),
    })
}

/// 保存网关同步配置（只保留已知字段）。
pub fn save_gateway_config(cfg: &Value) -> Result<Value, String> {
    let merged = load_gateway_config_at(&config_path());
    let enabled = cfg.get("enabled").and_then(Value::as_bool);
    let auths_dir = cfg
        .get("authsDir")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_string);
    let merged = json!({
        "enabled": enabled.unwrap_or_else(|| merged["enabled"].as_bool().unwrap_or(false)),
        "authsDir": auths_dir.unwrap_or_else(|| merged["authsDir"].as_str().unwrap_or("").to_string()),
    });
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(&path, &serde_json::to_string_pretty(&merged).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok(merged)
}

fn config_path() -> PathBuf {
    store_dir().join(CONFIG_FILE_NAME)
}

// ---------------------------------------------------------------------------
// 同步入口
// ---------------------------------------------------------------------------

/// 幂等同步当前账号凭证到网关 auths 目录。
///
/// 三条路径共用（切号 / 登录 / 凭证刷新），推荐显式传入刚确定的 uid
/// （切号 = 目标账号 uid）；传 `None` 时自动判定当前登录态 uid。
/// 返回结果 JSON：`{ok, action, uid, path, error?, reason}`，
/// action ∈ `synced / skipped / failed / disabled`。
pub fn sync_gateway_credentials_for(preferred_uid: Option<&str>, reason: &str) -> Value {
    let cfg = load_gateway_config();
    let dir = cfg["authsDir"].as_str().unwrap_or("").to_string();
    if cfg["enabled"].as_bool() != Some(true) || dir.is_empty() {
        return json!({
            "ok": false,
            "action": "disabled",
            "reason": reason,
        });
    }
    sync_at(Path::new(&dir), preferred_uid, reason)
}

/// 同步实现（authsDir 已解析；注入目录供单测使用）。
fn sync_at(dir: &Path, preferred_uid: Option<&str>, reason: &str) -> Value {
    // pending 重试优先：上次失败的号若仍是当前号，这次触发就是补偿机会。
    let pending = PENDING_UID.lock().unwrap().take();

    let Some(acc) = current_account_entry(preferred_uid) else {
        // 没有可同步的当前账号：恢复 pending（这次不消费它）。
        restore_pending(pending);
        return json!({ "ok": false, "action": "skipped", "reason": reason, "error": "未找到当前账号" });
    };
    let uid = account::get_str(&acc, "uid").unwrap_or_default();

    let Some(doc) = build_auth_doc(&acc) else {
        restore_pending(pending);
        return json!({
            "ok": false, "action": "skipped", "reason": reason, "uid": uid,
            "error": "账号凭证不完整（缺 access/refresh token），跳过同步",
        });
    };

    // 幂等防抖：指纹未变且上次成功 → 跳过（pending 重试豁免）。
    let fp = fingerprint_of(&uid, &doc);
    let retrying_same = pending.as_deref() == Some(uid.as_str());
    if !retrying_same {
        if let Some(last) = LAST_SYNC.lock().unwrap().as_ref() {
            if last.fingerprint == fp {
                return json!({
                    "ok": true, "action": "skipped", "reason": reason, "uid": uid,
                    "skipped": "credentials-unchanged",
                });
            }
        }
    }

    let target = dir.join(CURRENT_FILE_NAME);
    let write = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let content = serde_json::to_string_pretty(&doc).unwrap_or_default();
        atomic_write(&target, &content)?;
        prune_other_auth_files(dir, &uid);
        Ok(())
    })();
    match write {
        Ok(()) => {
            *LAST_SYNC.lock().unwrap() = Some(LastSync {
                fingerprint: fp,
                at_ms: now_ms(),
                reason: reason.to_string(),
            });
            json!({ "ok": true, "action": "synced", "reason": reason, "uid": uid,
                    "path": target.to_string_lossy() })
        }
        Err(error) => {
            // 失败补偿：记 pending，下次触发路径重试（网关目录暂不可写等场景自愈）。
            *PENDING_UID.lock().unwrap() = Some(uid.clone());
            json!({
                "ok": false, "action": "failed", "reason": reason, "uid": uid,
                "error": format!("写入网关 auths 失败: {error}"),
            })
        }
    }
}

fn restore_pending(pending: Option<String>) {
    if let Some(uid) = pending {
        *PENDING_UID.lock().unwrap() = Some(uid);
    }
}

/// 判定当前账号条目：优先用调用方传入的 uid；否则按「跟随源」判定——
/// **CLI 当前账号优先**（网关是 API 服务，CLI 是其调用身份；Windows 读 settings.json
/// env token 匹配账号库，复用 `codebuddy_cli::status` 的判定），CLI 未配置/匹配不到
/// 时回落 WorkBuddy App 登录态（两档位登录态文件）。
fn current_account_entry(preferred_uid: Option<&str>) -> Option<Value> {
    let accounts = account::load_accounts();
    let uid_of = |v: &Value| account::get_str(v, "uid").unwrap_or_default();
    if let Some(uid) = preferred_uid.filter(|s| !s.is_empty()) {
        return accounts.into_iter().find(|a| uid_of(a) == uid);
    }
    // 跟随源第一优先：CodeBuddy CLI 当前账号（activeAccountId）。
    let cli = crate::modules::codebuddy_cli::status();
    if let Some(id) = cli.get("activeAccountId").and_then(Value::as_str) {
        if let Some(acc) = accounts.iter().find(|a| account::get_str(a, "id").as_deref() == Some(id)) {
            return Some(acc.clone());
        }
    }
    // 回落：WorkBuddy App 登录态（同一时刻只有一个档位生效）。
    for variant in [WbVariant::Cn, WbVariant::Ai] {
        let Some(uid) = crate::modules::session::current_user_uid(variant) else {
            continue;
        };
        if let Some(acc) = accounts.iter().find(|a| uid_of(a) == uid) {
            return Some(acc.clone());
        }
    }
    None
}

/// switch 账号条目 → 2api auths 嵌套形凭证文档；凭证不完整返回 None。
fn build_auth_doc(acc: &Value) -> Option<Value> {
    let access_token = account::get_str(acc, "access_token")?;
    let refresh_token = account::get_str(acc, "refresh_token")?;
    let uid = account::get_str(acc, "uid")?;
    if access_token.is_empty() || refresh_token.is_empty() || uid.is_empty() {
        return None;
    }
    // switch 存毫秒（now_ms() 口径），2api `Auth.ExpiresAt` 是 Unix 秒；
    // 秒级时间戳 < 1e12，毫秒 ≥ 1e12，据此归一（norm_ts 同款判据）。
    let expires_at_ms = acc.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
    let expires_at_secs = if expires_at_ms >= 10_000_000_000 {
        expires_at_ms / 1000
    } else {
        expires_at_ms
    };
    let domain = account::get_str(acc, "domain").unwrap_or_default();
    let realm = if domain.ends_with("workbuddy.ai") {
        "global"
    } else {
        "cn"
    };
    // nickname/enterpriseId 在 WorkBuddy 5.6+ 可能是加密信封对象：get_str 折叠为 None，
    // 网关侧只作展示，写空串即可（对照 2api SaveAtomic 的形状）。
    Some(json!({
        "auth": {
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "expiresAt": expires_at_secs,
            "domain": domain,
            "realm": realm,
        },
        "account": {
            "uid": uid,
            "nickname": account::get_str(acc, "nickname").unwrap_or_default(),
            "enterpriseId": account::get_str(acc, "enterpriseId").unwrap_or_default(),
        },
    }))
}

/// 清掉池内其他 `workbuddy-*.json`（跟随模式 = 单号池）；保留 current 文件与 tmp 残留。
fn prune_other_auth_files(dir: &Path, keep_uid: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == CURRENT_FILE_NAME || !name.starts_with("workbuddy-") || !name.ends_with(".json") {
            continue;
        }
        // 按文件内容 uid 保险：uid 相同的旧文件名也保留（文件名是 workbuddy-<uuid>.json）。
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                let file_uid = value["account"]["uid"].as_str().unwrap_or("");
                if file_uid == keep_uid {
                    continue;
                }
            }
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

fn fingerprint_of(uid: &str, doc: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    uid.hash(&mut hasher);
    doc["auth"]["accessToken"].as_str().hash(&mut hasher);
    doc["auth"]["refreshToken"].as_str().hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// 状态查询（前端一致性校验 + 一键重同步）
// ---------------------------------------------------------------------------

/// 网关跟随同步状态：配置 + 本地当前 uid + auths 文件里的 uid + 上次同步结果。
///
/// `uidMatch = false` 时前端标红并展示「重新同步」。
pub fn gateway_sync_status() -> Value {
    let cfg = load_gateway_config();
    let dir = cfg["authsDir"].as_str().unwrap_or("").to_string();
    let local = current_account_entry(None);
    let local_uid = local.as_ref().and_then(|a| account::get_str(a, "uid"));
    // 展示名与账号管理页/CLI 同源：email → nickname → uid（account_display_name）。
    let local_name = local
        .as_ref()
        .map(account::account_display_name)
        .filter(|s| s != "unknown");
    let (gateway_uid, gateway_name) = if dir.is_empty() {
        (None, None)
    } else {
        read_current_file_identity(Path::new(&dir))
    };
    let pending = PENDING_UID.lock().unwrap().clone();
    let (last_at, last_reason) = LAST_SYNC
        .lock()
        .unwrap()
        .as_ref()
        .map(|l| (Some(l.at_ms), Some(l.reason.clone())))
        .unwrap_or((None, None));
    let uid_match = match (&local_uid, &gateway_uid) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    json!({
        "enabled": cfg["enabled"],
        "authsDir": cfg["authsDir"],
        "localUid": local_uid,
        "localName": local_name,
        "gatewayUid": gateway_uid,
        "gatewayName": gateway_name,
        "uidMatch": uid_match,
        "pendingUid": pending,
        "lastSyncAt": last_at,
        "lastSyncReason": last_reason,
        "currentFile": if dir.is_empty() { Value::Null } else { json!(Path::new(&dir).join(CURRENT_FILE_NAME).to_string_lossy()) },
    })
}

/// 强制重同步（前端「重新同步」按钮；绕过指纹幂等，仍写当前账号）。
pub fn gateway_resync() -> Value {
    *PENDING_UID.lock().unwrap() = None;
    let mut result = sync_gateway_credentials_for(None, "manual-resync");
    if result["action"] == "skipped"
        && result["skipped"].as_str() == Some("credentials-unchanged")
    {
        // 指纹没变但用户要求重写：直接落一次盘（比如网关侧文件被手删过）。
        let cfg = load_gateway_config();
        let dir = cfg["authsDir"].as_str().unwrap_or("");
        if cfg["enabled"].as_bool() == Some(true) && !dir.is_empty() {
            if let Some(acc) = current_account_entry(None) {
                if let Some(doc) = build_auth_doc(&acc) {
                    let target = Path::new(dir).join(CURRENT_FILE_NAME);
                    match std::fs::create_dir_all(dir)
                        .and_then(|()| {
                            atomic_write(&target, &serde_json::to_string_pretty(&doc).unwrap_or_default())
                        }) {
                        Ok(()) => {
                            result = json!({ "ok": true, "action": "synced", "reason": "manual-resync-force",
                                             "uid": doc["account"]["uid"], "path": target.to_string_lossy() });
                        }
                        Err(error) => {
                            result = json!({ "ok": false, "action": "failed", "reason": "manual-resync-force",
                                             "error": format!("写入网关 auths 失败: {error}") });
                        }
                    }
                }
            }
        }
    }
    result
}

fn read_current_file_identity(dir: &Path) -> (Option<String>, Option<String>) {
    let text = match std::fs::read_to_string(dir.join(CURRENT_FILE_NAME)) {
        Ok(t) => t,
        Err(_) => return (None, None),
    };
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let uid = account::get_str(&value["account"], "uid");
    let name = account::get_str(&value["account"], "nickname")
        .filter(|s| s != "unknown");
    (uid, name)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auth_doc_converts_ms_to_secs_and_infers_realm() {
        let acc = json!({
            "access_token": "at", "refresh_token": "rt", "uid": "u1",
            "expiresAt": 1794802841724_i64, "domain": "www.workbuddy.cn",
            "nickname": "H", "enterpriseId": null,
        });
        let doc = build_auth_doc(&acc).expect("doc");
        assert_eq!(doc["auth"]["accessToken"], "at");
        assert_eq!(doc["auth"]["expiresAt"], 1794802841_i64, "毫秒 → 秒");
        assert_eq!(doc["auth"]["realm"], "cn");
        assert_eq!(doc["account"]["uid"], "u1");
        assert_eq!(doc["account"]["nickname"], "H");
        assert_eq!(doc["account"]["enterpriseId"], "");
    }

    #[test]
    fn auth_doc_marks_ai_domain_global() {
        let acc = json!({
            "access_token": "at", "refresh_token": "rt", "uid": "u2",
            "expiresAt": 1794802841_i64, "domain": "api.workbuddy.ai",
        });
        let doc = build_auth_doc(&acc).expect("doc");
        assert_eq!(doc["auth"]["realm"], "global");
        // 秒级输入保持不变（2api 侧若已有秒级条目）。
        assert_eq!(doc["auth"]["expiresAt"], 1794802841_i64);
    }

    #[test]
    fn auth_doc_rejects_incomplete_credentials() {
        let acc = json!({ "access_token": "at", "uid": "u3" });
        assert!(build_auth_doc(&acc).is_none());
        let acc = json!({ "access_token": "", "refresh_token": "", "uid": "" });
        assert!(build_auth_doc(&acc).is_none());
    }

    #[test]
    fn config_roundtrip_keeps_known_fields_only() {
        let tmp = std::env::temp_dir().join(format!("gwcfg-{}.json", uuid::Uuid::new_v4().simple()));
        let saved = save_gateway_config_at(
            &tmp,
            json!({ "enabled": true, "authsDir": "D:/x/auths", "junk": 1 }),
        )
        .expect("save");
        assert_eq!(saved["enabled"], true);
        assert_eq!(saved["authsDir"], "D:/x/auths");
        assert!(saved.get("junk").is_none(), "未知字段不入库");
        let loaded = load_gateway_config_at(&tmp);
        assert_eq!(loaded["enabled"], true);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn sync_at_smoke_runs_without_panic() {
        // 全流程冒烟：真实登录态/账号库不可注入，只断言不 panic 且 action 合法
        // （本机有登录态时 = synced；CI 无登录态 = skipped）。端到端反向验证
        // 由 scripts/analysis/verify_gateway_sync.py 承担（探针纪律）。
        let tmp = std::env::temp_dir().join(format!("gwsmoke-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp).unwrap();
        let result = sync_at(&tmp, None, "test");
        assert!(matches!(
            result["action"].as_str(),
            Some("synced") | Some("skipped")
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_keeps_current_and_same_uid_files() {
        let tmp = std::env::temp_dir().join(format!("gwprune-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp).unwrap();
        let doc = json!({ "account": { "uid": "keep-1" } });
        std::fs::write(tmp.join(CURRENT_FILE_NAME), serde_json::to_string(&doc).unwrap()).unwrap();
        std::fs::write(
            tmp.join("workbuddy-keep-1.json"),
            serde_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        std::fs::write(
            tmp.join("workbuddy-drop-2.json"),
            json!({ "account": { "uid": "drop-2" } }).to_string(),
        )
        .unwrap();
        std::fs::write(tmp.join("unrelated.json"), "{}").unwrap();
        prune_other_auth_files(&tmp, "keep-1");
        assert!(tmp.join(CURRENT_FILE_NAME).exists());
        assert!(tmp.join("workbuddy-keep-1.json").exists(), "同 uid 旧文件名保留");
        assert!(!tmp.join("workbuddy-drop-2.json").exists(), "异 uid 清除");
        assert!(tmp.join("unrelated.json").exists(), "非 workbuddy- 前缀不动");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

/// 保存配置到指定路径（测试注入用；生产走 save_gateway_config 落真实配置文件）。
#[cfg(test)]
fn save_gateway_config_at(path: &Path, cfg: Value) -> Result<Value, String> {
    let enabled = cfg.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let auths_dir = cfg
        .get("authsDir")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let merged = json!({ "enabled": enabled, "authsDir": auths_dir });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(path, &serde_json::to_string_pretty(&merged).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok(merged)
}
