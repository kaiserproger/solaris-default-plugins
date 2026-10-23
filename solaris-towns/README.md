# Solaris Towns

**Deployment: Server-only.** A deliberately small Towny-like layer: `/town create`, `invite`, `join`, `leave`, `info`, `role`, `claim`, `unclaim`, `trust`, `untrust` and `flag`. Towns, roles, trusted guests and whole-chunk claims are durable and strictly bounded. Invitations expire on simulation-tick timers.

Each claimed chunk is a native protected zone whose allow-list is the town's leader, officers and members plus its trusted guests (at most `maximum_trusted_per_town`, 16 maximum; the zone owner admits 32 actors). The server enforces it natively for everyone else: no block edits, no opening chests or other containers, no lighting fire or TNT, no entity interaction, and no damage to players or non-hostile entities; explosions and fire never destroy or spread into claimed blocks. Server operators are always admitted.

- `/town trust <player|uuid>` / `/town untrust <player|uuid>` (leader or officer): add or remove a guest. A name must belong to an online player; a UUID works offline.
- `/town flag use <on|off>` (leader): let everyone use doors, trapdoors, fence gates, buttons and levers in the claims (never containers).
- `/town info [town]` reports members, claims, trusted guests and the door flag.

Every trust, flag, join or leave change commits durably and then rewrites each claim's zone; a refused rewrite rolls the change back. Storage is `towns-v1` rows (`v2`, with trusted guests and flags; a `v1` row is read as no guests and members-only doors). Commands map to the configured single dimension because command snapshots expose no dimension. Geopolitics, upkeep, war, taxes, and nation hierarchies are intentionally omitted.
