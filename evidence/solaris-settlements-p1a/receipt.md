# Evidence receipt — solaris-settlements P1-a (settlement domain ledger)

Slice: contract `SETTLEMENT_OVERHAUL_CONTRACT.md` P1, first buildable sub-slice
on API 0.6.0 — sections 3.1 (axes), 3.2 (growth gates), 3.3 (owner/steward/
captain roles), 7 (`settlement:<id>` records + bounded shard index).
No new host functions, no worldgen selector, no client bundle.

## Changed files (all inside solaris-default-plugins)

- `solaris-settlements/plugin.toml` (new): id solaris-settlements,
  api 0.6.0, events [server.started], capabilities [storage],
  player_commands [settlement].
- `solaris-settlements/config.toml` (new): maximum_settlements = 24.
- `solaris-settlements/main.lua` (new, ~760 lines): axes validation, gate
  table, per-record CAS with copy-on-write, index-first create / record-first
  abandon with loader self-heal, owner/steward fencing, 10 subcommands.
- `solaris-settlements/README.md` (new): scope, commands, deployment rule,
  known core gaps.
- `README.md` (+2 lines): package list entry.

## Validation (scoped; no project-wide suites, no core modifications)

1. Strict discovery + manifest + Luau typecheck via the real core loader
   (`mc-script::prepare_lua_plugins`, strict_discovery(true)) in
   /tmp/settle-strict (path-dep, read-only):
   - `cargo run --offline -q -- valid`
     -> `STRICT-OK loaded=1 id=solaris-settlements no-worldgen-selector`
   - `cargo run --offline -q -- bad-capability`
     -> fail-closed `unknown plugin capability "structure_operations"`
   - `cargo run --offline -q -- stray-entry`
     -> fail-closed `strict discovery permits only plugin directories`
2. Behavior: the shipped main.lua executed under mlua/luau against a stub
   `solaris` host with async result-event delivery, CAS versions, and a
   persistent store across fresh VMs (/tmp/settle-behavior):
   - `cargo run --offline -q` -> `BEHAVIOR-OK checks=40`
   - covers: empty boot, invalid create (no writes), create, duplicate,
     info axes/gates, gated advance, stranger fencing, steward grant +
     steward survey, hamlet->village, specs (2 ok / 3-by-shape / unknown),
     ruin blocks / restore unblocks, village->developed, branch-choice
     required, developed->fortress L1, CAS conflict retry with committed
     state intact, restart recovery (record+specs+survey), owner-fenced
     abandon (index + record removed), no orphan after restart.

## Known bug found by the suite (fixed)

- `survey` arity guard was `#words == 10`, correct is 9 (action + name +
  7 values). Caught by "stranger survey rejected" falling through to usage.

## Blocked on core (C1), recorded not worked around

- Atomic index+record commit needs `solaris.storage_batch_cas`
  (contract 7, queue C1). Current ordering (index-first create,
  record-first abandon) plus loader self-heal converges but is not atomic.
- `survey_site` / `prepare_structure` / resident handles / work orders /
  transfers / client views need C2/C3/C4/L1. Survey numbers here are
  operator-reported by design; nothing references the proposed APIs.

## Next slice

P1-b: construction intent ledger (cost estimate + material reserve intent
records referencing `structure_id` placeholders) — still 0.6.0-safe, but
most value lands after C1 batch CAS exists. Recommend C1 first.
