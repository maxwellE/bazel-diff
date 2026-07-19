#!/usr/bin/env bash
# End-to-end test of the hermetic-git injection.
#
# Starts //tools/git:bazel-diff-hermetic-git `serve` against a local clone
# while a poisoned `git` shim shadows the PATH. ServeCommand only reports
# healthy after the startup `git fetch` succeeds, so a 200 from /health proves
# the server resolved and used the injected hermetic git rather than the PATH
# one (which would have exited non-zero and lame-ducked the instance).

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

bazel_diff="$(rlocation "$1")"
git_install_dir="$(rlocation "$2")"
GIT="$git_install_dir/bin/git"
[[ -x "$bazel_diff" ]] || fail "bazel-diff-hermetic-git not found at $bazel_diff"
[[ -x "$GIT" ]] || fail "hermetic git not found at $GIT"

# Isolate from any host-level git configuration.
export HOME="$TEST_TMPDIR/home"
mkdir -p "$HOME"
export GIT_CONFIG_NOSYSTEM=1
export XDG_CONFIG_HOME="$HOME/xdg"
export GIT_AUTHOR_NAME=smoke GIT_AUTHOR_EMAIL=smoke@example.com
export GIT_COMMITTER_NAME=smoke GIT_COMMITTER_EMAIL=smoke@example.com

# A remote and a clone for the server to fetch in, created with the hermetic
# git itself.
remote="$TEST_TMPDIR/remote"
clone="$TEST_TMPDIR/clone"
"$GIT" init -q -b main "$remote"
echo one > "$remote/f.txt"
"$GIT" -C "$remote" add f.txt
"$GIT" -C "$remote" commit -q -m "c1"
"$GIT" clone -q --no-local "$remote" "$clone"

# Poison `git` on the PATH: if the server shells out to the PATH git instead of
# the injected one, its startup fetch fails and readiness never flips.
shim_dir="$TEST_TMPDIR/shim"
mkdir -p "$shim_dir"
cat > "$shim_dir/git" <<'EOF'
#!/bin/sh
echo "poisoned PATH git invoked" >&2
exit 97
EOF
chmod +x "$shim_dir/git"
export PATH="$shim_dir:$PATH"

log="$TEST_TMPDIR/serve.log"
"$bazel_diff" serve \
  --workspacePath "$clone" \
  --cacheDir "$TEST_TMPDIR/cache" \
  --port 0 \
  > "$log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT

# Wait for the ready message, which is only printed after the initial fetch
# succeeded: "[Info] initial git fetch complete; serving on port <port>".
port=""
for _ in $(seq 1 240); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$log" >&2
    fail "server exited before becoming ready"
  fi
  port="$(sed -n 's/.*initial git fetch complete; serving on port \([0-9][0-9]*\).*/\1/p' "$log" | head -1)"
  if [[ -n "$port" ]]; then
    break
  fi
  sleep 0.5
done
if [[ -z "$port" ]]; then
  cat "$log" >&2
  fail "server did not become ready in time"
fi

if grep -q "poisoned PATH git invoked" "$log"; then
  cat "$log" >&2
  fail "server invoked the PATH git instead of the injected hermetic git"
fi

exec 3<>"/dev/tcp/127.0.0.1/$port" || fail "cannot connect to port $port"
printf 'GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' >&3
status_line="$(head -n 1 <&3)"
exec 3<&- 3>&-
[[ "$status_line" == *" 200 "* ]] || fail "unexpected /health response: $status_line"

echo "PASS"
