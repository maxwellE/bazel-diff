#!/usr/bin/env bash
# Host-side driver: stages artifacts, builds the Linux benchmark image, and runs
# the cold-vs-warm benchmark + local-driver orchestrator inside a container.
#
# Prereqs (run from repo root; pick the musl config matching the docker host):
#   bazel build //release:bazel-diff --config=release-musl-arm64   # or --config=release-musl (amd64)
#   (cd tools/firecracker && GOOS=linux GOARCH="$ARCH" go build -o /tmp/bazel-diff-snap-linux .)
#
# Usage:
#   tools/firecracker/bench/run_docker_bench.sh [PKGS] [ITERS]
set -euo pipefail

PKGS=${1:-11500}
ITERS=${2:-2}
ARCH=${ARCH:-arm64}            # docker host arch (arm64 on Apple Silicon)
REPO_ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
BENCH_DIR="$REPO_ROOT/tools/firecracker/bench"

# //release:bazel-diff names the asset after the platform it was built for,
# and both docker host architectures map onto a published Linux asset.
case "$ARCH" in
  arm64|aarch64) ASSET=bazel-diff-rust-linux-arm64 ;;
  amd64|x86_64) ASSET=bazel-diff-rust-linux-amd64 ;;
  *) echo "unsupported ARCH=$ARCH (expected arm64 or amd64)"; exit 1 ;;
esac
BINARY="${BINARY:-$REPO_ROOT/bazel-bin/release/$ASSET}"
SNAP="${SNAP:-/tmp/bazel-diff-snap-linux-$ARCH}"
[ -f "$BINARY" ] || { echo "missing $BINARY — run: bazel build //release:bazel-diff --config=release-musl(-arm64)"; exit 1; }
[ -f "$SNAP" ] || { echo "missing $SNAP — cross-compile the go binary first"; exit 1; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp "$BINARY" "$STAGE/bazel-diff"
cp "$SNAP" "$STAGE/bazel-diff-snap"
cp "$BENCH_DIR/Dockerfile" "$BENCH_DIR/gen_project.py" \
   "$BENCH_DIR/bench.py" "$BENCH_DIR/run_in_container.sh" "$STAGE/"

RESULTS=${RESULTS:-"$REPO_ROOT/.bench-results"}
mkdir -p "$RESULTS"

echo "=== building image (arch=$ARCH) ==="
docker build --build-arg BAZELISK_ARCH="$ARCH" -t bazel-diff-bench "$STAGE"

echo "=== running benchmark: PKGS=$PKGS ITERS=$ITERS ==="
docker run --rm \
    -e PKGS="$PKGS" -e ITERS="$ITERS" \
    -v "$RESULTS:/results" \
    bazel-diff-bench

echo "=== results in $RESULTS ==="
cat "$RESULTS/target_count.txt" 2>/dev/null || true
cat "$RESULTS/report.json" 2>/dev/null || true
