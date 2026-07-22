# Podman runtime backend

EXPERIMENTAL, spawn-capable, session-provider-only (like herdr/zellij/cmux;
treehouse remains the worktree provider). NEVER auto-detected - unlike herdr
and cmux, podman owns a differently-isolated (containerized) execution
context that must always be an explicit choice: `--backend podman`,
`FM_BACKEND=podman`, or a local `config/backend` file containing `podman`.

Adapter: `bin/backends/podman.sh`. Wired into `bin/fm-backend.sh` (known/
spawn-capable lists, per-op dispatch) and `bin/fm-spawn.sh` (container
creation, worktree-detection, and meta recording) exactly like every other
experimental backend.

## Why a container

Every other backend's endpoint is a terminal-multiplexer pane on the host.
podman's endpoint is a purpose-built Linux container: the crewmate agent
process runs sandboxed, with least-privilege flags (no `--privileged`, no
added capabilities, `no-new-privileges`, only the task's project mounted -
see "Container profiles" below), instead of sharing the host's process
namespace and filesystem the way a tmux pane does.

## Adapter design: tmux-in-a-container

To reuse firstmate's already-verified pty capture/composer-state detection
instead of reinventing it, each podman container runs a tiny internal tmux
server (`podman exec <container> tmux ...`), and this adapter drives that
inner tmux session the same way `bin/backends/tmux.sh` drives a host pane -
just one `podman exec` away. The container is the sandbox boundary; the
inner tmux session is invisible plumbing specific to this adapter.

Target string shape: `"<container_name>@@<tmux_target>"`. `@@` (not `:`,
which the inner tmux target already uses for `session:window`) separates the
two layers unambiguously. `fm_backend_podman_parse_target` splits it into
`FM_BACKEND_PODMAN_CONTAINER` and `FM_BACKEND_PODMAN_TMUX_TARGET`.

One container per task, named `fm-<home-label>-<id>` via the same shared
`fm_backend_hometag` helper cmux and zellij use, so two firstmate homes
sharing one podman machine cannot collide.

Because the project clone is bind-mounted at the fixed path
`FM_BACKEND_PODMAN_MOUNT` (default `/work`) at container-create time, this
adapter's `fm_backend_podman_current_path` needs no active pwd-marker probe
the way zellij's/cmux's equivalents do - the mount point is known up front.

## Container profiles

`fm_backend_podman_image_for` resolves which image to run the task's
container from, in this order:

1. **Project override**: if the spawned project's repo root has its own
   `Containerfile`, that is built (tagged `fm-podman-proj-<hash-of-abs-path>`)
   and used - a devcontainer-style escape hatch for a project with unusual
   dependencies.
2. **Standard profile**: otherwise, one of two tracked profiles under
   `containers/` at the firstmate repo root, selected by the task's kind
   (`fm_backend_podman_profile_for`):
   - `containers/dev.Containerfile` (tag `fm-podman-dev:latest`) - the
     general coding profile, for ship tasks.
   - `containers/scout.Containerfile` (tag `fm-podman-scout:latest`) - the
     restricted investigation profile, for scout tasks.

Both images are Debian-slim with `tmux`, `git`, and `ca-certificates`
installed, running as a non-root `agent` user. Neither bakes in a specific
agent harness binary (claude/codex/opencode/pi/grok) - harness
installation/auth inside the container is out of scope for this phase (see
"Open questions").

Least-privilege **run** flags (`fm_backend_podman_create_task`), shared by
every profile: `--cap-drop=ALL`, `--security-opt=no-new-privileges`,
`--pids-limit=512`, no `--privileged`, no added capabilities.

Profile-specific mount/network posture on top of that shared set:

| Profile | Root FS | Project mount | Network | Scratch |
|---|---|---|---|---|
| dev | read-write (image default) | `rw` at `/work` | default bridge (registries, `gh`, etc.) | n/a |
| scout | `--read-only` | `ro` at `/work` | `--network=none` | `/work/.fm-scratch` tmpfs, 512m, `rw` |

## Open questions (flagged for captain confirmation)

- **Worktree acquisition scope**: `fm-spawn.sh` sends `treehouse get` into
  the container's inner tmux pane exactly like the cmux/zellij/herdr generic
  flow, which requires the PROJECT CLONE (not just the eventual worktree) to
  be bind-mounted read-write at spawn time, since treehouse needs to see the
  clone's `.git` and its own lease state to create the worktree. This means
  a dev-profile container has transient read-write reach into the whole
  project clone, not only the final worktree it ends up working in.
  Tightening this to mount only the pre-acquired final worktree path (host-side
  acquisition before container start, mirroring Orca's own-worktree model
  instead of Orca's approach) is real follow-up work, not implemented here.
- **Base image choice**: `debian:12-slim` was chosen for a small, generally
  available base with an easy `tmux`/`git` install; no captain preference was
  available to consult. Confirm or swap.
- **Scout capability set**: the scout profile's exact restriction shape
  (`--read-only` root, `ro` project mount, `--network=none`, one 512m tmpfs
  scratch dir) is this task's own least-privilege judgment call, not a
  captain-specified spec. Confirm the network-off default is acceptable for
  every investigation task (a scout that needs to fetch a URL or run
  `gh`/`git fetch` will fail until network is re-enabled for that task).
- **Harness bootstrap inside the container**: neither profile installs or
  authenticates an agent harness; how the launched harness binary and its
  credentials reach the container (baked into a captain-maintained image,
  mounted read-only, or installed at container-create time) is unresolved.
- **`--secondmate` spawns**: not supported yet, matching cmux/orca's own
  current restriction.

## Garbage collection

Long-running use of `backend=podman` would otherwise accumulate disk usage
from two sources: orphaned per-task containers left behind by a crash or a
skipped/failed teardown, and stale image layers left behind whenever a
project's or a standard profile's `Containerfile` is rebuilt under the same
reused tag. Both are reclaimed automatically, scoped so a captain's own
unrelated podman containers/images on the same machine are never touched.

**Label scheme**: every container and image this adapter creates carries
`--label firstmate.managed=true` plus `--label firstmate.home=<home-tag>`
(containers additionally carry `--label firstmate.task=<fm-id>`). This is
applied at `podman run`/`podman build` time
(`fm_backend_podman_label_args`) and is redundantly baked into
`containers/dev.Containerfile`/`containers/scout.Containerfile` via `LABEL`
so it survives even a manual rebuild. A project's own `Containerfile` does
not carry the `LABEL` itself, but the adapter's `--label` build flag applies
regardless of the Containerfile's own contents. **Every** cleanup filter
below reads `label=firstmate.managed=true` first - this is the single
mechanism that keeps the sweep from ever matching a resource it did not
create.

**Teardown-time cleanup** (`fm_backend_podman_kill`, called through
`bin/fm-teardown.sh`'s generic `fm_backend_kill` dispatch exactly like every
other backend): stops and removes the task's specific container
immediately. Images are never removed at teardown - both the project-override
image and each standard profile's image are SHARED, CACHED tags reused by
every future task for that project/profile, never a disposable per-task
build, so there is no task-specific image to remove at this point.

**Periodic sweep** (`fm_backend_podman_gc`, two independent pieces):

- `fm_backend_podman_gc_orphan_containers` - removes STOPPED
  (`exited`/`dead`) firstmate-labeled containers older than
  `FM_BACKEND_PODMAN_GC_GRACE_SECS` (default 600s/10min - long enough that a
  container mid-teardown is never raced). A RUNNING firstmate-labeled
  container is always a live task and is never touched.
- `fm_backend_podman_gc_dangling_images` - runs `podman image prune -f
  --filter label=firstmate.managed=true`, which only ever matches DANGLING
  (untagged) images - leftovers from a `Containerfile` rebuild reusing the
  same tag, since the adapter always overwrites in place rather than
  minting a new tag per build. A currently-tagged, in-use image is never a
  candidate regardless of age.

**Wiring and cadence**: `bin/fm-bootstrap.sh`'s `podman_gc` function calls
`fm_backend_podman_gc` at most once per `FM_PODMAN_GC_INTERVAL_SECS`
(default 86400s/24h), tracked in a durable `state/.podman-gc-last` timestamp
marker, and only when the `podman` binary is actually installed - it is one
of the MUTATING sweeps skipped under `FM_BOOTSTRAP_DETECT_ONLY=1` (a
lock-refused read-only session must never mutate podman state any more than
it mutates fleet-sync or secondmate state).

**Reporting**: removing anything prints an actionable
`PODMAN_GC: removed <n> orphaned container(s) and <n> dangling image(s)
(label=firstmate.managed=true)` line (see
`.agents/skills/bootstrap-diagnostics/SKILL.md` - this line is informational
only, never a problem requiring a response). Finding nothing to reclaim
stays silent by default, matching the rest of bootstrap's "nothing to do"
facts, and only prints a `BOOTSTRAP_INFO:` line under
`FM_BOOTSTRAP_VERBOSE_FACTS=1`.

Firstmate never runs a bare `podman system prune` or any unscoped destructive
command; every removal call carries the `label=firstmate.managed=true`
filter, so a captain's own unrelated podman containers and images on the
same machine are structurally excluded rather than merely unlikely to match.

## Event-source mapping

Reuses `bin/fm-tmux-lib.sh`'s verified capture/composer-state contract at one
remove (`podman exec ... tmux capture-pane`, `... tmux send-keys`). Busy
state and native push events are not implemented (`fm_backend_busy_state`
and `fm_backend_has_push` both fall through to `unknown`/no-push for
podman, exactly like every backend other than herdr), so `fm-watch.sh`'s
ordinary poll loop is podman's event source, synthesizing signal/stale/check
wakes from capture + composer-state polling like the tmux reference path.

`fm_backend_podman_agent_alive` mirrors `fm_backend_tmux_agent_alive`'s
`pane_current_command` classifier through `podman exec ... tmux
display-message`, but is not yet in `fm_backend_agent_alive`'s verified set
(`bin/fm-backend.sh`) - the session-start secondmate-liveness sweep reports
podman endpoints as `unknown` until independently verified, matching
zellij/Orca/cmux's own current status.

## Requires

`podman` on `PATH`, plus a reachable `podman info` (i.e. a running podman
machine/service). No `jq` dependency - this adapter reads `podman
inspect`/`ps --format` Go templates directly rather than JSON.
