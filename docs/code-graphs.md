# A.R.I.A code graphs

These diagrams describe the Rust codebase (`src/`).

## Runtime architecture

```mermaid
flowchart LR
    CLI[main.rs\naria command line] --> Linux[linux.rs\nRAM, cache, storage]
    CLI --> Updater[updater.rs\nversion check, self-update]
    CLI --> Server[server.rs\naxum: /, /read, /ws]
    CLI --> Repl[repl.rs\nshell and chat loop]
    CLI --> Additionals[additionals.rs]
    CLI --> CliConfig[cli_config.rs]
    CLI --> Voice[voice.rs]
    CLI --> Notifier[notifier.rs\nnotify-send]
    CLI --> Protocol[protocol.rs]

    Repl --> Nvidia[agents/nvidia.rs\nNvidiaAgent]
    Nvidia --> Loader[agents/mod.rs\nLoadAgent, pick_model]
    Loader --> ModelConfig[model_config.rs\nproviders, models, API keys]
    ModelConfig --> Crypto[crypto.rs\nFernet + key store]
    CliConfig --> Crypto
    Nvidia --> API[OpenAI-compatible API]

    Voice --> Linux
    Updater --> Paths[paths.rs\npaths, JSON, HTTP]
    Additionals --> Paths
    ModelConfig --> Paths
    Crypto --> Paths
```

## Workers and AWL

```mermaid
flowchart LR
    Files[Config/worker\n*.json, *.awl] --> Load[worker/spec.rs\nload_dir]
    Load -->|*.json| Spec[WorkerSpec]
    Load -->|*.awl| Lex[awl/lexer.rs] --> Parse[awl/parser.rs] --> Check[awl/check.rs\ntype checker]
    Check --> Lower[awl/mod.rs\ncompile, lower]
    Lower --> Spec
    Lower --> Logic[Logic\nwhen, on_success, on_failure]
    Spec --> Worker[Worker::compile]
    Logic --> Worker
    Worker --> Daemon[worker/daemon.rs\nsupervisor]
    Daemon -->|before a scheduled run| Eval[awl/eval.rs\nstep-limited evaluator]
    Daemon -->|after a run| Eval
    Eval --> Host[awl/host.rs\nclock, battery, load, disk]
    Eval -->|Actions: log, notify, trigger, pause| Daemon
```

## Notes

- `config/path_config.json` and `config/config.json` are compiled into the binary.
  An installed `~/.A.R.I.A/config/config.json` takes precedence for version metadata.
- Every module has its own logger (`logger.rs`); most write to a file under
  `~/.config/A.R.I.A/Logs`. `aria -v` mirrors log lines to stderr.
- `agents::Agents` is a placeholder for planned features. Workers (`worker/`) are described in [workers.md](workers.md).
