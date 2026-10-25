#!/usr/bin/env python3
"""Git helpers for path selection, added-line scans and coverage deltas."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path


class GitError(RuntimeError):
    pass


def _git(root: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if proc.returncode != 0:
        raise GitError(proc.stderr.strip() or f"git {' '.join(args)} failed")
    return proc.stdout


def changed_files(root: Path, base: str, head: str) -> list[str]:
    out = _git(root, "diff", "--name-only", "--diff-filter=ACMR", f"{base}...{head}")
    return [line.strip() for line in out.splitlines() if line.strip()]


def added_lines(root: Path, base: str, head: str, path: str) -> set[int]:
    """New-file line numbers that were added by the diff."""
    out = _git(root, "diff", "-U0", "--no-color", f"{base}...{head}", "--", path)
    added: set[int] = set()
    new_line = None
    for line in out.splitlines():
        if line.startswith("@@"):
            m = re.search(r"\+(\d+)(?:,(\d+))?", line)
            new_line = int(m.group(1)) if m else None
            continue
        if new_line is None:
            continue
        if line.startswith("+") and not line.startswith("+++"):
            added.add(new_line)
            new_line += 1
        elif line.startswith("-") and not line.startswith("---"):
            continue
        elif line.startswith(" "):
            new_line += 1
    return added


def added_line_text(root: Path, base: str, head: str, path: str) -> dict[int, str]:
    out = _git(root, "diff", "-U0", "--no-color", f"{base}...{head}", "--", path)
    rows: dict[int, str] = {}
    new_line = None
    for line in out.splitlines():
        if line.startswith("@@"):
            m = re.search(r"\+(\d+)(?:,(\d+))?", line)
            new_line = int(m.group(1)) if m else None
            continue
        if new_line is None:
            continue
        if line.startswith("+") and not line.startswith("+++"):
            rows[new_line] = line[1:]
            new_line += 1
        elif line.startswith(" ") :
            new_line += 1
    return rows


def merge_base(root: Path, base: str, head: str) -> str:
    return _git(root, "merge-base", base, head).strip()
