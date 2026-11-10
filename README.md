## A.R.I.A

AI-powered home-server project, written in Rust.

### Build

```bash
cargo build --release
```

The binary is `target/release/aria`. Run the tests with `cargo test`.

### Commands

| Command | What it does |
| --- | --- |
| `aria version` / `status` / `dev` / `version-name` / `is-stable` / `info` | Version metadata |
| `aria check-version` / `aria update` | Check for and install a new release |
| `aria ram-info` / `storage-info` / `cache-info` / `apps` | System information |
| `aria rm-cache [-y]` | Delete the 25 biggest entries of `~/.cache` |
| `aria notify <title> <body>` | Desktop notification (needs `notify-send`) |
| `aria serve [--host H] [--port P]` | HTTP server: `/`, `/read` (log stream), `/ws` (echo) |
| `aria chat` / `aria shell` | Chat with an LLM / interactive shell |
| `aria model download-config\|make\|insert-api\|providers` | Model config and encrypted API keys |
| `aria cli-config make\|override\|add\|show` | The CLI's own config |
| `aria additionals list\|check-update\|download <name>\|update` | Plug-and-play skills and tools |
| `aria voice download-config\|select-model` | Local voice model config |
| `aria protocol create\|show <path>` | Protocol files |
| `aria paths` | Where A.R.I.A keeps its files |

Add `-v` to any command to see log lines on stderr. Data lives under
`~/.config/A.R.I.A` (configs in `Config/`, logs in `Logs/`).

### Layout

| Module | Purpose |
| --- | --- |
| `src/main.rs` | Command line |
| `src/paths.rs` | Paths, JSON and download helpers |
| `src/logger.rs` | Named loggers with per-module log files |
| `src/crypto.rs` | Fernet encryption and the key store |
| `src/model_config.rs` | Providers, models, API keys |
| `src/agents/` | Model selection and the NVIDIA chat agent |
| `src/linux.rs` | RAM, storage, cache, installed apps |
| `src/updater.rs` | Version check and self-update |
| `src/additionals.rs` | Additionals download and update |
| `src/server.rs` | HTTP / SSE / WebSocket server |
| `src/cli_config.rs`, `src/voice.rs`, `src/protocol.rs`, `src/worker.rs`, `src/notifier.rs`, `src/repl.rs` | The rest |
