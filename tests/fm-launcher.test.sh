#!/usr/bin/env bash
# tests/fm-launcher.test.sh - fake-podman-CLI tests for the host-side `fm`
# launcher (tui/fm), covering its already-running decision: detect a live
# session, ask the captain, and either kill it and start or leave it alone.
# Mirrors tests/fm-backend-podman.test.sh's fakebin/command-log convention.
# The launcher keeps no session state of its own, so the fake podman carries
# the only state these cases need: whether a container named fm-tui is
# running, and whether `rm -f` can actually stop it.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

LAUNCHER="$ROOT/tui/fm"
assert_present "$LAUNCHER" "tui/fm launcher is missing"

# make_podman_fakebin: a `podman` stub logging every invocation (one line,
# unit-separated args, to $FM_PODMAN_LOG). `ps` reports a running fm-tui only
# while $FM_PODMAN_RUNNING names an existing file, and `rm` deletes that file
# unless FM_PODMAN_RM_STICKS is set - the survived-kill case.
make_podman_fakebin() {  # <dir> -> echoes fakebin dir
  local fb="$1/fakebin"
  mkdir -p "$fb"
  cat > "$fb/podman" <<'SH'
#!/usr/bin/env bash
set -u
LOG="${FM_PODMAN_LOG:?}"
{
  for a in "$@"; do printf '\x1f%s' "$a"; done
  printf '\n'
} >> "$LOG"

case "${1:-}" in
  ps)
    [ -f "${FM_PODMAN_RUNNING:?}" ] && printf 'fm-tui\n'
    exit 0
    ;;
  rm)
    [ -n "${FM_PODMAN_RM_STICKS:-}" ] || rm -f "${FM_PODMAN_RUNNING:?}"
    exit 0
    ;;
  image)
    # `image exists` succeeds, so no case builds the runtime image.
    exit 0
    ;;
  info)
    printf '/run/podman/podman.sock\n'
    exit 0
    ;;
  machine)
    # macOS-only path; answer as an existing, running machine.
    case "${4:-}" in
      *Running*) printf 'true\n' ;;
      *Name*) printf 'podman-machine-default\n' ;;
    esac
    exit 0
    ;;
  *) exit 0 ;;
esac
SH
  chmod +x "$fb/podman"
  printf '%s\n' "$fb"
}

# run_launcher <running|stopped> <stdin-as-printf-%b> -> sets OUT and CODE and
# leaves the invocation log at $DIR/podman.log. Installs the launcher the way
# install.sh does, since the repo copy carries an unsubstituted repo-root
# placeholder. FM_PODMAN_RM_STICKS is read from the caller's environment.
run_launcher() {
  local state=$1 reply=$2 fb
  DIR=$(fm_test_tmproot fm-launcher-case)
  fb=$(make_podman_fakebin "$DIR")

  mkdir -p "$DIR/repo/tui/target/release"
  : > "$DIR/repo/tui/target/release/firstmate-tui"
  sed "s|__FM_REPO_ROOT__|$DIR/repo|" "$LAUNCHER" > "$DIR/fm"
  chmod +x "$DIR/fm"

  : > "$DIR/podman.log"
  [ "$state" = running ] && : > "$DIR/running"

  OUT=$(printf '%b' "$reply" | env \
    PATH="$fb:$PATH" HOME="$DIR/home" \
    FM_PODMAN_LOG="$DIR/podman.log" FM_PODMAN_RUNNING="$DIR/running" \
    FM_PODMAN_RM_STICKS="${FM_PODMAN_RM_STICKS:-}" \
    "$DIR/fm" 2>&1)
  CODE=$?
}

log_calls() { tr $'\x1f' ' ' < "$DIR/podman.log" 2>/dev/null; }

launched() { log_calls | grep -q '^ run '; }
killed() { log_calls | grep -q '^ rm '; }

# --- no session running -------------------------------------------------------

run_launcher stopped ''
expect_code 0 "$CODE" "launcher should start when no session is running"
launched || fail "launcher must start the TUI when no session is running"
assert_not_contains "$OUT" "already running" \
  "launcher must not prompt when no session is running"
pass "no running session: starts without asking"

# --- running, captain answers yes ---------------------------------------------

run_launcher running 'y\n'
expect_code 0 "$CODE" "launcher should start after the captain approves the kill"
assert_contains "$OUT" "already running" "launcher must report the live session"
assert_contains "$OUT" "Kill it and start a new one? [y/N]" \
  "launcher must ask before killing a live session"
log_calls | grep -q '^ rm -f fm-tui' || fail "approved kill must stop the live session"
launched || fail "launcher must start the new session after an approved kill"
pass "running session, yes: kills the old session and starts a new one"

# --- running, captain answers no ----------------------------------------------

run_launcher running 'n\n'
expect_code 1 "$CODE" "declining must exit non-zero: no session was started"
assert_contains "$OUT" "leaving the running session in place" \
  "declining must say the live session was left alone"
! killed || fail "declining must not touch the live session"
! launched || fail "declining must not start a second session"
pass "running session, no: leaves it alone and starts nothing"

# --- running, no answer at all ------------------------------------------------

run_launcher running ''
expect_code 1 "$CODE" "an unanswered prompt must default to leaving the session alone"
! killed || fail "an unanswered prompt must not kill the live session"
! launched || fail "an unanswered prompt must not start a second session"
pass "running session, no answer: defaults to leaving it alone"

# --- running, approved kill does not take ------------------------------------

FM_PODMAN_RM_STICKS=1
run_launcher running 'y\n'
unset FM_PODMAN_RM_STICKS
expect_code 1 "$CODE" "a session surviving the kill must stop the launch"
assert_contains "$OUT" "could not stop the running session" \
  "a surviving session must be reported, not silently joined"
! launched || fail "launcher must never start beside a session that survived the kill"
pass "running session survives the kill: refuses to start a second one"
