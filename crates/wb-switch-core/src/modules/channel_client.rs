//! 云端通道客户端（Centrifugo + AES-GCM 信封）——账号全域会话枚举的传输层。
//!
//! 协议来源：APK 逆向（`reports/mobile-apk-channel-protocol-2026-09-15.md`）。
//! 红线：只订阅 `user:<uid>:*` 读通道；`executor:<deviceId>` 设备级通道不碰。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use serde_json::{json, Value};

/// credentials 请求体。
///
/// `POST https://www.workbuddy.cn/edge-cloud/credentials`（根路径，无 `/console/as` 前缀），
/// Bearer = 账号 access_token。`clientType` 固定 desktop、`useDefaultScopes` 固定 true。
pub fn credentials_body(user_id: &str, device_id: &str, device_name: &str) -> Value {
    json!({
        "userId": user_id,
        "deviceId": device_id,
        "clientType": "desktop",
        "deviceName": device_name,
        "useDefaultScopes": true,
    })
}

/// base64（44 字符，含 padding）→ 32 字节 AES 密钥（对应 credentials 响应的
/// `encryptionKeys[channel]`）。
pub fn key_from_b64(s: &str) -> Result<Vec<u8>, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("密钥 base64 解码失败: {e}"))?;
    if raw.len() != 32 {
        return Err(format!("AES-256 密钥应为 32 字节，实际 {}", raw.len()));
    }
    Ok(raw)
}

/// AES-GCM-256 解信封：`ct_tag` = 密文 + 尾挂 16B tag，`nonce` 12B，AAD 可空。
///
/// 注意：AAD 的具体取值（是否含 keyVersion / channel）由探针实测校准，
/// 校准后这里保持纯函数签名不变。
pub fn gcm_decrypt(
    key: &[u8],
    nonce: &[u8],
    ct_tag: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    if key.len() != 32 {
        return Err(format!("AES-256 密钥应 32B，实际 {}", key.len()));
    }
    if nonce.len() != 12 {
        return Err(format!("GCM nonce 应 12B，实际 {}", nonce.len()));
    }
    if ct_tag.len() < 16 {
        return Err("密文过短（不足 16B tag）".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("cipher init: {e}"))?;
    // aes-gcm 0.10 约定：Payload.msg = 密文 + 尾挂 16B tag（与探针/手机端一致）
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload { msg: ct_tag, aad },
        )
        .map_err(|_| "GCM 解密失败（tag 校验不过或密钥/AAD 不匹配）".to_string())
}

/// Centrifugo JSON 协议 connect 帧（`id` 从 1 起自增）。
pub fn connect_frame(id: u64, token: &str) -> String {
    json!({
        "id": id,
        "connect": { "token": token, "name": "wb-switch", "version": env!("CARGO_PKG_VERSION") }
    })
    .to_string()
}

/// Centrifugo JSON 协议 subscribe 帧。
pub fn subscribe_frame(id: u64, channel: &str, sub_token: &str) -> String {
    json!({
        "id": id,
        "subscribe": { "channel": channel, "token": sub_token }
    })
    .to_string()
}

/// 解析服务端推送：`{"push":{"pub":{"data":<信封>}}}` → 抽出 `data`（未解密信封）。
/// 非 push/非 pub 帧（心跳 `{}`/`{"ping":N}`、ack、订阅确认、join/leave）返回 `None`。
pub fn parse_push(frame: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(frame).ok()?;
    let push = v.get("push")?;
    let pub_msg = push.get("pub")?;
    pub_msg.get("data").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::from_str;

    // ---- 跨语言固定向量：由 scripts/analysis/gen_gcm_vector.py 用 cryptography 生成，
    // ---- Python 探针与 Rust 实现共用，保证两端解密一致。

    /// key = base64("0123456789abcdef0123456789abcdef") 的 32 字节测试密钥
    const TEST_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
    // 由 scripts/analysis/gen_gcm_vector.py 生成（cryptography AESGCM），跨语言共用
    const VEC_NONCE_B64: &str = "AAECAwQFBgcICQoL";
    const VEC_CT_B64: &str = "WoeWD65xps/A5hbdHOKydol7cKpK2Rf/T657ZMbmo0DioWOVFoluVxeK";
    const VEC_AAD: &[u8] = b"";
    const VEC_PLAIN: &str = "wb-switch:gcm-cross-vector";

    #[test]
    fn credentials_body_has_desktop_shape() {
        let b = credentials_body("uid-123", "dev-abc", "测试机");
        assert_eq!(b["userId"], "uid-123");
        assert_eq!(b["deviceId"], "dev-abc");
        assert_eq!(b["clientType"], "desktop");
        assert_eq!(b["useDefaultScopes"], true);
        assert!(b["deviceName"].is_string());
    }

    #[test]
    fn key_from_b64_accepts_44char_and_rejects_short() {
        let k = key_from_b64(TEST_KEY_B64).expect("44 字符合法密钥");
        assert_eq!(k.len(), 32);
        assert!(key_from_b64("aGVsbG8").is_err(), "非 32 字节必须报错");
        assert!(key_from_b64("!!!不是base64!!!").is_err());
    }

    #[test]
    fn gcm_decrypt_cross_language_vector() {
        let key = key_from_b64(TEST_KEY_B64).unwrap();
        use base64::engine::general_purpose::STANDARD as B64;
        let nonce = B64.decode(VEC_NONCE_B64).unwrap();
        let ct = B64.decode(VEC_CT_B64).unwrap();
        let plain = gcm_decrypt(&key, &nonce, &ct, VEC_AAD).expect("跨语言向量必须可解");
        assert_eq!(String::from_utf8(plain).unwrap(), VEC_PLAIN);
    }

    #[test]
    fn gcm_decrypt_rejects_tampered_tag() {
        let key = key_from_b64(TEST_KEY_B64).unwrap();
        use base64::engine::general_purpose::STANDARD as B64;
        let nonce = B64.decode(VEC_NONCE_B64).unwrap();
        let mut ct = B64.decode(VEC_CT_B64).unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0x01; // 翻转 tag 一位
        assert!(gcm_decrypt(&key, &nonce, &ct, VEC_AAD).is_err(), "tag 校验必须拦住篡改");
    }

    #[test]
    fn connect_and_subscribe_frames_are_centrifugo_json() {
        let c = from_str::<Value>(&connect_frame(1, "tok-1")).expect("connect 帧是合法 JSON");
        assert_eq!(c["id"], 1);
        assert_eq!(c["connect"]["token"], "tok-1");

        let s = from_str::<Value>(&subscribe_frame(2, "user:u1:conversations", "subtok"))
            .expect("subscribe 帧是合法 JSON");
        assert_eq!(s["id"], 2);
        assert_eq!(s["subscribe"]["channel"], "user:u1:conversations");
        assert_eq!(s["subscribe"]["token"], "subtok");
    }

    #[test]
    fn parse_push_extracts_pub_data_and_ignores_control_frames() {
        let push = r#"{"push":{"pub":{"data":{"iv":"x","ct":"y"}}}}"#;
        let d = parse_push(push).expect("pub 帧必须抽出 data");
        assert_eq!(d["iv"], "x");

        assert!(parse_push(r#"{"id":3,"subscribe":{"recoverable":false}}"#).is_none());
        assert!(parse_push(r#"{"id":3,"reply":{}}"#).is_none());
        assert!(parse_push("不是json").is_none());
    }
}
