# -*- coding: utf-8 -*-
"""Clean WorkBuddy trace telemetry: delete trace_*.json older than 14 days, then empty pid dirs."""
import os, sys, time, traceback

TRACES = r"C:\Users\WH\.workbuddy\traces"
RETENTION_DAYS = 14
SIZE_WARN_BYTES = int(2.5 * 1024**3)
LOG_PATH = r"C:\Users\WH\.workbuddy\logs\trace-cleanup.log"
LOG_MAX_BYTES = 512 * 1024
LOG_KEEP_LINES = 500
cutoff = time.time() - RETENTION_DAYS * 86400

def scan():
    files = []
    for root, dirs, names in os.walk(TRACES):
        for n in names:
            p = os.path.join(root, n)
            try:
                st = os.stat(p)
                files.append((p, st.st_size, st.st_mtime))
            except OSError:
                pass
    return files

def total(files):
    return sum(s for _, s, _ in files)

if not os.path.isdir(TRACES):
    print("SKIP: traces dir not found:", TRACES)
    sys.exit(0)

before = scan()
victims = [f for f in before if os.path.basename(f[0]).startswith("trace_") and f[0].endswith(".json") and f[2] < cutoff]

if "--apply" not in sys.argv:
    print("mode: DRY-RUN")
    print(f"candidates: {len(victims)} expired trace files, {total(victims)/1024**3:.2f} GB (pass --apply to execute)")
    for _p, _s, _m in victims[:30]:
        print(f"  {_s/1024**2:9.1f} MB  {_p}")
    sys.exit(0)

deleted, failed = 0, []
for path, size, mtime in victims:
    try:
        os.remove(path)
        deleted += 1
    except Exception as e:
        failed.append((path, repr(e)))

# remove now-empty subdirectories (pid dirs), deepest first
removed_dirs = 0
for root, dirs, names in os.walk(TRACES, topdown=False):
    if os.path.abspath(root) == os.path.abspath(TRACES):
        continue
    try:
        if not os.listdir(root):
            os.rmdir(root)
            removed_dirs += 1
    except OSError:
        pass

after = scan()
n_after, size_after = len(after), total(after)
n_before, size_before = len(before), total(before)

print("=== trace cleanup report ===")
print(f"before: {n_before} files, {size_before/1024**3:.2f} GB")
print(f"expired(>{RETENTION_DAYS}d): {len(victims)} files, {total(victims)/1024**3:.2f} GB")
print(f"deleted: {deleted}, failed: {len(failed)}")
for p, e in failed[:20]:
    print("  FAIL:", p, e)
print(f"empty dirs removed: {removed_dirs}")
print(f"after: {n_after} files, {size_after/1024**3:.2f} GB")
if size_after > SIZE_WARN_BYTES:
    print("WARN: remaining size exceeds 2.5 GB - growth may be accelerating, consider narrowing retention window")
# verify: no expired files remain
leftover = [f for f in after if os.path.basename(f[0]).startswith("trace_") and f[0].endswith(".json") and f[2] < cutoff]
print(f"expired remaining after cleanup: {len(leftover)}")

# append one-line result to a persistent, self-limiting log for later audit
try:
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    line = (f"{stamp} before={n_before}f/{size_before/1024**3:.2f}GB "
            f"deleted={deleted} failed={len(failed)} expired_left={len(leftover)} "
            f"after={n_after}f/{size_after/1024**3:.2f}GB")
    os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
    if os.path.exists(LOG_PATH) and os.path.getsize(LOG_PATH) > LOG_MAX_BYTES:
        with open(LOG_PATH, "r", encoding="utf-8", errors="replace") as fh:
            tail = fh.readlines()[-LOG_KEEP_LINES:]
        with open(LOG_PATH, "w", encoding="utf-8") as fh:
            fh.writelines(tail)
    with open(LOG_PATH, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")
except Exception as e:
    print("LOG FAIL:", repr(e))
