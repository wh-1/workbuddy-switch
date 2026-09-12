#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""按账号×日×模型 积分折线图生成器（纯 SVG，零外部依赖，离线可看）。

数据源：~/.wb-switch/credit_ledger/<accountId>.jsonl
产出：reports/credit_ledger_charts.html
  - 每账号一张折线图（每线=一模型，X=日，Y=当日扣分）
  - 一张三账号总扣分对比总览（每线=一账号，X=日，Y=当日总扣分）

不重算口径：credit 直接取账本逐笔明细（官方明细口径，与官方接口一致）。
"""
from __future__ import annotations

import collections
import datetime as dt
import json
import os

HOME = os.path.expanduser("~")
LEDGER_DIR = os.path.join(HOME, ".wb-switch", "credit_ledger")
ACCOUNTS_FILE = os.path.join(HOME, ".wb-switch", "accounts.json")
OUT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "reports",
    "credit_ledger_charts.html",
)

# 账号 id -> 显示名（静态已知：廿七无用量无账本，自然排除）
KNOWN = {
    "c700b29a-005f-4ff8-8934-6111c95c12b1": "H",
    "41f42fe1-5b4a-4573-8f95-3fd93980e787": "Elaine",
    "15b9031b-1be5-4a40-a700-5fcee1f5d67a": "Harvey",
}


def load_names() -> dict[str, str]:
    try:
        d = json.load(open(ACCOUNTS_FILE, encoding="utf-8"))
    except (OSError, ValueError):
        return dict(KNOWN)
    accts = d.get("accounts") if isinstance(d, dict) else d
    m = {}
    for a in accts or []:
        if not isinstance(a, dict):
            continue
        aid = a.get("id") or a.get("accountId")
        nm = a.get("nickname") or a.get("name") or a.get("email") or aid[:8]
        if aid:
            m[str(aid)] = str(nm)
    m.update(KNOWN)
    return m


# 调色板（高区分度，适配深色背景）
PALETTE = [
    "#ff6b6b", "#4dd0e1", "#ffd166", "#06d6a0", "#b388ff",
    "#f78fb3", "#ff9f1c", "#5eead4", "#a3e635", "#f472b6",
    "#60a5fa", "#facc15",
]


def svg_line_chart(title, x_labels, series, y_label="当日扣分", height=420):
    """series: [(name, [values...]), ...]；len(values)==len(x_labels)。"""
    W, H = 960, height
    ml, mr, mt, mb = 64, 180, 48, 56
    pw, ph = W - ml - mr, H - mt - mb
    all_vals = [v for _, vals in series for v in vals]
    ymax = max(all_vals) if all_vals else 1
    ymax = (int(ymax / 10) + 1) * 10 if ymax > 0 else 10
    n = len(x_labels)
    if n <= 1:
        n = 2

    def xpos(i):
        return ml + (pw * i / (n - 1)) if n > 1 else ml + pw / 2

    def ypos(v):
        return mt + ph - (ph * v / ymax)

    svg = [f'<svg viewBox="0 0 {W} {H}" xmlns="http://www.w3.org/2000/svg" '
           f'font-family="ui-monospace,Menlo,Consolas,monospace">']
    svg.append(f'<rect x="0" y="0" width="{W}" height="{H}" fill="#0f1420"/>')
    svg.append(f'<text x="{ml}" y="26" fill="#e6edf3" font-size="16" '
               f'font-weight="700">{title}</text>')
    # y 网格 + 刻度
    ticks = 5
    for t in range(ticks + 1):
        val = ymax * t / ticks
        y = ypos(val)
        svg.append(f'<line x1="{ml}" y1="{y:.1f}" x2="{ml+pw}" y2="{y:.1f}" '
                   f'stroke="#1f2937" stroke-width="1"/>')
        svg.append(f'<text x="{ml-8}" y="{y+4:.1f}" fill="#8b98a9" font-size="11" '
                   f'text-anchor="end">{val:.0f}</text>')
    svg.append(f'<text x="{ml-8}" y="{mt-10}" fill="#8b98a9" font-size="11" '
               f'text-anchor="end">{y_label}</text>')
    # x 标签（稀疏显示，避免重叠）
    step = max(1, (n + 11) // 12)
    for i, lab in enumerate(x_labels):
        if i % step != 0 and i != n - 1:
            continue
        x = xpos(i)
        svg.append(f'<line x1="{x:.1f}" y1="{mt}" x2="{x:.1f}" y2="{mt+ph}" '
                   f'stroke="#161b26" stroke-width="1"/>')
        svg.append(f'<text x="{x:.1f}" y="{mt+ph+18}" fill="#8b98a9" font-size="10" '
                   f'text-anchor="middle" transform="rotate(35 {x:.1f} {mt+ph+18})">{lab[5:]}</text>')
    # 折线
    for idx, (name, vals) in enumerate(series):
        color = PALETTE[idx % len(PALETTE)]
        pts = " ".join(f"{xpos(i):.1f},{ypos(v):.1f}" for i, v in enumerate(vals))
        svg.append(f'<polyline points="{pts}" fill="none" stroke="{color}" '
                   f'stroke-width="2"/>')
        for i, v in enumerate(vals):
            if v > 0:
                svg.append(f'<circle cx="{xpos(i):.1f}" cy="{ypos(v):.1f}" r="2.2" fill="{color}"/>')
    # 图例
    lx = ml + pw + 16
    ly = mt + 10
    for idx, (name, vals) in enumerate(series):
        color = PALETTE[idx % len(PALETTE)]
        tot = sum(vals)
        y = ly + idx * 20
        svg.append(f'<rect x="{lx}" y="{y-10}" width="12" height="12" rx="2" fill="{color}"/>')
        svg.append(f'<text x="{lx+18}" y="{y}" fill="#e6edf3" font-size="11.5">'
                   f'{name}  (Σ{tot:.0f})</text>')
    svg.append("</svg>")
    return "\n".join(svg)


def main() -> int:
    names = load_names()
    daily_model = collections.defaultdict(float)   # (acc, date, model) -> credit
    daily_total = collections.defaultdict(float)   # (acc, date) -> credit
    per_acc_models: dict[str, set] = collections.defaultdict(set)
    seen: set = set()
    dates: set = set()

    for fname in os.listdir(LEDGER_DIR):
        if not fname.endswith(".jsonl"):
            continue
        acc_id = fname[: -len(".jsonl")]
        label = names.get(acc_id, acc_id[:8])
        with open(os.path.join(LEDGER_DIR, fname), encoding="utf-8") as fh:
            for line in fh:
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                ts = row.get("ts")
                if not ts:
                    continue
                date = dt.datetime.fromtimestamp(int(ts) / 1000).strftime("%Y-%m-%d")
                credit = float(row.get("credit") or 0)
                model = str(row.get("model") or "?")
                key = (acc_id, row.get("requestId"), int(ts))
                if key in seen:
                    continue
                seen.add(key)
                daily_model[(label, date, model)] += credit
                daily_total[(label, date)] += credit
                per_acc_models[label].add(model)
                dates.add(date)

    if not dates:
        print("账本为空。")
        return 0

    all_dates = sorted(dates)
    charts = []

    # 每账号一张
    for acc in sorted(per_acc_models):
        models = sorted(per_acc_models[acc])
        series = []
        for m in models:
            vals = [daily_model.get((acc, d, m), 0.0) for d in all_dates]
            series.append((m, vals))
        svg = svg_line_chart(f"{acc} · 按模型每日扣分", all_dates, series)
        charts.append((f"{acc} · 按模型每日扣分", svg))

    # 三账号总扣分对比总览
    accs = sorted(per_acc_models)
    series = [(a, [daily_total.get((a, d), 0.0) for d in all_dates]) for a in accs]
    svg = svg_line_chart("三账号 · 每日总扣分对比", all_dates, series,
                         y_label="当日总扣分")
    charts.append(("三账号 · 每日总扣分对比", svg))

    # 汇总数字
    totals = {a: sum(daily_total.get((a, d), 0.0) for d in all_dates) for a in accs}
    total_all = sum(totals.values())
    summary = (f"窗口 {all_dates[0]} ~ {all_dates[-1]}（{len(all_dates)} 天） · "
               f"总扣分 {total_all:.0f} · " +
               " · ".join(f"{a} {totals[a]:.0f}" for a in accs))

    html_parts = [
        "<!doctype html><html lang='zh'><head><meta charset='utf-8'>",
        "<title>积分账本 · 按账号×日×模型</title>",
        "<style>body{background:#0b0f17;color:#e6edf3;margin:0;"
        "font-family:ui-monospace,Menlo,Consolas,monospace;}"
        ".wrap{max-width:1040px;margin:0 auto;padding:24px;}"
        "h1{font-size:20px;margin:0 0 4px;} .sub{color:#8b98a9;font-size:13px;"
        "margin-bottom:20px;} .card{background:#0f1420;border:1px solid #1f2937;"
        "border-radius:12px;padding:16px;margin-bottom:22px;}"
        "svg{width:100%;height:auto;}</style></head><body><div class='wrap'>",
        f"<h1>积分账本 · 按账号 × 日 × 模型</h1>",
        f"<div class='sub'>{summary}</div>",
    ]
    for title, svg in charts:
        html_parts.append(f"<div class='card'>{svg}</div>")
    html_parts.append("</div></body></html>")

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as fh:
        fh.write("\n".join(html_parts))
    print(f"已生成：{OUT}")
    print(summary)
    for a in accs:
        print(f"  {a}: {totals[a]:.1f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
