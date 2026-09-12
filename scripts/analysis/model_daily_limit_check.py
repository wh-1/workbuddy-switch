#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""主力模型「当前窗口用量 vs 峰值基线」检查器（手动执行，不接入项目页面）。

只盯两个主力模型：deepseek-v4.1-flash、glm-5.3-flash。

【配额窗口口径，2026-09-12 实弹确认】
WorkBuddy 每模型每日频率限制 = **固定重置于每天 14:26:22（北京时间）** 的 24h 窗口，
窗口为 [14:26:22 → 次日 14:26:22)。非日历日(00:00)、非触顶后滚动 24h。
验证见 scripts/analysis/verify_reset_anchor.py（三账号账本交叉确认）。

故本脚本按「14:26:22 窗口」聚合（不再按日历日）：
  - 一条请求发生在 14:26:22 及之后 → 归入「当天」窗口（标签=当天日期）
  - 发生在 14:26:22 之前       → 归入「前一天」窗口（标签=前一天日期）
  例：09-12 00:46 → 窗口 09-11；09-12 16:12 → 窗口 09-12。

- 当前窗口用量：按账号汇总，取「单账号单窗口最大值」口径
- 峰值基线：~/.wb-switch/model_daily_peaks.json（首次运行从账本全史播种，之后只增不减）
- 当前窗口用量 > 基线峰值 → 更新基线（peak/date/account），并打印「超峰值」提示

设计说明：
- 每模型每日限额是「每账号每份」（多账号切号=扩容），故用量按账号分别算、取最大对比
- 账本是账单记录，不含「换模型原因」，无法判断停用是触顶还是主动；本脚本只做用量对比，不推断原因
- credit 为官方逐笔明细扣分；免费模型(hy3 等) credit 恒 0，不在此脚本范围

用法：
  python scripts/analysis/model_daily_limit_check.py
  python scripts/analysis/model_daily_limit_check.py --reseed   # 强制从账本重播基线
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import os
import time

HOME = os.path.expanduser("~")
LEDGER_DIR = os.path.join(HOME, ".wb-switch", "credit_ledger")
BASELINE = os.path.join(HOME, ".wb-switch", "model_daily_peaks.json")

TARGET_MODELS = ["deepseek-v4.1-flash", "glm-5.3-flash"]

# 实弹确认的重置锚点（北京时间）：窗口 [14:26:22 → 次日 14:26:22)
WINDOW_ANCHOR = dt.time(14, 26, 22)
SCHEME = "window-142622"

NAMES = {
    "c700b29a-005f-4ff8-8934-6111c95c12b1": "H",
    "41f42fe1-5b4a-4573-8f95-3fd93980e787": "Elaine",
    "15b9031b-1be5-4a40-a700-5fcee1f5d67a": "Harvey",
}


def window_date(ts_ms: int) -> str:
    """把 epoch 毫秒时间戳映射到所属配额窗口标签（= 窗口起点日期）。"""
    t = dt.datetime.fromtimestamp(int(ts_ms) / 1000)
    if t.time() >= WINDOW_ANCHOR:
        return t.date().strftime("%Y-%m-%d")
    return (t.date() - dt.timedelta(days=1)).strftime("%Y-%m-%d")


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
            w = window_date(int(ts))
            m = str(r.get("model") or "?")
            k = (aid, r.get("requestId"), int(ts))
            if k in seen:
                continue
            seen.add(k)
            yield acc, w, m, float(r.get("credit") or 0)


def daily_model_credit():
    """返回 {(acc, window, model): 窗口累计扣分}。"""
    agg = collections.defaultdict(float)
    for acc, w, m, c in iter_rows():
        agg[(acc, w, m)] += c
    return agg


def seed_from_history():
    """从账本全史播种峰值基线（max over 账号×窗口 的窗口累计）。

    必须先按 (账号,窗口,模型) 汇总成窗口累计，再取 max；
    直接拿 iter_rows 的逐行 credit 比大小会退化成「单笔最大」而非「单窗口累计」。
    """
    peak = {}
    for (acc, w, m), c in daily_model_credit().items():
        if m not in TARGET_MODELS:
            continue
        if c > peak.get(m, {}).get("peak", -1):
            peak[m] = {"peak": c, "date": w, "account": acc}
    return peak


def load_baseline(reseed: bool) -> tuple[dict, bool]:
    if os.path.exists(BASELINE) and not reseed:
        with open(BASELINE, encoding="utf-8") as fh:
            data = json.load(fh)
        if data.get("_scheme") == SCHEME:
            return data.get("peaks", {}), False
        # 旧口径（日历日）基线与窗口口径不兼容 → 强制重播
        print("⚠️ 检测到旧口径(日历日)基线，已按 14:26:22 窗口重新播种。")
        return seed_from_history(), True
    return seed_from_history(), True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--reseed", action="store_true", help="强制从账本重播峰值基线")
    args = ap.parse_args()

    now_ms = int(time.time() * 1000)
    today_window = window_date(now_ms)

    baseline, seeded = load_baseline(args.reseed)

    # 当前窗口各账号累计（仅目标模型）
    today_acc = collections.defaultdict(float)   # (model, acc) -> credit
    for (acc, w, m), c in daily_model_credit().items():
        if m in TARGET_MODELS and w == today_window:
            today_acc[(m, acc)] += c

    # 窗口范围展示
    wstart = dt.datetime.strptime(today_window, "%Y-%m-%d")
    wend = wstart + dt.timedelta(days=1)
    print(f"当前窗口：{today_window} 14:26:22 → {wend.strftime('%Y-%m-%d')} 14:26:22"
          + ("  （基线首次播种/重播）" if seeded else ""))

    # 已知账号（含当前窗口有活动的），保证每个账号一行
    accounts = sorted(set(NAMES.values()) | {a for (_, a) in today_acc})

    # 逐模型检查并更新基线（当前窗口跨账号最大值 > 基线峰值 → 更新）
    updated = {}
    for m in TARGET_MODELS:
        top_acc = max(accounts, key=lambda a: today_acc.get((m, a), 0.0))
        val = today_acc.get((m, top_acc), 0.0)
        if val > baseline.get(m, {}).get("peak", 0.0):
            baseline[m] = {"peak": val, "date": today_window, "account": top_acc}
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
            print(f"  {m} → {baseline[m]['peak']:.1f} ({today_window}, {a})")
    else:
        print("\n基线未变。")
    print(f"基线文件：{BASELINE}")
    with open(BASELINE, "w", encoding="utf-8") as fh:
        json.dump({"_scheme": SCHEME, "peaks": baseline}, fh,
                  ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
