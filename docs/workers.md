# Workers

Workers are jobs described in JSON, or written in **AWL**, the worker
language. Drop a file into `~/.config/A.R.I.A/Config/worker/`, press `L` in
the worker manager (or run `aria worker reload`), and the daemon runs it. A
user or an agent can add work this way without touching code.

JSON says *what* to run and *when*. AWL (`*.awl`) says the same, and adds
decisions: a typed `when:` condition asked before every scheduled run, and
`on_success:` / `on_failure:` hooks that log, notify, trigger other workers
or pause. See [awl.md](awl.md).

```bash
aria worker init               # write example files (JSON and AWL) if there are no workers yet
aria worker check [files]      # type-check worker files without running them
aria worker daemon --detach    # start the daemon in the background
aria worker                    # open the worker manager (TUI)
aria worker list               # same information, as a table
aria worker run|stop|start|restart <name>
aria worker reload             # re-read the worker files (SIGHUP does the same)
aria worker shutdown           # stop every worker and the daemon
```

## Worker file

Each file is an object of `"name": { ... }`. Only `command` is required.

```json
{
  "disk-report": {
    "description": "Root filesystem usage, every hour on the hour",
    "command": "df -h /",
    "schedule": "0 * * * *",
    "timeout": "30s",
    "retries": 2,
    "catch_up": true
  },
  "ticker": {
    "command": ["my-service", "--port", "9000"],
    "schedule": "daemon",
    "restart": "on-failure"
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `command` | — | A string runs through `sh -c`. A list runs directly, with no shell. |
| `schedule` | `manual` | `on_startup`, `daemon` (keep running), `manual`, `every 5m` (1s at least), or a cron pattern (`*/5 * * * *`, `@hourly`, 6 fields for seconds). The old name `execution_time` still works. |
| `restart` | `always` | For `daemon` workers: `always`, `on-failure` or `never`. |
| `retries` | `0` | For the other schedules: how often a failed run is retried. |
| `timeout` | none | Kill the run after this long (`30s`, `5m`, `1h30m`). |
| `jitter` | none | Random extra delay of up to this long before each scheduled run. |
| `overlap` | `skip` | When a run comes due while the last one is still going: `skip` it, or `replace` the old run. |
| `catch_up` | `false` | Run once at start-up if a cron slot was missed while the daemon was down. |
| `max_restarts`, `restart_window` | `5`, `1m` | Restart intensity: past this, the worker is marked `failed`. |
| `cwd`, `env` | — | Working directory (`~/` allowed) and extra environment variables. Every run also gets `ARIA_WORKER=<name>`. |
| `enabled` | `true` | `false` keeps the worker listed but never runs it. |

## Research and the decisions it drove

The design borrows from tools with decades of production use. These are
Erlang/OTP supervisors, systemd, Kubernetes CronJobs and AWS's retry
guidance.

**1. Restart intensity, from Erlang/OTP supervisors.**
An OTP supervisor allows at most *MaxR* restarts within *MaxT* seconds. Past
that it gives up instead of restarting forever. Its defaults are 1 restart in
5 seconds, and its strategies include `one_for_one`, which restarts only the
child that failed.
→ Each worker is supervised on its own (`one_for_one`), with
`max_restarts`/`restart_window` (default 5 in 60s). A crash-looping worker
ends up `failed` and waits for a person, instead of burning CPU.
([OTP supervisor docs](https://www.erlang.org/doc/apps/stdlib/supervisor.md),
[design principles](https://www.erlang.org/docs/25/design_principles/sup_princ.html))

**2. Exponential backoff with jitter, from the AWS Architecture Blog.**
Plain exponential backoff makes clients that failed together retry together.
Adding randomness ("jitter") cuts total work and server load substantially.
→ Retries and restarts wait 1s, 2s, 4s and so on, capped at 5 minutes. Each
delay is randomised between half and all of that step. This is "equal jitter"
rather than "full jitter": the fixed half makes sure a crash loop always slows
down. The backoff resets after a run stays up for 10 seconds.
([Exponential Backoff And Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/),
[AWS Well-Architected REL05-BP03](https://docs.aws.amazon.com/en_us/wellarchitected/2022-03-31/framework/rel_mitigate_interaction_failure_limit_retries.html))

**3. Missed runs and spread-out starts, from systemd timers.**
`Persistent=true` stores the last trigger time on disk. If the machine was
off, a missed run fires once at boot. `RandomizedDelaySec=` spreads timers
out so they don't all fire at once.
→ The last start of every worker is kept in a state file. An overdue `every`
worker runs right away after a daemon restart, and `catch_up` does the same
for cron. Each case runs once, not once per missed slot. `jitter` is
`RandomizedDelaySec`.
([systemd.timer(5)](https://manpages.debian.org/buster/systemd/systemd.timer.5.en.html),
[ArchWiki: systemd timers](https://wiki.archlinux.org/title/Systemd/cron_functionality))

**4. Overlapping runs, from Kubernetes CronJobs.**
`concurrencyPolicy` decides what happens when a run comes due while the last
one is still going. `Forbid` skips it and counts it as missed. `Replace`
cancels the running job and starts a new one.
→ `overlap: skip` (the default, counted in `skipped`) and
`overlap: replace`. There is no `Allow`: two copies of the same job running
at once is rarely what anyone wants.
([Kubernetes CronJob](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/))

### Efficiency

- **Event-driven.** One small task per worker sleeps until its own next
  deadline. No loop checks every worker every second. Sleeps are capped at
  60s because tokio's timers stand still while the machine is suspended. The
  cap means a suspend/resume is noticed within a minute.
- **One thread.** The daemon only waits and starts processes, so it runs on a
  single-threaded runtime.
- **No copying of output.** A worker's stdout and stderr are its log file, so
  the kernel writes them directly. Logs over 5 MB are copied to `.1` and
  truncated in place. Workers write in append mode, so they carry on at the
  new end.
- **Bounded parallelism.** At most 2 × CPU cores scheduled runs execute at
  once. `daemon` workers don't count against that.

Measured on this machine with 25 workers: 20 jobs every 60–79s, a 30s
heartbeat, one long-running worker and one crash-looping worker. Over 60s,
with about 23 processes started, the daemon used **2 CPU ticks (0.033% of
one core)**, **1 thread** and **7.3 MB of memory**.

### Stopping, safely

Each run gets its own process group. Every stop goes through the same steps:
by hand, on timeout, on `overlap: replace` or at shutdown. The daemon sends
SIGTERM to the whole group and waits up to 5s for the run's main process to
exit. If it is still alive, the daemon sends SIGKILL to the group, then reaps
the process. A signal is only ever sent while that main process is still
unreaped, so a reused PID can't be hit by mistake. One consequence: if the
main process exits on SIGTERM but something else in its group ignores
SIGTERM, that other process keeps running.

### Limits worth knowing

- Background processes a worker leaves running after it exits normally are not
  cleaned up. Only stopping a run signals its whole group.
- If the daemon is killed with SIGKILL or crashes, its workers keep running
  without supervision. A normal exit (SIGTERM, Ctrl-C, `aria worker shutdown`)
  stops them all.
- The daemon does not start at login by itself. Add `aria worker daemon` to
  Hyprland autostart or a systemd user service.
- **Security:** anything that can write a file into the worker directory can
  run commands as you. Keep that directory owner-only. Remember that
  `aria additionals download` writes files to paths named in remote JSON.

## Layout

| File | Role |
| --- | --- |
| `src/worker/spec.rs` | File format, schedule parsing, next-run and backoff maths (pure, tested with fixed clocks); loads `*.json` and `*.awl` |
| `src/worker/awl/` | The AWL compiler and evaluator ([awl.md](awl.md)) |
| `src/worker/daemon.rs` | Supervisor, per-worker tasks, process control, control socket |
| `src/worker/ipc.rs` | JSON-lines protocol over `$XDG_RUNTIME_DIR/aria/workers.sock` (owner-only) |
| `src/worker/tui.rs` | The worker manager |

Files: definitions in `Config/worker/*.json`, logs in `Logs/worker/<name>.log`,
run history in `State/workers.json`, and the daemon's own log in
`Logs/worker-daemon.log`. All of these are under `~/.config/A.R.I.A`.
