---
name: improve-coverage
description: Use when you need to raise main-source line coverage in the bazel-diff repo, write tests for an under-covered Rust module, fix a CI failure on the 90% coverage gate, or pick the highest-leverage files to test next. Triggers on requests like "the coverage gate is failing, fix it", "write tests for X", "we need more coverage", or "what should I test to get to 90%".
---

# Improving coverage to clear the 90% gate

bazel-diff enforces a 90% main-source line-coverage gate on every PR (see [coverage-status](../coverage-status/SKILL.md) for the inspection side), plus per-target minimums on `//src:rust_tests` and `//src:cli_tests` that fire during `bazel coverage`. When the gate fails or you want to raise the bar, the workflow is: pick the worst-covered files, write small focused unit tests, re-run the gate locally before pushing.

## 1. Pick the right files to target

Run `make coverage` (or check the latest CI artifact) and look at the top of the sorted table. Prioritise files by **uncovered-lines-per-test-effort**, not by lowest percentage:

- **Highest-leverage**: small helpers at 0% (a parser, a converter, a value type's `Display`) — one short unit test usually moves the needle without much code.
- **Highest absolute gain**: large files with moderate coverage (`src/server.rs`, `src/bazel.rs`, `src/main.rs`) — closing a small percentage gap covers many lines.
- **Lowest leverage**: tiny files at 50–80% where the remaining branches are error paths needing fault injection or refactors.

## 2. Write the test

Existing tests follow a consistent shape:

- Unit tests live in a `#[cfg(test)] mod tests` at the bottom of the `src/` file they cover, and are run by `//src:rust_tests` (library) and `//src:cli_tests` (`main.rs`).
- They test pure transformations with injected data: query planning, repository lowering, hash computation and module-impact decisions take their inputs as values, not as a live Bazel. Nothing under `src/` spawns a fake `bazel` executable — subprocess integration is the e2e suite's job.
- Filesystem cases use `tempfile::TempDir`; server cases in `src/server.rs` bind a loopback port and drive the real HTTP handler.
- Anything that needs a real Bazel workspace is an e2e case under `tests/e2e/` (see [tools/e2e/README.md](../../../tools/e2e/README.md) for the per-case target split and `make regen-e2e`). E2E cases are slow and carry no per-target coverage minimum, so prefer a unit test whenever the logic can be reached without Bazel.

## 3. Verify locally before pushing

```bash
cargo test                                   # fastest inner loop (unit + e2e crate)
bazel test //:rust_tests                     # what CI runs, with the pinned toolchain
make coverage                                # the full gate
```

`bazel coverage` is what makes the per-target minimums fire: plain `bazel test` never invokes the LCOV merger. If a target falls below its minimum, its test log ends with the merger's per-file breakdown and the action exits 33.

Lint gates run on every build through the clippy and rustfmt aspects in `.bazelrc`; `make format` (`bazel run //tools/format:rustfmt`) fixes formatting with the exact rustfmt CI uses.

## 4. Things that don't work / aren't worth attempting

- **`main()` in `src/main.rs`** — exits the process; the testable surface is the command functions it dispatches to, which the `cli_tests` target already covers. Test those, not `main`.
- **`unreachable!` / exhaustive-match fallbacks** on generated protobuf enums — only reachable when Bazel's `Target.Discriminator` grows a new value. Not worth a hand-forged proto.
- **Network error paths in the S3 cache tier** — covered by the loopback mock in `src/server.rs`'s tests; do not add real-bucket tests.

## When the gate fails on a flake, not on a coverage drop

If the CI threshold step fails with `error: LCOV report not found at 'bazel-out/_coverage/_coverage_report.dat'`, that's an infrastructure issue, not a coverage regression. The fix that landed in PR #356 was to propagate `USE_BAZEL_VERSION` to the threshold step so `bazelisk run` doesn't start a different bazel server. If you see a similar mismatch resurface, check that env propagation first.
