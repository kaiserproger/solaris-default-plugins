# Evidence receipt — solaris-settlements squad handle + order path

Scope: `solaris-settlements/main.lua` only. No core edit. No commit.

## What changed

| # | Defect | Fix |
| --- | --- | --- |
| 1 | `squad <name> add <squad> <resident>` printed nothing: `on_plugin_storage_get_result` routed **every** `kind == "load"` read into the index decoder, so command reads with a custom purpose (`squad-member`, `job-building`, `job-plan`, `plan-for-build`, `building-for-build`, `cycle-building`, `recover-resident`, `recover-squad`) were finished without ever reaching `handle_value_read`. The roster entry was therefore never written and no refusal ever surfaced. | Added `LOAD_INDEX_PURPOSES`; only `settlement/bidx/ridx/sidx/ops/survey/site` decode as indexes, every other `load` purpose falls through to `handle_value_read`. |
| 2 | `squad <name> list <squad>` could not show the member's core handle. | `list <squad>` now reads the squad record (`squad-list` purpose) and prints `name handle=<handle> order_revision=<n>` per member; unknown squad is typed. |
| 3 | `create`'s durable operation id was the fixed `b-settlements-index-v1-create`, so a second settlement name returned `operation_conflict`. | identity is now `"create-" .. sanitize_id(name)` (a retry of the same name still replays). |
| 4 | Operations index entries are keyed by `target` (that is what `encode_operations` writes) but `set_pending` / `clear_pending` / `operation_target_of` / `recover_next` looked them up as `.name`, so intents were never reused or cleared: after `maximum_pending_operations` (default 4) distinct intents every order died with "Too many pending operations". | New `S.pending_entry(index, target)` matches `.target`; used by all four call sites. |
| 5 | With #4 fixed, `S.operation_id` (serial = `#index + 1`) recycled ids as entries committed, and core returned `operation_conflict` for a new intent reusing a committed id. | `S.operation_id` now consumes the settlement record's monotonic `revision`; every intent gets a strictly increasing id that the following bundle write persists. |
| 6 | With #4 fixed, boot recovery actually ran; a committed `advance` receipt then reached `handle_advance_receipt` with `entry.building == nil` and the plugin was disabled (`attempt to concatenate string with nil`, host log `Lua plugin disabled after handler failure`). | The success path hydrates `entry.building` / `entry.resident` / `entry.squad` from the durable `entry.target` before dispatch, mirroring the failure path. |

## Plugin-side validation

- Source installed at `/tmp/p1/plugins/solaris-settlements` (six packages, `strict = true`,
  `expected = [solaris-permissions, solaris-essentials, solaris-economy, solaris-towns, solaris-audit, solaris-settlements]`).
- `/home/kaiserroman/solaris/target/debug/mc-server --check --config /tmp/p1/server.toml` → **exit 0**,
  `discovered_plugins` lists all six, no warnings.
- Broken control: same directory with `local broken_control =` appended to the package `main.lua`
  → **exit 1**, `Luau type check failed: ... Expected <eof>, got 'local'`. So the pass is a real
  Luau compile/typecheck, not a skip.

## Live probe (own server + real client)

Server `target/debug/mc-server --config /tmp/p1/server.toml` on port 25578, fresh world
`/tmp/p1/world` (seed 712816, `tellus_like`); client MCP on 39108, user `P1Probe`.
Raw output: `/tmp/p1/drive.out`, `/tmp/p1/drive2.out`, `/tmp/p1/combat-samples.json`.

Observed chat (exact lines):

- per-settlement create id:
  `Founded p2town (small hamlet).` / `Founded p3town (small hamlet).` /
  `That settlement name is taken.` (repeat of an existing name is still a typed refusal)
- `Adopted site_3_0_31f075c1 (village): 12 buildings, 8 points of interest, revision 0.`
- `Survey settlement: plots=4096 water=0 claimed=false chunks=loaded tags=[].`
- `house_sm_1 projected (solaris:house_small, 4 stages). ...`
- `Reserved real materials for house_sm_1 (9d04b2423579acbb7fac576db984b4b7). ...`
  → `house_sm_1 solaris:house_small committed`
- `a3051b3d settled in p1town (alive_loaded), home site_3_0_31f075c1.0.home. ...`
- `a3051b3d serves as militia (core equipment: iron_sword,leather_chestplate).`
- squad, the defect under test:
  - `Unknown squad or resident.` (typed refusal for a non-member — never silence)
  - `a3051b3d joined squad alpha.`
  - `a3051b3d handle=6b4a4c3076190c0012d71b2e90efb94a7932259dc5bc49dc8736e7aa7cbfa531 order_revision=0`
    and `alpha forming order=- role=- members=1 armed=1`
- order path, eight consecutive orders after the id fix (ids no longer collide):
  `Squad alpha order hold, revision 122..150: a3051b3d=applied#0; stored targets=0`
- hostile perception + engagement order committed:
  `Squad alpha order hold, revision 158: a3051b3d=blocked_route targets=1; stored targets=1`
  `Squad alpha order attack, revision 162: a3051b3d=applied#0 targets=1; stored targets=1`

## Still unproven (blocked by a core-side gap, not by the package)

`minecraft_list_entities` sampled every 2 s for 36 s after the committed `attack`
(`/tmp/p1/combat-samples.json`): the summoned zombie stays at **hp 20.0 / 20.0**, the militia
villager at hp 20.0, the player at hp 20.0. No `combat:` line was ever emitted, the hostile never
died, and `dismiss` therefore could not be shown returning the gear either (its reply came back
empty after the combat loop).

Root cause is upstream of this package: `SiteCandidate.origin[1]` is documented as
"the y coordinate is resolved by the caller" (`crates/mc-worldgen/src/settlement_sites.rs`), but
the caller (`site_snapshot`, `crates/mc-net/src/script/storage/settlement.rs`) passes `candidate.origin`
verbatim, so every site POI is built on the y=0 layout plane. In this world the terrain at the
adopted site is ~y 92, so:

- the site POIs and the resident spawn at y≈2: the resident entity
  (`2578da6b-b5a1-85fa-b9a1-8c4e159d1d9a`) sits at `(1793, 2, 64)` **inside solid stone**
  (client column scan: solid −64…92);
- `hold` reports `blocked_route` (no path/formation slot), and a hostile summoned 1.8 blocks away
  still yields no committed combat — the engine's melee/LOS never fires between two entities
  embedded in stone.

The package-side order chain is proven up to the engine boundary (`targets=1`, `applied#0`,
committed receipts); the committed-damage/kill sample needs the site y resolution in core (or a
world whose terrain is at the site layout plane) before it can be captured.

Also still unproven: `dismiss` gear return (needs the same reachable, combat-capable resident).
No claims are made for those two paths.
