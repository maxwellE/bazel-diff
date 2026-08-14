#!/usr/bin/env python3
"""Validate issue #457's external-repository resolution fix.

Issue #457 reports that ``generate-hashes`` with
``--fineGrainedHashExternalRepos`` spends minutes resolving external
repositories under Bzlmod: ``ExternalRepoResolver::resolve`` guesses
``external/<repo>`` and ``external/<repo>+``, both of which miss when the
directory is named after the *canonical* repository name (for example
``rules_req_compile++requirements+pip_deps``), and every miss shells out to
``bazel query @<repo>//... --output location`` -- once per external source
file, serialized on the Bazel server lock. The proposed fix memoizes
resolution per repository and answers the canonical name from one
``bazel mod dump_repo_mapping`` invocation.

This harness reproduces that exact shape hermetically, in the style of
:mod:`perf_workload`:

* a main-repo graph plus N source-file targets in ``@pip_deps``, whose files
  live only under ``external/rules_req_compile++requirements+pip_deps`` --
  the canonical directory neither of the resolver's guesses finds;
* a replay ``bazel`` shim that logs every invocation, answers
  ``mod dump_repo_mapping`` with the apparent -> canonical mapping, and
  answers the per-repo ``query ... --output location`` fallback with a
  location line inside the canonical directory (optionally after a sleep, to
  model the cost of a real nested Bazel invocation);
* both binaries run over identical bytes; the harness reports wall time, the
  number of nested ``query`` fallbacks, the number of ``dump_repo_mapping``
  calls, whether the fallback carried the configured startup options, and
  whether the two binaries emitted identical hashes.

Example::

    python3 tools/validate_457.py \\
        --baseline-binary bins/bazel-diff-baseline \\
        --candidate-binary bins/bazel-diff-patched \\
        --external-sources 400 --rounds 3 --bazel-latency 0.05
"""

from __future__ import annotations

import argparse
import json
import shlex
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Sequence

from perf_workload import (
    GraphSpec,
    REPLAY_BAZEL_VERSION,
    TARGET_SOURCE_FILE,
    encode_delimited,
    encode_graph,
    encode_source_file,
    encode_target,
    encode_varint,
    write_workspace,
)

APPARENT_REPO = "pip_deps"
CANONICAL_REPO = "rules_req_compile++requirements+pip_deps"


def external_source_label(index: int) -> str:
    return f"@{APPARENT_REPO}//:src_{index:05d}.txt"


def external_source_file(index: int) -> str:
    return f"src_{index:05d}.txt"


def write_fixture(path: Path, spec: GraphSpec, external_sources: int, canonical_dir: Path) -> Path:
    """Write the main graph plus the ``@pip_deps`` source-file targets."""
    graph = encode_graph(spec)
    messages = []
    for index in range(external_sources):
        messages.append(
            encode_target(
                TARGET_SOURCE_FILE,
                encode_source_file(
                    external_source_label(index),
                    location=f"{canonical_dir / 'BUILD'}:{index + 1}:1",
                ),
            )
        )
    path.write_bytes(graph + encode_delimited(messages))
    return path


def write_external_repo(output_base: Path, external_sources: int) -> Path:
    """Materialize the canonical repository directory the resolver must find."""
    canonical = output_base / "external" / CANONICAL_REPO
    canonical.mkdir(parents=True, exist_ok=True)
    (canonical / "BUILD").write_text("# synthetic external package\n")
    for index in range(external_sources):
        (canonical / external_source_file(index)).write_text(
            f"external source {index}\n" * 4
        )
    return canonical


def write_logging_bazel(
    path: Path, fixture: Path, output_base: Path, log: Path, latency: float
) -> Path:
    """A replay ``bazel`` that also logs argv and answers the #457 code paths.

    Beyond :func:`perf_workload.write_replay_bazel` it answers:

    * ``mod dump_repo_mapping`` -- the apparent -> canonical mapping (the fixed
      resolver's one-invocation path); every other ``mod`` subcommand still
      exits non-zero so neither binary takes the module-graph path;
    * ``query ... --output location`` -- the per-repository fallback the
      unfixed resolver issues once per external source file, answered with a
      location line inside the canonical directory. ``--bazel-latency`` adds a
      sleep here and only here, modelling the real cost of a nested Bazel
      invocation serializing on the server lock.
    """
    canonical = output_base / "external" / CANONICAL_REPO
    location_line = (
        f"{canonical / external_source_file(0)}:1:1: "
        f"source file {external_source_label(0)}"
    )
    script = f"""#!/usr/bin/env bash
set -euo pipefail
fixture={shlex.quote(str(fixture))}
output_base={shlex.quote(str(output_base))}
log={shlex.quote(str(log))}
latency={shlex.quote(str(latency))}
echo "$*" >> "$log"
args=("$@")
command=
command_index=-1
for index in "${{!args[@]}}"; do
  case "${{args[$index]}}" in
    version|mod|info|query)
      command="${{args[$index]}}"
      command_index=$index
      break
      ;;
  esac
done
case "$command" in
  version)
    echo 'Build label: {REPLAY_BAZEL_VERSION}'
    ;;
  mod)
    subcommand="${{args[$((command_index + 1))]:-}}"
    if [[ "$subcommand" == "dump_repo_mapping" ]]; then
      echo '{{"": "_main", "{APPARENT_REPO}": "{CANONICAL_REPO}"}}'
    else
      exit 1
    fi
    ;;
  info)
    echo "$output_base"
    ;;
  query)
    if [[ " $* " == *" --output location "* ]]; then
      if [[ "$latency" != "0" && "$latency" != "0.0" ]]; then
        sleep "$latency"
      fi
      echo {shlex.quote(location_line)}
      exit 0
    fi
    output_file=
    for ((index = command_index + 1; index < ${{#args[@]}}; index++)); do
      case "${{args[$index]}}" in
        --output_file)
          ((index += 1))
          output_file="${{args[$index]}}"
          ;;
        --output_file=*)
          output_file="${{args[$index]#--output_file=}}"
          ;;
      esac
    done
    if [[ -n "$output_file" ]]; then
      cp "$fixture" "$output_file"
    else
      cat "$fixture"
    fi
    ;;
  *)
    echo "unsupported replay invocation: $*" >&2
    exit 2
    ;;
esac
"""
    path.write_text(script)
    path.chmod(0o755)
    return path


def run_once(
    binary: Path,
    workspace: Path,
    shim: Path,
    output_base: Path,
    log: Path,
    output: Path,
) -> dict:
    """One ``generate-hashes`` run; returns timing, invocation counts, hashes."""
    log.write_text("")
    command = [
        str(binary),
        "generate-hashes",
        "-w",
        str(workspace),
        "-b",
        str(shim),
        f"--bazelStartupOptions=--output_base={output_base}",
        f"--fineGrainedHashExternalRepos={APPARENT_REPO}",
        "--excludeExternalTargets",
        str(output),
    ]
    start = time.perf_counter()
    completed = subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    elapsed = time.perf_counter() - start
    if completed.returncode != 0:
        tail = "\n".join(completed.stderr.splitlines()[-20:])
        raise RuntimeError(f"{binary} failed ({completed.returncode}):\n{tail}")
    invocations = log.read_text().splitlines()
    fallback_queries = [
        line for line in invocations if " --output location" in f" {line}"
    ]
    return {
        "seconds": elapsed,
        "invocations": len(invocations),
        "nested_queries": len(fallback_queries),
        "nested_queries_with_startup_options": sum(
            1 for line in fallback_queries if f"--output_base={output_base}" in line
        ),
        "repo_mapping_dumps": sum(
            1 for line in invocations if "mod dump_repo_mapping" in line
        ),
        "hashes": json.loads(output.read_text()),
    }


def summarize(rounds: list[dict]) -> dict:
    seconds = [entry["seconds"] for entry in rounds]
    constant_keys = (
        "invocations",
        "nested_queries",
        "nested_queries_with_startup_options",
        "repo_mapping_dumps",
    )
    summary = {
        "rounds": len(rounds),
        "median_seconds": round(statistics.median(seconds), 4),
        "min_seconds": round(min(seconds), 4),
        "max_seconds": round(max(seconds), 4),
    }
    for key in constant_keys:
        values = {entry[key] for entry in rounds}
        if len(values) != 1:
            raise RuntimeError(f"{key} varied across rounds: {sorted(values)}")
        summary[key] = values.pop()
    return summary


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-binary", type=Path, required=True)
    parser.add_argument("--candidate-binary", type=Path, required=True)
    parser.add_argument(
        "--external-sources",
        type=int,
        default=400,
        help="source-file targets in @pip_deps (default: 400)",
    )
    parser.add_argument(
        "--packages",
        type=int,
        default=50,
        help="main-repo packages in the graph (default: 50)",
    )
    parser.add_argument(
        "--rounds", type=int, default=3, help="measured runs per binary (default: 3)"
    )
    parser.add_argument(
        "--bazel-latency",
        type=float,
        default=0.0,
        help="seconds each nested query fallback sleeps, modelling a real "
        "Bazel server invocation (default: 0)",
    )
    parser.add_argument("--json", type=Path, help="write the full report as JSON")
    return parser.parse_args(argv)


def main(argv: Sequence[str]) -> int:
    args = parse_args(argv)
    spec = GraphSpec(packages=args.packages)
    with tempfile.TemporaryDirectory(prefix="bazel-diff-457-") as temp:
        root = Path(temp)
        workspace = write_workspace(root / "workspace", spec)
        output_base = (root / "output-base").resolve()
        canonical = write_external_repo(output_base, args.external_sources)
        fixture = write_fixture(
            root / "targets.pb", spec, args.external_sources, canonical
        )
        log = root / "bazel-invocations.log"
        shim = write_logging_bazel(
            root / "bazel", fixture.resolve(), output_base, log, args.bazel_latency
        )
        binaries = {
            "baseline": args.baseline_binary.resolve(),
            "candidate": args.candidate_binary.resolve(),
        }
        results: dict[str, list[dict]] = {name: [] for name in binaries}
        hashes: dict[str, dict] = {}
        for round_index in range(args.rounds):
            order = (
                list(binaries) if round_index % 2 == 0 else list(reversed(binaries))
            )
            for name in order:
                outcome = run_once(
                    binaries[name],
                    workspace.resolve(),
                    shim,
                    output_base,
                    log,
                    root / f"hashes-{name}-{round_index}.json",
                )
                hashes[name] = outcome.pop("hashes")
                results[name].append(outcome)

    report = {
        "workload": {
            "packages": spec.packages,
            "main_targets": spec.target_count,
            "external_sources": args.external_sources,
            "apparent_repo": APPARENT_REPO,
            "canonical_repo": CANONICAL_REPO,
            "bazel_latency_seconds": args.bazel_latency,
        },
        "results": {name: summarize(rounds) for name, rounds in results.items()},
        "hashes_identical": hashes["baseline"] == hashes["candidate"],
    }
    baseline = report["results"]["baseline"]
    candidate = report["results"]["candidate"]
    if candidate["median_seconds"] > 0:
        report["median_speedup"] = round(
            baseline["median_seconds"] / candidate["median_seconds"], 2
        )

    print(json.dumps(report, indent=2))
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
    if not report["hashes_identical"]:
        print("FAIL: baseline and candidate emitted different hashes", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
