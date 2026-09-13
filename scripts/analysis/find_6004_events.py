#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
find_6004_events.py — WorkBuddy 频率限制(6004)事件扫描器

数据源（只读）：~/.workbuddy/logs/<日期>/sdk/conversations/*.log
- 每行 = 一次客户端事件，格式：`<UTC时间戳> method:... {JSON 载荷}`
  ⚠️ 时间戳在**行首文本**（`2026-09-06T14:47:20.929Z method:sendPrompt {...}`），
     **不是 JSON 字段** —— 解析别按 `\"timestamp\"` 正则（已踩坑）。
- `method:sendPrompt` 记录每次模型调用（modelId 在 JSON 载荷内；时间戳看行首）
- 所有时间戳为 UTC，展示/计算统一 +8 转北京时间
- `runtime.applyStopReason` 在触顶时带 `code:6004`、`statusCode:429`、
  `category:quota`、以及重置时间（如 "将在 2026-09-13 14:26:22 UTC+8 重置"）

重要结论（与记账轴区分）：
- credit_ledger 只记「成功+计费」请求，被 429 拒绝的不入帐 → 无法预判 6004。
- SDK 对话日志是「事后事件日志」，记录每次调用与每次 6004，但**不暴露剩余次数**，
  故本脚本是「检测/复盘」工具，不是「命中前预测」工具。
  → 6004 是硬阻断（模型直接不可用），唯一确定信号就是 6004 事件本身。
  → 处置：切其他模型 / 等该账号重置；多账号各自有一份每日限额可错峰。

⚠️ 关于"账号"归因：6004 错误文本里的 `32hex/36hex` 前缀经实测是**每次请求的相关性 ID**
（同一对话文件内会出现多个不同 32hex），**不是账号 ID**。因此本脚本里的 `tenant` 列
只能作事件去重参考，**不可用于账号级锚点归因**。账号→重置锚点映射只能靠 `credit_ledger`
（wb-switch 三主账号 ds-v4.1-flash 均落 14:26:22 边界）。

用法：
  python3 find_6004_events.py                 # 全量扫描，输出事件表 + 调用压力(代理)
  python3 find_6004_events.py --recent 7     # 仅近 7 天
  python3 find_6004_events.py --model deepseek-v4.1-flash
"""
import argparse
import glob
import json
import os
import re
import sys
from collections import defaultdict, Counter
from datetime import datetime, timedelta

HOME = os.environ.get("USERPROFILE") or os.environ.get("HOME")
LOGS_ROOT = os.path.join(HOME, ".workbuddy", "logs")
# 重置锚点（北京时间，每天固定 14:26:22）
WINDOW_ANCHOR = (14, 26, 22)


def parse_ts(s: str):
    # 2026-09-12T11:00:36.981Z  —— 日志时间戳是 UTC
    try:
        return datetime.strptime(s, "%Y-%m-%dT%H:%M:%S.%fZ")
    except ValueError:
        try:
            return datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
        except ValueError:
            return None


def to_bj(dt: datetime):
    """UTC(naive) -> 北京时间(+8)。日志行首时间戳一律 UTC，展示与计算必须转北京。"""
    return dt + timedelta(hours=8) if dt else dt


def window_date(dt: datetime) -> str:
    """14:26:22 固定窗口：≥锚点归当日，否则归前一日。"""
    if (dt.hour, dt.minute, dt.second) >= WINDOW_ANCHOR:
        return dt.strftime("%Y-%m-%d")
    return (dt - timedelta(days=1)).strftime("%Y-%m-%d")


RESET_RE = re.compile(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\s*UTC\+8")
# 6004 错误内常带 账号/租户哈希 + 会话ID，形如 c32055b34c9243428cced2a4d5a2afd0/ba5fe026-...
TENANT_RE = re.compile(r"([0-9a-f]{32})/[0-9a-f-]{36}")
# 6004 事件：明文「使用量已超出频率限制」最稳；也兼容 JSON 字符串内转义形式 \"code\":6004
ERR_RE = re.compile(r"使用量已超出频率限制|\\?\"code\\?\":\s*6004")
# 提取时兼容转义引号（错误对象常嵌在 errorMessageMetaPreview 字符串内）
CODE_RE = re.compile(r'\\?"code\\?":\s*(-?\d+)')
STATUS_RE = re.compile(r'\\?"statusCode\\?":\s*(\d+)')
TS_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)")


def extract_ts(line: str):
    m = TS_RE.match(line)
    return parse_ts(m.group(1)) if m else None


def extract_json(line: str):
    """日志行格式: '<ts> <method> <json>'，取首个 { 起的 JSON 片段。"""
    i = line.find("{")
    if i < 0:
        return None
    try:
        return json.loads(line[i:])
    except Exception:
        return None


def scan(recent_days=None, model_filter=None):
    logs = []
    for p in glob.glob(os.path.join(LOGS_ROOT, "*", "sdk", "conversations", "*.log")):
        logs.append(p)
    if recent_days:
        cutoff = datetime.now() - timedelta(days=recent_days)
        logs = [p for p in logs if os.path.getmtime(p) >= cutoff.timestamp()]
    logs.sort()

    events = []          # 6004 事件
    call_counter = defaultdict(Counter)  # (model, window_date) -> 调用次数(代理)
    last_model = {}      # file -> 最近一次 sendPrompt 的 modelId

    for path in logs:
        fname = os.path.basename(path)
        # 文件 mtime 推断日期（路径形如 logs/2026-09-12/sdk/...）
        mdate = re.search(r"(\d{4}-\d{2}-\d{2})", os.path.dirname(os.path.dirname(os.path.dirname(path))))
        file_date = mdate.group(1) if mdate else "????-??-??"
        try:
            f = open(path, "r", encoding="utf-8", errors="replace")
        except OSError:
            continue
        with f:
            for line in f:
                line = line.rstrip("\n")
                # 调用压力：sendPrompt
                if "method:sendPrompt" in line:
                    try:
                        obj = extract_json(line)
                        mid = obj.get("modelId") if obj else None
                        ts = extract_ts(line)
                        if mid and ts:
                            call_counter[mid][window_date(to_bj(ts))] += 1
                            last_model[fname] = mid
                    except Exception:
                        pass
                # 6004 事件
                if ERR_RE.search(line):
                    try:
                        # 行首时间
                        ts = extract_ts(line)
                        # 提取 modelId：优先本行，否则用同文件最近一次 sendPrompt
                        mid = None
                        obj = extract_json(line)
                        if obj and obj.get("modelId"):
                            mid = obj.get("modelId")
                        if not mid:
                            mid = last_model.get(fname)
                        # 重置时间
                        reset = None
                        rm = RESET_RE.search(line)
                        if rm:
                            reset = rm.group(1)
                        # code / statusCode
                        cm = CODE_RE.search(line)
                        sm = STATUS_RE.search(line)
                        # 触发消息中的模型名（如 "Deepseek-V4.1-Flash"）
                        msg_model = None
                        mm = re.search(r"当前您在([^\s]+?)模型的使用量", line)
                        if mm:
                            msg_model = mm.group(1)
                        # 账号/租户哈希
                        tenant = None
                        tm = TENANT_RE.search(line)
                        if tm:
                            tenant = tm.group(1)
                        events.append({
                            "date": file_date,
                            "ts": to_bj(ts).strftime("%Y-%m-%d %H:%M:%S") if ts else "?",
                            "model": mid or msg_model or "(unknown)",
                            "reset": reset or "(未知)",
                            "code": cm.group(1) if cm else "?",
                            "status": sm.group(1) if sm else "?",
                            "tenant": tenant or "(未知)",
                            "conv": fname,
                        })
                    except Exception:
                        pass
    return events, call_counter


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--recent", type=int, default=None)
    ap.add_argument("--model", type=str, default=None)
    args = ap.parse_args()

    events, call_counter = scan(recent_days=args.recent, model_filter=args.model)

    print("=" * 78)
    print("频率限制(6004)事件扫描器  —  数据源: ~/.workbuddy/logs/*/sdk/conversations/*.log")
    print("=" * 78)
    if not events:
        print("✅ 未检测到 6004 频率限制事件。")
    else:
        print(f"⚠️ 共检测到 {len(events)} 次 6004 频率限制事件：\n")
        print(f"{'日期':<12}{'触发时间(北京)':<22}{'模型':<22}{'重置时间(北京)':<22}")
        print("-" * 78)
        for e in events:
            if args.model and args.model.lower() not in e["model"].lower():
                continue
            print(f"{e['date']:<12}{e['ts']:<22}{e['model']:<22}{e['reset']:<22}")
        print()
        print("说明：每次 6004 = 该模型当日频率/用量限额已触顶，模型硬阻断不可用。")
        print("      处置 = 切其他模型 / 等重置时间；多账号各自一份每日限额可错峰。")
        # 按 (账号哈希, 模型) 分组，验证重置锚点是否一致
        print()
        print("-" * 78)
        print("重置锚点归因（按 请求相关性ID × 模型 分组，验证是否各自固定）")
        print("  ⚠️ 注意：下列『请求ID』是每次请求的相关性 ID，非账号 ID；同对话内会出现多个")
        print("     不同值，故本分组只能说明『同一相关性ID内重置时刻一致』，不可作账号级归因。")
        print("-" * 78)
        grp = defaultdict(set)
        for e in events:
            if args.model and args.model.lower() not in e["model"].lower():
                continue
            grp[(e["tenant"], e["model"])].add(e["reset"])
        for (tenant, model), resets in sorted(grp.items()):
            resets_str = " / ".join(sorted(resets))
            flag = "一致✅" if len(resets) == 1 else f"多变({len(resets)})⚠️"
            print(f"请求ID {tenant[:8]}…  模型 {model:<22} 重置锚点: {resets_str}  [{flag}]")
        print()
        print("→ 账号→重置锚点映射只能靠 credit_ledger（三主账号 ds-v4.1-flash 均落 14:26:22）。")
        print("  6004 日志本身不含账号 ID，无法可靠做账号级锚点归因。")

    print()
    print("=" * 78)
    print("调用压力(代理指标) — 每模型每 14:26:22 窗口 sendPrompt 次数")
    print("  ⚠️ 这是『调用次数』代理，不是官方限额；6004 真实阈值/单位未知(可能为QPS/每日免费次数)")
    print("=" * 78)
    if not call_counter:
        print("（无 sendPrompt 记录）")
    else:
        rows = []
        for mid, wins in call_counter.items():
            if args.model and args.model.lower() not in mid.lower():
                continue
            for w, c in wins.items():
                rows.append((mid, w, c))
        rows.sort(key=lambda r: (r[1], r[0]))
        print(f"{'窗口(日)':<14}{'模型':<28}{'调用次数':>10}")
        print("-" * 56)
        for mid, w, c in rows:
            print(f"{w:<14}{mid:<28}{c:>10}")

    print()
    print("结论：本地无『剩余次数/今日调用次数』可读字段，无法在命中前预判 6004。")
    print("      唯一确定信号 = 6004 事件本身（已收录）。监控请检测事件 + 错峰切号。")


if __name__ == "__main__":
    main()
