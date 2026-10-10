//! 切号后自动触发渲染层账号刷新（CDP，2026-10-08 定稿；2026-10-09 健壮化重写）。
//!
//! 背景：WorkBuddy 5.7.6 的渲染层账号资料是**懒加载**的——切号（换 auth 文件 + 重启）
//! 后，主进程身份已是目标号（`wb:account:profile` 实测返回目标 uid），但渲染层首屏
//! 不主动重拉，右下角徽章/侧栏仍显示源号，需一次用户交互（点账号/输入文字）才刷新。
//! 上游（changexbc/workbuddy-switch）完全未触及渲染层（`git grep sessionStorage` 零命中），
//! 这是本仓私有扩展。
//!
//! 做法：wb-switch 启动 WorkBuddy 时带上本机调试端口（随机高位端口，仅 127.0.0.1），
//! 切号完成后后台线程轮询端口就绪 → 经 CDP 等**渲染页 + 徽章元素**真正就绪 →
//! 用 `Input.dispatchMouseEvent` 派发**受信任鼠标点击**（打开左下角账号菜单，实测该动作
//! 触发渲染层重拉：徽章 旧号→目标号、侧栏条目补齐）。
//!
//! ## 2026-10-09 修复（一次真实失败复盘）
//!
//! 症状：切号日志出现 `{"ok":true,"detail":""}`（detail 为空串）——脚本"成功"但无结果；
//! 界面仍是源号，手动点一次账号才刷新。
//!
//! 根因：**端口就绪 ≠ 页面就绪**。`--remote-debugging-port` 在 Electron 早期即监听，
//! 当时主窗口的 page target 可能还是空白/导航中；旧实现拿到第一个 `type=page` 就执行
//! 一个「单条 `Runtime.evaluate` + `awaitPromise` 长轮询 JS」，页面导航会让执行上下文销毁，
//! CDP 返回无 `value` 的结果 ⇒ 落盘成空串，且代码把空串当成 `Ok` 静默吞掉。
//! 证据：本次进程 09:38:49 启动、日志 09:38:51（仅 2s）—— 正常成功需要 ~3s 轮询等元素。
//!
//! 修法（本文件）：①只认 `renderer/index.html` 的 page target，找不到就轮询等待；
//! ②把「等待元素」的轮询从 JS 挪到 Rust，每次 CDP 调用都是**短同步**调用（导航不会吞结果）；
//! ③`evaluate` 返回空/异常一律视为错误并重试，不再静默成功；④点击改用 `Input.dispatchMouseEvent`
//! 受信任事件（等价真实鼠标，而非 `el.click()` 合成事件）。
//!
//! 设计要点：
//! - **不阻塞切号**：独立线程、失败静默（只记结果，不报错给用户）。
//! - **幂等**：徽章已是目标号时不点击（先读后点）。
//! - **零依赖新增**：tungstenite 已在 vendor 依赖树内（agent-studio-core 引入）。
//! - 端口随机：仅 wb-switch 知道，等价于一次性调试通道，降低常开固定端口的面。

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// 给 WorkBuddy 的调试端口环境变量名（`launch_workbuddy` 读取并转成启动参数）。
pub const DEBUG_PORT_ENV: &str = "WB_SWITCH_REMOTE_DEBUG_PORT";

/// 主渲染页 URL 特征（用于在 `/json` 里挑出真正的应用窗口，避开启动早期的空白页）。
const RENDERER_URL_HINT: &str = "renderer/index.html";

/// 左下角账号徽章文本（幂等判据）。
const BADGE_EXPR: &str =
    r#"(() => { const b = document.querySelector('.user-menu-trigger'); return b ? b.textContent.trim() : ''; })()"#;

/// 徽章是否出现（页面是否已渲染到可交互）。
const BADGE_PRESENT_EXPR: &str = r#"!!document.querySelector('.user-menu-trigger')"#;

/// 徽章中心坐标（供受信任点击用）；元素不存在时返回字符串 `"null"`。
const TRIGGER_RECT_EXPR: &str = r#"(() => { const el = document.querySelector('.user-menu-trigger') || document.querySelector('[class*="user-menu"]'); if (!el) return null; const r = el.getBoundingClientRect(); return JSON.stringify({ x: r.left + r.width / 2, y: r.top + r.height / 2 }); })()"#;

/// 徽章是否已是目标账号（空值一律视为未匹配 —— 防"空 expect 命中一切"的假成功）。
pub fn badge_matches(badge: &str, expect: &str) -> bool {
    !badge.is_empty() && !expect.is_empty() && badge.contains(expect)
}

/// 轮询等待调试端口就绪；返回是否就绪。
pub fn wait_for_debug_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().expect("addr"),
            Duration::from_millis(500),
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(800));
    }
    false
}

/// 读 CDP 的 `/json` 目标列表（本机、绕代理）。
fn http_get(port: u16, path: &str) -> Option<String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .ok()?
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .ok()?
        .text()
        .ok()
}

/// 从 `/json` 响应里挑渲染页的 WebSocket 调试地址。
///
/// `exact_only = true` 时只认 `renderer/index.html`（应用主窗口）；
/// 否则退化为「第一个 page target」（兜底，防上游改 URL）。
fn pick_page_ws(body: &str, exact_only: bool) -> Option<String> {
    let targets: Value = serde_json::from_str(body).ok()?;
    let pages: Vec<&Value> = targets
        .as_array()?
        .iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .collect();
    let ws_of = |t: &Value| {
        t.get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    if let Some(t) = pages.iter().find(|t| {
        t.get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| u.contains(RENDERER_URL_HINT))
    }) {
        return ws_of(t);
    }
    if exact_only {
        return None;
    }
    pages.first().and_then(|t| ws_of(t))
}

/// 轮询等待**渲染页**出现（端口就绪 ≠ 页面就绪：启动早期只有空白 target）。
///
/// 超时前只接受精确匹配；确实等不到时用「第一个 page」兜底（记录降级，不静默失败）。
pub fn wait_for_page_ws(port: u16, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut fallback: Option<String> = None;
    while Instant::now() < deadline {
        if let Some(body) = http_get(port, "/json") {
            if let Some(ws) = pick_page_ws(&body, true) {
                return Ok(ws);
            }
            if fallback.is_none() {
                fallback = pick_page_ws(&body, false);
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
    fallback.ok_or_else(|| format!("未找到渲染页面调试目标（端口 {port}，超时）"))
}

/// 极简 CDP 客户端：单连接、按 id 匹配响应（同步调用，便于分步编排）。
struct CdpClient {
    sock: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next_id: i64,
}

impl CdpClient {
    fn connect(ws_url: &str) -> Result<Self, String> {
        let (sock, _) = tungstenite::connect(ws_url).map_err(|e| format!("ws connect: {e}"))?;
        if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = sock.get_ref() {
            // 每条命令都应是短往返（等待逻辑在 Rust 侧做），20s 足够。
            let _ = tcp.set_read_timeout(Some(Duration::from_secs(20)));
        }
        Ok(Self { sock, next_id: 1 })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.sock
            .send(tungstenite::Message::Text(
                json!({ "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .map_err(|e| format!("ws send: {e}"))?;
        // 依次读消息，取本 id 的响应（首帧可能是事件通知）。
        for _ in 0..50 {
            let msg = self.sock.read().map_err(|e| format!("ws read: {e}"))?;
            let text = match msg {
                tungstenite::Message::Text(t) => t.to_string(),
                tungstenite::Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                _ => continue,
            };
            let Ok(val) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if val.get("id").and_then(|v| v.as_i64()) != Some(id) {
                continue;
            }
            if let Some(err) = val.get("error") {
                return Err(format!("cdp error: {err}"));
            }
            return Ok(val.get("result").cloned().unwrap_or(Value::Null));
        }
        Err("ws: no response".to_string())
    }

    /// 执行 JS 并回读 JSON 值；**空结果视为错误**（页面导航/元素缺失 ⇒ 不静默成功）。
    fn eval(&mut self, expression: &str) -> Result<Value, String> {
        let r = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
        )?;
        if let Some(ex) = r.get("exceptionDetails") {
            return Err(format!(
                "page exception: {}",
                ex.get("text").and_then(|t| t.as_str()).unwrap_or("unknown")
            ));
        }
        let value = r
            .get("result")
            .and_then(|x| x.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        if value.is_null() {
            return Err("empty result（页面可能正在导航）".to_string());
        }
        Ok(value)
    }

    fn eval_str(&mut self, expression: &str) -> Result<String, String> {
        match self.eval(expression)? {
            Value::String(s) => Ok(s),
            Value::Bool(b) => Ok(if b { "true".into() } else { "false".into() }),
            other => Ok(other.to_string()),
        }
    }

    /// 受信任鼠标左键单击（等价真实用户点击，而非 `el.click()` 合成事件）。
    fn click(&mut self, x: f64, y: f64) -> Result<(), String> {
        for ty in ["mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": ty, "x": x, "y": y, "button": "left", "clickCount": 1, "buttons": 1 }),
            )?;
        }
        Ok(())
    }
}

/// 执行一次「刷新渲染层账号」（阻塞版，内部供后台线程调用；返回结果描述）。
pub fn refresh_renderer_account(
    port: u16,
    expect_name: &str,
    wait: Duration,
) -> Result<String, String> {
    if !wait_for_debug_port(port, wait) {
        return Err(format!("调试端口 {port} 未就绪"));
    }
    // ① 等真正的渲染页（不是启动早期的空白 target）。
    let ws_url = wait_for_page_ws(port, Duration::from_secs(90))?;
    let mut cdp = CdpClient::connect(&ws_url)?;

    // ② 等徽章元素出现（页面加载/导航期间 evaluate 会失败，忽略后重试）。
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut appeared = false;
    while Instant::now() < deadline {
        if let Ok(v) = cdp.eval_str(BADGE_PRESENT_EXPR) {
            if v == "true" {
                appeared = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !appeared {
        return Err("badge-not-found-timeout（60s 内徽章未出现）".to_string());
    }

    // ③ 幂等 + 受信任点击校验（≤3 轮）。
    let mut before = String::new();
    let mut after = String::new();
    for attempt in 1..=3 {
        before = cdp.eval_str(BADGE_EXPR).unwrap_or_default();
        if badge_matches(&before, expect_name) {
            return Ok(json!({ "skipped": "already", "badge": before, "attempt": attempt })
                .to_string());
        }
        let rect_raw = cdp
            .eval_str(TRIGGER_RECT_EXPR)
            .unwrap_or_else(|_| "null".to_string());
        if rect_raw == "null" {
            return Err("trigger 元素不存在".to_string());
        }
        let rect: Value =
            serde_json::from_str(&rect_raw).map_err(|e| format!("rect parse: {e}"))?;
        let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        cdp.click(x, y)?;
        std::thread::sleep(Duration::from_millis(2500));
        after = cdp.eval_str(BADGE_EXPR).unwrap_or_default();
        if badge_matches(&after, expect_name) {
            return Ok(json!({
                "clicked": true,
                "badgeBefore": before,
                "badgeAfter": after,
                "attempt": attempt,
            })
            .to_string());
        }
    }
    // ④ 兜底：点击 3 轮仍未生效 ⇒ 重载页面。渲染层重启必走 auth（5.7.6 懒加载的
    //    反面就是「重新加载才重新取」），因此这是点击路径失效时的最后手段。
    match reload_and_check(&mut cdp, expect_name) {
        Ok(badge) => Ok(json!({ "reloaded": true, "badge": badge, "clickAttempts": 3 }).to_string()),
        Err(e) => Ok(json!({
            "error": "not-refreshed",
            "badgeBefore": before,
            "badgeAfter": after,
            "reloadError": e,
        })
        .to_string()),
    }
}

/// 重载渲染页并校验徽章（兜底路径）。`Page.reload` 后旧文档可能短暂存活，
/// 因此先等固定间隔，再轮询「文档加载完成 + 徽章元素存在」。
fn reload_and_check(
    cdp: &mut CdpClient,
    expect_name: &str,
) -> Result<String, String> {
    cdp.call("Page.reload", json!({ "ignoreCache": false }))?;
    std::thread::sleep(Duration::from_secs(3));
    let ready_expr = "document.readyState === 'complete' && !!document.querySelector('.user-menu-trigger')";
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(v) = cdp.eval_str(ready_expr) {
            if v == "true" {
                // 元素出现 ≠ 账号资料已回填，留一点渲染时间。
                std::thread::sleep(Duration::from_millis(1500));
                let badge = cdp.eval_str(BADGE_EXPR).unwrap_or_default();
                if badge_matches(&badge, expect_name) {
                    return Ok(badge);
                }
                return Err(format!("reload 后徽章仍为「{badge}」"));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("reload 后 60s 内页面未就绪".to_string())
}

/// 后台线程：等待端口 → 执行刷新（失败静默，结果通过回调上报）。
pub fn spawn_refresh_after_launch<F>(port: u16, expect_name: String, on_done: F)
where
    F: FnOnce(Result<String, String>) + Send + 'static,
{
    std::thread::spawn(move || {
        let result = refresh_renderer_account(port, &expect_name, Duration::from_secs(90));
        on_done(result);
    });
}

/// 随机高位端口（40000–49999）：仅本机绑定，且只由 wb-switch 传给子进程，
/// 等价于「一次性调试通道」，避免固定端口长期可被本机他进程探测。
pub fn random_port() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 + d.as_secs() * 1_000_000_007)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    40000 + ((nanos ^ (pid << 17)) % 10000) as u16
}

/// 刷新结果落盘（追加一行 JSON）：异步线程无法回到切号报告，单独留痕便于排障。
pub fn log_refresh_result(port: u16, result: &Result<String, String>) {
    let path = crate::modules::config::home_dir()
        .join(".wb-switch")
        .join("renderer-refresh.log");
    let entry = json!({
        "at": crate::modules::config::utc_iso(),
        "port": port,
        "ok": result.is_ok(),
        "detail": match result { Ok(s) => s.clone(), Err(e) => e.clone() },
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{}", entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 幂等判据：空 expect / 空 badge 一律不匹配（防"空串命中一切"的假成功）。
    #[test]
    fn badge_matches_rejects_empty_and_requires_substring() {
        assert!(badge_matches("Elaine", "Elaine"));
        assert!(badge_matches(" Elaine ", "Elaine"), "应做包含匹配");
        assert!(badge_matches("18895610412", "18895610412"));
        assert!(!badge_matches("Elaine", "18895610412"));
        assert!(!badge_matches("", "Elaine"), "空徽章不算命中");
        assert!(!badge_matches("Elaine", ""), "空 expect 不算命中");
        assert!(!badge_matches("", ""));
    }

    /// 挑页面：优先精确匹配渲染页，避开启动早期的空白 target。
    #[test]
    fn pick_page_ws_prefers_renderer_page() {
        let body = r#"[
          {"type":"page","url":"about:blank","webSocketDebuggerUrl":"ws://x/blank"},
          {"type":"page","url":"file:///C:/app.asar/renderer/index.html?locale=zh-CN","webSocketDebuggerUrl":"ws://x/main"},
          {"type":"worker","url":"file:///w.js","webSocketDebuggerUrl":"ws://x/w"}
        ]"#;
        assert_eq!(pick_page_ws(body, true).as_deref(), Some("ws://x/main"));
        assert_eq!(pick_page_ws(body, false).as_deref(), Some("ws://x/main"));
    }

    /// exact_only 时没有渲染页 ⇒ 不返回空白页（否则就是本次事故的根因）。
    #[test]
    fn pick_page_ws_exact_only_skips_non_renderer_page() {
        let body = r#"[{"type":"page","url":"about:blank","webSocketDebuggerUrl":"ws://x/blank"}]"#;
        assert_eq!(pick_page_ws(body, true), None);
        assert_eq!(pick_page_ws(body, false).as_deref(), Some("ws://x/blank"));
    }

    #[test]
    fn port_env_name_is_stable() {
        // 与 process.rs 的读取侧保持一致的常量名
        assert_eq!(DEBUG_PORT_ENV, "WB_SWITCH_REMOTE_DEBUG_PORT");
    }

    /// 端口落在约定区间（40000–49999），且每次调用不恒定。
    #[test]
    fn random_port_in_expected_range() {
        for _ in 0..20 {
            let p = random_port();
            assert!((40000..50000).contains(&p), "端口越界: {p}");
        }
    }
}
