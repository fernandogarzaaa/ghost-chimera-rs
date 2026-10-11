#!/usr/bin/env python3
"""Changed-code mutation gate for GHOST-Chimera Rust.

Uses cargo-mutants' supported --in-diff mode and parses the current summary
format. A diff with no Rust changes is not applicable; an unresolvable base,
unparseable result, inconsistent result files, or zero tested mutants in a
Rust code diff fails closed.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

SUMMARY_RE = re.compile(
    r"(\\d+)\\s+mutants?\\s+tested(?:\\s+in\\s+\\S+)?\\s*:",
    re.IGNORECASE,
)
KILLED_RE = re.compile(r"(\\d+)\\s+(?:killed|caught)", re.IGNORECASE)
SURVIVED_RE = re.compile(r"(\\d+)\\s+(?:survived|missed|not caught)", re.IGNORECASE)
TIMEOUT_RE = re.compile(r"(\\d+)\\s+(?:timeouts?|timed out)", re.IGNORECASE)
UNVIABLE_RE = re.compile(r"(\\d+)\\s+unviable", re.IGNORECASE)


def git_diff(base_ref: str) -> tuple[str | None, str | None]:
    """Return (diff, error), comparing base_ref to HEAD."""
    if not base_ref.strip():
        return None, "no diff base was provided"
    proc = subprocess.run(
        ["git", "diff", "--binary", f"{base_ref}...HEAD", "--", "*.rs"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if proc.returncode != 0:
        return None, proc.stdout.strip() or f"git diff exited {proc.returncode}"
    return proc.stdout, None


def run_mutants(output_dir: Path, diff_path: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "cargo",
            "mutants",
            "--in-diff",
            str(diff_path),
            "--output",
            str(output_dir),
            "-j",
            "2",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def parse_summary(output: str) -> tuple[int, int, int, int, int] | None:
    """Return tested, caught, missed, timeout, unviable from the final summary."""
    matches = [line for line in output.splitlines() if SUMMARY_RE.search(line)]
    if not matches:
        return None
    line = matches[-1]
    tested_match = SUMMARY_RE.search(line)
    caught_match = KILLED_RE.search(line)
    missed_match = SURVIVED_RE.search(line)
    if not (tested_match and caught_match and missed_match):
        return None
    timeout_match = TIMEOUT_RE.search(line)
    unviable_match = UNVIABLE_RE.search(line)
    tested = int(tested_match.group(1))
    caught = int(caught_match.group(1))
    missed = int(missed_match.group(1))
    timeout = int(timeout_match.group(1)) if timeout_match else 0
    unviable = int(unviable_match.group(1)) if unviable_match else 0
    if caught + missed + timeout + unviable != tested:
        return None
    return tested, caught, missed, timeout, unviable


def cross_check_json(output_dir: Path, parsed: tuple[int, int, int, int, int]) -> bool:
    """Cross-check text summary with cargo-mutants outcomes.json or legacy mutants.json."""
    candidates = [output_dir / "outcomes.json", output_dir / "mutants.json"]
    json_path = next((path for path in candidates if path.exists()), None)
    if json_path is None:
        return False
    try:
        data = json.loads(json_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return False
    outcomes = data.get("outcomes") if isinstance(data, dict) else None
    if not isinstance(outcomes, list):
        return False
    counts: dict[str, int] = {}
    for entry in outcomes:
        if not isinstance(entry, dict):
            return False
        outcome = str(entry.get("outcome", "")).strip().lower().replace("-", "_").replace(" ", "_")
        counts[outcome] = counts.get(outcome, 0) + 1
    tested, caught, missed, timeout, unviable = parsed
    json_caught = counts.get("caught", 0) + counts.get("killed", 0)
    json_missed = counts.get("missed", 0) + counts.get("survived", 0) + counts.get("not_caught", 0)
    json_timeout = counts.get("timeout", 0) + counts.get("timed_out", 0)
    json_unviable = counts.get("unviable", 0)
    return (
        json_caught == caught
        and json_missed == missed
        and json_timeout == timeout
        and json_unviable == unviable
        and len(outcomes) == tested
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Enforce the cargo-mutants kill-rate gate on changed Rust code.")
    parser.add_argument("--base-ref", default="origin/main", help="git ref to compare HEAD against")
    parser.add_argument("--fail-under", type=float, default=80.0)
    parser.add_argument("--output-dir", default="mutants.out")
    args = parser.parse_args()

    diff, error = git_diff(args.base_ref)
    if error:
        print(f"MUTATION GATE FAILED: cannot compute Rust diff against {args.base_ref!r}: {error}", file=sys.stderr)
        return 1
    assert diff is not None
    if not diff.strip():
        print("MUTATION GATE NOT APPLICABLE: no Rust source changes in the selected diff.")
        return 0

    output_dir = Path(args.output_dir)
    with tempfile.TemporaryDirectory(prefix="ghost-mutants-") as temp_dir:
        diff_path = Path(temp_dir) / "changed-rust.diff"
        diff_path.write_text(diff, encoding="utf-8")
        proc = run_mutants(output_dir, diff_path)
    print(proc.stdout)

    parsed = parse_summary(proc.stdout)
    if parsed is None:
        print(
            "MUTATION GATE FAILED: could not parse cargo-mutants summary. "
            "Failing closed; inspect the output above.",
            file=sys.stderr,
        )
        return 1

    tested, caught, missed, timeout, unviable = parsed
    if tested == 0:
        print(
            "MUTATION GATE FAILED: a Rust source diff produced zero tested mutants.",
            file=sys.stderr,
        )
        return 1

    if not cross_check_json(output_dir, parsed):
        print(
            "MUTATION GATE FAILED: cargo-mutants summary and outcomes JSON are "
            "missing, malformed, or inconsistent. Inspect mutants.out/.",
            file=sys.stderr,
        )
        return 1

    # Timeouts and unviable mutants remain in the denominator. This is
    # conservative: only a mutant caught by a test counts as killed.
    rate = 100.0 * caught / tested
    print(f"Mutation kill rate: {caught}/{tested} = {rate:.1f}% (gate: >= {args.fail_under}%)")
    if missed:
        print(f"Missed mutants: {missed} (see mutants.out/)")
    if timeout:
        print(f"Timed-out mutants: {timeout} (counted as not killed)")
    if unviable:
        print(f"Unviable mutants: {unviable} (counted as not killed)")

    if rate < args.fail_under:
        print(f"MUTATION GATE FAILED: {rate:.1f}% < {args.fail_under}% required.", file=sys.stderr)
        return 1
    print("MUTATION GATE PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
