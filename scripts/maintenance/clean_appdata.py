# -*- coding: utf-8 -*-
"""Clean WorkBuddy app-data caches / history / backups under C:\\Users\\WH\\.workbuddy.

Strictly whitelist-based. Only the paths listed in TARGETS / ROOT_ONE_OFF /
SESSION_BACKUP_REL are ever touched. Everything else - workbuddy.db(+wal/shm),
edge-sync-mapping-v4.db, projects/, skills/, settings.json, device-id,
SOUL/USER/MEMORY/IDENTITY, binaries/, app/, logs/, traces/ - is NEVER touched.
plugins/ is left alone too, EXCEPT superseded plugin versions under
plugins/cache/<mkt>/<plugin>/<ver> that installed_plugins.json no longer points at
(if that manifest cannot be read, plugin cleanup is skipped entirely).

Tiers:
  A  pure derivatives (by mtime): file-tree-manifests 7d, cache 7d,
     clipboard-images 7d, shell-snapshots 3d
  B  history (by mtime): file-history 30d, changes-detail 30d
  P  plugin cache: superseded versions absent from installed_plugins.json, 7d
  R  one-off migration backups known by name (no time window)
  C  workspace/sessions/*/modify_backup  -> REPORT-ONLY unless
     --include-session-backups is passed (it is rollback material for live sessions)

Default is dry-run; pass --apply to actually delete.
"""
import glob
import json
import os
import shutil
import sys
import time

ROOT = r"C:\Users\WH\.workbuddy"
LOG_PATH = os.path.join(ROOT, "logs", "appdata-cleanup.log")
LOG_MAX_BYTES = 512 * 1024
LOG_KEEP_LINES = 500

APPLY = "--apply" in sys.argv
INCLUDE_SESSION = "--include-session-backups" in sys.argv

# (subpath relative to ROOT, name suffix filter or None, retention days, tier)
TARGETS = [
    ("file-tree-manifests", ".json", 7, "A"),
    ("cache", None, 7, "A"),
    ("clipboard-images", ".png", 7, "A"),
    ("shell-snapshots", ".sh", 3, "A"),
    ("file-history", None, 30, "B"),
    ("changes-detail", None, 30, "B"),
]

# one-off leftovers matched by glob under ROOT (no time window)
ROOT_ONE_OFF = ["workbuddy.db.bak-migrate-*"]

SESSION_BACKUP_REL = os.path.join("workspace", "sessions")
SESSION_BACKUP_DIRNAME = "modify_backup"
SESSION_BACKUP_DAYS = 30

# plugins/cache: superseded plugin versions (never the ones installed_plugins.json points at)
PLUGINS_DIR = os.path.join(ROOT, "plugins")
PLUGIN_CACHE = os.path.join(PLUGINS_DIR, "cache")
PLUGIN_INSTALLED = os.path.join(PLUGINS_DIR, "installed_plugins.json")
PLUGIN_STALE_DAYS = 7

# hard blacklist (belt and braces on top of the whitelist)
FORBIDDEN_REL = {
    "workbuddy.db", "workbuddy.db-shm", "workbuddy.db-wal",
    "edge-sync-mapping-v4.db", "settings.json", "device-id",
    "MEMORY.md", "SOUL.md", "USER.md", "IDENTITY.md",
}


def guarded(path):
    ap = os.path.abspath(path)
    base = os.path.abspath(ROOT)
    if not ap.startswith(base + os.sep):
        return False
    rel = os.path.relpath(ap, base)
    if rel in FORBIDDEN_REL:
        return False
    top = rel.split(os.sep)[0]
    if top in ("projects", "skills", "logs", "traces", "binaries", "app"):
        return False
    if top == "plugins":
        # only the plugin version cache (superseded versions) is allowed
        return rel.replace(os.sep, "/").startswith("plugins/cache/")
    return True


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


def plugin_active_paths():
    """Active installPaths from installed_plugins.json, or None if unreadable."""
    try:
        with open(PLUGIN_INSTALLED, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except Exception:  # noqa: BLE001
        return None
    active = set()
    for _key, entries in (data.get("plugins") or {}).items():
        if isinstance(entries, list):
            for entry in entries:
                if isinstance(entry, dict) and entry.get("installPath"):
                    active.add(os.path.normcase(os.path.abspath(entry["installPath"])))
    return active


def collect():
    now = time.time()
    victims = []
    report_only = []  # session backups, counted but not deleted by default

    for rel, suffix, days, tier in TARGETS:
        base = os.path.join(ROOT, rel)
        if not os.path.isdir(base):
            continue
        cutoff = now - days * 86400
        for dp, _dn, fn in os.walk(base):
            for f in fn:
                if suffix and not f.endswith(suffix):
                    continue
                fp = os.path.join(dp, f)
                if not guarded(fp):
                    continue
                try:
                    st = os.stat(fp)
                except OSError:
                    continue
                if st.st_mtime < cutoff:
                    victims.append((fp, tier, st.st_size))

    for pattern in ROOT_ONE_OFF:
        for fp in glob.glob(os.path.join(ROOT, pattern)):
            if os.path.isfile(fp) and guarded(fp):
                victims.append((fp, "R", os.path.getsize(fp)))

    base = os.path.join(ROOT, SESSION_BACKUP_REL)
    if os.path.isdir(base):
        cutoff = now - SESSION_BACKUP_DAYS * 86400
        for dp, _dn, fn in os.walk(base):
            if os.path.basename(os.path.abspath(dp)) != SESSION_BACKUP_DIRNAME:
                continue
            for f in fn:
                fp = os.path.join(dp, f)
                if not guarded(fp):
                    continue
                try:
                    st = os.stat(fp)
                except OSError:
                    continue
                if st.st_mtime < cutoff:
                    (victims if INCLUDE_SESSION else report_only).append((fp, "C", st.st_size))

    # P: superseded plugin versions under plugins/cache (skip if manifest unreadable)
    active = plugin_active_paths()
    if active is not None and os.path.isdir(PLUGIN_CACHE):
        cutoff_p = now - PLUGIN_STALE_DAYS * 86400
        for mkt in sorted(os.listdir(PLUGIN_CACHE)):
            mkp = os.path.join(PLUGIN_CACHE, mkt)
            if not os.path.isdir(mkp):
                continue
            for pl in sorted(os.listdir(mkp)):
                plp = os.path.join(mkp, pl)
                if not os.path.isdir(plp):
                    continue
                for ver in sorted(os.listdir(plp)):
                    vp = os.path.join(plp, ver)
                    if not os.path.isdir(vp) or not guarded(vp):
                        continue
                    if os.path.normcase(os.path.abspath(vp)) in active:
                        continue
                    try:
                        mt = os.path.getmtime(vp)
                    except OSError:
                        continue
                    if mt < cutoff_p:
                        size, _cnt = dir_stats(vp)
                        victims.append((vp, "P", size))

    return victims, report_only


def prune_empty_dirs():
    removed = 0
    for rel, _suffix, _days, _tier in TARGETS:
        base = os.path.join(ROOT, rel)
        if not os.path.isdir(base):
            continue
        for dp, _dn, _fn in os.walk(base, topdown=False):
            if os.path.abspath(dp) == os.path.abspath(base):
                continue
            try:
                if not os.listdir(dp):
                    os.rmdir(dp)
                    removed += 1
            except OSError:
                pass
    # prune empty dirs left under plugins/cache (deepest first)
    if os.path.isdir(PLUGIN_CACHE):
        for dp, _dn, _fn in os.walk(PLUGIN_CACHE, topdown=False):
            if os.path.abspath(dp) == os.path.abspath(PLUGIN_CACHE):
                continue
            try:
                if not os.listdir(dp):
                    os.rmdir(dp)
                    removed += 1
            except OSError:
                pass
    return removed


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
    if not os.path.isdir(ROOT):
        print("SKIP: root not found:", ROOT)
        return 0

    victims, report_only = collect()
    size = sum(v[2] for v in victims)
    ro_size = sum(v[2] for v in report_only)

    print(f"mode: {'APPLY' if APPLY else 'DRY-RUN'}"
          f"{' (+session-backups)' if INCLUDE_SESSION else ''}")
    by_tier = {}
    for _p, tier, s in victims:
        c, tot = by_tier.get(tier, (0, 0))
        by_tier[tier] = (c + 1, tot + s)
    for tier in sorted(by_tier):
        c, tot = by_tier[tier]
        print(f"  tier {tier}: {c} files, {tot/1024**3:.2f} GB")
    print(f"candidates: {len(victims)} files, {size/1024**3:.2f} GB")
    for p, tier, s in sorted(victims, key=lambda x: -x[2])[:15]:
        print(f"  [{tier}] {s/1024**2:9.2f} MB  {p}")

    if report_only:
        print(f"\nREPORT-ONLY (session modify_backup, >{SESSION_BACKUP_DAYS}d): "
              f"{len(report_only)} files, {ro_size/1024**3:.2f} GB "
              f"- pass --include-session-backups to delete")
        top = {}
        for p, _t, s in report_only:
            sid = p.split(os.sep)[-3]
            top[sid] = top.get(sid, 0) + s
        for sid, s in sorted(top.items(), key=lambda x: -x[1])[:5]:
            print(f"     session {sid}: {s/1024**3:.2f} GB")

    if not APPLY:
        print("dry-run only, nothing deleted (pass --apply to execute)")
        return 0

    deleted = 0
    freed = 0
    failed = []
    for p, _tier, s in victims:
        if not guarded(p):
            failed.append((p, "guard rejected"))
            continue
        try:
            if os.path.isdir(p):
                shutil.rmtree(p)
            else:
                os.remove(p)
            deleted += 1
            freed += s
        except Exception as e:  # noqa: BLE001
            failed.append((p, repr(e)))
    dirs = prune_empty_dirs()

    print("=== appdata cleanup report ===")
    print(f"deleted: {deleted} files, freed {freed/1024**3:.2f} GB, failed: {len(failed)}")
    for p, e in failed[:20]:
        print("  FAIL:", p, e)
    print(f"empty dirs removed: {dirs}")

    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    write_log(f"{stamp} deleted={deleted}f freed={freed/1024**3:.2f}GB "
              f"failed={len(failed)} dirs={dirs} report_only={len(report_only)}f")
    return 0


if __name__ == "__main__":
    sys.exit(main())
