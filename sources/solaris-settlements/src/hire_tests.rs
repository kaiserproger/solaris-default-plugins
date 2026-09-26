use super::*;
use solaris_plugin_sdk::inventories::{
    InventoryExpectedRevision, InventoryItem, InventorySlot, OwnedInventorySnapshot,
};

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
    snapshot.slots[slot].item = Some(InventoryItem {
        resource_id: item.to_owned(),
        count,
        damage: None,
        enchantments: Vec::new(),
        custom_name: None,
        item_model: None,
    });
}

#[test]
fn role_kit_refuses_missing_wage_and_occupied_gear_before_issuing_a_transfer() {
    let mut warehouse = snapshot(InventoryEndpoint::Warehouse("warehouse-1".to_owned()), 27);
    stock(&mut warehouse, 0, "minecraft:iron_sword", 1);
    stock(&mut warehouse, 1, "minecraft:leather_chestplate", 1);
    let mut equipment = snapshot(
        InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
        6,
    );
    let carry = snapshot(InventoryEndpoint::ResidentCarry("resident-1".to_owned()), 8);
    assert_eq!(
        hire::plan(hire::Role::Militia, &warehouse, &equipment, &carry).err(),
        Some("The warehouse is missing the requested kit or signing wage.")
    );
    stock(&mut warehouse, 2, "minecraft:emerald", 1);
    stock(&mut equipment, 3, "minecraft:leather_chestplate", 1);
    assert_eq!(
        hire::plan(hire::Role::Militia, &warehouse, &equipment, &carry).err(),
        Some("The resident's required gear or payment slot is occupied.")
    );
    equipment.slots[3].item = None;
    let plan = hire::plan(hire::Role::Militia, &warehouse, &equipment, &carry).unwrap();
    assert_eq!(plan.len(), 3);
    assert!(matches!(
        plan[0].destination,
        hire::Destination::Equipment(0)
    ));
    assert!(matches!(
        plan[1].destination,
        hire::Destination::Equipment(3)
    ));
    assert!(matches!(plan[2].destination, hire::Destination::Carry(0)));
}

#[test]
fn archer_ammunition_draws_multiple_real_stacks_and_keeps_payment_separate() {
    let mut warehouse = snapshot(InventoryEndpoint::Warehouse("warehouse-1".to_owned()), 27);
    stock(&mut warehouse, 0, "minecraft:bow", 1);
    stock(&mut warehouse, 1, "minecraft:leather_chestplate", 1);
    stock(&mut warehouse, 2, "minecraft:arrow", 20);
    stock(&mut warehouse, 3, "minecraft:arrow", 12);
    stock(&mut warehouse, 4, "minecraft:emerald", 1);
    let equipment = snapshot(
        InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
        6,
    );
    let carry = snapshot(InventoryEndpoint::ResidentCarry("resident-1".to_owned()), 8);
    let plan = hire::plan(hire::Role::Archer, &warehouse, &equipment, &carry).unwrap();
    assert_eq!(plan.len(), 5);
    assert_eq!((plan[2].source_slot, plan[2].count), (2, 20));
    assert_eq!((plan[3].source_slot, plan[3].count), (3, 12));
    assert!(matches!(plan[2].destination, hire::Destination::Carry(0)));
    assert!(matches!(plan[3].destination, hire::Destination::Carry(0)));
    assert!(matches!(plan[4].destination, hire::Destination::Carry(1)));
}

#[test]
fn reload_after_native_gear_commit_reissues_identical_transfer_then_finalizes_intent() {
    use solaris_plugin_sdk::domain_operations::DomainOperation;
    use solaris_plugin_sdk::inventories::InventoryOperation;

    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "resident_1", "resident-1", 4).unwrap();
    let mut warehouse = snapshot(InventoryEndpoint::Warehouse("warehouse-1".to_owned()), 27);
    stock(&mut warehouse, 0, "minecraft:iron_sword", 1);
    warehouse.slots[0].item.as_mut().unwrap().custom_name = Some("Issued blade".to_owned());
    stock(&mut warehouse, 1, "minecraft:leather_chestplate", 1);
    stock(&mut warehouse, 2, "minecraft:emerald", 1);
    let equipment = snapshot(
        InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
        6,
    );
    let carry = snapshot(InventoryEndpoint::ResidentCarry("resident-1".to_owned()), 8);
    let transfers = hire::plan(hire::Role::Militia, &warehouse, &equipment, &carry).unwrap();
    let issued =
        hire::issued_kit(hire::Role::Militia, "warehouse-1", &warehouse, &transfers).unwrap();
    registry.hire_sequence = 1;
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("resident_1")
        .unwrap()
        .hiring = Some(hire::Intent {
        employer: "oak_hollow".to_owned(),
        role: hire::Role::Militia,
        warehouse: "warehouse-1".to_owned(),
        operation_id: "hire-1".to_owned(),
        transfers,
        issued,
        warehouse_fence: hire::Fence::from_snapshot(&warehouse),
        equipment_fence: hire::Fence::from_snapshot(&equipment),
        carry_fence: hire::Fence::from_snapshot(&carry),
    });
    let mut plugin = Settlements::default();
    let recovery = plugin.recover_hires();
    let [Command::StorageGet(read)] = recovery.as_slice() else {
        panic!("a reload reads durable hiring intents");
    };
    let answers = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(5),
        }),
    });
    let [Command::Operation(operation)] = answers.as_slice() else {
        panic!("the saved intent retries the native transfer");
    };
    let DomainOperation::Inventory(InventoryOperation::Transfer(transfer)) = &operation.operation
    else {
        panic!("must move real owned items");
    };
    assert_eq!(transfer.actor_id, 0);
    assert_eq!(transfer.operation_id, "hire-1");
    assert_eq!(transfer.transfers.len(), 3);
    assert_eq!(transfer.expected_revisions.len(), 3);
    assert_eq!(
        transfer.expected_revisions[0].fence.snapshot_hash,
        warehouse.fence.snapshot_hash
    );
    let request = operation.request.clone();
    let answers = plugin.answered(&OperationAnswered {
        request,
        operation_id: Some("hire-1".to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 8,
            payload: OperationPayload::OwnedInventory(InventoryResult::Transfer(vec![
                InventoryExpectedRevision {
                    endpoint: InventoryEndpoint::Warehouse("warehouse-1".to_owned()),
                    fence: warehouse.fence,
                },
                InventoryExpectedRevision {
                    endpoint: InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
                    fence: InventoryFence {
                        revision: 8,
                        snapshot_hash: "b".repeat(64),
                    },
                },
                InventoryExpectedRevision {
                    endpoint: InventoryEndpoint::ResidentCarry("resident-1".to_owned()),
                    fence: InventoryFence {
                        revision: 8,
                        snapshot_hash: "c".repeat(64),
                    },
                },
            ])),
        }),
    });
    let [Command::Operation(query)] = answers.as_slice() else {
        panic!("after a transfer, read current gear to incorporate native work resume");
    };
    let DomainOperation::Inventory(InventoryOperation::Query { .. }) = &query.operation else {
        panic!("must query current resident gear");
    };
    let mut current = snapshot(
        InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
        6,
    );
    current.fence.revision = 10;
    let answers = plugin.answered(&OperationAnswered {
        request: query.request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 10,
            payload: OperationPayload::OwnedInventory(InventoryResult::Snapshot(current)),
        }),
    });
    let [Command::StorageGet(read)] = answers.as_slice() else {
        panic!("the current resident revision must lead to registry finalization");
    };
    let answers = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(5),
        }),
    });
    let [Command::StorageCas(write)] = answers.as_slice() else {
        panic!("finalization must be one compare-and-swap");
    };
    assert_eq!(write.expected_version, Some(5));
    let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
    let resident = &registry.settlements["oak_hollow"].residents["resident_1"];
    assert_eq!(resident.handle, "resident-1");
    assert_eq!(resident.work_revision, 10);
    assert_eq!(resident.employer.as_deref(), Some("oak_hollow"));
    assert!(matches!(resident.role, Some(hire::Role::Militia)));
    let issued = resident
        .issued
        .as_ref()
        .expect("issued gear survives transfer replay");
    assert_eq!(
        issued.items.len(),
        2,
        "the paid wage remains personal property"
    );
    assert_eq!(issued.items[0].custom_name.as_deref(), Some("Issued blade"));
    assert!(resident.hiring.is_none());
}

#[test]
fn repeated_hire_and_competing_employer_refuse_before_touching_inventory() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    register_settlement(&mut registry, "owner".to_owned(), "pine_haven", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "resident_1", "resident-1", 8).unwrap();
    record_settlement_resident(&mut registry, "pine_haven", "resident_1", "resident-1", 8).unwrap();
    let resident = registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("resident_1")
        .unwrap();
    resident.role = Some(hire::Role::Militia);
    resident.employer = Some("oak_hollow".to_owned());
    let mut plugin = Settlements::default();
    for settlement in ["oak_hollow", "pine_haven"] {
        let request = plugin.hire(7, "owner", settlement, "resident_1", "infantry", 0);
        let [Command::StorageGet(read)] = request.as_slice() else {
            panic!("admission must read durable registry");
        };
        let answers = plugin.storage_get(&StorageGetAnswered {
            request: read.request.clone(),
            outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                value: Some(serde_json::to_string(&registry).unwrap()),
                version: Some(9),
            }),
        });
        assert!(
            answers
                .iter()
                .all(|command| !matches!(command, Command::Operation(_) | Command::StorageCas(_))),
            "neither duplicate nor competing employer may equip this resident"
        );
        assert!(plugin.pending_hire.is_none());
    }
}

#[test]
fn a_child_is_refused_service_until_adulthood_then_admitted() {
    use crate::demography::ADULT_AGE_TICKS;

    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "kid", "resident-kid", 4).unwrap();
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("kid")
        .unwrap()
        .age_origin = Some(1_000);
    registry.settlements.get_mut("oak_hollow").unwrap().site_id = Some("site-1".to_owned());
    let mut plugin = Settlements::default();
    plugin.warehouse = Some(WarehouseBinding {
        handle: "warehouse-1".to_owned(),
        source: WarehouseSource::VanillaVillage(settlements::VillageWarehouseSource {
            site_id: "site-1".to_owned(),
            container_id: 0,
        }),
        revision: 1,
    });
    // One tick before adulthood the hire refuses with nothing persisted.
    let refused = plugin.hire(
        7,
        "owner",
        "oak_hollow",
        "kid",
        "militia",
        1_000 + ADULT_AGE_TICKS - 1,
    );
    let [Command::StorageGet(read)] = refused.as_slice() else {
        panic!("admission must read durable registry");
    };
    let answers = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(9),
        }),
    });
    assert!(
        answers.iter().any(|command| matches!(
            command,
            Command::SendMessage(message) if message.text.contains("grown")
        )),
        "a child is refused service: {answers:?}"
    );

    // At the adulthood tick the same hire proceeds to the resident query.
    plugin.pending_hire = None;
    let admitted = plugin.hire(
        7,
        "owner",
        "oak_hollow",
        "kid",
        "militia",
        1_000 + ADULT_AGE_TICKS,
    );
    let [Command::StorageGet(read)] = admitted.as_slice() else {
        panic!("admission must read durable registry");
    };
    let answers = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(9),
        }),
    });
    assert!(
        answers.iter().any(|command| matches!(
            command,
            Command::Operation(operation) if operation.request.starts_with("hire-resident")
        )),
        "an adult hire proceeds: {answers:?}"
    );

    // A resident with no recorded origin is an adopted adult: admitted at any tick.
    record_settlement_resident(&mut registry, "oak_hollow", "newcomer", "resident-new", 4).unwrap();
    plugin.pending_hire = None;
    let admitted = plugin.hire(7, "owner", "oak_hollow", "newcomer", "archer", 0);
    let [Command::StorageGet(read)] = admitted.as_slice() else {
        panic!("admission must read durable registry");
    };
    let answers = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(9),
        }),
    });
    assert!(
        answers.iter().any(|command| matches!(
            command,
            Command::Operation(operation) if operation.request.starts_with("hire-resident")
        )),
        "an adopted adult is admitted: {answers:?}"
    );
}
