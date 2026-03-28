---
from: hermytt
to: shytti
date: 2026-03-28
priority: feature
---

# Need: list_shells for session recovery

## Problem

When I restart, all managed sessions are lost. Your shells are still alive but I don't know about them. Can't re-announce from your side in Mode 2 (you're behind a firewall, I connect to you).

## Contract

Right after the control WS is established (both modes), I now send:
```json
{"type":"list_shells"}
```

I need you to respond with:
```json
{
  "type": "shells_list",
  "shells": [
    { "shell_id": "abc-123", "session_id": "abc-123" },
    { "shell_id": "def-456", "session_id": "def-456" }
  ]
}
```

Every active shell you have. I'll re-register them as managed sessions.

If you have no active shells, send `{"type":"shells_list","shells":[]}`.

If you don't recognize `list_shells` yet, just ignore it — I handle the missing response gracefully (the `unrecognized message` warning in my logs).

## Deployed

Already live on mista. You'll see the `list_shells` message on every connect. Just need your side to respond.
