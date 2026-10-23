# Solaris Default Plugins

First-party plugin source for the Solaris server (`../solaris` core repo).
The standard pack ships five API 0.8 WebAssembly component packages: each
deployed directory has `plugin.toml` + `plugin.wasm`, with an optional
`config.toml`. Settlement gameplay is owned natively by the core server in
`../solaris`; this repository ships no settlement component.

## Packages

Standard pack (recommended startup order: permissions, essentials, economy,
towns, audit) — see `standard-pack/README.md` for scope and omissions:

- `solaris-permissions`, `solaris-essentials`, `solaris-economy`,
  `solaris-towns`, `solaris-audit`

Optional components:

- `basic-economy` — inventory-backed shop, separate from the standard pack's
  virtual currency. Install it by explicitly selecting its package name.
- [`solaris-auth`](solaris-auth/README.md) — offline-mode `/register`, `/login`
  and `/changepassword`, durable salted hashes, session timeout and build denial.
  Build explicitly with `--package solaris-auth`; it is not enabled by the
  default pack. See its README for damage/other-command limitations.
- [`chisel`](chisel/README.md) — datapack-style chisel addon (slab/stair
  conversion, Loader item bundle). Build explicitly with `--package chisel`;
  it is not enabled by the default pack.
- [`graves`](graves/README.md) — death graves: the server's native death path
  takes a death's drops into a barrel at the death spot when this package is
  deployed, and the package protects that grave for its owner until
  `public_after_seconds` elapses. It declares the `death_graves` capability; two
  packages declaring it refuse startup. Build explicitly with
  `--package graves`; it is not enabled by the default pack.

Legacy source/reference examples (not deployable components):

- `land-claims`, `online-roster`, `geological-mines`;
  `currency-catalog` is a fixture. `install.sh` does not deploy these.

## Repository layout

- `<package>/` — an API 0.8 deployable component package (a standard-pack member).
- `sources/` — Rust source workspaces for component packages; source never
  enters a deployed package directory.
- `standard-pack/` — docs only; the deployed behaviour lives in the five member
  packages.
- `tools/` — repository tools (standard-pack build/publication, generic
  content-pipeline validation, integration gates).
- `evidence/` — point-in-time verification receipts for the settlement wave
  (what was run, what passed, what was missing). Not a package; never deployed.

Integration gates (`tools/run_integration.py`): the `standard-pack` profile
byte-verifies all seven shipped components against the selected core SDK/WIT,
runs `plugin_standard_pack`, and starts the real `solaris-v2` server with the
five standard packages, then with all seven shipped components together.
Settlement domain acceptance tests were v1-runtime-bound and deleted 2026-10-15;
settlement behavior now lives natively in the core server (`../solaris`).
Archive:
`/home/kaiserroman/solaris/.analysis/archive/sibling-v1-integration-tests/integration-tests-pre-phase5-20261015.tgz`.

Rebuild all shipped components whenever the core SDK/WIT changes, even if the
manifest API version remains `0.8.0`; that version alone does not establish
binary ABI compatibility:

```sh
python3 tools/build_standard_pack.py --core-root /path/to/solaris --write
```

The build validates the entire candidate before replacing any `plugin.wasm`.
Use `--check` instead of `--write` for deterministic source/artifact verification.
From the core checkout, run the sibling-owned gates with:

```sh
SOLARIS_DEFAULT_PLUGINS_ROOT=/path/to/solaris-default-plugins \
  python3 -m tools.harness run standard-pack
SOLARIS_DEFAULT_PLUGINS_ROOT=/path/to/solaris-default-plugins \
  python3 -m tools.harness run standard-pack --real-client
```

The real-client gate connects a graphical Java client and checks `/money`.
There is no `settlement-cargo` profile: settlements now belong to core.

## Deployment workflow

Installation is explicit; installing the core server does not enable plugins.
On Linux, clone this repository and run its installer (Bash 4+, GNU coreutils).
No compilation or network access is needed after cloning:

```sh
git clone https://github.com/kaiserproger/solaris-default-plugins.git
bash solaris-default-plugins/install.sh --directory /path/to/server/plugins
```

With no package names, the installer selects the five standard packages.
To install only selected packages:

```sh
bash solaris-default-plugins/install.sh --directory /path/to/server/plugins \
  solaris-essentials solaris-audit
```

Stop the server first. The installer refuses existing package paths, including
symlinks; it never updates packages, overwrites operator configuration, or
edits `server.toml`. All selected sources and destinations are checked before
copying. For reproducible deployment, check out a reviewed commit before
running the installer. To update, back up the deployed package and its data,
review configuration/API changes, and deliberately replace the package while
the server is stopped; this command is installation, not an updater.

Set `[plugins].directory` to the destination. Production deployments should
set `plugins.strict = true` and list every deployed plugin id under
`plugins.expected`. For the full standard pack:

```toml
[plugins]
directory = "/path/to/server/plugins"
strict = true
expected = [
  "solaris-permissions", "solaris-essentials", "solaris-economy",
  "solaris-towns", "solaris-audit",
]
```

Then run `solaris --check --config server.toml` before starting the server.
Package API/configuration validation belongs to the server, not the copy
script. The core binary loads deployed directories only; it never compiles
these sources in. Manual copying remains valid on other platforms.

## Component API contract

- API 0.8 applies to the five standard packages and optional `basic-economy`, `solaris-auth`, `chisel` and `graves`;
  legacy reference examples above retain historical manifests and are not
  installation targets.
- Server-only unless the manifest says otherwise; commands work with a vanilla
  client.
- Manifest capabilities/events must cover everything `plugin.wasm` uses; strict
  mode rejects malformed packages and stray files.
- `config.toml` (where present) is validated at load; see each package README
  and `docs/PLUGINS.md` in the core repo for the full contract.

## Layout

```text
solaris-default-plugins/
  <package>/  plugin.toml, plugin.wasm, [config.toml,] README.md
  sources/    Rust component source workspaces
  standard-pack/  pack README (no code; lists the five pack members)
```

## License

Same as Solaris core: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).
