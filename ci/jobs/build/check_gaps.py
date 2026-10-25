#!/usr/bin/env python3
"""Fail closed on known delivery gaps at release time.

Day-to-day CI keeps moving while upstream cards are unfinished. A release must
not: any gap with ``blocks_release: true`` makes ``--release`` exit non-zero.
The same file is printed into the PR job summary so the gap is never invisible.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DEFAULT = Path(__file__).resolve().parent.parent / "policy" / "known-gaps.json"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file", type=Path, default=DEFAULT)
    parser.add_argument("--release", action="store_true", help="fail on open blocking gaps")
    parser.add_argument("--json", type=Path, help="write the machine-readable gap report")
    args = parser.parse_args(argv)

    data = json.loads(args.file.read_text(encoding="utf-8"))
    gaps = data.get("gaps", [])
    open_blocking = [g for g in gaps if g.get("status") == "open" and g.get("blocks_release")]
    if args.json:
        report = {
            "updated_at": data.get("updated_at"),
            "total": len(gaps),
            "open_blocking": open_blocking,
            "gaps": gaps,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")

    print(f"known gaps: {len(gaps)} total, {len(open_blocking)} open and release-blocking")
    for gap in gaps:
        mark = "BLOCK" if gap in open_blocking else "open " if gap.get("status") == "open" else "closed"
        print(f"  [{mark}] {gap['id']} owner={gap.get('owner')} task={gap.get('task')}")
        print(f"          {gap.get('evidence', '')}")

    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write("### Known gaps\n\n")
            for gap in gaps:
                state = "OPEN (blocks release)" if gap in open_blocking else gap.get("status", "?")
                fh.write(f"- **{gap['id']}** ({gap.get('owner')}/{gap.get('task')}): {state} — {gap.get('evidence', '')}\n")
            fh.write("\n")

    if args.release and open_blocking:
        print("\nrelease blocked: close or explicitly re-scope every blocking gap", file=os.sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
