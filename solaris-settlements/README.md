# Solaris Settlements (v1)

The single settlement/profile package for API `0.6.0`: one owner of settlement
records, growth, economy accounting, population, recruitment records, squads and
the authored blueprint catalog. It replaces `colony-villager-scaffold` and
`settlement-prototype`; a deployed set must hold exactly one settlement profile.

**Server data plus verified client content.** The manifest declares one `[client]`
bundle (`client/settlements-ui.zip`, schema 2) with `views`/`view_actions`
content and the `present_views`/`send_view_actions` permissions. It declares one
`settlement` screen, `solaris-settlements:overview`, which the plugin opens for a
member from the client's settlement key or from `/settlement overview [name]`.
The screen shows only what this package actually holds or reads: the settlement
identity, the resident roster, the cycle's stop reason and the confirmed
contents of the warehouse container core bound for the package. Without a Loader
that activates the bundle the plugin still runs and `/settlement` still works;
only the screen is missing. World previews, entity presentation and keybinds
remain absent from v1 and frozen in the Loader repository.

## Full overhaul specification

The proposed client + server X–XV-century overhaul is specified in
[gameplay and campaign design](../docs/settlements/SPECIFICATION.md),
[equipment and production content](../docs/settlements/CONTENT_SPEC.md), and
[implementation contracts and acceptance](../docs/settlements/IMPLEMENTATION_SPEC.md).
These documents extend the earlier upstream contract; they do not claim the
server-only package below already implements the full overhaul. Specification
files live outside this deployable package so installation does not copy them
as runtime content.

## Contents

- `plugin.toml` — strict manifest: `server.started`, `storage`,
  `storage_batches`, `inventory_transfers`, `persistent_residents`,
  `resident_work`, `resident_orders`, `world_sites`, `structure_operations`,
  `player_queries` (with the matching `required_features`), command root
  `settlement`, and one schema-2 `[client]` bundle whose `sha256`/`size_bytes`
  are the shipped bytes of `client/settlements-ui.zip`.
- `client/settlements-ui.zip` — deterministic Loader artifact: index schema 2
  first, one `settlement` screen with a `paged_table`, a `resource_panel` and
  `refresh`/`page_next`/`page_prev` action buttons.
- `config.toml` — operator configuration: dimension, bounded maximums, cycle
  cadence, food and money item ids.
- `structures/*.toml` — 22 authored blueprints (schema 1 from the frozen
  contract): two house sizes, well/plaza, market, warehouse, farm, sawmill, pen,
  fishing pier, mine entrance, smithy, barracks, watch post, palisade gate,
  stone wall, stone tower, watchtower, manor hall, keep, `keep.ruined`, town
  hall, library. Each has real doors, walkable interiors, POIs and street
  connections, and 3–4 ordered construction stages.
- `../tools/gen_structures.py` — deterministic generator behind those files
  (`--write` regenerates, `--check` fails if the committed data drifts).

## Commands

```
list | info [name] | create <name> <small|medium|large> | abandon <name>
site [name] | adopt <name> <site_id> | survey <name> [plot|expand|restore]
project <name> <blueprint> here|<x> <y> <z> [0|90|180|270]
fund <name> <building> | build <name> <building> | pause|cancel <name> <building>
buildings <name> | promote <name> | branch <name> <estate|fortress|town>
specialize <name> [spec [spec]] | ruin|restore <name>
populate <name> | claim <name> <entity_uuid> | residents <name>
overview [name] | family <name> <resident> <family> | job <name> <resident> <job|none>
hire <name> <resident> <militia|infantry|spearman|archer> | dismiss <name> <resident>
squad <name> create|add|order|cancel|list ... | supply <name> | deposit <name>
role <name> <uuid> <steward|captain|member>
```

`job` assigns a real physical work order bound to the committed workplace
(harvest/cut_tree/mine/fish/tend_livestock/construct/haul/craft) and reports the
committed units, item deltas, and the typed stop reason (`missing_input`,
`missing_tool`, `blocked_route`, `unloaded`, `no_workers`, `interrupted`,
`protected`, `unsupported`, `no_storage`); `none` cancels the assignment.

`hire` draws the role's kit from the employer's real inventory and moves it into
the resident's core equipment/carry slots through `transfer_owned_items`; the
recorded summary is read back from the core inventory snapshot. `dismiss`
demobilises the resident through core, keeps the same handle and housing, and
returns the committed gear through a real transfer — if no store can take the
items the resident stays `demobilizing` with every stack intact.

`squad ... order <follow|move|hold|patrol|garrison|attack|retreat>` issues one
durable group order; per-member outcomes and committed combat events are
reported verbatim. `attack` uses the server-issued target references a previous
`hold`/`patrol`/`garrison` perception returned; `garrison` occupies the site's
approved guard posts; `cancel` maps to `cancel_resident_order`.

## Growth, economy and save discipline

- Growth is survey → project → reserve real materials → staged construction →
  verification → population/jobs → level recompute. A level or branch level only
  rises after the required buildings are **committed** and the population and
  supply conditions hold; a name or a payment alone never advances anything.
- Nothing mints resources. Construction spends a C1 inventory reservation bound
  to the core structure plan; receipts are applied once (watermark guard);
  verified supply is a projection of a real `query_owned_inventory` read at a
  stated tick and is zeroed when it goes stale; the treasury moves only by
  verified transfer, and taxes redistribute an existing balance.
- Every record is versioned (`v1`/`v2` schema tag plus a plugin revision) and
  sharded per settlement; indexes are derived projections written in the same
  atomic `storage_batch_cas` as their record. Pending operations are durable:
  after a restart they are resolved with `operation_status` before any new
  effect, so confirmed work is never charged twice.
- Economic cycles run on simulation ticks. An unloaded settlement produces
  nothing, and the pause reason is explicit and visible in `info`:
  `unloaded`, `no_workers`, `missing_input`, `blocked_route`, `interrupted`.

## Not implemented in v1 (reported, never faked)

- A deposit *transfer* into the settlement warehouse: the core `warehouse`
  inventory endpoint exists and this package binds the committed
  `solaris:warehouse` structure and reads its container for the overview, but no
  command moves an item into it yet, so `deposit` refuses, `haul` moves between
  the resident's own canonical endpoints, and soldier gear returns to a
  reachable player inventory.
- Package `structures/*.toml` discovery and the `feudal_settlements` selector
  (core C2 wiring): until the core runtime catalog is installed every
  settlement/structure call answers `runtime_unavailable` and the plugin says so
  instead of pretending success.
- Any money authority: `solaris-economy` owns virtual money and API `0.6.0` has
  no cross-plugin service contract.

Core C4 is wired: `assign_resident_work`/`cancel_resident_work`,
`issue_resident_order`/`cancel_resident_order` and `demobilize_resident` execute
the recorded job, squad and demobilisation state. `hire`/`dismiss` preserve the
**same** resident handle.

## Deployment

```sh
bash install.sh --directory /path/to/server/plugins solaris-settlements
```

Never deploy it beside another settlement package. See the repository README for
the verified deployed set.
