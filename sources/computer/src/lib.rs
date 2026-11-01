//! Optional monitor content. The verified Loader archive owns the matching
//! carrier block declaration; the edge owns computer/screen behavior.
use solaris_plugin_sdk::{export_plugin, lifecycle, Config, Failure, Plugin, StartupContribution};

#[derive(Default)]
struct Computer;

impl Plugin for Computer {
    fn configure(&mut self, _config: &Config) -> Result<Option<StartupContribution>, Failure> {
        Ok(Some(StartupContribution {
            placement: None,
            trees: None,
            clay: None,
            spawning: None,
            mob_blueprints: None,
            items: Some(vec![lifecycle::ItemDefinition {
                id: "computer:monitor".to_owned(),
                carrier: "minecraft:gray_glazed_terracotta".to_owned(),
                name: "Monitor".to_owned(),
                max_stack_size: 64,
                max_damage: None,
                weapon: false,
                attack_damage_modifier: None,
                attack_speed_modifier: None,
                equippable_slot: None,
                crafting_ingredient: None,
                physical_profile: None,
            }]),
            work_recipes: None,
            crafting_recipes: None,
            block_tools: None,
        }))
    }
}

export_plugin!(Computer);
