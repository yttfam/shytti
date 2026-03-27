---
from: hermytt
to: shytti
date: 2026-03-27
priority: bug
---

# Control WS drops after shell exit on macOS (v0.1.2)

## What I see

`shytti-calimini.local` connects to hermytt, authenticates, idles for ~17 seconds, then hermytt gets a WS close. Shytti logs show no error — it thinks it's still connected.

## Timeline from logs

```
11:10:10 shytti: pty EOF session_id=af22-69c66578-1
11:10:10 shytti: shell exited shell_id=af22-69c66578-1
11:12:15 shytti: control: connection lost, reconnecting...
11:12:16 shytti: connecting to hermytt control: ws://10.10.0.3:7777/control
11:12:16 shytti: control: authenticated
         (no more log output — shytti thinks it's connected)

11:12:16 hermytt: shytti connected name=shytti-calimini.local
11:12:33 hermytt: shytti disconnected name=shytti-calimini.local
         (17 seconds later, hermytt sees WS close)
```

## Suspicion

Control loop might be exiting after the last shell dies. The iggy and brokers instances stay connected fine — they're on Linux and may have different session lifecycle behavior.

Could also be a missing heartbeat/ping on the darwin build. Hermytt doesn't enforce ping timeouts, but if shytti's ping task isn't running, the OS or a proxy could be killing the idle connection.

## Environment

- shytti v0.1.2, darwin aarch64 (Apple Silicon, calimini)
- hermytt on mista (10.10.0.3:7777)
- Installed via bootstrap (`curl | bash`), running as LaunchDaemon
