# Evidence receipt — solaris-settlements v1 (server-side settlement package)

Wave: contract `SETTLEMENT_OVERHAUL_CONTRACT.md` P1 (v1, server-side only) plus
the clean cutover away from the two superseded prototype packages. C4 is landing
in a sibling agent in the same wave; its work-order and squad-order calls did not
exist when this package was written (see "Core calls not present yet").

## Changed files (all inside solaris-default-plugins)

- `solaris-settlements/plugin.toml` — strict manifest: `events=[server.started]`,
  capabilities `storage`, `storage_batches`, `inventory_transfers`,
  `persistent_residents`, `world_sites`, `structure_operations`,
  `player_queries`, matching `required_features`, command root `settlement`.
  No `[client]`, no Loader permission, no worldgen selector (`grep -in
  "client|loader|permission|worldgen"` on the manifest/config returns nothing).
- `solaris-settlements/config.toml` — dimension, bounded maximums, cycle cadence,
  food/money item ids.
- `solaris-settlements/main.lua` (~2.6k lines, `--!strict`) — records
  (`settlement:`, `building:`, `plan:`, `resident:`, `squad:`, `operations`,
  `survey:`, `site:`) with schema tags and revisions, sharded per settlement and
  written with derived indexes in one atomic `storage_batch_cas`; growth gates
  on committed buildings/population/jobs/verified supply; economy that never
  mints (C1 reservation + receipts + verified inventory projections);
  population/families/jobs; recruitment/demobilisation records; squad records;
  simulation-tick cycle with explicit pause reasons; durable operation intents
  resolved by `operation_status` after restart.
- `solaris-settlements/structures/*.toml` — 22 authored blueprints (schema 1),
  989 KB decoded, real doors/interiors/POIs/street connections, 3–4 stages each,
  `keep.ruined` as the restoration variant.
- `tools/gen_structures.py` — deterministic generator (`--check` is clean).
- `README.md` — package list and the verified deployed set (five standard
  packages plus `solaris-settlements`).
- Deleted: `colony-villager-scaffold/`, `settlement-prototype/` (clean cutover;
  no aliases, no second roster authority).

## Validation (scoped; no project-wide suites, no core edits)

1. **Authored data against the real core decoder.**
   `cd /tmp/settlement-catalog-validate && cargo run --offline -q`
   → `CATALOG-OK blueprints=22 layouts=3326` (hamlet 2160 / village 992 /
   town 174 candidates laid out). The validator uses the real
   `BlueprintCatalog::from_files` with `BlockRegistry::from_report(
   solaris_required_blocks_report())` and asserts stage unions, <=16 block ids,
   home capacity, entrances, and that every Hamlet/Village/Town candidate in
   cells x,z in -20..=20 for seeds 0..15 lays out `Ok`.
   `python3 tools/gen_structures.py --check` → `catalog is current: 22 blueprints`
   (exit 0).

2. **Strict load of the shipped package (real loader).**
   `cargo run --offline -q` in `/tmp/settle-behavior` prints
   `STRICT-ADMISSION ok plugins=1` from
   `mc_script::prepare_lua_plugins(LuaHostConfig::new(pkg).strict_discovery(true))`:
   the real manifest validation, `required_features` checks and the `--!strict`
   Luau typecheck all pass for the exact committed files.

3. **Growth sequence through the API surface with typed results.**
   The same harness starts the real Luau host, answers every host call with real
   `mc-script` DTOs and drives `/settlement` commands:
   `create → site → adopt → populate → hire → residents → job → specialize →
   dismiss → survey → project → fund → build → info`.
   Observed (final line `BEHAVIOR-OK`):
   - `Sites`/`Site` pages and a `ResidentSite` reservation (`revision 1` → `2`);
   - `spawn` resident snapshot, then hire/dismiss keep the same handle;
   - `Survey` snapshot with terrain tags; `specialize` still refused because the
     workplace is not committed (resources AND jobs required);
   - `PrepareStructure` → 4 stages (99/144/99/14 blocks = 356 units total);
   - `reserve_inventory_items` accepted the plugin's plan **byte-identical to
     core's `structure_resource_plan`** (`plan_match=true`), and every receipt's
     consumed materials stayed inside the reservation;
   - 4 `Receipt`s (`state=running` → `committed`), then `structure_status`
     verification → `house_sm_1 committed (solaris:house_small) at revision 5`;
   - `info` reports the next gate as `houses 1/9, residents 1/24, jobs 0/12,
     food 0/64, committed meeting hall` — grounded in committed facts only.
   Observed counters: `OBSERVED jobs=25 receipts=4 plan_match=true
   consumed_units=356 records=11 messages=15`.

4. **No Loader capability is declared or required.** Static check above; the
   manifest has no `[client]` section and `discovered_plugins()` reports
   `ServerOnly` for the package.

## Core calls needed that do not exist yet (reported, not faked)

- `assign_resident_work` / `cancel_resident_work` (C4): jobs are records only;
  `job` tells the player physical production needs the call.
- `issue_resident_order` / `cancel_resident_order` (C4): squads store
  membership/formation/order intent; no movement or combat is executed.
- A writable `warehouse` inventory endpoint (C1): `deposit` refuses; supply is a
  projection of the carrier's canonical inventory, zeroed when stale.
- Package `structures/*.toml` discovery plus the `feudal_settlements` worldgen
  selector (C2 wiring): `SettlementRuntime` is never attached in
  `mc-net/src/server.rs`, so every settlement/structure/resident operation
  answers `runtime_unavailable` on a real server today. The plugin surfaces that
  reason verbatim instead of pretending success.
- Any money authority: `solaris-economy` owns virtual money and API 0.6.0 has no
  cross-plugin service contract.

## Notes for the core owner (outside this repo)

Deleting the two prototype packages invalidates core-owned references, which are
fixed in core per contract §9: `crates/mc-server/src/main.rs` tests
(`settlement-prototype` at ~2111, `colony-villager-scaffold` at ~2133),
`crates/mc-test-harness/tests/plugin_examples.rs` (colony rows at 949/1115/1247/
1390/1557) and `docs/PLUGINS.md` (deployment table and prototype paragraphs).

While this wave ran, `crates/mc-script` was mid-flight non-compiling (the C4
sibling adding `ResidentWork`/`ResidentOrders` capability variants and unfinished
Lua binding argument lists). Validation therefore ran against a scratch copy at
`/tmp/solaris-snapshot` whose only differences are mechanical completions of
those in-flight edits (one match arm plus four arity fixes). Re-run the two
commands above against the final core tree once C4 lands.
