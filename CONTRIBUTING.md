# Contributing to Shytti

## Setup

```bash
git clone https://github.com/calibrae/shytti
cd shytti
cargo build
cargo test
```

Needs Rust 1.94+ (edition 2024).

## Tests

```bash
cargo test                  # all 80 tests
cargo test --lib            # unit tests only
cargo test --test integration
cargo test --test e2e
cargo test --test security
```

All tests must pass before submitting a PR.

## Code Style

- Keep it lean. 10 deps is the ceiling, not the floor.
- No `unwrap()` in library code. `unwrap()` is fine in tests.
- Error types over `anyhow`. We own our errors.
- Raw HTTP over reqwest. We don't need a client library to POST JSON.
- If a feature needs a new dep, justify it. Can you do it in 20 lines instead?

## Architecture

```
src/
├── main.rs    CLI dispatch + raw HTTP client
├── lib.rs     module re-exports
├── cli.rs     arg parsing (no clap, 50 lines)
├── config.rs  TOML config loading
├── error.rs   error types + axum integration
├── shell.rs   ShellManager: spawn/kill/list/resize PTYs
├── api.rs     REST API (axum)
└── bridge.rs  Hermytt WS bridge + registry heartbeat
```

## What We Accept

- Bug fixes with a test that reproduces the bug
- Security hardening (see SECURITY.md for known issues)
- Performance improvements with benchmarks
- New shell types (with tests)
- Hermytt/Crytter integration improvements

## What We Don't Accept

- New dependencies without strong justification
- Features that don't serve the core use case (spawn shells, pipe them)
- Code that makes the binary bigger without clear value
- Refactors that don't fix a bug or enable a feature

## Inbox System

The YTT family communicates via `inbox/` folders. If your change affects how Shytti talks to Hermytt or other siblings, drop a message in their inbox explaining the change.

## Releasing

Releases are cut from `main`. Tag with `v{version}`, binaries are built for:
- `shytti-darwin-aarch64` (macOS Apple Silicon)
- `shytti-linux-x86_64` (Linux x86_64, static musl)
