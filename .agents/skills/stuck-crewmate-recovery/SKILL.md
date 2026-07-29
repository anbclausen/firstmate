---
name: stuck-crewmate-recovery
description: >-
  Agent-only playbook for stuck or missing ordinary Firstmate direct reports.
  Use when the session-start digest reports an ordinary direct report's endpoint dead or its metadata has no window, or after a stale wake, looping pane, repeated confusion, an answered-by-brief question, an unresponsive crewmate, or a failed steer.
  Reconciles recorded work before escalating from targeted inspection through safe relaunch or failure.
user-invocable: false
metadata:
  internal: true
---

# stuck-crewmate-recovery

Use this playbook when the session-start digest reports an ordinary direct report's endpoint dead or its metadata has no window, or when a direct report is stale, looping, repeatedly confused, asking a question its brief already answers, unresponsive, or when a steer failed to land.

Load `harness-adapters` before sending an interrupt, exit command, resume command, or harness-specific skill invocation.
The target window's harness is recorded as `harness=` in `state/<id>.meta`.

## Session-start reconciliation for a dead ordinary direct report

This procedure covers ordinary `kind=ship` and `kind=scout` direct reports.
Load `secondmate-provisioning` instead for `kind=secondmate` recovery.

Treat the digest's endpoint result as a presence signal, not proof that the task's work or validation run is gone.
Read the targeted current state with `bin/fm-crew-state.sh <id>` before deciding to relaunch.
A no-mistakes run matched to the crew's branch and current code remains authoritative when the endpoint is dead: handle a terminal or parked run through the normal lifecycle, and keep supervising an active run instead of creating a duplicate worker.

When no authoritative run accounts for the task, inspect only its recorded backend and worktree inventory.
Use `treehouse status` for treehouse-backed tmux, herdr, zellij, or cmux tasks, and use the recorded `orca_worktree_id=` and `terminal=` for Orca tasks.
Do not sweep another home's endpoints or infer ownership from a matching window label.

Before relaunch, prove that no live agent still owns the recorded task and that the existing worktree remains available.
Preserve its uncommitted changes and commits, keep the same task identity, and resume or relaunch the recorded harness in that existing worktree with the same brief plus a concise progress note.
Do not use a fresh generic spawn while the recorded worktree is unaccounted for, because allocating another worktree can split one task across two copies.
If the worktree or ownership cannot be reconciled safely, leave all state intact and report the task failed or blocked with the conflicting evidence.

## Judge liveness by activity, not by a status string

A pane's footer or status line is cosmetic and can be stale for a long time after the condition it names has cleared.
A claude crewmate showing `Not logged in`, `Login expired`, or a spend-limit notice is not evidence that the crewmate is dead or credential-blocked.
Judge liveness only by activity: a running spinner, advancing token counts, new tool calls, fresh file edits in the worktree, or the harness's busy signature.
An inactive pane whose worktree shows recent edits is a crewmate that was working moments ago, not a wedged one.

If the pane really is credential-blocked, that is fixable inside the fleet without the captain: copy the primary's `$HOME/.claude/.credentials.json` into the worker's container with `podman cp`, then restart the worker process.
A running instance caches its credentials at startup, so the copy alone changes nothing until the restart.

Firstmate can always nudge or steer an already-running crewmate, because `fm-send` messages and key sends are not blocked.
Firstmate cannot type a `--dangerously-skip-permissions` launch into a pane, so an autonomous relaunch goes through `bin/fm-spawn.sh` or a one-liner the captain runs.
Plan recovery around that boundary rather than escalating a steerable crewmate to the captain.

## Live-endpoint escalation

Escalate in order:

1. Peek the pane and read its activity, per the section above.
2. If the crewmate looks parked but was recently working, send a plain nudge line via `FM_HOME=<this-firstmate-home> bin/fm-send.sh` and re-check activity before anything heavier.
   This is the first action for a parked claude crewmate; only continue down this list if it is still inert after the nudge.
3. If the crewmate is waiting on a question its brief already answers, answer in one line via `FM_HOME=<this-firstmate-home> bin/fm-send.sh` from an active firstmate session unless `FM_HOME` is already set to the active firstmate home.
4. If the crewmate is confused or looping, interrupt with the adapter's interrupt key, then redirect with one corrective line.
   For example, for a single-Escape adapter: `FM_HOME=<this-firstmate-home> bin/fm-send.sh <window> --key Escape`.
5. If the crewmate is genuinely wedged after redirection, exit the agent with the adapter's exit command and relaunch with the same brief plus a `progress so far` note appended to it.
   Genuine wedging means looping, unresponsive, repeating the same obstacle, or truly dead.
   A low context reading is not wedging; modern harnesses auto-compact and keep going.
   The worktree and commits persist, so relaunch is cheap.
6. If a second relaunch fails too, write `failed` to the backlog and tell the captain the plain failure, preserved work, and consequence using `AGENTS.md` section 9; do not mention metadata, harness, window, or worktree unless the path itself is needed for action.
