//! Slice A military witness: arming_sword / longsword / spear.
//!
//! Natural-material inputs, ordinary civilian manufacture, and per-model use
//! discrimination. Paper carriers only; no vanilla-tool substitution.
//! Values follow OWNER_DECISIONS §1 (RULESET-R2 proposal, not owner-admitted
//! balance): spear is explicitly two-handed; wear is per executed attack;
//! stamina capacity 100 / recovery 0.5 with per-attack costs and no restore.
//! War/balance/mounted policy (CP058/059/068) is untouched here.

use super::*;
use solaris_plugin_sdk::inventories::{
    InventoryEndpoint, InventoryFence, InventoryItem, InventorySlot, OwnedInventorySnapshot,
};

const NATURAL_INPUTS: [(&str, &[(&str, u32)]); 3] = [
    (
        military_catalog::ARMING_SWORD,
        &[
            ("minecraft:iron_ingot", 2),
            ("minecraft:stick", 1),
            ("minecraft:leather", 1),
        ],
    ),
    (
        military_catalog::LONGSWORD,
        &[
            ("minecraft:iron_ingot", 3),
            ("minecraft:stick", 1),
            ("minecraft:leather", 1),
        ],
    ),
    (
        military_catalog::SPEAR,
        &[("minecraft:iron_ingot", 1), ("minecraft:stick", 2)],
    ),
];

const DURATIONS: [(&str, u64); 3] = [
    (military_catalog::ARMING_SWORD, 320),
    (military_catalog::LONGSWORD, 480),
    (military_catalog::SPEAR, 240),
];

fn snapshot(endpoint: InventoryEndpoint, count: u8) -> OwnedInventorySnapshot {
    OwnedInventorySnapshot {
        endpoint,
        fence: InventoryFence {
            revision: 7,
            snapshot_hash: "a".repeat(64),
        },
        slots: (0..count)
            .map(|slot| InventorySlot { slot, item: None })
            .collect(),
    }
}

fn stock(snapshot: &mut OwnedInventorySnapshot, slot: usize, item: &str, count: u32) {
    let custom_model = match item {
        military_catalog::ARMING_SWORD
        | military_catalog::LONGSWORD
        | military_catalog::SPEAR
        | military_catalog::WAR_BOW
        | military_catalog::MAIL_HAUBERK
        | military_catalog::PAVISE => Some(item.to_owned()),
        _ => None,
    };
    snapshot.slots[slot].item = Some(InventoryItem {
        resource_id: if custom_model.is_some() {
            "minecraft:paper".to_owned()
        } else {
            item.to_owned()
        },
        count,
        damage: None,
        enchantments: Vec::new(),
        custom_name: None,
        item_model: custom_model,
        max_damage: None,
        custom_data: None,
        stew_effects: Vec::new(),
    });
}

fn profile(id: &str) -> serde_json::Value {
    let items = military_catalog::authored_items();
    let definition = items
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("slice-A item missing from catalog: {id}"));
    assert_eq!(
        definition.carrier, "minecraft:paper",
        "slice-A model must ride a paper carrier, never a vanilla tool: {id}"
    );
    serde_json::from_str(
        definition
            .physical_profile
            .as_ref()
            .expect("slice-A item carries a physical profile"),
    )
    .expect("physical profile parses")
}

fn authored_recipe(id: &str) -> serde_json::Value {
    military_catalog::authored_recipes()
        .into_iter()
        .find(|recipe| recipe["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing slice-A manufacture recipe: {id}"))
}

#[test]
fn slice_a_models_use_natural_inputs_and_paper_carriers() {
    let items = military_catalog::authored_items();
    assert_eq!(items.len(), 6, "catalog keeps all six military models");
    for (id, inputs) in NATURAL_INPUTS {
        let definition = items
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("missing slice-A definition: {id}"));
        assert_eq!(definition.carrier, "minecraft:paper");
        assert_eq!(definition.max_stack_size, 1);
        assert_eq!(
            definition.max_damage,
            Some(match id {
                military_catalog::ARMING_SWORD => 320,
                military_catalog::LONGSWORD => 280,
                _ => 240,
            }),
            "slice-A durability stays on the OWNER_DECISIONS §1 value: {id}"
        );
        let recipe = authored_recipe(id);
        assert_eq!(
            recipe["station"].as_str(),
            Some("minecraft:crafting_table"),
            "slice-A manufacture runs at an ordinary station: {id}"
        );
        let mut observed: Vec<(String, u64)> = recipe["inputs"]
            .as_array()
            .expect("recipe inputs")
            .iter()
            .map(|input| {
                (
                    input["item"].as_str().expect("input id").to_owned(),
                    input["count"].as_u64().expect("input count"),
                )
            })
            .collect();
        observed.sort();
        let mut expected: Vec<(String, u64)> = inputs
            .iter()
            .map(|(item, count)| ((*item).to_owned(), u64::from(*count)))
            .collect();
        expected.sort();
        assert_eq!(observed, expected, "slice-A keeps natural inputs: {id}");
        // No vanilla tool/gear substitution: the output is the paper-carried model itself.
        let outputs = recipe["outputs"].as_array().expect("recipe outputs");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0]["item"].as_str(), Some(id));
        assert!(
            outputs[0]["item"]
                .as_str()
                .is_some_and(|item| !item.starts_with("minecraft:")),
            "slice-A output is the authored model, not a vanilla item: {id}"
        );
    }
    for (id, duration) in DURATIONS {
        let recipe = authored_recipe(id);
        assert_eq!(
            recipe["duration_ticks"].as_u64(),
            Some(duration),
            "slice-A manufacture keeps its proposed duration: {id}"
        );
    }
}

#[test]
fn slice_a_manufacture_routes_through_ordinary_production_admission() {
    for (id, _) in NATURAL_INPUTS {
        let canonical = id.strip_prefix("solaris-settlements:").unwrap_or(id);
        // Base recipe is selectable (not a rolled quality variant)...
        assert!(
            production::recipe(canonical).is_some(),
            "slice-A base recipe admitted: {id}"
        );
        // ...its plan exposes quality variants rolled at admission, never picked...
        let plan = production::recipe_plan(canonical)
            .unwrap_or_else(|| panic!("slice-A plan exposed: {id}"));
        let variants = plan["variants"].as_array().expect("plan variants");
        assert!(
            variants.len() > 1,
            "slice-A manufacture rolls quality at admission: {id}"
        );
        assert!(
            variants.iter().all(|variant| variant["id"]
                .as_str()
                .is_some_and(|variant| variant.starts_with(id))),
            "quality variants keep the slice-A model identity: {id}"
        );
        // ...and admission deterministically fixes one variant + seed (no reroll).
        let (first, first_seed, first_tier) = production::admitted_recipe(id, "slice-a-op-1");
        let (second, second_seed, second_tier) = production::admitted_recipe(id, "slice-a-op-1");
        assert_eq!(
            (&first, &first_seed, &first_tier),
            (&second, &second_seed, &second_tier)
        );
        assert!(
            first.starts_with(id),
            "admitted variant keeps the slice-A model identity: {first}"
        );
        assert!(first_seed.is_some() && first_tier.is_some());
    }
}

#[test]
fn arming_sword_is_one_handed_cut_with_45_degree_cone() {
    let attack = &profile(military_catalog::ARMING_SWORD)["attacks"][0];
    assert_eq!(attack["mode"].as_str(), Some("melee"));
    assert_eq!(attack["damage_kind"].as_str(), Some("cut"));
    assert_eq!(attack["base_damage_hp"].as_f64(), Some(5.0));
    assert_eq!(attack["min_range_blocks"].as_f64(), Some(0.0));
    assert_eq!(attack["reach_blocks"].as_f64(), Some(1.9));
    assert_eq!(attack["windup_ticks"].as_u64(), Some(6));
    assert_eq!(attack["active_ticks"].as_u64(), Some(2));
    assert_eq!(attack["recovery_ticks"].as_u64(), Some(10));
    assert_eq!(attack["stamina_cost"].as_f64(), Some(12.0));
    assert_eq!(attack["shape"]["kind"].as_str(), Some("cone"));
    assert_eq!(attack["shape"]["half_angle_degrees"].as_f64(), Some(45.0));
    assert_eq!(
        attack["two_handed"].as_bool(),
        Some(false),
        "arming sword stays one-handed against the two-handed slice-A models"
    );
    assert_eq!(attack["wear"].as_u64(), Some(1));
}

#[test]
fn longsword_is_two_handed_cut_with_55_degree_cone_and_needs_free_offhand() {
    let attack = &profile(military_catalog::LONGSWORD)["attacks"][0];
    assert_eq!(attack["mode"].as_str(), Some("melee"));
    assert_eq!(attack["damage_kind"].as_str(), Some("cut"));
    assert_eq!(attack["base_damage_hp"].as_f64(), Some(7.0));
    assert_eq!(attack["min_range_blocks"].as_f64(), Some(0.0));
    assert_eq!(attack["reach_blocks"].as_f64(), Some(2.8));
    assert_eq!(attack["windup_ticks"].as_u64(), Some(10));
    assert_eq!(attack["active_ticks"].as_u64(), Some(3));
    assert_eq!(attack["recovery_ticks"].as_u64(), Some(16));
    assert_eq!(attack["stamina_cost"].as_f64(), Some(22.0));
    assert_eq!(attack["shape"]["half_angle_degrees"].as_f64(), Some(55.0));
    assert_eq!(
        attack["two_handed"].as_bool(),
        Some(true),
        "longsword demands both hands"
    );
    // The two-handed infantry kit refuses an occupied offhand before any move...
    let mut warehouse = snapshot(
        warehouse_endpoint(&"container:minecraft:overworld:0:64:0".to_owned())
            .expect("valid physical container fixture"),
        27,
    );
    stock(&mut warehouse, 0, military_catalog::LONGSWORD, 1);
    stock(&mut warehouse, 1, military_catalog::MAIL_HAUBERK, 1);
    stock(&mut warehouse, 2, "minecraft:emerald", 1);
    let mut equipment = snapshot(InventoryEndpoint::NpcEquipment("resident-1".to_owned()), 6);
    let carry = snapshot(InventoryEndpoint::NpcCarry("resident-1".to_owned()), 8);
    stock(&mut equipment, 1, military_catalog::PAVISE, 1);
    assert!(
        hire::plan(
            hire::Role::Infantry,
            Some(hire::Loadout::Longsword),
            &warehouse,
            &equipment,
            &carry
        )
        .is_err(),
        "two-handed longsword kit requires an empty offhand slot"
    );
    // ...and issues the exact paper-carried longsword once the hand is free.
    equipment.slots[1].item = None;
    let plan = hire::plan(
        hire::Role::Infantry,
        Some(hire::Loadout::Longsword),
        &warehouse,
        &equipment,
        &carry,
    )
    .expect("free offhand admits the longsword kit");
    let longsword = warehouse.slots[0].item.as_ref().unwrap();
    assert_eq!(longsword.resource_id, "minecraft:paper");
    assert_eq!(
        longsword.item_model.as_deref(),
        Some(military_catalog::LONGSWORD)
    );
    assert!(
        plan.iter().any(|transfer| transfer.source_slot == 0
            && matches!(transfer.destination, hire::Destination::Equipment(0))),
        "the longsword transfer draws the canonical model stack"
    );
}

#[test]
fn spear_is_two_handed_thrust_with_a_dead_zone() {
    let attack = &profile(military_catalog::SPEAR)["attacks"][0];
    assert_eq!(attack["mode"].as_str(), Some("melee"));
    assert_eq!(attack["damage_kind"].as_str(), Some("pierce"));
    assert_eq!(attack["base_damage_hp"].as_f64(), Some(5.0));
    assert_eq!(
        attack["min_range_blocks"].as_f64(),
        Some(1.25),
        "spear keeps its weak close zone against sword models"
    );
    assert_eq!(attack["reach_blocks"].as_f64(), Some(3.2));
    assert_eq!(attack["windup_ticks"].as_u64(), Some(8));
    assert_eq!(attack["active_ticks"].as_u64(), Some(2));
    assert_eq!(attack["recovery_ticks"].as_u64(), Some(14));
    assert_eq!(attack["stamina_cost"].as_f64(), Some(16.0));
    assert_eq!(attack["shape"]["kind"].as_str(), Some("thrust"));
    assert_eq!(attack["shape"]["radius_blocks"].as_f64(), Some(0.18));
    assert_eq!(
        attack["two_handed"].as_bool(),
        Some(true),
        "spear is explicitly two-handed per the RULESET decision"
    );
    assert_eq!(attack["wear"].as_u64(), Some(1));
    // Spearmen draw the exact paper-carried spear, never a vanilla stick/trident.
    let mut warehouse = snapshot(
        warehouse_endpoint(&"container:minecraft:overworld:0:64:0".to_owned())
            .expect("valid physical container fixture"),
        27,
    );
    stock(&mut warehouse, 0, military_catalog::SPEAR, 1);
    stock(&mut warehouse, 1, military_catalog::MAIL_HAUBERK, 1);
    stock(&mut warehouse, 2, "minecraft:emerald", 1);
    let equipment = snapshot(InventoryEndpoint::NpcEquipment("resident-1".to_owned()), 6);
    let carry = snapshot(InventoryEndpoint::NpcCarry("resident-1".to_owned()), 8);
    let plan = hire::plan(hire::Role::Spearman, None, &warehouse, &equipment, &carry)
        .expect("stocked spearman kit");
    let spear = warehouse.slots[0].item.as_ref().unwrap();
    assert_eq!(spear.resource_id, "minecraft:paper");
    assert_eq!(spear.item_model.as_deref(), Some(military_catalog::SPEAR));
    assert!(
        plan.iter().any(|transfer| transfer.source_slot == 0
            && matches!(transfer.destination, hire::Destination::Equipment(0))),
        "the spear transfer draws the canonical model stack"
    );
}

#[test]
fn slice_a_kits_issue_exact_paper_models_with_separate_wage_and_refuse_vanilla_blades() {
    // Militia and default infantry both draw the canonical arming sword...
    for role in [hire::Role::Militia, hire::Role::Infantry] {
        let mut warehouse = snapshot(
            warehouse_endpoint(&"container:minecraft:overworld:0:64:0".to_owned())
                .expect("valid physical container fixture"),
            27,
        );
        stock(&mut warehouse, 0, military_catalog::ARMING_SWORD, 1);
        stock(&mut warehouse, 1, military_catalog::MAIL_HAUBERK, 1);
        if matches!(role, hire::Role::Infantry) {
            stock(&mut warehouse, 2, military_catalog::PAVISE, 1);
            stock(&mut warehouse, 3, "minecraft:emerald", 1);
        } else {
            stock(&mut warehouse, 2, "minecraft:emerald", 1);
        }
        let equipment = snapshot(InventoryEndpoint::NpcEquipment("resident-1".to_owned()), 6);
        let carry = snapshot(InventoryEndpoint::NpcCarry("resident-1".to_owned()), 8);
        let plan = hire::plan(role, None, &warehouse, &equipment, &carry)
            .unwrap_or_else(|error| panic!("stocked {role:?} kit: {error}"));
        let sword = warehouse.slots[0].item.as_ref().unwrap();
        assert_eq!(sword.resource_id, "minecraft:paper");
        assert_eq!(
            sword.item_model.as_deref(),
            Some(military_catalog::ARMING_SWORD)
        );
        assert!(
            plan.iter().any(|transfer| transfer.source_slot == 0
                && matches!(transfer.destination, hire::Destination::Equipment(0))),
            "the arming-sword transfer draws the canonical model stack for {role:?}"
        );
        // ...while the signing wage stays a separate emerald, never kit stock.
        let wage_slot = if matches!(role, hire::Role::Infantry) {
            3
        } else {
            2
        };
        assert_eq!(
            warehouse.slots[wage_slot]
                .item
                .as_ref()
                .unwrap()
                .resource_id,
            "minecraft:emerald"
        );
    }
    // A vanilla blade is not canonical kit even though it shares the melee role.
    let mut warehouse = snapshot(
        warehouse_endpoint(&"container:minecraft:overworld:0:64:0".to_owned())
            .expect("valid physical container fixture"),
        27,
    );
    stock(&mut warehouse, 0, "minecraft:iron_sword", 1);
    stock(&mut warehouse, 1, military_catalog::MAIL_HAUBERK, 1);
    stock(&mut warehouse, 2, "minecraft:emerald", 1);
    let equipment = snapshot(InventoryEndpoint::NpcEquipment("resident-1".to_owned()), 6);
    let carry = snapshot(InventoryEndpoint::NpcCarry("resident-1".to_owned()), 8);
    assert_eq!(
        warehouse.slots[0].item.as_ref().unwrap().resource_id,
        "minecraft:iron_sword",
        "vanilla fixture really is a vanilla item, not a paper carrier"
    );
    assert!(
        hire::plan(hire::Role::Militia, None, &warehouse, &equipment, &carry).is_err(),
        "a vanilla sword never substitutes the authored arming-sword model"
    );
}

#[test]
fn slice_a_stamina_and_wear_follow_owner_decisions_without_restore() {
    for (id, cost) in [
        (military_catalog::ARMING_SWORD, 12.0),
        (military_catalog::LONGSWORD, 22.0),
        (military_catalog::SPEAR, 16.0),
    ] {
        let value = profile(id);
        assert_eq!(value["stamina_capacity"].as_f64(), Some(100.0));
        assert_eq!(value["stamina_recovery_per_tick"].as_f64(), Some(0.5));
        let attack = &value["attacks"][0];
        assert_eq!(attack["stamina_cost"].as_f64(), Some(cost));
        assert_eq!(
            attack["wear"].as_u64(),
            Some(1),
            "each executed slice-A attack wears exactly 1: {id}"
        );
        // No stamina grant rides along: costs debit, nothing restores.
        assert!(
            attack.get("stamina_restore").is_none() && attack.get("stamina_refund").is_none(),
            "slice-A attacks never restore stamina on swap/reconnect/deploy: {id}"
        );
    }
}

/// Per-model manufacture/use receipt: the ordinary production journal admits
/// each slice-A recipe, records one native portion with exact natural-material
/// inputs and the single model output, and keeps the receipt across a
/// serialized restart. `use` here is the per-model hire-kit issue of the exact
/// paper-carried model (no vanilla substitution); live native melee execution
/// stays with the windowed client gate (native_r2_military.py `_use`).
#[test]
fn slice_a_manufacture_and_use_receipts_close_per_model_headless() {
    for (id, inputs) in NATURAL_INPUTS {
        let canonical = id.strip_prefix("solaris-settlements:").unwrap_or(id);
        // Ordinary admission fixes the rolled quality variant + seed (no reroll).
        let operation = format!("slice-a-{canonical}");
        let (recipe_id, seed, quality) = production::admitted_recipe(id, &operation);
        assert!(recipe_id.starts_with(id));
        let seed = seed.expect("admission rolls a quality seed");
        let quality = quality.expect("admission rolls a quality tier");
        // The civilian journal admits the intent at an ordinary station...
        let mut journal = production::Journal::default();
        let station = production::Station {
            dimension: "minecraft:overworld".to_owned(),
            x: 12,
            y: 64,
            z: 9,
            building: "smithy".to_owned(),
            origin_id: "site-1".to_owned(),
            worksite_id: Some("site-1".to_owned()),
            revision: 3,
        };
        let recipe = authored_recipe(id);
        let duration = recipe["duration_ticks"].as_u64().expect("duration");
        journal
            .insert(production::Intent {
                operation_id: operation.clone(),
                settlement: "oak_hollow".to_owned(),
                authorized_by: "owner".to_owned(),
                worker: "worker-1".to_owned(),
                recipe_id: recipe_id.clone(),
                quality_seed: Some(seed),
                quality: Some(quality.clone()),
                count: 1,
                expected_revision: 7,
                station: station.clone(),
                accepted_tick: 20,
                due_tick: 20 + duration,
            })
            .expect("civilian intent admitted");
        // ...and closes on one native portion with exact natural inputs and
        // the single authored-model output (paper-carried, never vanilla).
        let portion_inputs: Vec<serde_json::Value> = inputs
            .iter()
            .map(|(item, count)| serde_json::json!({"item": {"resource_id": item, "count": count}}))
            .collect();
        let receipt = production::Receipt {
            operation_id: operation.clone(),
            settlement: "oak_hollow".to_owned(),
            worker: "worker-1".to_owned(),
            recipe_id: recipe_id.clone(),
            station: station.clone(),
            native_revision: 8,
            work_units_done: 1,
            work_units_planned: 1,
            state: "Committed".to_owned(),
            reason: None,
            changes: vec![
                production::ItemChange {
                    item_id: "minecraft:iron_ingot".to_owned(),
                    delta: -(inputs
                        .iter()
                        .find(|(item, _)| *item == "minecraft:iron_ingot")
                        .map(|(_, count)| i64::from(*count))
                        .unwrap_or(0)),
                },
                production::ItemChange {
                    item_id: id.to_owned(),
                    delta: 1,
                },
            ],
            native_portions: vec![serde_json::json!({
                "operation_id": operation,
                "handle": "worker-1",
                "portion_id": format!("{operation}-portion-1"),
                "sequence": 1,
                "recipe": recipe_id,
                "recipe_revision": "immutable-a",
                "inputs": portion_inputs,
                "outputs": [{"item": {"resource_id": id, "count": 1}}],
            })],
            acknowledged_sequence: 1,
            due_tick: None,
        };
        journal
            .finish(&operation, production::Outcome::Native(receipt.clone()))
            .expect("native receipt closes the job");
        let accepted = journal
            .accepted_practice("oak_hollow", &operation)
            .expect("committed receipt is readable practice");
        assert_eq!(accepted.operation_id, operation);
        assert_eq!(accepted.recipe_id, recipe_id);
        // Exact native portion totals match the natural-material ledger...
        let portion = &accepted.native_portions[0];
        let mut observed: Vec<(String, u64)> = portion["inputs"]
            .as_array()
            .expect("portion inputs")
            .iter()
            .map(|stack| {
                (
                    stack["item"]["resource_id"]
                        .as_str()
                        .expect("input id")
                        .to_owned(),
                    stack["item"]["count"].as_u64().expect("input count"),
                )
            })
            .collect();
        observed.sort();
        let mut expected: Vec<(String, u64)> = inputs
            .iter()
            .map(|(item, count)| ((*item).to_owned(), u64::from(*count)))
            .collect();
        expected.sort();
        assert_eq!(
            observed, expected,
            "portion debits exact natural inputs: {id}"
        );
        let outputs = portion["outputs"].as_array().expect("portion outputs");
        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0]["item"]["resource_id"].as_str(),
            Some(id),
            "portion outputs the authored model, not a vanilla item: {id}"
        );
        // ...and survive a serialized restart with a duplicate delivery.
        let mut restored =
            production::Journal::load(Some(&serde_json::to_string(&journal).unwrap()))
                .expect("journal survives restart");
        restored
            .finish(&operation, production::Outcome::Native(receipt))
            .expect("duplicate delivery is idempotent");
        assert!(restored.due(u64::MAX).is_none());
        assert!(restored
            .accepted_practice("oak_hollow", &operation)
            .is_some());
    }
}
