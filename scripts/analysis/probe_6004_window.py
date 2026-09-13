#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
probe_6004_window.py — 6004 滑动窗口长反推 + 首请求定位（标准查询）

用途：给定 6004 频率限制事件，自动反推其「窗口长」与「窗口内首条请求时间」，
     并校验「重置时刻 − 窗口长 ≈ 窗口首请求」规律，定档到已知模型限流画像。

数据源（只读）：~/.workbuddy/logs/<日期>/sdk/conversations/*.log
⚠️ 关键约定（已踩坑）：
  - 每行格式 `<UTC时间戳> method:... {JSON}`；时间戳在**行首文本**，不是 JSON 字段。
  - 所有时间统一 +8 转北京时间；重置时间从日志文本 `... UTC+8` 提取已是北京。
  - 6004 真实信号 = `statusCode:429,code:6004,category:quota`（常嵌在 refusal 的
    errorMessageMetaPreview 里）；错误文本**不含模型名**（只有"切换其他模型"提示）。
  - 因此触发模型只能靠「触发时刻之前、同文件最近一次 sendPrompt 的 modelId」判定
    （不可用文件末尾方向 last_model，会话换模型后会错）。
  - 6004 文本里的 32hex 是「每次请求相关性 ID」，不是账号 ID，不可用于账号级归因。
  - 模型名有 `-f` 后缀变体（hy4-preview / hy4-preview-f），统一归一化后匹配。

算法：
  1) 日窗(24h)优先：first_c = reset − 24h，若在 sendPrompt 中命中 ±30s 的点 p 且 p ≤ 触发
     → 定档「日窗 24h」，首请求=p，容量=窗口[p, 触发]内请求数，W 精确值=reset−p。
  2) 短窗枚举：W 从 0.5h 到 3h 步进 15min，同样命中规则（容量≥3）→ 定档「短窗」+ 精确 W。
  3) 已知画像兜底：命中 1/2 失败但模型在 KNOWN_PROFILES → 标注其画像。
  4) 其余 → 「未定」。

用法：
  python3 probe_6004_window.py                 # 全量反推
  python3 probe_6004_window.py --model hy4-preview
  python3 probe_6004_window.py --recent 7
"""
import argparse
import bisect
import glob
import os
import re
import sys
from collections import defaultdict
from datetime import datetime, timedelta

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import find_6004_events as fe  # 复用路径/正则

TS_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)")
ERR_RE = re.compile(r"使用量已超出频率限制|\\?\"code\\?\":\s*6004")
RESET_RE = fe.RESET_RE

# 已知画像参照（源自项目实测，非本脚本推断）
KNOWN_PROFILES = {
    "hy3": "短窗<3h",
    "deepseek-v4.1-flash": "日窗24h",
    "hy4-preview": "日窗24h(09-13定案)",
}


def norm_model(m: str):
    if not m:
        return m
    m = m.lower().strip()
    m = re.sub(r"-f$", "", m)
    return m


def parse_head_ts(line: str):
    m = TS_RE.match(line)
    return fe.parse_ts(m.group(1)) if m else None


def extract_json(line: str):
    i = line.find("{")
    if i < 0:
        return None
    try:
        return __import__("json").loads(line[i:])
    except Exception:
        return None


def collect_sends():
    """返回 {归一化模型: [北京 datetime 升序]} 与 {fname: [(北京 ts, 归一化模型), ...] 升序}。"""
    model_sends = defaultdict(list)
    file_sends = defaultdict(list)
    for path in sorted(glob.glob(os.path.join(fe.LOGS_ROOT, "*", "sdk", "conversations", "*.log"))):
        fname = os.path.basename(path)
        try:
            f = open(path, "r", encoding="utf-8", errors="replace")
        except OSError:
            continue
        with f:
            for line in f:
                line = line.rstrip("\n")
                if "method:sendPrompt" in line:
                    obj = extract_json(line)
                    mid = obj.get("modelId") if obj else None
                    ts = parse_head_ts(line)
                    if mid and ts:
                        nm = norm_model(mid)
                        bj = fe.to_bj(ts)
                        model_sends[nm].append(bj)
                        file_sends[fname].append((bj, nm))
    for k in model_sends:
        model_sends[k].sort()
    for k in file_sends:
        file_sends[k].sort()
    return model_sends, file_sends


def nearest_model_before(lst, trigger):
    """lst: [(ts, model)] 升序；返回 trigger 之前最近一条的 model（无则 None）。"""
    if not lst or trigger is None:
        return None
    lo, hi = 0, len(lst)
    while lo < hi:
        mid = (lo + hi) // 2
        if lst[mid][0] <= trigger:
            lo = mid + 1
        else:
            hi = mid
    return lst[lo - 1][1] if lo > 0 else None


def scan_events(model_sends, file_sends):
    events = []
    for path in sorted(glob.glob(os.path.join(fe.LOGS_ROOT, "*", "sdk", "conversations", "*.log"))):
        fname = os.path.basename(path)
        mdate = re.search(r"(\d{4}-\d{2}-\d{2})", os.path.dirname(os.path.dirname(os.path.dirname(path))))
        file_date = mdate.group(1) if mdate else "????-??-??"
        try:
            f = open(path, "r", encoding="utf-8", errors="replace")
        except OSError:
            continue
        with f:
            for line in f:
                line = line.rstrip("\n")
                if not ERR_RE.search(line):
                    continue
                ts = parse_head_ts(line)
                trigger_bj = fe.to_bj(ts) if ts else None
                obj = extract_json(line)
                obj_mid = (obj.get("modelId") if obj else None)
                mm = re.search(r"当前您在([^\s]+?)模型的使用量", line)
                msg_mid = norm_model(mm.group(1)) if mm else None
                model = norm_model(obj_mid) or msg_mid or nearest_model_before(file_sends.get(fname, []), trigger_bj) or "(unknown)"
                rm = RESET_RE.search(line)
                reset_bj = None
                if rm:
                    try:
                        reset_bj = datetime.strptime(rm.group(1), "%Y-%m-%d %H:%M:%S")
                    except ValueError:
                        reset_bj = None
                events.append({
                    "date": file_date,
                    "trigger": trigger_bj,
                    "model": model,
                    "reset": reset_bj,
                    "conv": fname,
                })
    return events


def try_window(reset, trigger, sends, W, tol_sec):
    """尝试窗口长 W：返回 {first, capacity, W_exact, err_sec} 或 None。"""
    if not (reset and trigger and sends):
        return None
    first_c = reset - W
    idx = bisect.bisect_left(sends, first_c)
    cand, cdiff = None, 1e9
    for j in range(max(0, idx - 2), min(len(sends), idx + 2)):
        d = abs((sends[j] - first_c).total_seconds())
        if d < cdiff:
            cdiff, cand = d, sends[j]
    if cand is None or cdiff > tol_sec or cand > trigger:
        return None
    lo = bisect.bisect_left(sends, cand - timedelta(seconds=1))
    hi = bisect.bisect_right(sends, trigger + timedelta(seconds=1))
    return {
        "first": cand,
        "capacity": hi - lo,
        "W_exact": reset - cand,
        "err_sec": (reset - cand - W).total_seconds(),
    }


def probe_event(ev, model_sends):
    model = ev["model"]
    reset, trigger = ev["reset"], ev["trigger"]
    sends = model_sends.get(model, [])
    # 0) 已知短窗画像直接兜底（避免误反推）
    if model in KNOWN_PROFILES and "短窗" in KNOWN_PROFILES[model]:
        return {"window": "短窗(<3h, 已知)", "model": model, "first": None,
                "capacity": None, "W_exact": None, "err_sec": None}
    # 1) 日窗 24h 优先
    r = try_window(reset, trigger, sends, timedelta(hours=24), 30)
    if r:
        r["window"] = "日窗 24h"
        r["model"] = model
        return r
    # 2) 短窗枚举 0.5h..3h（容量>=3 防单点误判）
    for minutes in range(30, 195, 15):
        r = try_window(reset, trigger, sends, timedelta(minutes=minutes), 90)
        if r and r["capacity"] >= 3:
            r["window"] = "短窗" if r["W_exact"] < timedelta(hours=12) else "日窗"
            r["model"] = model
            return r
    # 3) 已知画像兜底：日窗类若 24h 未反推成功，本事件窗口长存疑（早期/异账号策略不同），不谎报
    if model in KNOWN_PROFILES:
        return {"window": "未反推(" + KNOWN_PROFILES[model] + ")", "model": model, "first": None,
                "capacity": None, "W_exact": None, "err_sec": None}
    return {"window": "未定", "model": model, "first": None,
            "capacity": None, "W_exact": None, "err_sec": None}


def fmt_hm(td):
    if td is None:
        return "—"
    return f"{td.total_seconds() / 3600:.2f}h"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", type=str, default=None)
    ap.add_argument("--recent", type=int, default=None)
    args = ap.parse_args()

    model_sends, file_sends = collect_sends()
    events = scan_events(model_sends, file_sends)
    if args.recent:
        cutoff = datetime.now() - timedelta(days=args.recent) - timedelta(hours=8)
        events = [e for e in events if e["trigger"] and e["trigger"] >= cutoff]

    print("=" * 100)
    print("6004 滑动窗口反推 — 数据源: ~/.workbuddy/logs/*/sdk/conversations/*.log")
    print("=" * 100)
    if not events:
        print("✅ 未检测到 6004 频率限制事件。")
    else:
        print(f"共 {len(events)} 次事件；按「重置 − 窗口长 ≈ 窗口首请求」自动定档：\n")
        print(f"{'日期':<12}{'模型':<22}{'触发(北京)':<21}{'重置(北京)':<21}{'窗口档':<18}{'首请求(北京)':<21}{'容量':>5}{'W精确':>8}")
        print("-" * 140)
        for e in events:
            if args.model and args.model.lower() not in e["model"].lower():
                continue
            r = probe_event(e, model_sends)
            trig = e["trigger"].strftime("%Y-%m-%d %H:%M:%S") if e["trigger"] else "?"
            reset = e["reset"].strftime("%Y-%m-%d %H:%M:%S") if e["reset"] else "?"
            first = r["first"].strftime("%Y-%m-%d %H:%M:%S") if r["first"] else "—"
            cap = r["capacity"] if r["capacity"] is not None else "—"
            we = fmt_hm(r["W_exact"])
            print(f"{e['date']:<12}{r['model']:<22}{trig:<21}{reset:<21}{r['window']:<18}{first:<21}{str(cap):>5}{we:>8}")

    print()
    print("=" * 100)
    print("已知模型限流画像（项目实测，非本脚本推断）")
    print("=" * 100)
    for k, v in KNOWN_PROFILES.items():
        print(f"  {k:<22} → {v}")
    print()
    print("结论：6004 是硬阻断（「消耗积分继续」无效）；处置=切模型/切账号(每账号独立限额)/等重置。")
    print("      本脚本是事后复盘工具，无法命中前预测（本地无剩余次数可读字段）。")


if __name__ == "__main__":
    main()
