#!/usr/bin/env python3
"""Unit tests for mutation-gate.py's cargo-mutants 27.x result parser.

Run: python3 -m unittest discover -s .github/scripts -p 'test_*.py'
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location("mutation_gate", Path(__file__).with_name("mutation-gate.py"))
assert _SPEC and _SPEC.loader
gate = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = gate  # dataclasses resolve their module via sys.modules
_SPEC.loader.exec_module(gate)


def _mutant(name: str, summary: str) -> dict:
    return {
        "scenario": {"Mutant": {"name": name, "file": "src/lib.rs"}},
        "summary": summary,
        "log_path": f"log/{name}.log",
        "diff_path": f"diff/{name}.diff",
        "phase_results": [],
    }


BASELINE = {"scenario": "Baseline", "summary": "Success", "log_path": "log/baseline.log", "phase_results": []}


def write_fixture(root: Path, summaries: list[str], **overrides: int) -> Path:
    """Write a mutants.out directory shaped like cargo-mutants 27.1.0 output."""
    out = root / "mutants.out"
    out.mkdir()
    mutants = [_mutant(f"m{i}", s) for i, s in enumerate(summaries)]
    counters = {name: 0 for name in gate.SUMMARY_TO_COUNTER.values()}
    for s in summaries:
        if s in gate.SUMMARY_TO_COUNTER:
            counters[gate.SUMMARY_TO_COUNTER[s]] += 1
    data = {"outcomes": [BASELINE, *mutants], "total_mutants": len(summaries), **counters}
    data.update(overrides)
    (out / "outcomes.json").write_text(json.dumps(data), encoding="utf-8")
    for name in gate.LIST_FILES:
        lines = [m["scenario"]["Mutant"]["name"] for m in mutants if gate.SUMMARY_TO_COUNTER.get(m["summary"]) == name]
        (out / f"{name}.txt").write_text("".join(f"{line}\n" for line in lines), encoding="utf-8")
    return out


class ReadCountsTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_baseline_excluded_and_unviable_counts_against(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant"] * 8 + ["MissedMutant", "Unviable"])
        counts = gate.read_counts(out)
        self.assertEqual(counts.total, 10)
        self.assertEqual(counts.caught, 8)
        self.assertEqual(counts.unviable, 1)
        self.assertAlmostEqual(counts.rate, 80.0)
        self.assertTrue(gate.evaluate(counts, 80.0)[0])

    def test_unviable_can_fail_the_gate(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant"] * 7 + ["Unviable"] * 3)
        passed, _ = gate.evaluate(gate.read_counts(out), 80.0)
        self.assertFalse(passed)

    def test_timeout_counts_against(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant"] * 3 + ["Timeout"])
        self.assertAlmostEqual(gate.read_counts(out).rate, 75.0)

    def test_all_caught_passes_without_missed_category(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant"] * 4)
        self.assertTrue(gate.evaluate(gate.read_counts(out), 80.0)[0])

    def test_zero_mutants_fails(self) -> None:
        out = write_fixture(self.root, [])
        self.assertFalse(gate.evaluate(gate.read_counts(out), 80.0)[0])

    def test_counter_mismatch_fails_closed(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant", "MissedMutant"], caught=2, missed=0)
        with self.assertRaises(gate.GateError):
            gate.read_counts(out)

    def test_total_including_baseline_fails_closed(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant", "CaughtMutant"], total_mutants=3)
        with self.assertRaises(gate.GateError):
            gate.read_counts(out)

    def test_list_file_mismatch_fails_closed(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant", "MissedMutant"])
        (out / "missed.txt").write_text("", encoding="utf-8")
        with self.assertRaises(gate.GateError):
            gate.read_counts(out)

    def test_missing_outcomes_fails_closed(self) -> None:
        (self.root / "mutants.out").mkdir()
        with self.assertRaises(gate.GateError):
            gate.read_counts(self.root / "mutants.out")

    def test_unknown_scenario_fails_closed(self) -> None:
        out = write_fixture(self.root, ["CaughtMutant"])
        data = json.loads((out / "outcomes.json").read_text(encoding="utf-8"))
        data["outcomes"][0]["scenario"] = "Something"
        (out / "outcomes.json").write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaises(gate.GateError):
            gate.read_counts(out)

    def test_empty_base_is_rejected(self) -> None:
        for base in ("", "   ", gate.ZERO_SHA):
            with self.assertRaises(gate.GateError):
                gate.git_diff(base)


class TestOnlySkipTest(unittest.TestCase):
    """main's 4ab28c3 rule: a test-only diff skips the gate instead of failing."""

    def test_no_mutants_to_filter_is_test_only(self) -> None:
        self.assertTrue(gate.is_test_only("Found 12 mutants\nNo mutants to filter\n"))

    def test_normal_output_is_not_test_only(self) -> None:
        self.assertFalse(gate.is_test_only("Found 12 mutants to test\n12 mutants tested: 12 caught\n"))
        self.assertFalse(gate.is_test_only(""))

    def test_main_skips_on_test_only_diff(self) -> None:
        orig_diff, orig_run = gate.git_diff, gate.run_mutants
        try:
            gate.git_diff = lambda base: "diff --git a/src/lib.rs b/src/lib.rs\n"
            gate.run_mutants = lambda out, diff, jobs: (0, "No mutants to filter")
            with tempfile.TemporaryDirectory() as tmp:
                self.assertEqual(gate.main(["--output-dir", tmp]), 0)
            # A real run with no outcomes still fails closed.
            gate.run_mutants = lambda out, diff, jobs: (0, "Found 3 mutants to test")
            with tempfile.TemporaryDirectory() as tmp:
                self.assertEqual(gate.main(["--output-dir", tmp]), 1)
        finally:
            gate.git_diff, gate.run_mutants = orig_diff, orig_run


class MainFlowTest(unittest.TestCase):
    """End-to-end exit codes of main() with git and cargo-mutants stubbed."""

    def setUp(self) -> None:
        self._orig = (gate.git_diff, gate.run_mutants)
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        gate.git_diff = lambda base: "diff --git a/src/lib.rs b/src/lib.rs\n"

    def tearDown(self) -> None:
        gate.git_diff, gate.run_mutants = self._orig
        self._tmp.cleanup()

    def _run(self, summaries: list[str] | None, code: int = 0, fail_under: str = "80") -> int:
        def fake(out_dir: Path, diff: Path, jobs: int) -> tuple[int, str]:
            self.assertTrue(diff.read_text(encoding="utf-8").startswith("diff --git"))
            self.assertEqual(jobs, 2)
            if summaries is not None:
                write_fixture(Path(out_dir), summaries)
            return code, "Found mutants to test"

        gate.run_mutants = fake
        return gate.main(["--output-dir", str(self.root), "--fail-under", fail_under])

    def test_passing_run_returns_zero(self) -> None:
        self.assertEqual(self._run(["CaughtMutant"] * 9 + ["MissedMutant"]), 0)

    def test_below_threshold_returns_one(self) -> None:
        self.assertEqual(self._run(["CaughtMutant"] * 7 + ["MissedMutant"] * 3), 1)

    def test_exactly_at_threshold_passes(self) -> None:
        self.assertEqual(self._run(["CaughtMutant"] * 8 + ["MissedMutant"] * 2), 0)

    def test_missed_exit_codes_2_and_3_are_still_evaluated(self) -> None:
        self.assertEqual(self._run(["CaughtMutant"] * 4, code=2), 0)

    def test_bad_exit_code_fails_closed(self) -> None:
        self.assertEqual(self._run(["CaughtMutant"] * 4, code=1), 1)
        self.assertEqual(self._run(None, code=4), 1)

    def test_unreadable_results_fail_closed(self) -> None:
        self.assertEqual(self._run(None), 1)

    def test_diff_error_fails_closed(self) -> None:
        def boom(base: str) -> str:
            raise gate.GateError("bad ref")

        gate.git_diff = boom
        self.assertEqual(gate.main(["--output-dir", str(self.root)]), 1)

    def test_empty_diff_is_not_applicable(self) -> None:
        gate.git_diff = lambda base: "   \n"
        gate.run_mutants = lambda *a: self.fail("cargo-mutants must not run on an empty diff")
        self.assertEqual(gate.main(["--output-dir", str(self.root)]), 0)

    def test_rate_and_evaluate_message(self) -> None:
        counts = gate.Counts(total=4, caught=3, missed=1, timeout=0, unviable=0, success=0)
        self.assertAlmostEqual(counts.rate, 75.0)
        ok, msg = gate.evaluate(counts, 75.0)
        self.assertTrue(ok)
        self.assertIn("3/4 = 75.0%", msg)
        self.assertFalse(gate.evaluate(counts, 75.1)[0])
        self.assertEqual(gate.Counts(0, 0, 0, 0, 0, 0).rate, 0.0)


if __name__ == "__main__":
    unittest.main()
