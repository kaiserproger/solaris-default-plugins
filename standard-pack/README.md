# Solaris Standard Plugin Pack (alpha3)

This opt-in, first-party, lightweight pack provides common administration/gameplay workflows without cloning large Bukkit/Paper plugins:

- `solaris-permissions` — durable groups and permission-node catalog;
- `solaris-essentials` — homes, spawn/warps, back, TPA, and private messages;
- `solaris-economy` — the single virtual-money authority with idempotent payments;
- `solaris-towns` — small towns, invitations, roles, and leader-protected chunk claims;
- `solaris-audit` — bounded committed-action history and lookup.

All five are independent API 0.6 packages and **server-only**. Commands work with a vanilla Minecraft 26.1.2 client; Solaris Loader UI could enhance presentation later but is never required. Recommended startup/listing order is the order above. There are currently no manifest-level required dependencies because API 0.6 has no cross-plugin service/query contract; in particular, permissions cannot yet enforce another plugin's commands.

Copy the five plugin directories into the configured `[plugins].directory` plugin root. The server discovers and loads them from that directory (strict deployments also list them under `plugins.expected`); there is no server-side bundled-selection list.

Intentionally omitted: giant command catalogs, multiworld/cross-dimension teleport, auctions, multiple currencies, nations/geopolitics/upkeep/war, bulk world editing, WorldGuard-style general regions, unbounded logs, and guessed rollback. WorldEdit/WorldGuard-like editing and general protection should remain separate utilities. Each plugin README states the exact current API limitation affecting its subset.
