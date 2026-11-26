#!/usr/bin/env python3
"""CLI entry point: python composition/data-import/import_all.py ..."""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

from wximport.runner import RunnerConfig, run_import


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="resumable SQLite -> PostgreSQL row import")
    parser.add_argument("--sqlite", required=True, type=Path)
    parser.add_argument("--dsn", default=os.environ.get("WXLIKE_PG_URL", ""))
    parser.add_argument("--work-dir", type=Path, help="CSV/state directory (required for --resume)")
    parser.add_argument("--run-id")
    parser.add_argument("--mapping", type=Path)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--plan-only", action="store_true", help="write the plan and stop")
    parser.add_argument("--plan-out", type=Path)
    parser.add_argument(
        "--allow-concurrent-writes",
        action="store_true",
        help="do not fail when the source file/data_version moves during the window",
    )
    parser.add_argument("--max-window-seconds", type=int)
    args = parser.parse_args(argv)

    if not args.plan_only and not args.dsn:
        parser.error("--dsn or WXLIKE_PG_URL is required")
    if args.resume and not args.work_dir:
        parser.error("--resume needs the original --work-dir")
    work_dir = args.work_dir or Path(tempfile.mkdtemp(prefix="wxlike-data-import-"))
    config = RunnerConfig(
        sqlite=args.sqlite,
        dsn=args.dsn,
        work_dir=work_dir,
        run_id=args.run_id,
        mapping=args.mapping,
        resume=args.resume,
        verify_only=args.verify_only,
        plan_only=args.plan_only,
        require_quiescence=not args.allow_concurrent_writes,
        max_window_seconds=args.max_window_seconds,
        plan_out=args.plan_out,
    )
    report = run_import(config)
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["status"] in {"planned", "verified"} else 1


if __name__ == "__main__":
    sys.exit(main())
