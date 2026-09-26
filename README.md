# Solaris Default Plugins

First-party plugin source for the Solaris server (`../solaris` core repo).
The standard pack and `solaris-settlements` are API 0.7 WebAssembly component
packages: each deployed directory has `plugin.toml` + `plugin.wasm`, with an
optional `config.toml`.

## Packages

Standard pack (recommended startup order: permissions, essentials, economy,
towns, audit) — see `standard-pack/README.md` for scope and omissions:

- `solaris-permissions`, `solaris-essentials`, `solaris-economy`,
  `solaris-towns`, `solaris-audit`

Legacy examples and worldgen selectors:

- `basic-economy`, `land-claims`, `online-roster`, `geological-mines`,
  `currency-catalog` (fixture). They remain source/reference material until
  each is migrated to an API 0.7 component; `install.sh` does not deploy them.

- `solaris-settlements` — the single settlement component (server-side,
  `structures/*.toml` authored blueprints, no Loader requirement). Installed
  explicitly; a deployed set holds at most one settlement profile. It replaces
  the removed `colony-villager-scaffold` and `settlement-prototype` packages.

## Repository layout

- `<package>/` — an API 0.7 deployable component package (the standard pack and
  `solaris-settlements`).
- `sources/` — Rust source workspaces for component packages; source never
  enters a deployed package directory.
- `standard-pack/` — docs only; the deployed behaviour lives in the five member
  packages.
- `tools/` — repository tools (`gen_structures.py` generates and verifies the
  settlement package's `structures/*.toml`).
- `evidence/` — point-in-time verification receipts for the settlement wave
  (what was run, what passed, what was missing). Not a package; never deployed.

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

### Deployed set used for live verification

Install the five standard packages plus the settlement package explicitly:

```sh
bash install.sh --directory /path/to/server/plugins \
  solaris-permissions solaris-essentials solaris-economy solaris-towns \
  solaris-audit solaris-settlements
```

```toml
[plugins]
directory = "/path/to/server/plugins"
strict = true
expected = [
  "solaris-permissions", "solaris-essentials", "solaris-economy",
  "solaris-towns", "solaris-audit", "solaris-settlements",
]
```

`solaris-settlements` commands work with a vanilla client. Its optional
Loader view bundle does not change the server's command authority. It is the
only settlement profile in the set — the removed `settlement-prototype` and
`colony-villager-scaffold` packages must not be deployed beside it.

## Component API contract

- API 0.7 applies to the deployed standard pack and `solaris-settlements`;
  legacy examples above still use their historical manifests and are not
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
