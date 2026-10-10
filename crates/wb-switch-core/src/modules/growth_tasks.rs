//! 成长任务自动执行：夜猫子（`black_cat`）+ 活动任务（`school`，首期=开学季
//! `school_open_day_2026`，活动下线后自动空转、未来同类活动复用同端点）+ 任务家族
//! （`family`，16 项纯上报/领取类一次性任务）。三段共用一个 cycle，门控各自独立。
//!
//! 接口规格与编排语义移植自 2api `scripts/task_runner.py` + `school_open_day_2026.py`
//! （其头部注释固化了 M1–M15 共 14/14 任务的实测知识；本模块只取主人点名的两类）。
//!
//! ## 从 2api 实测沉淀里照抄的结论（每条都踩过，别简化）
//!
//! 1. **accept 200 ≠ 登记生效** —— 实测存在「HTTP 200 + msg=OK 但 accept_status 仍是
//!    not_accepted」的形态，此时后续上报全部不归账、任务永远点不亮。必须 accept 后回读
//!    `accept_status` 确认，未生效重试一次。
//! 2. **上报必须带 `userId`** —— 缺失时服务端 200 但静默丢弃（与活跃地图同款）。
//! 3. **claim 端点 = `/activity/growth/tasks/{code}/claim`**（路径含 code、无 body）；
//!    旧的 `/v2/activity/growth/tasks/reward/claim` 是 404 错端点。已领任务返回
//!    `already_claimed`，幂等不重复入账。chat 域 400 时回落 web 域
//!    `www.workbuddy.cn` + web 头（2api `web_claim_fallback` 同款）。
//! 4. **夜猫窗口 CST 23:00–08:00** —— `black_cat` 时段敏感，窗口内最多补 1 次
//!    （cap=1，即使 progress 还差 3 条也只发 1 条）；非窗口期 skip 不是失败。
//! 5. **夜猫事件体**：`chat_request_send` + `mode:"night"` + `requestModelId:"glm-5.2"`；
//!    字段刻意保留客户端全形状，上游加严校验时最小版本会被拒（同活跃地图结论）。
//! 6. **开学季活动有时限** —— `GET /portal/activity/school/tasks` 返回 `in_period`；
//!    非进行期全量跳过、正常收尾，不算失败。活动下线后本模块自动空转。
//! 7. **桌面上报必须走 copilot 域** —— `chat_3_times`/`expert_use` 走 codebuddy.cn，
//!    而 `desktop_chat_1_time` 的 6 连事件链**发 codebuddy.cn 域不点亮**，必须
//!    `copilot.tencent.com` + `X-Product: SaaS` + 桌面 UA（2api 实测注明）。
//! 8. **school 事件必须带 `activityId = school_open_day_2026`**，否则服务端不关联任务。
//! 9. **`machineId`/`sessionId` 由 uid+盐 md5 稳定派生**（2api `derive_id`）——
//!    勿每次随机，服务端按指纹聚合行为序列。
//! 10. **`task_student_verify`（学生认证）是人工环节，不碰、不伪造**；未知 `task_code`
//!     保守跳过（服务端下发随时可能变更，按字段寻址）。
//! 11. **抽奖 `draw_uuid` 每轮一次性**（随机 UUID 抽后即弃）；余额 0 时上游返回
//!     HTTP 409 + code=40900（no chance），安全边界，即停不过度抽。
//! 12. **写动作间隔 0.8~2.5s 抖动**（频控口径；恒定间隔本身是行为指纹。claim 会加
//!     抽奖机会、draw 真实产生收益，勿高频）。
//! 13. **防检测纪律（09-25 风险审计）**：① `conversationId` 一律 uuid，⛔ 自造前缀
//!     （`wbs-*` 一条 LIKE 就能捞出全部流量）；② `inputLength` 别用恒定值（按 timestamp
//!     抖动 20~199）；③ 家族任务执行顺序每号洗牌（多账号同表序连发易被聚类关联）；
//!     ④ cycle 内账号间顺序也洗牌（号间固定序同风险）；⑤ 同任务当日 error 连击
//!     ≥3 当日熔断跳过（防对真失败无限重试打洞）。
//!     加固降低特征明显度，但伪造行为 + 多账号同机的本质风险不可消除——量小、低调、别贪。

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Timelike, Utc};
use serde_json::{json, Value};

use super::account::{
    account_display_name, build_auth_headers, load_accounts, variant_of,
};
use super::config::{
    growth_tasks_cache_file, jitter_u64, load_growth_tasks_cache, load_growth_tasks_cache_at,
    load_tasks_config, now_ms, save_growth_tasks_cache, with_growth_tasks_cache_lock,
    RunFlagGuard, TASKS_DEFAULT_SCHOOL_HOUR, WORKBUDDY_API_ENDPOINT,
};
use super::config::http_request;
use super::refresh::refresh_account_token;

/// 复用活动地图的运行守卫与间隔：同一套「启动即跑 + 30 分钟检查」节奏。
pub const TASKS_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 任务域 base：成长任务/上报/开学季主体都挂 codebuddy.cn（switch 活跃地图已实证可用）。
const CHAT_BASE: &str = WORKBUDDY_API_ENDPOINT;
/// claim 回落域：chat 域 400 时走 workbuddy.cn web 端头再试一次（2api M15 实测口径）。
const WEB_CLAIM_BASE: &str = "https://www.workbuddy.cn";
/// 桌面 6 连事件的唯一有效域（发 codebuddy.cn 不点亮，2api 实测）。
const COPILOT_BASE: &str = "https://copilot.tencent.com";
/// 开学季活动域路径。
const SCHOOL_BASE: &str = "/portal/activity/school";
/// 开学季活动 code（事件 `activityId` 字段值，服务端按它关联任务）。
const SCHOOL_ACTIVITY_ID: &str = "school_open_day_2026";

const TASKS_LIST_PATH: &str = "/v2/activity/growth/tasks";
const TASKS_ACCEPT_PATH: &str = "/v2/activity/growth/tasks/accept";
const TASK_CLAIM_PATH: &str = "/activity/growth/tasks";
const REPORT_PATH: &str = "/v2/report";
const EXPERT_LIST_PATH: &str = "/v2/operation-platform/market/expert/list";

/// 小程序指纹 UA（微信注入形态，school 脚本同款字符串）。
const MP_UA: &str = "Mozilla/5.0 (Linux; Android 14; MicroMessenger/8.0.49 WeChat/0.8.0 \
                     MiniProgramEnv/android; wkbrowser xweb)";
/// 桌面指纹 UA（2api `DESKTOP_UA` 同款）。
const DESKTOP_UA: &str = "WorkBuddy/5.5.6 WorkBuddy/5.5.6 CLI/2.137.1";
/// 桌面指纹里的产品版本锚点（2api 实测值，勿随手改）。
const DESKTOP_COMMIT: &str = "5f9692923c93033111c51ad7b003eb80204a9b75";
const DESKTOP_RELEASE_DATE: i64 = 1_789_036_585_355;

/// 开学季专家分类 id（带数字前缀，list API 不做归一化，2api 实测）。
const SCHOOL_EXPERT_CATEGORY: &str = "16-BackToSchool";
/// 专家列表拉取失败时的回落专家（2api `SCHOOL_EXPERT_FALLBACK` 同款）。
const SCHOOL_EXPERT_FALLBACK: &[(&str, &str)] = &[
    ("ex_jB0dyFIQJEWa", "论小舟"),
    ("ex_lQjkerakvIex", "英语学习教练"),
];

static TASKS_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 小程序域请求头（school 域 + mini/expert 上报；2api `_build_headers` 默认带 MP UA）。
fn mp_ua_header() -> Vec<(String, String)> {
    vec![("User-Agent".to_string(), MP_UA.to_string())]
}

// ---------------------------------------------------------------------------
// 时间与账号辅助（与活动地图同口径；本仓惯例各模块自带同款实现）
// ---------------------------------------------------------------------------

fn cst_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("CST +8 恒为合法偏移")
}

/// 当前 CST 瞬时点（夜猫窗口/执行点判定用）。
pub fn cst_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&cst_offset())
}

/// 今日（CST 自然日，`YYYY-MM-DD`）。
pub fn cst_today() -> String {
    cst_now().format("%Y-%m-%d").to_string()
}

/// 未授权判定（业务码 401/403 或文案命中；与活动地图同口径）。
fn is_unauthorized(resp: &Value) -> bool {
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code == 401 || code == 403 {
        return true;
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|key| msg.contains(key))
}

/// 账号缓存键（id 优先，缺失回落展示名；与活动地图同款）。
pub fn account_key(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .unwrap_or_else(|| account_display_name(account))
}

// ---------------------------------------------------------------------------
// 账号筛选与 HTTP
// ---------------------------------------------------------------------------

/// 成长任务与活跃地图同受众：国内版账号（国际版无成长中心）。
pub fn tasks_capable_accounts(accounts: Vec<Value>) -> Vec<Value> {
    accounts
        .into_iter()
        .filter(|account| variant_of(account).supports_activity())
        .collect()
}

/// 与活跃地图同款的成长中心 web 端头。
fn tasks_headers(account: &Value) -> std::collections::HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("x-client-platform".to_string(), "web".to_string());
    headers.insert("origin".to_string(), WORKBUDDY_API_ENDPOINT.to_string());
    headers.insert(
        "referer".to_string(),
        format!("{WORKBUDDY_API_ENDPOINT}/profile/growth-center"),
    );
    headers
}

/// 向指定域发请求；未授权且存在 refresh token 时刷新一次并重试。
async fn domain_request(
    base: &str,
    path: &str,
    method: &str,
    body: Option<Value>,
    extra_headers: Option<Vec<(String, String)>>,
    account: &Value,
) -> Value {
    let extra = extra_headers.unwrap_or_default();
    let mut headers = tasks_headers(account);
    for (key, value) in &extra {
        headers.insert(key.clone(), value.clone());
    }
    let url = format!("{base}{path}");
    let mut resp = http_request(&url, method, body.clone(), Some(&headers)).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        let refreshed = refresh_account_token(account.clone()).await;
        let mut headers = tasks_headers(&refreshed);
        for (key, value) in &extra {
            headers.insert(key.clone(), value.clone());
        }
        resp = http_request(&url, method, body, Some(&headers)).await;
    }
    resp
}

/// 发请求并取业务 `data` 段（业务码非 0 视为错误）。
async fn domain_data(
    base: &str,
    path: &str,
    method: &str,
    body: Option<Value>,
    extra_headers: Option<Vec<(String, String)>>,
    account: &Value,
) -> Result<Value, String> {
    let resp = domain_request(base, path, method, body, extra_headers, account).await;
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let message = resp
            .get("message")
            .or_else(|| resp.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(format!("code={code} {message}"));
    }
    Ok(resp.get("data").cloned().unwrap_or(Value::Null))
}

/// 写动作之间的频控间隔：800~2500ms 抖动（恒定间隔本身是行为指纹，09-26 加固）。
async fn write_gap() {
    let ms = 800 + jitter_u64() % 1700;
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

// ---------------------------------------------------------------------------
// 指纹派生（2api `derive_id`：uid+盐 md5 稳定派生，勿随机）
// ---------------------------------------------------------------------------

/// 由 uid+盐稳定派生设备标识（2api `derive_id` 用 md5；这里用 sha256 截断——
/// 服务端只按指纹聚合行为序列、不校验哈希算法，稳定性要求已满足）。
fn derive_id(uid: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{salt}:{uid}").as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// 事件构造
// ---------------------------------------------------------------------------

/// 把 `extra` 的字段逐个并入 `event`（对象浅合并；Python `dict.update` 语义）。
fn merge_fields(event: &mut Value, extra: &Value) {
    if let (Some(target), Some(source)) = (event.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// 夜猫子事件：`chat_request_send` + `mode:"night"` + glm-5.2（字段保留客户端全形状）。
pub fn night_chat_event(uid: &str, conversation_id: &str, timestamp: i64) -> Value {
    json!({
        "eventCode": "chat_request_send",
        "timestamp": timestamp,
        "reportDelay": 0,
        "mode": "night",
        "conversationId": conversation_id,
        "requestId": conversation_id,
        "inputLength": 20 + timestamp % 180,
        "requestModelId": "glm-5.2",
        "requestModelName": "GLM-5.2",
        "isPlan": false,
        "isAutoExecuteTerminal": false,
        "isAutoModify": false,
        "codebaseEnable": false,
        "maxToken": 0,
        "maxSteps": 0,
        "temperature": 0,
        "maxRetries": 0,
        "mentionContexts": [],
        "knowledgeId": [],
        "knowledgeName": [],
        "codebaseId": "",
        "mentionContextCount": 0,
        "command": "",
        "expertId": "",
        "recommendId": "",
        "skillId": "",
        "skillCount": 0,
        "totalCount": 0,
        "fileUri": "",
        "presentAt": timestamp,
        "traceId": "",
        "rootRequestId": conversation_id,
        "parentConversationId": conversation_id,
        "agentName": "default",
        "agentType": "conversation",
        "userId": uid,
    })
}

/// 小程序指纹段（school 事件的公共形状，2api MP 指纹同款）。
fn mp_fingerprint(uid: &str, nick: &str) -> Value {
    json!({
        "source": "mini_program",
        "ideName": "wx_app_cloud",
        "ideType": "WorkBuddy_MP",
        "extName": "workbuddy-mp",
        "extVersion": "SaaS",
        "machineId": derive_id(uid, "machine"),
        "os": "android",
        "osVersion": "14",
        "arch": "arm64",
        "timezone": "Asia/Shanghai",
        "userId": uid,
        "userNickname": nick,
    })
}

/// 桌面指纹段（2api `desktop_fingerprint` 同款形状与实测锚点值）。
fn desktop_fingerprint(uid: &str, nick: &str, timestamp: i64) -> Value {
    json!({
        "timezone": "Asia/Shanghai",
        "reportDelay": 2000,
        "userId": uid,
        "username": nick,
        "userNickname": nick,
        "product": "SaaS",
        "releaseDate": DESKTOP_RELEASE_DATE,
        "commit": DESKTOP_COMMIT,
        "ideName": "WorkBuddy",
        "ideType": "WorkBuddy",
        "ideVersion": "5.5.6",
        "machineId": derive_id(uid, "machine"),
        "sessionId": derive_id(uid, "session"),
        "extName": "workbuddy-desktop",
        "extVersion": "5.5.6",
        "os": "win32",
        "arch": "x64",
        "osVersion": "10.0.26220",
        "cpuCores": 20,
        "memorySize": 24,
        "timestamp": timestamp,
        "presentAt": timestamp,
    })
}

/// 开学季 mini 对话事件（点亮 `chat_3_times`；必须带 activityId，否则服务端不关联）。
pub fn mini_chat_event(uid: &str, nick: &str, conversation_id: &str, timestamp: i64) -> Value {
    let mut event = json!({
        "eventCode": "chat_request_send",
        "timestamp": timestamp,
        "reportDelay": 0,
        "mode": "chat",
        "conversationId": conversation_id,
        "requestId": conversation_id,
        "inputLength": 20 + timestamp % 180,
        "activityId": SCHOOL_ACTIVITY_ID,
        "mentionContexts": [],
        "mentionContextCount": 0,
        "userId": uid,
    });
    merge_fields(&mut event, &mp_fingerprint(uid, nick));
    event
}

/// 开学季专家事件（点亮 `expert_use`；需 activityId + conversationId）。
pub fn school_expert_event(
    uid: &str,
    nick: &str,
    expert_id: &str,
    expert_name: &str,
    conversation_id: &str,
    timestamp: i64,
) -> Value {
    let mut event = json!({
        "eventCode": "expert_actual_use",
        "timestamp": timestamp,
        "reportDelay": 0,
        "id": expert_id,
        "name": expert_id,
        "expertTitle": expert_name,
        "type": "send_message",
        "characterCount": 12,
        "expertType": "agent",
        "conversationId": conversation_id,
        "activityId": SCHOOL_ACTIVITY_ID,
    });
    merge_fields(&mut event, &mp_fingerprint(uid, nick));
    event
}

/// 桌面成功对话 6 连事件链（点亮 `desktop_chat_1_time`；一次 6 连 = 一次桌面对话）。
///
/// 链序照抄客户端真实形状：agent_task_created → chat_message_send → chat_request_send
/// → chat_message_response → chat_message_status → chat_request_response；
/// 每条 merge 桌面指纹 + activityId。
pub fn desktop_chat_sequence(
    uid: &str,
    nick: &str,
    conversation_id: &str,
    request_id: &str,
    message_id: &str,
    timestamp: i64,
) -> Vec<Value> {
    let fp = desktop_fingerprint(uid, nick, timestamp);
    let mut events = vec![
        // 1) agent_task_created
        json!({
            "eventCode": "agent_task_created",
            "source": "LOCAL",
            "name": "working",
            "task_target": "local",
            "mode": "craft",
            "requestModelId": "fast-model",
            "requestModelName": "fast-model",
            "has_repo": false,
            "repo_type": "none",
            "workspace_type": "empty",
            "has_connector": false,
            "connector_types": [],
            "has_mention": false,
            "mention_types": [],
            "has_template": false,
            "action": "",
            "template_name": "",
            "has_expert": false,
            "expert_id": "",
            "expert_name": "",
            "expert_industry_id": "",
            "has_skill": false,
            "skill_names": [],
            "conversationId": conversation_id,
            "messageId": message_id,
            "buddyId": "",
            "buddyName": "",
        }),
        // 2) chat_message_send
        json!({
            "eventCode": "chat_message_send",
            "messageId": format!("{message_id}-assistant"),
            "historyCount": 0,
            "isContextTruncated": false,
            "currentStepCount": 1,
            "traceId": request_id,
            "rootRequestId": request_id,
            "parentConversationId": conversation_id,
            "agentName": "cli",
            "agentType": "main",
        }),
        // 3) chat_request_send
        json!({
            "eventCode": "chat_request_send",
            "inputLength": 24,
            "isPlan": false,
            "isAutoExecuteTerminal": false,
            "isAutoModify": false,
            "codebaseEnable": false,
            "maxToken": 0,
            "maxSteps": 500,
            "temperature": 0,
            "maxRetries": 0,
            "mentionContexts": [],
            "knowledgeId": [],
            "knowledgeName": [],
            "codebaseId": "",
            "mentionContextCount": 0,
            "command": "",
            "recommendId": "",
            "skillId": "",
            "skillCount": 0,
            "totalCount": 0,
            "traceId": request_id,
            "rootRequestId": request_id,
            "parentConversationId": conversation_id,
            "agentName": "cli",
            "agentType": "main",
            "codebuddy.session_id": conversation_id,
            "codebuddy.conversation_request_id": request_id,
        }),
        // 4) chat_message_response
        json!({
            "eventCode": "chat_message_response",
            "messageId": format!("{message_id}-assistant"),
            "responseModelId": "fast-model",
            "inputToken": 120,
            "outputToken": 80,
            "totalToken": 200,
            "cachedTokens": 0,
            "cachedWriteTokens": 0,
            "cachedMissTokens": 0,
            "isSuccessful": true,
            "messageErrorCode": "",
            "finishReason": "stop",
            "firstTokenAt": timestamp,
            "traceId": request_id,
            "conversationId": conversation_id,
            "rootRequestId": request_id,
            "parentConversationId": conversation_id,
            "agentName": "cli",
            "agentType": "main",
            "codebuddy.session_id": conversation_id,
            "codebuddy.conversation_request_id": request_id,
        }),
        // 5) chat_message_status
        json!({
            "eventCode": "chat_message_status",
            "messageId": format!("{message_id}-assistant"),
            "messageErrorCode": "0",
            "traceId": request_id,
            "rootRequestId": request_id,
            "parentConversationId": conversation_id,
            "agentName": "cli",
            "agentType": "main",
        }),
        // 6) chat_request_response
        json!({
            "eventCode": "chat_request_response",
            "mode": "craft",
            "toolCallCount": 0,
            "inputToken": 120,
            "outputToken": 80,
            "totalToken": 200,
            "cachedTokens": 0,
            "cachedWriteTokens": 0,
            "cachedMissTokens": 0,
            "isSuccessful": true,
            "messageErrorCode": "",
            "finishReason": "stop",
            "rootRequestId": request_id,
            "parentConversationId": conversation_id,
        }),
    ];
    for event in &mut events {
        merge_fields(event, &fp);
        event["activityId"] = json!(SCHOOL_ACTIVITY_ID);
    }
    events
}

// ---------------------------------------------------------------------------
// growth tasks 域（列表 / accept / claim）
// ---------------------------------------------------------------------------

/// 拉成长任务列表，返回 `data.tasks`（元素含 task_code/accept_status/progress 等）。
pub async fn list_growth_tasks(account: &Value) -> Result<Vec<Value>, String> {
    let data = domain_data(CHAT_BASE, TASKS_LIST_PATH, "GET", None, None, account).await?;
    Ok(data
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// mp 口径（`X-Client-Platform: miniprogram`）拉成长任务列表。
/// 小程序限定任务（`Sequential_Tasks_1` 等）**仅在该口径下发**，web 口径列表里
/// 不存在；accept/claim 缺头也返回 task not found（2api M-mp 实测）。
pub async fn list_growth_tasks_mp(account: &Value) -> Result<Vec<Value>, String> {
    let data = domain_data(
        CHAT_BASE,
        TASKS_LIST_PATH,
        "GET",
        None,
        Some(mp_platform_headers()),
        account,
    )
    .await?;
    Ok(data
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// 小程序平台头：mp 限定任务列表 / accept / claim 全链路必需。
fn mp_platform_headers() -> Vec<(String, String)> {
    vec![("x-client-platform".to_string(), "miniprogram".to_string())]
}

/// `Sequential_Tasks_1` 点亮事件：mini 指纹 `chat_request_send`，**无 activityId**
/// （growth 域按 `source=mini_program` 指纹关联，2api 实测 0ceb9c7c 点亮）。
/// 形状对齐 school.mini_chat_event 但去掉 activityId、extVersion=2.4.0。
pub fn mp_chat_event(uid: &str, timestamp: i64) -> Value {
    let cid = uuid_v4_simple();
    json!({
        "eventCode": "chat_request_send",
        "timestamp": timestamp,
        "reportDelay": 0,
        "source": "mini_program",
        "ideName": "wx_app_cloud",
        "ideType": "WorkBuddy_MP",
        "extName": "workbuddy-mp",
        "extVersion": "2.4.0",
        "mode": "chat",
        "conversationId": cid,
        "requestId": cid,
        "inputLength": 20 + timestamp % 180,
        "mentionContexts": [],
        "mentionContextCount": 0,
        "userId": uid,
    })
}

/// mp 版 GLM5.2 对话事件（`Sequential_Tasks_5` 判据，09-29 抓 desc 实证：
/// 「在小程序内选择 GLM5.2 模型并完成有效对话」）。mp_chat_event 本身无 modelId，
/// 服务端按模型过滤 ⇒ 直接静默丢弃（silentDrop）。modelId 字段名/取值沿用
/// mini 载荷惯例（mp_automation_event 的 modelId/modelIsThinking + 小写连字符 id）。
fn mp_chat_glm_event(uid: &str, timestamp: i64) -> Value {
    let mut event = mp_chat_event(uid, timestamp);
    event["modelId"] = json!("glm-5.2");
    event["modelIsThinking"] = json!(false);
    event
}

/// mp 限定 Sequential 阶梯链的判据映射：`_2` = 选中专家并完成有效对话
/// （expert_actual_use 的 mini 变体）；`_4` = 创建一个定时任务（automation 的
/// mini 变体）；`_5` = 选 GLM5.2 模型对话（chat 的 mini 变体 + modelId）；
/// 其余（`_1`/`_3`…）= mini 对话。
/// 次数一律以服务端 accept 后下发的 target 为准（desc 实证 `_3`=5 次而非 3 次）。
fn mp_event_kind(code: &str) -> &'static str {
    match code {
        "Sequential_Tasks_2" => "mp_expert",
        "Sequential_Tasks_4" => "mp_automation",
        "Sequential_Tasks_5" => "mp_chat_glm",
        _ => "mp_chat",
    }
}

/// mp 版专家使用事件（`Sequential_Tasks_2` 判据）：school_expert_event 形状
/// **去 activityId**（无 school 域关联）+ mini 指纹字段。
fn mp_expert_event(uid: &str, expert_id: String, expert_name: String, timestamp: i64) -> Value {
    let cid = uuid_v4_simple();
    let mut event = json!({
        "eventCode": "expert_actual_use",
        "timestamp": timestamp,
        "reportDelay": 0,
        "id": expert_id,
        "name": expert_id,
        "expertTitle": expert_name,
        "type": "send_message",
        "characterCount": 20 + timestamp % 180,
        "expertType": "agent",
        "conversationId": cid,
        "source": "mini_program",
        "ideName": "wx_app_cloud",
        "ideType": "WorkBuddy_MP",
        "extName": "workbuddy-mp",
        "extVersion": "2.4.0",
    });
    merge_fields(&mut event, &mp_fingerprint(uid, ""));
    event
}

/// mp 版定时任务创建事件（`Sequential_Tasks_4` 判据）：web automation_1 的
/// `automated_task_create_suc` 形状 + mini 指纹；name/prompt 用服务端 desc 自带
/// 例句（「提醒我每天 7 点钟读书」）。⚠️ 判据为形状推断未实测，silentDrop 如实记录。
fn mp_automation_event(uid: &str, timestamp: i64) -> Value {
    let cid = uuid_v4_simple();
    let mut event = json!({
        "eventCode": "automated_task_create_suc",
        "timestamp": timestamp,
        "reportDelay": 0,
        "name": "每天早上7点提醒读书",
        "source": "manually",
        "modelId": "deepseek-v4-flash",
        "modelIsThinking": false,
        "expertId": "",
        "expertMarketplace": "",
        "connectorIds": "",
        "connectorCount": 0,
        "skills": "",
        "skillCount": 0,
        "scheduleType": "recurring",
        "pushToWeChat": false,
        "pushToWecomBot": false,
        "conversationId": cid,
        "requestId": cid,
        "schedule": {"type": "recurring", "rrule": "FREQ=DAILY;BYHOUR=7;BYMINUTE=0"},
        "prompt": "提醒我每天 7 点钟读书",
        "userId": uid,
    });
    merge_fields(&mut event, &mp_fingerprint(uid, ""));
    event
}

/// 跑单账号 mp 限定 Sequential 任务（全链路：mp 列表 → accept(mp头) → 判据上报 →
/// 归账留时 → 回读 → claim(mp头)）。status：claimed=本轮新领 / done=已领 /
/// progress=上报了但未点亮（silentDrop 可能） / error / skipped。
async fn run_mp_task(account: &Value, code: &str, task: &Value) -> Value {
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uid.is_empty() {
        return json!({"status": "skipped", "reason": "no-uid"});
    }
    let kind = task_type_of(Some(task));
    let accept = accept_status(Some(task));
    let (current, target) = task_progress(Some(task), 1);
    if accept == "claimed" || (target > 0 && current >= target) {
        return json!({"status": "done", "note": "already-claimed", "current": current, "target": target, "taskType": kind});
    }
    // accept（mp 头；缺头实测 task not found）
    if accept == "not_accepted" {
        let body = json!({ "task_codes": [code] });
        let resp = domain_request(
            CHAT_BASE,
            TASKS_ACCEPT_PATH,
            "POST",
            Some(body),
            Some(mp_platform_headers()),
            account,
        )
        .await;
        let ok = resp.get("code").and_then(Value::as_i64) == Some(0)
            && resp.pointer("/data/results/0/status").and_then(Value::as_str) == Some("accepted");
        write_gap().await;
        if !ok {
            // 服务端阶梯冷却锁（2026-09-26 实测：_2 领完 _3 accept 返回
            // status=error + "task locked until 2026-09-27"）——次日自动解锁，
            // 属正常等待语义，计 skipped 不计 failed（否则 chip 天天报"没成"）。
            let msg = resp
                .pointer("/data/results/0/message")
                .and_then(Value::as_str)
                .unwrap_or("");
            if msg.contains("task locked") {
                let until = msg
                    .split("until ")
                    .nth(1)
                    .unwrap_or("?")
                    .trim()
                    .to_string();
                return json!({
                    "status": "skipped",
                    "reason": format!("locked-until-{until}"),
                    "taskType": kind,
                });
            }
            // 其余失败：服务端原始 code/message 留痕进缓存，否则排查要重写探针
            let mut err = json!({"status": "error", "error": "mp accept 未登记生效", "taskType": kind});
            if let Some(c) = resp.get("code") {
                err["serverCode"] = c.clone();
            }
            if let Some(m) = resp.get("message").or_else(|| resp.get("msg")) {
                err["serverMsg"] = m.clone();
            }
            if let Some(st) = resp.pointer("/data/results/0/status") {
                err["serverStatus"] = st.clone();
            }
            return err;
        }
    }
    // not_accepted 时 progress 为 null ⇒ accept 后回读拿真实 target（desc 实证 _3=5 次）
    let (current, target) = if target > 0 {
        (current, target)
    } else {
        match list_growth_tasks_mp(account).await {
            Ok(t2) => find_task(&t2, code).map_or((current, target), |t| task_progress(Some(t), 1)),
            Err(_) => (current, target),
        }
    };
    // 判据上报：mini 指纹事件走 codebuddy.cn 域（与 school mini chat 同款头）
    let need = (target - current).max(0);
    for _ in 0..need {
        let now = now_ms();
        let event = match mp_event_kind(code) {
            "mp_expert" => {
                let (expert_id, expert_name) = fetch_school_expert(account).await;
                mp_expert_event(uid, expert_id, expert_name, now)
            }
            "mp_automation" => mp_automation_event(uid, now),
            "mp_chat_glm" => mp_chat_glm_event(uid, now),
            _ => mp_chat_event(uid, now),
        };
        let body = json!([event]);
        if let Err(e) =
            domain_data(CHAT_BASE, REPORT_PATH, "POST", Some(body), Some(mp_ua_header()), account).await
        {
            return json!({"status": "error", "error": e, "taskType": kind});
        }
        write_gap().await;
    }
    // 归账留时（2api sleep 2.0 同款）
    tokio::time::sleep(Duration::from_secs(2)).await;
    // 回读（mp 口径）+ 达标即领（mp 头）
    let tasks2 = list_growth_tasks_mp(account).await.unwrap_or_default();
    let after = find_task(&tasks2, code);
    let (cur2, tgt2) = task_progress(after, target);
    let acc2 = accept_status(after);
    let mut result = json!({"status": "progress", "current": cur2, "target": tgt2, "taskType": kind});
    if need > 0 && cur2 == current {
        result["silentDrop"] = json!(true); // 上报了但没点亮：判据可能不对，如实记录
    }
    if cur2 >= tgt2 || acc2 == "completed" {
        write_gap().await;
        let path = format!("{TASK_CLAIM_PATH}/{code}/claim");
        let resp = domain_request(
            CHAT_BASE,
            &path,
            "POST",
            None,
            Some(mp_platform_headers()),
            account,
        )
        .await;
        if resp.get("code").and_then(Value::as_i64) == Some(0) {
            result["status"] = json!("claimed");
            result["claimed"] = json!(1);
        } else {
            let message = resp
                .get("message")
                .or_else(|| resp.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("claim failed");
            if message.to_lowercase().contains("already") {
                result["status"] = json!("done");
                result["note"] = json!("already-claimed");
            } else {
                result["status"] = json!("error");
                result["error"] = json!(format!("claim {message}"));
            }
        }
    }
    result
}

/// 在任务列表里找指定 code 的任务。
fn find_task<'a>(tasks: &'a [Value], code: &str) -> Option<&'a Value> {
    tasks
        .iter()
        .find(|task| task.get("task_code").and_then(Value::as_str) == Some(code))
}

/// 任务接单状态（`accept_status` 原值；缺失按 not_accepted）。
fn accept_status(task: Option<&Value>) -> String {
    task.and_then(|task| task.get("accept_status").and_then(Value::as_str))
        .unwrap_or("not_accepted")
        .to_string()
}

/// 任务进度 `(current, target)`；缺失时回落脚本兜底值。
/// 服务端周期字段（坑 78）：`single`=一次性 / `recurring`=每日 / `auto`（first_buddy）。
/// 取不到返回 `Null` —— 前端按「未知」处理，不猜、不硬编码清单。
fn task_type_of(task: Option<&Value>) -> Value {
    task.and_then(|t| t.get("task_type"))
        .and_then(Value::as_str)
        .map(|kind| json!(kind))
        .unwrap_or(Value::Null)
}

/// 统计一段任务里「每日 / 一次性」各几项（前端据此分组，别靠 target 猜）。
fn count_kinds(tasks: &[Value]) -> (i64, i64) {
    let mut recurring = 0i64;
    let mut once = 0i64;
    for task in tasks {
        match task.get("task_type").and_then(Value::as_str) {
            Some("recurring") => recurring += 1,
            Some(_) => once += 1,
            None => {}
        }
    }
    (recurring, once)
}

fn task_progress(task: Option<&Value>, fallback_target: i64) -> (i64, i64) {
    let progress = task.and_then(|task| task.get("progress")).cloned();
    let current = progress
        .as_ref()
        .and_then(|progress| progress.get("current"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let target = progress
        .as_ref()
        .and_then(|progress| progress.get("target"))
        .and_then(Value::as_i64)
        .filter(|target| *target > 0)
        .unwrap_or(fallback_target);
    (current, target)
}

/// accept 并验证登记生效：读回 `accept_status`（响应 status 只是初筛，可能 200 但未落账）。
/// 诊断用：对单个任务做一次 accept 原始请求（不重试、不判定），返回原始响应。
pub async fn accept_once_raw(account: &Value, code: &str) -> Value {
    let body = json!({ "task_codes": [code] });
    domain_request(CHAT_BASE, TASKS_ACCEPT_PATH, "POST", Some(body), None, account).await
}

/// mp 口径 accept 原始请求（诊断用，对称 web 版 `accept_once_raw`）：
/// 带 mp 平台头（缺头实测 task not found），不重试、不判定。
pub async fn mp_accept_once_raw(account: &Value, code: &str) -> Value {
    let body = json!({ "task_codes": [code] });
    domain_request(
        CHAT_BASE,
        TASKS_ACCEPT_PATH,
        "POST",
        Some(body),
        Some(mp_platform_headers()),
        account,
    )
    .await
}

async fn accept_with_verify(account: &Value, code: &str) -> bool {
    for _attempt in 1..=2 {
        let body = json!({ "task_codes": [code] });
        let resp = domain_request(
            CHAT_BASE,
            TASKS_ACCEPT_PATH,
            "POST",
            Some(body),
            None,
            account,
        )
        .await;
        let http_ok = resp.get("code").and_then(Value::as_i64) == Some(0);
        let accepted = resp
            .pointer("/data/results/0/status")
            .and_then(Value::as_str)
            == Some("accepted");
        // 回读确认登记生效（accept_status 可能滞后，重读任务列表；
        // 读失败（频控/网络）≠ 未登记——重试读，读成功且仍 not_accepted 才判失败）
        let mut registered = false;
        for _ in 0..3 {
            match list_growth_tasks(account).await {
                Ok(tasks) => {
                    registered = accept_status(find_task(&tasks, code)) != "not_accepted";
                    break;
                }
                Err(_) => write_gap().await,
            }
        }
        if http_ok && accepted && registered {
            return true;
        }
        // 响应明确 accepted 且重读全部失败：再给一次机会（保守，不轻信）
        if http_ok && accepted && !registered {
            write_gap().await;
            if let Ok(tasks) = list_growth_tasks(account).await {
                registered = accept_status(find_task(&tasks, code)) != "not_accepted";
            }
        }
        if http_ok && accepted && registered {
            return true;
        }
        write_gap().await;
    }
    false
}

/// 领奖：`POST /activity/growth/tasks/{code}/claim`（路径含 code、无 body）。
/// chat 域失败时回落 web 域 + web 端头（2api `web_claim_fallback` 同款）。
async fn claim_growth_task(account: &Value, code: &str) -> Result<Value, String> {
    let path = format!("{TASK_CLAIM_PATH}/{code}/claim");
    let resp = domain_request(CHAT_BASE, &path, "POST", None, None, account).await;
    let code_value = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code_value == 0 {
        return Ok(resp);
    }
    // chat 域 400 / 业务非 0：回落 web 域再试一次（400 视为可回落信号）
    if code_value == 400 || code_value == -1 {
        let mut headers = tasks_headers(account);
        headers.insert("Origin".to_string(), WEB_CLAIM_BASE.to_string());
        headers.insert("Referer".to_string(), format!("{WEB_CLAIM_BASE}/"));
        headers.insert("x-client-platform".to_string(), "web".to_string());
        headers.insert("X-Domain".to_string(), WEB_CLAIM_BASE.to_string());
        let url = format!("{WEB_CLAIM_BASE}{path}");
        let resp = http_request(&url, "POST", None, Some(&headers)).await;
        let fallback_code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if fallback_code == 0 {
            return Ok(resp);
        }
        let message = resp
            .get("message")
            .or_else(|| resp.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("claim failed");
        // already_claimed 幂等：视为成功
        if message.to_lowercase().contains("already") {
            return Ok(resp);
        }
        return Err(format!("code={fallback_code} {message}"));
    }
    let message = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("claim failed");
    if message.to_lowercase().contains("already") {
        return Ok(resp);
    }
    Err(format!("code={code_value} {message}"))
}

// ---------------------------------------------------------------------------
// 夜猫子 black_cat
// ---------------------------------------------------------------------------

/// 夜猫窗口判定：CST 23:00–次日 08:00（2api `within_night_window` 同款）。
pub fn within_night_window(now_hour: u32) -> bool {
    now_hour >= 23 || now_hour < 8
}

/// 跑单账号夜猫子任务。返回结果摘要（status: done / skipped / error）。
pub async fn run_black_cat(account: &Value) -> Value {
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uid.is_empty() {
        return json!({"status": "skipped", "reason": "no-uid"});
    }
    let tasks = match list_growth_tasks(account).await {
        Ok(tasks) => tasks,
        Err(error) => return json!({"status": "error", "error": error}),
    };
    let task = find_task(&tasks, "black_cat");
    let Some(task) = task else {
        // 服务端未下发该任务（活动调整/账号不可见）：如实记录，不算失败
        return json!({"status": "skipped", "reason": "task-missing"});
    };
    let accept = accept_status(Some(task));
    let (current, target) = task_progress(Some(task), 3);
    // 周期字段随段下发（坑 78）：black_cat 服务端是 single，官方 desc「每天 1 次，累计 3 天」
    // ⇒ 一次性但跨多天推进，前端按 target-current 显示「还需 N 天」。
    let kind = task_type_of(Some(task));
    if accept == "claimed" {
        return json!({"status": "done", "note": "already-claimed", "current": current, "target": target, "taskType": kind});
    }
    if current >= target || accept == "completed" {
        write_gap().await;
        return match claim_growth_task(account, "black_cat").await {
            Ok(_) => json!({"status": "done", "note": "claimed", "current": current, "target": target, "taskType": kind}),
            Err(error) => json!({"status": "error", "error": error, "taskType": kind}),
        };
    }
    // 时段敏感：非夜猫窗口直接 skip（不是失败，明天窗口内会补）
    if !within_night_window(cst_now().hour()) {
        return json!({"status": "skipped", "reason": "outside-night-window", "current": current, "target": target, "taskType": kind});
    }
    if accept == "not_accepted" {
        if !accept_with_verify(account, "black_cat").await {
            return json!({"status": "error", "error": "accept 未登记生效"});
        }
        write_gap().await;
    }
    // 窗口内最多补 1 次（2api cap=1：即使 target 未满也只发一条）
    // ⚠️ conversationId 必须用 uuid：自造前缀（如 `wbs-night-<ms>`）是服务端一条 LIKE 就能
    // 捞出全部流量的特征（09-25 风险审计）。防检测纪律见模块头坑注。
    let conversation_id = uuid_v4_simple();
    let body = json!([night_chat_event(uid, &conversation_id, now_ms())]);
    let report = domain_data(CHAT_BASE, REPORT_PATH, "POST", Some(body), None, account).await;
    if let Err(error) = report {
        return json!({"status": "error", "error": error});
    }
    // 回读进度：+1 记点亮，未变记静默丢弃（200 ≠ 计分，与活跃地图同口径）
    let tasks = list_growth_tasks(account).await.unwrap_or_default();
    let after = find_task(&tasks, "black_cat");
    let (after_current, after_target) = task_progress(after, target);
    let after_accept = accept_status(after);
    let mut result = if after_current > current {
        json!({"status": "progress", "current": after_current, "target": after_target, "taskType": kind})
    } else {
        json!({"status": "progress", "silentDrop": true, "current": after_current, "target": after_target, "taskType": kind})
    };
    if after_current >= after_target || after_accept == "completed" {
        write_gap().await;
        match claim_growth_task(account, "black_cat").await {
            Ok(_) => result["status"] = json!("done"),
            Err(error) => {
                result["status"] = json!("done");
                result["claimError"] = json!(error);
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// 开学季 school_open_day_2026
// ---------------------------------------------------------------------------

/// 开学季任务清单（脚本 `KNOWN_TASKS` 同款；manual/unknown 保守跳过）。
const SCHOOL_REPORT_TASKS: &[(&str, &str)] = &[
    ("chat_3_times", "mini_chat"),
    ("desktop_chat_1_time", "desktop_seq"),
    ("expert_use", "expert"),
];
const SCHOOL_SHARE_TASK: &str = "share_invite";

/// 拉开学季任务与进行期标志。
pub async fn fetch_school_tasks(account: &Value) -> Result<(Vec<Value>, bool), String> {
    let data = domain_data(
        CHAT_BASE,
        &format!("{SCHOOL_BASE}/tasks"),
        "GET",
        None,
        Some(mp_ua_header()),
        account,
    )
    .await?;
    let in_period = data.get("in_period").and_then(Value::as_bool).unwrap_or(false);
    let tasks = data
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok((tasks, in_period))
}

/// school 域任务激活（pending → in_progress）。
async fn school_viewed(account: &Value, code: &str) -> Result<(), String> {
    domain_data(
        CHAT_BASE,
        &format!("{SCHOOL_BASE}/tasks/{code}/viewed"),
        "POST",
        None,
        Some(mp_ua_header()),
        account,
    )
    .await
    .map(|_| ())
}

/// share_invite 完成判据（一次即完成）。
async fn school_share_complete(account: &Value) -> Result<(), String> {
    let body = json!({ "channel": "wechat" });
    domain_data(
        CHAT_BASE,
        &format!("{SCHOOL_BASE}/tasks/share-complete"),
        "POST",
        Some(body),
        Some(mp_ua_header()),
        account,
    )
    .await
    .map(|_| ())
}

/// school 域领奖（completed → claimed，发抽奖机会）。
async fn school_claim(account: &Value, code: &str) -> Result<(), String> {
    domain_data(
        CHAT_BASE,
        &format!("{SCHOOL_BASE}/tasks/{code}/claim"),
        "POST",
        None,
        Some(mp_ua_header()),
        account,
    )
    .await
    .map(|_| ())
}

/// 拉取一个 BackToSchool 分类专家；失败回落已知专家（2api 同款兜底）。
async fn fetch_school_expert(account: &Value) -> (String, String) {
    let body = json!({
        "edition_mode": "all,domestic",
        "page": 1,
        "page_size": 20,
        "sort_by": "use_count",
        "sort_order": "desc",
        "categories": [SCHOOL_EXPERT_CATEGORY],
        "expert_type": "agent",
    });
    if let Ok(data) = domain_data(
        CHAT_BASE,
        EXPERT_LIST_PATH,
        "POST",
        Some(body),
        None,
        account,
    )
    .await
    {
        if let Some(expert) = data
            .get("experts")
            .and_then(Value::as_array)
            .and_then(|experts| experts.first())
        {
            let id = expert
                .get("expert_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !id.is_empty() {
                let name = expert
                    .pointer("/display_name_zh/zh")
                    .and_then(Value::as_str)
                    .or_else(|| expert.get("display_name_zh").and_then(Value::as_str))
                    .unwrap_or(id);
                return (id.to_string(), name.to_string());
            }
        }
    }
    let (id, name) = SCHOOL_EXPERT_FALLBACK[0];
    (id.to_string(), name.to_string())
}

/// school 单任务进度字段（progress/target_count 直接是数字，与 growth tasks 域不同）。
fn school_progress(task: &Value) -> (i64, i64) {
    let current = task.get("progress").and_then(Value::as_i64).unwrap_or(0);
    let target = task
        .get("target_count")
        .and_then(Value::as_i64)
        .filter(|target| *target > 0)
        .unwrap_or(1);
    (current, target)
}

/// 触发一次 school 完成判据上报；返回事件条数（share 类不走这里）。
async fn school_report_once(
    account: &Value,
    kind: &str,
    uid: &str,
    nick: &str,
) -> Result<usize, String> {
    let stamp = now_ms();
    match kind {
        "mini_chat" => {
            // conversationId 用 uuid，别用自造前缀（防检测，见 run_black_cat 注）。
            let event = mini_chat_event(uid, nick, &uuid_v4_simple(), stamp);
            domain_data(
                CHAT_BASE,
                REPORT_PATH,
                "POST",
                Some(json!([event])),
                Some(mp_ua_header()),
                account,
            )
            .await
            .map(|_| 1)
        }
        "expert" => {
            let (expert_id, expert_name) = fetch_school_expert(account).await;
            let event = school_expert_event(
                uid,
                nick,
                &expert_id,
                &expert_name,
                &uuid_v4_simple(),
                stamp,
            );
            domain_data(
                CHAT_BASE,
                REPORT_PATH,
                "POST",
                Some(json!([event])),
                Some(mp_ua_header()),
                account,
            )
            .await
            .map(|_| 1)
        }
        "desktop_seq" => {
            let conv = uuid_v4_simple();
            let events = desktop_chat_sequence(uid, nick, &conv, &conv, &conv, stamp);
            let count = events.len();
            let extra = vec![
                ("X-Product".to_string(), "SaaS".to_string()),
                ("User-Agent".to_string(), DESKTOP_UA.to_string()),
            ];
            domain_data(
                COPILOT_BASE,
                REPORT_PATH,
                "POST",
                Some(json!(events)),
                Some(extra),
                account,
            )
            .await
            .map(|_| count)
        }
        _ => Err(format!("未知 school report_kind: {kind}")),
    }
}

/// 跑单账号开学季任务。返回结果摘要。
pub async fn run_school(account: &Value) -> Value {
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let nick = account
        .get("nickname")
        .or_else(|| account.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uid.is_empty() {
        return json!({"status": "skipped", "reason": "no-uid"});
    }
    let (tasks, in_period) = match fetch_school_tasks(account).await {
        Ok(pair) => pair,
        Err(error) => return json!({"status": "error", "error": error}),
    };
    if !in_period {
        // 活动下线自动空转（坑 6）：不算失败
        return json!({"status": "skipped", "reason": "out-of-period"});
    }
    let mut summary = json!({});
    let mut claimed_count = 0i64;
    for (code, kind) in SCHOOL_REPORT_TASKS {
        let Some(task) = find_task(&tasks, code) else {
            continue;
        };
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_lowercase();
        if status == "claimed" || status == "completed" {
            summary[code] = json!(status);
            continue;
        }
        if status == "pending" {
            if let Err(error) = school_viewed(account, code).await {
                summary[code] = json!({ "status": "error", "error": error });
                continue;
            }
            write_gap().await;
        }
        let (current, target) = school_progress(task);
        let need = (target - current).max(1);
        let mut done = 0i64;
        for _ in 0..need {
            match school_report_once(account, kind, uid, nick).await {
                Ok(_) => done += 1,
                Err(error) => {
                    summary[code] = json!({ "status": "error", "error": error });
                    break;
                }
            }
            write_gap().await;
        }
        // 回读确认；completed 即领
        if let Ok((tasks_now, _)) = fetch_school_tasks(account).await {
            if let Some(after) = find_task(&tasks_now, code) {
                let after_status = after
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending")
                    .to_lowercase();
                if after_status == "completed" {
                    write_gap().await;
                    match school_claim(account, code).await {
                        Ok(_) => {
                            summary[code] = json!("claimed");
                            claimed_count += 1;
                        }
                        Err(error) => summary[code] = json!({ "status": "completed", "claimError": error }),
                    }
                } else {
                    summary[code] = json!({ "status": after_status, "reported": done });
                }
            }
        }
    }
    // share_invite：一次即完成
    if let Some(task) = find_task(&tasks, SCHOOL_SHARE_TASK) {
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_lowercase();
        if status != "claimed" && status != "completed" {
            if status == "pending" {
                let _ = school_viewed(account, SCHOOL_SHARE_TASK).await;
                write_gap().await;
            }
            match school_share_complete(account).await {
                Ok(_) => {
                    write_gap().await;
                    match school_claim(account, SCHOOL_SHARE_TASK).await {
                        Ok(_) => {
                            summary[SCHOOL_SHARE_TASK] = json!("claimed");
                            claimed_count += 1;
                        }
                        Err(error) => summary[SCHOOL_SHARE_TASK] = json!({ "status": "completed", "claimError": error }),
                    }
                }
                Err(error) => summary[SCHOOL_SHARE_TASK] = json!({ "status": "error", "error": error }),
            }
        } else {
            summary[SCHOOL_SHARE_TASK] = json!(status);
        }
    }
    // 抽奖：claim 会发抽奖机会，余额 > 0 就抽空；409 no chance 安全即停（坑 11）
    let mut lottery = json!({"drawn": 0});
    if let Ok(config) = domain_data(
        CHAT_BASE,
        &format!("{SCHOOL_BASE}/config"),
        "GET",
        None,
        Some(mp_ua_header()),
        account,
    )
    .await
    {
        let balance = config
            .pointer("/chance/balance")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mut drawn = 0i64;
        for _ in 0..balance.max(0) {
            let body = json!({ "draw_uuid": uuid_v4_simple() });
            match domain_data(
                CHAT_BASE,
                &format!("{SCHOOL_BASE}/wheel/draw"),
                "POST",
                Some(body),
                Some(mp_ua_header()),
                account,
            )
            .await
            {
                Ok(_) => drawn += 1,
                Err(error) => {
                    lottery["stopReason"] = json!(error);
                    break;
                }
            }
            write_gap().await;
        }
        lottery["drawn"] = json!(drawn);
    }
    // 活动任务段是混合周期（首期实测：3 项 recurring + 2 项 single），两个计数都下发，
    // 前端据此决定整段归「每日」还是「一次性」（坑 78）。
    let (recurring, once) = count_kinds(&tasks);
    json!({
        "status": if summary.as_object().is_some_and(|map| map.values().any(|v| v.get("status").and_then(Value::as_str) == Some("error"))) { "partial" } else { "done" },
        "claimed": claimed_count,
        "recurring": recurring,
        "once": once,
        "tasks": summary,
        "lottery": lottery,
    })
}

// ---------------------------------------------------------------------------
// 成长任务全家族（2api task_runner.py M1-M15 移植：纯上报/领取类一次性任务）
// ---------------------------------------------------------------------------
// 范围：纯上报类 + 现成可领；**不可伪造**的永远跳过——Expert_Philanthropy（真实捐款）
// / wb_wechat_oa_subscribe_task（真人关注公众号）/ task_student_verify（人工认证）。
// 通道分域（2api 实测口径，别合并到一块）：
//   常规上报 = codebuddy.cn/v2/report（与活跃地图同域，实证可用）
//   桌面链   = copilot.tencent.com/v2/report + 桌面 UA + X-Product: SaaS
//   资料库   = www.workbuddy.cn/v2/report + 浏览器指纹（web 域）
//   任务域   = 本模块既有 CHAT_BASE（list/accept/claim，已实证）

const SCENES_PATH: &str = "/console/as/support/scenes?locale=zh-CN";
const SKILL_LIST_PATH: &str = "/v2/operation-platform/market/skill/list";
const APPEARANCE_RESOURCES_PATH: &str = "/v2/operation-platform/appearance/resources";
const PLAYBOOK_REGISTRY_URL: &str = "https://static.workbuddy.cn/workbuddy/playbook/registry.json";
const COS_EXPERT_URL: &str = "https://acc-1258344699.cos.accelerate.myqcloud.com/workbuddy/expert-marketplace/expert_center.json";
const PATH_BUDDY_AGREEMENT: &str = "/activity/growth/buddy/agreement";
const PATH_BUDDY_FIRST: &str = "/activity/growth/buddy/first";
const BUDDY_APP_ID: &str = "cb_y5Dy46tPQGGWtueMxXbe";
const BUDDY_APP_NAME: &str = "企鹅教师助手";
const LIBRARY_DOC_URL: &str = "https://www.workbuddy.cn/space/d/o0KWYeynteVv06UnAZqIFm";
const WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

/// (task_code, kind, 兜底 target)。kind 分派到事件构造器与 id 来源；
/// 兜底 target 用于 accept 前 progress=null 的场景（2api spec target 同款）。
const FAMILY_TASKS: &[(&str, &str, i64)] = &[
    ("create_canvas", "canvas", 1),
    ("template_5", "template", 5),
    ("expert_5", "expert", 5),
    ("Expert_team_use_3", "team", 3),
    ("skill_1", "skill", 1),
    ("automation_1", "automation", 1),
    ("playbook_prompt", "playbook", 1),
    ("Expert_lighthouse", "lighthouse", 1),
    ("Hp_Appearance", "skin", 1),
    ("chat_5", "chat", 5),
    ("Model_chat_GLM5.2", "glmchat", 1),
    ("Library_read", "library", 1),
    ("RichMeow_Chat", "richmeow", 1),
    ("Buddy_App", "buddy5", 1),
    ("Buddy_App_QQ", "buddy5", 1),
    ("first_buddy", "buddyfirst", 1),
];

/// 兜底表（在线清单拉不到时用，2api DEFAULT_* 同款）。
const DEFAULT_SCENES: &[(&str, &str)] = &[
    ("0", "幻灯片"), ("2", "视频生成"), ("4", "深度研究"), ("6", "文档处理"),
    ("8", "数据分析"), ("10", "可视化"), ("12", "金融服务"), ("14", "产品管理"),
];
const DEFAULT_EXPERTS: &[(&str, &str)] = &[
    ("ContentCreator", "内容创作专家"), ("UiDesigner", "UI设计师"),
    ("DataAnalyticsReporter", "数据分析报告师"), ("ChinaEcommerceOperationsExpert", "中国电商运营专家"),
    ("DouyinStrategist", "抖音策略师"), ("SalesCoach", "销售教练"),
    ("BrandGuardian", "品牌策略师"), ("XiaohongshuOperationsExpert", "小红书运营专家"),
];
const DEFAULT_TEAMS: &[&str] = &[
    "CloudOpsTeam", "SoftwareCompany", "TradingAgentTeam",
    "GPTResearcherTeam", "MarketingCampaignTeam",
];
const DEFAULT_SKILLS: &[(&str, &str)] = &[
    ("skill_2096525080079265792", "pptx"),
    ("skill_2096528888507297792", "xlsx"),
    ("skill_2070033533400236032", "qqmusic"),
    ("skill_2095322904487550976", "pdf"),
];
const DEFAULT_CASES: &[(&str, &str)] = &[("worker-ledger-freedom-dashboard", "打工人小账本")];
const LIGHTHOUSE_FALLBACK: &[(&str, &str)] = &[
    ("ex_2cvvUZQhDyeJ", "腾讯轻量云专家"),
];
const PE_THEME_FALLBACK: &str = "theme-tkmw7j";

/// (id, name) 对象清单（各家 id 来源统一形状）。
type IdList = Vec<(String, String)>;

fn dedup_slice(src: IdList, offset: usize, need: usize) -> IdList {
    let mut seen = std::collections::HashSet::new();
    let unique: IdList = src
        .into_iter()
        .filter(|(id, _)| seen.insert(id.clone()))
        .collect();
    unique.into_iter().skip(offset).take(need).collect()
}

async fn public_json(url: &str) -> Option<Value> {
    let headers = std::collections::HashMap::from([(
        "User-Agent".to_string(),
        WEB_UA.to_string(),
    )]);
    let resp = http_request(url, "GET", None, Some(&headers)).await;
    if resp.get("code").is_some() && resp.get("code").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    Some(resp)
}

async fn fetch_scenes(account: &Value) -> IdList {
    let mut out: IdList = Vec::new();
    if let Ok(data) = domain_data(CHAT_BASE, SCENES_PATH, "GET", None, None, account).await {
        if let Some(scenes) = data.get("scenes").and_then(Value::as_array) {
            for scene in scenes {
                if let Some(id) = scene.get("id") {
                    let name = scene.get("name").and_then(Value::as_str).unwrap_or("");
                    out.push((id.to_string(), name.to_string()));
                }
            }
        }
    }
    if out.is_empty() {
        out = DEFAULT_SCENES
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect();
    }
    out
}

/// 专家市场分页拉取；`team_only` 只留团队专家；COS 静态清单补足 team 来源。
async fn fetch_market_experts(account: &Value, team_only: bool, keyword: Option<&str>) -> IdList {
    let mut out: IdList = Vec::new();
    for page in 1..=3 {
        let mut body = json!({ "page": page, "page_size": 50 });
        if let Some(keyword) = keyword {
            body["keyword"] = json!(keyword);
        }
        let Ok(data) = domain_data(CHAT_BASE, EXPERT_LIST_PATH, "POST", Some(body), None, account).await
        else {
            break;
        };
        let Some(experts) = data.get("experts").and_then(Value::as_array) else {
            break;
        };
        if experts.is_empty() {
            break;
        }
        for expert in experts {
            let id = expert
                .get("expert_id")
                .or_else(|| expert.get("source_id"))
                .and_then(Value::as_str);
            let Some(id) = id else { continue };
            let etype = expert
                .get("expert_type")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            if team_only && etype != "team" {
                continue;
            }
            let name = expert
                .get("display_name_zh")
                .or_else(|| expert.get("profession_zh"))
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push((id.to_string(), name.to_string()));
        }
        if experts.len() < 50 {
            break;
        }
    }
    // COS 清单补足（团队专家市场列表很薄，COS 是 team 的主/补来源）
    if let Some(data) = public_json(COS_EXPERT_URL).await {
        if let Some(experts) = data.get("experts").and_then(Value::as_array) {
            let seen: std::collections::HashSet<String> = out.iter().map(|(id, _)| id.clone()).collect();
            for expert in experts {
                let Some(id) = expert.get("id").and_then(Value::as_str) else { continue };
                let etype = expert
                    .get("expertType")
                    .and_then(Value::as_str)
                    .unwrap_or("agent");
                if team_only && etype != "team" {
                    continue;
                }
                if seen.contains(id) {
                    continue;
                }
                let name = expert
                    .pointer("/profession/zh")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                out.push((id.to_string(), name.to_string()));
            }
        }
    }
    if out.is_empty() {
        let source: &[&str] = if team_only { DEFAULT_TEAMS } else { &[] };
        if team_only {
            out = source
                .iter()
                .map(|id| ((*id).to_string(), String::new()))
                .collect();
        } else {
            out = DEFAULT_EXPERTS
                .iter()
                .map(|(id, name)| (id.to_string(), name.to_string()))
                .collect();
        }
    }
    out
}

async fn fetch_skills(account: &Value) -> IdList {
    let mut out: IdList = Vec::new();
    let body = json!({ "page": 1, "page_size": 8 });
    if let Ok(data) = domain_data(CHAT_BASE, SKILL_LIST_PATH, "POST", Some(body), None, account).await {
        if let Some(skills) = data.get("skills").and_then(Value::as_array) {
            for skill in skills {
                if let Some(id) = skill.get("skill_id").and_then(Value::as_str) {
                    let name = skill.get("name").and_then(Value::as_str).unwrap_or("");
                    out.push((id.to_string(), name.to_string()));
                }
            }
        }
    }
    if out.is_empty() {
        out = DEFAULT_SKILLS
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect();
    }
    out
}

async fn fetch_playbook_cases() -> IdList {
    let mut out: IdList = Vec::new();
    if let Some(data) = public_json(PLAYBOOK_REGISTRY_URL).await {
        if let Some(cases) = data.get("cases").and_then(Value::as_array) {
            for case in cases {
                if let Some(id) = case.get("id").and_then(Value::as_str) {
                    out.push((id.to_string(), String::new()));
                }
            }
        }
    }
    if out.is_empty() {
        out = DEFAULT_CASES
            .iter()
            .map(|(id, title)| (id.to_string(), title.to_string()))
            .collect();
    }
    out
}

async fn fetch_lighthouse(account: &Value) -> IdList {
    let mut out: IdList = Vec::new();
    for keyword in ["lighthouse", "轻量云"] {
        for (id, name) in fetch_market_experts(account, false, Some(keyword)).await {
            let blob = format!("{id}{name}").to_lowercase();
            if ["lighthouse", "轻量", "light", "yun"].iter().any(|k| blob.contains(k)) {
                out.push((id, name));
            }
        }
    }
    if out.is_empty() {
        out = LIGHTHOUSE_FALLBACK
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect();
    }
    out
}

async fn fetch_pe_theme(account: &Value) -> IdList {
    let mut out: IdList = Vec::new();
    let body = json!({
        "platform": "client", "kind": "theme", "version": "5.5.6", "lang": "zh-CN",
    });
    if let Ok(data) = domain_data(CHAT_BASE, APPEARANCE_RESOURCES_PATH, "POST", Some(body), None, account).await {
        if let Some(resources) = data.get("resources").and_then(Value::as_array) {
            for resource in resources {
                let name = resource.get("name").and_then(Value::as_str).unwrap_or("");
                if name.contains("和平精英") || name.to_lowercase().contains("pubg") {
                    if let Some(id) = resource.get("id").and_then(Value::as_str) {
                        out.push((id.to_string(), name.to_string()));
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out = vec![(PE_THEME_FALLBACK.to_string(), "和平精英激战金秋".to_string())];
    }
    out
}

/// 任务家族事件构造（2api `build_event` 同款形状；必带 userId）。
fn build_family_event(
    uid: &str,
    kind: &str,
    obj_id: &str,
    meta: &str,
    _idx: usize,
) -> Value {
    let now = now_ms();
    // cid/rid 一律 uuid（`wb-run-*` 同样是自造前缀特征）。
    let cid = uuid_v4_simple();
    let rid = uuid_v4_simple();
    match kind {
        "canvas" => json!({
            "eventCode": "wbx_design_canvas_task_create", "timestamp": now,
            "reportDelay": 0, "conversationId": cid, "requestId": rid,
            "source": "summon_keyword", "isCustomModel": false, "name": "",
            "inputLength": 20 + now % 180, "id": if obj_id.is_empty() { format!("wbx-canvas-{now}") } else { obj_id.to_string() },
            "cost": 0, "isSuccessful": true, "userId": uid,
        }),
        "template" => json!({
            "eventCode": "agent_task_created_with_template", "timestamp": now,
            "reportDelay": 0, "isCustomModel": true, "id": obj_id,
            "name": meta, "requestId": rid, "conversationId": cid, "userId": uid,
        }),
        // expert / team / lighthouse 同为 expert_actual_use，仅 expertType 不同
        "expert" | "team" | "lighthouse" => {
            let expert_type = if kind == "team" { "team" } else { "agent" };
            json!({
                "eventCode": "expert_actual_use", "timestamp": now, "reportDelay": 0,
                "mode": "CLOUD", "id": obj_id, "name": meta,
                "expertTitle": meta, "type": "", "expertType": expert_type,
                "source": "builtin", "version": "", "cost": 0, "characterCount": 12,
                "conversationId": cid, "requestId": rid, "messageId": rid,
                "requestModelId": "deepseek-v4-flash", "requestModelName": "DeepSeek V4 Flash",
                "userId": uid,
            })
        }
        "skill" => json!({
            "eventCode": "skill_info", "timestamp": now, "reportDelay": 0,
            "skillId": obj_id, "skillName": if meta.is_empty() { obj_id } else { meta },
            "skillVersion": "", "action": "use", "conversationId": cid,
            "requestId": rid, "userId": uid,
        }),
        "automation" => json!({
            "eventCode": "automated_task_create_suc", "timestamp": now,
            "reportDelay": 0, "name": "每周五自动生成周报", "source": "manually",
            "modelId": "deepseek-v4-flash", "modelIsThinking": false,
            "expertId": "", "expertMarketplace": "", "connectorIds": "",
            "connectorCount": 0, "skills": "", "skillCount": 0,
            "scheduleType": "recurring", "pushToWeChat": false, "pushToWecomBot": false,
            "conversationId": cid, "requestId": rid,
            "schedule": {"type": "recurring", "rrule": "FREQ=WEEKLY;BYDAY=FR;BYHOUR=9;BYMINUTE=0"},
            "prompt": "每周五自动整理本周工作，生成一份周报。", "userId": uid,
        }),
        "playbook" => json!({
            "eventCode": "playbook_prompt_send", "timestamp": now, "reportDelay": 0,
            "id": obj_id, "name": if meta.is_empty() { obj_id } else { meta },
            "type": "other", "promptLength": 0, "isOfficial": 1,
            "skills": "", "skillNames": "", "expertId": "", "expertName": "",
            "categoryId": "", "categoryName": "", "query": "",
            "source": "discover", "conversationId": cid, "requestId": rid,
            "ext1": "discover", "userId": uid,
        }),
        "skin" => json!({
            "eventCode": "appearance_skin_apply", "timestamp": now, "reportDelay": 0,
            "action": "apply", "source": "settings_close", "id": obj_id,
            "vipLevel": "free", "series": "craft", "type": "unknown",
            "name": if meta.is_empty() { obj_id } else { meta }, "userId": uid,
        }),
        // chat_5 用 craft/deepseek；glmchat 用 craft/glm-5.2（black_cat 才是 night）
        "chat" | "glmchat" => {
            let (model_id, model_name) = if kind == "glmchat" {
                ("glm-5.2", "GLM-5.2")
            } else {
                ("deepseek-v4-flash", "DeepSeek V4 Flash")
            };
            json!({
                "eventCode": "chat_request_send", "timestamp": now, "reportDelay": 0,
                "mode": "craft", "conversationId": cid, "requestId": cid,
                "inputLength": 20 + now % 180, "requestModelId": model_id, "requestModelName": model_name,
                "isPlan": false, "isAutoExecuteTerminal": false, "isAutoModify": false,
                "codebaseEnable": false, "maxToken": 0, "maxSteps": 0, "temperature": 0,
                "maxRetries": 0, "mentionContexts": [], "knowledgeId": [],
                "knowledgeName": [], "codebaseId": "", "mentionContextCount": 0,
                "command": "", "expertId": "", "recommendId": "", "skillId": "",
                "skillCount": 0, "totalCount": 0, "fileUri": "", "presentAt": now,
                "traceId": "", "rootRequestId": cid, "parentConversationId": cid,
                "agentName": "default", "agentType": "conversation", "userId": uid,
            })
        }
        _ => json!({ "eventCode": "unknown", "userId": uid }),
    }
}

/// 桌面指纹头（2api `_desktop_headers` 同款；桌面链必须走 copilot 域）。
fn desktop_report_headers(uid: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_string(), DESKTOP_UA.to_string()),
        ("X-Domain".to_string(), COPILOT_BASE.to_string()),
        ("X-Product".to_string(), "SaaS".to_string()),
        (
            "X-Request-ID".to_string(),
            format!("{}{}", derive_id(uid, "req"), now_ms() % 1_000_000),
        ),
        ("X-User-Id".to_string(), uid.to_string()),
    ]
}

/// web 域指纹头（2api `_web_event_headers` 同款；资料库事件专用）。
fn web_report_headers(uid: &str, page_url: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("x-client-platform".to_string(), "web".to_string()),
        ("Origin".to_string(), WEB_CLAIM_BASE.to_string()),
        ("Referer".to_string(), page_url.to_string()),
        ("User-Agent".to_string(), WEB_UA.to_string()),
        ("X-User-Id".to_string(), uid.to_string()),
        ("X-Domain".to_string(), WEB_CLAIM_BASE.to_string()),
    ]
}

/// 上报一批事件；返回业务码（0 = 成功）。
async fn report_events(
    account: &Value,
    base: &str,
    extra_headers: Vec<(String, String)>,
    events: Vec<Value>,
) -> i64 {
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("");
    let nick = account.get("name").and_then(Value::as_str).unwrap_or("");
    let fingerprint = desktop_fingerprint(uid, nick, now_ms());
    let wrapped: Vec<Value> = events
        .into_iter()
        .map(|mut event| {
            merge_fields(&mut event, &fingerprint);
            event
        })
        .collect();
    let resp = domain_request(
        base,
        REPORT_PATH,
        "POST",
        Some(Value::Array(wrapped)),
        Some(extra_headers),
        account,
    )
    .await;
    resp.get("code").and_then(Value::as_i64).unwrap_or(-1)
}

/// 桌面 6 连对话链（家族版 RichMeow：复用 school 链但**不带 activityId**）。
async fn report_richmeow_once(account: &Value, _idx: usize) -> i64 {
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("");
    let nick = account.get("name").and_then(Value::as_str).unwrap_or("");
    let now = now_ms();
    // conv/req/msg 一律 uuid（自造前缀是聚类特征，防检测纪律见模块头 13）。
    let conv = uuid_v4_simple();
    let req = uuid_v4_simple();
    let msg = uuid_v4_simple();
    let mut events = desktop_chat_sequence(uid, nick, &conv, &req, &msg, now);
    for event in &mut events {
        if let Some(map) = event.as_object_mut() {
            map.remove("activityId"); // 家族任务不带 activityId（与 school 域区分）
        }
    }
    report_events(account, COPILOT_BASE, desktop_report_headers(uid), events).await
}

/// Buddy 五连「进入应用」事件组（2api `desktop_buddy5_sequence` 同款）。
fn buddy5_sequence(buddy_id: &str, buddy_name: &str) -> Vec<Value> {
    vec![
        json!({"eventCode": "buddyapp_discover_click", "mode": "LOCAL", "buddyId": buddy_id, "buddyName": buddy_name}),
        json!({"eventCode": "buddyapp_show", "mode": "LOCAL", "buddyId": buddy_id, "buddyName": buddy_name, "elementId": buddy_id, "elementName": buddy_name, "position": 2}),
        json!({"eventCode": "buddyapp_enter_click", "mode": "LOCAL", "buddyId": buddy_id, "buddyName": buddy_name, "elementId": buddy_id, "elementName": buddy_name, "position": 2, "isFirstPage": "1"}),
        json!({"eventCode": "buddyapp_auth_confirm_click", "mode": "LOCAL", "buddyId": buddy_id, "buddyName": buddy_name, "elementId": buddy_id, "elementName": buddy_name}),
        json!({"eventCode": "buddyapp_bindaccount_skip_click", "mode": "LOCAL", "buddyId": buddy_id, "buddyName": buddy_name, "elementId": buddy_id, "elementName": buddy_name}),
    ]
}

/// web 域资料库点击事件（点亮 `Library_read`；2api `report_web_event` 同款形状）。
async fn report_library_once(account: &Value) -> i64 {
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("");
    let nick = account.get("name").and_then(Value::as_str).unwrap_or("");
    let event = json!({
        "eventCode": "web_element_click", "timestamp": now_ms(), "reportDelay": 0,
        "pageURL": LIBRARY_DOC_URL, "elementId": "library_doc_intro_click",
        "elementName": "WorkBuddy资料库介绍",
        "os": "Win32", "arch": "", "osVersion": "10.0", "userAgent": WEB_UA,
        "machineId": derive_id(uid, "webmachine"), "userId": uid, "userNickname": nick,
    });
    let resp = domain_request(
        WEB_CLAIM_BASE,
        REPORT_PATH,
        "POST",
        Some(json!([event])),
        Some(web_report_headers(uid, LIBRARY_DOC_URL)),
        account,
    )
    .await;
    resp.get("code").and_then(Value::as_i64).unwrap_or(-1)
}

/// 单个家族任务的补齐流程：accept（若未接）→ 补上报 → 回读 → 达标即 claim。
/// 返回该任务的结果摘要；`claimed=true` 表示本轮真的入账了。
async fn run_family_task(account: &Value, code: &str, kind: &str, task: &Value) -> Value {
    let mut outcome = json!({ "code": code, "kind": kind });
    let accept = accept_status(Some(task));
    if accept == "claimed" {
        outcome["status"] = json!("already");
        return outcome;
    }
    if accept == "not_accepted" && !accept_with_verify(account, code).await {
        outcome["status"] = json!("accept-failed");
        return outcome;
    }
    // accept 后服务端才填 progress（accept 前恒为 null），重读一次拿真实 (cur, target)
    let task = if accept == "not_accepted" {
        let tasks = list_growth_tasks(account).await.unwrap_or_default();
        find_task(&tasks, code).cloned().unwrap_or_else(|| task.clone())
    } else {
        task.clone()
    };
    let fallback = FAMILY_TASKS
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, _, target)| *target)
        .unwrap_or(0);
    let (cur, target) = task_progress(Some(&task), fallback);
    if target == 0 {
        outcome["status"] = json!("no-target");
        return outcome;
    }
    let need = target.saturating_sub(cur);
    outcome["need"] = json!(need);
    let uid = account.get("uid").and_then(Value::as_str).unwrap_or("");
    let mut reported = 0i64;
    if need > 0 {
        match kind {
            "library" => {
                for _ in 0..need {
                    let result = report_library_once(account).await;
                    reported += (result == 0) as i64;
                    write_gap().await;
                }
            }
            "richmeow" => {
                for index in 0..need {
                    let result = report_richmeow_once(account, index as usize).await;
                    reported += (result == 0) as i64;
                    write_gap().await;
                }
            }
            "buddy5" => {
                let events = buddy5_sequence(BUDDY_APP_ID, BUDDY_APP_NAME);
                for _ in 0..need {
                    let result =
                        report_events(account, COPILOT_BASE, desktop_report_headers(uid), events.clone()).await;
                    reported += (result == 0) as i64;
                    write_gap().await;
                }
            }
            "buddyfirst" => {
                // 领养链：report 前置解锁 → agreement → buddy/first（直给积分即完成）
                let unlock = json!({ "eventCode": "chat_request_send", "timestamp": now_ms(),
                    "reportDelay": 0, "mode": "craft", "conversationId": uuid_v4_simple(),
                    "inputLength": 20 + now_ms() % 180, "requestModelId": "deepseek-v4-flash",
                    "requestModelName": "DeepSeek V4 Flash", "userId": uid });
                let _ = report_events(account, CHAT_BASE, Vec::new(), vec![unlock]).await;
                write_gap().await;
                let _ = domain_request(CHAT_BASE, PATH_BUDDY_AGREEMENT, "POST",
                    Some(json!({ "agree": true })), None, account).await;
                write_gap().await;
                let resp = domain_request(CHAT_BASE, PATH_BUDDY_FIRST, "POST",
                    Some(json!({})), None, account).await;
                let ok = resp.get("code").and_then(Value::as_i64) == Some(0);
                outcome["buddy_first"] = json!(if ok { "granted" } else { "rejected" });
                reported = ok as i64;
            }
            _ => {
                // 纯上报类：按 kind 取对象 id，偏移 cur 避免复用
                let ids: IdList = match kind {
                    "canvas" => (0..need)
                        .map(|index| (format!("wbx-canvas-{}", now_ms() + index), String::new()))
                        .collect(),
                    "template" => dedup_slice(fetch_scenes(account).await, cur as usize, need as usize),
                    "expert" => dedup_slice(fetch_market_experts(account, false, None).await, cur as usize, need as usize),
                    "team" => dedup_slice(fetch_market_experts(account, true, None).await, cur as usize, need as usize),
                    "skill" => dedup_slice(fetch_skills(account).await, cur as usize, need as usize),
                    "playbook" => dedup_slice(fetch_playbook_cases().await, cur as usize, need as usize),
                    "lighthouse" => dedup_slice(fetch_lighthouse(account).await, cur as usize, need as usize),
                    "skin" => dedup_slice(fetch_pe_theme(account).await, cur as usize, need as usize),
                    // chat / glmchat / automation 无对象 id，空 meta 直报
                    _ => (0..need).map(|_| (String::new(), String::new())).collect(),
                };
                if ids.len() < need as usize {
                    outcome["status"] = json!("no-ids");
                    return outcome;
                }
                for (index, (obj_id, meta)) in ids.iter().enumerate() {
                    let event = build_family_event(uid, kind, obj_id, meta, index);
                    let resp = domain_request(CHAT_BASE, REPORT_PATH, "POST",
                        Some(json!([event])), None, account).await;
                    reported += (resp.get("code").and_then(Value::as_i64) == Some(0)) as i64;
                    write_gap().await;
                }
            }
        }
        // 服务端归账可能异步（2api 实测口径）
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    outcome["reported"] = json!(reported);
    // 回读确认；达标且未入账即 claim
    let tasks = list_growth_tasks(account).await.unwrap_or_default();
    let task2 = find_task(&tasks, code);
    let accept2 = accept_status(task2);
    let (cur2, target2) = task_progress(task2, target);
    outcome["progress"] = json!(format!("{cur2}/{target2}"));
    if accept2 == "claimed" {
        outcome["status"] = json!("already");
    } else if cur2 >= target2 && target2 > 0 {
        match claim_growth_task(account, code).await {
            Ok(_) => {
                outcome["status"] = json!("claimed");
            }
            Err(error) => {
                outcome["status"] = json!("claim-failed");
                outcome["error"] = json!(error);
            }
        }
    } else {
        outcome["status"] = json!("incomplete");
    }
    outcome
}

/// 当日 error 熔断阈值：同一任务当日连错达此数即当日跳过（缓存跨日滚动自动复位）。
/// 防的是对真失败（非 locked）无限重试——每 30 分钟一轮打同一个洞。
const TASK_ERROR_BREAKER: i64 = 3;

/// 读当日缓存里该任务已累计的 error 连击数。
fn prior_error_count(prior_tasks: &Value, code: &str) -> i64 {
    prior_tasks
        .pointer(&format!("/{code}/errors"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// mp 阶梯 progress 结果是否「今日已无可为」（计 parked）。
/// 阶梯 1 次/天：current>0 = 本轮已推进，剩余次数次日解锁 ⇒ chip 应转完成态；
/// current==0 = 真没成。silentDrop（上报被服务端丢弃）的 parked 判定在
/// run_task_family 的 progress 分支单独处理（09-29 口径：不保留 outstanding）。
fn ladder_progress_parked(outcome: &Value) -> bool {
    outcome.get("current").and_then(Value::as_i64).unwrap_or(0) > 0
}

/// 跑单账号任务家族。返回结果摘要（status: done / partial / error）。
/// `prior_tasks` = 当日缓存 family.tasks，供 error 熔断计数；首轮传空对象。
pub async fn run_task_family(account: &Value, prior_tasks: &Value) -> Value {
    let tasks = match list_growth_tasks(account).await {
        Ok(tasks) => tasks,
        Err(error) => {
            return json!({ "status": "error", "error": error });
        }
    };
    let mut summary = json!({});
    let mut claimed = 0i64;
    let mut already = 0i64;
    let mut failed = 0i64;
    let mut total = 0i64;
    // parked = 「今日无可为」的项：阶梯冷却锁（次日解锁）+ error 当日熔断 + 阶梯 progress
    // 本轮已推进（current>0，阶梯 1 次/天，剩余次日解锁）。
    // 不从 total/outstanding 里摘掉的话，chip 会天天挂「还有 1 项待领」不转完成态
    //（09-26 主人口径：今天能领的都领完了就该显示完成）。
    let mut parked = 0i64;
    let mut kind = Value::Null;
    // 每号随机洗牌执行顺序：多账号若按同一表序连发，行为序列雷同易被聚类关联（防检测审计）。
    // 无 rand 依赖（checkin 同款约束），用 FNV(uid) ⊕ now_ms 做 LCG 种子足够打散。
    let mut order: Vec<usize> = (0..FAMILY_TASKS.len()).collect();
    let mut seed = now_ms() as u64 ^ {
        let mut h = 0xcbf29ce484222325u64;
        for b in account_key(account).bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        h
    };
    for i in (1..order.len()).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        order.swap(i, j);
    }
    for &idx in &order {
        let (code, family_kind, _) = FAMILY_TASKS[idx];
        let Some(task) = find_task(&tasks, code) else {
            continue; // 服务端未下发，跳过
        };
        total += 1;
        // 周期字段取服务端第一个命中任务（家族实测全 single，但不硬编码）
        if kind.is_null() {
            kind = task_type_of(Some(task));
        }
        // 当日熔断：同任务 error 连击达阈值即当日跳过，不再发任何请求
        let errs = prior_error_count(prior_tasks, code);
        if errs >= TASK_ERROR_BREAKER {
            parked += 1;
            summary[code.to_string()] =
                json!({"status": "skipped", "reason": "error-breaker", "errors": errs});
            continue;
        }
        let mut outcome = run_family_task(account, code, family_kind, task).await;
        write_gap().await; // 任务间隔频控（accept/claim 密集，防整批被限流）
        let status = outcome
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        match status.as_str() {
            "claimed" => claimed += 1,
            "already" => already += 1,
            "error" => {
                failed += 1;
                outcome["errors"] = json!(errs + 1); // 留给下轮熔断计数
            }
            _ => failed += 1,
        }
        summary[code.to_string()] = outcome;
    }
    // ---- mp 口径 Sequential 阶梯链（小程序限定，web 列表不可见；2api 只做了 _1）----
    // 阶梯解锁：领了 _N 才下发 _N+1，故每天自动接新解锁的级。mp 拉取失败不拖垮 web 段。
    if let Ok(mp_tasks) = list_growth_tasks_mp(account).await {
        for task in &mp_tasks {
            let Some(code) = task.get("task_code").and_then(Value::as_str) else {
                continue;
            };
            if !code.starts_with("Sequential_Tasks_") {
                continue;
            }
            total += 1;
            if kind.is_null() {
                kind = task_type_of(Some(task));
            }
            let errs = prior_error_count(prior_tasks, code);
            if errs >= TASK_ERROR_BREAKER {
                parked += 1;
                summary[code.to_string()] =
                    json!({"status": "skipped", "reason": "error-breaker", "errors": errs});
                continue;
            }
            let mut outcome = run_mp_task(account, code, task).await;
            write_gap().await;
            let status = outcome
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            match status.as_str() {
                "claimed" => claimed += 1,
                "done" => already += 1,
                "error" => {
                    failed += 1;
                    outcome["errors"] = json!(errs + 1); // 留给下轮熔断计数
                }
                "skipped" => {
                    // 阶梯冷却锁：今日无可为，不算待领也不算失败
                    if outcome
                        .get("reason")
                        .and_then(Value::as_str)
                        .is_some_and(|r| r.starts_with("locked-until-"))
                    {
                        parked += 1;
                    }
                }
                "progress" => {
                    // 09-27 口径：current>0 = 本轮已推进（阶梯 1 次/天，剩余次日解锁）
                    // ⇒ parked，chip 转完成态。
                    // 09-29 主人反馈「还有 1 项待领」天天挂 ⇒ silentDrop（上报被服务端
                    // 静默丢弃，当日门控短路不可重试、同形状次日重发也无效）改计 parked，
                    // 不再保留 outstanding 盯重试——真修复靠判据（_5 已实证 modelId 缺失）。
                    if ladder_progress_parked(&outcome)
                        || outcome.get("silentDrop").and_then(Value::as_bool) == Some(true)
                    {
                        parked += 1;
                    }
                }
                _ => {} // 未知状态不 parked，保留 outstanding 供排查
            }
            summary[code.to_string()] = outcome;
        }
    }
    // total/outstanding 均为「今日可动作」口径：parked（次日解锁/当日熔断/阶梯已推进）摘出去，
    // 今日能领的全领完时前端才能转完成态；tasks 明细仍保留 locked 条目供排查。
    let actionable = (total - parked).max(0);
    let outstanding = (actionable - claimed - already).max(0);
    json!({
        "status": if failed > 0 { "partial" } else { "done" },
        "claimed": claimed,
        "already": already,
        "failed": failed,
        "total": actionable,
        "outstanding": outstanding,
        "parked": parked,
        "taskType": kind,
        "tasks": summary,
    })
}

fn uuid_v4_simple() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

// ---------------------------------------------------------------------------
// 全账号轮（配置门控 / 当日幂等 / 缓存）
// ---------------------------------------------------------------------------

/// 缓存里某账号的当日结果。
fn cache_result(cache: &Value, account_id: &str) -> Value {
    cache
        .get("results")
        .and_then(|results| results.get(account_id))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// 写入某账号的当日结果；跨自然日先滚动缓存（与活跃地图同款）。
fn write_cache_result(account_id: &str, result: &Value, today: &str) {
    with_growth_tasks_cache_lock(|| {
        let mut cache = load_growth_tasks_cache();
        if cache.get("date").and_then(Value::as_str) != Some(today) {
            cache = json!({ "date": today, "results": {} });
        }
        if !cache.get("results").is_some_and(Value::is_object) {
            cache["results"] = json!({});
        }
        let mut entry = cache_result(&cache, account_id);
        if let Some(field) = result.as_object() {
            for (key, value) in field {
                entry[key] = value.clone();
            }
        }
        cache["results"][account_id] = entry;
        if let Err(error) = save_growth_tasks_cache(&cache) {
            eprintln!("[成长任务] 缓存写入失败: {error}");
        }
    });
}

/// 成长任务到点判定：总开关开 且（开学季到 hour 或 夜猫窗口内）。
/// 夜猫子与开学季共用一个 cycle，但各自的门控独立：
/// 开学季按 `schoolHour`，夜猫子按夜猫窗口（23–08）。
pub fn tasks_due_now(config: &Value, cache: &Value, today: &str, now_hour: u32) -> bool {
    if config.get("enabled").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    // 当日已有任一任务办妥即不再自动触发（force 手动仍可重跑）
    if cache.get("date").and_then(Value::as_str) != Some(today) {
        return true; // 新的一天：只要在任一窗口/时点就办
    }
    let school_hour = config
        .get("schoolHour")
        .and_then(Value::as_i64)
        .unwrap_or(TASKS_DEFAULT_SCHOOL_HOUR) as u32;
    if now_hour >= school_hour {
        return true;
    }
    within_night_window(now_hour)
}

/// 段级当日门控：缓存里该段 status=done 即当日跳过。段值是对象——早期误用
/// `as_str` 直接比，对对象恒取 None ⇒ 门控**从未生效过**（每段每轮空转，09-26 修复）。
fn section_prior_done(prior: &Value, key: &str) -> bool {
    prior
        .pointer(&format!("/{key}/status"))
        .and_then(Value::as_str)
        == Some("done")
}

/// 跑单账号（black_cat + school + family 三段独立，互不影响）。
/// `force=true` 无视当日门控全量重跑（「立即执行」/排障 `?force=1` 语义）。
pub async fn run_tasks_for_account(account: &Value, prior: &Value, force: bool) -> Value {
    let mut result = json!({});
    if force || !section_prior_done(prior, "black_cat") {
        result["black_cat"] = run_black_cat(account).await;
    }
    if force || !section_prior_done(prior, "school") {
        result["school"] = run_school(account).await;
    }
    // 任务家族（纯上报/领取类一次性任务）：当日已 done 则跳过；error 段由
    // run_task_family 内的当日熔断兜底，不会无限重试
    let prior_family_tasks = prior
        .pointer("/family/tasks")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if force || !section_prior_done(prior, "family") {
        result["family"] = run_task_family(account, &prior_family_tasks).await;
    }
    result
}

/// 跑一轮成长任务（`force = true` 跳过开关/门控/当日已办，供「立即执行」）。
pub async fn run_tasks_cycle(force: bool) -> Value {
    let config = load_tasks_config();
    if !force && config.get("enabled").and_then(Value::as_bool) != Some(true) {
        return json!({"status": "skipped", "reason": "disabled"});
    }
    let today = cst_today();
    if !force {
        let cache = load_growth_tasks_cache();
        if !tasks_due_now(&config, &cache, &today, cst_now().hour()) {
            return json!({"status": "skipped", "reason": "not-due"});
        }
    }
    let Some(_guard) = RunFlagGuard::try_acquire(&TASKS_RUNNING) else {
        return json!({"status": "skipped", "reason": "running"});
    };

    let accounts = tasks_capable_accounts(load_accounts());
    // 账号间顺序也洗牌（号内任务已洗牌，号间固定序同样易被聚类关联，09-26 加固）
    let mut order: Vec<usize> = (0..accounts.len()).collect();
    let mut seed = now_ms() as u64;
    for i in (1..order.len()).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        order.swap(i, j);
    }
    let mut results = Vec::new();
    for &idx in &order {
        let account = accounts[idx].clone();
        let key = account_key(&account);
        let prior = with_growth_tasks_cache_lock(|| {
            let cache = load_growth_tasks_cache();
            let fresh = cache.get("date").and_then(Value::as_str) == Some(today.as_str());
            if fresh {
                cache_result(&cache, &key)
            } else {
                json!({})
            }
        });
        let outcome = run_tasks_for_account(&account, &prior, force).await;
        // ⛔ 别在外面再套一层 `with_growth_tasks_cache_lock`：write_cache_result 内部已加锁，
        // 而该锁是 std Mutex（**不可重入**），同线程二次 lock = 永久死锁（09-25 实证：cycle
        // 挂在第一个账号写盘处，缓存从不落盘 ⇒ 前端 chip 只剩活跃地图一段）。
        write_cache_result(&key, &outcome, &today);
        results.push(json!({
            "id": key,
            "name": account_display_name(&account),
            "result": outcome,
        }));
    }
    json!({
        "status": "ok",
        "date": today,
        "total": results.len(),
        "results": results,
    })
}

/// 单账号展示数据（当日无记录返回 `status: "pending"`）。
///
/// 三段并列下发：`blackCat`（夜猫子）/ `school`（活动任务）/ `family`（任务家族）。
/// 前端 chip 按段渲染，缺任一段该段即不显示 —— 早期版本漏发 `family`，导致
/// 「任务家族」段永远空白，改这里时别再漏。
pub fn tasks_display(account_id: &str) -> Value {
    tasks_display_at(account_id, &growth_tasks_cache_file())
}

/// 同 `tasks_display`，缓存路径可注入（单测试净室用，见坑 73）。
pub fn tasks_display_at(account_id: &str, cache_path: &Path) -> Value {
    let cache = load_growth_tasks_cache_at(cache_path);
    let today = cst_today();
    let fresh = cache.get("date").and_then(Value::as_str) == Some(today.as_str());
    let entry = if fresh {
        cache
            .get("results")
            .and_then(|results| results.get(account_id))
            .cloned()
            .unwrap_or_else(|| json!({}))
    } else {
        json!({})
    };
    let black_cat = entry
        .get("black_cat")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let school = entry.get("school").cloned().unwrap_or_else(|| json!({}));
    // 任务家族（纯上报/领取类一次性任务）：与夜猫子/活动任务平行的第三段，前端据此渲染。
    let family = entry.get("family").cloned().unwrap_or_else(|| json!({}));
    let black_cat_status = black_cat
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let school_status = school
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let family_status = family
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    json!({
        "date": if fresh { json!(today) } else { Value::Null },
        "blackCat": black_cat,
        "school": school,
        "family": family,
        "status": if black_cat_status != "pending"
            || school_status != "pending"
            || family_status != "pending" { "done" } else { "pending" },
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_window_matches_2api_semantics() {
        // 23:00–08:00 CST 含边界：23 点与 0-7 点在窗口内，8-22 点不在
        assert!(within_night_window(23));
        assert!(within_night_window(0));
        assert!(within_night_window(7));
        assert!(!within_night_window(8));
        assert!(!within_night_window(12));
        assert!(!within_night_window(22));
    }

    #[test]
    fn night_event_is_single_chat_request_send_with_uid() {
        let event = night_chat_event("uid-x", "conv-1", 1_700_000_000_000);
        assert_eq!(event["eventCode"], "chat_request_send");
        assert_eq!(event["mode"], "night");
        assert_eq!(event["requestModelId"], "glm-5.2");
        assert_eq!(event["userId"], "uid-x");
        assert_eq!(event["conversationId"], "conv-1");
        // 关键计数/时间字段存在（上游加严校验的兜底）
        assert!(event["maxSteps"].is_i64());
        assert!(event["presentAt"].is_i64());
    }

    #[test]
    fn mini_chat_event_carries_activity_id_and_mp_fingerprint() {
        let event = mini_chat_event("uid-x", "昵称", "conv-2", 1_700_000_000_000);
        assert_eq!(event["activityId"], "school_open_day_2026");
        assert_eq!(event["source"], "mini_program");
        assert_eq!(event["extName"], "workbuddy-mp");
        assert_eq!(event["ideName"], "wx_app_cloud");
        assert_eq!(event["userId"], "uid-x");
    }

    #[test]
    fn desktop_sequence_has_six_events_all_with_activity_id() {
        let events = desktop_chat_sequence("uid-x", "n", "c1", "r1", "m1", 1_700_000_000_000);
        assert_eq!(events.len(), 6);
        let codes: Vec<&str> = events
            .iter()
            .map(|event| event["eventCode"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            codes,
            vec![
                "agent_task_created",
                "chat_message_send",
                "chat_request_send",
                "chat_message_response",
                "chat_message_status",
                "chat_request_response",
            ]
        );
        for event in &events {
            assert_eq!(event["activityId"], "school_open_day_2026");
            assert_eq!(event["ideName"], "WorkBuddy");
            assert_eq!(event["extName"], "workbuddy-desktop");
            assert_eq!(event["userId"], "uid-x");
        }
    }

    #[test]
    fn derived_ids_are_stable_for_same_uid() {
        // 指纹必须稳定派生（服务端按指纹聚合行为序列），同 uid 同盐两次一致
        assert_eq!(derive_id("uid-x", "machine"), derive_id("uid-x", "machine"));
        assert_ne!(derive_id("uid-x", "machine"), derive_id("uid-y", "machine"));
        assert_ne!(derive_id("uid-x", "machine"), derive_id("uid-x", "session"));
    }

    #[test]
    fn family_task_codes_are_unique_and_skip_unforgeable() {
        let mut seen = std::collections::HashSet::new();
        for (code, _kind, _) in FAMILY_TASKS {
            assert!(seen.insert(*code), "重复 task_code: {code}");
        }
        // 不可伪造的三类绝不进家族表
        assert!(!seen.contains("Expert_Philanthropy"));
        assert!(!seen.contains("wb_wechat_oa_subscribe_task"));
        assert!(!seen.contains("task_student_verify"));
    }

    #[test]
    fn family_events_carry_uid_and_event_code() {
        let event = build_family_event("uid-abc", "canvas", "wbx-canvas-1", "", 0);
        assert_eq!(event["eventCode"], "wbx_design_canvas_task_create");
        assert_eq!(event["userId"], "uid-abc");
        assert_eq!(event["id"], "wbx-canvas-1");
        let glm = build_family_event("uid-abc", "glmchat", "", "", 0);
        assert_eq!(glm["eventCode"], "chat_request_send");
        assert_eq!(glm["requestModelId"], "glm-5.2");
        // 与夜猫子区分：家族 chat 是 craft 模式
        assert_eq!(glm["mode"], "craft");
        let chat = build_family_event("uid-abc", "chat", "", "", 0);
        assert_eq!(chat["requestModelId"], "deepseek-v4-flash");
    }

    #[test]
    fn buddy5_sequence_has_five_steps_with_app_id() {
        let events = buddy5_sequence("cb_x", "测试应用");
        assert_eq!(events.len(), 5);
        assert_eq!(events[0]["eventCode"], "buddyapp_discover_click");
        assert_eq!(events[4]["eventCode"], "buddyapp_bindaccount_skip_click");
        for event in &events {
            assert_eq!(event["buddyId"], "cb_x");
        }
    }

    #[test]
    fn tasks_due_respects_enabled_and_windows() {
        let config = json!({"enabled": true, "schoolHour": 12});
        let stale = json!({"date": "2026-09-24", "results": {}});
        let fresh = json!({"date": "2026-09-25", "results": {"a": {"black_cat": {"status": "done"}}}});
        // 总开关关：一律不到点
        assert!(!tasks_due_now(
            &json!({"enabled": false, "schoolHour": 12}),
            &stale,
            "2026-09-25",
            12
        ));
        // 跨日：任何小时都到点（窗口内/外都能跑，school 有自己的时点语义交给上层）
        assert!(tasks_due_now(&config, &stale, "2026-09-25", 9));
        // 同日：夜猫窗口内到点
        assert!(tasks_due_now(&config, &fresh, "2026-09-25", 23));
        assert!(tasks_due_now(&config, &fresh, "2026-09-25", 7));
        // 同日：过了 schoolHour 到点
        assert!(tasks_due_now(&config, &fresh, "2026-09-25", 12));
        // 同日：非窗口且未到 schoolHour 不到点
        assert!(!tasks_due_now(&config, &fresh, "2026-09-25", 9));
        assert!(!tasks_due_now(&config, &fresh, "2026-09-25", 11));
    }

    #[test]
    fn school_progress_reads_flat_fields() {
        let task = json!({"progress": 2, "target_count": 3});
        assert_eq!(school_progress(&task), (2, 3));
        // 缺失回落 (0, 1)
        assert_eq!(school_progress(&json!({})), (0, 1));
    }

    #[test]
    fn task_progress_prefers_server_target() {
        let task = json!({"accept_status": "in_progress", "progress": {"current": 1, "target": 3}});
        assert_eq!(task_progress(Some(&task), 5), (1, 3));
        // 服务端没给 target 时用兜底
        let bare = json!({"accept_status": "not_accepted"});
        assert_eq!(task_progress(Some(&bare), 3), (0, 3));
    }

    #[test]
    fn ladder_progress_parked_only_when_advanced() {
        // 09-27 口径：本轮已推进（current>0）⇒ parked；未点亮（current==0）⇒ 保留 outstanding
        let advanced = json!({"status": "progress", "current": 1, "target": 5});
        assert!(ladder_progress_parked(&advanced));
        let silent = json!({"status": "progress", "current": 0, "target": 1, "silentDrop": true});
        assert!(!ladder_progress_parked(&silent));
        let bare = json!({"status": "progress"});
        assert!(!ladder_progress_parked(&bare));
    }

    #[test]
    fn task_kinds_split_by_server_task_type() {
        // 周期判定只读服务端字段（坑 78）：不靠 target 猜，未知原样返回 Null。
        let recurring = json!({"task_code": "chat_3_times", "task_type": "recurring"});
        let single = json!({"task_code": "black_cat", "task_type": "single"});
        let bare = json!({"task_code": "x"});
        assert_eq!(task_type_of(Some(&recurring)), "recurring");
        assert_eq!(task_type_of(Some(&single)), "single");
        assert!(task_type_of(Some(&bare)).is_null());
        assert!(task_type_of(None).is_null());
        // 首期开学季实测：3 项每日 + 2 项一次性
        let tasks = vec![
            json!({"task_type": "recurring"}), json!({"task_type": "recurring"}),
            json!({"task_type": "recurring"}), json!({"task_type": "single"}),
            json!({"task_type": "single"}),
        ];
        assert_eq!(count_kinds(&tasks), (3, 2));
    }

    #[test]
    fn tasks_display_handles_stale_and_missing() {
        // 空缓存 → pending
        let value = tasks_display("nonexistent");
        assert_eq!(value["status"], "pending");
    }

    #[test]
    fn mp_event_kind_maps_by_code_and_event_shape() {
        // mp 阶梯链判据映射：_2=专家（desc「选中专家并完成有效对话」），
        // _4=创建定时任务（desc 例句「提醒我每天 7 点钟读书」），
        // _5=GLM5.2 对话（09-29 抓 desc「在小程序内选择 GLM5.2 模型并完成有效对话」），
        // 其余=mini 对话。
        assert_eq!(mp_event_kind("Sequential_Tasks_1"), "mp_chat");
        assert_eq!(mp_event_kind("Sequential_Tasks_2"), "mp_expert");
        assert_eq!(mp_event_kind("Sequential_Tasks_3"), "mp_chat");
        assert_eq!(mp_event_kind("Sequential_Tasks_4"), "mp_automation");
        assert_eq!(mp_event_kind("Sequential_Tasks_5"), "mp_chat_glm");
        // _5 判据事件：mp_chat 骨架 + modelId（缺 modelId 实测被服务端静默丢弃）
        let glm = mp_chat_glm_event("uid-x", 1_790_335_871_000);
        assert_eq!(glm["eventCode"], "chat_request_send");
        assert_eq!(glm["source"], "mini_program");
        assert_eq!(glm["modelId"], "glm-5.2");
        assert_eq!(glm["modelIsThinking"], false);
        // _4 判据事件形状：automated_task_create_suc 的 mini 变体（desc 例句 + 定时计划）
        let auto = mp_automation_event("uid-x", 1_790_335_871_000);
        assert_eq!(auto["eventCode"], "automated_task_create_suc");
        assert_eq!(auto["source"], "mini_program");
        assert_eq!(auto["scheduleType"], "recurring");
        assert!(auto["prompt"].as_str().unwrap().contains("读书"));
        assert!(auto["schedule"]["rrule"].as_str().unwrap().contains("FREQ=DAILY"));
        // mp chat 事件形状（2api 实测 0ceb9c7c 点亮）：mini 指纹 + 无 activityId + uuid 会话。
        let event = mp_chat_event("uid-x", 1_790_335_871_000);
        assert_eq!(event["eventCode"], "chat_request_send");
        assert_eq!(event["source"], "mini_program");
        assert_eq!(event["ideType"], "WorkBuddy_MP");
        assert!(event.get("activityId").is_none());
        assert_ne!(event["conversationId"], event["userId"]);
        assert!(event["conversationId"].as_str().unwrap().len() >= 32);
        // inputLength 抖动（20~199），非恒定
        let l1 = event["inputLength"].as_i64().unwrap();
        let l2 = mp_chat_event("uid-x", 1_790_335_872_500)["inputLength"].as_i64().unwrap();
        assert!((20..=199).contains(&l1) && (20..=199).contains(&l2));
    }

    #[test]
    fn section_gate_reads_object_status_and_breaker_counts_errors() {
        // 段级门控读对象里的 status（早期 as_str 直比对对象恒 None ⇒ 门控从未生效）
        let prior = json!({"family": {"status": "done"}, "school": {"status": "partial"}});
        assert!(section_prior_done(&prior, "family"));
        assert!(!section_prior_done(&prior, "school"));
        assert!(!section_prior_done(&json!({}), "family"));
        // 当日熔断计数：嵌套 errors 字段，缺省 0，达阈值前不熔断
        let tasks = json!({"Sequential_Tasks_3": {"status": "error", "errors": 2}});
        assert_eq!(prior_error_count(&tasks, "Sequential_Tasks_3"), 2);
        assert_eq!(prior_error_count(&tasks, "Sequential_Tasks_4"), 0);
        assert_eq!(prior_error_count(&json!({}), "x"), 0);
        assert!(prior_error_count(&tasks, "Sequential_Tasks_3") < TASK_ERROR_BREAKER);
    }

    #[test]
    fn tasks_display_emits_family_section() {
        // 仅测试用到写盘注入版（放这里避免 lib 构建报 unused import）。
        use super::super::config::save_growth_tasks_cache_at;
        // 任务家族段必须与夜猫子/活动任务并列下发，否则前端「任务家族」永远空白。
        let dir = std::env::temp_dir().join(format!("wb-tasks-display-{}", uuid_v4_simple()));
        std::fs::create_dir_all(&dir).expect("临时目录可建");
        let path = dir.join("growth_tasks_cache.json");
        let cache = json!({
            "date": cst_today(),
            "results": {
                "acct-1": {
                    "black_cat": {"status": "done"},
                    "school": {"status": "skipped", "reason": "out-of-period"},
                    "family": {"status": "done", "claimed": 12, "failed": 0},
                }
            }
        });
        save_growth_tasks_cache_at(&path, &cache).expect("缓存可写");

        let value = tasks_display_at("acct-1", &path);
        assert_eq!(value["status"], "done");
        assert_eq!(value["family"]["status"], "done");
        assert_eq!(value["family"]["claimed"], 12);
        assert_eq!(value["blackCat"]["status"], "done");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
