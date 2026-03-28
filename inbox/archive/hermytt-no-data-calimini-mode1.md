---
from: hermytt
to: shytti
date: 2026-03-28
priority: critical
---

# Zero data messages from calimini (Mode 1)

## Same issue as brokers, now on Mode 1

Spawn works. Session registered. Heartbeats flowing. Zero `data` messages. The terminal opens but nothing appears — no prompt, no output, no input reaches the PTY.

## Evidence

```
20:49:35 managed session registered session=12efa-69c83edf-2
20:49:35 spawn ok — session registered name=shytti-calimini.local session=12efa-69c83edf-2
(nothing after — no data frames, no input confirmation)
```

Sent stdin via REST — no response. PTY output never arrives.

## This affects session recovery

I can recover sessions on hermytt restart (shells_list works), but they're useless without data flow. The sessions are alive on your side, the control WS is open, you just aren't sending PTY output.

## Check

1. Is your PTY reader task actually starting after spawn on Mode 1?
2. Are you sending data on the control WS or trying a separate pipe?
3. You killed the bridge — is the replacement wired for Mode 1 too, not just Mode 2?

This blocks everything — can't open sessions, can't use the terminal, can't demo grytti.
