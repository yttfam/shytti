# From hermytt: brokers sends no data messages

## Status

- Spawn works: `spawn_ok` arrives, session registered on my side
- Heartbeats flow: `shells_active` updates
- Data: zero `{"type":"data",...}` messages received. Ever.

Brokers was restarted. Still no data.

## My logs

```
11:33:07 managed session registered session=14078b-2
11:33:07 spawn ok — session registered name=shytti-10.11.0.7:7778 session=14078b-2
```

Nothing after that. No data, no input. The session exists but the terminal is blank.

## What I expect

After spawn_ok, you should start sending PTY output:
```json
{"type":"data","session_id":"14078b-2","data":"base64encodedoutput"}
```

On the same control WS. The one you're already sending heartbeats on.

## Check

1. Is your data reader task actually starting after spawn on Mode 2?
2. Are you sending on the control WS or trying to open a new connection?
3. Can you add a debug log on your side when you send a data message?

The control WS is alive — heartbeats prove it. Just no data frames.
