//! Token 刷新与保活。
//!
//! 对照 server.py `refresh_account_token` / `ensure_fresh_token` /
//! `run_keepalive_cycle`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use crate::modules::account::{build_auth_headers, upsert_account};
use crate::modules::config::{
    batch_gap, http_request, is_network_error, load_checkin_config, norm_ts, now_ms,
    shuffle_by_seed, RunFlagGuard,
};

static KEEPALIVE_RUNNING: AtomicBool = AtomicBool::new(false);

/// 刷新接口 URL：域名与路径前缀都按账号自身档位选（缺档位字段 → 国内版，零回归）。
fn refresh_url(account: &Value) -> String {
    let variant = crate::modules::account::variant_of(account);
    format!(
        "{}{}/auth/token/refresh",
        variant.api_endpoint(),
        variant.api_prefix()
    )
}

/// 构造刷新请求头：鉴权头 + 刷新会话头。
///
/// `X-Auth-Refresh-Source: plugin` 必须保留：两个档位的官方客户端刷新时都发送该头，
/// 缺失时网关会把这次刷新判定成另一个 client 来源，国际版实测返回
/// `invalid_grant: Invalid refresh token`（code 12153）。抽成纯函数是为了让单测
/// 能把这个头固定住，避免将来被顺手删掉。
fn refresh_headers(account: &Value, refresh_token: &str) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("X-Refresh-Token".to_string(), refresh_token.to_string());
    headers.insert("X-Auth-Refresh-Source".to_string(), "plugin".to_string());
    headers
}

/// 刷新单账号 token（POST /v2/plugin/auth/token/refresh），成功则落盘并返回新账号。
///
/// 刷新失败（refresh token 失效等）时给账号标记 needs_relogin，避免无限重试。
/// 网络层失败例外：只写 `last_refresh_network_error`，不动 needs_relogin。
pub async fn refresh_account_token(mut account: Value) -> Value {
    let previous_access_token = account
        .get("access_token")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let rt = account
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if rt.is_empty() {
        account["needs_relogin"] = json!(true);
        account["needs_relogin_reason"] = json!("缺少 refresh token，无法刷新，需重新登录");
        let _ = upsert_account(&account);
        return account;
    }

    let headers = refresh_headers(&account, &rt);
    // 刷新必须走账号自身档位的域名：把国际版 token 打到国内网关等于登录失效。
    let url = refresh_url(&account);
    let resp = http_request(&url, "POST", Some(json!({})), Some(&headers)).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        let message = resp
            .get("message")
            .or_else(|| resp.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误")
            .to_string();
        // 网络层失败 ≠ 登录失效：留痕后原样返回，等下次网络恢复自愈。
        if is_network_error(code, &message) {
            account["last_refresh_network_error"] = json!({"at": now_ms(), "message": message});
            let _ = upsert_account(&account);
            return account;
        }
        account["needs_relogin"] = json!(true);
        account["needs_relogin_reason"] = json!(format!("刷新失败(code={code}): {message}"));
        let _ = upsert_account(&account);
        return account;
    }

    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let new_at = data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("access_token").and_then(|v| v.as_str()))
        .map(|s| s.to_string());
    let Some(new_at) = new_at else {
        account["needs_relogin"] = json!(true);
        account["needs_relogin_reason"] = json!("刷新响应缺少 accessToken");
        let _ = upsert_account(&account);
        return account;
    };

    account["access_token"] = json!(new_at);
    if let Some(new_rt) = data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("refresh_token").and_then(|v| v.as_str()))
    {
        account["refresh_token"] = json!(new_rt);
    }
    // 官方接口只返回相对 expiresIn（秒），需换算为绝对时间戳
    let new_exp = norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")));
    let new_exp = match new_exp {
        Some(v) => Some(v),
        None => data
            .get("expiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };
    if let Some(v) = new_exp {
        account["expiresAt"] = json!(v);
    }
    let fallback_rt_exp = norm_ts(
        account
            .get("auth_raw")
            .and_then(|a| a.get("refreshExpiresAt")),
    );
    let mut new_rt_exp = norm_ts(
        data.get("refreshExpiresAt")
            .or_else(|| data.get("refresh_expires_at")),
    );
    if new_rt_exp.is_none() {
        new_rt_exp = fallback_rt_exp;
    }
    let new_rt_exp = match new_rt_exp {
        Some(v) => Some(v),
        None => data
            .get("refreshExpiresIn")
            .and_then(|v| v.as_i64())
            .map(|e| now_ms() + e * 1000),
    };
    if let Some(v) = new_rt_exp {
        account["refreshExpiresAt"] = json!(v);
    }
    account["refreshedAt"] = json!(now_ms());
    let map = account.as_object_mut().unwrap();
    map.remove("needs_relogin");
    map.remove("needs_relogin_reason");
    // 刷新成功即证明网络恢复，清掉上一次的网络留痕。
    map.remove("last_refresh_network_error");
    let _ = upsert_account(&account);
    // Windows 不执行 apiKeyHelper；当前 CLI 账号刷新后同步 settings env。
    // 同步失败不阻断 WorkBuddy 保活；状态接口会根据 settings 与账号库是否
    // 一致显示“待同步”，避免把认证配置错误混入账号数据。
    if cfg!(windows) {
        let _ = crate::modules::codebuddy_cli::sync_windows_env_for_account(
            &account,
            previous_access_token.as_deref(),
        );
    }
    // gateway(私有) —— 跟随模式：刷新后同步「跟随源当前号」（CLI 优先，内部判定 +
    // 指纹幂等：被刷账号不是跟随源时写跟随源旧凭证自动跳过；是则写新凭证纠漂）。
    {
        let _ = crate::modules::gateway_sync::sync_gateway_credentials_for(None, "refresh");
    }
    account
}

/// 惰性刷新：expiresAt 缺失或剩余 < lazy_refresh_hours 则刷新。返回最新账号。
pub async fn ensure_fresh_token(mut account: Value, cfg: &Value) -> Value {
    let lazy_h = cfg
        .get("lazy_refresh_hours")
        .and_then(|v| v.as_i64())
        .unwrap_or(24);
    let exp = account.get("expiresAt").and_then(|v| v.as_i64());
    let stale = match exp {
        Some(e) => now_ms() >= e || e - now_ms() < lazy_h * 3600 * 1000,
        None => true,
    };
    let has_rt = !account
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty();
    if stale && has_rt {
        account = refresh_account_token(account).await;
    }
    account
}

/// 保活检查：每天由后台循环调用一次，默认（keepalive_days <= 0）无条件刷新
/// 全部带 refresh token 的账号；keepalive_days > 0 时仅刷新剩余不足该天数的账号。
///
/// 高频保活是为了避免官方服务端清理闲置的 refresh 会话——曾出现闲置数天后
/// 刷新返回 12153 invalid_grant（Session doesn't have required client）导致
/// 账号被迫重新登录。
pub async fn run_keepalive_cycle() -> Value {
    let Some(_guard) = RunFlagGuard::try_acquire(&KEEPALIVE_RUNNING) else {
        return json!({"skipped": "already_running"});
    };
    let cfg = load_checkin_config();
    let keep_days = cfg
        .get("keepalive_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    // 每轮重新洗牌：账号库表序是固定的，若每轮都按同一顺序连发，服务端拿到的是
    // 一串雷同的 uid 先后关系（growth_tasks 的家族任务洗牌同款防护）。
    let mut accounts = crate::modules::account::load_accounts();
    let total = accounts.len();
    shuffle_by_seed(&mut accounts, now_ms() as u64 ^ 0x9E37_79B9_7F4A_7C15);
    let mut results: Vec<Value> = Vec::new();
    // 保活是「一个网络窗口内批量刷全库」：若前几个账号全卡在连不上，剩下的
    // 必失败无疑，继续跑只是把整库账号逐个刷成异常状态。连挂 2 个即熔断。
    let started_ms = now_ms();
    let mut net_fail_streak = 0i32;
    for mut acc in accounts {
        let exp = acc.get("expiresAt").and_then(|v| v.as_i64());
        let stale = keep_days <= 0
            || match exp {
                Some(e) => now_ms() >= e || e - now_ms() < keep_days * 24 * 3600 * 1000,
                None => true,
            };
        if !stale {
            continue;
        }
        if acc
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
        {
            acc["needs_relogin"] = json!(true);
            acc["needs_relogin_reason"] = json!("缺少 refresh token，无法保活，需重新登录");
            let _ = upsert_account(&acc);
            results.push(json!({
                "email": crate::modules::account::account_display_name(&acc),
                "status": "missing_rt",
            }));
            continue;
        }
        let name = crate::modules::account::account_display_name(&acc);
        // 账号间抖动（2~8s）：只在真要发请求前等，判断为不必刷新的账号不产生延迟。
        batch_gap().await;
        let fresh = refresh_account_token(acc).await;
        let failed = fresh.get("needs_relogin").and_then(|v| v.as_bool()) == Some(true);
        // 时间戳 >= 本轮起点才算「本轮刚发生的网络失败」，避免拿历史留痕误熔断。
        let net_err = fresh
            .get("last_refresh_network_error")
            .and_then(|v| v.get("at"))
            .and_then(|v| v.as_i64())
            .map(|at| at >= started_ms)
            .unwrap_or(false);
        if net_err {
            net_fail_streak += 1;
            results.push(json!({
                "email": name,
                "status": "network_error",
                "error": fresh.get("last_refresh_network_error")
                    .and_then(|v| v.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }));
            if net_fail_streak >= 2 {
                return json!({
                    "checked": total,
                    "refreshed": results,
                    "aborted": "network_unreachable",
                    "aborted_after": results.len(),
                });
            }
            continue;
        }
        net_fail_streak = 0;
        results.push(json!({
            "email": name,
            "status": if failed { "failed" } else { "ok" },
            "error": if failed {
                fresh.get("needs_relogin_reason").and_then(|v| v.as_str()).map(|s| s.to_string())
            } else {
                None
            },
        }));
    }
    json!({"checked": total, "refreshed": results})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_url_follows_account_variant() {
        // 旧账号（无档位字段）→ 国内版，与改造前逐字一致。
        assert_eq!(
            refresh_url(&json!({"uid": "u-1"})),
            format!(
                "{}/v2/plugin/auth/token/refresh",
                crate::modules::variant::WbVariant::Cn.api_endpoint()
            )
        );
        assert_eq!(
            refresh_url(&json!({"uid": "u-1", "access_token": "t"})),
            "https://www.codebuddy.cn/v2/plugin/auth/token/refresh"
        );
        // 国际版账号 → 国际版域名。
        assert_eq!(
            refresh_url(&json!({"uid": "u-2", "variant": "ai"})),
            format!(
                "{}/v2/plugin/auth/token/refresh",
                crate::modules::variant::WbVariant::Ai.api_endpoint()
            )
        );
        assert_ne!(
            refresh_url(&json!({"variant": "ai"})),
            refresh_url(&json!({"variant": "cn"}))
        );
    }

    /// AC11：刷新请求头必须带 `X-Auth-Refresh-Source: plugin`，两个档位都覆盖。
    ///
    /// 断言为 `Some("plugin")`：头被删掉时为 `None`，用例立即失败，不会被写成恒真。
    #[test]
    fn refresh_headers_always_declare_plugin_refresh_source() {
        let cases = [
            // 国内版：旧账号（无档位字段）与显式档位各一例。
            json!({"uid": "u-cn", "access_token": "at", "refresh_token": "rt"}),
            json!({"uid": "u-cn", "access_token": "at", "variant": "cn"}),
            // 国际版：D1 的失败现场。
            json!({"uid": "u-ai", "access_token": "at", "variant": "ai"}),
        ];
        for account in cases {
            let headers = refresh_headers(&account, "rt-value");
            assert_eq!(
                headers.get("X-Auth-Refresh-Source").map(String::as_str),
                Some("plugin"),
                "刷新请求头缺少 X-Auth-Refresh-Source: plugin（{account}）"
            );
            // 刷新会话头不能被这次改动挤掉。
            assert_eq!(
                headers.get("X-Refresh-Token").map(String::as_str),
                Some("rt-value")
            );
            // 刷新不带 platform 查询参数/头：官方刷新同样不带（research §2 已推翻旧假设）。
            assert!(!headers.contains_key("platform"));
        }
    }
}
