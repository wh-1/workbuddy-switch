#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
验证 WorkBuddy 各模型「每日频率限制」的重置锚点。

背景：2026-09-12 19:01 实弹弹窗
  「当前您在 Deepseek-V4.1-Flash 模型的使用量已超出频率限制，
   可在 2026-09-13 14:26:22 重置可用」
→ 重置时刻 14:26:22 非日历零点，说明配额窗口是「锚定在每天 14:26:22 的 24h 窗口」，
  而非日历日(00:00-24:00)，也非「触顶后滚动 24h」（见下方 H2 否决检验）。

用法：
  python scripts/analysis/verify_reset_anchor.py
  python scripts/analysis/verify_reset_anchor.py --model deepseek-v4.1-flash
  python scripts/analysis/verify_reset_anchor.py --account 41f42fe1-5b4a-4573-8f95-3fd93980e787

仅读本地账本 ~/.wb-switch/credit_ledger/*.jsonl，不打任何接口。
"""
import os
import glob
import json
import argparse
from collections import defaultdict

LEDGER_DIR = os.path.join(os.path.expanduser("~"), ".wb-switch", "credit_ledger")
DEFAULT_MODEL = "deepseek-v4.1-flash"
# 实弹重置锚点（北京时间，来自 2026-09-12 弹窗）
KNOWN_RESET = "2026-09-13 14:26:22"
KNOWN_ANCHOR_HHMMSS = "14:26:22"


def load_rows(account=None):
    rows = []
    for f in sorted(glob.glob(os.path.join(LEDGER_DIR, "*.jsonl"))):
        acc = os.path.basename(f).replace(".jsonl", "")
        if account and acc != account:
            continue
        for line in open(f, encoding="utf-8"):
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            r["_account"] = acc
            rows.append(r)
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--account", default=None)
    args = ap.parse_args()

    rows = load_rows(args.account)
    ds = sorted(
        [r for r in rows if r["model"] == args.model],
        key=lambda r: (r["_account"], r["requestTime"]),
    )
    print(f"账本目录: {LEDGER_DIR}")
    print(f"模型: {args.model}  命中 {len(ds)} 条\n")

    byacc = defaultdict(list)
    for r in ds:
        byacc[r["_account"]].append(r)

    for acc, recs in sorted(byacc.items()):
        print(f"===== 账号 {acc[:8]}  ({len(recs)} 条) =====")
        byday = defaultdict(lambda: defaultdict(int))
        for r in recs:
            d, t = r["requestTime"].split(" ")
            byday[d][int(t[:2])] += 1
        for d in sorted(byday):
            # 标记是否跨越 14:26 锚点
            hours = sorted(byday[d])
            has_before = any(h < 14 for h in hours)
            has_after = any(h >= 14 for h in hours)
            flag = ""
            if has_before and has_after:
                flag = "  ← 含上一窗口尾巴(00-14) + 当前窗口(14-24)"
            elif has_after and not has_before:
                flag = "  ← 仅当前窗口(14:26 之后)"
            print(f"  {d}  hourly={dict(sorted(byday[d].items()))}{flag}")
        # 末次请求时间
        print(f"  首条 {recs[0]['requestTime']}  末条 {recs[-1]['requestTime']}")

        # H2 否决检验：若重置 = 末次请求 + 24h，应当等于 KNOWN_RESET
        last = recs[-1]["requestTime"]
        print(f"  [H2 检验] 末次请求+24h = ?  vs 已知重置 {KNOWN_RESET}")
        print(f"           末次请求 {last} → 若 H2 成立重置应为 {last[:11]}{_add24(last)}")
        print()


def _add24(ts):
    """返回 ts+24h 的 时分秒 字符串（用于 H2 演示，不依赖 datetime 解析库细节）。"""
    from datetime import datetime, timedelta
    dt = datetime.strptime(ts, "%Y-%m-%d %H:%M:%S")
    return (dt + timedelta(hours=24)).strftime("%H:%M:%S")


if __name__ == "__main__":
    main()
