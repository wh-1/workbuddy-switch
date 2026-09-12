#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""账号时间轴归因（共享模块，供 session_cost.py / credit_diagnose.py 复用）。

背景：切号对齐（L3）会把 sessions.user_id 改写成当前账号，**库里字段无法还原历史归属**。
唯一真相源是官方 auth 目录的凭据快照：
    %LOCALAPPDATA%\\CodeBuddyExtension\\Data\\Public\\auth\\workbuddy-desktop.<ISO>.<pid>.<uuid>.info
文件名含 ISO 时间戳、内容含 account.uid；每次快照 = 一个「当时登录的是谁」事件。

给定任意时刻，取「不晚于该时刻的最后一个事件」即为当时的账号（二分查找）。

精度约定：
  - Token 按**记录时刻**归因 → 可精确到每次调用（跨账号会话也能拆）
  - 积分按**会话起始时刻**归因 → session_usage 一行=一个会话、无逐次时间戳，
    跨账号会话的积分只能整笔归给起始账号
  - 早于首个快照的记录归因不到（返回 None）
"""
from __future__ import annotations

import datetime as dt
import glob
import json
import os
import re

HOME = os.path.expanduser("~")
AUTH_DIR = os.path.join(
    HOME, "AppData", "Local", "CodeBuddyExtension", "Data", "Public", "auth"
)
PROJECTS_DIR = os.path.join(HOME, ".workbuddy", "projects")
_AUTH_PAT = re.compile(
    r"workbuddy-desktop\.(\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z)\."
)


def load_account_timeline():
    """从官方 auth 目录历史快照重建账号切换时间线。

    返回 (events, names)
      events = [(ts_ms, uid), ...]  按时间升序
      names  = {uid: nickname}
    """
    events: list[tuple[int, str]] = []
    names: dict[str, str] = {}
    if not os.path.isdir(AUTH_DIR):
        return events, names
    for name in os.listdir(AUTH_DIR):
        m = _AUTH_PAT.match(name)
        if not m:
            continue
        try:
            # 2026-08-08T01-00-59-843Z -> 2026-08-08T01:00:59.843Z
            parsed = dt.datetime.strptime(m.group(1), "%Y-%m-%dT%H-%M-%S-%fZ")
        except ValueError:
            continue
        try:
            with open(os.path.join(AUTH_DIR, name), encoding="utf-8") as fh:
                data = json.load(fh)
        except (OSError, ValueError):
            continue
        acct = data.get("account") or {}
        uid = acct.get("uid")
        if not uid:
            continue
        names[uid] = acct.get("nickname") or uid[:8]
        events.append((int(parsed.replace(tzinfo=dt.timezone.utc).timestamp() * 1000), uid))
    events.sort()
    return events, names


def account_at(events, ts_ms):
    """给定毫秒时间戳，返回当时的 uid（二分查找最后一个 <= ts 的事件）。"""
    if not events or not ts_ms:
        return None
    lo, hi, best = 0, len(events) - 1, None
    while lo <= hi:
        mid = (lo + hi) // 2
        if events[mid][0] <= ts_ms:
            best = events[mid][1]
            lo = mid + 1
        else:
            hi = mid - 1
    return best


def label(uid, names) -> str:
    """账号展示名（未归因显示「未归因」）。"""
    if uid is None:
        return "未归因"
    return names.get(uid) or uid[:8]


def _usage(v):
    """按项目口径取 usage：message.usage > providerData.usage > 顶层 usage。"""
    for path in (("message", "usage"), ("providerData", "usage"), ("usage",)):
        cur = v
        for key in path:
            cur = cur.get(key) if isinstance(cur, dict) else None
            if cur is None:
                break
        if isinstance(cur, dict) and any(k in cur for k in ("input", "input_tokens")):
            return cur
    return None


def _num(obj, *keys) -> int:
    for key in keys:
        val = obj.get(key)
        if isinstance(val, (int, float)):
            return int(val)
    return 0


def scan_records(cutoff_ms: int | None = None):
    """扫描 JSONL 记录（排除 subagents/），产出 (session_id, ts_ms, tokens)。

    tokens = input + output（与项目 total 口径一致，仅用于**账号归因的比例**，
    绝对总量仍以 Rust 侧 dump_stats 为准）。
    """
    for path in glob.glob(os.path.join(PROJECTS_DIR, "**", "*.jsonl"), recursive=True):
        if "subagents" in path.replace("\\", "/"):
            continue
        sid = os.path.splitext(os.path.basename(path))[0]
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
                usage = _usage(v)
                if not usage:
                    continue
                ts = v.get("timestamp") or (v.get("message") or {}).get("timestamp")
                if not ts or (cutoff_ms and ts < cutoff_ms):
                    continue
                tokens = _num(usage, "input", "input_tokens") + _num(
                    usage, "output", "output_tokens"
                )
                yield sid, int(ts), tokens
