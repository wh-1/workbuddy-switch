#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""workbuddy-switch 积分/Token 消耗诊断

用途：量化本机 WorkBuddy 的积分与 Token 消耗分布，定位浪费点。

数据源（全部本地、只读）：
  1. ~/.workbuddy/workbuddy.db  session_usage 表
     - credit_json: {traceId: 积分}  ← 真实的积分消耗明细（官方记账）
     - size: 上下文窗口大小, used: 已用
  2. ~/.workbuddy/projects/**/*.jsonl  （排除 subagents/）
     - providerData.traceId / model / usage.{inputTokens,outputTokens,...}
     - 通过 traceId 把积分精确关联到 模型 / 项目 / 会话 / 时间

设计口径：
  - 积分 = 服务端记账（credit_json），绝对权威
  - Token = 从 JSONL 派生（本机请求记录）
  - 二者通过 traceId join，可实现「按模型/项目 分摊积分」

用法：
  python credit_diagnose.py                 # 全量
  python credit_diagnose.py --days 30       # 近 30 天
  python credit_diagnose.py --json out.json # 额外导出 JSON
"""
from __future__ import annotations

import argparse
import datetime as dt
import glob
import json
import os
import sqlite3
import sys
from collections import defaultdict

import account_timeline as at

HOME = os.path.expanduser("~")
DB_PATH = os.path.join(HOME, ".workbuddy", "workbuddy.db")
PROJECTS_DIR = os.path.join(HOME, ".workbuddy", "projects")
AUTH_DIR = at.AUTH_DIR

# 账号时间轴归因已抽到共享模块 account_timeline.py（session_cost.py 也用它）
load_account_timeline = at.load_account_timeline
account_at = at.account_at


def open_db():
    """只读打开，绝不写锁。"""
    uri = "file:" + DB_PATH.replace("\\", "/") + "?mode=ro"
    con = sqlite3.connect(uri, uri=True, timeout=5)
    con.row_factory = sqlite3.Row
    return con


def load_credits(con):
    """返回 {traceId: (credit, session_id, updated_at)}

    注意：一个 traceId 只应计入一次。session_usage 每会话一行，
    credit_json 为该会话累积的 {traceId: 积分} 映射。
    """
    out = {}
    cur = con.execute(
        "SELECT session_id, updated_at, credit_json FROM session_usage "
        "WHERE credit_json IS NOT NULL AND credit_json != ''"
    )
    for row in cur:
        try:
            data = json.loads(row["credit_json"])
        except Exception:
            continue
        for tid, val in data.items():
            try:
                credit = float(val)
            except (TypeError, ValueError):
                continue
            # 同一 traceId 可能出现在多行（理论上不该），取较大值避免重复累加
            prev = out.get(tid)
            if prev is None or credit > prev[0]:
                out[tid] = (credit, row["session_id"], row["updated_at"] or 0)
    return out


def num(obj, *keys):
    for k in keys:
        v = obj.get(k)
        if isinstance(v, (int, float)):
            return float(v)
    return 0.0


def usage_of(pd):
    """从 providerData 取 usage（含嵌套别名）。"""
    u = pd.get("usage")
    if not isinstance(u, dict):
        u = {}
    raw = pd.get("rawUsage")
    if not isinstance(raw, dict):
        raw = {}

    inp = num(u, "inputTokens", "input_tokens", "prompt_tokens") or num(
        raw, "prompt_tokens", "input_tokens"
    )
    outp = num(u, "outputTokens", "output_tokens", "completion_tokens") or num(
        raw, "completion_tokens", "output_tokens"
    )
    total = num(u, "totalTokens", "total_tokens") or num(raw, "total_tokens")
    if not total:
        total = inp + outp
    # 缓存读（便宜）
    cached = 0.0
    details = u.get("inputTokensDetails")
    if isinstance(details, list):
        for d in details:
            if isinstance(d, dict):
                cached += num(d, "cached_tokens", "cachedTokens")
    if not cached:
        pd_raw = raw.get("prompt_tokens_details")
        if isinstance(pd_raw, dict):
            cached = num(pd_raw, "cached_tokens", "cachedTokens")
    return inp, outp, total, cached, 1


def scan_jsonl(cutoff_ms=None):
    """扫描 JSONL，返回 records 列表。"""
    records = []
    pattern = os.path.join(PROJECTS_DIR, "**", "*.jsonl")
    for path in glob.glob(pattern, recursive=True):
        # 排除 subagents（口径与项目 token_stats 一致）
        norm = path.replace("\\", "/")
        if "/subagents/" in norm:
            continue
        rel = os.path.relpath(path, PROJECTS_DIR).replace("\\", "/")
        project = rel.split("/")[0] if "/" in rel else "(root)"
        try:
            with open(path, "r", encoding="utf-8", errors="ignore") as fh:
                for line in fh:
                    if '"usage"' not in line and '"rawUsage"' not in line:
                        continue
                    try:
                        obj = json.loads(line)
                    except Exception:
                        continue
                    pd = obj.get("providerData")
                    if not isinstance(pd, dict):
                        continue
                    if not (pd.get("usage") or pd.get("rawUsage")):
                        continue
                    ts = obj.get("timestamp") or 0
                    if cutoff_ms and ts and ts < cutoff_ms:
                        continue
                    inp, outp, total, cached, reqs = usage_of(pd)
                    if total <= 0:
                        continue
                    records.append(
                        {
                            "trace_id": pd.get("traceId") or "",
                            "model": pd.get("model") or "(unknown)",
                            "agent": pd.get("agent") or "(unknown)",
                            "project": project,
                            "session": (obj.get("sessionId") or os.path.splitext(os.path.basename(path))[0]),
                            "ts": ts,
                            "input": inp,
                            "output": outp,
                            "total": total,
                            "cached": cached,
                            "reqs": reqs,
                        }
                    )
        except OSError:
            continue
    return records


def fmt(n):
    if n >= 1_000_000:
        return f"{n/1_000_000:.2f}M"
    if n >= 1_000:
        return f"{n/1_000:.1f}K"
    return f"{n:.0f}"


def fmt_credit(n):
    return f"{n:,.2f}"


def report(records, credits, days):
    print("=" * 78)
    print("  WorkBuddy 积分 / Token 消耗诊断")
    print("=" * 78)
    now = dt.datetime.now()
    scope = f"近 {days} 天" if days else "全量"
    print(f"  生成时间: {now:%Y-%m-%d %H:%M:%S}   范围: {scope}")
    print()

    # ---------- 1. 积分总览（权威） ----------
    print("-" * 78)
    print("【1】积分总览（来源：workbuddy.db session_usage，服务端记账口径）")
    print("-" * 78)

    cutoff_ms = None
    if days:
        cutoff_ms = int((now - dt.timedelta(days=days)).timestamp() * 1000)

    cred_items = []
    for tid, (c, sid, up) in credits.items():
        if cutoff_ms and up and up < cutoff_ms:
            continue
        cred_items.append((tid, c, sid, up))

    total_credit = sum(c for _, c, _, _ in cred_items)
    print(f"  可归属请求数: {len(cred_items):,}        合计积分: {fmt_credit(total_credit)}")
    print(f"  （本机 credit_json 记录总数 {len(credits):,}，"
          f"其中落在范围内 {len(cred_items):,}）")

    # 归因覆盖率：有多少去重 traceId 有积分记录
    jsonl_traces = {r["trace_id"] for r in records if r["trace_id"]}
    covered = jsonl_traces & set(credits.keys())
    if jsonl_traces:
        cov = len(covered) / len(jsonl_traces) * 100
        print()
        print(f"  归因覆盖率: {len(covered):,} / {len(jsonl_traces):,} 个去重请求 "
              f"= {cov:.1f}%")
        print(f"  → 未覆盖的 {len(jsonl_traces)-len(covered):,} 个请求不产生积分")
        print("     记账（免费额度 / 订阅内含额度内消耗），属正常现象。")
    if not cred_items:
        print("  ⚠️  范围内无积分记账 —— 可能被清理过，或时间窗口太窄。")
    print()

    # ---------- 2. Token 总览 ----------
    print("-" * 78)
    print("【2】Token 总览（来源：JSONL 派生，本机请求记录）")
    print("-" * 78)
    t_in = sum(r["input"] for r in records)
    t_out = sum(r["output"] for r in records)
    t_tot = sum(r["total"] for r in records)
    t_cache = sum(r["cached"] for r in records)
    print(f"  请求数:   {len(records):,}")
    print(f"  Input:    {fmt(t_in)}")
    print(f"  Output:   {fmt(t_out)}")
    print(f"  Total:    {fmt(t_tot)}")
    if t_in > 0:
        print(f"  缓存命中: {fmt(t_cache)}  (占 input {t_cache/t_in*100:.1f}%)")
    print()

    # ---------- 3. 按模型 ----------
    print("-" * 78)
    print("【3】按模型分布")
    print("-" * 78)
    by_model = defaultdict(lambda: {"total": 0.0, "input": 0.0, "output": 0.0,
                                    "cached": 0.0, "reqs": 0, "credit": 0.0})
    cred_by_trace = {tid: c for tid, c, _, _ in cred_items}
    # 积分必须按「唯一 traceId」归属一次，否则 JSONL 里同一 traceId 的多条
    # 记录（一次请求会写入多条：reasoning/function_call/message 等）会重复累加。
    model_of_trace = {}
    proj_of_trace = {}
    for r in records:
        tid = r["trace_id"]
        if tid and tid not in model_of_trace:
            model_of_trace[tid] = r["model"]
            proj_of_trace[tid] = r["project"]
    for r in records:
        m = by_model[r["model"]]
        m["total"] += r["total"]
        m["input"] += r["input"]
        m["output"] += r["output"]
        m["cached"] += r["cached"]
        m["reqs"] += 1
    # 积分按唯一 traceId 归到其模型
    for tid, c in cred_by_trace.items():
        mdl = model_of_trace.get(tid)
        if mdl is not None:
            by_model[mdl]["credit"] += c

    print(f"  {'模型':<26} {'请求':>6} {'Total':>9} {'Input':>9} {'Output':>8} {'积分':>10}")
    for name, m in sorted(by_model.items(), key=lambda kv: -kv[1]["total"])[:20]:
        print(f"  {name[:26]:<26} {m['reqs']:>6} {fmt(m['total']):>9} "
              f"{fmt(m['input']):>9} {fmt(m['output']):>8} "
              f"{fmt_credit(m['credit']):>10}")
    print()

    # ---------- 3.5 按账号 ----------
    print("-" * 78)
    print("【3.5】按账号分布（归属由官方 auth 备份时间线推断）")
    print("-" * 78)
    events, names = load_account_timeline()
    if not events:
        print("  ⚠️  未找到 auth 备份，跳过。")
    else:
        print(f"  时间线事件: {len(events)} 个，覆盖账号 {len(names)} 个")
        for uid, nick in sorted(names.items(), key=lambda kv: kv[1]):
            print(f"    {nick:<12} {uid}")
        print()
        by_acct = defaultdict(lambda: {"total": 0.0, "reqs": 0, "credit": 0.0,
                                       "sessions": set()})
        acct_of_ts = {}
        for r in records:
            uid = account_at(events, r["ts"])
            if not uid:
                continue
            nick = names.get(uid, uid[:8])
            a = by_acct[nick]
            a["total"] += r["total"]
            a["reqs"] += 1
            a["sessions"].add(r["session"])
            acct_of_ts[r["ts"]] = nick
        # 积分按 updated_at 推账号（与归日口径一致）
        for tid, c, _sid, up in cred_items:
            uid = account_at(events, up)
            if not uid:
                continue
            by_acct[names.get(uid, uid[:8])]["credit"] += c
        print(f"  {'账号':<12} {'请求':>6} {'Total':>9} {'积分':>10} {'会话':>5}")
        for nick, a in sorted(by_acct.items(), key=lambda kv: -kv[1]["total"]):
            print(f"  {nick:<12} {a['reqs']:>6} {fmt(a['total']):>9} "
                  f"{fmt_credit(a['credit']):>10} {len(a['sessions']):>5}")
        print()
        # 积分效率：每百万 token 扣多少积分（越高说明该账号额度越紧张）
        print("  积分效率（积分 / 百万 token，越高=该账号额度越紧张）：")
        rates = []
        for nick, a in by_acct.items():
            if a["total"] > 0:
                rates.append((nick, a["credit"] / (a["total"] / 1e6)))
        for nick, rate in sorted(rates, key=lambda kv: -kv[1]):
            bar = "█" * min(30, int(rate * 5)) if rate > 0 else ""
            print(f"    {nick:<12} {rate:>10.2f}  {bar}")
        print()

    # ---------- 4. 按项目 ----------
    print("-" * 78)
    print("【4】按项目分布（Top 15）")
    print("-" * 78)
    by_proj = defaultdict(lambda: {"total": 0.0, "input": 0.0, "reqs": 0,
                                   "credit": 0.0, "sessions": set()})
    for r in records:
        p = by_proj[r["project"]]
        p["total"] += r["total"]
        p["input"] += r["input"]
        p["reqs"] += 1
        p["sessions"].add(r["session"])
    for tid, c in cred_by_trace.items():
        prj = proj_of_trace.get(tid)
        if prj is not None:
            by_proj[prj]["credit"] += c
    print(f"  {'项目':<38} {'请求':>6} {'Total':>9} {'积分':>10} {'会话':>5}")
    for name, p in sorted(by_proj.items(), key=lambda kv: -kv[1]["total"])[:15]:
        print(f"  {name[:38]:<38} {p['reqs']:>6} {fmt(p['total']):>9} "
              f"{fmt_credit(p['credit']):>10} {len(p['sessions']):>5}")
    print()

    # ---------- 5. 按天 ----------
    print("-" * 78)
    print("【5】按天趋势（近 14 个活跃日）")
    print("-" * 78)
    print("  ⚠️  积分按 session_usage.updated_at 归日（避免重复累加）；")
    print("     请求量按 JSONL timestamp 归日。两列口径不同，不宜直接相除。")
    print("     注：某一行的 credit_json 是该会话累积值，其 updated_at 为最后写入时间，")
    print("     故积分会集中显示在会话活跃的最后一天，非逐日实际扣分。")
    print()
    by_day = defaultdict(lambda: {"total": 0.0, "reqs": 0, "credit": 0.0})
    for r in records:
        if not r["ts"]:
            continue
        d = dt.datetime.fromtimestamp(r["ts"] / 1000).strftime("%Y-%m-%d")
        by_day[d]["total"] += r["total"]
        by_day[d]["reqs"] += 1
    # 积分单独按 updated_at 归日
    for tid, c, _sid, up in cred_items:
        if not up:
            continue
        d = dt.datetime.fromtimestamp(up / 1000).strftime("%Y-%m-%d")
        by_day[d]["credit"] += c
    days_sorted = sorted(by_day.items(), key=lambda kv: kv[0])[-14:]
    print(f"  {'日期':<12} {'请求':>6} {'Total':>9} {'积分*':>10}  趋势")
    peak = max((v["total"] for _, v in days_sorted), default=1)
    for d, v in days_sorted:
        bar = "█" * int(v["total"] / peak * 30) if peak else ""
        print(f"  {d:<12} {v['reqs']:>6} {fmt(v['total']):>9} "
              f"{fmt_credit(v['credit']):>10}  {bar}")
    print()

    # ---------- 5.5 账号 × 日期矩阵 ----------
    if events:
        print("-" * 78)
        print("【5.5】账号 × 日期消耗矩阵（M token）")
        print("-" * 78)
        mat = defaultdict(lambda: defaultdict(float))
        for r in records:
            uid = account_at(events, r["ts"])
            if not uid:
                continue
            d = dt.datetime.fromtimestamp(r["ts"] / 1000).strftime("%m-%d")
            mat[d][names.get(uid, uid[:8])] += r["total"] / 1e6
        accts = sorted({a for v in mat.values() for a in v})
        recent = sorted(mat)[-14:]
        header = f"  {'日期':<8}" + "".join(f"{a:>10}" for a in accts)
        print(header)
        for d in recent:
            row = f"  {d:<8}" + "".join(
                f"{mat[d].get(a, 0.0):>10.1f}" for a in accts
            )
            print(row)
        print()

    # ---------- 6. 浪费点诊断 ----------
    print("-" * 78)
    print("【6】浪费点诊断")
    print("-" * 78)
    findings = []

    # 6.1 缓存命中率
    if t_in > 0:
        rate = t_cache / t_in * 100
        if rate < 50:
            findings.append(
                f"缓存命中率偏低：{rate:.1f}%（缓存读≈1/10 价）→ "
                f"提升空间大，减少重复读大文件/反复粘贴上下文"
            )
        else:
            findings.append(f"缓存命中率 {rate:.1f}%（良好）")

    # 6.2 请求规模分层
    buckets = [
        ("<50K", 0, 50_000),
        ("50K-100K", 50_000, 100_000),
        ("100K-200K", 100_000, 200_000),
        (">200K", 200_000, float("inf")),
    ]
    if t_tot > 0:
        print("  请求规模分布：")
        for label, lo, hi in buckets:
            sel = [r for r in records if lo <= r["total"] < hi]
            tok = sum(r["total"] for r in sel)
            if not sel:
                continue
            pct_n = len(sel) / len(records) * 100
            pct_t = tok / t_tot * 100
            print(f"    {label:<12} {len(sel):>6} 次 ({pct_n:>5.1f}%)  "
                  f"{fmt(tok):>9}  ({pct_t:>5.1f}%)")
        print()
        # 头部集中度：最大的 10% 请求吃掉多少
        sorted_r = sorted(records, key=lambda r: -r["total"])
        top10n = max(1, len(sorted_r) // 10)
        top10tok = sum(r["total"] for r in sorted_r[:top10n])
        findings.append(
            f"头部集中：最大 10% 请求（{top10n:,} 个）消耗 {fmt(top10tok)} "
            f"= {top10tok/t_tot*100:.1f}% 总 token → 控制单次请求体量是最大杠杆"
        )

    # 6.3 Output 占比
    if t_tot > 0:
        orate = t_out / t_tot * 100
        if orate > 15:
            findings.append(f"Output 占比 {orate:.1f}%（偏高）→ 长文生成集中，可考虑分批")

    # 6.4 Input/Output 比
    if t_out > 0:
        ratio = t_in / t_out
        if ratio > 20:
            findings.append(
                f"Input/Output = {ratio:.1f}:1（输入远大于输出）→ "
                f"典型「读多写少」，靠上下文纪律压制最有效"
            )

    # 6.5 高消耗会话
    by_sess = defaultdict(float)
    for r in records:
        by_sess[r["session"]] += r["total"]
    if by_sess:
        top_sess = sorted(by_sess.items(), key=lambda kv: -kv[1])[:5]
        top5 = sum(v for _, v in top_sess)
        findings.append(
            f"Top 5 会话消耗 {fmt(top5)} = {top5/t_tot*100:.1f}% 总消耗 → "
            f"长会话是主要成本项（压缩 ≥2 次就该收尾换会话）"
        )

    # 6.6 积分时间分布（近期是否真在扣分）
    now_ms = int(now.timestamp() * 1000)
    d7 = sum(c for _, c, _, up in cred_items if up and up >= now_ms - 7 * 86_400_000)
    d30 = sum(c for _, c, _, up in cred_items if up and up >= now_ms - 30 * 86_400_000)
    findings.append(
        f"积分时间分布：近 7 天 {fmt_credit(d7)} / 近 30 天 {fmt_credit(d30)} "
        f"/ 累计 {fmt_credit(total_credit)}"
    )
    if total_credit > 0 and d7 / total_credit < 0.05:
        findings.append(
            "⚠️  近 7 天几乎未产生积分消耗 → 当前账号额度充足（消耗在免费/套餐额度内），"
            "「积分不够」多半不是当下的瓶颈"
        )

    # 6.7 未归属积分（JSONL 已清理或跨设备）
    attributed = sum(cred_by_trace.get(t, 0.0) for t in model_of_trace)
    unattr = total_credit - attributed
    if unattr > 0.01:
        findings.append(
            f"未归属积分 {fmt_credit(unattr)} "
            f"({unattr/total_credit*100:.1f}%) → 对应会话的 JSONL 已不在本地"
            f"（多为历史/其他设备/已清理），无法拆到模型"
        )

    # 6.8 账号额度紧张度
    if events:
        _ev_rates = []
        _bynick = defaultdict(lambda: {"total": 0.0, "credit": 0.0})
        for r in records:
            uid = account_at(events, r["ts"])
            if uid:
                _bynick[names.get(uid, uid[:8])]["total"] += r["total"]
        for tid, c, _sid, up in cred_items:
            uid = account_at(events, up)
            if uid:
                _bynick[names.get(uid, uid[:8])]["credit"] += c
        for nick, a in _bynick.items():
            if a["total"] > 0 and a["credit"] > 0:
                _ev_rates.append((nick, a["credit"] / (a["total"] / 1e6)))
        if _ev_rates:
            tight = max(_ev_rates, key=lambda kv: kv[1])
            loose = min(_ev_rates, key=lambda kv: kv[1])
            findings.append(
                f"账号额度紧张度：{tight[0]} 最紧（{tight[1]:.1f} 积分/百万 token），"
                f"{loose[0]} 最松（{loose[1]:.1f}）→ 重负载优先走 {loose[0]}"
            )

    for i, f in enumerate(findings, 1):
        print(f"  {i}. {f}")
    print()

    # ---------- 7. 结论 ----------
    print("=" * 78)
    print("【7】结论")
    print("=" * 78)
    if cred_items and t_tot:
        cred_idx = total_credit / t_tot * 1000
        print(f"  综合单价: 约 {cred_idx:.3f} 积分 / 1K token")
    print(f"  总消耗:   Token {fmt(t_tot)}  ·  积分 {fmt_credit(total_credit)}")
    if by_model:
        top = max(by_model.items(), key=lambda kv: kv[1]["total"])
        print(f"  最烧模型: {top[0]}（{fmt(top[1]['total'])} token, "
              f"{fmt_credit(top[1]['credit'])} 积分）")
    if by_proj:
        top = max(by_proj.items(), key=lambda kv: kv[1]["total"])
        print(f"  最烧项目: {top[0]}（{fmt(top[1]['total'])} token, "
              f"{fmt_credit(top[1]['credit'])} 积分）")
    print()


def main():
    ap = argparse.ArgumentParser(description="WorkBuddy 积分/Token 消耗诊断")
    ap.add_argument("--days", type=int, default=None, help="时间范围（天），默认全量")
    ap.add_argument("--json", type=str, default=None, help="导出 JSON 路径")
    args = ap.parse_args()

    if not os.path.exists(DB_PATH):
        print(f"错误：找不到 {DB_PATH}", file=sys.stderr)
        return 1

    con = open_db()
    try:
        credits = load_credits(con)
    finally:
        con.close()

    cutoff_ms = None
    if args.days:
        cutoff_ms = int((dt.datetime.now() - dt.timedelta(days=args.days)).timestamp() * 1000)

    records = scan_jsonl(cutoff_ms)
    report(records, credits, args.days)

    if args.json:
        events, names = load_account_timeline()
        acct_out = {}
        for r in records:
            uid = account_at(events, r["ts"]) if events else None
            if not uid:
                continue
            nick = names.get(uid, uid[:8])
            slot = acct_out.setdefault(nick, {"tokens": 0.0, "requests": 0,
                                              "credit": 0.0})
            slot["tokens"] += r["total"]
            slot["requests"] += 1
        for c, _sid, up in credits.values():
            uid = account_at(events, up) if events else None
            if not uid:
                continue
            nick = names.get(uid, uid[:8])
            slot = acct_out.setdefault(nick, {"tokens": 0.0, "requests": 0,
                                              "credit": 0.0})
            slot["credit"] += c
        for slot in acct_out.values():
            slot["credit_per_mtok"] = round(
                slot["credit"] / (slot["tokens"] / 1e6), 2
            ) if slot["tokens"] > 0 else 0.0

        out = {
            "generated_at": dt.datetime.now().isoformat(),
            "range_days": args.days,
            "credits_total": round(sum(c for c, _, _ in credits.values()), 2),
            "records": len(records),
            "tokens_total": sum(r["total"] for r in records),
            "accounts": acct_out,
        }
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, indent=2)
        print(f"  JSON 摘要已写入 {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
