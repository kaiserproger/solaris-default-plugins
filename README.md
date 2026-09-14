# Solaris Default Plugins

First-party Luau plugin packages for the Solaris server
(`../solaris` core repo). Each subdirectory is one independent deployable
package: `plugin.toml` + `main.lua`, with an optional `config.toml`.

## Packages

Standard pack (recommended startup order: permissions, essentials, economy,
towns, audit) — see `standard-pack/README.md` for scope and omissions:

- `solaris-permissions`, `solaris-essentials`, `solaris-economy`,
  `solaris-towns`, `solaris-audit`

Demonstrations and worldgen selectors:

- `basic-economy`, `land-claims`, `online-roster`, `geological-mines`,
  `currency-catalog` (fixture)
- `solaris-settlements` — the single settlement package (server-side,
  `structures/*.toml` authored blueprints, no Loader requirement). Installed
  explicitly; a deployed set holds at most one settlement profile. It replaces
  the removed `colony-villager-scaffold` and `settlement-prototype` packages,
  and the core settlement selector must name it (`feudal_settlements`) once the
  core catalog loader lands.

## Repository layout

- `<package>/` — one deployed plugin package each (see the list above).
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

`solaris-settlements` is server-only: it declares no client bundle and no
Loader capability, so a vanilla client can run `/settlement`. It is the only
settlement profile in the set — the removed `settlement-prototype` and
`colony-villager-scaffold` packages must not be deployed beside it.

## Lua API contract

- Target API is `0.6.0` (`api = "0.6.0"` in every `plugin.toml`).
- Server-only unless the manifest says otherwise; commands work with a
  vanilla client.
- Manifest capabilities/events must cover everything `main.lua` uses;
  strict mode rejects malformed packages and stray files.
- `config.toml` (where present) is validated at load; see each package
  README and `docs/PLUGINS.md` in the core repo for the full contract.

## Layout

```
solaris-default-plugins/
  <package>/  plugin.toml, main.lua, [config.toml,] README.md
  standard-pack/  pack README (no code; lists the five pack members)
```

## License

Same as Solaris core: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).
