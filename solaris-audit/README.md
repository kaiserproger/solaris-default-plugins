# Solaris Audit

**Deployment: Server-only.** A bounded append-oriented ring records committed block place/break plus the craft, pickup, kill, living-entity interaction, and death actions exposed by API 0.6. Operators use `/audit [count]`, `/audit actor <uuid> [count]`, `/audit here <radius> [count]`, or `/audit since <ticks> [count]`.

Each hot-path event performs O(1) bounded queue/ring work; lookup scans only the configured in-memory cap. Storage is one versioned record under the 4096-byte API value bound. Tick values are the latest delivered simulation tick and may skip intermediate ticks.

Container mutations have no committed plugin event, and block events do not include the exact prior block state/properties or block-entity data. `world_blocks` can only set a default block state. Exact rollback is therefore intentionally unavailable rather than guessed; the needed core API is a committed reversible mutation record plus a fenced rollback command.
