//! 2api 网关的**协议端点**（Anthropic Messages / OpenAI Responses）· 开关与探活。
//!
//! 背景：网关本体只说 OpenAI Chat。Claude Code 说 Anthropic Messages、codex 说
//! Responses —— 2026-09-23 起这两个转换层**内嵌进网关同端口**（:7863），由
//! `config.json` 的 `protocol_endpoints.{enabled,anthropic,responses}` 控制挂不挂。
//! 缺省（或不写）＝ 全开；配置坏 / 缺文件**回落全开**（这是 2api 的设计，别在前端
//! 把它显示成"没配就是关"）。
//!
//! 本模块负责三件事：
//! 1. **读**：解析那份配置里的开关（含「有效值」= `enabled && 各自` 的 AND 语义）；
//! 2. **探活**：`GET /v1/messages`、`GET /v1/responses` —— 判据是响应头 `X-Service`
//!    （`anthropic-messages` / `openai-responses`）。端点对未知路径有 404 兜底且**带**
//!    这个头，网关本体对未知路径只有默认 404、无该头 ⇒ 打一个只支持 POST 的路径就能
//!    判定「挂没挂上」，**不触上游、不耗额度、不进号池台账**（零副作用，与 2api 的
//!    `scripts/port_check.py` 同一判据）。
//! 3. **写**：改开关 —— **最小编辑**那份配置文件，只动 `protocol_endpoints` 这一个顶层
//!    成员，其余字节原样保留（缩进 / 换行风格 / 键序都不重排）。⛔ 别用「整文件重新
//!    序列化」的写法：那会把别人生成的配置全文件重排，diff 从一行变成全文件。
//!
//! ⚠️ **改完必须重启网关才生效** —— 2api 侧 `mount.Load` 在**启动时读一次**。
//! 所以「保存」之后一定要跟一次 `gateway_services::gateway_service_restart("2api")`，
//! 否则就是一个不生效的假开关。
//!
//! ⚠️ 环境变量 `WB2A_PROTOCOL_ENDPOINTS=off` 会**整体覆盖**文件开关（排障用，2api 侧
//! `mount.Load` 里写死）。本机实测计划任务与用户/系统环境变量**都没有**它；若哪天有人
//! 加上，文件里怎么改都不生效 —— 所以状态里带上 `envOverride` 供前端提示。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, now_ms};
use crate::modules::gateway_services;

/// 端点定义表。`id` 与 2api 配置里的字段名一致（`anthropic` / `responses`）。
struct EndpointDef {
    id: &'static str,
    label: &'static str,
    /// 探活路径（只支持 POST，GET 打上去落端点自己的 404 兜底，零副作用）。
    path: &'static str,
    /// 响应头 `X-Service` 的期望值 —— 「挂上了」的唯一判据。
    expect_service: &'static str,
    /// 走这条协议的客户端，给 UI 显示。
    client: &'static str,
}

const ENDPOINTS: &[EndpointDef] = &[
    EndpointDef {
        id: "anthropic",
        label: "Anthropic Messages",
        path: "/v1/messages",
        expect_service: "anthropic-messages",
        client: "Claude Code · codeg",
    },
    EndpointDef {
        id: "responses",
        label: "OpenAI Responses",
        path: "/v1/responses",
        expect_service: "openai-responses",
        client: "codex",
    },
];

/// 网关服务 id（端口与启停都从 `gateway_services` 取，别在这儿写第二份端口）。
const GATEWAY_ID: &str = "2api";
/// 2api 配置文件名（启动器 `run_gateway_hidden.pyw` 里也是 `<root>/config.json`).
const CONFIG_FILE: &str = "config.json";
/// 配置里的节名。
const SECTION_KEY: &str = "protocol_endpoints";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

fn config_path() -> PathBuf {
    config_path_from(&gateway_services::load_root())
}

fn config_path_from(root: &str) -> PathBuf {
    Path::new(root).join(CONFIG_FILE)
}

// ---------------------------------------------------------------------------
// 读：开关
// ---------------------------------------------------------------------------

/// 读开关。返回 `(gates, configured)` —— `configured=false` 表示配置里**没有**这一节
/// （2api 的语义是「全开」，不是「全关」）。
fn read_gates(text: &str) -> (Value, bool) {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    let section = parsed.as_ref().and_then(|v| v.get(SECTION_KEY));
    let configured = section.is_some();
    let get = |k: &str| -> bool {
        section
            .and_then(|s| s.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(true) // 缺失 / 类型不对 ⇒ 按 2api 的 on() 口径当"开"
    };
    let enabled = get("enabled");
    let anthropic = get("anthropic");
    let responses = get("responses");
    (
        json!({
            "enabled": enabled,
            "anthropic": anthropic,
            "responses": responses,
            // 有效值：2api 侧是 enabled && 各自 ⇒ 前端展示这个，别让"总闸关着但分闸开着"看着像能用
            "anthropicEffective": enabled && anthropic,
            "responsesEffective": enabled && responses,
        }),
        configured,
    )
}

// ---------------------------------------------------------------------------
// 探活
// ---------------------------------------------------------------------------

/// 单端点探活。`mounted` 才是结论，`status` / `service` 只作证据展示。
fn probe(def: &EndpointDef, port: u16) -> Value {
    let url = format!("http://127.0.0.1:{}{}", port, def.path);
    let mut out = json!({
        "id": def.id,
        "label": def.label,
        "path": def.path,
        "client": def.client,
        "mounted": false,
        "status": Value::Null,
        "service": Value::Null,
        "detail": Value::Null,
    });
    match gateway_services::local_http_client().get(&url).send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let service = resp
                .headers()
                .get("x-service")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            out["status"] = json!(status);
            out["service"] = json!(service.clone());
            let mounted = service.as_deref() == Some(def.expect_service);
            out["mounted"] = json!(mounted);
            out["detail"] = json!(if mounted {
                "已挂上".to_string()
            } else {
                match service {
                    Some(s) => format!("身份不符：期望 {}，实得 {}", def.expect_service, s),
                    None => format!("HTTP {} 但没带身份头 —— 这个端点没挂上", status),
                }
            });
        }
        Err(e) => {
            out["detail"] = json!(format!("探不到（网关可能没在跑）：{}", e));
        }
    }
    out
}

/// 状态总览：开关 + 探活 + 一处提示用的元信息。
pub fn gateway_protocol_status() -> Value {
    let root = gateway_services::load_root();
    let path = config_path_from(&root);
    let port = gateway_services::service_port(GATEWAY_ID).unwrap_or(7863);

    let text = std::fs::read_to_string(&path).ok();
    let (gates, configured) = match text.as_deref() {
        Some(t) => read_gates(t),
        None => read_gates(""),
    };

    // 环境变量覆盖：只在**本进程能看到**时提示（网关进程自己的环境可能不同，所以措辞保留余地）
    let env_override = std::env::var("WB2A_PROTOCOL_ENDPOINTS")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false);

    let endpoints: Vec<Value> = ENDPOINTS.iter().map(|d| probe(d, port)).collect();
    json!({
        "root": root,
        "configPath": path.to_string_lossy().to_string(),
        "configExists": path.is_file(),
        "port": port,
        "gates": gates,
        "configured": configured,
        "envOverride": env_override,
        "endpoints": endpoints,
        "checkedAt": now_ms(),
    })
}

// ---------------------------------------------------------------------------
// 写：最小编辑
// ---------------------------------------------------------------------------

/// 探测原文的换行风格。
fn detect_eol(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// 探测原文的顶层缩进（第一个非空、非 `}` 开头的行的前导空白）。
fn detect_indent(text: &str) -> String {
    for line in text.split('\n').skip(1) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('}') {
            continue;
        }
        let ws = &line[..line.len() - trimmed.len()];
        if !ws.is_empty() {
            return ws.replace('\r', "");
        }
    }
    "  ".to_string()
}

/// 从 `start`（值的第一个字符）找到值的结束位置（排他）。
fn end_of_value(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    if start >= len {
        return Some(len);
    }
    match bytes[start] {
        b'{' | b'[' => {
            let (open, close) = (bytes[start], if bytes[start] == b'{' { b'}' } else { b']' });
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut i = start;
            while i < len {
                let c = bytes[i];
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                    i += 1;
                    continue;
                }
                match c {
                    b'"' => in_str = true,
                    x if x == open => depth += 1,
                    x if x == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None // 括号不配平
        }
        b'"' => {
            let mut i = start + 1;
            let mut esc = false;
            while i < len {
                let c = bytes[i];
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            None
        }
        _ => {
            // 数字 / true / false / null：读到 `,` 或 `}` 之前
            let mut i = start;
            while i < len && bytes[i] != b',' && bytes[i] != b'}' && bytes[i] != b']' {
                i += 1;
            }
            while i > start && bytes[i - 1].is_ascii_whitespace() {
                i -= 1;
            }
            Some(i)
        }
    }
}

/// 顶层成员扫描结果。
struct TopLevelScan {
    /// 同名顶层成员的值区间 `(值起始, 值结束)`。
    target: Option<(usize, usize)>,
    /// 最后一个顶层成员的值结束位置（新增时插在它后面）。
    last_end: Option<usize>,
}

/// 扫一遍顶层成员。字符串里的 `{` `}` `:` 与嵌套对象一律不误判。
fn scan_top_level(text: &str) -> TopLevelScan {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut out = TopLevelScan {
        target: None,
        last_end: None,
    };
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < len {
        let c = bytes[i];
        match c {
            b'"' => {
                let s_start = i + 1;
                let mut j = s_start;
                let mut esc = false;
                while j < len {
                    let d = bytes[j];
                    if esc {
                        esc = false;
                    } else if d == b'\\' {
                        esc = true;
                    } else if d == b'"' {
                        break;
                    }
                    j += 1;
                }
                let k = j + 1; // 跳过闭合引号
                // 只有「后面紧跟冒号」才是键；值位置的字符串不会命中
                let mut probe = k;
                while probe < len && bytes[probe].is_ascii_whitespace() {
                    probe += 1;
                }
                if depth == 1 && probe < len && bytes[probe] == b':' {
                    let key = &text[s_start..j];
                    let mut v_start = probe + 1;
                    while v_start < len && bytes[v_start].is_ascii_whitespace() {
                        v_start += 1;
                    }
                    if let Some(v_end) = end_of_value(text, v_start) {
                        out.last_end = Some(v_end);
                        if key == SECTION_KEY {
                            out.target = Some((v_start, v_end));
                        }
                        i = v_end;
                        continue;
                    }
                }
                i = k;
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// 把 serde 的 pretty 输出对齐到原文风格：多行，首行不加缩进，其余行补 `indent`。
fn render_value(value: &Value, eol: &str, indent: &str) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    let mut lines = pretty.split('\n');
    let first = lines.next().unwrap_or("{}").to_string();
    let rest: Vec<String> = lines
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                format!("{}{}", indent, l)
            }
        })
        .collect();
    std::iter::once(first)
        .chain(rest)
        .collect::<Vec<_>>()
        .join(eol)
}

/// 最小编辑：替换或新增一个顶层成员，其余字节原样保留。
fn upsert_top_level_member(text: &str, key: &str, value: &Value) -> Result<String, String> {
    let eol = detect_eol(text);
    let indent = detect_indent(text);
    let rendered = render_value(value, eol, &indent);
    let scan = scan_top_level(text);

    if let Some((v_start, v_end)) = scan.target {
        let mut out = String::with_capacity(text.len() + rendered.len());
        out.push_str(&text[..v_start]);
        out.push_str(&rendered);
        out.push_str(&text[v_end..]);
        return Ok(out);
    }

    let insert_at = scan
        .last_end
        .ok_or_else(|| "配置文件里找不到顶层对象，没敢动".to_string())?;
    let mut out = String::with_capacity(text.len() + rendered.len() + 32);
    out.push_str(&text[..insert_at]);
    out.push(',');
    out.push_str(eol);
    out.push_str(&indent);
    out.push('"');
    out.push_str(key);
    out.push_str("\": ");
    out.push_str(&rendered);
    out.push_str(&text[insert_at..]);
    Ok(out)
}

/// 归一化用户传入的开关（只认三个键，缺省 = true）。
fn normalize_gates(input: &Value) -> Value {
    let get = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(true);
    json!({
        "enabled": get("enabled"),
        "anthropic": get("anthropic"),
        "responses": get("responses"),
    })
}

/// 保存开关（⚠️ 要跟一次 `gateway_service_restart` 才生效）。
pub fn protocol_gates_save(gates: &Value) -> Result<Value, String> {
    protocol_gates_save_at(&config_path(), gates)
}

/// 写到指定配置文件（@测试用：单测绝不碰用户真实的 config.json）。
fn protocol_gates_save_at(path: &Path, gates: &Value) -> Result<Value, String> {
    if !path.is_file() {
        return Err(format!(
            "没找到 2api 的配置文件：{}\n先在「网关接口」卡里把 2api 程序目录指对",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("读配置失败：{}", e))?;
    let wanted = normalize_gates(gates);

    let before = read_gates(&text).0;
    let new_text = upsert_top_level_member(&text, SECTION_KEY, &wanted)?;

    // ★ 写前自检（反向断言）：新文本必须能解析，且解析出来的节与预期逐字一致。
    // 不做这一步，一旦拼装出错就是把网关配置写坏 —— 那比不写更糟。
    let check: Value = serde_json::from_str(&new_text)
        .map_err(|e| format!("改完的配置解析不过（已放弃写入）：{}", e))?;
    let got_section = check.get(SECTION_KEY).cloned().unwrap_or(Value::Null);
    if got_section != wanted {
        return Err(format!(
            "改完的配置自检没过（已放弃写入）：期望 {:?}，实得 {:?}",
            wanted, got_section
        ));
    }

    // ⚠️ 必须拿**基础三键**比：`read_gates` 的结果还带 `*Effective` 派生字段，
    // 直接和 `wanted` 比会永远"不等" ⇒ 保存永不幂等（实测踩过）。
    let changed = normalize_gates(&before) != wanted;
    if !changed {
        return Ok(json!({
            "changed": false,
            "restartRequired": false,
            "gates": before,
            "backup": Value::Null,
            "message": "开关本来就是这样的，没动文件",
        }));
    }

    // 备份：改别人的文件前先留一份原件
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = path.with_extension(format!("json.bak-{}", stamp));
    std::fs::copy(path, &backup).map_err(|e| format!("备份失败，已放弃写入：{}", e))?;

    atomic_write(path, &new_text).map_err(|e| format!("写配置失败：{}", e))?;

    let saved = read_gates(&new_text).0;
    Ok(json!({
        "changed": true,
        "restartRequired": true,
        "gates": saved,
        "backup": backup.to_string_lossy().to_string(),
        "message": "开关存下了 —— 网关要重启一次才生效",
    }))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "{\r\n  \"listen\": \"127.0.0.1:7863\",\r\n  \"api_key\": \"k\",\r\n  \"admin\": {\r\n    \"enabled\": true\r\n  },\r\n  \"refresh_on_request\": false\r\n}";

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wb-switch-proto-{}-{}-{}.json",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// 没有该节 ⇒ 插入，且**其它字节与缩进 / 换行风格**都不能变。
    #[test]
    fn upsert_inserts_and_keeps_format() {
        let v = json!({ "enabled": true, "anthropic": false, "responses": true });
        let out = upsert_top_level_member(SAMPLE, SECTION_KEY, &v).expect("upsert");

        let parsed: Value = serde_json::from_str(&out).expect("parse");
        assert_eq!(parsed["protocol_endpoints"], v);
        // 原有键一个不动
        assert_eq!(parsed["listen"], json!("127.0.0.1:7863"));
        assert_eq!(parsed["admin"]["enabled"], json!(true));
        assert_eq!(parsed["refresh_on_request"], json!(false));
        // CRLF 与 2 空格缩进原样保留（全文件重排的写法会在这里露馅）
        assert!(out.contains("\r\n"), "换行风格被改成了 LF");
        assert!(out.contains("\r\n  \"protocol_endpoints\": {\r\n"), "顶层节缩进不对：\n{}", out);
        assert!(out.contains("\r\n    \"enabled\": true"), "节内缩进不对：\n{}", out);
        // 其它行的原文必须逐字保留
        assert!(out.contains("\"listen\": \"127.0.0.1:7863\""));
        // 新节插在**最后一个顶层成员之后**，顶层闭合仍在末尾
        assert!(
            out.contains("  \"refresh_on_request\": false,\r\n  \"protocol_endpoints\": {"),
            "插入位置不对：\n{}",
            out
        );
        assert!(out.trim_end().ends_with('}'), "顶层闭合丢了：\n{}", out);
    }

    /// 已有该节 ⇒ 原地替换，不留旧值。
    #[test]
    fn upsert_replaces_existing_section() {
        let text = "{\n  \"a\": 1,\n  \"protocol_endpoints\": { \"enabled\": true },\n  \"b\": 2\n}";
        let v = json!({ "enabled": false, "anthropic": false, "responses": false });
        let out = upsert_top_level_member(text, SECTION_KEY, &v).expect("upsert");

        let parsed: Value = serde_json::from_str(&out).expect("parse");
        assert_eq!(parsed["protocol_endpoints"], v);
        assert_eq!(parsed["a"], json!(1));
        assert_eq!(parsed["b"], json!(2));
        // 旧值不该残留
        assert!(!out.contains("\"protocol_endpoints\": { \"enabled\": true }"));
        // 顶层键名只出现一次
        assert_eq!(out.matches("\"protocol_endpoints\"").count(), 1);
    }

    /// 嵌套对象里出现同名键 ⇒ 只动顶层，别把子节的改掉。
    #[test]
    fn upsert_only_touches_top_level() {
        let text = "{\n  \"outer\": {\n    \"protocol_endpoints\": { \"enabled\": true }\n  },\n  \"z\": 9\n}";
        let v = json!({ "enabled": false, "anthropic": true, "responses": true });
        let out = upsert_top_level_member(text, SECTION_KEY, &v).expect("upsert");

        let parsed: Value = serde_json::from_str(&out).expect("parse");
        assert_eq!(parsed["protocol_endpoints"], v, "顶层没加上");
        assert_eq!(
            parsed["outer"]["protocol_endpoints"]["enabled"],
            json!(true),
            "嵌套里的同名节被误改"
        );
    }

    /// 字符串值里含 `}` 与键名 ⇒ 扫描不许被骗（否则会从字符串中间插入，把文件写坏）。
    #[test]
    fn upsert_survives_braces_in_strings() {
        let text = "{\n  \"note\": \"a } b { c \\\"protocol_endpoints\\\" : 1\",\n  \"n\": 2\n}";
        let v = json!({ "enabled": true, "anthropic": true, "responses": false });
        let out = upsert_top_level_member(text, SECTION_KEY, &v).expect("upsert");

        let parsed: Value = serde_json::from_str(&out).expect("parse");
        assert_eq!(parsed["protocol_endpoints"], v);
        assert_eq!(parsed["n"], json!(2));
        assert!(parsed["note"].as_str().unwrap().contains("a } b {"));
        // 新增的那节是真的新增（原字符串里那个不算）
        assert_eq!(parsed.as_object().unwrap().len(), 3);
    }

    /// 读开关：没配 ⇒ 全开（不是全关）；「总闸关」时有效值也必须为 false。
    #[test]
    fn read_gates_semantics() {
        let (g, configured) = read_gates("{}");
        assert!(!configured);
        assert_eq!(g["enabled"], json!(true));
        assert_eq!(g["anthropicEffective"], json!(true));

        let (g2, configured2) = read_gates("{\"protocol_endpoints\":{\"enabled\":false}}");
        assert!(configured2);
        assert_eq!(g2["enabled"], json!(false));
        // 分闸没写（默认开）但总闸关了 ⇒ 有效值必须是 false
        assert_eq!(g2["anthropic"], json!(true));
        assert_eq!(g2["anthropicEffective"], json!(false));
        assert_eq!(g2["responsesEffective"], json!(false));

        // 坏节（类型不对）⇒ 按"开"兜底，与 2api 的 on() 口径一致
        let (g3, _) = read_gates("{\"protocol_endpoints\": 12345}");
        assert_eq!(g3["anthropicEffective"], json!(true));
    }

    /// 写文件走临时路径：真配置一个字节都不许动（照 gateway_services 的 `*_at` 约定）。
    #[test]
    fn save_writes_temp_file_and_backs_up() {
        let path = tmp_path("save");
        std::fs::write(&path, SAMPLE).expect("seed");

        let v = json!({ "enabled": true, "anthropic": false, "responses": true });
        let res = protocol_gates_save_at(&path, &v).expect("save");
        assert_eq!(res["changed"], json!(true));
        assert_eq!(res["restartRequired"], json!(true));
        let backup = res["backup"].as_str().unwrap().to_string();
        assert!(Path::new(&backup).is_file(), "没留备份");

        let after = std::fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&after).expect("parse");
        assert_eq!(parsed["protocol_endpoints"], v);
        // 备份内容 = 改前的原文（逐字节）
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), SAMPLE);

        // 幂等：同样内容再存一次 ⇒ changed=false 且不再写文件 / 不再备份
        let again = protocol_gates_save_at(&path, &v).expect("save again");
        assert_eq!(again["changed"], json!(false));
        assert_eq!(again["restartRequired"], json!(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }

    /// 找不到配置文件 ⇒ 明确报错，不许"假装成功"。
    #[test]
    fn save_fails_loudly_without_config() {
        let path = tmp_path("missing");
        let err = protocol_gates_save_at(&path, &json!({"enabled": true})).unwrap_err();
        assert!(err.contains("没找到"), "错误文案没指向真实原因：{}", err);
    }

    /// 配置本来就坏 ⇒ 读成默认（全开）但写入要拒绝，别在坏文件上继续拼。
    #[test]
    fn save_refuses_broken_config() {
        let path = tmp_path("broken");
        std::fs::write(&path, "{ not json").expect("seed");
        // 坏文件里扫不到顶层成员 ⇒ 插入点找不到 ⇒ 报错而不是写坏
        let err = protocol_gates_save_at(&path, &json!({"enabled": false})).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json", "坏文件被动了");
        let _ = std::fs::remove_file(&path);
    }

    /// 真机只读体检（默认 ignore）：网关在跑时两个端点都该是「已挂上」。
    /// 手动跑：`cargo test -p wb-switch-core protocol_live -- --ignored --nocapture`
    #[test]
    #[ignore = "依赖本机真实网关，手动跑"]
    fn protocol_live() {
        let st = gateway_protocol_status();
        println!("config={} envOverride={}", st["configPath"], st["envOverride"]);
        for e in st["endpoints"].as_array().unwrap() {
            println!(
                "{} {} => mounted={} {}",
                e["label"], e["path"], e["mounted"], e["detail"]
            );
            assert_eq!(e["mounted"], json!(true), "端点没挂上：{}", e);
        }
    }

    /// 端到端反向验证（默认 ignore，会真改配置 + 真重启网关）：
    /// 关总闸 → 重启 → **必须探不到**（证明开关真生效、探活真有判别力）→
    /// 写回原值 → 重启 → 必须恢复。
    ///
    /// 顺序刻意「先恢复、后断言」：中间任何一步出意外，配置也已经回到原样，
    /// 不会把网关留在「只剩 OpenAI、Claude Code 连不上」的状态里。
    ///
    /// 手动跑：`cargo test -p wb-switch-core protocol_gates_roundtrip_live -- --ignored --nocapture`
    #[test]
    #[ignore = "会改 2api 配置并重启网关，手动跑"]
    fn protocol_gates_roundtrip_live() {
        let path = config_path();
        if !path.is_file() {
            println!("跳过：没找到 {}", path.display());
            return;
        }
        let st = gateway_protocol_status();
        let original = json!({
            "enabled": st["gates"]["enabled"],
            "anthropic": st["gates"]["anthropic"],
            "responses": st["gates"]["responses"],
        });

        // 关掉总闸（分闸保持原样 ⇒ 只验总闸这一条路径）
        let saved = protocol_gates_save(&json!({
            "enabled": false,
            "anthropic": original["anthropic"],
            "responses": original["responses"],
        }))
        .expect("关总闸失败");
        assert_eq!(saved["changed"], json!(true), "开关没写进文件：{:?}", saved);
        assert_eq!(saved["restartRequired"], json!(true));
        let r1 = gateway_services::gateway_service_restart(GATEWAY_ID).expect("重启失败");
        let after_off = gateway_protocol_status();
        let mounted_off: Vec<bool> = after_off["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["mounted"] == json!(true))
            .collect();

        // —— 先恢复现场 ——
        let back = protocol_gates_save(&original).expect("写回原值失败");
        let r2 = gateway_services::gateway_service_restart(GATEWAY_ID).expect("二次重启失败");
        let after_on = gateway_protocol_status();
        let mounted_on: Vec<bool> = after_on["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["mounted"] == json!(true))
            .collect();

        println!(
            "关总闸后 mounted={:?} · 写回后 mounted={:?}",
            mounted_off, mounted_on
        );
        println!(
            "restart1={} restart2={} backup={}",
            r1["action"], r2["action"], back["backup"]
        );

        assert!(
            mounted_off.iter().all(|m| !*m),
            "总闸关了端点还探得到 ⇒ 开关没生效或探活没判别力：{:?}",
            after_off
        );
        assert!(
            mounted_on.iter().all(|m| *m),
            "写回原值后端点没恢复：{:?}",
            after_on
        );
    }
}
