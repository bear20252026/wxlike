#!/usr/bin/env python3
"""Small path/glob helpers shared by CI scripts (stdlib only)."""

from __future__ import annotations

import re
from pathlib import Path


def glob_to_regex(pattern: str) -> re.Pattern[str]:
    """Translate a POSIX-ish glob into a regex.

    ``**/`` matches zero or more leading directories, trailing ``**`` matches
    everything below, ``*`` never crosses ``/``. This is deliberately stricter
    than fnmatch: a pattern must not accidentally match a sibling directory.
    """
    out = ["^"]
    i = 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    out.append("$")
    return re.compile("".join(out))


class GlobSet:
    def __init__(self, patterns: list[str]):
        self.patterns = list(patterns)
        self._compiled = [(p, glob_to_regex(p)) for p in self.patterns]

    def match(self, path: str) -> str | None:
        path = path.replace("\\", "/").lstrip("./")
        for raw, rx in self._compiled:
            if rx.match(path):
                return raw
        return None

    def any(self, path: str) -> bool:
        return self.match(path) is not None


def normalize(path: str) -> str:
    return path.replace("\\", "/").lstrip("./")


def read_lines(path: Path) -> list[str]:
    return [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
