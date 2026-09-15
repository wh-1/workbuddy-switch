//! 云端会话删除（本地新增，上游零冲突）。
//!
//! 背景（2026-09-15 溯源，详见 `reports/session-cloud-delete-mechanism.md`）：
//! 官方「删除对话连带删云端」的实现是 **App 内 edge-sync 扩展**监听本地删除事件
//! （`wb:conversation:deleted`）后调
//! `POST /console/as/conversations/{sid}/delete`，**用当前登录账号的凭证**。
//! 会话瘦身只做本机软删 ⇒ 云端那份留在原地（手机端仍可见、本机又已无入口）。
//! 本模块把「云端删除」补齐，让瘦身能「本地 + 云端一起瘦」。
//!
//! 三条铁律（违反任一条都会复刻「幽灵会话」故障）：
//!   1. **必须用该会话云端归属账号的 token** —— 服务端按 token 的 uid 校验，
//!      用错账号返回 `403 access denied`（实测），绝不会误删，但也删不掉；
//!   2. **404 有三义**（真已删 / 账号不对 / id 不存在）⇒ 调用方必须先按归属筛过，
//!      再谈 404 —— 不可拿 404 当「账号没问题」的证据；
//!   3. **先删云端成功、再本地软删** —— 顺序反转才可回退。
//!
//! 归属判据：`<configDir>/edge-sync-mapping-v4.db` 的
//! `edge_sync_mapping.msg_channel = convmsg:<uid>`。
//! ⚠️ 该表**会记错**（实测 ~20% 偏差）⇒ 只用来缩小范围，真正的把关是服务端 403/200。

use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::modules::account;
use crate::modules::config::home_dir;

/// 官方网关（与 `ui_theme` 一致；本机 WARP 需 `no_proxy` 直连）。
pub const CLOUD_BASE: &str = "https://www.workbuddy.cn";

/// 云端删除结果。调用方按它决定「是否软删本地」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudDelete {
    /// 云端已删（HTTP 200 或业务 code==0）。
    Deleted,
    /// 云端本来就没有（HTTP 404）—— 归属已校验的前提下视为成功。
    AlreadyGone,
    /// `403 conversation access denied`：**该会话不归这个 token 的账号**
    /// ⇒ 调用方应放弃云端删除（但本地归属不受影响，可照常软删）。
    Forbidden,
    /// 网络/服务错误 ⇒ **不要本地软删**，留待下次重试。
    Failed(String),
}

impl CloudDelete {
    /// 是否可视为「云端这一侧已处理完」。
    pub fn ok(&self) -> bool {
        matches!(self, CloudDelete::Deleted | CloudDelete::AlreadyGone)
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // 本机 WARP 代理会碍事；workbuddy.cn 直连可达
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 从 HTTP 状态 + 响应体判定结果。抽出来便于单测（不需要真发请求）。
pub fn classify(status: u16, body: &str) -> CloudDelete {
    match status {
        200..=299 => {
            // 官方可能 200 带业务错误码；code 缺失或为 0 才认成功
            let code = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_i64()));
            match code {
                None | Some(0) => CloudDelete::Deleted,
                Some(c) => CloudDelete::Failed(format!("http 200 但业务 code={c}")),
            }
        }
        404 => CloudDelete::AlreadyGone,
        403 => CloudDelete::Forbidden,
        s => CloudDelete::Failed(format!("http {s}")),
    }
}

/// 删除一条云端会话。`sid` 既是 session id 也是 conversation id（实测一致，158/158）。
pub fn delete_conversation(token: &str, sid: &str) -> CloudDelete {
    if token.is_empty() || sid.is_empty() {
        return CloudDelete::Failed("缺 token 或 sid".into());
    }
    let url = format!("{CLOUD_BASE}/console/as/conversations/{}/delete", urlencode(sid));
    let resp = http().post(&url).bearer_auth(token).send();
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            classify(status, &body)
        }
        Err(e) => CloudDelete::Failed(format!("网络错误：{e}")),
    }
}

/// 只对 id 里可能出现的字符做最小转义（sid 是 uuid，通常无需变）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `convmsg:<uid>` → `<uid>`（不是该前缀就原样返回）。
pub fn channel_uid(ch: &str) -> String {
    ch.strip_prefix("convmsg:").unwrap_or(ch).to_string()
}

/// 探测当前生效的映射库：**v4 → v3 → v2 → 无名**，取第一个存在的。
///
/// ⚠️ 官方 `extensions/edge-sync/server/index.cjs` 里**硬编码 `edge-sync-mapping-v4.db`**；
/// 本仓早期代码写死 v2，导致「复制会话注册云端归属」静默写进了 App 不读的旧库（HANDOFF 坑位 40）。
/// 这里改为探测，跟随官方换代。
pub fn latest_mapping_db() -> Option<PathBuf> {
    let root = home_dir().join(".workbuddy");
    for name in [
        "edge-sync-mapping-v4.db",
        "edge-sync-mapping-v3.db",
        "edge-sync-mapping-v2.db",
        "edge-sync-mapping.db",
    ] {
        let p = root.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 读映射表的 `session_id → msg_channel` 全量。读不到就返回空表（调用方退化为「只本地瘦身」）。
pub fn mapping_channels() -> HashMap<String, String> {
    let Some(db) = latest_mapping_db() else {
        return HashMap::new();
    };
    mapping_channels_from(&db)
}

/// 供测试注入路径用。
pub fn mapping_channels_from(db: &std::path::Path) -> HashMap<String, String> {
    let Ok(conn) = Connection::open(db) else {
        return HashMap::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT session_id, msg_channel FROM edge_sync_mapping")
    else {
        return HashMap::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.flatten()
        .filter_map(|(sid, ch)| ch.map(|c| (sid, c)))
        .collect()
}

/// 按 uid 从 `accounts.json` 找 `access_token`。
pub fn token_of(uid: &str) -> Option<String> {
    account::load_accounts()
        .into_iter()
        .find(|a| crate::modules::align::account_uid(a) == uid)
        .and_then(|a| account::get_str(&a, "access_token"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_http_status() {
        assert_eq!(classify(200, r#"{"code":0,"msg":"OK"}"#), CloudDelete::Deleted);
        assert_eq!(classify(200, ""), CloudDelete::Deleted);
        assert_eq!(
            classify(404, r#"{"code":14284,"msg":"conversation not found"}"#),
            CloudDelete::AlreadyGone
        );
        assert_eq!(
            classify(403, r#"{"code":14287,"msg":"conversation access denied"}"#),
            CloudDelete::Forbidden
        );
        assert!(matches!(classify(500, "boom"), CloudDelete::Failed(_)));
        // 200 但业务错码 ⇒ 不能当成功
        assert!(matches!(
            classify(200, r#"{"code":1,"msg":"x"}"#),
            CloudDelete::Failed(_)
        ));
    }

    #[test]
    fn ok_covers_deleted_and_gone_only() {
        assert!(CloudDelete::Deleted.ok());
        assert!(CloudDelete::AlreadyGone.ok());
        assert!(!CloudDelete::Forbidden.ok());
        assert!(!CloudDelete::Failed("x".into()).ok());
    }

    #[test]
    fn channel_uid_strips_prefix() {
        assert_eq!(channel_uid("convmsg:uid-a"), "uid-a");
        assert_eq!(channel_uid("uid-a"), "uid-a");
        assert_eq!(channel_uid(""), "");
    }

    #[test]
    fn urlencode_keeps_uuid_and_escapes_others() {
        assert_eq!(
            urlencode("c522dc02-672a-419c-b6d0-0eccec45126a"),
            "c522dc02-672a-419c-b6d0-0eccec45126a"
        );
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn missing_input_never_touches_network() {
        assert!(matches!(delete_conversation("", "x"), CloudDelete::Failed(_)));
        assert!(matches!(delete_conversation("t", ""), CloudDelete::Failed(_)));
    }

    #[test]
    fn mapping_channels_from_reads_table() {
        let db = std::env::temp_dir().join(format!(
            "wb_cloudconv_{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE edge_sync_mapping (session_id TEXT PRIMARY KEY, \
             conversation_id TEXT NOT NULL, msg_channel TEXT NOT NULL DEFAULT '');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edge_sync_mapping VALUES ('s1','s1','convmsg:uid-a')",
            [],
        )
        .unwrap();
        drop(conn);
        let m = mapping_channels_from(&db);
        assert_eq!(m.get("s1").map(|s| s.as_str()), Some("convmsg:uid-a"));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn mapping_channels_from_missing_db_is_empty() {
        let m = mapping_channels_from(std::path::Path::new("C:/__no_such_dir__/x.db"));
        assert!(m.is_empty());
    }
}
