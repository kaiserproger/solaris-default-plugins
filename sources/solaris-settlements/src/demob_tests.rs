use super::*;
use solaris_plugin_sdk::domain_operations::DomainOperation;
use solaris_plugin_sdk::inventories::{
    InventoryItem, InventoryOperation, InventorySlot, OwnedInventorySnapshot,
};
use solaris_plugin_sdk::residents::ResidentOrderOperation;

fn read(
    plugin: &mut Settlements,
    command: &[Command],
    registry: &SettlementRegistry,
    version: u64,
) -> Vec<Command> {
    let [Command::StorageGet(request)] = command else {
        panic!("registry read required")
    };
    plugin.storage_get(&StorageGetAnswered {
        request: request.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(registry).unwrap()),
            version: Some(version),
        }),
    })
}

fn save(
    plugin: &mut Settlements,
    command: &[Command],
    registry: &mut SettlementRegistry,
    version: &mut u64,
) -> Vec<Command> {
    let [Command::StorageCas(write)] = command else {
        panic!("durable CAS required")
    };
    assert_eq!(write.expected_version, Some(*version));
    *registry = serde_json::from_str(&write.value).unwrap();
    *version += 1;
    plugin.storage_cas(&StorageCasAnswered {
        request: write.request.clone(),
        outcome: StorageCasOutcome::Committed(*version),
    })
}

fn item(resource_id: &str, count: u32) -> InventoryItem {
    InventoryItem {
        resource_id: resource_id.to_owned(),
        count,
        damage: None,
        enchantments: Vec::new(),
        custom_name: None,
        item_model: None,
    }
}

fn snapshot(
    endpoint: InventoryEndpoint,
    size: u8,
    stock: &[(u8, &str, u32)],
    revision: u64,
) -> OwnedInventorySnapshot {
    OwnedInventorySnapshot {
        endpoint,
        fence: InventoryFence {
            revision,
            snapshot_hash: "a".repeat(64),
        },
        slots: (0..size)
            .map(|slot| InventorySlot {
                slot,
                item: stock
                    .iter()
                    .find(|(where_, _, _)| *where_ == slot)
                    .map(|(_, resource, count)| item(resource, *count)),
            })
            .collect(),
    }
}

fn answer_inventory(
    plugin: &mut Settlements,
    command: &[Command],
    snapshot: OwnedInventorySnapshot,
) -> Vec<Command> {
    let [Command::Operation(query)] = command else {
        panic!("inventory query required")
    };
    assert!(matches!(&query.operation,
        DomainOperation::Inventory(InventoryOperation::Query(query)) if same_inventory_endpoint(&query.endpoint, &snapshot.endpoint)));
    plugin.answered(&OperationAnswered {
        request: query.request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: snapshot.fence.revision,
            payload: OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)),
        }),
    })
}

fn answer_demob(
    plugin: &mut Settlements,
    command: &[Command],
    expected_id: &str,
    expected_revision: u64,
    state: residents::DemobilizeState,
    revision: u64,
) -> Vec<Command> {
    let [Command::Operation(operation)] = command else {
        panic!("native demobilization required")
    };
    assert!(matches!(&operation.operation,
        DomainOperation::ResidentOrder(residents::ResidentOrderOperation::Demobilize(request))
            if request.operation_id == expected_id && request.handle == "resident-1"
                && request.expected_revision == expected_revision));
    plugin.answered(&OperationAnswered {
        request: operation.request.clone(),
        operation_id: Some(expected_id.to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision,
            payload: OperationPayload::ResidentOrder(ResidentOrderResult::Demobilized(
                residents::DemobilizeResult {
                    handle: "resident-1".to_owned(),
                    state,
                    reason: None,
                    returned: Vec::new(),
                    revision,
                },
            )),
        }),
    })
}

#[test]
fn dismissed_soldier_returns_only_issued_gear_and_recovers_after_reload() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "soldier", "resident-1", 4).unwrap();
    let soldier = registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("soldier")
        .unwrap();
    soldier.role = Some(hire::Role::Militia);
    soldier.employer = Some("oak_hollow".to_owned());
    let source = snapshot(
        InventoryEndpoint::Warehouse("warehouse-1".to_owned()),
        27,
        &[
            (0, "minecraft:iron_sword", 1),
            (1, "minecraft:leather_chestplate", 1),
        ],
        11,
    );
    let transfers = vec![
        hire::Transfer {
            source_slot: 0,
            destination: hire::Destination::Equipment(0),
            count: 1,
        },
        hire::Transfer {
            source_slot: 1,
            destination: hire::Destination::Equipment(3),
            count: 1,
        },
    ];
    soldier.issued =
        Some(hire::issued_kit(hire::Role::Militia, "warehouse-1", &source, &transfers).unwrap());
    let mut version = 1;
    let mut plugin = Settlements::default();
    let command = plugin.dismiss(7, "owner", "oak_hollow", "soldier");
    let command = read(&mut plugin, &command, &registry, version);
    let [Command::Operation(query)] = command.as_slice() else {
        panic!("read the serving resident")
    };
    assert!(matches!(
        &query.operation,
        DomainOperation::Resident(residents::ResidentOperation::Query(_))
    ));
    let command = plugin.answered(&OperationAnswered {
        request: query.request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 4,
            payload: OperationPayload::Resident(ResidentResult::Page(residents::ResidentPage {
                residents: vec![residents::ResidentSnapshot {
                    handle: "resident-1".to_owned(),
                    entity_uuid: "entity-1".to_owned(),
                    lifecycle: residents::ResidentLifecycle::AliveLoaded,
                    revision: 4,
                    generation_id: None,
                    pois: residents::ResidentPois {
                        home: None,
                        work: None,
                        meeting: None,
                    },
                    loaded: Some(residents::ResidentLoadedState {
                        x: 0.0,
                        y: 64.0,
                        z: 0.0,
                        health: 20.0,
                        carried: Vec::new(),
                    }),
                }],
                cursor: None,
            })),
        }),
    });
    let command = answer_inventory(
        &mut plugin,
        &command,
        snapshot(
            InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
            6,
            &[
                (0, "minecraft:iron_sword", 1),
                (3, "minecraft:leather_chestplate", 1),
            ],
            11,
        ),
    );
    let command = save(&mut plugin, &command, &mut registry, &mut version);
    assert!(registry.settlements["oak_hollow"].residents["soldier"]
        .role
        .is_some());
    let command = answer_demob(
        &mut plugin,
        &command,
        "dismiss-stop-1",
        11,
        residents::DemobilizeState::Demobilizing,
        12,
    );
    let command = answer_inventory(
        &mut plugin,
        &command,
        snapshot(
            InventoryEndpoint::Warehouse("warehouse-1".to_owned()),
            27,
            &[],
            11,
        ),
    );
    let mut worn = snapshot(
        InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
        6,
        &[
            (0, "minecraft:iron_sword", 1),
            (3, "minecraft:leather_chestplate", 1),
        ],
        12,
    );
    worn.slots[0].item.as_mut().unwrap().custom_name = Some("Private replacement".to_owned());
    let command = answer_inventory(&mut plugin, &command, worn);
    let command = answer_inventory(
        &mut plugin,
        &command,
        snapshot(
            InventoryEndpoint::ResidentCarry("resident-1".to_owned()),
            8,
            &[(0, "minecraft:emerald", 1)],
            12,
        ),
    );
    let command = read(&mut plugin, &command, &registry, version);
    let command = save(&mut plugin, &command, &mut registry, &mut version);
    let [Command::Operation(first)] = command.as_slice() else {
        panic!("issued transfer follows saved intent")
    };
    let DomainOperation::Inventory(InventoryOperation::Transfer(issued)) = &first.operation else {
        panic!("return real gear")
    };
    assert_eq!(issued.operation_id, "dismiss-return-1-0");
    assert_eq!(issued.actor_id, 0);
    assert_eq!(issued.transfers.len(), 1);
    assert_eq!(issued.transfers[0].source_slot, 3);
    assert!(issued
        .transfers
        .iter()
        .all(|transfer| matches!(&transfer.source,
        InventoryEndpoint::ResidentEquipment(handle) if handle == "resident-1")));
    assert_eq!(issued.expected_revisions.len(), 2);
    let mut reloaded = Settlements::default();
    let command = reloaded.recover_demobs();
    let command = read(&mut reloaded, &command, &registry, version);
    let [Command::Operation(replayed)] = command.as_slice() else {
        panic!("restart replays the same return")
    };
    let DomainOperation::Inventory(InventoryOperation::Transfer(replayed_transfer)) =
        &replayed.operation
    else {
        panic!("replay returns the issued items")
    };
    assert_eq!(replayed_transfer.operation_id, issued.operation_id);
    assert_eq!(replayed_transfer.transfers.len(), issued.transfers.len());
    assert_eq!(
        replayed_transfer.expected_revisions[0].fence.snapshot_hash,
        issued.expected_revisions[0].fence.snapshot_hash
    );
    let command = reloaded.answered(&OperationAnswered {
        request: replayed.request.clone(),
        operation_id: Some("dismiss-return-1-0".to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 13,
            payload: OperationPayload::OwnedInventory(InventoryResult::Transfer(vec![
                solaris_plugin_sdk::inventories::InventoryExpectedRevision {
                    endpoint: InventoryEndpoint::Warehouse("warehouse-1".to_owned()),
                    fence: InventoryFence {
                        revision: 11,
                        snapshot_hash: "b".repeat(64),
                    },
                },
                solaris_plugin_sdk::inventories::InventoryExpectedRevision {
                    endpoint: InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
                    fence: InventoryFence {
                        revision: 13,
                        snapshot_hash: "c".repeat(64),
                    },
                },
            ])),
        }),
    });
    let command = answer_inventory(
        &mut reloaded,
        &command,
        snapshot(
            InventoryEndpoint::ResidentEquipment("resident-1".to_owned()),
            6,
            &[(0, "minecraft:iron_sword", 1)],
            14,
        ),
    );
    let command = read(&mut reloaded, &command, &registry, version);
    let command = save(&mut reloaded, &command, &mut registry, &mut version);
    let command = answer_demob(
        &mut reloaded,
        &command,
        "dismiss-finish-1",
        14,
        residents::DemobilizeState::Civilian,
        15,
    );
    let command = read(&mut reloaded, &command, &registry, version);
    let _command = save(&mut reloaded, &command, &mut registry, &mut version);
    let soldier = &registry.settlements["oak_hollow"].residents["soldier"];
    assert_eq!(soldier.handle, "resident-1");
    assert!(
        soldier.role.is_none()
            && soldier.employer.is_none()
            && soldier.issued.is_none()
            && soldier.dismissing.is_none()
    );
    assert_eq!(soldier.work_revision, 15);
}

#[test]
fn startup_recovery_continues_past_a_refused_dead_resident_to_captures() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "attacker".to_owned(), "oak_hollow", "small").unwrap();
    register_settlement(&mut registry, "defender".to_owned(), "pine_haven", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "guard", "resident-guard", 4).unwrap();
    record_settlement_resident(
        &mut registry,
        "oak_hollow",
        "dead_soldier",
        "resident-dead",
        6,
    )
    .unwrap();
    record_settlement_resident(&mut registry, "pine_haven", "soldier", "resident-victim", 7)
        .unwrap();
    let guard = &mut registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("guard")
        .unwrap();
    guard.role = Some(hire::Role::Infantry);
    guard.employer = Some("oak_hollow".to_owned());
    let victim = &mut registry
        .settlements
        .get_mut("pine_haven")
        .unwrap()
        .residents
        .get_mut("soldier")
        .unwrap();
    victim.role = Some(hire::Role::Militia);
    victim.employer = Some("pine_haven".to_owned());
    // The dead resident keeps its durable dismissal intent and issued gear.
    let kit = hire::IssuedKit {
        warehouse: "oak-warehouse".to_owned(),
        items: Vec::new(),
    };
    let dead = &mut registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("dead_soldier")
        .unwrap();
    dead.role = Some(hire::Role::Militia);
    dead.employer = Some("oak_hollow".to_owned());
    dead.issued = Some(kit.clone());
    dead.dismissing = Some(demob_runtime::Intent {
        issued: kit,
        sequence: 1,
        attempt: 0,
        step: demob_runtime::Step::Stop { revision: 11 },
    });
    let mut version = 3;
    // A real capture intent is in flight beside the pending dismissal.
    let mut setup = Settlements::default();
    let start = setup.capture(
        17,
        "attacker",
        "oak_hollow",
        "guard",
        "pine_haven",
        "soldier",
        "7",
    );
    let written = read(&mut setup, &start, &registry, version);
    let _native = save(&mut setup, &written, &mut registry, &mut version);
    assert!(registry.settlements["pine_haven"].residents["soldier"]
        .capturing
        .is_some());
    let before = serde_json::to_value(&registry).unwrap();
    let refuse_dead = |plugin: &mut Settlements, command: &[Command]| {
        let [Command::Operation(operation)] = command else {
            panic!("native demobilization of the dead resident required")
        };
        let DomainOperation::ResidentOrder(ResidentOrderOperation::Demobilize(request)) =
            &operation.operation
        else {
            panic!("recovery must replay the pending dismissal")
        };
        assert_eq!(request.operation_id, "dismiss-stop-1");
        assert_eq!(request.handle, "resident-dead");
        assert_eq!(request.expected_revision, 11);
        plugin.answered(&OperationAnswered {
            request: operation.request.clone(),
            operation_id: Some("dismiss-stop-1".to_owned()),
            outcome: OperationOutcome::Refused(solaris_plugin_sdk::events::OperationRefused {
                reason: OperationFailure::NotFound,
                payload: OperationPayload::None,
            }),
        })
    };
    // Startup recovery: the refused dismissal must not starve capture replay.
    let mut recovered = Settlements::default();
    let command = recovered.recover_demobs();
    assert!(
        matches!(command.as_slice(), [Command::StorageGet(_)]),
        "startup recovery reads the registry once"
    );
    let command = read(&mut recovered, &command, &registry, version);
    let command = refuse_dead(&mut recovered, &command);
    let [Command::Operation(capture)] = command.as_slice() else {
        panic!("capture recovery must still be reached after a refused resident")
    };
    let DomainOperation::ResidentOrder(ResidentOrderOperation::Capture(replay)) =
        &capture.operation
    else {
        panic!("pending capture must replay")
    };
    assert_eq!(replay.operation_id, "capture-1");
    assert_eq!(replay.handle, "resident-victim");
    assert_eq!(replay.custodian, "resident-guard");
    assert_eq!(replay.expected_revision, 7);
    // The durable refusal is never rewritten: intent, gear and ownership stay.
    assert_eq!(
        serde_json::to_value(&registry).unwrap(),
        before,
        "a refused startup dismissal must not touch the registry"
    );
    // A second pass is bounded and ends at the same point, without looping.
    let mut again = Settlements::default();
    let command = again.recover_demobs();
    let command = read(&mut again, &command, &registry, version);
    let command = refuse_dead(&mut again, &command);
    assert!(
        matches!(command.as_slice(), [Command::Operation(_)]),
        "the second pass terminates at capture recovery"
    );
    assert_eq!(serde_json::to_value(&registry).unwrap(), before);
}

#[test]
fn player_dismissal_refusal_still_reports_only_to_its_session() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "soldier", "resident-1", 4).unwrap();
    let kit = hire::IssuedKit {
        warehouse: "oak-warehouse".to_owned(),
        items: Vec::new(),
    };
    let soldier = &mut registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("soldier")
        .unwrap();
    soldier.role = Some(hire::Role::Militia);
    soldier.employer = Some("oak_hollow".to_owned());
    soldier.issued = Some(kit.clone());
    soldier.dismissing = Some(demob_runtime::Intent {
        issued: kit,
        sequence: 1,
        attempt: 0,
        step: demob_runtime::Step::Stop { revision: 11 },
    });
    let version = 3;
    let mut plugin = Settlements::default();
    let command = plugin.dismiss(7, "owner", "oak_hollow", "soldier");
    let command = read(&mut plugin, &command, &registry, version);
    let [Command::Operation(operation)] = command.as_slice() else {
        panic!("dismissal replays the durable intent")
    };
    let command = plugin.answered(&OperationAnswered {
        request: operation.request.clone(),
        operation_id: Some("dismiss-stop-1".to_owned()),
        outcome: OperationOutcome::Refused(solaris_plugin_sdk::events::OperationRefused {
            reason: OperationFailure::NotFound,
            payload: OperationPayload::None,
        }),
    });
    let [Command::SendMessage(message)] = command.as_slice() else {
        panic!("the asking session is told, and nothing else happens")
    };
    assert!(matches!(
        message.target,
        solaris_plugin_sdk::MessageTarget::Session(7)
    ));
}
