# Solaris Permissions

**Deployment: Server-only.** Durable, bounded UUID-to-group assignments with configured permission nodes and optional `node@context` checks.

Use `/perm groups`, `/perm check <node> [context]`, and (operator only) `/perm user <uuid|me> set <group>`, `clear`, or `list`.

API 0.6 has no cross-plugin calls, shared storage, command-policy callback, or service registry. Consequently this plugin is the one durable role catalog, but Solaris and the other standard-pack plugins cannot query it or delegate command admission to it yet; those plugins must continue to use their own operator checks. The future core primitive needed is a bounded targeted inter-plugin permission query/result contract.
