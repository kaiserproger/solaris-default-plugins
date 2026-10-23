//! Datapack-style chisel addon: right-click a block that has stair/slab
//! variants. TOP face -> top slab, BOTTOM face -> bottom slab (one matching
//! slab granted, dropped if the inventory is full), SIDE face -> stairs facing
//! that side (nothing granted); right-clicking stairs yields a slab.
//!
//! The (base block, slab, stairs) triple list below was derived once from
//! `crates/mc-data/data/required_blocks.json` in the core checkout with a
//! throwaway script: for every registry key ending in `_slab` whose sibling
//! `_stairs` key also exists, the full block was resolved in this order — the
//! stem itself, `<stem>_planks`, `<stem>s` (covers `bricks`, `tiles`,
//! `deepslate_bricks`), `<stem>_block` (covers `purpur_block`, `quartz_block`)
//! — every candidate verified present in the same registry snapshot, and
//! stems with only one side (`cut_red_sandstone`, `cut_sandstone`,
//! `petrified_oak`, `smooth_stone`) excluded. The script was not committed;
//! the hand-authored table is the source of truth and the unit test below pins
//! the rule budget.
use solaris_plugin_sdk::{export_plugin, lifecycle, Config, Failure, Plugin, StartupContribution};

/// Shaped crafting recipe: iron ingot over stick, yielding `chisel:chisel`.
///
/// Vanilla `minecraft:crafting_shaped` format. `RawIngredientValue::String`
/// admits a bare item id string as one ingredient alternative, and
/// `RawResult::Object` admits `{"id","count"}`; this JSON was checked against
/// `parse_ingredient` / `parse_result` in `crates/mc-data/src/recipes.rs`.
const CRAFTING_RECIPE: &str = r#"{"type":"minecraft:crafting_shaped","pattern":["I","S"],"key":{"I":"minecraft:iron_ingot","S":"minecraft:stick"},"result":{"id":"chisel:chisel","count":1}}"#;

/// Every vanilla full block that has BOTH a `_slab` and a `_stairs` variant
/// in `required_blocks.json`: (base block, slab, stairs).
const TRIPLES: &[(&str, &str, &str)] = &[
    (
        "minecraft:acacia_planks",
        "minecraft:acacia_slab",
        "minecraft:acacia_stairs",
    ),
    (
        "minecraft:bamboo_planks",
        "minecraft:bamboo_slab",
        "minecraft:bamboo_stairs",
    ),
    (
        "minecraft:birch_planks",
        "minecraft:birch_slab",
        "minecraft:birch_stairs",
    ),
    (
        "minecraft:cherry_planks",
        "minecraft:cherry_slab",
        "minecraft:cherry_stairs",
    ),
    (
        "minecraft:crimson_planks",
        "minecraft:crimson_slab",
        "minecraft:crimson_stairs",
    ),
    (
        "minecraft:dark_oak_planks",
        "minecraft:dark_oak_slab",
        "minecraft:dark_oak_stairs",
    ),
    (
        "minecraft:jungle_planks",
        "minecraft:jungle_slab",
        "minecraft:jungle_stairs",
    ),
    (
        "minecraft:mangrove_planks",
        "minecraft:mangrove_slab",
        "minecraft:mangrove_stairs",
    ),
    (
        "minecraft:oak_planks",
        "minecraft:oak_slab",
        "minecraft:oak_stairs",
    ),
    (
        "minecraft:pale_oak_planks",
        "minecraft:pale_oak_slab",
        "minecraft:pale_oak_stairs",
    ),
    (
        "minecraft:spruce_planks",
        "minecraft:spruce_slab",
        "minecraft:spruce_stairs",
    ),
    (
        "minecraft:warped_planks",
        "minecraft:warped_slab",
        "minecraft:warped_stairs",
    ),
    (
        "minecraft:bricks",
        "minecraft:brick_slab",
        "minecraft:brick_stairs",
    ),
    (
        "minecraft:deepslate_bricks",
        "minecraft:deepslate_brick_slab",
        "minecraft:deepslate_brick_stairs",
    ),
    (
        "minecraft:deepslate_tiles",
        "minecraft:deepslate_tile_slab",
        "minecraft:deepslate_tile_stairs",
    ),
    (
        "minecraft:end_stone_bricks",
        "minecraft:end_stone_brick_slab",
        "minecraft:end_stone_brick_stairs",
    ),
    (
        "minecraft:mossy_stone_bricks",
        "minecraft:mossy_stone_brick_slab",
        "minecraft:mossy_stone_brick_stairs",
    ),
    (
        "minecraft:mud_bricks",
        "minecraft:mud_brick_slab",
        "minecraft:mud_brick_stairs",
    ),
    (
        "minecraft:nether_bricks",
        "minecraft:nether_brick_slab",
        "minecraft:nether_brick_stairs",
    ),
    (
        "minecraft:prismarine_bricks",
        "minecraft:prismarine_brick_slab",
        "minecraft:prismarine_brick_stairs",
    ),
    (
        "minecraft:red_nether_bricks",
        "minecraft:red_nether_brick_slab",
        "minecraft:red_nether_brick_stairs",
    ),
    (
        "minecraft:resin_bricks",
        "minecraft:resin_brick_slab",
        "minecraft:resin_brick_stairs",
    ),
    (
        "minecraft:polished_blackstone_bricks",
        "minecraft:polished_blackstone_brick_slab",
        "minecraft:polished_blackstone_brick_stairs",
    ),
    (
        "minecraft:stone_bricks",
        "minecraft:stone_brick_slab",
        "minecraft:stone_brick_stairs",
    ),
    (
        "minecraft:tuff_bricks",
        "minecraft:tuff_brick_slab",
        "minecraft:tuff_brick_stairs",
    ),
    (
        "minecraft:purpur_block",
        "minecraft:purpur_slab",
        "minecraft:purpur_stairs",
    ),
    (
        "minecraft:quartz_block",
        "minecraft:quartz_slab",
        "minecraft:quartz_stairs",
    ),
    (
        "minecraft:andesite",
        "minecraft:andesite_slab",
        "minecraft:andesite_stairs",
    ),
    (
        "minecraft:bamboo_mosaic",
        "minecraft:bamboo_mosaic_slab",
        "minecraft:bamboo_mosaic_stairs",
    ),
    (
        "minecraft:blackstone",
        "minecraft:blackstone_slab",
        "minecraft:blackstone_stairs",
    ),
    (
        "minecraft:cobbled_deepslate",
        "minecraft:cobbled_deepslate_slab",
        "minecraft:cobbled_deepslate_stairs",
    ),
    (
        "minecraft:cobblestone",
        "minecraft:cobblestone_slab",
        "minecraft:cobblestone_stairs",
    ),
    (
        "minecraft:cut_copper",
        "minecraft:cut_copper_slab",
        "minecraft:cut_copper_stairs",
    ),
    (
        "minecraft:dark_prismarine",
        "minecraft:dark_prismarine_slab",
        "minecraft:dark_prismarine_stairs",
    ),
    (
        "minecraft:diorite",
        "minecraft:diorite_slab",
        "minecraft:diorite_stairs",
    ),
    (
        "minecraft:exposed_cut_copper",
        "minecraft:exposed_cut_copper_slab",
        "minecraft:exposed_cut_copper_stairs",
    ),
    (
        "minecraft:granite",
        "minecraft:granite_slab",
        "minecraft:granite_stairs",
    ),
    (
        "minecraft:mossy_cobblestone",
        "minecraft:mossy_cobblestone_slab",
        "minecraft:mossy_cobblestone_stairs",
    ),
    (
        "minecraft:oxidized_cut_copper",
        "minecraft:oxidized_cut_copper_slab",
        "minecraft:oxidized_cut_copper_stairs",
    ),
    (
        "minecraft:polished_andesite",
        "minecraft:polished_andesite_slab",
        "minecraft:polished_andesite_stairs",
    ),
    (
        "minecraft:polished_blackstone",
        "minecraft:polished_blackstone_slab",
        "minecraft:polished_blackstone_stairs",
    ),
    (
        "minecraft:polished_deepslate",
        "minecraft:polished_deepslate_slab",
        "minecraft:polished_deepslate_stairs",
    ),
    (
        "minecraft:polished_diorite",
        "minecraft:polished_diorite_slab",
        "minecraft:polished_diorite_stairs",
    ),
    (
        "minecraft:polished_granite",
        "minecraft:polished_granite_slab",
        "minecraft:polished_granite_stairs",
    ),
    (
        "minecraft:polished_tuff",
        "minecraft:polished_tuff_slab",
        "minecraft:polished_tuff_stairs",
    ),
    (
        "minecraft:prismarine",
        "minecraft:prismarine_slab",
        "minecraft:prismarine_stairs",
    ),
    (
        "minecraft:red_sandstone",
        "minecraft:red_sandstone_slab",
        "minecraft:red_sandstone_stairs",
    ),
    (
        "minecraft:sandstone",
        "minecraft:sandstone_slab",
        "minecraft:sandstone_stairs",
    ),
    (
        "minecraft:smooth_quartz",
        "minecraft:smooth_quartz_slab",
        "minecraft:smooth_quartz_stairs",
    ),
    (
        "minecraft:smooth_red_sandstone",
        "minecraft:smooth_red_sandstone_slab",
        "minecraft:smooth_red_sandstone_stairs",
    ),
    (
        "minecraft:smooth_sandstone",
        "minecraft:smooth_sandstone_slab",
        "minecraft:smooth_sandstone_stairs",
    ),
    (
        "minecraft:stone",
        "minecraft:stone_slab",
        "minecraft:stone_stairs",
    ),
    (
        "minecraft:tuff",
        "minecraft:tuff_slab",
        "minecraft:tuff_stairs",
    ),
    (
        "minecraft:waxed_cut_copper",
        "minecraft:waxed_cut_copper_slab",
        "minecraft:waxed_cut_copper_stairs",
    ),
    (
        "minecraft:waxed_exposed_cut_copper",
        "minecraft:waxed_exposed_cut_copper_slab",
        "minecraft:waxed_exposed_cut_copper_stairs",
    ),
    (
        "minecraft:waxed_oxidized_cut_copper",
        "minecraft:waxed_oxidized_cut_copper_slab",
        "minecraft:waxed_oxidized_cut_copper_stairs",
    ),
    (
        "minecraft:waxed_weathered_cut_copper",
        "minecraft:waxed_weathered_cut_copper_slab",
        "minecraft:waxed_weathered_cut_copper_stairs",
    ),
    (
        "minecraft:weathered_cut_copper",
        "minecraft:weathered_cut_copper_slab",
        "minecraft:weathered_cut_copper_stairs",
    ),
];

/// Four native block-tool rules per triple: up->slab (top, give slab),
/// down->slab (bottom, give slab), side->stairs (facing the clicked side),
/// stairs->slab (copy `half`/`waterlogged`, any face).
fn block_tool_rules() -> Vec<String> {
    let mut rules = Vec::with_capacity(TRIPLES.len() * 4);
    for (base, slab, stairs) in TRIPLES {
        rules.push(format!(
            "{{\"item\":\"chisel:chisel\",\"block\":\"{base}\",\"face\":\"up\",\"result\":\"{slab}\",\"set\":{{\"type\":\"top\"}},\"give\":\"{slab}\"}}"
        ));
        rules.push(format!(
            "{{\"item\":\"chisel:chisel\",\"block\":\"{base}\",\"face\":\"down\",\"result\":\"{slab}\",\"set\":{{\"type\":\"bottom\"}},\"give\":\"{slab}\"}}"
        ));
        rules.push(format!(
            "{{\"item\":\"chisel:chisel\",\"block\":\"{base}\",\"face\":\"side\",\"result\":\"{stairs}\",\"set\":{{\"facing\":\"$face\"}}}}"
        ));
        rules.push(format!(
            "{{\"item\":\"chisel:chisel\",\"block\":\"{stairs}\",\"face\":\"any\",\"result\":\"{slab}\",\"set\":{{\"type\":\"@half\",\"waterlogged\":\"@waterlogged\"}}}}"
        ));
    }
    rules
}

#[derive(Default)]
struct Chisel;

impl Plugin for Chisel {
    fn configure(&mut self, _config: &Config) -> Result<Option<StartupContribution>, Failure> {
        Ok(Some(StartupContribution {
            placement: None,
            trees: None,
            clay: None,
            spawning: None,
            mob_blueprints: None,
            items: Some(vec![lifecycle::ItemDefinition {
                id: "chisel:chisel".to_owned(),
                carrier: "minecraft:stick".to_owned(),
                name: "Chisel".to_owned(),
                max_stack_size: 1,
                max_damage: Some(250),
                weapon: false,
                attack_damage_modifier: None,
                attack_speed_modifier: None,
                equippable_slot: None,
                crafting_ingredient: None,
                physical_profile: None,
            }]),
            work_recipes: None,
            crafting_recipes: Some(vec![CRAFTING_RECIPE.to_owned()]),
            block_tools: Some(block_tool_rules()),
        }))
    }
}

export_plugin!(Chisel);

#[cfg(test)]
mod tests {
    use super::{block_tool_rules, CRAFTING_RECIPE, TRIPLES};

    /// Rule-count and byte budgets mirror the host bounds in
    /// `crates/mc-data/src/block_tools.rs` (`MAX_BLOCK_TOOL_RULES = 1024`,
    /// `MAX_BLOCK_TOOL_BYTES = 1 MiB`) so growth past them fails here first.
    #[test]
    fn rule_catalog_stays_within_host_bounds() {
        let rules = block_tool_rules();
        assert_eq!(rules.len(), TRIPLES.len() * 4);
        assert!(rules.len() <= 1024, "rule count {}", rules.len());
        let total: usize = rules.iter().map(|rule| rule.len()).sum();
        assert!(total <= 1_048_576, "rule bytes {total}");
    }

    /// Every rule is flat JSON with balanced braces/quotes and only the
    /// contract's keys — a serde_json-free smoke check (this crate has no
    /// JSON dependency) that complements the host's strict parse.
    #[test]
    fn rules_are_flat_json_with_known_keys() {
        let allowed = [
            "\"item\":",
            "\"block\":",
            "\"face\":",
            "\"result\":",
            "\"set\":",
            "\"give\":",
            "\"type\":",
            "\"facing\":",
            "\"waterlogged\":",
        ];
        for rule in block_tool_rules() {
            assert!(rule.starts_with('{') && rule.ends_with('}'), "{rule}");
            assert_eq!(rule.bytes().filter(|b| *b == b'{').count(), 2, "{rule}");
            assert_eq!(rule.bytes().filter(|b| *b == b'}').count(), 2, "{rule}");
            assert_eq!(rule.bytes().filter(|b| *b == b'"').count() % 2, 0, "{rule}");
            let mut rest = &rule[1..rule.len() - 1];
            while let Some(colon) = rest.find(':') {
                let key = rest[..colon].trim_end();
                assert!(
                    allowed
                        .iter()
                        .any(|known| key.ends_with(known.trim_end_matches(':'))),
                    "unexpected key in {rule}"
                );
                rest = &rest[colon + 1..];
                let mut depth = 0i32;
                let mut in_string = false;
                let mut escaped = false;
                let mut end = None;
                for (index, byte) in rest.bytes().enumerate() {
                    if escaped {
                        escaped = false;
                        continue;
                    }
                    match byte {
                        b'\\' if in_string => escaped = true,
                        b'"' => in_string = !in_string,
                        b'{' | b'[' if !in_string => depth += 1,
                        b'}' | b']' if !in_string => {
                            if depth == 0 {
                                end = Some(index);
                                break;
                            }
                            depth -= 1;
                        }
                        b',' if !in_string && depth == 0 => {
                            end = Some(index);
                            break;
                        }
                        _ => {}
                    }
                }
                rest = match end {
                    Some(index) => rest[index + 1..].trim_start_matches(',').trim_start(),
                    None => break,
                };
            }
        }
    }

    #[test]
    fn crafting_recipe_shape_is_vanilla_shaped() {
        assert!(CRAFTING_RECIPE.contains("\"type\":\"minecraft:crafting_shaped\""));
        assert!(CRAFTING_RECIPE.contains("\"result\":{\"id\":\"chisel:chisel\",\"count\":1}"));
    }
}
