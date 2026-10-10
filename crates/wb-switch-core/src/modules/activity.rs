//! 活跃地图：对话活跃上报（点亮连登）+ 连登管家（补签保连登 / 档位兑换 / 抽奖 / 礼包补偿）。
//!
//! 接口规格与编排语义移植自 2api 仓已跑通实现（`internal/upstream/report.go` ·
//! `internal/upstream/growth_bonus.go` · `internal/upstream/growth_reward.go` ·
//! `internal/scheduler/scheduler.go` 的 `runActivity` / `claimGrowthRewards` /
//! `makeupYesterday`，实测报告 `REPORT-active-map.md`）。本项目用 Rust 重写为
//! 「单账号全链 + 全账号轮」两段，与 `travel.rs` 同构（账号过滤 → 逐号执行 → 缓存滚动）。
//!
//! 照抄的六条实测结论（每条都踩过，别自行简化）：
//!
//! 1. **上报零额度** —— `/v2/report` 是客户端事件上报（`chat_request_send`），不走上游
//!    模型推理 ⇒ **不消耗积分**。请求体是「单元素数组」，必须带 `userId`（= 账号 uid）：
//!    缺 `userId` 时服务端 **200 但静默丢弃**。
//! 2. **200 ≠ 计分** —— 上报成功后必须回读 `streak` 自检（`days==0` 即疑似静默丢弃），
//!    否则会出现「日志全绿、连登没涨」的假成功。
//! 3. **每号每天一次** —— 风控口径：不做多时点高频上报；同一账号内多发时按
//!    `REPORT_GAP` 间隔，避免秒发触发风控。
//! 4. **国际版跳过** —— 由 `WbVariant::supports_activity()` 在发任何请求前短路。
//! 5. **补签固定盯昨日** —— 连登断档只可能发生在「上一个 CST 自然日」（今日尚未结算）；
//!    判据 = heatmap 昨日格 `score==0`（漏签）且 `makeup_cards.balance>0`（有卡）。
//! 6. **幂等键每次新生成** —— `redeem` / `draw` 的 `client_token` 必须新键；复用旧键会被
//!    上游按幂等去重静默吞掉本次领取（抽奖尤其明显）。

use chrono::{DateTime, FixedOffset, Local, Timelike, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::modules::account::{
    account_display_name, build_auth_headers, load_accounts, variant_of,
};
use crate::modules::config::{
    batch_gap, http_request, is_network_message, load_activity_cache, load_activity_config,
    load_checkin_config, now_ms, save_activity_cache, shuffle_by_seed, with_activity_cache_lock,
    RunFlagGuard, WORKBUDDY_API_ENDPOINT,
};
use crate::modules::refresh::{ensure_fresh_token, refresh_account_token};

static ACTIVITY_RUNNING: AtomicBool = AtomicBool::new(false);

/// 补轮周期：启动即跑一轮，之后每 30 分钟检查「到点且当日未办」。
pub const ACTIVITY_RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 同一账号内多条上报之间的间隔（避免秒发触发风控）。
const REPORT_GAP: Duration = Duration::from_millis(1500);

/// ★连登滞后补读：回读值低于预期时的等待时长（2026-10-04 实证）。
///
/// 背景：上报成功后**立刻**回读 `streak`，个别账号拿到的还是「今天尚未结算」的旧值
/// （实测 01:35 读到 3，几小时后真值已是 4）。当天 `status=done` 后日期闸不再重跑，
/// 于是这个偏低值被写进缓存、界面整天错着显示 —— 只能靠人工 `--fix-cache` 救。
/// 本补读把「上报 → 立刻回读」改成「上报 → 回读 → 低于预期则等一等再读，取最大值」。
///
/// 只补 **GET**，不再上报：风控口径是「每号每天一次上报」，读多少次都不违规。
const STREAK_RECHECK_DELAY: Duration = Duration::from_secs(5);

/// 补读最多几轮（含首读共 `1 + ROUNDS` 次请求，避免异常时无限重试）。
const STREAK_RECHECK_ROUNDS: usize = 2;

/// 单号每日上报条数上限（配置项 `report_count` 的钳制区间）。
const MAX_REPORT_COUNT: i64 = 10;

/// 单轮最多连抽次数（有一抽一，避免配置异常时死循环）。
const MAX_DRAW_PER_ROUND: usize = 10;

/// ★单账号全链硬超时（09-26 实证补的）。
///
/// 背景：`ACTIVITY_RUNNING` 是进程内 `AtomicBool` 守卫，**只在整轮跑完才释放**。
/// 若某账号的某个请求网络挂起（无超时保护）永不返回 ⇒ 守卫永久占用 ⇒
/// 之后每轮 cycle 都返回 `skipped/running` ⇒ 活跃地图静默停摆、连登悄悄断签
/// （09-26 实证：缓存停在 09-25 19:11，而 travel / growth_tasks 同期都在正常写盘）。
/// 本超时把「最坏情况」从「永久停摆」压成「单号最多卡 90s 后跳过，其余账号照跑」。
pub const ACCOUNT_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------------------
// 路径常量（全部挂在 WORKBUDDY_API_ENDPOINT = https://www.codebuddy.cn 下）
// ---------------------------------------------------------------------------

/// 对话活跃上报（billing 域，直接挂域名根）。
const REPORT_PATH: &str = "/v2/report";
/// 连登状态（连登天数 + 补登卡余额 + 各档兑换状态，一次读完）。
const STREAK_PATH: &str = "/activity/growth/streak";
/// 活跃地图热力格（一日一格，`score==0` 判漏签）。
const HEATMAP_PATH: &str = "/activity/growth/heatmap";
/// 补签卡使用（对指定 CST 自然日补签，保住连登连续天数）。
const MAKEUP_USE_PATH: &str = "/activity/growth/makeup-cards/use";
/// 连登档位兑换（里程碑制，非按天 claim）。
const REDEEM_PATH: &str = "/activity/growth/redeem";
/// 抽奖次数余额。
const LOTTERY_CHANCES_PATH: &str = "/activity/growth/lottery/chances";
/// 抽奖一次。
const LOTTERY_DRAW_PATH: &str = "/activity/growth/lottery/draw";
/// 新手礼包（每号一次，幂等写）。
const CLAIM_GIFT_PATH: &str = "/v2/billing/meter/claim-gift";
/// 活动补偿（有则领，幂等写）。
const CLAIM_COMPENSATION_PATH: &str = "/v2/billing/meter/claim-compensation";

/// 连登档位（天数升序）。与上游 `streak.redemption_status.tiers` 同构：
/// 7d 入门档 / 14d 进阶档 / 28d 巅峰档，同月每档各可领一次。
pub const TIER_DAYS: [(&str, i64); 3] = [("7d", 7), ("14d", 14), ("28d", 28)];

// ---------------------------------------------------------------------------
// 自然日口径（CST）
// ---------------------------------------------------------------------------

/// CST（Asia/Shanghai）固定 +8：上游自然日口径，不依赖本机时区设置。
fn cst_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("CST +8 恒为合法偏移")
}

fn cst_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&cst_offset())
}

/// 今日（CST 自然日，`YYYY-MM-DD`）。
pub fn cst_today() -> String {
    cst_now().format("%Y-%m-%d").to_string()
}

/// 昨日（CST 自然日）。
///
/// 必须先归一到 CST 再减一天：若按本地时区先做日历日减法，跨夏令时切换日会把瞬时点
/// 挪 1 小时、导致 CST 日期错位一天（补签会漏掉真实断档或补错日期）。
pub fn cst_yesterday() -> String {
    (cst_now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

// ---------------------------------------------------------------------------
// 连登状态解析（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 连登状态快照（`GET /activity/growth/streak` 的 `data` 段）。
#[derive(Debug, Clone, Default)]
pub struct StreakState {
    /// 当前连续活跃天数。
    pub days: i64,
    /// 补签卡可用余额。
    pub makeup_balance: i64,
    /// 补签卡持有上限。
    pub makeup_max: i64,
    /// 距下一档还差几天（上游口径，0 表示无下一档）。
    pub remaining_days: i64,
    /// 档位 → 状态（`available` 可领 / `claimed` 已领 / `locked` 未达标）。
    pub statuses: HashMap<String, String>,
}

impl StreakState {
    /// 档位状态；字段缺失时为空串（按「未领取」处理，由天数闸兜底）。
    pub fn status_of(&self, tier: &str) -> &str {
        self.statuses.get(tier).map(String::as_str).unwrap_or("")
    }

    /// 该档位本月是否已领。
    pub fn claimed(&self, tier: &str) -> bool {
        self.status_of(tier) == "claimed"
    }
}

/// 解析连登状态。
pub fn parse_streak_state(data: &Value) -> StreakState {
    let mut state = StreakState {
        days: data
            .pointer("/streak/days")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        makeup_balance: data
            .pointer("/makeup_cards/balance")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        makeup_max: data
            .pointer("/makeup_cards/max")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        remaining_days: data
            .pointer("/redemption_status/remaining_days")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        statuses: HashMap::new(),
    };
    for (tier, key) in [
        ("7d", "tier_7d_status"),
        ("14d", "tier_14d_status"),
        ("28d", "tier_28d_status"),
    ] {
        if let Some(value) = data
            .pointer(&format!("/redemption_status/{key}"))
            .and_then(Value::as_str)
        {
            state.statuses.insert(tier.to_string(), value.to_string());
        }
    }
    state
}

/// 挑选「已达标且本月未领」的最高档位；返回 `""` 表示无可领档（正常态，不写上游）。
///
/// 每日一轮只领一档：跨档连领（连登 28 天时 7d/14d 都没领）在上游是允许的，
/// 但同日多写对上游是多余压力，次日轮次会继续补领。
pub fn eligible_tier(days: i64, state: &StreakState) -> &'static str {
    for (tier, need) in TIER_DAYS.iter().rev() {
        if days >= *need && !state.claimed(tier) {
            return tier;
        }
    }
    ""
}

// ---------------------------------------------------------------------------
// 连登滞后补读（纯判定 + I/O 薄壳）
// ---------------------------------------------------------------------------

/// 整轮开跑前的连登天数快照（`account_id -> 昨日天数`）。
///
/// ⚠️ 必须在 `run_activity_cycle` 里**第一个账号写入之前**取：`write_cache_result`
/// 跨天时会把 `date` 盖成今天并清空 `results`，一旦跑起来就再也读不到昨天的值了。
pub fn yesterday_streak_days(snapshot: &Value, yesterday: &str) -> HashMap<String, i64> {
    if snapshot.get("date").and_then(Value::as_str) != Some(yesterday) {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    if let Some(results) = snapshot.get("results").and_then(Value::as_object) {
        for (id, value) in results {
            // 只认昨天真办成的：拿一份 error/skipped 的旧记录当基准会误判成滞后。
            if value.get("status").and_then(Value::as_str) != Some("done") {
                continue;
            }
            if let Some(days) = value.get("streakDays").and_then(Value::as_i64) {
                map.insert(id.clone(), days);
            }
        }
    }
    map
}

/// 今日预期连登天数 = 昨日天数 + 1；`None` 表示无判据（不补读）。
///
/// 无昨日记录（首日 / 中间空过整天）时**不猜**：那种情况下连登本就该从 1 重新数，
/// 硬拿「昨天 +1」去要求它，只会白白多等两轮。
pub fn streak_expected(yesterday: &HashMap<String, i64>, account_id: &str) -> Option<i64> {
    yesterday.get(account_id).map(|days| days + 1)
}

/// 是否需要补读：读到的值低于预期（含 0 —— 疑似静默丢弃）。无预期则不补。
pub fn streak_needs_recheck(read: i64, expected: Option<i64>) -> bool {
    match expected {
        Some(want) => read < want,
        None => false,
    }
}

/// 汇总「首读 + 各轮补读」：取最大值，并给出补读结论。
///
/// 取 max 而不是「最后一次」：结算完成只会往上涨，但万一上游抖动返回个更小的数，
/// 取 max 保证不会把好值覆盖成坏值。
///
/// 返回 `(最终天数, 状态)`：`none` 未补读 / `recovered` 补读追上了 /
/// `stale` 补读后仍低于预期（上游结算比本轮等待还慢，界面仍会偏低一天）。
pub fn streak_settle(first: i64, retries: &[i64], expected: Option<i64>) -> (i64, &'static str) {
    let best = retries.iter().fold(first, |acc, &x| acc.max(x));
    match expected {
        None => (best, "none"),
        Some(want) => {
            if retries.is_empty() {
                (best, if best >= want { "none" } else { "stale" })
            } else if best >= want {
                (best, "recovered")
            } else {
                (best, "stale")
            }
        }
    }
}

/// 读连登状态，必要时补读（纯 I/O 薄壳；判定全在上面的纯函数里，便于单测）。
async fn read_streak_settled(
    account: &Value,
    expected: Option<i64>,
) -> Result<(StreakState, &'static str), String> {
    let mut state = parse_streak_state(&activity_data(STREAK_PATH, "GET", None, account).await?);
    let mut retries: Vec<i64> = Vec::new();
    while retries.len() < STREAK_RECHECK_ROUNDS && streak_needs_recheck(state.days, expected) {
        tokio::time::sleep(STREAK_RECHECK_DELAY).await;
        match activity_data(STREAK_PATH, "GET", None, account).await {
            Ok(data) => {
                let next = parse_streak_state(&data);
                retries.push(next.days);
                if next.days > state.days {
                    state = next;
                }
            }
            // 补读失败不算主链路失败：首读已经成功，只是没追上预期值。
            Err(_) => break,
        }
    }
    let (days, status) = streak_settle(state.days, &retries, expected);
    state.days = days;
    Ok((state, status))
}

/// 热力格里指定日期（`YYYY-MM-DD`）的计分；无该日格返回 `None`（无判据，不动）。
pub fn heatmap_day_score(cells: &[Value], date: &str) -> Option<i64> {
    for cell in cells {
        let day = cell.get("date").and_then(Value::as_str).unwrap_or("");
        if day.len() >= 10 && &day[..10] == date {
            return Some(cell.get("score").and_then(Value::as_i64).unwrap_or(0));
        }
    }
    None
}

/// 热力格列表（`data.cells`）。
pub fn heatmap_cells(data: &Value) -> Vec<Value> {
    data.get("cells")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// 到点判据：缓存里的自然日不是今天，且本地小时已过配置时点。
///
/// 之所以用「日期不为今天」而不是「逐账号 done」：前者是单次判据、零请求，
/// 后者要对每个账号跑一遍循环；未办账号由 `run_activity_for_account` 自己跳过。
///
/// `hour` 默认 0（`ACTIVITY_DEFAULT_HOUR`）⇒ 跨天后第一次检查就补跑。保留这个参数
/// 是留给「夜里不联网」的场景：设成 8 就等于把补跑推到早上 8 点。
pub fn due_now(config: &Value, cache: &Value, today: &str, now_hour: u32) -> bool {
    if cache.get("date").and_then(Value::as_str) == Some(today) {
        return false;
    }
    let hour = config.get("hour").and_then(Value::as_i64).unwrap_or(0);
    (now_hour as i64) >= hour
}

/// 每号每日上报条数（配置 `report_count`，默认 1，钳制到 1..=10）。
///
/// 1 条即可点亮连登；调到 5 是因为「领养猫前置需 5 次对话」的对话量门槛
/// （见 2api `runActivity` 注释：`chat_5` 前置需 5 次对话）。
pub fn report_count(config: &Value) -> i64 {
    config
        .get("report_count")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, MAX_REPORT_COUNT)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// 与 `travel.rs` 同款的成长中心请求头（web 端形态）。
fn activity_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("x-client-platform".to_string(), "web".to_string());
    headers.insert("origin".to_string(), WORKBUDDY_API_ENDPOINT.to_string());
    headers.insert(
        "referer".to_string(),
        format!("{WORKBUDDY_API_ENDPOINT}/profile/growth-center"),
    );
    headers
}

/// 未授权判定（与 `travel.rs::is_unauthorized` 同口径：业务码 401/403 或文案命中）。
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

/// 发活跃地图接口请求；未授权且存在 refresh token 时刷新一次并重试。
async fn activity_request(path: &str, method: &str, body: Option<Value>, account: &Value) -> Value {
    let url = format!("{WORKBUDDY_API_ENDPOINT}{path}");
    let headers = activity_headers(account);
    let mut resp = http_request(&url, method, body.clone(), Some(&headers)).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        let refreshed = refresh_account_token(account.clone()).await;
        let headers = activity_headers(&refreshed);
        resp = http_request(&url, method, body, Some(&headers)).await;
    }
    resp
}

/// 发请求并取业务 `data` 段；业务码非 0 视为错误（回执文案）。
async fn activity_data(
    path: &str,
    method: &str,
    body: Option<Value>,
    account: &Value,
) -> Result<Value, String> {
    let resp = activity_request(path, method, body, account).await;
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(resp_error(&resp, code));
    }
    Ok(resp.get("data").cloned().unwrap_or(Value::Null))
}

fn resp_error(resp: &Value, fallback_code: i64) -> String {
    resp.get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("code={fallback_code}"))
}

fn truncate(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 120 {
        return trimmed.to_string();
    }
    trimmed.chars().take(120).collect::<String>() + "…"
}

/// 文案命中判据（大小写不敏感）。
fn msg_matches(msg: &str, markers: &[&str]) -> bool {
    let lower = msg.to_lowercase();
    markers.iter().any(|marker| lower.contains(&marker.to_lowercase()))
}

// ---------------------------------------------------------------------------
// 对话活跃上报
// ---------------------------------------------------------------------------

/// 构造 `chat_request_send` 事件体（单元素数组，字段照抄客户端形状）。
///
/// 刻意保留全部字段：上游后续若加严校验，最小三字段版本会被拒（2api 实测结论）。
pub fn report_body(uid: &str, conversation_id: &str, request_id: &str, timestamp: i64) -> Value {
    json!([{
        "eventCode": "chat_request_send",
        "timestamp": timestamp,
        "reportDelay": 0,
        "mode": "craft",
        "conversationId": conversation_id,
        "requestId": request_id,
        "inputLength": 12,
        "requestModelId": "deepseek-v4-flash",
        "requestModelName": "DeepSeek V4 Flash",
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
    }])
}

/// 向上游连发 `count` 条活跃上报（同会话多轮：conversationId 相同、requestId 各自独立）。
/// 返回成功条数；任一条失败即停止续发（后续自检无意义）。
async fn report_activity(account: &Value, uid: &str, count: i64) -> (i64, Option<String>) {
    let conversation_id = format!("wbs-{}", now_ms());
    let mut ok = 0;
    for index in 1..=count {
        let request_id = format!("{conversation_id}-r{index}");
        let body = report_body(uid, &conversation_id, &request_id, now_ms());
        match activity_data(REPORT_PATH, "POST", Some(body), account).await {
            Ok(_) => ok += 1,
            Err(error) => return (ok, Some(error)),
        }
        if index < count {
            tokio::time::sleep(REPORT_GAP).await;
        }
    }
    (ok, None)
}

// ---------------------------------------------------------------------------
// 单账号全链
// ---------------------------------------------------------------------------

fn skipped(reason: &str) -> Value {
    json!({"status": "skipped", "reason": reason})
}

fn account_key(account: &Value) -> String {
    account
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .unwrap_or_else(|| account_display_name(account))
}

/// 只保留支持活跃地图的账号（国际版不发任何请求）。
fn activity_capable_accounts(accounts: Vec<Value>) -> Vec<Value> {
    accounts
        .into_iter()
        .filter(|account| variant_of(account).supports_activity())
        .collect()
}

/// 补签目标：昨日漏签且有卡时返回昨日日期，否则 `None`（无判据不动）。
async fn makeup_target(account: &Value, state: &StreakState) -> Option<String> {
    if state.makeup_balance <= 0 {
        return None;
    }
    let data = activity_data(HEATMAP_PATH, "GET", None, account).await.ok()?;
    let yesterday = cst_yesterday();
    match heatmap_day_score(&heatmap_cells(&data), &yesterday) {
        Some(0) => Some(yesterday),
        _ => None,
    }
}

/// 单账号当日一轮全链：上报 → 回读自检（含滞后补读）→ 礼包/补偿 → 补签 → 档位兑换 → 抽奖。
///
/// 幂等：调用方只在「当日未办」时调用；`prior.bonusDone` 保证礼包/补偿每号只试一次。
///
/// `expected` = 今日预期连登天数（昨日 + 1，`None` 表示无判据）。回读值低于它时会
/// 隔 `STREAK_RECHECK_DELAY` 补读，取最大值 —— 见常量处的 2026-10-04 实证。
pub async fn run_activity_for_account(
    account: &Value,
    prior: &Value,
    config: &Value,
    expected: Option<i64>,
) -> Value {
    if !variant_of(account).supports_activity() {
        return skipped("unsupported_variant");
    }
    let checkin_config = load_checkin_config();
    let fresh = ensure_fresh_token(account.clone(), &checkin_config).await;
    let uid = fresh
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if uid.is_empty() {
        return skipped("no-uid");
    }

    let mut result = json!({
        "status": "done",
        "streakDays": 0,
        "makeupCards": 0,
        "makeupMax": 0,
        "reported": 0,
        "makeup": "none",
        "tier": "",
        "redeem": "none",
        "lottery": "none",
        "gift": 0,
        "silentDrop": false,
        "bonusDone": prior
            .get("bonusDone")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "at": now_ms(),
    });

    // ① 上报（点亮连登）
    let (reported, report_error) = report_activity(&fresh, &uid, report_count(config)).await;
    result["reported"] = json!(reported);
    if let Some(error) = report_error {
        result["status"] = json!("error");
        result["message"] = json!(truncate(&error));
        return result;
    }

    // ② 回读 streak 自检（200 可能被静默丢弃：缺 userId 时 progress 不动）
    //    低于预期时补读：个别账号上报后紧接着读，拿到的还是「今天没结算」的旧值。
    let (mut state, recheck) = match read_streak_settled(&fresh, expected).await {
        Ok(pair) => pair,
        Err(error) => {
            result["status"] = json!("error");
            result["message"] = json!(format!("连登状态读取失败：{}", truncate(&error)));
            return result;
        }
    };
    result["silentDrop"] = json!(state.days == 0);
    result["streakRecheck"] = json!(recheck);
    if let Some(want) = expected {
        result["streakExpected"] = json!(want);
    }

    // ③ 礼包 / 补偿（幂等写，业务错误静默 —— 绝大多数号早已领过）
    let mut gift = 0i64;
    if !result
        .get("bonusDone")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Ok(data) = activity_data(CLAIM_GIFT_PATH, "POST", Some(json!({})), &fresh).await {
            gift += data.get("credit").and_then(Value::as_i64).unwrap_or(0);
        }
        if let Ok(data) =
            activity_data(CLAIM_COMPENSATION_PATH, "POST", Some(json!({})), &fresh).await
        {
            gift += data.get("credit").and_then(Value::as_i64).unwrap_or(0);
        }
        result["bonusDone"] = json!(true);
    }
    result["gift"] = json!(gift);

    // ④ 补签保连登（补成功后重读天数，让本日兑换直接吃到恢复后的连登）
    if let Some(target) = makeup_target(&fresh, &state).await {
        match activity_data(
            MAKEUP_USE_PATH,
            "POST",
            Some(json!({ "target_date": target })),
            &fresh,
        )
        .await
        {
            Ok(_) => {
                result["makeup"] = json!("ok");
                if let Ok(data) = activity_data(STREAK_PATH, "GET", None, &fresh).await {
                    state = parse_streak_state(&data);
                }
            }
            Err(error) => {
                result["makeup"] = json!("failed");
                result["makeupMessage"] = json!(truncate(&error));
            }
        }
    }

    // ⑤ 档位兑换（已达标且本月未领的最高档，每日一轮一档）
    let tier = eligible_tier(state.days, &state);
    if !tier.is_empty() {
        let token = format!("redeem-{}-{}", tier, uuid::Uuid::new_v4().simple());
        match activity_data(
            REDEEM_PATH,
            "POST",
            Some(json!({ "tier": tier, "client_token": token })),
            &fresh,
        )
        .await
        {
            Ok(data) => {
                result["tier"] = json!(tier);
                result["redeem"] = json!("ok");
                result["redeemCredit"] = json!(data
                    .get("credit_granted")
                    .and_then(Value::as_i64)
                    .unwrap_or(0));
                result["redeemEnergy"] = json!(data
                    .get("energy_granted")
                    .and_then(Value::as_i64)
                    .unwrap_or(0));
            }
            Err(error) => {
                result["tier"] = json!(tier);
                result["redeem"] = json!(if msg_matches(
                    &error,
                    &["duplicate", "已领取", "已兑换", "连续登录天数不足"]
                ) {
                    "skip" // 本月已领 / 天数不足：正常态，不刷错误
                } else {
                    "error"
                });
                result["redeemMessage"] = json!(truncate(&error));
            }
        }
    }

    // ⑥ 抽奖（有一抽一；无次数/未开启是正常态）
    if let Ok(data) = activity_data(LOTTERY_CHANCES_PATH, "GET", None, &fresh).await {
        let mut chances = data.get("balance").and_then(Value::as_i64).unwrap_or(0);
        let mut draws = 0;
        while chances > 0 && draws < MAX_DRAW_PER_ROUND {
            let token = format!("draw-{}", uuid::Uuid::new_v4().simple());
            match activity_data(
                LOTTERY_DRAW_PATH,
                "POST",
                Some(json!({ "client_token": token })),
                &fresh,
            )
            .await
            {
                Ok(data) => {
                    draws += 1;
                    if let Some(name) = data.get("prize_name").and_then(Value::as_str) {
                        result["prize"] = json!(name);
                    }
                    chances -= 1;
                }
                Err(error) => {
                    if !msg_matches(
                        &error,
                        &["insufficient lottery chance balance", "lottery disabled"],
                    ) {
                        result["drawMessage"] = json!(truncate(&error));
                    }
                    break;
                }
            }
        }
        if draws > 0 {
            result["lottery"] = json!("ok");
            result["draws"] = json!(draws);
        }
    }

    result["streakDays"] = json!(state.days);
    result["makeupCards"] = json!(state.makeup_balance);
    result["makeupMax"] = json!(state.makeup_max);
    result["remainingDays"] = json!(state.remaining_days);
    if state.days == 0 {
        result["message"] = json!("上报已返回成功，但连登天数仍是 0，像是被上游静默丢弃了");
    }
    result
}

// ---------------------------------------------------------------------------
// 全账号轮
// ---------------------------------------------------------------------------

/// 缓存里某账号的当日结果。
///
/// ⚠️ 必须先校验缓存日期：缓存停在昨天时，昨天的 `done` 不能当成「当日已办」——
/// 否则 due_now（日期闸）判定该跑、逐账号 skip（旧 done）却全跳过 ⇒ 一行不写、
/// 缓存永远不滚动 ⇒ 活跃地图从「昨号全员 done」那天起永久死锁不再自动跑
///（2026-09-27 实证：昨晚全员 done，今晨起连续两次 GUI 重启 cycle 都 9ms 空转）。
fn cache_result(account_id: &str) -> Value {
    let cache = load_activity_cache();
    fresh_cache_result(&cache, account_id)
}

/// `cache_result` 的纯函数形态（可测）：只认当日缓存，跨天缓存一律视为无记录。
fn fresh_cache_result(cache: &Value, account_id: &str) -> Value {
    let today = cst_today();
    if cache.get("date").and_then(Value::as_str) != Some(today.as_str()) {
        return json!({});
    }
    cache
        .get("results")
        .and_then(|results| results.get(account_id))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// 写入某账号的当日结果；跨自然日先滚动缓存（旧的当日结果全部作废）。
fn write_cache_result(account_id: &str, result: &Value, today: &str) {
    let mut cache = load_activity_cache();
    if cache.get("date").and_then(Value::as_str) != Some(today) {
        cache = json!({ "date": today, "results": {} });
    }
    if !cache.get("results").is_some_and(Value::is_object) {
        cache["results"] = json!({});
    }
    cache["results"][account_id] = result.clone();
    if let Err(error) = save_activity_cache(&cache) {
        eprintln!("[活跃地图] 缓存写入失败: {error}");
    }
}

/// 给单账号全链套硬超时；超时返回 `status=timeout` 占位结果（不 panic、不卡整轮）。
///
/// 独立成函数是为了可测：单测用「极小超时 + 永不返回的 future」验证超时分支真的会落位，
/// 而 `run_activity_cycle` 侧只负责传真实时长（对照 skill `guard-reverse-verification`：
/// 只看正常态 PASS 等于没验）。
async fn with_account_timeout<F>(timeout: Duration, fut: F) -> Value
where
    F: std::future::Future<Output = Value>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(value) => value,
        Err(_) => json!({
            "status": "timeout",
            "message": format!(
                "单账号活跃地图 {}s 未返回（请求挂起），已跳过本轮",
                timeout.as_secs()
            ),
        }),
    }
}

/// 跑一轮活跃地图（`force = true` 跳过开关/时点/当日已办三道闸，供「立即执行」）。
pub async fn run_activity_cycle(force: bool) -> Value {
    let config = load_activity_config();
    if !force && config.get("enabled").and_then(Value::as_bool) != Some(true) {
        return json!({"status": "skipped", "reason": "disabled"});
    }
    let today = cst_today();
    if !force {
        let cache = load_activity_cache();
        if !due_now(&config, &cache, &today, Local::now().hour()) {
            return json!({"status": "skipped", "reason": "not-due"});
        }
    }
    let Some(_guard) = RunFlagGuard::try_acquire(&ACTIVITY_RUNNING) else {
        return json!({"status": "skipped", "reason": "running"});
    };

    let mut accounts = activity_capable_accounts(load_accounts());
    // 每轮重新洗牌：表序固定 ⇒ 每轮 uid 先后关系雷同，是可聚类特征。
    shuffle_by_seed(&mut accounts, now_ms() as u64 ^ 0x9E37_79B9_7F4A_7C15);
    // 开跑前留一份当日快照：整轮被网络打断时整体回滚（见下方 `net_failed` 分支）。
    let snapshot = load_activity_cache();
    // ★昨日连登快照必须在这里取：下面第一个账号一写入，缓存 date 就被盖成今天、
    //   results 清空，之后谁也拿不到「昨天是几天」这个补读基准了。
    let expected_days = yesterday_streak_days(&snapshot, &cst_yesterday());
    let mut results = Vec::new();
    let mut net_failed = false;
    for account in accounts {
        let key = account_key(&account);
        let prior = with_activity_cache_lock(|| cache_result(&key));
        // 当日已办则跳过（force 时重跑，用于排障）
        if !force && prior.get("status").and_then(Value::as_str) == Some("done") {
            continue;
        }
        // 账号间抖动（2~8s）：与保活同款防护，已办跳过的账号不产生延迟。
        batch_gap().await;
        // 硬超时：网络挂起时跳过该号继续跑，避免守卫被永久占用（见 ACCOUNT_ACTIVITY_TIMEOUT）
        let result = with_account_timeout(
            ACCOUNT_ACTIVITY_TIMEOUT,
            run_activity_for_account(
                &account,
                &prior,
                &config,
                streak_expected(&expected_days, &key),
            ),
        )
        .await;
        net_failed |= is_network_failure(&result);
        with_activity_cache_lock(|| write_cache_result(&key, &result, &today));
        results.push(json!({
            "id": key,
            "name": account_display_name(&account),
            "result": result,
        }));
    }

    // 坑（2026-10-04）：`write_cache_result` 会在跨天时把 `date` 盖成今天，
    // 于是「网络不通」这一轮也会把当天盖戳收工 —— 而 `hour` 默认 0 意味着
    // 跨天后第一次检查就开跑，凌晨网络没就绪时一整轮全挂、当天就再也不会补。
    // 与保活同一处置：整轮有网络级失败就回滚到开跑前的快照，等下一个 30 分钟窗口。
    if net_failed {
        with_activity_cache_lock(|| {
            if let Err(error) = save_activity_cache(&snapshot) {
                eprintln!("[活跃地图] 回滚当日缓存失败: {error}");
            }
        });
        return json!({
            "status": "aborted",
            "reason": "network_unreachable",
            "date": today,
            "total": results.len(),
            "done": 0,
            "rolled_back": true,
            "results": results,
        });
    }

    let done = results
        .iter()
        .filter(|entry| entry["result"]["status"] == "done")
        .count();
    json!({
        "status": "ok",
        "date": today,
        "total": results.len(),
        "done": done,
        "results": results,
    })
}

/// 账号结果是否属于「网络层失败」而非业务失败。
///
/// `status=error` 的文案来自上游请求（活跃链路拿不到 `code`，只剩 `message`），
/// `status=timeout` 是本机硬超时 —— 两者都只说明「这一轮链路没通」，
/// 不能被当成「今天办过了、可以盖戳收工」。业务失败（如档位不支持）不算。
fn is_network_failure(result: &Value) -> bool {
    match result.get("status").and_then(Value::as_str) {
        Some("timeout") => true,
        Some("error") => result
            .get("message")
            .and_then(Value::as_str)
            .map(is_network_message)
            .unwrap_or(false),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 展示
// ---------------------------------------------------------------------------

/// 单账号展示数据（供账号页/详情用）。
///
/// 当日已点亮 ⇒ `stale=false` + `date=今天`；当日还没跑但**昨天有成功记录** ⇒ 回落到
/// 昨天那一份并标 `stale=true` + `date=昨天`，前端据此显示「昨日已连登 N 天」。
/// 两者都没有 ⇒ `status: "pending"`。
///
/// 2026-10-04：原实现是「非今日一律 pending」，于是每天 00:00 缓存翻篇后
/// 账号卡的活跃地图整段消失（chip 只在 `status=done` 时渲染），空白 10 小时。
/// 回落比「什么都不显示」好，但**必须带 stale + 真实日期**——拿昨天的数字冒充今天
/// 会让人误判断签。
pub fn activity_display(account_id: &str) -> Value {
    display_from_cache(&load_activity_cache(), account_id, &cst_today())
}

/// `activity_display` 的纯函数形态（可测）：不碰磁盘，日期由调用方注入。
fn display_from_cache(cache: &Value, account_id: &str, today: &str) -> Value {
    let cache_date = cache.get("date").and_then(Value::as_str).unwrap_or("");
    let entry = cache
        .get("results")
        .and_then(|results| results.get(account_id))
        .cloned();
    match entry {
        // 只回落「昨天真的办成了」的记录：昨天 error/skipped 的号回落过来，
        // 等于拿一份失败记录冒充今天的成果。
        Some(value)
            if value.get("status").and_then(Value::as_str) == Some("done")
                && !cache_date.is_empty() =>
        {
            let stale = cache_date != today;
            json!({
                "status": value.get("status").cloned().unwrap_or(json!("done")),
                "date": if stale { cache_date } else { today },
                "stale": stale,
                "streakDays": value.get("streakDays").cloned().unwrap_or(json!(0)),
                "makeupCards": value.get("makeupCards").cloned().unwrap_or(json!(0)),
                "makeupMax": value.get("makeupMax").cloned().unwrap_or(json!(0)),
                "tier": value.get("tier").cloned().unwrap_or(json!("")),
                "redeem": value.get("redeem").cloned().unwrap_or(json!("none")),
                "lottery": value.get("lottery").cloned().unwrap_or(json!("none")),
                "gift": value.get("gift").cloned().unwrap_or(json!(0)),
                "reported": value.get("reported").cloned().unwrap_or(json!(0)),
                "message": value.get("message").cloned().unwrap_or(Value::Null),
            })
        }
        _ => json!({
            "status": "pending",
            "date": today,
            "stale": false,
            "streakDays": null,
            "makeupCards": null,
            "makeupMax": null,
            "tier": "",
            "redeem": "none",
            "lottery": "none",
            "gift": 0,
            "reported": 0,
            "message": Value::Null,
        }),
    }
}

// 缓存读取工具在 config.rs（load_activity_cache / save_activity_cache），
// 单测如需临时目录版本请用 config 的 `*_at` 变体。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::config::{
        load_activity_cache_at, load_activity_config_at, save_activity_cache_at, ACTIVITY_DEFAULT_HOUR,
    };

    fn parse(text: &str) -> Value {
        serde_json::from_str::<Value>(text).expect("测试 JSON 恒合法")
    }

    fn streak(text: &str) -> StreakState {
        parse_streak_state(&parse(text))
    }

    /// 反向验证：future 永不返回 + 极小超时 ⇒ 必须拿到 `status=timeout` 占位
    /// （不是 panic、不是永久挂起 —— 挂起正是 09-26 活跃地图停摆的机理）。
    #[tokio::test]
    async fn account_timeout_returns_placeholder() {
        let value = with_account_timeout(
            Duration::from_millis(10),
            std::future::pending::<Value>(),
        )
        .await;
        assert_eq!(
            value.get("status").and_then(Value::as_str),
            Some("timeout"),
            "挂起必须落 timeout 占位，别把整轮拖死"
        );
        assert!(value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("请求挂起"));
    }

    /// 正向对照：正常结果不被超时包装改写（防止加了超时后把好结果吞掉）。
    #[tokio::test]
    async fn account_timeout_passes_normal_result() {
        let value = with_account_timeout(
            Duration::from_secs(5),
            std::future::ready(json!({"status": "done", "reported": 1})),
        )
        .await;
        assert_eq!(value.get("status").and_then(Value::as_str), Some("done"));
        assert_eq!(value.get("reported").and_then(Value::as_i64), Some(1));
    }

    /// 上游 streak 响应体的 `data` 段形状（照抄 2api 实测字段）。
    fn streak_json(days: i64, cards: i64, s7: &str, s14: &str, s28: &str) -> String {
        format!(
            r#"{{"streak":{{"days":{days}}},"makeup_cards":{{"balance":{cards},"max":4}},
                "redemption_status":{{"tier_7d_status":"{s7}","tier_14d_status":"{s14}",
                "tier_28d_status":"{s28}","remaining_days":3}}}}"#
        )
    }

    #[test]
    fn parse_streak_state_reads_nested_fields() {
        let state = streak(&streak_json(12, 2, "claimed", "available", "locked"));
        assert_eq!(state.days, 12);
        assert_eq!(state.makeup_balance, 2);
        assert_eq!(state.makeup_max, 4);
        assert_eq!(state.remaining_days, 3);
        assert_eq!(state.status_of("14d"), "available");
        assert!(state.claimed("7d"));
        assert!(!state.claimed("14d"));
    }

    #[test]
    fn parse_streak_state_tolerates_missing_sections() {
        // 字段缺失 → 零值（不 panic、不误判 claimed）
        let state = streak(r#"{"streak":{"days":3}}"#);
        assert_eq!(state.days, 3);
        assert_eq!(state.makeup_balance, 0);
        assert_eq!(state.status_of("7d"), "");
        assert!(!state.claimed("7d"));
    }

    // -----------------------------------------------------------------------
    // 连登滞后补读（2026-10-04 实证：个别号上报后立刻回读拿到「今天没结算」的旧值）
    // -----------------------------------------------------------------------

    /// 昨日快照：只认「日期 == 昨天」且「status == done」的记录。
    fn snap(date: &str, entries: &[(&str, &str, i64)]) -> Value {
        let mut results = serde_json::Map::new();
        for (id, status, days) in entries {
            results.insert(
                id.to_string(),
                json!({"status": status, "streakDays": days}),
            );
        }
        json!({"date": date, "results": Value::Object(results)})
    }

    #[test]
    fn yesterday_streak_days_reads_only_yesterday_done_records() {
        let s = snap(
            "2026-10-03",
            &[
                ("acc-1", "done", 3),
                ("acc-2", "done", 2),
                ("acc-3", "error", 9),
                ("acc-4", "skipped", 9),
            ],
        );
        let map = yesterday_streak_days(&s, "2026-10-03");
        assert_eq!(map.get("acc-1"), Some(&3));
        assert_eq!(map.get("acc-2"), Some(&2));
        // 反向：失败/跳过记录不能当基准，否则会把它们误判成「滞后」
        assert_eq!(map.get("acc-3"), None);
        assert_eq!(map.get("acc-4"), None);
        // 反向：日期不是昨天 ⇒ 整份作废（跨了不止一天时连登本就该重新数）
        assert!(yesterday_streak_days(&s, "2026-10-04").is_empty());
    }

    #[test]
    fn streak_expected_is_yesterday_plus_one_or_none() {
        let map = HashMap::from([("acc-1".to_string(), 3i64)]);
        assert_eq!(streak_expected(&map, "acc-1"), Some(4));
        // 无昨日记录 ⇒ 无判据，不补读
        assert_eq!(streak_expected(&map, "acc-x"), None);
    }

    #[test]
    fn streak_needs_recheck_only_when_below_expected() {
        assert!(streak_needs_recheck(3, Some(4)));
        assert!(streak_needs_recheck(0, Some(4))); // 静默丢弃也该补读
        assert!(!streak_needs_recheck(4, Some(4)));
        assert!(!streak_needs_recheck(5, Some(4)));
        assert!(!streak_needs_recheck(1, None)); // 无判据不强补
    }

    #[test]
    fn streak_settle_recovers_when_retry_catches_up() {
        // 正向对照 + 本次故障的复现：首读 3、补读 3、再补读追上 4
        assert_eq!(streak_settle(3, &[3, 4], Some(4)), (4, "recovered"));
        assert_eq!(streak_settle(3, &[4], Some(4)), (4, "recovered"));
    }

    #[test]
    fn streak_settle_never_worse_than_first_read() {
        // 反向：上游抖动返回更小的值，取 max 不许把好值覆盖坏
        assert_eq!(streak_settle(4, &[2, 3], Some(4)), (4, "recovered"));
    }

    #[test]
    fn streak_settle_marks_stale_when_upstream_never_catches_up() {
        // 补读追不上（上游结算比本轮等待还慢）⇒ 如实标 stale，不假装成功
        assert_eq!(streak_settle(3, &[3, 3], Some(4)), (3, "stale"));
        assert_eq!(streak_settle(3, &[3], Some(4)), (3, "stale"));
    }

    #[test]
    fn streak_settle_does_not_inflate_a_real_break() {
        // ★反向验证的核心：真断签（连登掉回 1）绝不能被补读逻辑抬上去
        assert_eq!(streak_settle(1, &[1, 1], Some(4)), (1, "stale"));
    }

    #[test]
    fn streak_settle_without_expectation_is_plain_max() {
        assert_eq!(streak_settle(5, &[], None), (5, "none"));
        assert_eq!(streak_settle(5, &[6], None), (6, "none"));
        // 首读达标 ⇒ 无需补读，状态 none
        assert_eq!(streak_settle(4, &[], Some(4)), (4, "none"));
        // 首读就没达标且一次都没补读（不该发生，防御性）⇒ stale
        assert_eq!(streak_settle(3, &[], Some(4)), (3, "stale"));
    }

    #[test]
    fn tier_pick_prefers_highest_unclaimed() {
        let all_locked = streak(&streak_json(30, 0, "locked", "locked", "locked"));
        assert_eq!(eligible_tier(30, &all_locked), "28d");

        let top_claimed = streak(&streak_json(30, 0, "locked", "locked", "claimed"));
        assert_eq!(eligible_tier(30, &top_claimed), "14d");

        let mid_claimed = streak(&streak_json(30, 0, "claimed", "claimed", "claimed"));
        assert_eq!(eligible_tier(30, &mid_claimed), "");
    }

    #[test]
    fn tier_pick_respects_thresholds() {
        let state = streak(&streak_json(0, 0, "locked", "locked", "locked"));
        assert_eq!(eligible_tier(6, &state), "");
        assert_eq!(eligible_tier(7, &state), "7d");
        assert_eq!(eligible_tier(14, &state), "14d");
        assert_eq!(eligible_tier(28, &state), "28d");
    }

    #[test]
    fn heatmap_day_score_uses_date_prefix() {
        let cells = vec![
            json!({"date": "2026-09-21T00:00:00+08:00", "score": 0}),
            json!({"date": "2026-09-22", "score": 5}),
        ];
        assert_eq!(heatmap_day_score(&cells, "2026-09-21"), Some(0));
        assert_eq!(heatmap_day_score(&cells, "2026-09-22"), Some(5));
        // 无该日格 ⇒ None（无判据，不动），而非 Some(0) —— 否则会把「未覆盖」当漏签去补
        assert_eq!(heatmap_day_score(&cells, "2026-09-20"), None);
    }

    #[test]
    fn heatmap_cells_handles_missing_array() {
        assert!(heatmap_cells(&json!({})).is_empty());
        assert_eq!(
            heatmap_cells(&json!({"cells": [{"date": "2026-09-22", "score": 1}]})).len(),
            1
        );
    }

    #[test]
    fn due_now_gates_on_date_first_then_hour() {
        let config = json!({ "hour": 10 });
        // 当日已办 ⇒ 永不再跑（即使已过点）
        assert!(!due_now(&config, &json!({"date": "2026-09-23"}), "2026-09-23", 23));
        // 未到点 ⇒ 不跑
        assert!(!due_now(&config, &json!({"date": "2026-09-22"}), "2026-09-23", 9));
        // 到点且跨日 ⇒ 跑
        assert!(due_now(&config, &json!({"date": "2026-09-22"}), "2026-09-23", 10));
        // 无缓存（首次启动）⇒ 到点即跑
        assert!(due_now(&config, &json!({}), "2026-09-23", 15));
        // hour 缺省按 10 点
        assert!(due_now(&json!({}), &json!({}), "2026-09-23", 10));
    }

    #[test]
    fn fresh_cache_result_ignores_stale_date() {
        let today = cst_today();
        let yesterday = cst_yesterday();
        // 反向验证（09-27 死锁 bug）：缓存停在昨天时，昨天的 done 不得当成「当日已办」，
        // 否则 due_now 判该跑、逐账号 skip 却全跳 ⇒ 缓存永不滚动 ⇒ 活跃地图永久停摆。
        // 用动态日期而非写死值，避免 fixture 随真实日历过期（原写死 09-27，过当天即挂）。
        let stale = json!({
            "date": yesterday,
            "results": { "acc-1": {"status": "done", "streakDays": 5} }
        });
        assert!(fresh_cache_result(&stale, "acc-1").get("status").is_none());
        // 当日缓存正常命中
        let fresh = json!({
            "date": today,
            "results": { "acc-1": {"status": "done", "streakDays": 6} }
        });
        assert_eq!(fresh_cache_result(&fresh, "acc-1")["status"], json!("done"));
        // 当日缓存无该账号 ⇒ 空
        assert!(fresh_cache_result(&fresh, "acc-x").get("status").is_none());
    }

    #[test]
    fn report_count_clamps_to_sane_range() {
        assert_eq!(report_count(&json!({})), 1);
        assert_eq!(report_count(&json!({"report_count": 0})), 1);
        assert_eq!(report_count(&json!({"report_count": -5})), 1);
        assert_eq!(report_count(&json!({"report_count": 5})), 5);
        assert_eq!(report_count(&json!({"report_count": 999})), MAX_REPORT_COUNT);
    }

    #[test]
    fn report_body_is_single_element_with_user_id() {
        let body = report_body("uid-123", "cid-1", "cid-1-r1", 1_700_000_000_000);
        let arr = body.as_array().expect("上报体是数组（客户端事件形状）");
        assert_eq!(arr.len(), 1, "单条上报 = 单元素数组");
        let event = &arr[0];
        assert_eq!(event["eventCode"], json!("chat_request_send"));
        assert_eq!(event["userId"], json!("uid-123"));
        assert_eq!(event["conversationId"], json!("cid-1"));
        assert_eq!(event["requestId"], json!("cid-1-r1"));
        // 契约：字段总数固定 36（与 2api chatRequestEvent 同形状）。
        // 上游后续若加严校验，这里会先失败提醒补字段，而不是线上静默丢弃。
        assert_eq!(
            event.as_object().expect("事件是对象").len(),
            36,
            "字段集与 2api 事件形状不一致 —— 改字段务必同步本断言"
        );
    }

    #[test]
    fn cst_dates_are_ordered_and_well_formed() {
        let today = cst_today();
        let yesterday = cst_yesterday();
        assert_eq!(today.len(), 10);
        assert_eq!(yesterday.len(), 10);
        // 字典序 = 日期序，恒有 today > yesterday（不受跨午夜瞬间影响）
        assert!(yesterday < today, "{yesterday} 应早于 {today}");
    }

    #[test]
    fn activity_cache_roundtrip_uses_injected_path() {
        let dir = std::env::temp_dir().join(format!("wb-activity-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("activity_cache.json");

        let real_path = crate::modules::config::activity_cache_file();
        let real_before = std::fs::read_to_string(&real_path).ok();

        let cache = json!({"date": "2026-09-23", "results": {"acc-1": {"status": "done"}}});
        save_activity_cache_at(&path, &cache).expect("写临时缓存");
        let loaded = load_activity_cache_at(&path);
        assert_eq!(loaded["date"], json!("2026-09-23"));
        assert_eq!(loaded["results"]["acc-1"]["status"], json!("done"));

        // 反向断言：注入路径的写入没有碰到真实 store_dir（HANDOFF §5 坑 73）
        assert_eq!(
            std::fs::read_to_string(&real_path).ok(),
            real_before,
            "临时路径写入污染了真实 activity_cache.json"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn activity_config_roundtrip_and_hour_clamp() {
        let dir =
            std::env::temp_dir().join(format!("wb-activity-cfg-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("auto_activity_config.json");

        // 缺文件 ⇒ 默认（开启 + 默认时点）
        let defaulted = load_activity_config_at(&path);
        assert_eq!(defaulted["enabled"], json!(true));
        assert_eq!(defaulted["hour"], json!(ACTIVITY_DEFAULT_HOUR));

        // 越界 hour 被忽略（不写进配置）
        std::fs::write(&path, r#"{"enabled": false, "hour": 99}"#).expect("写测试配置");
        let clamped = load_activity_config_at(&path);
        assert_eq!(clamped["enabled"], json!(false));
        assert_eq!(clamped["hour"], json!(ACTIVITY_DEFAULT_HOUR));

        // 合法值透传
        std::fs::write(&path, r#"{"enabled": true, "hour": 22}"#).expect("写测试配置");
        let ok = load_activity_config_at(&path);
        assert_eq!(ok["hour"], json!(22));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// v1 → v2 迁移：老配置里的 `hour=10` 是**旧默认值**而非主人挑的时点，须落到新默认 0，
    /// 否则改了默认也治不了已经落盘的这台机器（空白窗照旧）。
    #[test]
    fn legacy_default_hour_migrates_to_zero_and_custom_hour_survives() {
        let dir =
            std::env::temp_dir().join(format!("wb-activity-mig-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("auto_activity_config.json");

        // 旧默认 10、无版本号 ⇒ 迁到 0（这正是本机 2026-10-04 的实况）
        std::fs::write(&path, r#"{"enabled": true, "hour": 10}"#).expect("写 v1 配置");
        assert_eq!(load_activity_config_at(&path)["hour"], json!(0));

        // 旧配置里的自定义值（≠10）视为显式选择 ⇒ 原样保留，不能被迁移顺手抹掉
        std::fs::write(&path, r#"{"enabled": true, "hour": 6}"#).expect("写 v1 自定义配置");
        assert_eq!(load_activity_config_at(&path)["hour"], json!(6));

        // v1 没写 hour（走默认）⇒ 同样是旧默认，迁到 0
        std::fs::write(&path, r#"{"enabled": false}"#).expect("写 v1 无 hour 配置");
        let migrated = load_activity_config_at(&path);
        assert_eq!(migrated["hour"], json!(0));
        assert_eq!(migrated["enabled"], json!(false));

        // 反向验证：已带 v2 版本号的 hour=10 是**新版显式选择** ⇒ 不许再迁
        std::fs::write(
            &path,
            r#"{"enabled": true, "hour": 10, "config_version": 2}"#,
        )
        .expect("写 v2 配置");
        assert_eq!(
            load_activity_config_at(&path)["hour"],
            json!(10),
            "迁移必须是一次性的：主人自己设回 10 不能被反复改回 0"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 默认时点下，跨天后第一次检查（00:00 整）就该补跑 —— 这是空白窗的根治点。
    #[test]
    fn due_now_is_immediate_after_midnight_by_default() {
        let yesterday = json!({"date": "2026-10-03", "results": {}});
        let today = json!({"date": "2026-10-04", "results": {}});
        let zero = json!({"enabled": true, "hour": ACTIVITY_DEFAULT_HOUR});

        assert!(
            due_now(&zero, &yesterday, "2026-10-04", 0),
            "默认时点 0 ⇒ 零点一过就该补跑，否则 00:00-10:00 空白"
        );
        assert!(
            !due_now(&zero, &today, "2026-10-04", 0),
            "当日已办不该重跑（幂等）"
        );
        assert!(
            due_now(&zero, &yesterday, "2026-10-04", 23),
            "白天检查同样成立"
        );

        // hour 仍可显式抬高（夜里不联网的场景）
        let eight = json!({"enabled": true, "hour": 8});
        assert!(!due_now(&eight, &yesterday, "2026-10-04", 3));
        assert!(due_now(&eight, &yesterday, "2026-10-04", 8));
    }

    /// 展示回落：缓存还停在昨天但昨天真的办成了 ⇒ 回落给昨天那一份并打 stale 标，
    /// 而不是让账号卡的活跃地图整段消失（2026-10-04 修复的正是这个）。
    #[test]
    fn display_falls_back_to_previous_day_with_stale_marker() {
        let cache = json!({
            "date": "2026-10-03",
            "results": {"acc-1": {"status": "done", "streakDays": 4, "makeupCards": 2, "makeupMax": 4}},
        });
        let value = display_from_cache(&cache, "acc-1", "2026-10-04");
        assert_eq!(value["status"], json!("done"));
        assert_eq!(
            value["stale"],
            json!(true),
            "回落必须自报「这是昨天的数」，否则会被当成今天已连登 4 天"
        );
        assert_eq!(
            value["date"],
            json!("2026-10-03"),
            "date 必须是缓存的真实日期"
        );
        assert_eq!(
            value["streakDays"],
            json!(4),
            "昨天的连登天数要保留，不能清空"
        );
        assert_eq!(value["makeupCards"], json!(2));
    }

    /// 反向验证：昨天是失败/跳过记录时**不许**回落 —— 拿一份失败冒充今天办成更糟。
    #[test]
    fn display_stays_pending_when_previous_day_failed() {
        let cache = json!({
            "date": "2026-10-03",
            "results": {
                "acc-err": {"status": "error", "message": "连登状态读取失败：超时"},
                "acc-skip": {"status": "skipped", "message": "unsupported_variant"},
            },
        });
        for id in ["acc-err", "acc-skip"] {
            let value = display_from_cache(&cache, id, "2026-10-04");
            assert_eq!(value["status"], json!("pending"), "{id} 不该回落");
            assert_eq!(value["streakDays"], json!(null));
        }
    }

    /// 当日缓存 ⇒ 与改动前完全一致（`stale=false` + `date=今天`），别把新字段当回归。
    #[test]
    fn display_fresh_when_cache_is_today() {
        let cache = json!({
            "date": "2026-10-04",
            "results": {"acc-1": {"status": "done", "streakDays": 5}},
        });
        let value = display_from_cache(&cache, "acc-1", "2026-10-04");
        assert_eq!(value["status"], json!("done"));
        assert_eq!(value["stale"], json!(false));
        assert_eq!(value["date"], json!("2026-10-04"));
        assert_eq!(value["streakDays"], json!(5));
    }

    /// 网络级失败分类：只把「链路没通」算进回滚，业务失败照常盖戳。
    /// 判据写反的后果是双向的 —— 漏判则一轮网络抖动烧掉一整天，误判则整天重跑。
    #[test]
    fn network_failure_classification() {
        assert!(
            is_network_failure(&json!({"status": "timeout", "message": "请求挂起"})),
            "硬超时 = 链路没通"
        );
        assert!(is_network_failure(&json!({
            "status": "error",
            "message": "连登状态读取失败：error sending request for url (https://...)",
        })));
        assert!(
            !is_network_failure(&json!({"status": "error", "message": "积分不足，无法兑换"})),
            "业务失败不该回滚：回滚会让这号今天永远不再试"
        );
        assert!(!is_network_failure(
            &json!({"status": "done", "streakDays": 3})
        ));
        assert!(!is_network_failure(
            &json!({"status": "skipped", "message": "no-uid"})
        ));
    }
}
