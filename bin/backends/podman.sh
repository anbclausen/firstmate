#!/usr/bin/env bash
# bin/backends/podman.sh - the podman session-provider adapter (EXPERIMENTAL,
# NEVER auto-detected).
#
# Design: Phase 1 of the podman-containerization rearchitecture. Unlike every
# other session-provider adapter (tmux/herdr/zellij/cmux), podman's endpoint
# is not a terminal-multiplexer pane but a purpose-built, least-privilege
# LINUX CONTAINER that isolates the agent process from the host - the whole
# point captain asked for. To avoid reinventing pty capture/composer-state
# detection from scratch, this adapter runs a tiny tmux server INSIDE each
# container and drives it with `podman exec`, reusing the already-verified
# tmux capture/send/composer primitives (bin/fm-tmux-lib.sh) at one remove.
# The container is the sandbox boundary; tmux inside it is just this
# adapter's own event-source plumbing, invisible to every other backend.
#
# Target string shape: "<container_name>@@<tmux_target>" - "@@" (not ":",
# which tmux targets themselves already use for session:window) separates
# the two layers unambiguously. Sets FM_BACKEND_PODMAN_CONTAINER and
# FM_BACKEND_PODMAN_TMUX_TARGET.
#
# One container PER TASK, named "fm-<home>-<label>" via the shared
# fm_backend_hometag helper (mirrors cmux's/zellij's home-scoped naming so
# two firstmate homes sharing one podman machine cannot collide).
#
# OPEN ASSUMPTION (flagged for captain confirmation, docs/podman-backend.md
# "Open questions"): worktree acquisition still runs `treehouse get` INSIDE
# the container's tmux pane, exactly like the cmux/zellij/herdr generic
# fm-spawn.sh flow, which means the project clone's directory (containing
# .git and the treehouse lease state) must be bind-mounted read-write at
# spawn time so treehouse can create the worktree lease from inside the
# sandbox. Once `treehouse get` completes, the pane's cwd moves into the
# freshly created worktree subdirectory, which is what the crewmate agent
# actually works in - but the container also had transient read-write reach
# into the rest of the project clone during that step. Tightening this so
# the container only ever sees the FINAL worktree path (host-side worktree
# acquisition before container start, mirroring Orca's own-worktree model)
# is real follow-up work, not done here.
#
# Requires: podman (CLI). No jq dependency (podman's `inspect`/`ps --format`
# use Go templates directly, no JSON adapter needed for the fields this
# backend reads).

FM_BACKEND_PODMAN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FM_ROOT="${FM_ROOT_OVERRIDE:-${FM_ROOT:-$FM_BACKEND_PODMAN_ROOT}}"
FM_HOME="${FM_HOME:-${FM_ROOT_OVERRIDE:-$FM_ROOT}}"

# shellcheck source=bin/fm-backend-hometag-lib.sh
. "$FM_BACKEND_PODMAN_ROOT/bin/fm-backend-hometag-lib.sh"
# shellcheck source=bin/fm-composer-lib.sh
. "$FM_BACKEND_PODMAN_ROOT/bin/fm-composer-lib.sh"

# Fixed in-container mount point and tmux session name - deterministic, so
# (unlike zellij's/cmux's active pwd-probe) this adapter never needs to poll
# for where the worktree landed once treehouse get finishes; it always lands
# under FM_BACKEND_PODMAN_MOUNT.
FM_BACKEND_PODMAN_MOUNT=${FM_BACKEND_PODMAN_MOUNT:-/work}
FM_BACKEND_PODMAN_TMUX_SESSION=${FM_BACKEND_PODMAN_TMUX_SESSION:-work}

# Standard-profile Containerfiles (used when the target project has no
# Containerfile of its own). docs/configuration.md "Container profiles" owns
# the tracked location and profile-selection contract.
FM_BACKEND_PODMAN_CONTAINERS_DIR="${FM_BACKEND_PODMAN_ROOT}/containers"
FM_BACKEND_PODMAN_DEV_PROFILE="dev"
FM_BACKEND_PODMAN_SCOUT_PROFILE="scout"

# Ownership label applied to EVERY container and image this adapter creates
# (containers at `podman run`, images at `podman build`, redundantly with the
# `LABEL firstmate.managed=true` baked into containers/*.Containerfile so a
# project's own Containerfile-built image is labeled here too, since firstmate
# does not control that file's contents). This is the ONE filter the garbage
# collector (fm_backend_podman_gc below) is scoped by - it must never touch a
# container or image lacking this exact label, so a captain's own unrelated
# podman resources on the same machine are never at risk. `firstmate.home`
# additionally scopes to the resolved FM_ROOT so two firstmate homes sharing
# one podman machine can tell their own resources apart if that is ever
# needed; the GC sweep itself is fleet-wide (every firstmate.managed=true
# resource, not just this home's), matching how orphaned containers can be
# safely reclaimed regardless of which home originally spawned them.
FM_BACKEND_PODMAN_LABEL="firstmate.managed=true"

# fm_backend_podman_label_args: sets the global array
# FM_BACKEND_PODMAN_LABEL_ARGS to the `--label ...` flags every managed
# `podman run`/`podman build` call must carry, expanded unquoted-safely by
# the caller with "${FM_BACKEND_PODMAN_LABEL_ARGS[@]}" (mirrors
# FM_BACKEND_PODMAN_RUN_FLAGS's array-not-string convention above).
fm_backend_podman_label_args() {
  FM_BACKEND_PODMAN_LABEL_ARGS=(--label "$FM_BACKEND_PODMAN_LABEL" --label "firstmate.home=$(fm_backend_podman_home_label)")
}

fm_backend_podman_tool_check() {
  command -v podman >/dev/null 2>&1 || { echo "error: backend=podman selected but the 'podman' CLI was not found on PATH (https://podman.io)" >&2; return 1; }
  return 0
}

# fm_backend_podman_container_ensure: podman has no per-home session/server to
# stand up (each task owns its own container, unlike tmux's/herdr's shared
# server) - this is purely the version/tool gate. Nothing to echo.
fm_backend_podman_container_ensure() {
  fm_backend_podman_tool_check || return 1
  podman info >/dev/null 2>&1 || { echo "error: backend=podman selected but 'podman info' failed - is the podman machine/service running?" >&2; return 1; }
  return 0
}

fm_backend_podman_home_label() {
  fm_backend_hometag
}

fm_backend_podman_container_name() {  # <fm-task-label>
  local label=$1 rest home
  home=$(fm_backend_podman_home_label)
  case "$label" in
    fm-*) rest=${label#fm-} ;;
    *) rest=$label ;;
  esac
  # podman container names are restricted to [a-zA-Z0-9_.-]; the home tag and
  # label are already drawn from that safe set (task ids, hometag hash), so
  # no further sanitization is done here.
  printf 'fm-%s-%s' "$home" "$rest"
}

# fm_backend_podman_profile_for: which standard profile (dev|scout) fits
# <kind> ("ship"/"scout"/"secondmate"). A scout task gets the restricted,
# read-mostly investigation profile; everything else gets the general dev
# profile. Callers pass the task KIND, not the backend name.
fm_backend_podman_profile_for() {  # <kind>
  case "$1" in
    scout) printf '%s' "$FM_BACKEND_PODMAN_SCOUT_PROFILE" ;;
    *) printf '%s' "$FM_BACKEND_PODMAN_DEV_PROFILE" ;;
  esac
}

fm_backend_podman_standard_image_tag() {  # <profile>
  printf 'fm-podman-%s:latest' "$1"
}

# fm_backend_podman_path_hash: short stable tag suffix for a project's own
# Containerfile image, so distinct projects (or the same project relocated)
# never collide or wrongly share a cached build.
fm_backend_podman_path_hash() {  # <path>
  local path=$1 real
  real=$(cd "$path" 2>/dev/null && pwd -P) || real=$path
  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "$real" | shasum -a 256 | awk '{print substr($1,1,12)}'
  elif command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$real" | sha256sum | awk '{print substr($1,1,12)}'
  else
    printf '%s' "$real" | cksum | awk '{printf "%08x", $1}'
  fi
}

# fm_backend_podman_image_for: resolve (building if needed) the image tag to
# run the task's container from. A project-owned Containerfile at the repo
# root wins over the standard library (devcontainer-style override); its
# built image is tagged per-project so a Containerfile edit is picked up by
# `podman build`'s own layer-cache invalidation on the next spawn, never
# reused stale.
# Both branches build a SHARED, CACHED image tag reused by every future task
# for the same project (project branch) or the same profile (standard-library
# branch), never a disposable per-task tag - so ordinary task teardown never
# removes an image, only the garbage collector's dangling-image prune
# (fm_backend_podman_gc) ever reclaims stale layers left behind by a rebuild.
fm_backend_podman_image_for() {  # <proj_abs> <kind> -> prints image tag
  local proj_abs=$1 kind=$2 cf="$1/Containerfile" tag profile cprofile
  fm_backend_podman_label_args
  if [ -f "$cf" ]; then
    tag="fm-podman-proj-$(fm_backend_podman_path_hash "$proj_abs")"
    podman build -q -t "$tag" "${FM_BACKEND_PODMAN_LABEL_ARGS[@]}" -f "$cf" "$proj_abs" >&2 || { echo "error: podman build failed for project Containerfile at $cf" >&2; return 1; }
    printf '%s' "$tag"
    return 0
  fi
  profile=$(fm_backend_podman_profile_for "$kind")
  tag=$(fm_backend_podman_standard_image_tag "$profile")
  cprofile="$FM_BACKEND_PODMAN_CONTAINERS_DIR/$profile.Containerfile"
  [ -f "$cprofile" ] || { echo "error: no Containerfile at $cf and no standard profile at $cprofile" >&2; return 1; }
  if ! podman image exists "$tag" 2>/dev/null; then
    podman build -q -t "$tag" "${FM_BACKEND_PODMAN_LABEL_ARGS[@]}" -f "$cprofile" "$FM_BACKEND_PODMAN_CONTAINERS_DIR" >&2 || { echo "error: podman build failed for standard profile '$profile' ($cprofile)" >&2; return 1; }
  fi
  printf '%s' "$tag"
}

# FM_BACKEND_PODMAN_RUN_FLAGS: the least-privilege flag set shared by every
# profile - no --privileged, no added capabilities, no new-privileges
# escalation. An array (not a function that prints a string) so callers can
# expand it unquoted-safely with "${FM_BACKEND_PODMAN_RUN_FLAGS[@]}" instead
# of word-splitting a command substitution. Individual profiles add their own
# mount/network posture on top (fm_backend_podman_create_task).
FM_BACKEND_PODMAN_RUN_FLAGS=(
  '--cap-drop=ALL'
  '--security-opt=no-new-privileges'
  '--pids-limit=512'
)

# fm_backend_podman_create_task: start the task's container from the
# resolved image, mounting the project clone read-write at
# FM_BACKEND_PODMAN_MOUNT (see the file header's "OPEN ASSUMPTION" - treehouse
# get needs write access there to create the worktree lease), then start the
# in-container tmux session. Refuses an existing live container of the same
# name (mirrors every other adapter's duplicate check). Echoes the container
# name on success; the caller composes the full "<name>@@<tmux_target>"
# target string once the tmux session is confirmed up.
fm_backend_podman_create_task() {  # <label> <proj_abs> <kind>
  local label=$1 proj_abs=$2 kind=$3 name image profile net_flag
  name=$(fm_backend_podman_container_name "$label")
  if podman container exists "$name" 2>/dev/null; then
    echo "error: podman container '$name' already exists" >&2
    return 1
  fi
  image=$(fm_backend_podman_image_for "$proj_abs" "$kind") || return 1
  profile=$(fm_backend_podman_profile_for "$kind")
  fm_backend_podman_label_args
  FM_BACKEND_PODMAN_LABEL_ARGS+=(--label "firstmate.task=$label")
  if [ "$profile" = "$FM_BACKEND_PODMAN_SCOUT_PROFILE" ]; then
    # Investigation profile: read-only project mount plus a small rw scratch
    # dir, and no network by default - least privilege for read-mostly work.
    net_flag="--network=none"
    if ! podman run -d --name "$name" "${FM_BACKEND_PODMAN_RUN_FLAGS[@]}" "${FM_BACKEND_PODMAN_LABEL_ARGS[@]}" \
      --read-only \
      "$net_flag" \
      -v "$proj_abs:$FM_BACKEND_PODMAN_MOUNT:ro" \
      --tmpfs "$FM_BACKEND_PODMAN_MOUNT/.fm-scratch:rw,size=512m" \
      --tmpfs /tmp \
      "$image" sleep infinity >/dev/null 2>&1; then
      echo "error: podman run failed to start scout container '$name'" >&2
      return 1
    fi
  else
    # General dev/coding profile: the project clone (and thus the worktree
    # treehouse creates under it) is read-write; nothing else of the host is
    # mounted. Network stays on (bridge) - a coding task needs registries/gh.
    if ! podman run -d --name "$name" "${FM_BACKEND_PODMAN_RUN_FLAGS[@]}" "${FM_BACKEND_PODMAN_LABEL_ARGS[@]}" \
      -v "$proj_abs:$FM_BACKEND_PODMAN_MOUNT:rw" \
      --tmpfs /tmp \
      "$image" sleep infinity >/dev/null 2>&1; then
      echo "error: podman run failed to start dev container '$name'" >&2
      return 1
    fi
  fi
  if ! podman exec -d "$name" tmux new-session -d -s "$FM_BACKEND_PODMAN_TMUX_SESSION" -c "$FM_BACKEND_PODMAN_MOUNT" 2>/dev/null; then
    echo "error: failed to start the in-container tmux session for '$name' (does the image have tmux installed?)" >&2
    podman rm -f "$name" >/dev/null 2>&1 || true
    return 1
  fi
  printf '%s' "$name"
}

# fm_backend_podman_parse_target: split "<container>@@<tmux_target>". Sets
# FM_BACKEND_PODMAN_CONTAINER and FM_BACKEND_PODMAN_TMUX_TARGET.
fm_backend_podman_parse_target() {  # <target>
  local target=$1
  FM_BACKEND_PODMAN_CONTAINER=${target%%@@*}
  FM_BACKEND_PODMAN_TMUX_TARGET=${target#*@@}
  [ -n "$FM_BACKEND_PODMAN_CONTAINER" ] && [ -n "$FM_BACKEND_PODMAN_TMUX_TARGET" ] && [ "$FM_BACKEND_PODMAN_TMUX_TARGET" != "$target" ]
}

fm_backend_podman_exec() {  # <container> <tmux-args...>
  local name=$1
  shift
  podman exec "$name" tmux "$@"
}

fm_backend_podman_running() {  # <container>
  [ "$(podman inspect -f '{{.State.Running}}' "$1" 2>/dev/null)" = "true" ]
}

# fm_backend_podman_target_ready: is the container running AND does its
# in-container tmux session still exist? Structural check only, never a
# content read (mirrors zellij's/cmux's pane_exists posture).
fm_backend_podman_target_ready() {  # <target> [expected-label]
  fm_backend_podman_parse_target "$1" || return 1
  fm_backend_podman_running "$FM_BACKEND_PODMAN_CONTAINER" || return 1
  fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" has-session -t "$FM_BACKEND_PODMAN_TMUX_SESSION" >/dev/null 2>&1
}

# fm_backend_podman_current_path: always FM_BACKEND_PODMAN_MOUNT - the bind
# mount point is fixed at container-create time, so (unlike zellij/cmux)
# this needs no active pwd-marker probe. fm-spawn.sh's worktree-detection
# poll still calls this each iteration; it settles on the FIRST call because
# the value never depends on treehouse having finished cd'ing yet.
fm_backend_podman_current_path() {  # <target> [expected-label]
  fm_backend_podman_target_ready "$1" "${2:-}" || return 0
  printf '%s' "$FM_BACKEND_PODMAN_MOUNT"
}

fm_backend_podman_capture() {  # <target> <lines> [expected-label]
  fm_backend_podman_target_ready "$1" "${3:-}" || return 1
  local lines=${2:-200}
  case "$lines" in ''|*[!0-9]*) lines=200 ;; esac
  fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" capture-pane -p -t "$FM_BACKEND_PODMAN_TMUX_TARGET" -S -"$lines" 2>/dev/null
}

fm_backend_podman_send_literal() {  # <target> <text> [expected-label]
  fm_backend_podman_target_ready "$1" "${3:-}" || return 1
  fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" send-keys -t "$FM_BACKEND_PODMAN_TMUX_TARGET" -l "$2" >/dev/null 2>&1
}

fm_backend_podman_normalize_key() {  # <key>
  case "$1" in
    Enter|enter) printf 'Enter' ;;
    Escape|escape|Esc|esc) printf 'Escape' ;;
    C-c|c-c|ctrl+c|Ctrl+c|Ctrl+C|ctrl-c) printf 'C-c' ;;
    *) printf '%s' "$1" ;;
  esac
}

fm_backend_podman_send_key() {  # <target> <key> [expected-label]
  fm_backend_podman_target_ready "$1" "${3:-}" || return 1
  local key
  key=$(fm_backend_podman_normalize_key "$2")
  fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" send-keys -t "$FM_BACKEND_PODMAN_TMUX_TARGET" "$key" >/dev/null 2>&1
}

fm_backend_podman_send_text_line() {  # <target> <text> [expected-label]
  fm_backend_podman_target_ready "$1" "${3:-}" || return 1
  fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" send-keys -t "$FM_BACKEND_PODMAN_TMUX_TARGET" "$2" Enter >/dev/null 2>&1
}

# fm_backend_podman_composer_state: reuse the shared bordered/bare composer
# classifier (bin/fm-composer-lib.sh) against a plain-text capture, exactly
# like the cmux adapter (no ANSI-style channel over `podman exec ... capture-pane -p`).
FM_BACKEND_PODMAN_COMPOSER_LINES=${FM_BACKEND_PODMAN_COMPOSER_LINES:-20}
FM_BACKEND_PODMAN_IDLE_RE=${FM_BACKEND_PODMAN_IDLE_RE:-'^[>%$#]\s*$'}

fm_backend_podman_composer_state() {  # <target> [expected-label] -> empty|pending|unknown
  local target=$1 expected_label=${2:-} cap line trimmed stripped="" found=0
  cap=$(fm_backend_podman_capture "$target" "$FM_BACKEND_PODMAN_COMPOSER_LINES" "$expected_label") || { printf 'unknown'; return 0; }
  while IFS= read -r line; do
    trimmed="${line#"${line%%[![:space:]]*}"}"
    trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
    [ -n "$trimmed" ] || continue
    case "$trimmed" in
      '│'*'│'|'┃'*'┃'|'|'*'|') : ;;
      *) continue ;;
    esac
    stripped=$trimmed
    found=1
  done < <(printf '%s\n' "$cap")
  [ "$found" -eq 1 ] || { printf 'unknown'; return 0; }
  stripped=${stripped//│/}
  stripped=${stripped//┃/}
  stripped=${stripped//|/}
  stripped="${stripped#"${stripped%%[![:space:]]*}"}"
  stripped="${stripped%"${stripped##*[![:space:]]}"}"
  fm_composer_classify_content 1 "$stripped" "$FM_BACKEND_PODMAN_IDLE_RE"
}

fm_backend_podman_send_text_submit() {  # <target> <text> <retries> <enter-sleep> <settle> [expected-label]
  local target=$1 text=$2 retries=$3 sleep_s=$4 settle=$5 expected_label=${6:-} i=0 state
  fm_backend_podman_parse_target "$target" || { printf 'unknown'; return 0; }
  fm_backend_podman_send_literal "$target" "$text" "$expected_label" || { printf 'send-failed'; return 0; }
  sleep "$settle"
  while :; do
    fm_backend_podman_send_key "$target" Enter "$expected_label" || true
    sleep "$sleep_s"
    state=$(fm_backend_podman_composer_state "$target" "$expected_label")
    [ "$state" = pending ] || { printf '%s' "$state"; return 0; }
    i=$((i + 1))
    [ "$i" -lt "$retries" ] || { printf 'pending'; return 0; }
  done
}

# fm_backend_podman_kill: stop and remove the task's container, best-effort
# (mirrors every other backend's `kill` `|| true` contract). Reclaims the
# whole sandbox in one step - unlike the multiplexer backends there is no
# separate "session" to leave behind.
fm_backend_podman_kill() {  # <target> [unused] [expected-label]
  local expected_label=${3:-} name
  if [ -n "$expected_label" ]; then
    fm_backend_podman_target_ready "$1" "$expected_label" || return 0
    name=$FM_BACKEND_PODMAN_CONTAINER
  else
    fm_backend_podman_parse_target "$1" || return 0
    name=$FM_BACKEND_PODMAN_CONTAINER
  fi
  podman stop -t 5 "$name" >/dev/null 2>&1 || true
  podman rm -f "$name" >/dev/null 2>&1 || true
}

# fm_backend_podman_list_live: recovery/orphan discovery. Lists every
# container whose name is scoped to this firstmate home, by NAME (podman
# container names/ids are stable across a podman machine restart, unlike
# cmux's workspace uuids, so no id-vs-label reconciliation is needed here).
# One "<name>@@<session>\tfm-<id>" line per live task container.
fm_backend_podman_list_live() {
  local home prefix names name plain
  home=$(fm_backend_podman_home_label)
  prefix="fm-$home-"
  names=$(podman ps -a --format '{{.Names}}' 2>/dev/null) || return 0
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    case "$name" in
      "$prefix"*) : ;;
      *) continue ;;
    esac
    plain=${name#"$prefix"}
    [ -n "$plain" ] || continue
    printf '%s@@%s\tfm-%s\n' "$name" "$FM_BACKEND_PODMAN_TMUX_SESSION" "$plain"
  done <<EOF
$names
EOF
}

# fm_backend_podman_agent_alive: best-effort CONFIDENT liveness of the
# harness process inside the container's tmux pane, mirroring tmux's own
# `pane_current_command` classifier (fm_backend_tmux_agent_alive) at one
# remove through `podman exec`.
fm_backend_podman_agent_alive() {  # <target>
  local target=$1 comm
  fm_backend_podman_target_ready "$target" || { printf 'unknown'; return 0; }
  comm=$(fm_backend_podman_exec "$FM_BACKEND_PODMAN_CONTAINER" display-message -p -t "$FM_BACKEND_PODMAN_TMUX_TARGET" '#{pane_current_command}' 2>/dev/null) || { printf 'unknown'; return 0; }
  comm=${comm#-}
  case "$comm" in
    '') printf 'unknown' ;;
    *claude*|*codex*|*opencode*|*grok*) printf 'alive' ;;
    zsh|bash|sh|dash|ash|ksh|mksh|tcsh|csh|fish) printf 'dead' ;;
    *) printf 'unknown' ;;
  esac
}

# --- garbage collection -------------------------------------------------
#
# Firstmate's podman footprint is entirely label-scoped (fm_backend_podman_label_args
# above), which is what makes this sweep safe: every filter below reads
# `label=firstmate.managed=true` FIRST, so a captain's own unrelated podman
# containers/images on the same machine are structurally excluded, never
# just "unlikely to match". This is the single owner of that scoped cleanup;
# bin/fm-bootstrap.sh's PODMAN_GC detect-only check calls it and only decides
# cadence/reporting shape, never its own filtering.

# fm_backend_podman_gc_orphan_containers: remove STOPPED (exited/dead)
# firstmate-managed containers older than FM_BACKEND_PODMAN_GC_GRACE_SECS
# (default 600s - long enough that a container mid-teardown is never raced).
# A RUNNING firstmate-managed container is always a live task and is never
# touched here; ordinary teardown (fm_backend_podman_kill, called through
# fm-teardown.sh's generic fm_backend_kill dispatch) already stops+removes
# its own container immediately, so this only reclaims orphans left behind
# by a crash, a `kill -9`, or a skipped/failed teardown. Prints the count
# removed.
FM_BACKEND_PODMAN_GC_GRACE_SECS=${FM_BACKEND_PODMAN_GC_GRACE_SECS:-600}
fm_backend_podman_gc_orphan_containers() {
  local now id finished_at finished_epoch age removed=0
  now=$(date +%s)
  while IFS= read -r id; do
    [ -n "$id" ] || continue
    finished_at=$(podman inspect -f '{{.State.FinishedAt}}' "$id" 2>/dev/null) || continue
    finished_epoch=$(date -j -f '%Y-%m-%dT%H:%M:%S' "${finished_at%%.*}" +%s 2>/dev/null) \
      || finished_epoch=$(date -d "$finished_at" +%s 2>/dev/null) \
      || finished_epoch=0
    age=$(( now - finished_epoch ))
    [ "$finished_epoch" -eq 0 ] || [ "$age" -ge "$FM_BACKEND_PODMAN_GC_GRACE_SECS" ] || continue
    podman rm -f "$id" >/dev/null 2>&1 && removed=$((removed + 1))
  done < <(podman ps -a --filter "label=$FM_BACKEND_PODMAN_LABEL" --filter status=exited --filter status=dead --format '{{.ID}}' 2>/dev/null)
  printf '%s' "$removed"
}

# fm_backend_podman_gc_dangling_images: prune DANGLING (untagged) images
# carrying the firstmate label - leftovers from an image rebuild
# (fm_backend_podman_image_for always reuses the SAME tag, so a rebuild's old
# layers become dangling, never a live tagged image). `podman image prune`'s
# own `--filter label=...` scoping means a tagged, in-use image (project or
# standard-profile) is never a candidate regardless of age. Prints the count
# removed.
fm_backend_podman_gc_dangling_images() {
  local out
  out=$(podman image prune -f --filter "label=$FM_BACKEND_PODMAN_LABEL" 2>/dev/null) || { printf '0'; return 0; }
  printf '%s' "$out" | grep -c '^[0-9a-f]\{12,64\}$' || printf '0'
}

# fm_backend_podman_gc: run both sweeps, echo "<containers_removed> <images_removed>".
fm_backend_podman_gc() {
  fm_backend_podman_tool_check >/dev/null 2>&1 || { printf '0 0'; return 1; }
  local c i
  c=$(fm_backend_podman_gc_orphan_containers)
  i=$(fm_backend_podman_gc_dangling_images)
  printf '%s %s' "${c:-0}" "${i:-0}"
}
