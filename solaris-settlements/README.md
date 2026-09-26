# Solaris Settlements

`solaris-settlements` is the first-party API 0.7 WebAssembly component that owns
its settlement command flow. The deployable directory contains only the package
manifest, `plugin.wasm`, this README, and its authored `structures/` catalog.
Core supplies generic world, inventory, resident, structure, storage, receipt,
and fence capabilities; it contains no settlement policy or product source.

## Commands

The component exposes the initial durable `/settlement` product routes:

```text
create <name> <small|medium|large>
list
info [name]
role <name> <player> <steward|captain|member>
ruin <name>
restore <name>
site <name>
adopt <name> <site-id>
survey <name> [plot|expand|restore]
project <name> <blueprint-id> [x y z]
fund <name> <building>
advance <name> <building>
specialize <name> [farming|forestry|ranching|fishing|mining] [second specialty]
populate <name>
hire <name> <resident-alias> <militia|infantry|spearman|archer|medic>
defend <name> <resident-alias> <order-revision> <post-x> <post-y> <post-z> <rally-x> <rally-y> <rally-z> [officer-alias]
march <name> <alias:order-revision,alias:order-revision> <x> <y> <z>
retreat <name> <alias:order-revision,alias:order-revision> <x> <y> <z>
halt <name> <alias:order-revision,alias:order-revision>
treat <name> <resident-alias>
evacuate <name> <medic-alias> <medic-order-revision> <patient-alias> <patient-order-revision> <x> <y> <z>
dismiss <name> <resident-alias>
capture <captor-name> <guard-alias> <opposing-name> <resident-alias> <resident-order-revision>
```

Creation, membership roles, adoption, settlement condition, and named project
selection are owner-scoped records in component storage. `site` reads generic
core sites. Adoption currently accepts only core-authored `site_*` entries:
generated village sites cannot yet reserve a resident home through the generic
resident authority. `project` re-reads its exact adopted site before preparing
a structure, so it uses a current core revision fence. After core commits the
plan, the component stores it under a generated, bounded building name such as
`warehouse_1`; `info <name>` lists those names. `fund <name> <building>`
authorizes an owner or steward, reads that selected structure's current core
status, then reserves the caller's inventory against its authoritative material
plan. `advance <name> <building>` performs the same owner/steward and exact
structure read before committing the next bounded core work portion.
`specialize <name> ...` lets an owner or steward select up to two specialties:
each needs a persisted matching survey tag and a matching named building whose
current core state is committed. Omitting specialties clears them. `populate
<name>` lets any settlement member select one free home through an exact
adopted `site_*` query, reserve it in core, and spawn a core-managed resident.
The component durably indexes that resident under a bounded generated alias
such as `resident_1`, including its core handle and initial work revision `0`;
`info <name>` prints `alias=handle@revision:role`. `hire <name>
<resident-alias> <role>` lets an owner or steward employ an existing live
resident from that settlement; it never spawns another villager. Bind a
warehouse belonging to the chosen settlement first. Militia, infantry,
spearman and archer each take their real equipment from its container, plus
one emerald signing wage delivered to the resident's carry (slot 1 for an
archer, whose 32 arrows occupy slot 0). Missing stock, an occupied destination
slot, an already employed resident or a competing employer is refused without
granting a role. The component persists a fenced hiring intent before the
native atomic warehouse/gear transfer and finalizes the employer/role only
after its receipt; reload replays that same transfer id before finalization.
Before saving the roster revision it re-reads the resident gear, including any
native paused-work resume that the transfer awakened.
This is a one-time signing wage, not recurring payroll. Work assignments and
resident identity survive the gear transfer. Each accepted work response
reports the revision required by its immediate successor; the component does
not yet recover an in-progress work revision after restart.

A medic is a hired, noncombat role with an emerald signing wage; it cannot
serve as a guard or attacker. `evacuate` requires a living wounded resident
and a living medic within four blocks, then submits both original handles in
one native Move order. Only `2/2 applied` means the movement was admitted;
partial/blocked reports do not claim evacuation. Residents walk independently
on the native terrain and can separate or be interrupted; this is ambulatory
movement, not a stretcher or guaranteed arrival. Equipment and injuries
remain on the original patient. `treat` consumes the warehouse bread through
core's durable health/debit decision; evacuation alone does not heal.

`defend` lets an owner or steward assign one hired resident a hold anchor;
the hold order does not relocate it to that coordinate.
The supplied order revision is `0` for its first order and the revision
reported by the previous command thereafter. The component holds position,
selects one server-issued hostile reference if one is visible, and issues a
second native attack order using the returned policy/expiry fences. The
server checks the live weapon, target, reach, line of sight, cooldown and
ammunition; bows create a world arrow whose hit arrives later through native
projectile physics, not as immediate order damage. An accepted attack can
continue after the guard moves into reach on native simulation ticks, without
per-tick guest commands. Without a visible hostile, the guard stays at its
current position. An interrupted hold-to-attack continuation does not recover
automatically yet; reissue with the current order revision.
The rally anchor must be distinct, standable and reachable. Native morale
advances on newly witnessed injury, allied loss, flank exposure or assigned
officer loss; a routing guard walks toward the rally anchor and only rallies
there (with the named officer nearby, if supplied). It does not teleport or
resume pursuit merely because another tick elapsed.

`capture` requires a serving guard of the owner's or steward's settlement
physically within three blocks of a living, routing attacker from a different
owner's settlement. Native capture stops that resident's order without copying
its UUID or gear. A durable guest intent removes the victim from its old army
before the native decision; restart replays the same fenced capture, then one
registry CAS records custody on the original resident entry. A distant guard
or a native refusal restores the former role. Prisoners cannot be hired,
ordered or demobilized while captive. This command is a physical custody
operation, not a war declaration or permission to attack after a truce.

`march`, `retreat`, and `halt` accept one to 32 distinct hired aliases, each
paired with its current native order revision (`0` before its first order).
Only the owner or a steward of an active settlement may issue the command.
`march` issues one overworld column-formation movement order for the selected
roster; `retreat` replaces pursuit with native movement toward the chosen rally
anchor, without teleporting; `halt` cancels the selected roster's orders.
The response gives the new shared order
revision and counts applied members, listing blocked routes separately. A
blocked route does not revoke the atomic group acceptance: reachable members
still walk to distinct positions, while nobody teleports. Repeat a command
with the reported revisions; changing the roster, obstacle, or destination
replans only the selected members. A resident omitted from a later group
order retains its prior order. Reopening an accepted movement journal restores
the original per-member destination, not a shared anchor.

Core remains the authority for material consumption, structures, container contents,
resident state, world edits, transaction receipts, and stale-fence refusals.
`fund-warehouse` reads the currently bound warehouse and reserves the prepared
structure's exact material plan from it; the core-held reservation prevents
other withdrawals from consuming the promised stock.

The lower-level component operations remain available for direct diagnosis and
composition:

```text
homes
survey
prepare <blueprint-id> <x> <y> <z>
status <structure-id>
fund
fund-warehouse
advance
spawn <poi-id>
bind <structure-id> <container-id>
bind-village <site-id> <container-id>
haul <resident-handle> <work-revision> [item-id]
issue <resident-handle> <work-revision> <carry|equipment> [item-id]
craft <resident-handle> <work-revision> <recipe-id> <count> <x> <y> <z>
harvest <resident-handle> <work-revision> <tool-id> <min-x> <min-y> <min-z> <max-x> <max-y> <max-z>
```

`homes` reports each free point as `home=<poi-id>@<x>,<y>,<z>` so an operator
can inspect and load its candidate chunk. A successful spawn also requires a
physical floor with clear feet and head; a selected site layout alone does not
materialize housing.

## Migration boundary

`sources/solaris-settlements/legacy/main.lua` preserves the prior Luau product
source as porting input only. It is neither deployed nor an alternate
entrypoint. The component has ported the initial durable owner, membership,
condition, site-adoption, survey, named project, funding, advancement,
specialization, named core-authored-site population and live-resident hiring
routes. Population on generated villages, families, recurring jobs/payroll,
squads, claims, and abandonment remain porting work; this package does not
claim those commands as implemented.

## Source and build

Reproducible Rust source lives at `sources/solaris-settlements/`, outside the
deployable package directory. Build it against the sibling core SDK, then encode
the resulting core Wasm module as a component:

```sh
cargo build --manifest-path sources/solaris-settlements/Cargo.toml \
  --target wasm32-unknown-unknown --release
cargo run --manifest-path ../solaris/crates/mc-plugin-host/Cargo.toml \
  --example plugin_component -- \
  sources/solaris-settlements/target/wasm32-unknown-unknown/release/solaris_settlements_plugin.wasm \
  solaris-settlements/plugin.wasm
```

Run `solaris --check --config server.toml` against a deployment that grants the
manifest capabilities before starting the server.

## Deployment

```sh
bash install.sh --directory /path/to/server/plugins solaris-settlements
```

Deploy at most one settlement profile. Vanilla clients can use the command
without the optional Loader view bundle; the package also ships that bundle
for compatible loaders. This does not establish graphical acceptance.
