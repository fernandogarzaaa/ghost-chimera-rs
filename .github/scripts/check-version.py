#!/usr/bin/env python3
"""Enforce the single-source-of-truth version rule for releases.

The git tag (e.g. `v0.5.0`), [workspace.package] version in Cargo.toml,
and `version` in desktop/package.json must all agree. Any mismatch
fails the release before a single binary is built.

Usage:
    python3 .github/scripts/check-version.py v0.5.0
    python3 .github/scripts/check-version.py --repo /path/to/repo v0.5.0
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

TAG_RE = re.compile(r"^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$")


def main() -> int:
    parser = argparse.ArgumentParser(description="Check release version consistency.")
    parser.add_argument("tag", help="Git tag, e.g. v0.5.0")
    parser.add_argument("--repo", default=".", help="Repo root")
    args = parser.parse_args()

    repo = Path(args.repo)
    m = TAG_RE.match(args.tag.strip())
    if not m:
        print(f"VERSION CHECK FAILED: tag {args.tag!r} is not v<semver>", file=sys.stderr)
        return 1
    tag_version = m.group(1)

    cargo_toml = repo / "Cargo.toml"
    try:
        ws_version = tomllib.loads(cargo_toml.read_text())["workspace"]["package"]["version"]
    except (OSError, KeyError, tomllib.TOMLDecodeError) as exc:
        print(f"VERSION CHECK FAILED: cannot read workspace version: {exc}", file=sys.stderr)
        return 1

    desktop_pkg = repo / "desktop" / "package.json"
    try:
        desktop_version = json.loads(desktop_pkg.read_text())["version"]
    except (OSError, KeyError, json.JSONDecodeError) as exc:
        print(f"VERSION CHECK FAILED: cannot read desktop/package.json version: {exc}", file=sys.stderr)
        return 1

    errors = []
    if ws_version != tag_version:
        errors.append(f"Cargo.toml workspace version {ws_version!r} != tag version {tag_version!r}")
    if desktop_version != tag_version:
        errors.append(f"desktop/package.json version {desktop_version!r} != tag version {tag_version!r}")

    if errors:
        print("VERSION CHECK FAILED:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print(f"VERSION CHECK PASSED: {tag_version} (tag, Cargo.toml, desktop/package.json agree)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
