use super::technology::*;
use std::collections::BTreeMap;

fn definitions() -> BTreeMap<String, Definition> {
    [
        (
            "CR1".into(),
            Definition {
                id: "CR1".into(),
                parents: vec![],
                practice_recipes: vec!["oak_planks".into()],
                unlock_recipes: vec!["wooden_pickaxe".into()],
            },
        ),
        (
            "CR2".into(),
            Definition {
                id: "CR2".into(),
                parents: vec!["CR1".into()],
                practice_recipes: vec!["stick".into()],
                unlock_recipes: vec!["wooden_hoe".into()],
            },
        ),
    ]
    .into()
}
fn practice() -> Practice {
    Practice {
        operation_id: "accepted-craft-1".into(),
        recipe: "oak_planks".into(),
        recipe_revision: Some("authored-recipe-revision-7".into()),
        worker: "npc-1".into(),
        station: "station-1".into(),
        station_revision: 3,
        portion: Some("portion-1".into()),
        output_item: Some("minecraft:oak_planks".into()),
        output_count: Some(4),
    }
}
fn source() -> Source {
    Source::LocalProject {
        worker: "npc-1".into(),
        project: "project-1".into(),
    }
}
#[test]
fn knowledge_without_accepted_practice_neither_unlocks_craft_nor_child() {
    let defs = definitions();
    let mut local = Knowledge::default();
    local.learn("CR1", source(), &defs).unwrap();
    assert_eq!(
        local.can_craft("wooden_pickaxe", &defs, true),
        Err(Refusal::MissingKnowledge)
    );
    assert_eq!(
        local.learn("CR2", source(), &defs),
        Err(Refusal::MissingParent("CR1".into()))
    );
    local.accept_practice("CR1", practice(), &defs).unwrap();
    assert_eq!(local.can_craft("wooden_pickaxe", &defs, true), Ok(()));
    local.learn("CR2", source(), &defs).unwrap();
    assert_eq!(
        local.can_craft("wooden_hoe", &defs, true),
        Err(Refusal::MissingKnowledge)
    );
}
#[test]
fn net_delta_only_or_unaccepted_receipts_cannot_establish_practice() {
    let defs = definitions();
    let mut local = Knowledge::default();
    local.learn("CR1", source(), &defs).unwrap();
    for field in 0..4 {
        let mut receipt = practice();
        match field {
            0 => receipt.recipe_revision = None,
            1 => receipt.portion = None,
            2 => receipt.output_item = None,
            _ => receipt.output_count = None,
        }
        assert_eq!(
            local.accept_practice("CR1", receipt, &defs),
            Err(Refusal::WrongPractice)
        );
    }
    assert_eq!(
        local.can_craft("wooden_pickaxe", &defs, true),
        Err(Refusal::MissingKnowledge)
    );
}
#[test]
fn replay_is_idempotent_conflict_refused_and_station_loss_keeps_history() {
    let defs = definitions();
    let mut local = Knowledge::default();
    local.learn("CR1", source(), &defs).unwrap();
    local.accept_practice("CR1", practice(), &defs).unwrap();
    let revision = local.revision;
    local.accept_practice("CR1", practice(), &defs).unwrap();
    assert_eq!(local.revision, revision);
    let mut conflicting = practice();
    conflicting.output_count = Some(8);
    assert_eq!(
        local.accept_practice("CR1", conflicting, &defs),
        Err(Refusal::OperationConflict)
    );
    assert_eq!(
        local.can_craft("wooden_pickaxe", &defs, false),
        Err(Refusal::StationUnavailable)
    );
    let restarted: Knowledge =
        serde_json::from_str(&serde_json::to_string(&local).unwrap()).unwrap();
    assert_eq!(restarted.can_craft("wooden_pickaxe", &defs, true), Ok(()));
}
#[test]
fn transfer_requires_live_carrier_and_current_source_without_copying_practice() {
    let defs = definitions();
    let mut origin = Knowledge::default();
    origin.learn("CR1", source(), &defs).unwrap();
    origin.accept_practice("CR1", practice(), &defs).unwrap();
    let mut destination = Knowledge::default();
    let transfer = Transfer {
        technology: "CR1".into(),
        source_scope: "village-a".into(),
        source_revision: origin.revision,
        destination_scope: "village-b".into(),
        source: Source::Teacher {
            worker: "npc-1".into(),
            origin_scope: "village-a".into(),
            origin_revision: origin.revision,
        },
    };
    assert_eq!(
        destination.receive("teaching-1", transfer.clone(), &origin, false, &defs),
        Err(Refusal::MissingCarrier)
    );
    let mut stale = transfer.clone();
    stale.source_revision += 1;
    assert_eq!(
        destination.receive("teaching-1", stale, &origin, true, &defs),
        Err(Refusal::MissingKnowledge)
    );
    destination
        .receive("teaching-1", transfer.clone(), &origin, true, &defs)
        .unwrap();
    assert_eq!(
        destination.can_craft("wooden_pickaxe", &defs, true),
        Err(Refusal::MissingKnowledge)
    );
    let revision = destination.revision;
    destination
        .receive("teaching-1", transfer, &origin, true, &defs)
        .unwrap();
    assert_eq!(destination.revision, revision);
}

#[test]
fn durable_manufacturing_gate_requires_local_exact_practice_after_restart() {
    use super::technology_runtime::allows;
    let defs = initial_definitions();
    let mut knowledge = Knowledge::default();
    knowledge.learn("CR1", source(), &defs).unwrap();
    let before = serde_json::json!({"scopes": {"village-a": knowledge}}).to_string();
    assert!(allows(Some(&before), "village-a", "solaris-settlements:stick").is_err());
    let mut receipt = practice();
    receipt.recipe = "solaris-settlements:oak_planks".into();
    knowledge.accept_practice("CR1", receipt, &defs).unwrap();
    let persisted = serde_json::json!({"scopes": {"village-a": knowledge}}).to_string();
    assert_eq!(
        allows(Some(&persisted), "village-a", "solaris-settlements:stick"),
        Ok(())
    );
    assert!(allows(Some(&persisted), "village-b", "solaris-settlements:stick").is_err());
    assert!(allows(Some(&persisted), "village-a", "solaris-settlements:bread").is_err());
    assert!(allows(Some("corrupt"), "village-a", "solaris-settlements:stick").is_err());
    // Bootstrap practice itself remains reachable, not a circular unlock.
    assert_eq!(
        allows(None, "village-a", "solaris-settlements:oak_planks"),
        Ok(())
    );
}

#[test]
fn cyclic_or_missing_prerequisite_references_refuse_catalog_admission() {
    let mut defs = definitions();
    let recipes = ["oak_planks", "wooden_pickaxe", "stick", "wooden_hoe"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    defs.get_mut("CR1").unwrap().parents.push("CR2".into());
    assert!(matches!(validate(&defs, &recipes), Err(Refusal::Cycle(_))));
    defs.get_mut("CR1").unwrap().parents = vec!["ABSENT".into()];
    assert_eq!(
        validate(&defs, &recipes),
        Err(Refusal::MissingReference("ABSENT".into()))
    );
}

#[test]
fn executable_graph_rejects_missing_recipe_and_self_locked_practice() {
    let mut defs = definitions();
    let recipes = ["oak_planks", "wooden_pickaxe", "stick", "wooden_hoe"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(validate_executable(&defs, &recipes), Ok(()));
    defs.get_mut("CR2").unwrap().practice_recipes = vec!["absent".into()];
    assert_eq!(
        validate_executable(&defs, &recipes),
        Err(Refusal::MissingReference("absent".into()))
    );
    defs.get_mut("CR2").unwrap().practice_recipes = vec!["wooden_hoe".into()];
    assert_eq!(
        validate_executable(&defs, &recipes),
        Err(Refusal::MissingReference("CR2".into()))
    );
}

#[test]
fn knowledge_transfer_rejects_forged_origin_or_teacher_and_preserves_source() {
    let defs = definitions();
    let mut origin = Knowledge::default();
    origin.learn("CR1", source(), &defs).unwrap();
    origin.accept_practice("CR1", practice(), &defs).unwrap();
    let saved = origin.clone();
    let transfer = Transfer {
        technology: "CR1".into(),
        source_scope: "village-a".into(),
        source_revision: origin.revision,
        destination_scope: "village-b".into(),
        source: Source::Teacher {
            worker: "npc-1".into(),
            origin_scope: "village-a".into(),
            origin_revision: origin.revision,
        },
    };
    let mut destination = Knowledge::default();
    for field in 0..3 {
        let mut forged = transfer.clone();
        let Source::Teacher {
            worker,
            origin_scope,
            origin_revision,
        } = &mut forged.source
        else {
            unreachable!()
        };
        match field {
            0 => *worker = "trophy-owner".into(),
            1 => *origin_scope = "unrelated-scope".into(),
            _ => *origin_revision += 1,
        }
        assert!(matches!(
            destination.receive("transfer", forged, &origin, true, &defs),
            Err(Refusal::MissingReference(_))
        ));
        assert_eq!(destination, Knowledge::default());
    }
    destination
        .receive("transfer", transfer.clone(), &origin, true, &defs)
        .unwrap();
    assert_eq!(
        origin, saved,
        "accounted transfer neither moves nor fabricates practice"
    );
    let restored: Knowledge =
        serde_json::from_str(&serde_json::to_string(&destination).unwrap()).unwrap();
    assert_eq!(restored.transfers["transfer"], transfer);
    assert!(restored.records["CR1"].accepted_practice.is_empty());
    assert_eq!(
        restored.can_craft("wooden_pickaxe", &defs, true),
        Err(Refusal::MissingKnowledge)
    );
}

#[test]
fn lost_parent_blocks_child_without_erasing_history_and_restoration_resumes() {
    let defs = definitions();
    let mut local = Knowledge::default();
    local.learn("CR1", source(), &defs).unwrap();
    local.accept_practice("CR1", practice(), &defs).unwrap();
    local.learn("CR2", source(), &defs).unwrap();
    let mut child = practice();
    child.operation_id = "stick-portion".into();
    child.recipe = "stick".into();
    child.output_item = Some("minecraft:stick".into());
    local.accept_practice("CR2", child, &defs).unwrap();
    let history = local.records["CR2"].clone();
    let parent = local.records.remove("CR1").unwrap();
    assert_eq!(
        local.can_craft("wooden_hoe", &defs, true),
        Err(Refusal::MissingParent("CR1".into()))
    );
    let mut restarted: Knowledge =
        serde_json::from_str(&serde_json::to_string(&local).unwrap()).unwrap();
    assert_eq!(restarted.records["CR2"], history);
    restarted.records.insert("CR1".into(), parent);
    assert_eq!(restarted.can_craft("wooden_hoe", &defs, true), Ok(()));
    assert_eq!(restarted.records["CR2"], history);
}

#[test]
fn family_selector_and_native_quality_variants_share_the_same_practice_gate() {
    let mut defs = definitions();
    defs.get_mut("CR2").unwrap().unlock_recipes = vec![
        "stone_pickaxe_rough".into(),
        "stone_pickaxe_masterwork".into(),
    ];
    let mut local = Knowledge::default();
    local.learn("CR1", source(), &defs).unwrap();
    local.accept_practice("CR1", practice(), &defs).unwrap();
    for recipe in [
        "stone_pickaxe",
        "stone_pickaxe_rough",
        "stone_pickaxe_masterwork",
    ] {
        assert_eq!(
            local.can_craft(recipe, &defs, true),
            Err(Refusal::MissingKnowledge)
        );
    }
    local.learn("CR2", source(), &defs).unwrap();
    let mut receipt = practice();
    receipt.operation_id = "sticks".into();
    receipt.recipe = "stick".into();
    receipt.output_item = Some("minecraft:stick".into());
    local.accept_practice("CR2", receipt, &defs).unwrap();
    for recipe in [
        "stone_pickaxe",
        "stone_pickaxe_rough",
        "stone_pickaxe_masterwork",
    ] {
        assert_eq!(local.can_craft(recipe, &defs, true), Ok(()));
    }
}
