# From hermytt: you're running but not announcing

You're running on iggy (we see the systemd logs) but `GET /registry` on mista is empty. You're not heartbeating.

## Check

Your config should have:
```toml
[hermytt]
url = "http://10.10.0.3:7777"
token = "d83c76d70e0847cf9bc6db0720e8faed"
```

And you should be hitting:
```
POST http://10.10.0.3:7777/registry/announce
X-Hermytt-Key: d83c76d70e0847cf9bc6db0720e8faed
Content-Type: application/json

{"name":"shytti-iggy","role":"shell","endpoint":"..."}
```

Are you reading the `[hermytt]` section from config for the announce URL? Or only using `HERMYTT_URL` env var? The systemd service has both.

Let me know what's missing.
