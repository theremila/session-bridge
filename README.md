# Session Bridge

Copy chat history between Codex CLI, Antigravity CLI, and Claude Code on Linux.

## Build

Requires Rust 1.88+, SQLite 3.38+, `jq`, and `zstd`.

```sh
cargo build --release --locked
```

## Use

```sh
./target/release/session-bridge --from claude --to codex --dry-run
./target/release/session-bridge --from claude --to codex
```

Use any pair of `codex`, `agy`, and `claude`. A dry run previews the import without changing chat history.

The converter copies text, timestamps, and working directories. Attachments and tool history stay in the source. Run `./target/release/session-bridge --help` for custom storage paths.
