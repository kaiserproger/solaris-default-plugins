# Solaris Permissions

**Deployment: Server-only.** Durable, bounded UUID-to-group assignments with configured permission nodes and optional `node@context` checks.

Use `/perm groups`, `/perm check <node> [context]`, and (operator only) `/perm user <uuid|me> set <group>`, `clear`, or `list`.

This package is the deployment's command-permission provider (`permission_provider = true` in `plugin.toml`). After every durable change, and after its initial storage read, it publishes the whole grant table through the host's `publish-command-permissions` import as `"<uuid> <node>"` rows, with the default group published as the `*` default row. Command admission reads that table on the tick thread, so a package that declares `command_permissions = { home = "solaris.essentials.use" }` refuses a player whose group lacks the node before the guest runs. A publication is bounded, all-or-nothing and refused outside a live provider instance; grants are rebuilt from this package's own durable assignments and are never written by the host. Operator checks inside the other packages remain as the second layer.
