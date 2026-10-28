# Solaris Auth

Optional offline-mode authentication for ordinary vanilla clients. Install the
`solaris-auth` package alongside the other packages in `[plugins].directory`.
Native v2 discovers every directory entry automatically; no expected-id list or
explicit precommit registration is supported. The manifest declares the `auth`
capability alongside `storage` as the explicit auth grant; the native
fail-closed session gate is the only build/input enforcement path.

Deploying this package turns the native fail-closed session gate on by itself
(the manifest's `auth` capability is what core looks for), so the default
`[auth] require_plugin_login = false` is enough. Setting it to `true` is still
accepted and additionally makes startup fail when no auth package is deployed:

```toml
[plugins]
directory = "/path/to/plugins"

[auth]
require_plugin_login = true
```

Every connection starts locked; successful login or committed registration emits
`unlock_session(session)` for that exact connection. The native gate owns input
quarantine (only the declared authentication commands may pass while locked).

- `/register <password> <password>` creates an account and logs in only after the
  durable compare-and-swap succeeds.
- `/login <password>` authenticates this connection only.
- `/changepassword <old> <new>` requires an authenticated session and verifies
  the old password before durably replacing it. Passwords are 4–128 UTF-8 bytes,
  without whitespace.
- Join prompts are Russian + English. An unauthenticated connection is kicked
  after 1,200 native simulation ticks (60 seconds at 20 TPS); five wrong password
  verifications also kick. Logout discards authentication and pending requests.
- Before authentication, the native gate holds the session locked: only the
  declared authentication commands may pass, and no other input (including
  block placement and breaking) reaches the world. Any other command (for
  example `/op`) and every chat line is refused with a short Russian/English
  hint and is never broadcast.

## Storage and hashing

Account keys are `account:<lowercase username>` in the server's native durable
plugin storage, not a second database. Offline identity is deliberately the
case-insensitive username. Values are Argon2id PHC strings containing the
algorithm, work parameters, random salt and hash. Hashing and verification use
the native host's narrow `password_hash` / `password_verify` API on the existing
plugin worker, not custom guest cryptography or a separate database/worker.
Registration and password changes get fresh salts from the host's OS RNG.
The package needs no configuration file or deployment secret.

The 60-second deadline includes hashing and storage completion. CAS conflicts
require a fresh command after the new prompt; they never authenticate silently.
Do not delete/reset the native world database without also planning account
migration: plugin storage is part of that database.

## Security boundaries

These are commands, not chat: the plugin sends only private session responses,
never broadcasts password arguments and never calls the logging import. Command
history, screenshots, client-side mods, an operator's packet tracing and the
unencrypted offline-mode network are outside the plugin's control. Do not reuse
valuable passwords. The plugin does not claim TLS or online-mode identity.

The component API alone cannot intercept another package's commands, native
commands or ordinary chat. The native gate is enabled by deploying this package on a
core build that includes the auto-enable rule (core versions before it need
`[auth] require_plugin_login = true`; without the gate the plugin provides only
prompts and timeout, **not** build protection or full input quarantine). Native v2 rejects `before-damage` declarations: incoming
environment/mob damage is **not blocked** by this package. It makes no claim of
full invulnerability or encrypted transport. Reconnect after a plugin instance
restart.

Build with `tools/build_standard_pack.py --core-root ../solaris --package solaris-auth`;
auth is opt-in and is not added to the five-package default installation.
