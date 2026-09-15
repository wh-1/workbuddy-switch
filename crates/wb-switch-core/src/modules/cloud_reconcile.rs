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

use super::cloud_conv::{self, CloudDelete};

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

#[cfg(test)]
mod tests {
    use super::*;
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
}
