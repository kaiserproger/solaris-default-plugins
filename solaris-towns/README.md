# Solaris Towns

**Deployment: Server-only.** A deliberately small Towny-like layer: `/town create`, `invite`, `join`, `leave`, `info`, `role`, `claim`, `unclaim`, `trust`, `untrust` and `flag`. Towns, roles, trusted guests and whole-chunk claims are durable and strictly bounded. Invitations expire on simulation-tick timers.

Each claimed chunk is a native protected zone whose allow-list is the town's leader, officers and members plus its trusted guests (at most `maximum_trusted_per_town`, 16 maximum; the zone owner admits 32 actors). The server enforces it natively for everyone else: no block edits, no opening chests or other containers, no lighting fire or TNT, no entity interaction, and no damage to players or non-hostile entities; explosions and fire never destroy or spread into claimed blocks. Server operators are always admitted.

- `/town trust <player|uuid>` / `/town untrust <player|uuid>` (leader or officer): add or remove a guest. A name must belong to an online player; a UUID works offline.
- `/town flag use <on|off>` (leader): let everyone use doors, trapdoors, fence gates, buttons and levers in the claims (never containers).
- `/town info [town]` reports members, claims with dimension and chunk coordinates, trusted guests and the door flag.

Every trust, flag, join or leave change commits durably and then rewrites each claim's zone; a refused rewrite rolls the change back. Claims and unclaims use the command player's server-authoritative dimension; the same chunk coordinates in different dimensions are independent. `minimum_y` and `maximum_y` apply to every dimension. The old `dimension` configuration field, if present, is ignored.

Storage is `towns-v1`: `v3` rows record each claim's dimension, while older `v1` and `v2` claims without a dimension are interpreted as `minecraft:overworld` regardless of configuration. Existing Overworld zone IDs are retained on migration. Geopolitics, upkeep, war, taxes, and nation hierarchies are intentionally omitted.
