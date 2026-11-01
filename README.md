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
- `computer` — optional monitor item/block content for the existing edge
  computer/screen feature. Build explicitly with `--package computer`; it is
  not part of the default pack. See [Computer monitor](#computer-monitor).

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

The guest `Cargo.lock` files pin `solaris-plugin-sdk` at `0.8.0`, matching
the selected core SDK. Update that pin with an SDK version migration: the
builder uses `--locked` and refuses stale locks before compiling a component.

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

## Computer monitor

`computer` contributes the real `computer:monitor` item at startup (stack 64,
no durability) and a matching carrier-backed block in its verified schema-2
client archive. Both use `minecraft:gray_glazed_terracotta`; the block drops
`computer:monitor`. The archive includes an authored monitor block model,
matching item definition/model, and original idle-screen/case textures.
No Mojang image/model bytes are copied into this package; the vanilla model
parent is referenced by id.

This package is declarative content only. It does not spawn processes,
implement a relay, add commands, or register crafting recipes. Computer
sessions and `solaris:screen` / `solaris:computer` remain edge/client-owned
features with their existing opt-in and consent requirements. Installing the
content does not enable those features or grant desktop capture/input access.
Servers must use the core's `computer:monitor` identity, not the former private
`solaris:monitor` id; there is no alias.

### Client support

| Client | Monitor content |
| --- | --- |
| Java with Fabric Loader | Targeted by the required block/item/asset bundle |
| Java with NeoForge Loader | Targeted by the same required bundle |
| Java with Forge Loader | Targeted by the same required bundle |
| Native `solaris-client` | Unsupported: its Loader rejects block content/indexes; excluded from this bundle |
| Vanilla Java without Solaris Loader | Unsupported: this package requires Loader client content |

The real component and archive have passed simultaneous monitor/Ruby/Sapphire
grant, placement, rendering, break and custom-item pickup cycles on all three
Java adapters. The matching core ADR 0018 records the exact 2026-11-01 live
receipts. These content gates do not prove terminal, capture or screen-relay UI.
The edge relay admits Fabric/native sessions, not NeoForge/Forge; native's relay
implementation does not overcome its block archive refusal. No fallback to
vanilla terracotta is provided.

### Build, install and acquire

The source checkout alone is not deployable until its generated archive and
component exist. From this repository, using the same core SDK/WIT and Loader
revision as the server:

```sh
python3 sources/computer/build_bundle.py
python3 tools/build_standard_pack.py \
  --core-root /path/to/solaris --loader-root /path/to/solaris-loader \
  --package computer --build-root /path/to/build-output/computer --write
bash install.sh --directory /path/to/server/plugins computer
```

The first command follows the chisel authoring convention: hashes every
authored asset into the schema-2 index, writes a deterministic archive (index
first, sorted assets, fixed timestamps), and refreshes the actual archive hash
and size in `computer/plugin.toml`. Run it after asset changes. The second
builds the API 0.8.0 component and runs the existing native/Java authoring
validators before publishing `computer/plugin.wasm`. Neither command is run
by the installer. For an isolated immutable deployment, use
`--output-dir /path/to/new-candidate` instead of `--write`; the package is then
under that candidate's `plugins/computer/`.

Stop the server before installation, keep all other deployed ids in
`plugins.expected`, and add `"computer"`. Default builder/installer selections
are unchanged; this package must be selected explicitly.
Adding the custom item/carrier changes the persisted plugin startup identity.
An existing world with a different identity is refused, not automatically
migrated; installing this optional package does not rewrite saved worlds.
With the package activated and the required Loader bundle accepted, an operator
can obtain a monitor through the existing command:

```text
/give @s computer:monitor 1
```

Place it as an ordinary block. Its south model face is the screen; the carrier's
horizontal `facing` determines the screen face in-world. Break/drop, inventory
and persistence behavior use the core carrier-block path, not plugin-owned
world state. Ruby and Sapphire retain their pink/blue glazed-terracotta
carriers; this package reserves only gray glazed terracotta.

## Component API contract

- API 0.8 applies to the five standard packages and optional `basic-economy`, `solaris-auth`, `chisel`, `graves` and `computer`;
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
