# Chisel

Optional datapack-style addon for ordinary Loader clients. Right-click a
block that has stair/slab variants while holding the chisel:

- TOP face -> top slab (one matching slab granted, dropped if the inventory
  is full),
- BOTTOM face -> bottom slab (same grant),
- SIDE face -> stairs facing that side (nothing granted),
- right-click stairs -> slab, copying `half`/`waterlogged` (nothing granted).

The tool is `chisel:chisel` (vanilla `minecraft:stick` carrier, stack 1,
durability 250, 1 wear per use), crafted from an iron ingot over a stick
(shaped, vertical). Covers every vanilla full block with both `_slab` and
`_stairs` variants (58 families, 232 native block-tool rules).

This is a `server_and_client` package: it ships a Loader bundle (item
definition, model and texture), so players need a Solaris client (native
solaris-client, or the Fabric/NeoForge/Forge Loader). A vanilla client is
refused during Configuration by the platform's Loader-required rule; there is
no optional-bundle negotiation yet, so no vanilla fallback exists. Install the
`chisel` package alongside the other packages in `[plugins].directory` — it is
opt-in and not part of the five-package standard pack.

Build the component with
`tools/build_standard_pack.py --core-root ../solaris --package chisel`;
rebuild the tracked `chisel/client/chisel-content.zip` bundle (and the hashes
in `chisel/plugin.toml`) after changing `sources/chisel/client-src` with:

```sh
python3 sources/chisel/build_bundle.py
```

The bundle and the index it carries are deterministic (fixed timestamps,
sorted entries): rerunning the script without source changes is a no-op.
`chisel/plugin.wasm` is built, never committed by hand.
