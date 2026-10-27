#!/usr/bin/env python3
"""Fail on secrets that leaked into collected logs.

    python deploy/production/check-log-redaction.py /var/log/wxlike/*.log
    python deploy/production/check-log-redaction.py --self-test

Placeholders (``<redacted>``, ``***``, ``REDACTED``, empty) are accepted so a
correctly redacting logger does not trip the check.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from pathlib import Path

PLACEHOLDER = re.compile(r"^(?:<redacted>|\*{3,}|\[?redacted\]?|none|null|-)?$", re.IGNORECASE)
PATTERNS = [
    ("bearer", re.compile(r"authorization\s*[:=]\s*bearer\s+(\S+)", re.IGNORECASE)),
    ("admin-token", re.compile(r"x-admin-token\s*[:=]\s*(\S+)", re.IGNORECASE)),
    ("password", re.compile(r"\b(?:password|passwd|keystore_password)\s*[:=]\s*(\S+)", re.IGNORECASE)),
    ("private-key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
    ("secret-assignment", re.compile(r"\b(?:secret|token)\s*[:=]\s*(\S{8,})", re.IGNORECASE)),
]
SECRET_ENV_NAMES = (
    "WXLIKE_LIVE_INGEST_SECRET",
    "WXLIKE_MEDIA_SECRET",
    "WXLIKE_TURN_SECRET",
    "WXLIKE_ADMIN_TOKEN",
    "WXLIKE_VIDEO_CALLBACK_TOKEN",
)


def load_env_values(env_file: Path | None) -> list[tuple[str, str]]:
    values = []
    if env_file and env_file.is_file():
        for line in env_file.read_text(encoding="utf-8").splitlines():
            name, sep, value = line.strip().partition("=")
            if sep and name in SECRET_ENV_NAMES and len(value) >= 8 and not value.startswith("CHANGE_ME"):
                values.append((name, value))
    for name in SECRET_ENV_NAMES:
        value = os.environ.get(name, "")
        if len(value) >= 8:
            values.append((name, value))
    return values


def scan(path: Path, env_values: list[tuple[str, str]]) -> list[str]:
    problems: list[str] = []
    for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        for label, pattern in PATTERNS:
            for match in pattern.finditer(line):
                captured = match.group(1) if match.lastindex else ""
                if captured and PLACEHOLDER.match(captured.strip().strip("\"',")):
                    continue
                problems.append(f"{path}:{number}: {label} leaked (value hidden)")
                break
        for name, value in env_values:
            if value in line:
                problems.append(f"{path}:{number}: value of {name} leaked (value hidden)")
    return problems


def self_test() -> int:
    good = [
        'INFO request handled status=200',
        'DEBUG authorization: Bearer <redacted>',
        'INFO token=*** password=[REDACTED]',
    ]
    bad = [
        'DEBUG authorization: Bearer eyJhbGciOiJIUzI1NiJ9.secret',
        'WARN x-admin-token: super-secret-admin',
        'INFO password=hunter2hunter2',
        'INFO WXLIKE_TURN_SECRET=turn-secret-value',
    ]
    with tempfile.TemporaryDirectory() as tmp:
        good_path = Path(tmp, "good.log")
        good_path.write_text("\n".join(good) + "\n", encoding="utf-8")
        bad_path = Path(tmp, "bad.log")
        bad_path.write_text("\n".join(bad) + "\n", encoding="utf-8")
        env_values = [("WXLIKE_TURN_SECRET", "turn-secret-value")]
        quiet = scan(good_path, env_values)
        noisy = scan(bad_path, env_values)
    if quiet:
        print(f"self-test failed: benign log flagged: {quiet}", file=sys.stderr)
        return 1
    if len(noisy) < len(bad):
        print(f"self-test failed: only {len(noisy)} of {len(bad)} leaks detected", file=sys.stderr)
        return 1
    print(f"redaction self-test: benign accepted, {len(noisy)} leaks detected")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="*", type=Path)
    parser.add_argument("--env-file", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not args.logs:
        parser.error("provide at least one log file or --self-test")
    env_values = load_env_values(args.env_file)
    problems: list[str] = []
    for path in args.logs:
        if not path.is_file():
            problems.append(f"{path}: not a file")
            continue
        problems.extend(scan(path, env_values))
    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    print(f"scanned {len(args.logs)} file(s); {len(problems)} leak(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
