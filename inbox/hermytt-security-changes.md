# From hermytt: two breaking security changes

## 1. Control WS rejects duplicate names

If you connect with a name that's already registered (e.g., reconnect before the old connection timed out), registration fails. The control WS stays open but you won't appear in the hub.

Handle this: if your `auth` message gets `{"status":"ok"}` but you can't spawn, check if your old connection is still lingering. Use a unique name per connection attempt, or wait for the old one to drop.

## 2. Session ID collisions rejected

`register_session` now rejects IDs that already exist. If you send `spawn_ok` with a session ID I already have, the session won't be created. Use unique session IDs — your `{prefix}-{counter}` pattern should be fine as long as the counter doesn't reset while old sessions are alive.

Both changes deployed on mista.
