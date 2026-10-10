# -*- coding: utf-8 -*-
"""Clean WorkBuddy logs: remove dated log directories older than retention.

Scope is intentionally narrow - only two kinds of directories under
C:\\Users\\WH\\.workbuddy\\logs are ever touched:
  1. <LOGS>\\<YYYY-MM-DD>\\          retention 14 days  (per-project / main-thread / sdk logs)
  2. <LOGS>\\sandbox\\<YYYYMMDD>\\   retention 5 days   (sandbox process logs, fastest growing)

Everything else is NEVER touched: root *.log files (daemon/main/renderer/...),
and the non-dated dirs database/ migration/ startup/ network/ perf/ sites/
update/ Crash-Log/ Diagnostics/ editor_sdk/ mcp-runtime/ weixinpay/ sandbox/dumps/.

Date is parsed from the directory NAME (not mtime) so the rule is deterministic.
Default is dry-run; pass --apply to actually delete.
"""
import os
import re
import sys
import time
import shutil

LOGS = r"C:\Users\WH\.workbuddy\logs"
SANDBOX = os.path.join(LOGS, "sandbox")
DATE_RETENTION_DAYS = 14
SANDBOX_RETENTION_DAYS = 5
SIZE_WARN_BYTES = int(4.0 * 1024**3)
LOG_PATH = os.path.join(LOGS, "cleanup-report.log")
LOG_MAX_BYTES = 512 * 1024
LOG_KEEP_LINES = 500

DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
SB_RE = re.compile(r"^\d{8}$")

APPLY = "--apply" in sys.argv


def dir_stats(path):
    total = 0
    count = 0
    for dp, _dn, fn in os.walk(path):
        for f in fn:
            try:
                total += os.path.getsize(os.path.join(dp, f))
                count += 1
            except OSError:
                pass
    return total, count


def day00(ts=None):
    return time.mktime(time.strptime(time.strftime("%Y-%m-%d", time.localtime(ts)), "%Y-%m-%d"))


def parse(name, fmt):
    try:
        return time.mktime(time.strptime(name, fmt))
    except ValueError:
        return None


def guarded(path):
    """Hard safety net: only dated dirs strictly inside LOGS."""
    ap = os.path.abspath(path)
    base = os.path.abspath(LOGS)
    if not ap.startswith(base + os.sep):
        return False
    return bool(DATE_RE.match(os.path.basename(ap)) or SB_RE.match(os.path.basename(ap)))


def collect():
    victims = []
    cutoff_date = day00() - (DATE_RETENTION_DAYS - 1) * 86400
    cutoff_sb = day00() - (SANDBOX_RETENTION_DAYS - 1) * 86400
    if os.path.isdir(LOGS):
        for n in sorted(os.listdir(LOGS)):
            p = os.path.join(LOGS, n)
            if os.path.isdir(p) and DATE_RE.match(n):
                t = parse(n, "%Y-%m-%d")
                if t is not None and t < cutoff_date and guarded(p):
                    size, cnt = dir_stats(p)
                    victims.append((p, "dated", size, cnt))
    if os.path.isdir(SANDBOX):
        for n in sorted(os.listdir(SANDBOX)):
            p = os.path.join(SANDBOX, n)
            if os.path.isdir(p) and SB_RE.match(n):
                t = parse(n, "%Y%m%d")
                if t is not None and t < cutoff_sb and guarded(p):
                    size, cnt = dir_stats(p)
                    victims.append((p, "sandbox", size, cnt))
    return victims


def write_log(line):
    try:
        os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
        if os.path.exists(LOG_PATH) and os.path.getsize(LOG_PATH) > LOG_MAX_BYTES:
            with open(LOG_PATH, "r", encoding="utf-8", errors="replace") as fh:
                tail = fh.readlines()[-LOG_KEEP_LINES:]
            with open(LOG_PATH, "w", encoding="utf-8") as fh:
                fh.writelines(tail)
        with open(LOG_PATH, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")
    except Exception as e:  # noqa: BLE001
        print("LOG FAIL:", repr(e))


def main():
    if not os.path.isdir(LOGS):
        print("SKIP: logs dir not found:", LOGS)
        return 0

    victims = collect()
    total_size = sum(v[2] for v in victims)
    total_files = sum(v[3] for v in victims)

    print(f"mode: {'APPLY' if APPLY else 'DRY-RUN'}")
    print(f"logs retention: dated={DATE_RETENTION_DAYS}d  sandbox={SANDBOX_RETENTION_DAYS}d")
    print(f"candidates: {len(victims)} dirs, {total_files} files, {total_size/1024**3:.2f} GB")
    for p, kind, size, cnt in victims:
        print(f"  [{kind:7s}] {size/1024**2:9.1f} MB  {cnt:5d}f  {p}")

    if not APPLY:
        print("dry-run only, nothing deleted (pass --apply to execute)")
        return 0

    deleted = 0
    freed = 0
    failed = []
    for p, kind, size, cnt in victims:
        if not guarded(p):
            failed.append((p, "guard rejected"))
            continue
        try:
            shutil.rmtree(p)
            deleted += 1
            freed += size
        except Exception as e:  # noqa: BLE001
            failed.append((p, repr(e)))

    # verify by re-collect
    leftover = collect()
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    print("=== log cleanup report ===")
    print(f"deleted: {deleted} dirs, freed {freed/1024**3:.2f} GB, failed: {len(failed)}")
    for p, e in failed[:20]:
        print("  FAIL:", p, e)
    print(f"expired remaining: {len(leftover)} dirs")

    remaining = 0
    for dp, _dn, fn in os.walk(LOGS):
        for f in fn:
            try:
                remaining += os.path.getsize(os.path.join(dp, f))
            except OSError:
                pass
    print(f"logs total after: {remaining/1024**3:.2f} GB")
    if remaining > SIZE_WARN_BYTES:
        print("WARN: logs total exceeds 4.0 GB - growth may be accelerating")

    write_log(f"{stamp} deleted={deleted}dirs freed={freed/1024**3:.2f}GB "
              f"failed={len(failed)} left={remaining/1024**3:.2f}GB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
