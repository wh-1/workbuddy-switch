#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""主力模型「今日用量 vs 峰值基线」检查器（手动执行，不接入项目页面）。

只盯两个主力模型：deepseek-v4.1-flash、glm-5.3-flash。
- 今日用量：按账号汇总，取「单账号单日最大值」口径（与 model_max_daily.md 峰值表一致）
- 峰值基线：~/.wb-switch/model_daily_peaks.json（首次运行从账本全史播种，之后只增不减）
- 今日用量 > 基线峰值 → 更新基线（peak/date/account），并打印「超峰值」提示

设计说明：
- 每模型每日限额是「每账号每份」（多账号切号=扩容），故今日用量按账号分别算、取最大对比
- 账本是账单记录，不含「换模型原因」，无法判断停用是触顶还是主动；本脚本只做用量对比，不推断原因
- credit 为官方逐笔明细扣分；免费模型(hy3 等) credit 恒 0，不在此脚本范围

用法：
  python scripts/analysis/model_daily_limit_check.py
"""
from __future__ import annotations

import collections
import datetime as dt
import json
import os

HOME = os.path.expanduser("~")
LEDGER_DIR = os.path.join(HOME, ".wb-switch", "credit_ledger")
BASELINE = os.path.join(HOME, ".wb-switch", "model_daily_peaks.json")

TARGET_MODELS = ["deepseek-v4.1-flash", "glm-5.3-flash"]

NAMES = {
    "c700b29a-005f-4ff8-8934-6111c95c12b1": "H",
    "41f42fe1-5b4a-4573-8f95-3fd93980e787": "Elaine",
    "15b9031b-1be5-4a40-a700-5fcee1f5d67a": "Harvey",
}


def iter_rows():
    seen = set()
    for f in os.listdir(LEDGER_DIR):
        if not f.endswith(".jsonl"):
            continue
        aid = f[: -len(".jsonl")]
        acc = NAMES.get(aid, aid[:8])
        for line in open(os.path.join(LEDGER_DIR, f), encoding="utf-8"):
            try:
                r = json.loads(line)
            except ValueError:
                continue
            ts = r.get("ts")
            if not ts:
                continue
            d = dt.datetime.fromtimestamp(int(ts) / 1000).strftime("%Y-%m-%d")
            m = str(r.get("model") or "?")
            k = (aid, r.get("requestId"), int(ts))
            if k in seen:
                continue
            seen.add(k)
            yield acc, d, m, float(r.get("credit") or 0)


def daily_model_credit():
    """返回 {(acc, date, model): 当日累计扣分}。"""
    agg = collections.defaultdict(float)
    for acc, d, m, c in iter_rows():
        agg[(acc, d, m)] += c
    return agg


def seed_from_history():
    """从账本全史播种峰值基线（max over 账号×天 的日累计）。

    注意：必须先用 daily_model_credit 按 (账号,日,模型) 汇总成日累计，
    再取 max；直接拿 iter_rows 的逐行 credit 比大小会退化成「单笔最大」而非「单日累计」。
    """
    peak = {}
    for (acc, d, m), c in daily_model_credit().items():
        if m not in TARGET_MODELS:
            continue
        if c > peak.get(m, {}).get("peak", -1):
            peak[m] = {"peak": c, "date": d, "account": acc}
    return peak


def main() -> int:
    today = dt.date.today().strftime("%Y-%m-%d")

    # 加载或播种基线
    if os.path.exists(BASELINE):
        with open(BASELINE, encoding="utf-8") as fh:
            baseline = json.load(fh)
        seeded = False
    else:
        baseline = seed_from_history()
        seeded = True

    # 今日各账号日累计（仅目标模型）
    today_acc = collections.defaultdict(float)   # (model, acc) -> credit
    for (acc, d, m), c in daily_model_credit().items():
        if m in TARGET_MODELS and d == today:
            today_acc[(m, acc)] += c

    print(f"检查日期：{today}"
          + ("  （基线首次运行，已从账本全史播种）" if seeded else ""))

    # 已知账号（含今日有活动的），保证每个账号一行
    accounts = sorted(set(NAMES.values()) | {a for (_, a) in today_acc})

    # 逐模型检查并更新基线（今日跨账号最大值 > 基线峰值 → 更新）
    updated = {}
    for m in TARGET_MODELS:
        top_acc = max(accounts, key=lambda a: today_acc.get((m, a), 0.0))
        val = today_acc.get((m, top_acc), 0.0)
        if val > baseline.get(m, {}).get("peak", 0.0):
            baseline[m] = {"peak": val, "date": today, "account": top_acc}
            updated[m] = top_acc

    # 表格：首行=基线，其后每行=一个账号
    c0, c1, c2, c3 = 8, 26, 26, 10
    print(f"{'账号':<{c0}}{TARGET_MODELS[0]:<{c1}}{TARGET_MODELS[1]:<{c2}}状态")
    print("-" * (c0 + c1 + c2 + c3))
    p0 = baseline.get(TARGET_MODELS[0], {}).get("peak", 0.0)
    p1 = baseline.get(TARGET_MODELS[1], {}).get("peak", 0.0)
    base_status = "已更新" if updated else "未变"
    print(f"{'基线':<{c0}}{p0:<{c1}.1f}{p1:<{c2}.1f}{base_status:>{c3}}")
    for a in accounts:
        v0 = today_acc.get((TARGET_MODELS[0], a), 0.0)
        v1 = today_acc.get((TARGET_MODELS[1], a), 0.0)
        r0 = f"{v0:.1f} ({v0 / p0 * 100:.1f}%)" if p0 else f"{v0:.1f}"
        r1 = f"{v1:.1f} ({v1 / p1 * 100:.1f}%)" if p1 else f"{v1:.1f}"
        status = "超峰值" if a in updated.values() else "正常"
        print(f"{a:<{c0}}{r0:<{c1}}{r1:<{c2}}{status:>{c3}}")

    if updated:
        print("\n⚠️ 超峰值，基线已更新：")
        for m, a in updated.items():
            print(f"  {m} → {baseline[m]['peak']:.1f} ({today}, {a})")
    else:
        print("\n基线未变。")
    print(f"基线文件：{BASELINE}")
    with open(BASELINE, "w", encoding="utf-8") as fh:
        json.dump(baseline, fh, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
