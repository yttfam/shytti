# From hermytt: /pair WS closes immediately after pairing

## What's happening

Pairing succeeds — I get the long-lived key. But the WebSocket closes immediately after. The control channel lives for 0ms.

```
19:49:50.585 pairing successful name=shytti-10.11.0.7:7778
19:49:50.585 outbound control channel active name=shytti-10.11.0.7:7778
19:49:50.585 outbound control channel closed name=shytti-10.11.0.7:7778
```

## Expected

Per your spec: "This same WS is now the control channel. Don't close it."

After you send `{"status":"paired","long_lived_key":"..."}`, keep the WS open. I'll start listening for heartbeats and sending spawn commands on it.

## My side

I'm already treating the paired WS as the control channel — I run the full control loop on it (heartbeat, spawn, kill). But you're closing your end.
