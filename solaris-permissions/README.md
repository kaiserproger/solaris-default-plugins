# Solaris Permissions

**Deployment: Server-only.** Durable, bounded UUID-to-group assignments with configured permission nodes and optional `node@context` checks.

Use `/perm groups`, `/perm check <node> [context]`, and (operator only) `/perm user <uuid|me> set <group>`, `clear`, or `list`.

This package is the deployment's command-permission provider (`permission_provider = true` in `plugin.toml`). After every durable change, and after its initial storage read, it publishes the whole grant table through the host's `publish-command-permissions` import as `"<uuid> <node>"` rows, with default grants under identity `*`. Every explicit assignment also publishes an identity-only UUID row; this represents an empty grant set when the group has no nodes, and suppresses the default group. Node rows for the same identity add its grants. Identity-only rows count towards the host's 8192-entry limit: with at most 64 assignments and 64 nodes per group, this provider publishes at most 4224 entries. Command admission reads that table on the tick thread, so a package that declares `command_permissions = { home = "solaris.essentials.use" }` refuses a player whose group lacks the node before the guest runs. Publication is bounded and all-or-nothing; rejected updates retain the previous table. Grants are rebuilt from this package's durable assignments and are never written by the host. Operator checks inside the other packages remain as the second layer.

Conflicted writes and failed or invalid storage reads do not publish a fallback
table. The last accepted assignments remain in force while reloading; before the
first successful read, managed command admission stays denied. This prevents an
explicit empty group from temporarily regaining default-group permissions.
