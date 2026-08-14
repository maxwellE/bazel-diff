# Validating issue #457 with the PR #459 performance harness

Issue [#457](https://github.com/Tinder/bazel-diff/issues/457) reports two
performance problems in the Rust implementation and proposes an inline patch
for each. This document records an independent validation of both claims and
both fixes, using the hermetic workload/gate machinery from PR
[#459](https://github.com/Tinder/bazel-diff/pull/459): the baseline (unpatched)
Rust binary is placed in the gate's reference slot and the patched binary in
the candidate slot, so the gate's output-parity check doubles as proof that the
patches do not change emitted hashes.

Environment: 4-CPU Linux container, rustc 1.94.1, release builds of the same
tree before and after applying the two patches.

## Claim 1: external repository resolution shells out per source file

**Code-level check (confirmed).** At the pre-patch tree,
`ExternalRepoResolver::resolve` (src/hash.rs) has no memoization; it guesses
only `external/<repo>` and `external/<repo>+`, which both miss under Bzlmod
where the directory carries the canonical repository name (e.g.
`rules_req_compile++requirements+pip_deps`); every miss runs
`bazel query @<repo>//... --output location` via a **bare** `Command::new(bazel)`
that drops the configured startup options and `--bazelrc` handling; and
`SourceHasher::resolve` calls it once per external source file.

**Measured check (confirmed).** `tools/validate_457.py` builds a
perf_workload-style fixture with 400 `@pip_deps//` source-file targets whose
files exist only under the canonical directory, plus a replay `bazel` shim
that logs every invocation and answers `mod dump_repo_mapping`:

| | baseline | patched |
|---|---|---|
| nested `query ... --output location` invocations | **400** (one per file) | **0** |
| `mod dump_repo_mapping` invocations | 0 | **1** |
| fallback queries carrying `--output_base` startup option | 0 of 400 | n/a |
| wall time, free stub bazel | 0.358 s | 0.033 s (**10.8x**) |
| wall time, 50 ms per nested invocation | 5.61 s | 0.034 s (**167x**) |
| emitted hashes | identical | identical |

The invocation counts reproduce the issue's mechanism exactly (709 nested
queries -> 0 in the reporter's repository), including the secondary claim that
the fallback ignored `--bazelStartupOptions` (all 400 baseline fallback queries
ran without the configured `--output_base`, i.e. against a different output
base in a real setup). With any realistic per-invocation Bazel cost the
baseline time scales linearly with external source files, which makes the
reported 300 s -> 2.4 s entirely consistent.

**Fix behavior.** The patch's own unit tests (repo-mapping resolution, cached
query fallback with startup options, mapping parser) pass, as does the full
lib suite (81 tests).

## Claim 2: query output is decoded on a single thread

**Code-level check (confirmed).** Pre-patch `decode_target_stream`
(src/bazel.rs) decodes one message at a time off a default 8 KiB `BufReader`,
entirely on the calling thread.

**Measured check (directionally confirmed).** PR #459's gate, baseline binary
vs patched binary, 7 interleaved rounds, scale 2 (~48k targets):

| workload | baseline | patched | speedup | wins |
|---|---|---|---|---|
| generate-hashes-small (~4.8k targets) | 0.107 s | 0.103 s | 1.04x | 71% |
| generate-hashes-large (~48k targets) | 0.480 s | 0.446 s | 1.08x | 71% |
| get-impacted-targets (150k hashes) | 0.444 s | 0.456 s | 0.98x | 36% |
| peak RSS, generate-hashes-large | 115 MB | 119 MB | 1.04 | — |

Numbers are end-to-end wall time on 4 CPUs, where decode is only a fraction of
the phase; the issue's own claim is a 1.44x improvement of the query-parse
phase alone on a 1.05 GB stream. A 4-8% end-to-end win on graphs this small is
consistent with that, and every gate run passed output parity (hashes
identical). `get-impacted-targets` does not touch the decode path and measured
within noise (an initial run suggesting a 20% regression had a 0.64 s stdev
from machine contention; a clean 11-round run landed at 0.98x). Peak RSS is
unchanged within 4%, matching the claim that batching (8192 messages / 32 MB)
bounds memory. The patch's order-preservation and truncation tests pass.

## Verdict

Both root-cause claims are accurate at the code level, both fixes behave as
described (400 -> 0 nested Bazel invocations; parallel batched decode with
stream order preserved), emitted hashes are byte-identical before and after on
every workload measured, and no regression was found in the diff paths the
patches do not touch. The dramatic absolute numbers in the issue (300 s -> 2.4 s)
are attributable to the per-file nested Bazel invocations, which this
validation reproduces mechanically.

Reproduce with:

```
cargo build --release                        # patched tree
python3 tools/validate_457.py \
    --baseline-binary <pre-patch bazel-diff> \
    --candidate-binary target/release/bazel-diff \
    --external-sources 400 --rounds 3 --bazel-latency 0.05
python3 tools/perf_gate.py \
    --kotlin-binary <pre-patch bazel-diff> \
    --rust-binary target/release/bazel-diff \
    --rounds 7 --warmup-rounds 2 --scale 2 \
    --min-speedup 0.5 --min-logic-speedup 0.5 --min-win-rate 0
```
