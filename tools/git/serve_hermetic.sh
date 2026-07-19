#!/usr/bin/env bash
# Launches the query service via //tools/git:bazel-diff-hermetic-git, the
# bazel-diff binary with the hermetic git injected into its runfiles:
#
#   bazel run //tools/git:serve -- -w /path/to/clone --cacheDir /path/to/cache
#
# The first argument is the runfiles path of that binary, injected by the BUILD
# file; everything after it is forwarded to `bazel-diff serve`. Git resolution
# happens inside the tool (ServeCommand.resolveGitPath): an explicit --gitPath
# in the forwarded arguments wins over the injected hermetic git.

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

bazel_diff="$(rlocation "$1")"
shift

exec "$bazel_diff" serve "$@"
