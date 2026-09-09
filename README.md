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

- `basic-economy`, `land-claims`, `online-roster`,
  `colony-villager-scaffold`, `geological-mines`,
  `settlement-prototype`, `currency-catalog` (fixture)

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
