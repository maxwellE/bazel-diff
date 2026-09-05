---
name: coverage-status
description: Use to check the current main-source line coverage of the bazel-diff repo, find files below the 90% threshold, see how far the repo is from the gate, or produce an interactive HTML coverage report. Triggers on questions like "what's the test coverage", "is the coverage gate passing", "which files have the worst coverage", "show me a coverage report", or any other inspection of the LCOV output from bazel coverage.
---

# Checking coverage status

bazel-diff has a 90% main-source line-coverage gate enforced in CI ([.github/workflows/ci.yaml](../../../.github/workflows/ci.yaml)). The repo-wide enforcement code lives in [tools/coverage_check.py](../../../tools/coverage_check.py); the gate runs on every `coverage` matrix entry as `bazelisk run //tools:coverage-check -- bazel-out/_coverage/_coverage_report.dat`. On top of that, individual test targets declare per-target minimums that the Rust LCOV merger under [tools/coverage/](../../../tools/coverage/) enforces during the coverage run itself.

## Fastest path: just check the current state

```bash
make coverage
```

This runs `bazel coverage --combined_report=lcov //src:cli_tests //src:rust_tests //tools:coverage_check_test //tools/coverage/... //tools/go/...`, then `bazel run //tools:coverage-check` against the combined report (twice: once for `src/`, once scoped to `tools/go/`). It prints a per-file table sorted worst-first, the overall percentage, and PASS/FAIL against the threshold. Exits non-zero when below.

If a coverage report already exists in `bazel-out/_coverage/_coverage_report.dat` (e.g. from a prior `bazel coverage` invocation) and you only want to re-check the threshold without re-running the test suite:

```bash
make coverage-check
```

For an annotated HTML report (lines highlighted as covered/uncovered, per-file drilldown), use:

```bash
make coverage-html
```

Output lands at `coverage-html/index.html`. Requires `genhtml` from the `lcov` package (`brew install lcov` on macOS, `apt-get install lcov` on Debian/Ubuntu). The threshold gate still runs alongside HTML generation — the HTML is produced even when the gate fails, since the below-threshold case is exactly when the report is most useful.

## Interpreting the output

The per-file table looks like:

```
    COV%  LINES (hit/total)  FILE
--------  -----------------  ----
  85.10%    401 / 471        src/main.rs
  93.42%   1263 / 1352       src/server.rs
  ...
 100.00%     23 / 23         src/fingerprint.rs

Overall main-source line coverage: 94.37% (4816 / 5103)
Threshold:                         90.00%
PASS
```

- **Sort order is ascending** — worst-covered files appear first, so the gap is at the top.
- **Only `src/` counts** (plus `tools/coverage_check.py`) — the e2e crate under `tests/` and the tooling under `tools/` are excluded, so well-covered test code cannot hide thin production coverage. The Rust unit tests live in `#[cfg(test)]` modules inside the `src/` files, so their lines are counted along with the code they cover; that is the same scope the per-target minimum on `//src:rust_tests` uses.
- **Files with `LF:0` are dropped** — that's a "no instrumentation data" signal (e.g. Bazel's Python coverage when no toolchain coverage tool is configured), and treating it as 0% would tank the threshold.
- **Threshold is inclusive at the boundary** — exactly 90.00% passes; 89.99% fails.

## Looking at CI's number, not local

The number CI sees comes from the `coverage` matrix; download the `coverage-report-*` artifact from a recent run for an exact snapshot, or just read the "Overall main-source line coverage:" line in the workflow log.

## Configuration

- **Threshold**: `COVERAGE_THRESHOLD` env var (default 90). Set in [.github/workflows/ci.yaml](../../../.github/workflows/ci.yaml) for CI, or on the command line to experiment: `COVERAGE_THRESHOLD=85 make coverage`.
- **Include prefixes**: `--include` flag or `$COVERAGE_INCLUDE` env var (default `src/,tools/coverage_check.py`). Only files starting with one of these prefixes count. CI runs a second pass with `--include tools/go/` so Go is gated independently.
- **Self-coverage of the checker**: `tools/coverage_check.py` is itself in the default include list, but Bazel's Python coverage emits `LF:0` for it today because the Python toolchain in `MODULE.bazel` has no `coverage_tool` configured. The py_test (`bazel test //tools:coverage_check_test`) is the actual proof the checker is exercised.
- **Per-target minimums**: declared through `coverage_enforced_test` / `coverage_minimum_env` in [tools/coverage/defs.bzl](../../../tools/coverage/defs.bzl); a failing target exits with code 33 and its test log carries the per-file breakdown.
