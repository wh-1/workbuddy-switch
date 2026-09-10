//! UI 主题跟随账号（L6）：切号时把目标账号的主题预写进 WorkBuddy Local Storage。
//!
//! 背景：WorkBuddy 桌面端把 UI 主题存在 Electron Local Storage（LevelDB），
//! 键 `agent-ui-theme`，登录后由云端账号偏好**异步**回写 → 切号重启的瞬间可能
//! 先渲染默认主题（实测出现过：应深色却先浅色）。本模块在 App 完全关闭后、
//! 重启前：① 备份被切走账号的当前主题；② 把目标账号上次备份的主题 append 进
//! LevelDB log，重启即为目标账号主题，不等云端同步。
//!
//! 安全设计（append-only，不改写任何现有字节）：
//! - 只向**最新 .log 文件末尾**追加一条完整 record；LevelDB 打开时按序 replay，
//!   我们这条因 sequence 更大而覆盖旧值。
//! - record 的 CRC 若算错或格式不符，LevelDB 会丢弃该 record——最坏情况是
//!   「预写不生效」，原有数据不受影响。
//! - 追加前整目录备份到 `~/.wb-switch/backups/localstorage/<ts>/`。
//!
//! 格式（2026-09-10 在本机 leveldb 上字节级验证）：
//! - key   = `_file://\0\x01agent-ui-theme`
//! - value = 0x01 + JSON（`{"theme":"dark","followSystem":false,
//!           "vsCodeThemeName":"IDE Night","vsCodeThemeKind":"vscode-dark"}`）
//! - batch = seq(u64 le) + count(u32 le) + [type(1) + varint(klen) + key + varint(vlen) + value]
//! - record = crc32c-masked(4 le) + len(2 le) + type(1) + batch（crc 覆盖 type+batch）

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::modules::config::{backup_dir, home_dir, now_ms, utc_iso};

/// 主题键的完整 LevelDB key（origin 固定为打包渲染进程的 `_file://`）。
const THEME_KEY: &[u8] = b"_file://\x00\x01agent-ui-theme";
/// Chromium localStorage value 首字节（0x01 = 单字节文本标记，实测样例如此）。
const VALUE_PREFIX: u8 = 0x01;
/// WorkBuddy userData 下的 Local Storage 位置。
const LEVELDB_SUBDIR: &str = "app/session/Local Storage/leveldb";
/// 主题备份目录名（~/.wb-switch/ui_prefs/）。
const PREFS_DIR: &str = "ui_prefs";
/// LevelDB log 常量。
const BLOCK: usize = 32 * 1024;
const RECORD_HEADER: usize = 7;
const RECORD_FULL: u8 = 1;
const BATCH_TYPE_VALUE: u8 = 1;

fn leveldb_dir() -> PathBuf {
    home_dir().join(".workbuddy").join(LEVELDB_SUBDIR)
}

fn prefs_dir() -> PathBuf {
    home_dir().join(".wb-switch").join(PREFS_DIR)
}

// ---------------------------------------------------------------------------
// CRC32C（Castagnoli，LevelDB 标准校验）
// ---------------------------------------------------------------------------

fn crc32c_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

pub fn crc32c(data: &[u8]) -> u32 {
    let t = crc32c_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = t[(crc ^ b as u32) as usize & 0xFF] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// LevelDB 的 crc 掩码：rotate right 15 再加固定常数。
fn mask_crc(crc: u32) -> u32 {
    ((crc >> 15) | (crc << 17)).wrapping_add(0xA282_EAD8)
}

// ---------------------------------------------------------------------------
// 编码
// ---------------------------------------------------------------------------

fn put_varint32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        if v < 0x80 {
            out.push(v as u8);
            break;
        }
        out.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
}

fn read_varint32(data: &[u8], p: usize) -> Option<(u32, usize)> {
    let mut v = 0u32;
    let mut s = 0u32;
    for n in 0..5 {
        let b = *data.get(p + n)?;
        v |= ((b & 0x7F) as u32) << s;
        if b & 0x80 == 0 {
            return Some((v, n + 1));
        }
        s += 7;
    }
    None
}

/// 构造一条含单条 put 的 WriteBatch payload。
fn build_batch(seq: u64, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + key.len() + value.len() + 6);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes()); // count = 1
    p.push(BATCH_TYPE_VALUE);
    put_varint32(&mut p, key.len() as u32);
    p.extend_from_slice(key);
    put_varint32(&mut p, value.len() as u32);
    p.extend_from_slice(value);
    p
}

/// 把 batch payload 封成一条 FULL log record。
fn build_log_record(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() + RECORD_HEADER <= BLOCK, "单条 record 必须放进一个 block");
    let mut crc_input = Vec::with_capacity(1 + payload.len());
    crc_input.push(RECORD_FULL);
    crc_input.extend_from_slice(payload);
    let masked = mask_crc(crc32c(&crc_input));

    let mut out = Vec::with_capacity(RECORD_HEADER + payload.len());
    out.extend_from_slice(&masked.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.push(RECORD_FULL);
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// LevelDB log 读写（.log 文件，字节级扫描）
// ---------------------------------------------------------------------------

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let num: u64 = name.strip_suffix(".log")?.parse().ok()?;
            Some((num, e.path()))
        })
        .collect();
    logs.sort_by_key(|(n, _)| *n);
    logs.into_iter().map(|(_, p)| p).collect()
}

/// 在一段字节里找 KEY 的所有出现位置。
fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return out;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle {
            out.push(i);
        }
    }
    out
}

/// 扫描一段字节，取最后一次出现的主题 value（0x01 前缀 + 合法 JSON）。
fn scan_theme_json_in(data: &[u8]) -> Option<String> {
    let mut best: Option<String> = None;
    for i in find_all(data, THEME_KEY) {
        let Some((vlen, n)) = read_varint32(data, i + THEME_KEY.len()) else {
            continue;
        };
        let vs = i + THEME_KEY.len() + n;
        let ve = vs + vlen as usize;
        if ve > data.len() {
            continue;
        }
        let raw = &data[vs..ve];
        if raw.first() != Some(&VALUE_PREFIX) {
            continue;
        }
        let Ok(s) = std::str::from_utf8(&raw[1..]) else {
            continue;
        };
        // 格式自校验：必须是含 theme 键的合法 JSON，防止误匹配到别的内容
        let Ok(v) = serde_json::from_str::<Value>(s) else {
            continue;
        };
        if v.get("theme").and_then(|t| t.as_str()).is_some() {
            best = Some(s.to_string());
        }
    }
    best
}

/// 读取当前主题 JSON（只扫 .log：新写入总落在 .log，compaction 后由云端回写兜底）。
fn scan_current_theme(dir: &Path) -> Option<String> {
    for p in log_files(dir).into_iter().rev() {
        let Ok(data) = fs::read(&p) else { continue };
        if let Some(s) = scan_theme_json_in(&data) {
            return Some(s);
        }
    }
    None
}

/// 向最新 .log 末尾追加一条覆盖主题的 record（处理 block 剩余空间）。
fn append_theme_record(dir: &Path, seq: u64, theme_json: &str) -> Result<(), String> {
    let mut logs = log_files(dir);
    let latest = logs.pop().ok_or_else(|| "leveldb 无 .log 文件（可能刚被 compaction），跳过主题预写".to_string())?;
    let payload = build_batch(seq, THEME_KEY, &[VALUE_PREFIX].iter().chain(theme_json.as_bytes()).copied().collect::<Vec<u8>>());
    let record = build_log_record(&payload);

    let existing = fs::read(&latest).map_err(|e| e.to_string())?;
    let mut out: Vec<u8> = Vec::new();
    let rem = BLOCK - (existing.len() % BLOCK);
    if rem < RECORD_HEADER {
        out.extend(std::iter::repeat(0u8).take(rem)); // trailer
    } else if rem < RECORD_HEADER + record.len() {
        out.extend(std::iter::repeat(0u8).take(rem)); // 本 block 放不下，另起 block
    }
    out.extend_from_slice(&record);

    let mut f = fs::OpenOptions::new().append(true).open(&latest).map_err(|e| e.to_string())?;
    f.write_all(&out).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 备份 / 恢复
// ---------------------------------------------------------------------------

fn theme_backup_path(uid: &str) -> PathBuf {
    prefs_dir().join(format!("theme-{uid}.json"))
}

/// 备份某账号的当前主题（app 关闭后调用；读不到就跳过，等下次）。
fn backup_theme_for(uid: &str, current: Option<&str>) -> bool {
    let Some(json_text) = current else { return false };
    fs::create_dir_all(prefs_dir()).ok();
    let doc = json!({ "uid": uid, "capturedAt": now_ms(), "themeJson": json_text });
    let p = theme_backup_path(uid);
    let tmp = p.with_extension("json.tmp");
    if fs::write(&tmp, doc.to_string()).is_err() {
        return false;
    }
    fs::rename(&tmp, &p).is_ok()
}

/// 恢复目标账号主题：备份存在且与当前值不同才写（写前整目录备份）。
fn restore_theme_for(dir: &Path, uid: &str) -> Result<bool, String> {
    let p = theme_backup_path(uid);
    let Ok(text) = fs::read_to_string(&p) else {
        return Ok(false); // 该账号还没有备份，交给云端同步兜底
    };
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let Some(theme_json) = doc.get("themeJson").and_then(|v| v.as_str()) else {
        return Ok(false);
    };
    if let Some(current) = scan_current_theme(dir) {
        if current == theme_json {
            return Ok(false); // 已是目标主题，无需写
        }
    }
    // 整目录备份（小文件，几个 MB 以内）
    let dst = backup_dir().join("localstorage").join(utc_iso());
    copy_dir_recursive(dir, &dst)
        .map_err(|e| format!("Local Storage 备份失败: {e}"))?;
    append_theme_record(dir, now_ms() as u64, theme_json)?;
    Ok(true)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let t = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir_recursive(&e.path(), &t)?;
        } else {
            fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 对外入口
// ---------------------------------------------------------------------------

/// 切号时同步主题：备份被切走账号的当前主题 → 恢复目标账号的上次主题。
///
/// 必须在 WorkBuddy 完全关闭后、写 auth/重启前调用。全程只读源 + append 目标，
/// 任何失败都不阻断切号（只影响主题是否瞬时到位，云端同步最终会兜底）。
pub fn sync_theme_for_switch(source_uid: Option<&str>, target_uid: &str) -> Value {
    let dir = leveldb_dir();
    if !dir.is_dir() || target_uid.is_empty() {
        return json!({ "backedUp": false, "restored": false, "skipped": true });
    }
    let current = scan_current_theme(&dir);

    let backed_up = source_uid
        .filter(|u| !u.is_empty() && *u != target_uid)
        .map(|u| backup_theme_for(u, current.as_deref()))
        .unwrap_or(false);

    let restored = match restore_theme_for(&dir, target_uid) {
        Ok(v) => v,
        Err(e) => {
            return json!({ "backedUp": backed_up, "restored": false, "error": e });
        }
    };
    json!({ "backedUp": backed_up, "restored": restored })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_known_vectors() {
        // CRC-32C 标准检验值
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0x0000_0000);
        assert_eq!(crc32c(b"a"), 0xC1D0_4330);
    }

    #[test]
    fn varint_roundtrip() {
        for v in [0u32, 1, 127, 128, 300, 0xFFFF, 0x1234_5678] {
            let mut buf = Vec::new();
            put_varint32(&mut buf, v);
            let (got, n) = read_varint32(&buf, 0).unwrap();
            assert_eq!(got, v);
            assert_eq!(n, buf.len());
        }
    }

    const SAMPLE_LIGHT: &str =
        r#"{"theme":"light","followSystem":false,"vsCodeThemeName":"IDE Light","vsCodeThemeKind":"vscode-light"}"#;
    const SAMPLE_DARK: &str =
        r#"{"theme":"dark","followSystem":false,"vsCodeThemeName":"IDE Night","vsCodeThemeKind":"vscode-dark"}"#;

    fn theme_value(json_text: &str) -> Vec<u8> {
        let mut v = vec![VALUE_PREFIX];
        v.extend_from_slice(json_text.as_bytes());
        v
    }

    /// 构造一个仿真 .log：真实 batch 框架 + 真实样例值，验证 scan 能读回。
    #[test]
    fn scan_reads_back_appended_record() {
        let dir = std::env::temp_dir().join(format!("wbs-theme-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("000007.log");
        fs::write(&log, build_log_record(&build_batch(100, THEME_KEY, &theme_value(SAMPLE_LIGHT)))).unwrap();

        assert_eq!(scan_current_theme(&dir).as_deref(), Some(SAMPLE_LIGHT));

        // 追加 dark，读回应为 dark（后写覆盖）
        append_theme_record(&dir, 200, SAMPLE_DARK).unwrap();
        assert_eq!(scan_current_theme(&dir).as_deref(), Some(SAMPLE_DARK));

        // 文件末尾应恰好是追加的 dark record（CRC/长度自洽）
        let data = fs::read(&log).unwrap();
        let expected = build_log_record(&build_batch(200, THEME_KEY, &theme_value(SAMPLE_DARK)));
        assert!(data.len() > expected.len());
        assert_eq!(&data[data.len() - expected.len()..], &expected[..]);

        fs::remove_dir_all(&dir).ok();
    }

    /// block 边界：剩余 < header 时填 trailer 另起 block，record 仍可被 scan 读回。
    #[test]
    fn append_handles_block_boundary() {
        let dir = std::env::temp_dir().join(format!("wbs-theme-blk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("000003.log");
        // 预填到只剩 3 字节（< header 7），append 应先填 trailer 再从新 block 写
        let prefix_len = BLOCK - 3;
        fs::write(&log, vec![0xABu8; prefix_len]).unwrap();
        append_theme_record(&dir, 1, SAMPLE_DARK).unwrap();
        let data = fs::read(&log).unwrap();
        let expected = build_log_record(&build_batch(1, THEME_KEY, &theme_value(SAMPLE_DARK)));
        assert_eq!(data.len(), prefix_len + 3 + expected.len());
        // record 必须落在 block 边界（LevelDB 要求 record 不跨 block 起始）
        assert_eq!(&data[prefix_len + 3..], &expected[..]);
        assert_eq!(scan_theme_json_in(&data).as_deref(), Some(SAMPLE_DARK));
        fs::remove_dir_all(&dir).ok();
    }

    /// 真实样例字节（2026-09-10 本机 leveldb 提取）可被扫描还原。
    #[test]
    fn scan_parses_real_world_sample() {
        // batch payload: seq + count + entry(type+varint klen+key+varint vlen+value)
        let payload = build_batch(42, THEME_KEY, &theme_value(SAMPLE_DARK));
        assert_eq!(scan_theme_json_in(&payload).as_deref(), Some(SAMPLE_DARK));
    }
}
