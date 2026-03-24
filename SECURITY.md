# Shytti Security Audit Report

**Date**: 2026-03-24 (revision 2)
**Previous audit**: 2026-03-22
**Auditor**: Security review (OWASP + adversarial)
**Scope**: All source files in `src/`, `install.sh`

## Architecture Changes Since Last Audit

- Bridge HTTP calls to Hermytt replaced with persistent control WebSocket
- Mode 2 added: OTP pairing via `/pair` WS, reconnect via `/control` WS
- All PTY data now flows as base64-encoded JSON over the control WS
- Auth middleware bypasses `/pair` and `/control` (first-message auth)
- `install.sh` bootstrap script added (runs as root)
- Long-lived key persisted to `/opt/shytti/.shytti-key`

## Summary Table

| # | Finding | Severity | OWASP Category | File | Status |
|---|---------|----------|----------------|------|--------|
| 1 | Command injection via `cmd` field (`sh -c`) | **CRITICAL** | A03: Injection | `shell.rs:234-236` | Open |
| 2 | Predictable key generation (timestamp + PID) | **CRITICAL** | A02: Crypto Failures | `control.rs:381-386` | New |
| 3 | Control WS spawn bypasses all validation | **CRITICAL** | A01: Broken Access | `control.rs:186-193` | New |
| 4 | install.sh: no integrity verification on downloaded binary | **HIGH** | A08: Software Integrity | `install.sh:33` | New |
| 5 | Long-lived key: TOCTOU race in file permissions | **HIGH** | A05: Misconfiguration | `control.rs:404-416` | New |
| 6 | Timing-based key comparison (not constant-time) | **HIGH** | A02: Crypto Failures | `api.rs:98,258` | New |
| 7 | No authentication on REST API when key is empty | **HIGH** | A07: Auth Failures | `api.rs:89-91` | Open (worsened) |
| 8 | install.sh: service runs as root | **HIGH** | A05: Misconfiguration | `install.sh:51-64` | New |
| 9 | Pair token replay within 5-min window | **MEDIUM** | A07: Auth Failures | `control.rs:309-323` | New |
| 10 | Control WS has no rate limiting on spawn commands | **MEDIUM** | A05: Misconfiguration | `control.rs:185` | New |
| 11 | `/pair` and `/control` unauthenticated at HTTP layer | **MEDIUM** | A07: Auth Failures | `api.rs:84-87` | New |
| 12 | install.sh: `pkill -9 -f 'shytti'` kills unrelated processes | **MEDIUM** | A05: Misconfiguration | `install.sh:29` | New |
| 13 | install.sh: deletes existing pairing key on every run | **MEDIUM** | A05: Misconfiguration | `install.sh:37` | New |
| 14 | Base64 decode silently ignores invalid characters | **MEDIUM** | A04: Insecure Design | `control.rs:439-467` | New |
| 15 | Arbitrary SSH target via `host` field | **HIGH** | A03: Injection | `shell.rs:219-225` | Open |
| 16 | Arbitrary binary execution via `shell` field (autostart bypasses allowlist) | **HIGH** | A03: Injection | `shell.rs:191-193` | Worsened |
| 17 | Path traversal via `cwd` field | **MEDIUM** | A01: Broken Access | `shell.rs:167-186` | Improved (partial) |
| 18 | Unbounded `read_to_string` in raw HTTP client | **MEDIUM** | A05: Misconfiguration | `bridge.rs:209`, `main.rs:136` | Open |
| 19 | Auth key transmitted over plaintext HTTP/WS | **MEDIUM** | A02: Crypto Failures | `control.rs:97-99`, `bridge.rs:167` | Open |
| 20 | Error messages leak internal paths | **LOW** | A02: Crypto Failures | `error.rs:17-18` | Open |
| 21 | `parse_resize` truncation on large values | **LOW** | A04: Insecure Design | `bridge.rs:159-160` | Open |
| 22 | Writer HashMap keyed by session_id with no expiry | **LOW** | A04: Insecure Design | `control.rs:169-170` | New |
| 23 | Hostname command injection surface | **INFO** | A03: Injection | `control.rs:13-19` | New |
| 24 | install.sh: `curl | sudo bash` anti-pattern | **INFO** | A08: Software Integrity | `install.sh:5` | New |

---

## New Findings

### 2. CRITICAL: Predictable key generation (timestamp + PID)

**Description**: Both `gen_key()` and `gen_long_lived_key()` derive keys from `SystemTime::now().as_nanos()` and `std::process::id()`. These are not cryptographically random.

**Location**: `control.rs:381-394`
```rust
fn gen_key() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let pid = std::process::id();
    format!("{t:x}{pid:x}")
}

pub fn gen_long_lived_key() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let pid = std::process::id();
    format!("sk-{t:x}{pid:x}{t:x}")  // timestamp repeated — zero additional entropy
}
```

**Impact**: An attacker who can observe or guess the approximate startup time and PID of the Shytti process can reconstruct the pairing key and the long-lived key. PIDs on Linux are sequential and typically in 1-65535. Timestamps at nanosecond resolution within a 1-second window give ~1 billion candidates — feasible for an offline brute-force. The long-lived key repeats the timestamp, adding zero entropy.

**Attack scenario**:
1. Attacker observes `install.sh` output or `journalctl` to get approximate startup time
2. PID is leaked in shell IDs (`format!("{:x}-{n}", std::process::id())` in `next_id()`)
3. With PID known, the key space collapses to nanosecond-precision timestamp guessing

**Fix**: Use `getrandom` or `/dev/urandom` for key material. 32 bytes of random data, hex-encoded (64 chars), is standard.

---

### 3. CRITICAL: Control WS spawn bypasses all validation

**Description**: When Hermytt sends a `Spawn` command over the control WebSocket, `run_control()` calls `manager.spawn()` directly — NOT `manager.spawn_with_limits()`.

**Location**: `control.rs:186-193`
```rust
ControlMsg::Spawn { req_id, shell, cwd, session_id, name } => {
    let result = manager.spawn(SpawnRequest {
        name,
        shell,    // no allowlist check
        cwd,      // no path traversal check
        host: None,
        agent: None,
        cmd: None,
    }).await;
```

**Impact**: A compromised Hermytt (or anyone who obtains the control WS key) can:
- Spawn shells with arbitrary binaries (e.g., `shell: "/usr/bin/python3"`) bypassing `ALLOWED_SHELLS`
- Use arbitrary `cwd` values bypassing path traversal checks
- Spawn unlimited shells bypassing `max_shells`
- Name validation bypassed

This is the most severe finding because it combines the auth bypass from Finding #2 with full shell spawning capability.

**Fix**: Route all spawn requests through `spawn_with_limits()`, including those from the control WS.

---

### 4. HIGH: install.sh downloads binary without integrity verification

**Description**: The install script downloads a binary from GitHub over HTTPS and immediately makes it executable. There is no checksum verification, no GPG signature check, and no pinned hash.

**Location**: `install.sh:33-34`
```sh
curl -fsSL "$URL" -o "$BIN"
chmod +x "$BIN"
```

**Impact**: If the GitHub release is compromised (account takeover, CI supply chain), or if a CDN/proxy MITM occurs (less likely with HTTPS), the attacker gets arbitrary code execution as root on the target machine.

**Fix**: Publish SHA256 checksums alongside releases. Verify after download:
```sh
curl -fsSL "$URL.sha256" -o "$BIN.sha256"
echo "$(cat "$BIN.sha256")  $BIN" | sha256sum -c -
```

---

### 5. HIGH: TOCTOU race in key file permissions

**Description**: `save_key()` first writes the key to disk with default permissions, then calls `set_permissions()` to restrict to 0o600. Between these two operations, the file is world-readable.

**Location**: `control.rs:404-416`
```rust
pub fn save_key(path: &std::path::Path, key: &str) {
    if let Err(e) = std::fs::write(path, key) {  // created with umask perms (often 644)
        ...
    } else {
        let _ = std::fs::set_permissions(path, ...from_mode(0o600));  // fixed AFTER
    }
}
```

**Impact**: Another process monitoring `/opt/shytti/` can read the key in the window between creation and permission change. On a multi-user system, this leaks the long-lived authentication key.

**Fix**: Set umask to 0o077 before writing, or use `OpenOptions` with mode 0o600 from the start:
```rust
use std::os::unix::fs::OpenOptionsExt;
std::fs::OpenOptions::new()
    .write(true).create(true).truncate(true)
    .mode(0o600)
    .open(path)?
    .write_all(key.as_bytes())?;
```

---

### 6. HIGH: Timing-based key comparison

**Description**: Key comparisons use `==` (standard string equality), which short-circuits on the first differing byte. This leaks key length and prefix information via timing side channels.

**Location**:
- `api.rs:98`: `Some(key) if key == state.api_key`
- `api.rs:258`: `Some(k) if *k == auth_key` (control WS auth)
- `api.rs:190`: `!p.used && p.pair_key == pair_key` (pair WS auth)

**Impact**: An attacker making repeated auth attempts over the network can statistically determine the key byte-by-byte. Over a local network with low jitter, this is practical with ~10,000 requests per byte position.

**Fix**: Use constant-time comparison. The `subtle` crate provides `ConstantTimeEq`, or implement a simple fixed-time compare:
```rust
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() { return false; }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
```

---

### 7. HIGH: No authentication when API key is empty (default)

**Description**: The auth middleware skips entirely when `hermytt_key` is empty (the default). The default config has no key set.

**Location**: `api.rs:89-91`
```rust
if state.api_key.is_empty() {
    return Ok(next.run(request).await);
}
```

**Impact**: Out of the box, the REST API is completely unauthenticated. Since the default listen address is `127.0.0.1:7778`, this is mitigated for remote attacks but any local process can spawn shells, including:
- Malicious npm packages running postinstall scripts
- Browser-based attacks if CORS allows it (no CORS headers set = browser blocks, but non-browser clients are unaffected)
- Other compromised services on the same host

**Status**: Was HIGH in previous audit. Still open. Worsened because the control WS endpoints also bypass auth.

**Fix**: Generate a random API key on first startup if none is configured. Store it alongside the config. Require it for all API access.

---

### 8. HIGH: install.sh runs service as root

**Description**: The systemd unit file created by `install.sh` has no `User=` directive, so the service runs as root.

**Location**: `install.sh:51-64` — the generated `[Service]` section lacks `User=` and `Group=`.

**Impact**: If Shytti is compromised (via any of the RCE findings), the attacker gets root. All spawned shells run as root. The PTYs are owned by root.

**Note**: The CLAUDE.md design doc specifies `User=cali` in the service file, but the actual `install.sh` omits this.

**Fix**: Add to the generated service file:
```ini
User=cali
Group=cali
```
Or better, create a dedicated `shytti` user with limited privileges.

---

### 9. MEDIUM: Pair token replay within 5-minute window

**Description**: The pairing token contains the key in cleartext (base64-encoded, not encrypted). Once generated, anyone who intercepts the token can use it within 5 minutes. The token is printed to stderr/journal, meaning it is visible in `journalctl` output.

**Location**: `control.rs:309-323`, `main.rs:53-54`

**Impact**: The token appears in:
- `journalctl -u shytti` (readable by any user in `systemd-journal` group, or root)
- The `install.sh` output (visible to anyone watching the terminal or with access to the shell history)
- Potentially in log aggregation systems

An attacker with journal access can read the token and pair before the legitimate Hermytt does. The `used` flag prevents re-pairing, but if the attacker pairs first, they own the control channel.

**Fix**:
- Display only a short confirmation code, not the full key
- Or encrypt the token with a pre-shared secret
- Reduce the window to 60 seconds
- Clear the token from journal after successful pairing

---

### 10. MEDIUM: No rate limiting on control WS spawn commands

**Description**: Once authenticated on the control WS, there is no rate limiting or shell count enforcement on `Spawn` messages. A compromised Hermytt can flood the system with spawn requests.

**Location**: `control.rs:185` — the `Spawn` handler has no limits.

**Impact**: Denial of service via file descriptor and PID exhaustion. Even though `max_shells` exists for the REST API, it is not enforced on the control WS path (see Finding #3).

**Fix**: Pass `max_shells` and `allowed_hosts` into `run_control()` and use `spawn_with_limits()`.

---

### 11. MEDIUM: `/pair` and `/control` bypass auth middleware

**Description**: The auth middleware explicitly skips `/pair` and `/control`, relying on first-message WebSocket auth instead. This means any client can upgrade to a WebSocket connection on these endpoints without any HTTP-layer authentication.

**Location**: `api.rs:84-87`
```rust
if path == "/pair" || path == "/control" {
    return Ok(next.run(request).await);
}
```

**Impact**:
- An attacker can hold open WebSocket connections to `/pair` and `/control`, consuming server resources
- The WebSocket upgrade happens before any auth check, so the TLS handshake, HTTP upgrade, and memory allocation all occur for unauthenticated clients
- No connection limit means an attacker can open thousands of WS connections

**Fix**: Add connection rate limiting per IP. Consider requiring a basic auth token in the WS upgrade request headers (e.g., as a query parameter or header), verified before accepting the upgrade.

---

### 12. MEDIUM: `pkill -9 -f 'shytti'` in install.sh

**Description**: The install script uses `pkill -9 -f 'shytti'` which matches any process whose command line contains "shytti" — including the install script itself, editors with the file open, or unrelated processes.

**Location**: `install.sh:29`

**Impact**: Kills unrelated processes. Could kill user's editor, other scripts, or the install script itself on some systems.

**Fix**: Use `systemctl stop shytti` only, or use a PID file. Remove the `pkill` line.

---

### 13. MEDIUM: install.sh deletes existing pairing key

**Description**: `install.sh` unconditionally removes `.shytti-key`, breaking any existing pairing.

**Location**: `install.sh:37`
```sh
rm -f "$INSTALL_DIR/.shytti-key"
```

**Impact**: On upgrade/reinstall, the existing Hermytt connection is permanently broken. Hermytt will try to reconnect with the old key, which no longer exists. Manual re-pairing is required.

**Fix**: Only delete the key if this is a fresh install (check if config already exists). Or prompt the user.

---

### 14. MEDIUM: Base64 decode silently ignores invalid characters

**Description**: The custom base64 decoder maps unknown characters to 0 (via the `unwrap_or(0)` in the `b` closure), rather than returning an error.

**Location**: `control.rs:455-459`
```rust
let b = |i: usize| -> u32 {
    chunk.get(i).copied()
        .and_then(|c| TABLE.get(c as usize).copied())
        .filter(|&v| v != 255)
        .unwrap_or(0) as u32  // invalid → 0, not error
};
```

**Impact**: Malformed input is silently accepted, which could lead to unexpected behavior. If base64-encoded data is used for auth (e.g., the pair token), corrupted tokens might partially decode rather than being rejected.

Additionally, the decoder does not validate that non-ASCII bytes (>127) are rejected — `TABLE.get(c as usize)` would return `None` for values 128-255, which maps to 0 rather than an error.

**Fix**: Return `Err(())` when encountering any invalid character instead of silently substituting 0. Consider using the `base64` crate instead of a hand-rolled implementation.

---

### 16. HIGH: Autostart shells bypass spawn_with_limits

**Description**: Shells spawned at startup from config (`autostart = true`) go through `manager.spawn()` directly, not `spawn_with_limits()`.

**Location**: `main.rs:25`
```rust
match manager.spawn(shell_cfg.into()).await {
```

**Impact**: Config-defined shells bypass the shell allowlist, host allowlist, name length validation, cwd validation, and max_shells limit. If an attacker can modify the config file, they can specify arbitrary binaries, hosts, and paths.

**Note**: Config file access implies local file write, which is already a strong primitive. However, defense-in-depth says validation should still apply.

**Fix**: Use `spawn_with_limits()` for autostart shells as well, or at minimum validate config entries at load time.

---

### 22. LOW: Writer HashMap with no expiry or cleanup

**Description**: The `writers` HashMap in `run_control()` stores PTY writers keyed by `session_id`. Writers are only removed on write error or explicit `Kill` command. If a session dies without a kill command, the writer leaks.

**Location**: `control.rs:169-170`

**Impact**: Minor memory leak over time. The `ShellDied` event removes the shell from the manager, but does not clean up the writer from the HashMap.

**Fix**: Listen for `ShellDied` events in the control loop and remove the corresponding writer.

---

### 23. INFO: Hostname resolution via subprocess

**Description**: `gethostname()` shells out to `hostname` (and on macOS, `ipconfig`). These are not security-critical but represent unnecessary subprocess spawning.

**Location**: `control.rs:13-19`, `bridge.rs:190-197` (duplicate function)

**Impact**: Minimal. The hostname binary is trusted. Note that `gethostname()` exists in both `bridge.rs` and `control.rs` — the duplicate in `bridge.rs` appears unused since the bridge HTTP path was replaced.

**Fix**: Use `libc::gethostname()` or the `hostname` crate. Remove the duplicate in `bridge.rs`.

---

## Re-checked Previous Findings

### Shell allowlist — PARTIALLY EFFECTIVE

The `ALLOWED_SHELLS` list at `shell.rs:13-31` is enforced in `spawn_with_limits()`. However:
- Control WS spawns bypass it entirely (Finding #3)
- Autostart shells bypass it (Finding #16)
- The allowlist is comprehensive for common shells

### Host allowlist — PARTIALLY EFFECTIVE

Host validation at `shell.rs:149-155` has a bypass: if `allowed_hosts` is empty (no hosts configured), any host is allowed. This is by design but means a default config with no `[[shells]]` entries with `host` fields has no host restriction. Additionally:
- Control WS spawns don't pass `host` at all (`host: None` hardcoded in control.rs:191)
- The REST API properly enforces it

### Max shells limit — PARTIALLY EFFECTIVE

The `max_shells` check at `shell.rs:130-137` works for REST API spawns. Not enforced for:
- Control WS spawns (Finding #3)
- Autostart shells (Finding #16)

### Body size limit — EFFECTIVE

`DefaultBodyLimit::max(65536)` at `api.rs:62` is properly configured. This is a good improvement from the last audit.

### Path traversal protection — IMPROVED but INCOMPLETE

The cwd validation at `shell.rs:167-186` now blocks `/proc`, `/sys`, `/root`. However:
- Symlink traversal is not checked (`/tmp/link -> /root` bypasses the check)
- `..` traversal is not canonicalized (e.g., `/home/cali/../../root` bypasses)
- Control WS spawns bypass all cwd validation

---

## Control WS Security

### Authentication Model

Mode 1 (Shytti connects to Hermytt): Auth key sent as first WS message. Key is from config file.

Mode 2 (Hermytt connects to Shytti):
1. Initial pairing: Hermytt sends OTP key via `/pair` WS
2. Reconnect: Hermytt sends long-lived key via `/control` WS

### Weaknesses

1. **Single-factor auth**: Both modes rely solely on a shared key. No mutual TLS, no certificate pinning.
2. **No session binding**: Once authenticated, the WS connection has full access. No per-command authorization.
3. **No message signing**: Messages are not authenticated. If the WS transport is compromised (e.g., WS without TLS), messages can be injected.
4. **Reconnect window**: Between disconnection and reconnection, another client could connect with the same key. There is no mechanism to detect or prevent concurrent control connections.

---

## Pairing Security

### Token Structure

```json
{"ip": "10.10.0.4", "port": 7778, "key": "<hex-timestamp+pid>", "expires": <unix_ts>}
```
Base64-encoded (not encrypted).

### Weaknesses

1. **Key is not random** (Finding #2) — derived from time + PID
2. **Token is not encrypted** — anyone who sees the base64 string has the key
3. **Single-use flag is per-process** — if Shytti restarts, the pair state is lost and the key could be reused (though the expiry check helps)
4. **No confirmation step** — pairing succeeds immediately with no human confirmation
5. **IP in token is informational** — not validated by the server

### Positive Notes

- 5-minute expiry is reasonable
- `used` flag prevents re-pairing on the same instance
- Token expiry is validated on decode

---

## Key Management

### Long-lived Key

- Stored at `/opt/shytti/.shytti-key`
- Permissions set to 0o600 after creation (TOCTOU issue, Finding #5)
- Key format: `sk-<hex_timestamp><hex_pid><hex_timestamp>` — predictable (Finding #2)
- Key never rotates automatically
- No mechanism to revoke a key without deleting the file and restarting

### Recommendations

1. Generate keys from `/dev/urandom` (32 bytes, hex-encoded)
2. Create file with restricted permissions atomically (not write-then-chmod)
3. Implement key rotation (generate new key, notify connected Hermytt, grace period)
4. Log key usage for audit trail

---

## Install Script Security

### `install.sh` — runs as root

| Risk | Description |
|------|-------------|
| No checksum | Binary downloaded without hash verification (Finding #4) |
| Runs as root | Service has no `User=` directive (Finding #8) |
| Kills broadly | `pkill -9 -f 'shytti'` matches too broadly (Finding #12) |
| Deletes state | Removes `.shytti-key` unconditionally (Finding #13) |
| `curl \| bash` | Standard supply-chain risk (Finding #24) |
| Listens on 0.0.0.0 | Default config binds to all interfaces, not just localhost |

### Positive Notes

- Uses `set -e` for error handling
- Platform detection is reasonable
- Uses HTTPS for download
- Waits for pairing token and displays it

---

## Recommendations (Priority Order)

### Immediate (before any deployment)

1. **Replace key generation** with cryptographically random keys (`getrandom` or `/dev/urandom`)
2. **Route control WS spawns through `spawn_with_limits()`** — this is the highest-impact fix
3. **Add `User=cali` to the systemd unit** in `install.sh`
4. **Use constant-time key comparison** for all auth checks

### Short-term

5. Add checksum verification to `install.sh`
6. Fix TOCTOU in `save_key()` — create file with 0o600 from the start
7. Add connection rate limiting to `/pair` and `/control`
8. Replace custom base64 with the `base64` crate
9. Canonicalize `cwd` paths and resolve symlinks before validation

### Medium-term

10. Add mutual TLS or certificate pinning for control WS
11. Implement key rotation
12. Add per-command authorization on the control channel
13. Add audit logging for all shell operations
14. Remove dead code (`bridge.rs` HTTP functions if no longer used)
