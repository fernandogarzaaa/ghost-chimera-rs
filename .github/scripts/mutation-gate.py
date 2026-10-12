#!/usr/bin/env python3
"""Changed-code mutation gate for GHOST-Chimera Rust.

Runs cargo-mutants (pinned to 27.1.0 in CI) in its supported ``--in-diff``
mode on the Rust code changed between ``--base-ref`` and ``HEAD`` and
enforces a kill-rate gate.

Where the numbers come from
---------------------------
cargo-mutants 27.x writes ``<output-dir>/mutants.out/outcomes.json`` (note:
``--output DIR`` creates ``DIR/mutants.out``; it does not name the directory
itself). That file is a ``LabOutcome``:

* top-level counters ``total_mutants``, ``caught``, ``missed``, ``timeout``,
  ``unviable``, ``success`` (mutants only; the baseline is not counted);
* ``outcomes``: one entry per scenario **including the baseline build**, each
  with ``scenario`` (``"Baseline"`` or ``{"Mutant": {...}}``) and ``summary``
  (``CaughtMutant``, ``MissedMutant``, ``Timeout``, ``Unviable``, ``Success``,
  ``Failure``).

The gate reads the counters, recounts them from the per-mutant ``summary``
fields (baseline excluded), and cross-checks both against cargo-mutants'
``caught.txt`` / ``missed.txt`` / ``timeout.txt`` / ``unviable.txt``. Any
disagreement fails the gate. The text summary on stdout is not parsed: its
wording omits zero-valued categories and is not a stable interface.

Fail-closed rules
-----------------
* An empty or unresolvable base fails. A diff touching no ``*.rs`` file is
  reported as not applicable and passes (nothing to mutate).
* A Rust diff that yields zero mutants fails: an empty gate proves nothing.
* A cargo-mutants exit code other than 0 (all caught), 2 (some missed) or
  3 (some timeouts) fails (usage error, failing baseline, bad diff, ...).
* Conservative scoring: the denominator is every generated mutant. Only a
  mutant a test caught counts as killed; missed, timed-out, unviable and
  unclassified mutants all count against the rate.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

ZERO_SHA = "0" * 40
OK_EXIT_CODES = (0, 2, 3)

# SummaryOutcome variant -> counter name in LabOutcome.
SUMMARY_TO_COUNTER = {
    "CaughtMutant": "caught",
    "MissedMutant": "missed",
    "Timeout": "timeout",
    "Unviable": "unviable",
    "Success": "success",
}
LIST_FILES = ("caught", "missed", "timeout", "unviable")


class GateError(Exception):
    """The mutation results cannot be trusted; the gate must fail closed."""


@dataclass(frozen=True)
class Counts:
    total: int
    caught: int
    missed: int
    timeout: int
    unviable: int
    success: int

    @property
    def rate(self) -> float:
        """Kill rate in percent. Every generated mutant is in the denominator."""
        return 100.0 * self.caught / self.total if self.total else 0.0


def git_diff(base_ref: str) -> str:
    """Return the Rust diff between the merge-base of base_ref and HEAD."""
    base_ref = base_ref.strip()
    if not base_ref or base_ref == ZERO_SHA:
        raise GateError("no usable diff base was provided")
    proc = subprocess.run(
        ["git", "diff", "--no-color", "--no-ext-diff", f"{base_ref}...HEAD", "--", "*.rs"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if proc.returncode != 0:
        raise GateError(proc.stdout.strip() or f"git diff exited {proc.returncode}")
    return proc.stdout


def run_mutants(output_dir: Path, diff_path: Path, jobs: int) -> tuple[int, str]:
    proc = subprocess.run(
        [
            "cargo",
            "mutants",
            "--in-diff",
            str(diff_path),
            "--output",
            str(output_dir),
            "--jobs",
            str(jobs),
            "--no-shuffle",
            "--colors",
            "never",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    print(proc.stdout)
    return proc.returncode, proc.stdout or ""


def is_test_only(cargo_mutants_output: str) -> bool:
    """True when cargo-mutants reports the diff touched no mutable source.

    If only test code changed (e.g. a ``#[cfg(test)]`` module or tests/),
    cargo-mutants prints "No mutants to filter". That is not a failure: there
    is no source code to mutate. (Rule kept from main, commit 4ab28c3.)
    """
    return "No mutants to filter" in cargo_mutants_output


def _is_mutant(entry: dict) -> bool:
    scenario = entry.get("scenario")
    if scenario == "Baseline":
        return False
    if isinstance(scenario, dict) and "Mutant" in scenario:
        return True
    raise GateError(f"unrecognised scenario in outcomes.json: {scenario!r}")


def _count_lines(path: Path) -> int:
    if not path.exists():
        return 0
    return sum(1 for line in path.read_text(encoding="utf-8").splitlines() if line.strip())


def read_counts(mutants_out: Path) -> Counts:
    """Parse and cross-check a cargo-mutants 27.x ``mutants.out`` directory."""
    outcomes_path = mutants_out / "outcomes.json"
    try:
        data = json.loads(outcomes_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise GateError(f"cannot read {outcomes_path}: {exc}") from exc
    if not isinstance(data, dict):
        raise GateError("outcomes.json is not an object")
    try:
        declared = Counts(
            total=int(data["total_mutants"]),
            caught=int(data["caught"]),
            missed=int(data["missed"]),
            timeout=int(data["timeout"]),
            unviable=int(data["unviable"]),
            success=int(data.get("success", 0)),
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise GateError(f"unexpected outcomes.json schema: {exc!r}") from exc

    outcomes = data.get("outcomes")
    if not isinstance(outcomes, list):
        raise GateError("outcomes.json has no outcomes list")
    recount = {name: 0 for name in SUMMARY_TO_COUNTER.values()}
    mutants = 0
    for entry in outcomes:
        if not isinstance(entry, dict):
            raise GateError("malformed entry in outcomes list")
        if not _is_mutant(entry):
            continue  # the baseline build is not a mutant
        mutants += 1
        counter = SUMMARY_TO_COUNTER.get(str(entry.get("summary", "")))
        if counter is not None:
            recount[counter] += 1
        # "Failure" (unclassified) stays uncounted: it is still in `mutants`,
        # so it lands in the denominator without counting as killed.

    if mutants != declared.total:
        raise GateError(f"outcomes list has {mutants} mutants but total_mutants is {declared.total}")
    for name, value in recount.items():
        if value != getattr(declared, name):
            raise GateError(f"outcomes list has {value} {name} but the counter says {getattr(declared, name)}")
    for name in LIST_FILES:
        listed = _count_lines(mutants_out / f"{name}.txt")
        if listed != getattr(declared, name):
            raise GateError(f"{name}.txt lists {listed} but outcomes.json says {getattr(declared, name)}")
    return declared


def evaluate(counts: Counts, fail_under: float) -> tuple[bool, str]:
    """Return (passed, message) for already-validated counts."""
    if counts.total == 0:
        return False, "a Rust source diff produced zero mutants"
    msg = (
        f"Mutation kill rate: {counts.caught}/{counts.total} = {counts.rate:.1f}% "
        f"(gate: >= {fail_under}%; {counts.missed} missed, {counts.timeout} timeout, "
        f"{counts.unviable} unviable, all counted as not killed)"
    )
    return counts.rate >= fail_under, msg


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Enforce the cargo-mutants kill-rate gate on changed Rust code.")
    parser.add_argument("--base-ref", default="origin/main", help="git ref to compare HEAD against (merge-base)")
    parser.add_argument("--fail-under", type=float, default=80.0)
    parser.add_argument(
        "--output-dir",
        default=".",
        help="parent directory; cargo-mutants writes <output-dir>/mutants.out",
    )
    parser.add_argument("--jobs", type=int, default=2)
    args = parser.parse_args(argv)

    try:
        diff = git_diff(args.base_ref)
    except GateError as exc:
        print(f"MUTATION GATE FAILED: cannot compute Rust diff against {args.base_ref!r}: {exc}", file=sys.stderr)
        return 1
    if not diff.strip():
        print("MUTATION GATE NOT APPLICABLE: no Rust source changes in the selected diff.")
        return 0

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ghost-mutants-") as temp_dir:
        diff_path = Path(temp_dir) / "changed-rust.diff"
        diff_path.write_text(diff, encoding="utf-8")
        code, output = run_mutants(output_dir, diff_path, args.jobs)
    if is_test_only(output):
        print("MUTATION GATE SKIPPED: no source mutants in diff (test-only changes).")
        return 0
    if code not in OK_EXIT_CODES:
        print(
            f"MUTATION GATE FAILED: cargo-mutants exited {code} "
            "(usage error, failing baseline, bad diff, or internal error).",
            file=sys.stderr,
        )
        return 1

    mutants_out = output_dir / "mutants.out"
    try:
        counts = read_counts(mutants_out)
    except GateError as exc:
        print(f"MUTATION GATE FAILED: {exc}. Failing closed; inspect {mutants_out}/.", file=sys.stderr)
        return 1

    passed, msg = evaluate(counts, args.fail_under)
    print(msg)
    if counts.missed:
        print("Missed mutants:")
        print((mutants_out / "missed.txt").read_text(encoding="utf-8"))
    if not passed:
        print(f"MUTATION GATE FAILED: {msg}", file=sys.stderr)
        return 1
    print("MUTATION GATE PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
