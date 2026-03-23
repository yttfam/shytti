# shytti

Shell orchestrator daemon. The sixth child of the YTT family.

Replaces SSH + tmux with something that actually works. One daemon per machine, infinite shells, zero tmux.

## The Problem

```
ssh mini → tmux new -s foo → work → Ctrl+B D → ssh mini → tmux attach → fuck scroll is broken
× 20 sessions × 5 machines = madness
```

## The Fix

```
shytti daemon                              # start on each machine
shytti spawn --name infra --cwd ~/infra    # local shell
shytti spawn --host cali@10.10.0.7         # SSH session
shytti spawn --agent rustguard             # Claude agent as a tab
shytti list                                # see everything
shytti kill 1a2b-3                         # done with it
```

Every shell pipes through [Hermytt](https://github.com/calibrae/hermytt) and renders in [Crytter](https://github.com/calibrae/crytter). Open a browser, see all your shells across all your machines.

## Install

```bash
# Bootstrap from Hermytt (downloads binary, writes config, installs systemd, starts service)
curl -H 'X-Hermytt-Key: TOKEN' http://hermytt:7777/bootstrap/shytti | sudo bash

# Or grab a binary
curl -LO https://github.com/calibrae/shytti/releases/latest/download/shytti-$(uname -s | tr A-Z a-z)-$(uname -m)
chmod +x shytti-*
sudo mv shytti-* /usr/local/bin/shytti

# Or build from source
cargo install --path .
```

## Shell Types

| Type | Spawns | Example |
|------|--------|---------|
| Local | PTY with your shell | `shytti spawn --name work` |
| Remote | `ssh -t` session | `shytti spawn --host cali@polnareff` |
| Agent | `claude --agent` | `shytti spawn --agent infra` |
| Command | One-off process | `shytti spawn --cmd "journalctl -f"` |

## Config

`~/.config/shytti/shytti.toml`

```toml
[daemon]
listen = "127.0.0.1:7778"
hermytt_url = "http://localhost:7777"
hermytt_key = "your-secret"
max_shells = 64

[defaults]
shell = "/bin/zsh"
scrollback = 10000

[[shells]]
name = "infra"
cwd = "~/Developer/perso/infra"
autostart = true

[[shells]]
name = "polnareff"
host = "cali@10.10.0.7"
autostart = true

[[shells]]
name = "kernel-vet"
agent = "rustguard"
autostart = false
```

## API

```
POST   /shells              spawn shell
GET    /shells              list shells
DELETE /shells/{id}         kill shell
POST   /shells/{id}/resize  resize PTY
```

All endpoints require `X-Shytti-Key` header when `hermytt_key` is set.

## Architecture

```
Crytter (browser)
  ↕ WebSocket
Hermytt (transport)
  ↕ WS pipe
Shytti (this) ← one per machine
  ↕ PTY / SSH / claude
Shell processes
```

## Stats

```
Binary:  1.7 MB (stripped, LTO)
Code:    ~800 lines
Deps:    10 direct
Tests:   80 (unit, integration, e2e, security)
```

## Security

See [SECURITY.md](SECURITY.md) for the full audit. Key protections:

- API auth via shared key
- Shell binary allowlist (no arbitrary execution)
- SSH host allowlist (from config)
- Shell count limits
- Request body size limits
- Path traversal protection

## The YTT Family

```
Crytter  terminal rendering (tabs, UI)
Hermytt  transport layer (WS/MQTT/SSE)
Shytti   shell orchestrator (you are here)
Fytti    GPU renderer & WASM host
Wytti    WASI runtime & sandbox
Prytty   syntax highlighting
```

## License

[MIT](LICENSE)
