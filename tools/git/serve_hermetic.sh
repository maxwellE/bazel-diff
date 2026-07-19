#!/usr/bin/env bash
# Launches bazel-diff's serve mode with the hermetic git built by
# //tools/git:git, so the query service does not depend on a system git:
#
#   bazel run //tools/git:serve -- -w /path/to/clone --cacheDir /path/to/cache
#
# The first two arguments are runfiles paths injected by the BUILD file (the
# git install tree and the bazel-diff launcher); everything after them is
# forwarded to `bazel-diff serve`. If the caller passes an explicit --gitPath,
# it wins: the hermetic default is then omitted so picocli does not reject the
# option as set twice.

# --- begin runfiles.bash initialization v3 ---
set -uo pipefail; set +e; f=bazel_tools/tools/bash/runfiles/runfiles.bash
# shellcheck disable=SC1090
source "${RUNFILES_DIR:-/dev/null}/$f" 2>/dev/null || \
  source "$(grep -sm1 "^$f " "${RUNFILES_MANIFEST_FILE:-/dev/null}" | cut -f2- -d' ')" 2>/dev/null || \
  source "$0.runfiles/$f" 2>/dev/null || \
  source "$(grep -sm1 "^$f " "$0.runfiles_manifest" | cut -f2- -d' ')" 2>/dev/null || \
  source "$(grep -sm1 "^$f " "$0.exe.runfiles_manifest" | cut -f2- -d' ')" 2>/dev/null || \
  { echo>&2 "ERROR: cannot find $f"; exit 1; }; f=; set -e
# --- end runfiles.bash initialization v3 ---

git_install_dir="$(rlocation "$1")"
bazel_diff="$(rlocation "$2")"
shift 2

git_bin="$git_install_dir/bin/git"
if [[ ! -x "$git_bin" ]]; then
  echo >&2 "ERROR: hermetic git not found at $git_bin"
  exit 1
fi

for arg in "$@"; do
  case "$arg" in
    --gitPath|--gitPath=*)
      exec "$bazel_diff" serve "$@"
      ;;
  esac
done
exec "$bazel_diff" serve "--gitPath=$git_bin" "$@"
