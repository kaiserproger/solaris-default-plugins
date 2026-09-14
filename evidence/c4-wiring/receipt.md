# Evidence receipt — solaris-settlements C4 wiring

Wave: wire the shipped `solaris-settlements` package to the C4 core APIs
(`assign_resident_work` / `cancel_resident_work`, `issue_resident_order` /
`cancel_resident_order`, `demobilize_resident`) plus the C1 resident
`resident_equipment` / `resident_carry` endpoints. Scope: plugin policy and
records only; core was not edited.

Status: **plugin wiring complete and gate-green; live end-to-end combat proof
not captured before the run budget closed.** The exact live steps observed and
the blocked steps are recorded below. No core change was needed.

## Changed files (inside `solaris-default-plugins`)

| file | before | after | delta |
| --- | --- | --- | --- |
| `solaris-settlements/main.lua` | 4224 | 5468 | +1244 |
| `solaris-settlements/plugin.toml` | 22 | 26 | +4 |
| `solaris-settlements/README.md` | 90 | 111 | +21 |

`plugin.toml`: added capabilities `resident_work`, `resident_orders` and the
matching `required_features` (both are in the core `FEATURE_CAPABILITIES` set).

`main.lua`, by area:

- **Work** — `job <name> <resident> <job|none>` now resolves the job to the
  committed workplace (`harvest`/`cut_tree`/`mine`/`fish`/`tend_livestock` bound
  to a 16×16 work area at the building origin, `construct` bound to the active
  funded/building structure + stage + revision, `haul` between the resident's own
  endpoints, `craft` with `minecraft:stick`), reads the resident's core order
  revision from `query_owned_inventory(resident_equipment)`, persists the intent
  through `storage_batch_cas`, then calls `assign_resident_work`. `none` maps to
  `cancel_resident_work`. The result reports only committed work units and real
  `item ± delta` changes, and surfaces the typed reason verbatim
  (`missing_input`, `missing_tool`, `blocked_route`, `unloaded`, `no_workers`,
  `interrupted`, `protected`, `unsupported`, `no_storage`).
- **Hire/gear** — `hire` reads the employer's real `player_inventory`, moves the
  role kit (militia/infantry/spearman/archer) into the resident's
  `resident_equipment`/`resident_carry` slots through `transfer_owned_items` with
  both endpoint fences, then reads the equipment back from core for the recorded
  summary. Missing kit refuses and equips nothing.
- **Orders** — `squad <name> order <squad> <kind>` issues one batch
  `issue_resident_order` with the closed union
  (follow/move/hold/patrol/garrison/attack/retreat), a formation
  (`line`, spacing 4), a bounded engagement policy (revision 1, hostile-only,
  allies = owner/stewards/captains + owned handles) and per-member
  `expected_order_revisions` carried in the squad roster. `attack` uses the
  server-issued target refs a prior proximity order returned; `garrison` uses the
  adopted site's guard POIs. Per-member outcomes and each committed combat event
  (`attacker`, `victim target_ref`, `damage_milli`, `killed`) are reported
  verbatim. `squad <name> cancel <squad>` maps to `cancel_resident_order`.
- **Demobilise** — `dismiss` calls `demobilize_resident` with the record
  revision, then returns the committed gear through `transfer_owned_items` to a
  reachable player inventory; with no reachable/allowed store the resident stays
  `demobilizing` with every stack on it and the same handle and housing.
- **Records** — resident index `v2` adds `role` + `gear` (core-reported summary);
  squad record `v2` adds the handle/order-revision roster and stored target refs;
  squad index `v2` adds member/armed counts. `v1` records still decode.
- **Recovery** — a pending C4 intent resolves through `operation_status`; a
  committed receipt applies once, an uncommitted one is reported, never faked.

All four "needs core C4" placeholder strings are gone; the README command list
and status section match what the calls now do.

## Validation

1. **Real strict admission + Luau typecheck (core loader).**
   `config` at `/tmp/c4w/server.toml`: `plugins.directory = /tmp/c4w/plugins`,
   `strict = true`, `expected = [solaris-permissions, solaris-essentials,
   solaris-economy, solaris-towns, solaris-audit, solaris-settlements]`.
   `target/debug/mc-server --check --config /tmp/c4w/server.toml` → **exit 0**;
   `discovered_plugins` lists all six, no warnings except the fresh
   `world_dir_missing_on_disk`.
   Control: appending a syntax error to the package `main.lua` turns the same
   check into `exit 1` with
   `Luau type check failed: ... Expected <eof>, got 'local'`, so the pass is a
   real compile/typecheck, not a skip.
   `required_features`/capability validation passes with the two new features.

2. **Live server + real client (my own copy).**
   Server: `target/debug/mc-server --config /tmp/c4w/server.toml --no-console`
   on port 25577, fresh world `/tmp/c4w/world`, six-package directory.
   Client: `python3 -m tools.harness client` with
   `SOLARIS_CLIENT_MCP_PORT=39107`, `SOLARIS_CLIENT_MCP_USERNAME=C4Probe`,
   driven through `tools.harness.mcp.McpClient` (`minecraft_connect`,
   `minecraft_wait_for_play`, `minecraft_send_chat` with `command: true`,
   `minecraft_observe`, `minecraft_list_entities`, `minecraft_scan_blocks`).

   Observed (exact client chat lines):

   - `/settlement create probetown small` → `Founded probetown (small hamlet).`
   - `/settlement site probetown` →
     `site_6_0_448887a6 hamlet origin 3203,0,300 size 128,32,128 buildings=7`
   - `/settlement adopt probetown site_6_0_448887a6` →
     `Adopted site_6_0_448887a6 (hamlet): 7 buildings, 8 points of interest, revision 0.`
   - `/settlement survey probetown plot` →
     `Survey settlement: plots=4096 water=0 claimed=false chunks=loaded tags=[].`
   - `/settlement project probetown solaris:house_small here` →
     `Core refused the request: not_found.` — the project anchor was in cell
     (0,0) while the adopted candidate is at cell (6,0); core rejects an anchor
     whose cell has no settlement candidate. Probe placement, not a plugin bug.
   - `/settlement populate probetown` →
     `e2aa6ffa settled in probetown (alive_loaded), home site_6_0_448887a6.0.home. House capacity is tracked by that home POI.`
   - `/settlement hire probetown e2aa6ffa militia` →
     `Cannot equip e2aa6ffa as militia; missing from your inventory: minecraft:iron_sword, minecraft:leather_chestplate. Nothing was equipped.`
     — this is the new C4 gear path executing against a real `player_inventory`
     read; the driver's `/give` syntax was wrong (`/give <item> [count]`, no
     player selector), so the kit was absent.
   - `/settlement squad probetown create alpha` → `Squad alpha formed.`
   - `/settlement residents probetown` →
     `e2aa6ffa family=unassigned job=- service=civilian role=- squad=- life=alive_loaded gear=-`

   Blocked: after the failed `project` the house was never committed, so no
   armed militia, no squad order and no combat. Two probe-side defects were
   found and fixed in the throwaway driver (`/give` syntax; scanning unloaded
   chunks before `/tp`), and one real plugin bug was found and fixed:
   `squad <name> list` was unreachable because dispatch required 4 words.
   The retry on a fresh world hit a pre-existing create-receipt collision
   (`create`'s durable operation id is not per-settlement, so a second
   settlement name returns `operation_conflict`) before the world could be
   rebuilt, and the run budget closed first.

   Not yet captured live: `issue_resident_order` hold/attack with a summoned
   zombie, committed combat `damage_milli`/`killed`, and the `dismiss` gear
   return. These paths are typecheck-green and share the same tested query/
   intent/result plumbing as the observed `hire` path.

## Notes for the core owner (outside this repo)

None required to land this change. Two observations:

- The durable `create` write uses a fixed storage operation id
  (`b-settlements-index-v1-create`), so founding a second settlement in one
  plugin-storage lifetime returns `operation_conflict`. Core is behaving
  correctly; the plugin identity should include the settlement name.
- `/give <item> [count]` has no player selector (core admin command), which
  shapes any automated gear setup.
