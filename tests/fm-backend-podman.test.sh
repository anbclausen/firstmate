#!/usr/bin/env bash
# tests/fm-backend-podman.test.sh - fake-podman-CLI unit tests for the podman
# session-provider adapter (bin/backends/podman.sh). Mirrors
# tests/fm-backend-cmux.test.sh's/tests/fm-backend-zellij.test.sh's
# fakebin/command-log convention with a smaller surface: podman's own CLI is
# not JSON, so this stubs plain text/Go-template-shaped output rather than
# reusing the jq-response-file harness those two suites use. No real-binary
# smoke test exists yet (unlike cmux/zellij) - this is Phase 1 unit coverage
# only; a real-podman smoke pass is follow-up work.
set -u

# shellcheck source=tests/lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TMP_ROOT=$(fm_test_tmproot fm-backend-podman-tests)

# make_podman_fakebin: a `podman` stub that logs every invocation (one line,
# unit-separated args, to $FM_PODMAN_LOG) and answers a handful of
# subcommands from env-configured canned responses, mirroring the other
# experimental adapters' fakebin convention.
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
  info) exit "${FM_PODMAN_FAKE_INFO_EXIT:-0}" ;;
  inspect)
    printf '%s\n' "${FM_PODMAN_FAKE_INSPECT:-true}"
    exit 0
    ;;
  ps)
    printf '%b' "${FM_PODMAN_FAKE_PS:-}"
    exit 0
    ;;
  image)
    if [ "${2:-}" = exists ]; then
      exit "${FM_PODMAN_FAKE_IMAGE_EXISTS_EXIT:-1}"
    fi
    if [ "${2:-}" = prune ]; then
      printf '%b' "${FM_PODMAN_FAKE_PRUNE:-}"
      exit 0
    fi
    exit 0
    ;;
  container)
    [ "${2:-}" = exists ] && exit "${FM_PODMAN_FAKE_CONTAINER_EXISTS_EXIT:-1}"
    exit 0
    ;;
  exec)
    exit "${FM_PODMAN_FAKE_EXEC_EXIT:-0}"
    ;;
  run|build|stop|rm)
    exit "${FM_PODMAN_FAKE_RUN_EXIT:-0}"
    ;;
  *) exit 0 ;;
esac
SH
  chmod +x "$fb/podman"
  printf '%s\n' "$fb"
}

setup_case() {  # -> sets DIR, FM_PODMAN_LOG, PATH prefix
  DIR=$(fm_test_tmproot fm-backend-podman-case)
  FB=$(make_podman_fakebin "$DIR")
  FM_PODMAN_LOG="$DIR/podman.log"
  export FM_PODMAN_LOG
  : > "$FM_PODMAN_LOG"
  PATH="$FB:$PATH"
  export PATH
  FM_ROOT_OVERRIDE="$ROOT"
  FM_HOME="$DIR/home"
  mkdir -p "$FM_HOME"
  export FM_ROOT_OVERRIDE FM_HOME
}

log_calls() { tr $'\x1f' ' ' < "$FM_PODMAN_LOG" 2>/dev/null; }

# --- tool_check / container_ensure ------------------------------------------

setup_case
# shellcheck source=bin/backends/podman.sh
. "$ROOT/bin/backends/podman.sh"

fm_backend_podman_tool_check || fail "tool_check should pass when podman is on PATH"
pass "tool_check passes with fake podman on PATH"

FM_PODMAN_FAKE_INFO_EXIT=0 fm_backend_podman_container_ensure \
  || fail "container_ensure should pass when podman info succeeds"
pass "container_ensure passes when podman info succeeds"

export FM_PODMAN_FAKE_INFO_EXIT=1
if fm_backend_podman_container_ensure 2>/dev/null; then
  fail "container_ensure should fail-closed when podman info fails"
fi
pass "container_ensure fails closed when podman info fails"
unset FM_PODMAN_FAKE_INFO_EXIT

# --- target parsing ----------------------------------------------------------

if fm_backend_podman_parse_target "fm-home-abc123@@work"; then
  [ "$FM_BACKEND_PODMAN_CONTAINER" = "fm-home-abc123" ] || fail "parse_target: wrong container '$FM_BACKEND_PODMAN_CONTAINER'"
  [ "$FM_BACKEND_PODMAN_TMUX_TARGET" = "work" ] || fail "parse_target: wrong tmux target '$FM_BACKEND_PODMAN_TMUX_TARGET'"
  pass "parse_target splits container@@tmux_target correctly"
else
  fail "parse_target should succeed on a well-formed target"
fi

fm_backend_podman_parse_target "no-separator-here" && fail "parse_target should refuse a target with no @@ separator"
pass "parse_target refuses a target missing the @@ separator"

# --- container naming ---------------------------------------------------------

name=$(fm_backend_podman_container_name "fm-abc123")
case "$name" in
  fm-*-abc123) pass "container_name scopes the label under the home tag ($name)" ;;
  *) fail "container_name unexpected shape: $name" ;;
esac

# --- profile selection ---------------------------------------------------------

[ "$(fm_backend_podman_profile_for scout)" = scout ] || fail "profile_for scout should select the scout profile"
[ "$(fm_backend_podman_profile_for ship)" = dev ] || fail "profile_for ship should select the dev profile"
[ "$(fm_backend_podman_profile_for secondmate)" = dev ] || fail "profile_for should default unknown kinds to dev"
pass "profile_for maps scout->scout and everything else->dev"

# --- run flags are least-privilege, never --privileged ------------------------

flags="${FM_BACKEND_PODMAN_RUN_FLAGS[*]}"
case "$flags" in
  *--privileged*) fail "run flags must never include --privileged" ;;
esac
case "$flags" in
  *cap-drop=ALL*) : ;;
  *) fail "run flags must drop all capabilities by default" ;;
esac
pass "shared run flags drop all capabilities and never grant --privileged"

# --- kill is best-effort (never fails the caller) ------------------------------

FM_PODMAN_FAKE_RUN_EXIT=1 fm_backend_podman_kill "fm-home-xyz@@work" \
  || fail "kill must be best-effort and never propagate a podman failure"
pass "kill swallows a failing podman stop/rm exactly like every other backend"
calls=$(log_calls)
case "$calls" in
  *"stop -t 5 fm-home-xyz"*) : ;;
  *) fail "kill should call podman stop on the parsed container name; got: $calls" ;;
esac
pass "kill targets the parsed container name"

# --- list_live is home-scoped and read-only ------------------------------------

home_tag=$(fm_backend_podman_home_label)
: > "$FM_PODMAN_LOG"
export FM_PODMAN_FAKE_PS="fm-${home_tag}-t1\nsome-unrelated-container\nfm-${home_tag}-t2\n"
out=$(fm_backend_podman_list_live)
case "$out" in
  *"fm-t1"*) : ;;
  *) fail "list_live should surface home-scoped container fm-t1; got: $out" ;;
esac
case "$out" in
  *"some-unrelated-container"*) fail "list_live must never surface a non-firstmate container" ;;
esac
pass "list_live only surfaces this home's own scoped containers"

# --- gc is label-scoped and never touches a running task -----------------------

: > "$FM_PODMAN_LOG"
FM_PODMAN_FAKE_PS="" fm_backend_podman_gc_orphan_containers >/dev/null
calls=$(log_calls)
case "$calls" in
  *"label=firstmate.managed=true"*) : ;;
  *) fail "orphan-container GC must filter podman ps by the firstmate label; got: $calls" ;;
esac
pass "orphan-container GC scopes its podman ps call to label=firstmate.managed=true"

: > "$FM_PODMAN_LOG"
fm_backend_podman_gc_dangling_images >/dev/null
calls=$(log_calls)
case "$calls" in
  *"image prune"*"label=firstmate.managed=true"*) : ;;
  *) fail "dangling-image GC must filter podman image prune by the firstmate label; got: $calls" ;;
esac
pass "dangling-image GC scopes its podman image prune call to label=firstmate.managed=true"

echo "all fm-backend-podman.test.sh checks passed"
