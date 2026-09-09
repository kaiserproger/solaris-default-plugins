# Solaris Towns

**Deployment: Server-only.** A deliberately small Towny-like layer: `/town create`, `invite`, `join`, `leave`, `info`, `role`, `claim`, and `unclaim`. Towns, roles, and whole-chunk claims are durable and strictly bounded. Invitations expire on simulation-tick timers.

API 0.6 protected zones accept exactly one UUID (or an operator), not a plugin-supplied member predicate. Claims therefore authorize only the town leader; members and officers are organizational metadata and cannot build in claims. The missing core primitive is a bounded zone-policy callback or atomically replaceable UUID allow-list. Commands also map to the configured single dimension because command snapshots expose no dimension. Geopolitics, upkeep, war, taxes, and nation hierarchies are intentionally omitted.
