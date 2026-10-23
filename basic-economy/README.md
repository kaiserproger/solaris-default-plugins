# Basic Economy

**Deployment: Server-only API 0.8 WebAssembly component.** No Solaris Loader
is required; an ordinary vanilla 26.1.2 client can use this plugin.

The component preserves `basic-economy`'s `shop:economy:<uuid>` v2 ledger,
economy zone, `/economy [shop]` command, server-owned inventory menu, and
atomic inventory/storage purchase and refund transactions. `plugin.wasm` is
rebuilt from `sources/basic-economy/` by `tools/build_standard_pack.py`.
