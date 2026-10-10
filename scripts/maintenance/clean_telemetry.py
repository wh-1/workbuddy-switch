# -*- coding: utf-8 -*-
"""Single entry point for WorkBuddy periodic cleanup (traces + logs).

Runs the focused cleaners in order and prints a combined report:
  1. clean_traces.py - trace telemetry files (14d by mtime, os.remove)
  2. clean_logs.py   - logs/ dated dirs (dated 14d / sandbox 5d, rmtree)

Both cleaners default to dry-run and require --apply; this orchestrator
follows the same convention and forwards --apply to each step.

Usage:
  python clean_telemetry.py            # dry-run both, delete nothing
  python clean_telemetry.py --apply    # actually delete
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
APPLY = "--apply" in sys.argv

STEPS = [
    ("traces", "clean_traces.py"),
    ("logs", "clean_logs.py"),
    ("appdata", "clean_appdata.py"),
]


def run_step(script):
    cmd = [sys.executable, os.path.join(HERE, script)]
    if APPLY:
        cmd.append("--apply")
    print(f"\n===== {script} =====")
    r = subprocess.run(cmd, capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    if r.stdout and r.stdout.strip():
        print(r.stdout.rstrip())
    if r.stderr and r.stderr.strip():
        print("STDERR:", r.stderr.strip())
    return r.returncode


def main():
    print(f"mode: {'APPLY' if APPLY else 'DRY-RUN'}")
    codes = {}
    for label, script in STEPS:
        codes[label] = run_step(script)

    print("\n===== summary =====")
    for label, _script in STEPS:
        c = codes[label]
        print(f"  {label:8s}: exit={c} {'OK' if c == 0 else 'FAIL'}")
    return 0 if all(c == 0 for c in codes.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
