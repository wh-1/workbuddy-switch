//! 本地网关服务的生命周期与健康检查（2api 网关）。
//!
//! 起因：2api（workbuddy2api）网关的开关要靠命令行或计划任务，网关页只能看不能动。
//! 本模块把它收敛成一张服务表：能查、能启、能停，并按**身份**判定健康 ——
//! 端口通了不代表服务对（见 [`health_of`]）。
//!
//! 判定口径（自下而上，任一层失败即降级）：
//! 1. **端口**：`127.0.0.1:<port>` 能否连上（400ms 超时的裸 TCP connect，不受代理影响）。
//! 2. **HTTP**：`GET /healthz` 是否 2xx（本机请求必须 `no_proxy`，否则被系统代理挡 502）。
//! 3. **身份**：响应里的 `kind` / `service` 字段必须等于本服务的期望标识。端口漏水给别的
//!    服务时前两层都会骗人，只有身份能抓住（与 2api `ServiceName` 的防护同源）。
//!
//! 启停策略：先看**同端口是否已在跑**（幂等，不重复拉起）；启动优先走**计划任务下发**
//! （本机服务是 ONLOGON 常驻任务，任务不存在才退回直接 spawn）；停止同理优先
//! `schtasks /end`，再按端口 PID 兜底杀。
//! ⚠️ 不可省略 plan-task 优先：会话内直接起的常驻进程会被会话 Job Object 连带清杀
//! （2026-09-22 两连实证），计划任务拉起的才活得下来。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::modules::config::{atomic_write, now_ms, store_dir};

/// 服务静态定义。各服务的配置差异全在这张表里。
struct ServiceDef {
    /// 前端与配置文件里的稳定 id（改名不影响已存配置）。
    id: &'static str,
    /// 界面显示名。
    label: &'static str,
    /// 默认监听端口。
    port: u16,
    /// 相对 root 的 exe 路径。
    exe_rel: &'static str,
    /// 启动时附加参数（2api server 需要 `-config config.json`）。
    args: &'static [&'static str],
    /// 本机对应的计划任务名（不存在则退回直接 spawn）。
    task: &'static str,
    /// /healthz 响应里标识本服务的字段值（身份校验基准）。
    expect_kind: &'static str,
    /// 说明一句话，给 UI 当副标题。
    desc: &'static str,
}

/// 本机网关服务清单。
///
/// 2026-09-23：Anthropic / Responses 两个协议端点已**内嵌进 2api 网关同端口**
/// （2api 侧 `internal/protocol/*` + `/v1/messages`、`/v1/responses`），原先的
/// 独立桥进程（:8787 / :8788，任务 `cc-anthropic-bridge` / `cc-responses-bridge`）
/// 已连回退链一起拆除（任务删除、exe 与 Python 脚本清除）⇒ 本清单只剩网关一项；
/// 三种协议全落在网关这一个端口上，转换由网关内部承担。
const SERVICES: &[ServiceDef] = &[
    ServiceDef {
        id: "2api",
        label: "2api 网关",
        port: 7863,
        exe_rel: "bin/wb2api-server.exe",
        args: &["-config", "config.json"],
        task: "wb2api-gateway",
        expect_kind: "workbuddy2api",
        desc: "账号池与 upstream；一个端口上提供 OpenAI Chat / Anthropic / Responses 三种协议",
    },
];

const CONFIG_FILE: &str = "gateway_services.json";

fn def_of(id: &str) -> Option<&'static ServiceDef> {
    SERVICES.iter().find(|s| s.id == id)
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 读取配置；缺失/损坏返回默认。
pub fn load_config() -> Value {
    load_config_at(&config_path())
}

/// 从指定文件读配置（@测试用：单测不得写真身会默认的 store_dir）。
fn load_config_at(path: &Path) -> Value {
    let mut out = json!({ "root": load_root_at(&config_path()), "services": load_overrides_at(&config_path()) });
    let root = load_root_at(path);
    if !root.is_empty() {
        out["root"] = json!(root);
    }
    out["services"] = load_overrides_at(path);
    out
}

fn config_path() -> PathBuf {
    store_dir().join(CONFIG_FILE)
}

fn load_overrides() -> Value {
    load_overrides_at(&config_path())
}

fn load_overrides_at(path: &Path) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return json!({});
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("services").cloned())
        .unwrap_or_else(|| json!({}))
}

/// 2api 部署根目录（exe 与 config.json 都在这下面）。
///
/// 优先级：配置里的 root → 环境变量 `WB2API_ROOT` → 常见安装路径探测。
/// `pub(crate)`：`gateway_protocol` 也靠它定位同一份 `config.json`。
pub(crate) fn load_root() -> String {
    load_root_at(&config_path())
}

fn load_root_at(path: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(s) = v.get("root").and_then(Value::as_str).map(str::trim) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    if let Ok(s) = std::env::var("WB2API_ROOT") {
        if !s.trim().is_empty() {
            return s.trim().to_string();
        }
    }
    detect_root().unwrap_or_default()
}

/// 默认在本仓库同源目录里找 workbuddy2api（本机布局 `D:/w-dev/wb/`）。
fn detect_root() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("w-dev").join("wb").join("workbuddy2api"));
    }
    if cfg!(windows) {
        candidates.push(PathBuf::from(r"D:\w-dev\wb\workbuddy2api"));
    }
    candidates
        .into_iter()
        .find(|p| p.join("config.json").is_file())
        .map(|p| p.to_string_lossy().to_string())
}

/// 保存 root 与逐服务 exe/task 覆盖。只保留已知字段，类型归一。
pub fn save_config(cfg: &Value) -> Result<Value, String> {
    save_config_at(&config_path(), cfg)
}

/// 写到指定文件（@测试用：避免单测改写用户真实配置）。
fn save_config_at(path: &Path, cfg: &Value) -> Result<Value, String> {
    let mut root = cfg
        .get("root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(load_root);

    // 顺手把 \ 换成 / 之外的分隔符差异消掉：存原文，读的时候再归一。
    if cfg!(windows) {
        root = root.replace('/', "\\");
    }

    let overrides = cfg.get("services").cloned().unwrap_or_else(|| json!({}));
    let mut services = json!({});
    for def in SERVICES {
        let cur = overrides.get(def.id).cloned().unwrap_or_else(|| json!({}));
        let exe = cur
            .get("exe")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let task = cur
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_string);
        let disabled = cur.get("disabled").and_then(Value::as_bool);
        let mut entry = json!({});
        if let Some(exe) = exe {
            entry["exe"] = json!(exe);
        }
        if let Some(task) = task {
            entry["task"] = json!(task);
        }
        if let Some(disabled) = disabled {
            entry["disabled"] = json!(disabled);
        }
        services[def.id] = entry;
    }

    let saved = json!({ "root": root, "services": services, "updatedAt": now_ms() });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(path, &serde_json::to_string_pretty(&saved).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok(load_config_at(path))
}

// ---------------------------------------------------------------------------
// 健康检查
// ---------------------------------------------------------------------------

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        // 本机回环请求必须绕开系统代理（坑 19：WARP 会把 127.0.0.1 也拦一道）。
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 同一个 client 给 `gateway_protocol` 的端点探活复用（no_proxy 这条容易被漏掉）。
pub(crate) fn local_http_client() -> reqwest::blocking::Client {
    http_client()
}

/// 服务默认端口；未知 id 返回 None。
pub(crate) fn service_port(id: &str) -> Option<u16> {
    def_of(id).map(|d| d.port)
}

/// 裸 TCP 连通性探测。不用 reqwest 是因为要区分「端口没开」和「HTTP 层出错」。
pub(crate) fn port_open(port: u16) -> (bool, u128) {
    let started = Instant::now();
    let addr = match ("127.0.0.1", port).to_socket_addrs() {
        Ok(mut a) => match a.next() {
            Some(a) => a,
            None => return (false, 0),
        },
        Err(_) => return (false, 0),
    };
    let ok = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok();
    (ok, started.elapsed().as_millis())
}

/// 单服务深度体检。
///
/// 返回 `health` ∈ `ok` / `degraded`（端口通但服务不对或 HTTP 异常）/ `down`，
/// 外加给前端展示的延迟、原始响应摘要与 pid。
fn health_of(def: &ServiceDef, root: &str, pid_map: &HashMap<String, u32>) -> Value {
    let (open, _) = port_open(def.port);
    let mut out = json!({
        "id": def.id,
        "label": def.label,
        "port": def.port,
        "desc": def.desc,
        "portOpen": open,
        "httpOk": false,
        "kindMatch": false,
        "kind": Value::Null,
        "health": if open { "degraded" } else { "down" },
        "latencyMs": 0,
        "pid": pid_map.get(def.id).copied(),
        "message": Value::Null,
    });

    let exe_path = exe_path_of(def, root);
    out["exe"] = json!(exe_path.as_ref().map(|p| p.to_string_lossy().to_string()));
    out["exeExists"] = json!(exe_path.as_ref().map(|p| p.is_file()).unwrap_or(false));

    if !open {
        out["message"] = json!("端口没开，服务还没起来");
        return out;
    }

    let url = format!("http://127.0.0.1:{}/healthz", def.port);
    let started = Instant::now();
    match http_client().get(&url).send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let latency = started.elapsed().as_millis();
            out["latencyMs"] = json!(latency);
            out["status"] = json!(status);
            let body = resp.text().unwrap_or_default();
            let parsed: Option<Value> = serde_json::from_str(&body).ok();
            let http_ok = (200..300).contains(&status);
            out["httpOk"] = json!(http_ok);

            let kind = parsed
                .as_ref()
                .and_then(|v| v.get("kind").or_else(|| v.get("service")))
                .and_then(Value::as_str)
                .map(str::to_string);
            out["kind"] = json!(kind.clone());
            let kind_match = kind.as_deref() == Some(def.expect_kind);
            out["kindMatch"] = json!(kind_match);

            out["health"] = json!(if http_ok && kind_match {
                "ok"
            } else {
                "degraded"
            });
            out["message"] = json!(if !http_ok {
                format!("健康检查返回 HTTP {}", status)
            } else if !kind_match {
                match kind {
                    Some(k) => format!("{} 端口上跑的是别的服务（{}）", def.port, k),
                    None => format!("{} 端口上的服务没报身份", def.port),
                }
            } else {
                "正常".to_string()
            });
        }
        Err(e) => {
            out["message"] = json!(format!("健康检查请求失败：{}", e));
        }
    }
    out
}

/// 全部服务一体体检（逐项一次 TCP + 一次 HTTP）。
///
/// 返回 `{ ok, root, services[], summary{ok,degraded,down}, checkedAt }`。
pub fn gateway_services_status() -> Value {
    let root = load_root();
    let pid_map = load_pids();
    let services: Vec<Value> = SERVICES
        .iter()
        .map(|def| health_of(def, &root, &pid_map))
        .collect();
    let mut ok = 0usize;
    let mut degraded = 0usize;
    let mut down = 0usize;
    for s in &services {
        match s.get("health").and_then(Value::as_str).unwrap_or("down") {
            "ok" => ok += 1,
            "degraded" => degraded += 1,
            _ => down += 1,
        }
    }
    json!({
        "root": root,
        "services": services,
        "summary": { "ok": ok, "degraded": degraded, "down": down },
        "checkedAt": now_ms(),
    })
}

// ---------------------------------------------------------------------------
// 启停
// ---------------------------------------------------------------------------

fn exe_path_of(def: &ServiceDef, root: &str) -> Option<PathBuf> {
    let overrides = load_overrides();
    if let Some(s) = overrides
        .get(def.id)
        .and_then(|v| v.get("exe"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(PathBuf::from(s));
    }
    if root.is_empty() {
        return None;
    }
    Some(Path::new(root).join(def.exe_rel))
}

fn task_of(def: &ServiceDef) -> String {
    load_overrides()
        .get(def.id)
        .and_then(|v| v.get("task"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| def.task.to_string())
}

fn is_disabled(def: &ServiceDef) -> bool {
    load_overrides()
        .get(def.id)
        .and_then(|v| v.get("disabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 起子进程。Windows 上隐藏窗口并尽量脱离当前 Job（会话内直接拉的常驻进程会被 Job 清杀）。
fn spawn_hidden(program: &Path, args: &[String], cwd: &Path) -> Result<u32, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        // CREATE_BREAKAWAY_FROM_JOB：任务 Job 不允许时会失败，需回落。
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(child) => return Ok(child.id()),
            Err(_) => {
                let mut cmd = std::process::Command::new(program);
                cmd.args(args)
                    .current_dir(cwd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW);
                let child = cmd.spawn().map_err(|e| e.to_string())?;
                return Ok(child.id());
            }
        }
    }
    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(child.id())
    }
}

/// Windows 计划任务是否存在（`schtasks /query` 成功即存在）。
#[cfg(windows)]
fn task_exists(task: &str) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("schtasks")
        .args(["/query", "/tn", task])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 非 Windows 没有 schtasks：一律退回直接 spawn。
#[cfg(not(windows))]
fn task_exists(_task: &str) -> bool {
    false
}

fn run_cmd(program: &str, args: &[String]) -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let status = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| e.to_string())?;
        Ok(status.success())
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        Ok(status.success())
    }
}

const PID_FILE: &str = "gateway_services_pids.json";

fn pid_path() -> PathBuf {
    store_dir().join(PID_FILE)
}

fn load_pids() -> HashMap<String, u32> {
    let Ok(text) = std::fs::read_to_string(pid_path()) else {
        return HashMap::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if let Some(pid) = val.as_u64().and_then(|n| u32::try_from(n).ok()) {
                if pid_alive(pid) {
                    map.insert(k.clone(), pid);
                }
            }
        }
    }
    map
}

fn save_pid(id: &str, pid: u32) {
    let mut map = load_pids();
    map.insert(id.to_string(), pid);
    let json: Value = map.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let _ = atomic_write(&pid_path(), &serde_json::to_string(&json).unwrap_or_default());
}

fn drop_pid(id: &str) {
    let mut map = load_pids();
    map.remove(id);
    let json: Value = map.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let _ = atomic_write(&pid_path(), &serde_json::to_string(&json).unwrap_or_default());
}

/// pid 是否还活着。Windows 用 tasklist 精准过滤（`/FI PID eq` 无匹配时不含该 pid）。
fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// 端口监听者的 pid（停止兜底用）。Windows 走 netstat，其余走 lsof。
fn pid_listening_on(port: u16) -> Vec<u32> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("netstat")
            .args(["-ano", "-p", "tcp"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let Ok(o) = out else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        let needle = format!("127.0.0.1:{} ", port);
        let needle_any = format!("0.0.0.0:{} ", port);
        let mut pids: Vec<u32> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with("TCP") || !line.contains("LISTENING") {
                continue;
            }
            if !line.contains(&needle) && !line.contains(&needle_any) {
                continue;
            }
            if let Some(pid) = line.split_whitespace().last().and_then(|s| s.parse::<u32>().ok()) {
                if pid != 0 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
        pids
    }
    #[cfg(not(windows))]
    {
        let out = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{}", port)])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// 等端口开/闭。用于 start/stop 后的结果确认。
fn wait_port(port: u16, want_open: bool, timeout_ms: u64) -> bool {
    let step = Duration::from_millis(250);
    let mut waited = 0u64;
    while waited < timeout_ms {
        let (open, _) = port_open(port);
        if open == want_open {
            return true;
        }
        std::thread::sleep(step);
        waited += step.as_millis() as u64;
    }
    // 最后一次再判一次，避免刚好差一轮
    let (open, _) = port_open(port);
    open == want_open
}

/// 启动单个服务。
///
/// 流程：本机已在跑 → 直接返回 `already_running`；禁用清单里 → 拒绝；
/// 计划任务存在 → `schtasks /run` 下发；否则 exe 存在 → 直接 spawn。
pub fn gateway_service_start(id: &str) -> Result<Value, String> {
    let Some(def) = def_of(id) else {
        return Err(format!("没有叫 {} 的服务", id));
    };
    if is_disabled(def) {
        return Err(format!("{} 被标记为不在此处管理，先在设置里放开", def.label));
    }
    let root = load_root();
    if port_open(def.port).0 {
        return Ok(json!({
            "id": def.id, "action": "already_running", "port": def.port,
            "message": format!("{} 已经在跑着了", def.label),
        }));
    }

    let task = task_of(def);
    let mut via = None;
    if task_exists(&task) {
        let ok = run_cmd("schtasks", &["/run".into(), "/tn".into(), task.clone()])?;
        if wait_port(def.port, true, 6000) {
            via = Some(format!("schtasks:{}", task));
        } else if !ok {
            log_line(&format!("计划任务 {} 下发失败，尝试直接拉起 exe", task));
        }
    }

    if via.is_none() {
        let exe = exe_path_of(def, &root).ok_or_else(|| {
            format!("没找到 {} 的程序：先把 2api 根目录指过去", def.label)
        })?;
        if !exe.is_file() {
            return Err(format!(
                "{} 的程序不在那儿：{}\n在本页下面的「程序目录」里填上 2api 安装路径",
                def.label,
                exe.display()
            ));
        }
        let cwd = PathBuf::from(if root.is_empty() {
            exe.parent()
                .map(|p| p.parent().map(|q| q.to_path_buf()).unwrap_or_default())
                .unwrap_or_default()
        } else {
            PathBuf::from(root)
        });
        let args: Vec<String> = def.args.iter().map(|s| s.to_string()).collect();
        let pid = spawn_hidden(&exe, &args, &cwd)?;
        save_pid(def.id, pid);
        via = Some(format!("spawn(pid={})", pid));
        if !wait_port(def.port, true, 8000) {
            return Ok(json!({
                "id": def.id, "action": "started_not_confirmed", "port": def.port,
                "via": via, "pid": pid,
                "message": format!("{} 已拉起，但端口还没响应，稍后再刷新看看", def.label),
            }));
        }
    }

    Ok(json!({
        "id": def.id, "action": "started", "port": def.port, "via": via,
        "message": format!("{} 起来了", def.label),
    }))
}

/// 停止单个服务。
///
/// 流程：先 `schtasks /end`（任务 typing 存在时），再按端口 PID `taskkill /F`，
/// 等端口真的关闭才返回成功 —— **判据是端口，不是命令退出码**（命令会报告幕成功）。
pub fn gateway_service_stop(id: &str) -> Result<Value, String> {
    let Some(def) = def_of(id) else {
        return Err(format!("没有叫 {} 的服务", id));
    };
    let task = task_of(def);
    let mut actions: Vec<String> = Vec::new();

    if task_exists(&task) {
        let ok = run_cmd("schtasks", &["/end".into(), "/tn".into(), task.clone()])?;
        actions.push(format!("schtasks /end {}{}", task, if ok { "" } else { "（未成功）" }));
    }

    let pids: Vec<u32> = pid_listening_on(def.port)
        .into_iter()
        .chain(load_pids().get(def.id).copied())
        .collect();
    let mut seen: Vec<u32> = Vec::new();
    for pid in pids {
        if pid == 0 || seen.contains(&pid) {
            continue;
        }
        seen.push(pid);
        #[cfg(windows)]
        let ok = run_cmd(
            "taskkill",
            &["/PID".into(), pid.to_string(), "/F".into()],
        )?;
        #[cfg(not(windows))]
        let ok = run_cmd("kill", &["-9".into(), pid.to_string()])?;
        actions.push(format!("kill pid {}{}", pid, if ok { "" } else { "（未成功）" }));
    }
    drop_pid(def.id);

    let stopped = wait_port(def.port, false, 8000);
    if !stopped && actions.is_empty() {
        return Ok(json!({
            "id": def.id, "action": "not_running", "port": def.port,
            "message": format!("{} 本来就没在跑", def.label),
        }));
    }
    Ok(json!({
        "id": def.id,
        "action": if stopped { "stopped" } else { "stop_not_confirmed" },
        "port": def.port,
        "actions": actions,
    }))
}

/// 重启单个服务：停 → 等端口真关 → 起 → 等端口真开。
///
/// 为什么必须有它：网关的**协议端点开关在启动时读一次**（`config.json` 的
/// `protocol_endpoints`），改完配置不重启 = 开关不生效（假开关）。凡是「改配置」
/// 类的入口，保存后都要走这一条。
pub fn gateway_service_restart(id: &str) -> Result<Value, String> {
    let Some(def) = def_of(id) else {
        return Err(format!("没有叫 {} 的服务", id));
    };
    let stop = gateway_service_stop(id)?;
    let start = gateway_service_start(id)?;
    let up = wait_port(def.port, true, 8000);
    Ok(json!({
        "id": def.id,
        "action": if up { "restarted" } else { "restart_not_confirmed" },
        "port": def.port,
        "stop": stop,
        "start": start,
        "message": if up {
            format!("{} 重启完成", def.label)
        } else {
            format!("{} 重启后端口还没起来，稍等一下或点「拉起」", def.label)
        },
    }))
}

/// 一键：全部拉起（先看各自是否在跑，幂等）。
pub fn gateway_services_start_all() -> Value {
    let mut results = Vec::new();
    for def in SERVICES {
        if is_disabled(def) {
            results.push(json!({"id": def.id, "action": "skipped", "message": "已标记为不管"}));
            continue;
        }
        results.push(match gateway_service_start(def.id) {
            Ok(v) => v,
            Err(e) => json!({"id": def.id, "action": "failed", "error": e}),
        });
    }
    json!({ "results": results, "status": gateway_services_status() })
}

/// 一行日志（追加到 store_dir 下），失败不影响主流程。
fn log_line(msg: &str) {
    let path = store_dir().join("gateway_services.log");
    let line = format!("[{}] {}\n", chrono::Local::now().format("%H:%M:%S"), msg);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defs_have_unique_ids_and_ports() {
        let mut ids = std::collections::HashSet::new();
        let mut ports = std::collections::HashSet::new();
        for def in SERVICES {
            assert!(ids.insert(def.id), "id 重复：{}", def.id);
            assert!(ports.insert(def.port), "端口重复：{}", def.port);
        }
        // 2026-09-23 内嵌后只剩网关一项（Anthropic / Responses 端点并入同端口）。
        assert_eq!(SERVICES.len(), 1);
    }

    #[test]
    fn unknown_id_is_rejected() {
        assert!(def_of("nope").is_none());
        assert!(gateway_service_start("nope").is_err());
        assert!(gateway_service_stop("nope").is_err());
    }

    /// ⚠️ 单测**不许**调 `save_config()`：那会改写用户真实的
    /// `~/.wb-switch/gateway_services.json`（改过的 disabled 标记会让本机的 real service 起不来）。
    /// 所以这里一律走 `*\_at` 的版本写到临时目录。
    #[test]
    fn save_config_keeps_only_known_services() {
        let tmp = std::env::temp_dir().join(format!(
            "wb-switch-test-{}-{:?}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let cfg = json!({
            "root": "  D:/tmp/wb2api  ",
            "services": {
                "2api": { "exe": "D:/x/server.exe", "disabled": true },
                "ghost": { "exe": "D:/x/ghost.exe" }
            }
        });
        let saved = save_config_at(&tmp, &cfg).expect("save");
        let _ = std::fs::remove_file(&tmp);

        let services = saved.get("services").unwrap().as_object().unwrap();
        assert!(services.get("ghost").is_none(), "未知服务不得写进配置");
        assert_eq!(services["2api"]["exe"], json!("D:/x/server.exe"));
        assert_eq!(services["2api"]["disabled"], json!(true));
        // 真实配置不该被单测改写
        let real = load_overrides();
        assert!(
            real.get("2api").and_then(|v| v.get("exe")).is_none(),
            "单测污染了真实配置：{:?}",
            real
        );
    }

    #[test]
    fn status_reports_down_for_unused_port() {
        // 用一个几乎不可能被占用的高位端口：必须被判为 down，而不是 panic。
        let def = ServiceDef {
            id: "probe",
            label: "探针",
            port: 59999,
            exe_rel: "bin/none.exe",
            args: &[],
            task: "no-such-task",
            expect_kind: "probe",
            desc: "单测用",
        };
        let out = health_of(&def, "", &HashMap::new());
        assert_eq!(out["health"], json!("down"));
        assert_eq!(out["portOpen"], json!(false));
        assert_eq!(out["exeExists"], json!(false));
    }

    /// pid 生死判定（`tasklist` / `kill -0` 的解析都得对）：
    /// 起一个真进程 → 必须判定活着 → 杀掉后必须判定死了；越界 pid 不得 panic。
    /// 反向验证意义：这段判据写反 = stop 永远「成功」却没人真停。
    ///
    /// ⚠️ 手动跑（2026-09-25 起 ignore）：CI 共享 runner 上对刚收割的 pid 做
    /// `kill -0`，僵尸回收 / pid 复用语义不可控 ⇒ 偶发假红（ubuntu 实证）。
    /// 与 gateway_services_live / start_stop_roundtrip 同类：依赖真实进程环境。
    #[test]
    #[ignore = "依赖真实进程生死语义，CI runner 不可控；手动跑"]
    fn pid_alive_tracks_real_process() {
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 6 127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cmd");
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");

        let pid = child.id();
        assert!(pid > 0);
        assert!(pid_alive(pid), "子进程还活着时应判定 alive");
        let _ = child.kill();
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && pid_alive(pid) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!pid_alive(pid), "子进程已杀时应判定 not alive");
        assert!(!pid_alive(u32::MAX), "不存在的 pid 不得 panic，必须判死");
    }

    /// 端口归属查询：本进程起一个监听 → `pid_listening_on` 必须能找到本 pid。
    /// stop 的兜底路径完全依赖这一段，写错会让「停了还在跑」。
    #[test]
    fn listening_pid_is_found_and_cleared() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let me = std::process::id();
        let pids = pid_listening_on(port);
        assert!(
            pids.contains(&me),
            "端口 {} 应由本进程({})监听，实际查到 {:?}",
            port,
            me,
            pids
        );
        drop(listener);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && pid_listening_on(port).contains(&me) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !pid_listening_on(port).contains(&me),
            "监听关闭后不应还能查到本 pid"
        );
    }

    /// `wait_port` 的两种方向都必须能很快返回（poll 写反会白等 timeout 一轮）。
    #[test]
    fn wait_port_returns_both_directions() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        // 无人监听：等"关闭"应立刻成立
        assert!(wait_port(port, false, 1500));
        let keep = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind again");
        assert!(wait_port(port, true, 1500));
        drop(keep);
    }

    /// 启停闭环：**真的停、真的起**（依赖本机服务在跑；结束态仍是"在跑"）。
    /// 清单直接遍历 `SERVICES`，服务增删不用同步改这里。
    /// 手动跑：`cargo test -p wb-switch-core start_stop_roundtrip -- --ignored --nocapture`
    #[test]
    #[ignore = "会真的停掉本机服务，手动跑"]
    fn start_stop_roundtrip() {
        for def in SERVICES {
            let id = def.id;
            let before = port_open(def.port).0;
            let stopped = gateway_service_stop(id).expect("stop ok");
            assert!(
                !port_open(def.port).0,
                "{} 停止后端口仍是通的：{:?}",
                id, stopped
            );
            let started = gateway_service_start(id).expect("start ok");
            assert!(
                wait_port(def.port, true, 10_000),
                "{} 启动后端口没起来：{:?}",
                id, started
            );
            println!(
                "{} before={} stop={} start={}",
                id, before, stopped["action"], started["action"]
            );
        }
    }

    /// 真实本机体检（只读）。默认 ignore —— CI 上没有 2api 环境，并且它依赖本机服务在跑。
    /// 手动跑：`cargo test -p wb-switch-core gateway_services_live -- --ignored`
    #[test]
    #[ignore = "依赖本机真实服务，手动跑"]
    fn gateway_services_live() {
        let st = gateway_services_status();
        // 条数跟 SERVICES 走，不写死（清单增减时这里不该误报）。
        assert_eq!(
            st["services"].as_array().map(|a| a.len()),
            Some(SERVICES.len())
        );
        assert!(!st["root"].as_str().unwrap_or("").is_empty());
        for s in st["services"].as_array().unwrap() {
            assert!(s.get("health").is_some(), "缺 health：{}", s);
            println!(
                "{} :{} => {} {}",
                s["label"], s["port"], s["health"], s["message"]
            );
        }
    }
}
