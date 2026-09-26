# Solaris Economy

**Deployment: Server-only.** This is the standard pack's sole virtual-money authority. It stores one bounded, versioned ledger and offers `/money`, `/pay <player-uuid> <amount> <token>`, and operator-only `/econadmin <set|add> <player-uuid> <amount>`.

The lowercase transfer token names a durable storage-batch operation per
sender. Before a new `/pay`, the package probes its receipt. A payment committed
through this path cannot debit again after the bounded ledger window evicts its
token or a reload loses the callback. Old ledger tokens have no retroactive
receipt and retain only their original bounded retention guarantee. The
balance update and token commit atomically under the existing
`ledger-v1` key. A refused or uncertain outcome reloads the ledger; it is not
rolled back or retried under a fresh token. `/econadmin` still uses ledger CAS.
Accounts begin with the configured balance. The ledger remains capped. The
`basic-economy` example is a separate item-currency shop; do not enable it
alongside this plugin as a second money authority.
