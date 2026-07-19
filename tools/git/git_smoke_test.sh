#!/usr/bin/env bash
# Smoke test for the hermetic git built by //tools/git:git.
#
# Exercises exactly the git operations serve's ProcessGitClient performs
# (cli/src/main/kotlin/com/bazel_diff/server/GitClient.kt): clone, broad
# `fetch --all --prune`, targeted `fetch <remote> <sha>` (the fetchRevision
# path), `rev-parse --verify <rev>^{commit}`, and `-c gc.auto=0 checkout
# --force`. Only the hermetic binary is invoked; the test also asserts the
# binary is the pinned version and resolves its exec path inside its own
# install tree (i.e. RUNTIME_PREFIX relocation works in the runfiles tree).

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

fail() {
  echo >&2 "FAIL: $*"
  exit 1
}

git_install_dir="$(rlocation "$1")"
GIT="$git_install_dir/bin/git"
[[ -x "$GIT" ]] || fail "hermetic git not found at $GIT"

# Isolate from any host-level git configuration.
export HOME="$TEST_TMPDIR/home"
mkdir -p "$HOME"
export GIT_CONFIG_NOSYSTEM=1
export XDG_CONFIG_HOME="$HOME/xdg"
export GIT_AUTHOR_NAME=smoke GIT_AUTHOR_EMAIL=smoke@example.com
export GIT_COMMITTER_NAME=smoke GIT_COMMITTER_EMAIL=smoke@example.com

version_output="$("$GIT" version)"
echo "$version_output"
[[ "$version_output" == *"git version 2.55.0"* ]] || \
  fail "unexpected version: $version_output"

# RUNTIME_PREFIX must resolve the exec path inside the runfiles install tree;
# anything else means git would look for its helpers in a compiled-in sandbox
# path that no longer exists.
exec_path="$("$GIT" --exec-path)"
[[ "$exec_path" == "$git_install_dir"/* ]] || \
  fail "exec path $exec_path escapes install tree $git_install_dir"

remote="$TEST_TMPDIR/remote"
clone="$TEST_TMPDIR/clone"

"$GIT" init -q -b main "$remote"
echo one > "$remote/f.txt"
"$GIT" -C "$remote" add f.txt
"$GIT" -C "$remote" commit -q -m "c1"

# --no-local forces the real transport (spawning upload-pack) instead of the
# same-filesystem hardlink shortcut, matching how fetches behave later.
"$GIT" clone -q --no-local "$remote" "$clone"

echo two > "$remote/f.txt"
"$GIT" -C "$remote" commit -q -am "c2"
sha2="$("$GIT" -C "$remote" rev-parse HEAD)"

# ProcessGitClient.fetch
"$GIT" -C "$clone" fetch --all --prune --quiet
# ProcessGitClient.resolveSha
resolved="$("$GIT" -C "$clone" rev-parse --verify "$sha2^{commit}")"
[[ "$resolved" == "$sha2" ]] || fail "resolved $resolved, expected $sha2"
# ProcessGitClient.checkout
"$GIT" -C "$clone" -c gc.auto=0 checkout --force -q "$sha2"
[[ "$(cat "$clone/f.txt")" == "two" ]] || fail "checkout did not materialize c2"

# ProcessGitClient.fetchRevision: a targeted fetch by SHA of a commit that a
# broad fetch never delivers because it is reachable only from a ref outside
# the default refspec (refs/pull/*, like a GitHub PR-head), permitted via
# uploadpack.allowReachableSHA1InWant.
"$GIT" -C "$remote" config uploadpack.allowReachableSHA1InWant true
"$GIT" -C "$remote" checkout -q --detach
echo three > "$remote/f.txt"
"$GIT" -C "$remote" commit -q -am "c3"
sha3="$("$GIT" -C "$remote" rev-parse HEAD)"
"$GIT" -C "$remote" update-ref refs/pull/1/head "$sha3"
"$GIT" -C "$remote" checkout -q -f main
"$GIT" -C "$clone" fetch --all --prune --quiet
if "$GIT" -C "$clone" rev-parse --verify --quiet "$sha3^{commit}"; then
  fail "broad fetch unexpectedly delivered the refs/pull-only commit"
fi
"$GIT" -C "$clone" fetch --quiet origin "$sha3"
resolved3="$("$GIT" -C "$clone" rev-parse --verify "$sha3^{commit}")"
[[ "$resolved3" == "$sha3" ]] || fail "targeted fetch did not deliver $sha3"
"$GIT" -C "$clone" -c gc.auto=0 checkout --force -q "$sha3"
[[ "$(cat "$clone/f.txt")" == "three" ]] || fail "checkout did not materialize c3"

echo "PASS"
