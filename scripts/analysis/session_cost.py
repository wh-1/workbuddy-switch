#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""WorkBuddy 按对话（会话）统计 Token / 命中率 / 积分。

设计原则：**口径完全复用项目实现**，不另立标准。
  - Token / 命中率  → 调用 wb-switch-core 的 token_stats::get_statistics
                      （经 examples/dump_stats.rs 导出，口径与「Token 统计」页一致）
  - 积分            → workbuddy.db 的 session_usage.credit_json
                      （该表天然按会话存储：每行一个会话，值为 {traceId: 积分}）

三个指标都以**对话**为单位，不再按账号拆分。

用法：
  python session_cost.py                      # 近 30 天
  python session_cost.py --days 7             # 近 7 天
  python session_cost.py --days 0             # 全量
  python session_cost.py --top 30             # 只看前 30 个对话
  python session_cost.py --json out.json      # 导出合并结果
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import subprocess
import sys
import tempfile

HOME = os.path.expanduser("~")
DB_PATH = os.path.join(HOME, ".workbuddy", "workbuddy.db")
PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DUMP_EXE = os.path.join(PROJECT_ROOT, "target", "debug", "examples", "dump_stats.exe")


def run_project_stats(days: int | None) -> dict:
    """调用项目实现导出统计 JSON（口径与产品页一致）。"""
    if not os.path.exists(DUMP_EXE):
        raise SystemExit(
            f"未找到 {DUMP_EXE}\n"
            "请先编译：cargo build -p wb-switch-core --example dump_stats"
        )
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tmp:
        path = tmp.name
    cmd = [DUMP_EXE]
    if days:
        cmd.append(str(days))
    cmd += ["--json", path]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
    if proc.returncode != 0:
        raise SystemExit(f"统计导出失败：{proc.stderr or proc.stdout}")
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
    return data


def load_session_credits() -> dict:
    """返回 {session_id: {credit, updated_at, used, size}}。

    session_usage 每行 = 一个会话；credit_json 为该会话累积的 {traceId: 积分}。
    同一 traceId 跨行出现时取最大值，避免重复累加（实测差异 <0.5%）。
    """
    if not os.path.exists(DB_PATH):
        return {}
    uri = "file:" + DB_PATH.replace("\\", "/") + "?mode=ro"
    con = sqlite3.connect(uri, uri=True, timeout=5)
    con.row_factory = sqlite3.Row
    out: dict[str, dict] = {}
    trace_owner: dict[str, float] = {}
    try:
        cur = con.execute(
            "SELECT session_id, updated_at, used, size, credit_json "
            "FROM session_usage"
        )
        for row in cur:
            sid = row["session_id"]
            slot = out.setdefault(
                sid,
                {"credit": 0.0, "updated_at": row["updated_at"] or 0,
                 "used": row["used"] or 0, "size": row["size"] or 0},
            )
            raw = row["credit_json"]
            if not raw:
                continue
            try:
                data = json.loads(raw)
            except (TypeError, ValueError):
                continue
            for tid, val in data.items():
                try:
                    credit = float(val)
                except (TypeError, ValueError):
                    continue
                # 同一 traceId 只计一次，取最大值
                prev = trace_owner.get(tid)
                if prev is None or credit > prev:
                    trace_owner[tid] = credit
                    slot["credit"] += credit - (prev or 0.0)
    finally:
        con.close()
    return out


def fmt(n: float) -> str:
    if n >= 1e9:
        return f"{n/1e9:.2f}B"
    if n >= 1e6:
        return f"{n/1e6:.2f}M"
    if n >= 1e3:
        return f"{n/1e3:.1f}K"
    return f"{n:.0f}"


def truncate(text: str, width: int) -> str:
    text = text or "(无标题)"
    if len(text) <= width:
        return text
    return text[: width - 2] + ".."


def main() -> int:
    ap = argparse.ArgumentParser(description="按对话统计 Token / 命中率 / 积分")
    ap.add_argument("--days", type=int, default=30,
                    help="时间范围（天）；0 或负数=全量，默认 30")
    ap.add_argument("--top", type=int, default=0, help="只显示前 N 个对话")
    ap.add_argument("--json", type=str, default=None, help="导出合并结果 JSON")
    ap.add_argument("--source", type=str, default="workbuddy",
                    help="数据源（默认 workbuddy）")
    args = ap.parse_args()

    days = args.days if args.days and args.days > 0 else None
    data = run_project_stats(days)
    sources = data.get("sources") or []
    src = next(
        (s for s in sources if s.get("source") == args.source),
        sources[0] if sources else None,
    )
    if not src:
        print("未取到统计数据", file=sys.stderr)
        return 1

    credits = load_session_credits()
    sessions = src.get("sessions") or []
    summary = src.get("summary") or {}

    rows = []
    for s in sessions:
        sid = s.get("sessionId") or ""
        cred = credits.get(sid, {})
        rows.append(
            {
                "title": s.get("title") or "",
                "project": s.get("project") or "",
                "sessionId": sid,
                "total": s.get("total", 0),
                "input": s.get("input", 0),
                "output": s.get("output", 0),
                "cacheRead": s.get("cacheRead", 0),
                "records": s.get("records", 0),
                "hitRate": s.get("cacheHitRate") or 0.0,
                "credit": round(cred.get("credit", 0.0), 2),
            }
        )
    rows.sort(key=lambda r: -r["total"])

    scope = f"近 {days} 天" if days else "全量"
    print("=" * 92)
    print("  按对话统计 — Token / 命中率 / 积分")
    print("=" * 92)
    print(f"  范围: {scope}    数据源: {src.get('source')}    会话数: {len(rows)}")
    print()
    print(f"  Token 总览: Total {fmt(summary.get('total', 0))}  ·  "
          f"Input {fmt(summary.get('input', 0))}  ·  "
          f"Output {fmt(summary.get('output', 0))}")
    print(f"  缓存命中率: {(summary.get('cacheHitRate') or 0) * 100:.1f}%  ·  "
          f"调用 {summary.get('records', 0):,} 次  ·  "
          f"文件 {src.get('filesScanned', 0)}")
    credited = [r for r in rows if r["credit"] > 0]
    print(f"  积分: {sum(r['credit'] for r in rows):,.2f} "
          f"（其中 {len(credited)} 个对话有积分记账）")
    print()
    print("-" * 92)
    print(f"  {'对话':<34} {'项目':<16} {'Total':>9} {'命中率':>7} {'调用':>6} {'积分':>9}")
    print("-" * 92)

    shown = rows[: args.top] if args.top else rows
    for r in shown:
        label = truncate(r["title"], 34)
        print(f"  {label:<34} {truncate(r['project'], 16):<16} "
              f"{fmt(r['total']):>9} {r['hitRate'] * 100:>6.1f}% "
              f"{r['records']:>6} {r['credit']:>9.2f}")
    print("-" * 92)
    if args.top and len(rows) > args.top:
        rest = rows[args.top:]
        print(f"  其余 {len(rest)} 个对话: Total {fmt(sum(r['total'] for r in rest))}  ·  "
              f"积分 {sum(r['credit'] for r in rest):,.2f}")

    print()
    print("=" * 92)
    print("  说明")
    print("=" * 92)
    print("  · Token / 命中率 口径与产品「Token 统计」页完全一致")
    print("    （total = input + output + cacheWrite；命中率 = cacheRead / input）")
    print("  · 积分来自 workbuddy.db session_usage.credit_json（服务端本地记账）")
    print("  · 删对话会清 session_usage → 无积分的对话多为已删除或纯免费额度消耗")
    print()

    if args.json:
        out = {
            "range_days": days,
            "source": src.get("source"),
            "summary": summary,
            "sessions": rows,
        }
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(out, fh, ensure_ascii=False, indent=2)
        print(f"  JSON 已写入 {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
