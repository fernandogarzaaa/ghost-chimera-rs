#!/usr/bin/env python3
"""Mutation gate for GHOST-Chimera Rust.

Runs `cargo mutants --changed` (mutants in code changed vs the default
branch) and enforces a kill-rate gate, mirroring the Python `testteeth
--fail-under 80` policy from Inan's standing rules.

Fail-closed: if the cargo-mutants summary cannot be parsed, or no
mutants were found in changed code, the gate FAILS loudly rather than
silently passing. An empty diff is not evidence of good tests.

Usage:
    python3 .github/scripts/mutation-gate.py --fail-under 80
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# Matches cargo-mutants summary lines such as:
#   "60 mutants tested: 45 killed, 3 timeout, 12 survived"
# We intentionally accept several verb variants because the exact
# wording has drifted across cargo-mutants releases; anything we do
# not recognize fails closed below.
SUMMARY_RE = re.compile(r"(\d+)\s+mutants?\s+tested\s*:", re.IGNORECASE)
KILLED_RE = re.compile(r"(\d+)\s+(killed|caught)", re.IGNORECASE)
SURVIVED_RE = re.compile(r"(\d+)\s+(survived|missed|not caught)", re.IGNORECASE)
TIMEOUT_RE = re.compile(r"(\d+)\s+timeout", re.IGNORECASE)
UNVIABLE_RE = re.compile(r"(\d+)\s+unviable", re.IGNORECASE)


def run_mutants(output_dir: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "cargo",
            "mutants",
            "--changed",
            "--output",
            str(output_dir),
            "-j",
            "2",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def parse_summary(output: str) -> tuple[int, int, int, int] | None:
    """Return (tested, killed, survived, timeout) or None if unparseable."""
    tested_m = SUMMARY_RE.search(output)
    killed_m = KILLED_RE.search(output)
    survived_m = SURVIVED_RE.search(output)
    if not (tested_m and killed_m and survived_m):
        return None
    timeout_m = TIMEOUT_RE.search(output)
    unviable_m = UNVIABLE_RE.search(output)
    tested = int(tested_m.group(1))
    killed = int(killed_m.group(1))
    survived = int(survived_m.group(1))
    timeout = int(timeout_m.group(1)) if timeout_m else 0
    unviable = int(unviable_m.group(1)) if unviable_m else 0
    # Sanity: the parts must add up to the total. If they don't, the
    # format drifted and we must not trust the parse.
    if killed + survived + timeout + unviable != tested:
        return None
    return tested, killed, survived, timeout


def cross_check_json(output_dir: Path, parsed: tuple[int, int, int, int]) -> bool:
    """Cross-check the text summary against mutants.json when present.

    Returns True if consistent or if the JSON file is absent (older
    cargo-mutants); False on inconsistency.
    """
    json_path = output_dir / "mutants.json"
    if not json_path.exists():
        return True
    try:
        data = json.loads(json_path.read_text())
    except (json.JSONDecodeError, OSError):
        return False
    outcomes = data.get("outcomes") if isinstance(data, dict) else None
    if not isinstance(outcomes, list):
        return True  # schema drift; text parse already sanity-checked
    tested, killed, survived, timeout = parsed
    counts: dict[str, int] = {}
    for entry in outcomes:
        outcome = str(entry.get("outcome", "")).lower()
        counts[outcome] = counts.get(outcome, 0) + 1
    json_killed = counts.get("killed", 0) + counts.get("caught", 0)
    json_survived = counts.get("survived", 0) + counts.get("missed", 0)
    json_timeout = counts.get("timeout", 0)
    return (
        json_killed == killed
        and json_survived == survived
        and json_timeout == timeout
        and sum(counts.values()) == tested
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Enforce the cargo-mutants kill-rate gate.")
    parser.add_argument("--fail-under", type=float, default=80.0)
    parser.add_argument("--output-dir", default="mutants.out")
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    proc = run_mutants(output_dir)
    print(proc.stdout)

    parsed = parse_summary(proc.stdout)
    if parsed is None:
        print(
            "MUTATION GATE FAILED: could not parse cargo-mutants summary. "
            "Failing closed: inspect the output above.",
            file=sys.stderr,
        )
        return 1

    tested, killed, survived, timeout = parsed
    if tested == 0:
        print(
            "MUTATION GATE FAILED: cargo-mutants reported zero mutants in "
            "changed code. An empty gate proves nothing.",
            file=sys.stderr,
        )
        return 1

    if not cross_check_json(output_dir, parsed):
        print(
            "MUTATION GATE FAILED: text summary and mutants.json disagree. "
            "Failing closed: inspect mutants.out/.",
            file=sys.stderr,
        )
        return 1

    # Timeouts count against the kill rate: a mutant that hangs the suite
    # is not a mutant the tests caught.
    killable = tested
    rate = 100.0 * killed / killable if killable else 0.0
    print(f"Mutation kill rate: {killed}/{killable} = {rate:.1f}% (gate: >= {args.fail_under}%)")
    if survived:
        print(f"Survived mutants: {survived} (see mutants.out/ for details)")
    if timeout:
        print(f"Timed-out mutants: {timeout} (counted as not killed)")

    if rate < args.fail_under:
        print(
            f"MUTATION GATE FAILED: {rate:.1f}% < {args.fail_under}% required.",
            file=sys.stderr,
        )
        return 1
    print("MUTATION GATE PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
