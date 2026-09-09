# Solaris Economy

**Deployment: Server-only.** This is the standard pack's sole virtual-money authority. It stores one bounded, versioned ledger and offers `/money`, `/pay <player-uuid> <amount> <token>`, and operator-only `/econadmin <set|add> <player-uuid> <amount>`.

The caller-supplied lowercase transfer token makes `/pay` idempotent per sender while that token remains in the configured bounded retention window; the oldest token is evicted when the window fills. Storage CAS serializes mutations and stale updates are rejected. Accounts begin with the configured balance when first mutated. The ledger is intentionally capped; use a future core transactional multi-record API before raising the limit. The existing `basic-economy` example is a separate item-currency shop demonstration and should not be enabled alongside this plugin as a second money authority.
