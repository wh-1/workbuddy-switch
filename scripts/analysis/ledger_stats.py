#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""本地账本统计 — 按账号的积分消耗（与官方接口口径一致）。

数据源：~/.wb-switch/credit_ledger/<accountId>.jsonl
        （Rust 侧 credit_ledger.rs 在统计页刷新时从官方接口全量落盘）

与 official_vs_local.py 的区别：那个对照「官方缓存 vs 本地 session_usage」；
本脚本直接读按账号落盘的官方明细账本——不受切号对齐影响，天然按账号。

用法：
  python ledger_stats.py                 # 全部账号，按账号×日汇总
  python ledger_stats.py --days 7        # 最近 7 天
  python ledger_stats.py --by-model      # 追加按账号×日×模型明细
  python ledger_stats.py --account H     # 只看某个账号
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import account_timeline as at

HOME = os.path.expanduser("~")
LEDGER_DIR = os.path.join(HOME, ".wb-switch", "credit_ledger")
ACCOUNTS_FILE = os.path.join(HOME, ".wb-switch", "accounts.json")


def load_account_names() -> dict[str, str]:
    """accounts.json → {accountId: 显示名}。"""
    try:
        with open(ACCOUNTS_FILE, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return {}
    accounts = data.get("accounts") if isinstance(data, dict) else data
    names: dict[str, str] = {}
    for acc in accounts or []:
        if not isinstance(acc, dict):
            continue
        acc_id = acc.get("id") or acc.get("accountId") or ""
        label = acc.get("name") or acc.get("nickname") or acc.get("email") or acc_id[:8]
        if acc_id:
            names[str(acc_id)] = str(label)
    return names


def iter_ledger_rows() -> "collections.abc.Iterator[tuple[str, dict]]":
    """产出 (accountId, row)。"""
    if not os.path.isdir(LEDGER_DIR):
        return
    for fname in os.listdir(LEDGER_DIR):
        if not fname.endswith(".jsonl"):
            continue
        acc_id = fname[: -len(".jsonl")]
        path = os.path.join(LEDGER_DIR, fname)
        try:
            with open(path, encoding="utf-8") as fh:
                for line in fh:
                    try:
                        row = json.loads(line)
                    except ValueError:
                        continue
                    yield acc_id, row
        except OSError:
            continue


def main() -> int:
    ap = argparse.ArgumentParser(description="按账号积分账本统计（官方明细口径）")
    ap.add_argument("--days", type=int, default=0, help="只看最近 N 天（0=全部）")
    ap.add_argument("--by-model", action="store_true", help="追加按账号×日×模型明细")
    ap.add_argument("--account", type=str, default=None, help="只看指定账号（显示名或 id 前缀）")
    args = ap.parse_args()

    names = load_account_names()
    id_by_label = {v: k for k, v in names.items()}

    cutoff_date = None
    if args.days > 0:
        cutoff_date = (dt.date.today() - dt.timedelta(days=args.days - 1)).strftime("%Y-%m-%d")

    daily: collections.Counter = collections.Counter()          # (acc, date) -> credit
    daily_model: collections.Counter = collections.Counter()    # (acc, date, model) -> credit
    reqs: collections.Counter = collections.Counter()           # (acc, date) -> count
    total: collections.Counter = collections.Counter()          # acc -> credit
    seen: set = set()

    for acc_id, row in iter_ledger_rows():
        label = names.get(acc_id, acc_id[:8])
        if args.account and args.account not in (label, acc_id):
            # 支持显示名或 id 前缀匹配
            if not acc_id.startswith(args.account):
                continue
        ts = row.get("ts")
        if not ts:
            continue
        date = dt.datetime.fromtimestamp(int(ts) / 1000).strftime("%Y-%m-%d")
        if cutoff_date and date < cutoff_date:
            continue
        credit = float(row.get("credit") or 0)
        model = str(row.get("model") or "?")
        key = (acc_id, row.get("requestId"), int(ts))
        if key in seen:
            continue
        seen.add(key)
        daily[(label, date)] += credit
        daily_model[(label, date, model)] += credit
        reqs[(label, date)] += 1
        total[label] += credit

    if not total:
        print(f"账本为空（{LEDGER_DIR}）。")
        print("触发方式：在统计页点「刷新」触发官方用量拉取，credit_ledger 会自动落盘。")
        return 0

    print("=" * 84)
    print("  本地账本 — 按账号积分消耗（官方逐笔明细口径，不受切号对齐影响）")
    print("=" * 84)

    print(f"\n  {'账号':<10}{'总扣分':>10}")
    for label, credit in sorted(total.items(), key=lambda kv: -kv[1]):
        print(f"  {label:<10}{credit:>10.1f}")

    print(f"\n  按账号 × 日（{'最近 ' + str(args.days) + ' 天' if cutoff_date else '全部'}）")
    print(f"  {'账号':<10}{'日期':<12}{'扣分':>10}{'请求':>7}")
    for (label, date), credit in sorted(daily.items(), key=lambda kv: (kv[0][0], kv[0][1])):
        if credit == 0 and reqs[(label, date)] == 0:
            continue
        print(f"  {label:<10}{date:<12}{credit:>10.1f}{reqs[(label, date)]:>7}")

    if args.by_model:
        print("\n  按账号 × 日 × 模型（仅扣分 > 0）")
        print(f"  {'账号':<10}{'日期':<12}{'模型':<24}{'扣分':>10}")
        for (label, date, model), credit in sorted(
            daily_model.items(), key=lambda kv: (kv[0][0], kv[0][1], -kv[1])
        ):
            if credit <= 0:
                continue
            print(f"  {label:<10}{date:<12}{model[:23]:<24}{credit:>10.1f}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
