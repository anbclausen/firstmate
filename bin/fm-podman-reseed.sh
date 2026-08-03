#!/usr/bin/env bash
# fm-podman-reseed.sh - re-seed the primary's CURRENT Claude credentials into a
# LIVE podman crewmate container, without tearing the task down.
#
# Why: a podman crewmate copies the primary's ~/.claude at container-create
# time (bin/backends/podman.sh's fm_backend_podman_seed_credentials). Those
# credentials expire, so a long-lived container eventually holds a login the
# agent can no longer refresh and freezes at "Login expired - Run /login". Full
# teardown + respawn also fixes it, but throws away the worktree and the
# agent's session; this re-credentials the container in place.
#
# Usage:
#   fm-podman-reseed.sh <id>             re-seed credentials only
#   fm-podman-reseed.sh <id> --restart   re-seed, then restart the agent in the
#                                        container's pane using the launch
#                                        command recorded at spawn (meta
#                                        launch=). The restarted agent re-reads
#                                        its brief and starts a fresh session -
#                                        use it for a worker that never
#                                        authenticated, not for one mid-task.
#
# Refuses loudly rather than half-working: a task that is not podman-backed, a
# container that is not running, or a credential copy that cannot be verified
# byte-for-byte against the primary's own file all stop with a diagnostic.
# Reads the credentials from $HOME at call time, so it always seeds what the
# primary holds NOW.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-$(cd "$SCRIPT_DIR/.." && pwd)}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"
STATE="${FM_STATE_OVERRIDE:-$FM_HOME/state}"

# shellcheck source=bin/backends/podman.sh
. "$SCRIPT_DIR/backends/podman.sh"

ID=${1:-}
RESTART=${2:-}
[ -n "$ID" ] || { echo "usage: fm-podman-reseed.sh <id> [--restart]" >&2; exit 2; }
case "$RESTART" in ''|--restart) : ;; *) echo "usage: fm-podman-reseed.sh <id> [--restart]" >&2; exit 2 ;; esac

META="$STATE/$ID.meta"
[ -f "$META" ] || { echo "error: no metadata for '$ID' at $META" >&2; exit 1; }

meta_value() {  # <key>
  grep "^$1=" "$META" 2>/dev/null | tail -1 | cut -d= -f2- || true
}

BACKEND=$(meta_value backend)
[ "$BACKEND" = podman ] || { echo "error: task '$ID' runs on backend '${BACKEND:-tmux}', not podman - credential seeding is a podman-only mechanism" >&2; exit 1; }

NAME=$(meta_value podman_container)
[ -n "$NAME" ] || { echo "error: task '$ID' has no recorded container name" >&2; exit 1; }

fm_backend_podman_tool_check || exit 1
fm_backend_podman_running "$NAME" || { echo "error: podman container '$NAME' is not running - re-seeding needs a live container; respawn the task instead" >&2; exit 1; }

fm_backend_podman_seed_credentials "$NAME" || exit 1
echo "fm-podman-reseed: re-seeded current credentials into '$NAME'"

[ "$RESTART" = --restart ] || exit 0

LAUNCH=$(meta_value launch)
[ -n "$LAUNCH" ] || { echo "error: task '$ID' recorded no launch command, so the agent cannot be restarted here; steer the worker to restart its own agent, or respawn the task" >&2; exit 1; }

TARGET=$(meta_value window)
[ -n "$TARGET" ] || { echo "error: task '$ID' has no recorded endpoint to restart the agent in" >&2; exit 1; }

# Exit the stale agent (C-c twice is every verified harness's interrupt-then-
# quit shape at a frozen login prompt), then re-run the recorded launch line in
# the pane's own cwd, which is already the task worktree.
fm_backend_podman_send_key "$TARGET" C-c || true
sleep 0.5
fm_backend_podman_send_key "$TARGET" C-c || true
sleep 0.5
fm_backend_podman_send_text_line "$TARGET" "$LAUNCH" \
  || { echo "error: could not send the launch command to '$TARGET'" >&2; exit 1; }
echo "fm-podman-reseed: relaunched the agent in '$NAME'"
