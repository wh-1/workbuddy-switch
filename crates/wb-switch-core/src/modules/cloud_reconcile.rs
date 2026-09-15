//! 云端对账（全账合账的对账层）：云端索引 × 本机 sessions → 分类 + 残留清理。
//!
//! 语义（2026-09-15 定，测试钉死）：
//! - 云端有 + 本机存活 → `Aligned`（保留，不动）
//! - 云端有 + 本机已软删 → `CloudOnly`（确凿残留 → victims，交 `cloud_conv` 删）
//! - 云端有 + 本机无任何痕迹 → `Unknown`（**不删**：可能是其他设备的活会话，仅报告）
//! - 本机有 + 云端无 → `Ignored`（本机事务，与云端无关）
//!
//! 红线：只读映射库，绝不删 `edge-sync-mapping-*.db` 的行（坑位 41）。

use std::collections::HashSet;

use serde_json::{json, Value};

use super::cloud_conv::CloudDelete;

/// 对账结果。`victims` 只含 `CloudOnly`；`unknown` 仅报告不动作。
#[derive(Debug, Default, PartialEq)]
pub struct ReconcileReport {
    pub aligned: Vec<String>,
    pub cloud_only: Vec<String>,
    pub unknown: Vec<String>,
    pub ignored_local_only: usize,
}

/// 纯对账：不联网、不落盘。
pub fn reconcile(
    cloud_sids: &[String],
    local_alive: &HashSet<String>,
    local_deleted: &HashSet<String>,
) -> ReconcileReport {
    let mut r = ReconcileReport::default();
    for sid in cloud_sids {
        if local_alive.contains(sid) {
            r.aligned.push(sid.clone());
        } else if local_deleted.contains(sid) {
            r.cloud_only.push(sid.clone());
        } else {
            r.unknown.push(sid.clone());
        }
    }
    r.ignored_local_only = local_alive
        .iter()
        .chain(local_deleted.iter())
        .filter(|s| !cloud_sids.contains(s))
        .count();
    r
}

/// 清扫 victims：逐条调 `delete`（真实现 = `cloud_conv::delete_conversation`，
/// 归属账号 token）。dry_run 只统计不调 `delete`。
///
/// 返回报告沿用瘦身 `cloud.*` 字段口径：`removed`(200) / `alreadyGone`(404) 分列，
/// `failed` 汇总非 200/404 结果。
pub fn sweep<F>(token: Option<&str>, victims: &[String], dry_run: bool, mut delete: F) -> Value
where
    F: FnMut(&str, &str) -> CloudDelete,
{
    let mut removed = 0usize;
    let mut already_gone = 0usize;
    let mut failed = 0usize;
    if let (false, Some(tok)) = (dry_run, token) {
        for sid in victims {
            match delete(tok, sid) {
                CloudDelete::Deleted => removed += 1,
                CloudDelete::AlreadyGone => already_gone += 1,
                _ => failed += 1,
            }
        }
    }
    json!({
        "planned": victims.len(),
        "dryRun": dry_run,
        "tokenReady": token.is_some(),
        "noToken": if token.is_none() { victims.len() } else { 0 },
        "removed": removed,
        "alreadyGone": already_gone,
        "failed": failed,
    })
}

/// 对账编排：映射行全集 × 本机存活/软删集合 → 分类 + 清理 victims。
///
/// `rows` 注入（离线可测）；真实现传 `cloud_conv::mapping_rows()`。
/// Unknown（映射有、本机无行）**永不删**——可能是其他设备的活会话，只计数。
/// 报告并入瘦身 `cloud.reconcile` 子对象。
pub fn sweep_stale_mappings<F>(
    rows: &[super::cloud_conv::MappingEntry],
    local_alive: &HashSet<String>,
    local_deleted: &HashSet<String>,
    token_of_uid: Option<&str>,
    dry_run: bool,
    mut delete: F,
) -> Value
where
    F: FnMut(&str, &str) -> CloudDelete,
{
    let sids: Vec<String> = rows.iter().map(|r| r.session_id.clone()).collect();
    let r = reconcile(&sids, local_alive, local_deleted);
    // victims 需要映射回 conversation_id（删除接口的唯一钥匙）
    let cid_of: std::collections::HashMap<&str, &str> = rows
        .iter()
        .map(|e| (e.session_id.as_str(), e.conversation_id.as_str()))
        .collect();
    let victim_cids: Vec<String> = r
        .cloud_only
        .iter()
        .filter_map(|sid| cid_of.get(sid.as_str()).map(|c| c.to_string()))
        .collect();
    let mut sweep = sweep(token_of_uid, &victim_cids, dry_run, &mut delete);
    sweep["aligned"] = json!(r.aligned.len());
    sweep["unknown"] = json!(r.unknown.len());
    sweep["mapped"] = json!(rows.len());
    sweep
}

/// 全账巡检（**只读，永不删**）：云端全账 × 本机状态 → 分类计数。
///
/// 与 `sweep_stale_mappings` 的口径差别：那个只看「本机映射行」这本账，
/// 看不到云端还有哪些**本机映射库根本不知道**的会话；这里直接吃
/// `GET /v2/as/conversations/?type=all` 的账号全域清单（跨设备，2026-09-15 打通）。
///
/// - `stale`   = 云端有 + 本机已软删 → 清理仍归 `sweep_stale_mappings`（那才有映射钥匙）
/// - `foreign` = 云端有 + 本机无任何痕迹 → **他机/跨设备会话，永不删**
/// - `localOnly` = 本机有 + 云端无 → 未上云
///
/// 取数失败（无 token / 网络错）只回 `enabled:false` + 原因，**不影响主流程**。
pub fn inventory<F>(
    token: Option<&str>,
    local_alive: &HashSet<String>,
    local_deleted: &HashSet<String>,
    mut fetch: F,
) -> Value
where
    F: FnMut(&str) -> Result<Vec<String>, String>,
{
    let tok = match token {
        Some(t) => t,
        None => return json!({"enabled": false, "reason": "noToken"}),
    };
    let cloud_sids = match fetch(tok) {
        Ok(v) => v,
        Err(e) => return json!({"enabled": false, "reason": e}),
    };
    let r = reconcile(&cloud_sids, local_alive, local_deleted);
    json!({
        "enabled": true,
        "cloud": cloud_sids.len(),
        "aligned": r.aligned.len(),
        "stale": r.cloud_only.len(),
        "foreign": r.unknown.len(),
        "localOnly": r.ignored_local_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::cloud_conv;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reconcile_classifies_four_ways() {
        // 云端 5 条：c1 本机存活 / c2 本机已软删 / c3 无痕迹 / c4 同 c2 / c5 同 c1
        let cloud = ["c1", "c2", "c3", "c4", "c5"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let alive = set(&["c1", "c5", "l1"]);
        let deleted = set(&["c2", "c4", "l2"]);

        let r = reconcile(&cloud, &alive, &deleted);

        assert_eq!(r.aligned, vec!["c1".to_string(), "c5".to_string()]);
        assert_eq!(r.cloud_only, vec!["c2".to_string(), "c4".to_string()]);
        assert_eq!(r.unknown, vec!["c3".to_string()]);
        assert_eq!(r.ignored_local_only, 2, "l1/l2 云端没有 → 本机事务");
        // 红线语义：unknown 绝不混进 victims
        assert!(!r.cloud_only.contains(&"c3".to_string()));
    }

    #[test]
    fn reconcile_empty_cloud_is_noop() {
        let r = reconcile(&[], &set(&["c1"]), &set(&["c2"]));
        assert!(r.aligned.is_empty() && r.cloud_only.is_empty() && r.unknown.is_empty());
        assert_eq!(r.ignored_local_only, 2);
    }

    #[test]
    fn sweep_dry_run_counts_without_calling_delete() {
        let victims = vec!["a".to_string(), "b".to_string()];
        let r = sweep(Some("tok"), &victims, true, |_t, _sid| {
            panic!("dry_run 不得发删除请求");
        });
        assert_eq!(r["planned"], 2);
        assert_eq!(r["removed"], 0);
        assert_eq!(r["alreadyGone"], 0);
    }

    #[test]
    fn sweep_no_token_reports_and_skips_all() {
        let victims = vec!["a".to_string()];
        let r = sweep(None, &victims, false, |_t, _sid| {
            panic!("无 token 不得发删除请求");
        });
        assert_eq!(r["tokenReady"], false);
        assert_eq!(r["noToken"], 1);
        assert_eq!(r["removed"], 0);
    }

    #[test]
    fn sweep_separates_removed_alreadygone_and_failed() {
        let victims = vec!["r1".to_string(), "g1".to_string(), "f1".to_string()];
        let r = sweep(Some("tok"), &victims, false, |_t, sid| match sid {
            "r1" => cloud_conv::classify(200, "{}"),
            "g1" => cloud_conv::classify(404, r#"{"code":14284}"#),
            _ => cloud_conv::classify(403, r#"{"code":14287}"#),
        });
        assert_eq!(r["removed"], 1);
        assert_eq!(r["alreadyGone"], 1);
        assert_eq!(r["failed"], 1);
        assert_eq!(r["planned"], 3);
    }

    fn entry(sid: &str, cid: &str, ch: &str) -> cloud_conv::MappingEntry {
        cloud_conv::MappingEntry {
            session_id: sid.to_string(),
            conversation_id: cid.to_string(),
            channel: ch.to_string(),
        }
    }

    #[test]
    fn sweep_stale_mappings_routes_only_local_deleted() {
        let rows = vec![
            entry("s1", "c1", "convmsg:u1"), // 本机存活 → aligned
            entry("s2", "c2", "convmsg:u1"), // 本机已软删 → victim
            entry("s3", "c3", "convmsg:u1"), // 无行 → unknown 不删
        ];
        let alive = set(&["s1"]);
        let deleted = set(&["s2"]);
        let mut called: Vec<String> = Vec::new();
        let r = sweep_stale_mappings(
            &rows,
            &alive,
            &deleted,
            Some("tok"),
            false,
            |_t, cid| {
                called.push(cid.to_string());
                cloud_conv::classify(200, "{}")
            },
        );
        assert_eq!(called, vec!["c2".to_string()], "只删本机已软删那把 cid 钥匙");
        assert_eq!(r["removed"], 1);
        assert_eq!(r["aligned"], 1);
        assert_eq!(r["unknown"], 1);
        assert_eq!(r["mapped"], 3);
    }

    #[test]
    fn sweep_stale_mappings_dry_run_never_calls_delete() {
        let rows = vec![entry("s2", "c2", "convmsg:u1")];
        let r = sweep_stale_mappings(
            &rows,
            &set(&[]),
            &set(&["s2"]),
            Some("tok"),
            true,
            |_t, _cid| panic!("dry_run 不得发删除请求"),
        );
        assert_eq!(r["planned"], 1);
        assert_eq!(r["removed"], 0);
    }

    #[test]
    fn inventory_classifies_without_any_delete() {
        // 云端 4 条：c1/c4 本机存活 · c2 本机已软删 · c3 本机无痕迹（他机）
        let alive = set(&["c1", "c4", "l1"]);
        let deleted = set(&["c2"]);
        let r = inventory(Some("tok"), &alive, &deleted, |_t| {
            Ok(["c1", "c2", "c3", "c4"]
                .iter()
                .map(|s| s.to_string())
                .collect())
        });
        assert_eq!(r["enabled"], true);
        assert_eq!(r["cloud"], 4);
        assert_eq!(r["aligned"], 2);
        assert_eq!(r["stale"], 1);
        assert_eq!(r["foreign"], 1, "c3 是本机无痕迹的他机会话");
        assert_eq!(r["localOnly"], 1, "l1 未上云");
    }

    #[test]
    fn inventory_without_token_is_disabled_not_fatal() {
        let r = inventory(None, &set(&["c1"]), &set(&[]), |_t| {
            panic!("无 token 不得发请求")
        });
        assert_eq!(r["enabled"], false);
        assert_eq!(r["reason"], "noToken");
    }

    #[test]
    fn inventory_fetch_error_degrades_silently() {
        let r = inventory(Some("tok"), &set(&[]), &set(&[]), |_t| {
            Err("http 500".to_string())
        });
        assert_eq!(r["enabled"], false);
        assert_eq!(r["reason"], "http 500");
        assert!(
            r.get("stale").is_none(),
            "降级时不得报出任何可能被误读成「可删」的计数"
        );
    }
}
