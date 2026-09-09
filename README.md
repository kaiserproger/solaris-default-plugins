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

No build step. Copy the package directories you want into the server's
`[plugins].directory` (see `example.toml` in the core repo):

```sh
cp -r solaris-permissions solaris-essentials solaris-economy \
  solaris-towns solaris-audit /path/to/server/plugins/
```

Production deployments should set `plugins.strict = true` and list every
deployed plugin id under `plugins.expected`. The core binary loads deployed
directories only; it never compiles these sources in.

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
