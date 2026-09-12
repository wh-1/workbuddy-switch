//! 本地专属 HTTP 路由（上游无此文件 → 与上游合并零冲突）。
//!
//! 这里只放本项目新增的接口：账号发现 / 补录、数据对齐（自动化归属、全量对齐）。
//! `api.rs` 只保留一行 `.merge(api_local::router())`，避免在上游热点文件里堆代码。
//!
//! 说明：`json_ok` / `json_err` 在本文件内重复实现（各 4 行），刻意不改 `api.rs`
//! 中同名私有函数，以免触碰上游既有行、放大合并冲突面。

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};

use wb_switch_core::modules::{account, align, discover};

fn json_ok(v: Value) -> Response {
    Json(v).into_response()
}

fn json_err(e: String, code: StatusCode) -> Response {
    (code, Json(json!({ "ok": false, "error": e }))).into_response()
}

/// 本地路由表（由 `api::router()` merge）。
pub fn router() -> Router {
    Router::new()
        .route("/api/accounts/discover", get(api_discover_accounts))
        .route("/api/accounts/adopt", post(api_adopt_account))
        .route("/api/automations/align", post(api_align_automations))
        .route("/api/align/data", post(api_align_data))
}

/// GET /api/accounts/discover —— 识别本机曾登录/留有数据的账号（对照在册）。
async fn api_discover_accounts() -> Response {
    json_ok(discover::discover_known_accounts())
}

/// POST /api/accounts/adopt —— 用最新 auth 历史备份补录指定 uid 进账号库。
async fn api_adopt_account(Json(body): Json<Value>) -> Response {
    let uid = body
        .get("uid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if uid.trim().is_empty() {
        return json_err("缺少 uid".to_string(), StatusCode::BAD_REQUEST);
    }
    match discover::adopt_account(&uid) {
        Ok(meta) => json_ok(json!({ "ok": true, "account": meta })),
        Err(error) => json_err(error, StatusCode::BAD_REQUEST),
    }
}

/// POST /api/automations/align —— 自动化归属对齐（不切号）。需先完全退出 WorkBuddy。
async fn api_align_automations(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if account_id.trim().is_empty() {
        return json_err("缺少 accountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let Some(target) = account::find_account(&account_id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    let uid = align::account_uid(&target);
    if uid.is_empty() {
        return json_err("该账号缺少 uid，无法对齐".to_string(), StatusCode::BAD_REQUEST);
    }
    match align::align_automations_owner(&uid) {
        Some(v) => json_ok(v),
        None => json_err("workbuddy.db 不存在".to_string(), StatusCode::BAD_REQUEST),
    }
}

/// POST /api/align/data —— 多账号数据全量对齐（L1/L3/L4/L5），dryRun=true 只预览。
async fn api_align_data(Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if account_id.trim().is_empty() {
        return json_err("缺少 accountId".to_string(), StatusCode::BAD_REQUEST);
    }
    let Some(target) = account::find_account(&account_id) else {
        return json_err("账号不存在".to_string(), StatusCode::BAD_REQUEST);
    };
    let uid = align::account_uid(&target);
    if uid.is_empty() {
        return json_err("该账号缺少 uid，无法对齐".to_string(), StatusCode::BAD_REQUEST);
    }
    let opts = align::AlignOptions {
        align_automations: body
            .get("alignAutomations")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        align_sessions: body
            .get("alignSessions")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        align_files: body
            .get("alignFiles")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        dry_run: body
            .get("dryRun")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    json_ok(align::align_data(&uid, None, &opts))
}
