# Solaris Audit

**Deployment: Server-only.** A bounded append-oriented ring records committed block place/break plus the craft, pickup, kill, living-entity interaction, and death actions exposed by component API 0.8. Operators use `/audit [count]`, `/audit actor <uuid> [count]`, `/audit here <radius> [count]`, or `/audit since <ticks> [count]`.

Each hot-path event performs O(1) bounded queue/ring work; lookup scans only the configured in-memory cap. Storage is one versioned record under the 4096-byte API value bound. Tick values are the latest delivered simulation tick and may skip intermediate ticks.

Container mutations have no committed plugin event, and block events do not include the exact prior block state/properties or block-entity data, so this plugin cannot roll anything back. Exact block and container rollback is native in the core: operators use `/co inspect`, `/co lookup`, `/co rollback` and `/co restore` (Solaris `docs/OPERATING.md`, "Block log and rollback"), whose attributed pre-images, retention and fenced rollback live beside the world state. This plugin stays a lightweight action feed for the other events above.
