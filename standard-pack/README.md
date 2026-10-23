# Solaris Standard Plugin Pack (alpha3)

This opt-in, first-party, lightweight pack provides common administration/gameplay workflows without cloning large Bukkit/Paper plugins:

- `solaris-permissions` — durable groups and permission-node catalog;
- `solaris-essentials` — homes, spawn/warps, back, TPA, and private messages;
- `solaris-economy` — the single virtual-money authority with idempotent payments;
- `solaris-towns` — small towns, invitations, roles, trusted guests, and chunk claims enforced natively against griefing;
- `solaris-audit` — bounded committed-action history and lookup.

All five are independent API 0.7 component packages and **server-only**. Commands work with a vanilla Minecraft 26.1.2 client; Solaris Loader UI could enhance presentation later but is never required. Recommended startup/listing order is the order above. There are currently no manifest-level required dependencies; the one cross-package contract is command permission admission: `solaris-permissions` declares `permission_provider = true` and publishes its group grants through the host's `publish-command-permissions` import, while a package such as `solaris-essentials` declares `command_permissions` for the roots it gates (`home`/`sethome` need `solaris.essentials.use`, `setwarp`/`delwarp` need `solaris.essentials.manage`). Deploy `solaris-permissions` with the pack or those declared nodes are inert, exactly as before this contract.

Install explicitly with `bash install.sh --directory /path/to/server/plugins`
from the repository root; see the [deployment guide](../README.md#deployment-workflow).
The installer selects these five packages by default and refuses existing
package paths. Set the same `[plugins].directory`, enable `plugins.strict`,
list every deployed id under `plugins.expected`, and run `solaris --check`.
There is no server-side bundled-selection list.

For source verification, run
`python3 tools/build_standard_pack.py --core-root /path/to/solaris --check`
from this repository. It independently builds all seven shipped Rust guests
(the five standard packages plus optional `basic-economy` and `solaris-auth`) and encodes each
component through that core checkout's contract encoder. The optional `chisel`
addon is not in that selection; build it with `--package chisel`. `--write`
explicitly refreshes the tracked `plugin.wasm` outputs after native candidate validation.
Rebuild every component after SDK/WIT changes, even when API stays `0.7.0`;
matching manifest versions do not prove matching Wasm function types.

Intentionally omitted: giant command catalogs, multiworld/cross-dimension teleport, auctions, multiple currencies, nations/geopolitics/upkeep/war, bulk world editing, WorldGuard-style general regions, unbounded logs, and guessed rollback. WorldEdit/WorldGuard-like editing and general protection should remain separate utilities. Each plugin README states the exact current API limitation affecting its subset.
