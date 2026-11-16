# AWL — the A.R.I.A Worker Language

AWL is a small language for writing workers. Its syntax is shaped like
Python: indentation, `if`/`elif`/`else`, `for x in xs`, f-strings. Its types
are checked the way Rust checks them: before anything runs, with no implicit
conversions, and with an error message that points at the exact spot.

A `.awl` file goes in the same directory as the JSON workers
(`~/.config/A.R.I.A/Config/worker/`). Both kinds can be mixed, and worker
names must be unique across all files.

```python
let quiet_hours: list[int] = [23, 0, 1, 2, 3, 4, 5, 6]

fn can_spend_power() -> bool:
    return on_ac() or battery() >= 30

worker old_downloads:
    description = "Lists downloads older than 30 days"
    run = "find ~/Downloads -type f -mtime +30 -print"
    schedule = cron("30 21 * * *")
    timeout = 2m
    retries = 1

    when:
        if hour() in quiet_hours:
            return false
        return can_spend_power()

    on_failure:
        notify("old_downloads failed", f"exit {status.exit}")
        if status.failures >= 3:
            pause()
```

```bash
aria worker check              # type-check every worker file
aria worker check my.awl       # or just some
aria worker init               # writes example.json and example.awl
```

## What a worker says

| Question | AWL |
| --- | --- |
| **What to do** | `run = "shell command"` or `run = ["program", "arg"]` |
| **When to run** | `schedule = every(5m)` / `cron("0 * * * *")` / `Schedule.daemon` / `Schedule.startup` / `Schedule.manual` |
| **Whether to run this time** | `when:` a condition, decided again before every scheduled run |
| **How long it may take** | `timeout = 30s` |
| **What to do afterwards** | `on_success:` and `on_failure:` hooks |
| **What it runs with** | `cwd = "~/project"`, `env = {"LANG": "C"}` |
| **Its status** | `status.exit`, `status.runs`, `status.failures`, `status.duration`... in `when:` and the hooks |

### Fields

| Field | Type | Default |
| --- | --- | --- |
| `run` | `str` (through `sh -c`) or `list[str]` (run directly) | required |
| `description` | `str` | |
| `schedule` | `Schedule` | `Schedule.manual` |
| `timeout`, `jitter` | `duration` | none |
| `retries` | `int` | `0` |
| `restart` | `Restart`: `Restart.always`, `Restart.on_failure`, `Restart.never` | `Restart.always` |
| `max_restarts`, `restart_window` | `int`, `duration` | `5`, `1m` |
| `overlap` | `Overlap`: `Overlap.skip`, `Overlap.replace` | `Overlap.skip` |
| `catch_up`, `enabled` | `bool` | `false`, `true` |
| `cwd` | `str` | |
| `env` | `map[str, str]` | |

These mean exactly what the JSON fields of the same name mean (see
[workers.md](workers.md)). An AWL worker is turned into the same internal
spec a JSON worker produces, so it is scheduled and supervised the same way.
Field values are computed once, when the file is loaded. They can use
constants, arithmetic and pure functions (`timeout = 2 * 15m`), but not live
state like `hour()`.

### Hooks

| Hook | Runs | May use |
| --- | --- | --- |
| `when:` | before each **scheduled** run; must give a `bool` | pure functions and live state |
| `on_success:` | after a run exits with 0 | everything, including actions |
| `on_failure:` | after a run fails or times out | everything, including actions |

`when: <condition>` on one line is short for a block that returns it. In
block form, every path must end in `return true` or `return false`; the
checker enforces that.

- **Who asks `when:`:** every scheduled run, every restart of a `daemon`
  worker, and the fresh run that `overlap = Overlap.replace` starts.
- **Who skips it:** a run started by hand (`aria worker run`, `r` or `R` in
  the TUI), a run started by another worker's `trigger()`, and the retry of a
  failed scheduled run (`retries`), which continues a run that was already
  allowed.
- When `when:` says no, that slot is used up. The run counts as `skipped`,
  `aria worker list` and the TUI show why, and the worker sleeps until its
  next slot. A `daemon` worker asks again after 30 seconds. A `startup`
  worker that is told no does not run at all.
- A runtime error in `when:` (say, an index out of range) counts as "no".
  The error goes to the worker's log with its file, line and column.
- Hooks run only after real outcomes. A run you stop or restart by hand
  does not trigger them.

### Actions (hooks only)

| Action | Effect |
| --- | --- |
| `log(msg)` | Appends `» msg` to the worker's log |
| `notify(title, body)` | Desktop notification (`notify-send`) |
| `trigger("worker")` | Runs another worker now, like `aria worker run` (its `when:` is not asked). A worker can't trigger itself, and one worker is triggered at most 10 times a minute, so workers that trigger each other can't loop at full speed. |
| `pause()` | Stops scheduling this worker until it is started again. For a `daemon` worker this also replaces the restart. |

Actions are collected while the hook runs and carried out afterwards. An
action that starts a process does so through `trigger`, so that process gets
a worker of its own: a process group, a timeout, a log, stop and shutdown.
A hook can't start a process the supervisor doesn't know about.

## The language

### Types

`int` (64-bit), `float`, `bool`, `str`, `duration`, `list[T]`,
`map[str, T]`, plus `Schedule`, `Restart`, `Overlap` and `Status`. All of
these can be written in annotations, so a function can take the status:
`fn flaky(s: Status) -> bool:`.

- **No implicit conversions.** `1 + 2.0` is an error, and so is
  `"n=" + 5`. Use `float(x)`, `int(x)`, `str(x)` or an f-string
  (`f"n={n}"`).
- **Duration literals:** `250ms`, `30s`, `5m`, `1h30m`, `2d`. A duration plus
  a duration is a duration. A duration times an `int` is a duration. A
  duration divided by a duration is an `int`.
- **Variables are immutable** unless declared `let mut`. Top-level `let`s are
  constants. An annotation is optional (`let n: int = 3`), except for an
  empty collection, which needs one: `let xs: list[str] = []`.
- **Functions** are typed: `fn name(a: int, b: str) -> bool:`. They must
  return a value on every path. They can only call functions defined above
  them, and never themselves, so every evaluation ends.
- **Effects are checked.** Each function is known to be *pure*, *probe*
  (reads live state) or *action*, including through the functions it calls.
  A probe can't appear in a field or a top-level `let`. An action can't
  appear in `when:`.

### Syntax

```python
# comments
let x = 1                      # constant (top level) / variable (in a block)
let mut n = 0                  # mutable, in a function or hook
n = n + 1
if a and not b:                # also: elif, else
    pass
for item in [1, 2, 3]:         # also: range(n), range(a, b), keys(m); break, continue
    log(str(item))
x in xs     "sub" in s     key in m     x not in xs
xs[0]   xs[-1]   m["key"]
f"took {status.duration}, exit {status.exit}"
"text".upper()                 # method syntax: same as upper("text")
```

Operators, from loosest to tightest: `or`, `and`, `not`, comparisons
(`== != < <= > >= in`, not chainable), `+ -`, `* / %`, unary `-`, then calls,
fields and indexing. Integer `/` rounds toward zero, as in Rust. Overflow,
division by zero, a bad index or a missing key is a runtime error, never a
wrong answer.

### `status`

Inside `when:` and the hooks, `status` describes the worker:

| Field | Type | In the hooks | In `when:` |
| --- | --- | --- | --- |
| `name` | `str` | the worker's name | |
| `runs` | `int` | runs started, ever | |
| `failures` | `int` | runs that failed, ever | |
| `exit` | `int` | this run's exit code (128+N when killed by signal N) | the last run's, or `-1` |
| `success` | `bool` | `exit == 0` | for the last run |
| `duration` | `duration` | how long this run took | the last run's |
| `retry` | `int` | retries used for this scheduled run | |

### Built-in functions

| Pure | |
| --- | --- |
| `str(x)`, `int(x)`, `float(x)` | conversions (`int("42")`, `int(2.9) == 2`) |
| `len(x)`, `range(n)`, `range(a, b)`, `min(a, b)`, `max(a, b)`, `abs(x)` | |
| `upper`, `lower`, `strip`, `starts_with`, `ends_with`, `contains`, `replace`, `split`, `join` | strings |
| `keys(m)`, `get(m, key, default)` | maps |
| `secs(d)`, `millis(d)` | durations → `int` |
| `every(d)`, `cron(pattern)` | build a `Schedule` |

| Live state (`when:`, hooks, functions they call) | |
| --- | --- |
| `hour()`, `minute()`, `weekday()` (0 = Monday), `day()`, `month()`, `today()`, `now()` | the clock |
| `env(name)`, `has_env(name)` | the daemon's environment |
| `file_exists(path)`, `read_file(path)` (first 64 KiB) | `~/` is expanded |
| `battery()` (-1 without one), `on_ac()` | power |
| `load()`, `cpus()`, `mem_used()` (percent), `disk_free(path)` (percent), `uptime()` | the machine |

## Errors

`aria worker check` prints every error in the file, in the form `rustc` uses:

```text
error[E0308]: mismatched types
 --> backup.awl:8:16
  |
8 |     schedule = "daily"
  |                ^^^^^^^ expected `Schedule`, found `str`
  |
  = help: use `every(5m)`, `cron("0 * * * *")`, `Schedule.daemon`, `Schedule.startup` or `Schedule.manual`

error[E0015]: `notify()` has side effects, so it can't be used in `when:`
  --> backup.awl:12:9
   |
12 |         notify("checking", "x")
   |         ^^^^^^^^^^^^^^^^^^^^^^^ not allowed here
   |
   = help: `when:` only decides whether to run; act in `on_success:` or `on_failure:`
```

Misspelled names get a "did you mean" suggestion. A file with errors does not
stop the other files from loading. It shows up in `aria worker list` as one
invalid entry, named after the file, with its first error.

Codes follow rustc where the meaning matches: E0308 mismatched types, E0369
bad operator, E0425 unknown name, E0384 assigning to an immutable variable,
E0317 a missing return, E0061 wrong arguments, E0015 an effect where it isn't
allowed, E0560 an unknown field or hook. Lexer and parser errors are E00xx
and E01xx.

## Safety limits

- A script never sleeps or starts a process itself; actions are carried out
  by the daemon afterwards.
- `when:` and the hooks run on a worker thread, away from the daemon's
  single thread. One that takes longer than 5 seconds (say, `disk_free` on a
  hung network mount) is abandoned. For `when:` that counts as "no". The
  stuck thread is left to finish on its own.
- Each evaluation has a budget of 200 000 steps and nests at most 64
  function calls deep. A runaway loop stops with a runtime error.
- No string or list can grow past 100 000 bytes or items: `range`, `+`,
  `join` and `replace` all stop there. `read_file` reads at most 64 KiB.
- Source nests at most 32 levels (brackets, `not`, `-`, blocks).
- **Reloading:** a worker restarts on reload when its own text changes, or
  when a top-level `let` or `fn` in its file changes. Editing another worker,
  or a comment between workers, leaves it running.

## Implementation

| File | Role |
| --- | --- |
| `src/worker/awl/lexer.rs` | Source → tokens; indentation → `Indent`/`Dedent`, duration literals, f-strings |
| `src/worker/awl/parser.rs` | Tokens → syntax tree (`ast.rs`), by recursive descent |
| `src/worker/awl/types.rs` | Types, effects, built-in signatures |
| `src/worker/awl/check.rs` | The static type checker |
| `src/worker/awl/eval.rs` | The step-limited evaluator; side effects come out as `Action`s |
| `src/worker/awl/host.rs` | Live state (`battery()`, `load()`...) behind a trait, so tests use fixed values |
| `src/worker/awl/diag.rs` | rustc-style diagnostics and "did you mean" |
| `src/worker/awl/mod.rs` | `compile()`: check, evaluate the constants, lower each worker to a `WorkerSpec` plus its `Logic` |
