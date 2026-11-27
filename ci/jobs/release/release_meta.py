#!/usr/bin/env python3
"""Resolve and validate the release version before it reaches any shell.

H-44: `workflow_dispatch` inputs used to be interpolated straight into
`GITHUB_OUTPUT` and shell/Python argv. A value containing a newline could add
arbitrary output keys; a value containing quotes could break out of the
`run:` string. This module is the only place the raw version is trusted, and it
only emits a strict SemVer string after validation.

The CLI is driven by environment variables so the workflow never builds a
shell command out of the input value.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

# Strict SemVer 2.0.0. Numeric identifiers have no leading zeros; pre-release
# and build identifiers are dot-separated ASCII alphanumerics/hyphens.
_VERSION_RE = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)
_MAX_VERSION = 96


class VersionError(ValueError):
    pass


def validate_version(raw: str) -> str:
    """Return the canonical version, or raise VersionError.

    Newlines/carriage returns are rejected first: even if a future regex change
    accidentally allowed them, output-file injection must stay impossible.
    """
    if "\n" in raw or "\r" in raw:
        raise VersionError("version must be a single line")
    if not raw:
        raise VersionError("version is empty")
    if len(raw) > _MAX_VERSION:
        raise VersionError(f"version is longer than {_MAX_VERSION} characters")
    if not _VERSION_RE.fullmatch(raw):
        raise VersionError(f"version is not strict SemVer: {raw!r}")
    return raw


def resolve_version(raw_input: str, ref_name: str) -> str:
    """Dispatch input wins; otherwise the tag name must be `v<semver>`."""
    if raw_input:
        return validate_version(raw_input)
    if not ref_name.startswith("v"):
        raise VersionError(f"tag {ref_name!r} must start with 'v'")
    return validate_version(ref_name[1:])


def publication_mode(event_name: str, publish_input: str) -> bool:
    return event_name == "push" or publish_input.strip().lower() == "true"


def image_name(repository: str) -> str:
    if not repository or "/" not in repository:
        raise VersionError(f"GITHUB_REPOSITORY is not owner/repo: {repository!r}")
    return f"ghcr.io/{repository.lower()}/wxlike-server"


def write_outputs(path: Path, version: str, publish: bool, image: str) -> None:
    # Values are already constrained (SemVer/repo shape), never raw input.
    with path.open("a", encoding="utf-8") as handle:
        handle.write(f"version={version}\n")
        handle.write(f"publish={'true' if publish else 'false'}\n")
        handle.write(f"image={image}\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", default=os.environ.get("INPUT_VERSION", ""))
    parser.add_argument("--ref", default=os.environ.get("GITHUB_REF_NAME", ""))
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--publish", default=os.environ.get("INPUT_PUBLISH", "false"))
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY", ""))
    parser.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT", ""))
    args = parser.parse_args(argv)
    try:
        version = resolve_version(args.version, args.ref)
        publish = publication_mode(args.event, args.publish)
        image = image_name(args.repository)
    except VersionError as err:
        print(f"::error::{err}", file=sys.stderr)
        return 1
    if args.github_output:
        write_outputs(Path(args.github_output), version, publish, image)
    print(f"version={version} publish={'true' if publish else 'false'} image={image}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
