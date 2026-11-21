#!/usr/bin/env python3
"""Run the pinned Board protocol oracle against a disposable Engage board."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PIN = "fef19e5bb1e17551f119be11f7af8ed3387892bb"
GROUPS = {"core", "paging", "search", "schedules", "tasks", "grants", "schemas", "attachments", "watchers", "sync"}


def run(args, cwd=ROOT):
    subprocess.run(args, cwd=cwd, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="Disposable board; conformance creates messages, tasks, schemas and grants")
    parser.add_argument("--report", type=Path, default=ROOT / ".cache/verification/conformance.json")
    parser.add_argument("--prepare", action="store_true", help="Fetch and build the pinned upstream development-only oracle")
    args = parser.parse_args()
    upstream = ROOT / ".cache/board"
    if args.prepare:
        if not (upstream / ".git").exists():
            upstream.parent.mkdir(parents=True, exist_ok=True)
            run(["git", "clone", "https://github.com/vercel/board.git", str(upstream)])
        run(["git", "fetch", "origin", PIN], upstream)
        run(["git", "checkout", "--detach", PIN], upstream)
        run(["pnpm", "install", "--frozen-lockfile"], upstream)
        run(["pnpm", "--filter", "@vercel/board", "build"], upstream)
    actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=upstream, text=True).strip()
    if actual != PIN:
        sys.exit(f"Oracle revision must be {PIN}; got {actual}")
    env = dict(os.environ, BOARD_URL=args.url)
    # Clear a selected account/team from unrelated developer configuration.
    env.pop("BOARD_NAME", None)
    result = subprocess.run(["node", str(upstream / "packages/board/dist/cli.mjs"), "conformance", "--json"], cwd=upstream, env=env, capture_output=True, text=True)
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(result.stderr + result.stdout)
        sys.exit("The oracle did not produce a JSON conformance report")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    checks = report.get("checks", [])
    failed = [check for check in checks if check["status"] == "failed"]
    unsupported = [check for check in checks if check["status"] == "skipped" and "not kept here" in check.get("detail", "")]
    missing = GROUPS - {check["group"] for check in checks if check["status"] == "passed"}
    for check in failed + unsupported:
        print(f"{check['status']}: {check['group']}: {check['name']}: {check.get('detail', '')}")
    counts = {status: sum(c["status"] == status for c in checks) for status in ["passed", "failed", "skipped"]}
    print(json.dumps({"upstream": PIN, "counts": counts, "missing_groups": sorted(missing), "report": str(args.report)}))
    return int(result.returncode != 0 or bool(failed) or bool(unsupported) or bool(missing))


if __name__ == "__main__":
    sys.exit(main())
