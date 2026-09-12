#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""官方账单 vs 本地记账 对照分析。

目的：量化本地 `session_usage.credit_json` 与官方接口
(`billing/meter/get-user-request-usage`) 的差异，标定本地的可信边界。

数据源（全部本地缓存，不打接口）：
  1. ~/.wb-switch/official_usage_cache.json   官方逐笔明细的投影（权威扣分）
     - accounts[].daily[].models[] = 按账号×日×模型的官方 credit 与请求数
     - accounts[].accountId        = workbuddy-switch 的账号 id
  2. ~/.workbuddy/workbuddy.db  session_usage  本地记账（credit_json，键=conversationRequestId）
     - 本地行没有账号字段；用 JSONL conversationRequestId → 时刻 → auth 时间轴归因
  3. ~/.wb-switch/credit_usage_snapshots.json  官方余额池快照（remaining/total）

已知口径（2026-09-12 实测）：
  - 官方 credit = 每笔请求真实扣分（与余额池下降吻合，是权威）
  - 本地 credit_json ≠ 官方镜像：只在部分条件（疑为超额段）写入，
    且经切号对齐后会话归属被改写 → 只能当「超额事件线索」
"""
from __future__ import annotations

import collections
import datetime as dt
import glob
import json
import os
import sqlite3
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import account_timeline as at

HOME = os.path.expanduser("~")
OFFICIAL_CACHE = os.path.join(HOME, ".wb-switch", "official_usage_cache.json")
SNAP_FILE = os.path.join(HOME, ".wb-switch", "credit_usage_snapshots.json")
DB_PATH = os.path.join(HOME, ".workbuddy", "workbuddy.db")
PROJ_DIR = os.path.join(HOME, ".workbuddy", "projects")


def load_official_by_account_day_model() -> dict:
    """官方缓存 → {(accountName, date, model): (credit, requestCount)}。"""
    with open(OFFICIAL_CACHE, encoding="utf-8") as fh:
        payload = json.load(fh).get("payload", {})
    out: dict[tuple, tuple] = {}
    for acc in payload.get("accounts", []):
        name = acc.get("accountName") or str(acc.get("accountId", "?"))[:8]
        for day in acc.get("daily") or []:
            date = day.get("date")
            for m in day.get("models") or []:
                key = (name, date, m.get("model") or "?")
                prev = out.get(key)
                credit = float(m.get("credit") or 0)
                count = int(m.get("requestCount") or 0)
                if prev is None:
                    out[key] = (credit, count)
                else:
                    out[key] = (prev[0] + credit, prev[1] + count)
    return out


def load_local_by_account_day_model() -> dict:
    """本地记账 → {(accountName, date, model): (credit, count)}。

    credit_json 键（conversationRequestId）→ JSONL 找首现时刻与模型 →
    auth 时间轴归因到账号。无时间戳/无归属的丢弃并计数。
    """
    con = sqlite3.connect(
        "file:" + DB_PATH.replace("\\", "/") + "?mode=ro", uri=True, timeout=5
    )
    credit: dict[str, float] = {}
    for (raw,) in con.execute("SELECT credit_json FROM session_usage"):
        if not raw:
            continue
        try:
            cj = json.loads(raw)
        except (TypeError, ValueError):
            continue
        for k, v in cj.items():
            try:
                c = float(v)
            except (TypeError, ValueError):
                continue
            if k not in credit or c > credit[k]:
                credit[k] = c
    con.close()

    events, names = at.load_account_timeline()
    jk_first: dict[str, tuple[int, str]] = {}
    for path in glob.glob(os.path.join(PROJ_DIR, "**", "*.jsonl"), recursive=True):
        if "subagents" in path.replace("\\", "/"):
            continue
        try:
            fh = open(path, encoding="utf-8", errors="ignore")
        except OSError:
            continue
        with fh:
            for line in fh:
                try:
                    v = json.loads(line)
                except ValueError:
                    continue
                prov = v.get("providerData") or {}
                jk = prov.get("conversationRequestId")
                ts = v.get("timestamp")
                if jk and ts and jk not in jk_first:
                    jk_first[jk] = (int(ts), prov.get("model") or "?")

    out: dict[tuple, list] = {}
    missed = 0
    for tid, c in credit.items():
        hit = jk_first.get(tid)
        if not hit:
            missed += 1
            continue
        ts, model = hit
        uid = at.account_at(events, ts)
        if uid is None:
            missed += 1
            continue
        name = at.label(uid, names)
        date = dt.datetime.fromtimestamp(ts / 1000).strftime("%Y-%m-%d")
        slot = out.setdefault((name, date, model), [0.0, 0])
        slot[0] += c
        slot[1] += 1
    return {k: (v[0], v[1]) for k, v in out.items()}, missed


def load_latest_snapshots() -> list[dict]:
    try:
        with open(SNAP_FILE, encoding="utf-8") as fh:
            rows = json.load(fh)
    except (OSError, ValueError):
        return []
    latest: dict[str, dict] = {}
    for row in rows:
        latest[row.get("accountId")] = row
    out = []
    for row in latest.values():
        out.append(
            {
                "name": row.get("accountName", "?"),
                "remaining": float(row.get("remaining") or 0),
                "total": float(row.get("total") or 0),
                "ts": int(row.get("ts") or 0),
            }
        )
    out.sort(key=lambda r: -r["total"])
    return out


def main() -> int:
    import argparse

    ap = argparse.ArgumentParser(description="官方账单 vs 本地记账 对照")
    ap.add_argument("--days", type=int, default=4, help="只看最近 N 天（默认 4）")
    args = ap.parse_args()

    official = load_official_by_account_day_model()
    local, missed_local = load_local_by_account_day_model()
    snapshots = load_latest_snapshots()

    today = dt.date.today()
    cutoff = today - dt.timedelta(days=max(0, args.days - 1))
    cutoff_s = cutoff.strftime("%Y-%m-%d")

    print("=" * 100)
    print("  官方账单 vs 本地记账（session_usage）对照 — 找出本地计算的盲区")
    print("=" * 100)

    print("\n【0】账号余额池（官方 credit_usage_snapshots，权威）")
    print(f"  {'账号':<10}{'剩余':>10}{'总额':>10}{'已用':>10}{'已用%':>8}{'快照时刻':>18}")
    for s in snapshots:
        used = s["total"] - s["remaining"]
        pct = used / s["total"] * 100 if s["total"] else 0
        ts = dt.datetime.fromtimestamp(s["ts"] / 1000).strftime("%m-%d %H:%M")
        warn = "  ⚠️ 余额告急" if pct >= 90 else ""
        print(f"  {s['name']:<10}{s['remaining']:>10.1f}{s['total']:>10.0f}{used:>10.1f}"
              f"{pct:>7.1f}%{ts:>18}{warn}")

    # 逐 (账号, 日, 模型) 对照
    keys = sorted(
        {k for k in official if k[1] >= cutoff_s} | {k for k in local if k[1] >= cutoff_s},
        key=lambda k: (k[1], k[0], k[2]),
    )
    print(f"\n【1】逐格对照（{cutoff_s} 起，credit / 请求数；Δ=本地-官方）")
    print(f"  {'日期':<11}{'账号':<9}{'模型':<22}{'官方':>10}{'笔':>5}{'本地':>10}{'笔':>5}"
          f"{'Δ积分':>9}  判定")
    print("-" * 100)
    stats = collections.Counter()
    for name, date, model in keys:
        off_c, off_n = official.get((name, date, model), (0.0, 0))
        loc_c, loc_n = local.get((name, date, model), (0.0, 0))
        if off_c == 0 and loc_c == 0:
            continue
        delta = loc_c - off_c
        if off_c == 0 and loc_c > 0:
            verdict = "本地独有（归因错位/官方缺页）"
            stats["local_only"] += 1
        elif loc_c == 0 and off_c > 0:
            verdict = "⚠️ 本地漏记"
            stats["missed"] += 1
        elif abs(delta) <= max(1.0, off_c * 0.1):
            verdict = "✓ 吻合(±10%)"
            stats["match"] += 1
        elif delta > 0:
            verdict = "本地多记（跨账号混入？）"
            stats["over"] += 1
        else:
            verdict = "本地少记"
            stats["under"] += 1
        print(f"  {date:<11}{name:<9}{model[:21]:<22}{off_c:>10.1f}{off_n:>5}"
              f"{loc_c:>10.1f}{loc_n:>5}{delta:>9.1f}  {verdict}")
    print("-" * 100)
    total = sum(stats.values())
    if total:
        print(f"  判定汇总: ✓吻合 {stats['match']} · 本地漏记 {stats['missed']} · "
              f"本地多记 {stats['over']} · 本地少记 {stats['under']} · "
              f"本地独有 {stats['local_only']}（共 {total} 格）")

    print("\n【2】结论")
    print("  · 官方 credit = 每笔请求真实扣分（与余额池下降吻合），是唯一权威")
    print("  · 本地 session_usage 只覆盖部分扣分（疑为超额段），且切号对齐会改写")
    print("    会话归属 → 跨账号混记；Harvey 09 月扣分在本地全漏即为例证")
    print("  · 本地 JSONL token 用量是「真实用量」维度，与扣分之间隔限额/倍率/缓存")
    print("  · 每模型每日限额：官方接口无现成字段，只能用「官方明细在某日某模型")
    print("    的 credit 突停 + 换模型」行为标定；限额内消耗只在官方明细可见")
    print(f"  · 本地 credit 键无法归因条数: {missed_local}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
