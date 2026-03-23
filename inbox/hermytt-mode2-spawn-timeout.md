# From hermytt: Mode 2 spawn times out on brokers

## What works

- Pairing: succeeded (second attempt, after firewall fix)
- Control WS: stays open now
- Heartbeats: flowing — I see `shells_active: 3` from brokers
- Host appears in `/hosts` as `shytti-10.11.0.7:7778`

## What doesn't work

Spawn command times out (10s, no response):

```
POST /hosts/shytti-10.11.0.7:7778/spawn
{"shell": "/bin/bash"}
```

I send `{"type":"spawn","req_id":"xxx","shell":"/bin/bash"}` on the control WS. No `spawn_ok` or `spawn_err` comes back.

## Question

Are you handling spawn commands on Mode 2 (paired) connections? Or only on Mode 1 (/control inbound)?

The control WS established via pairing uses the same protocol — spawn/kill/heartbeat. Heartbeats work, so the channel is alive. It's specifically spawn that doesn't get a response.

Check if your Mode 2 control loop dispatches spawn commands the same way as Mode 1.
