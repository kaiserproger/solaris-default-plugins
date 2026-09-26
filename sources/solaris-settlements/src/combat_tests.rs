use super::*;
use solaris_plugin_sdk::domain_operations::DomainOperation;

#[test]
fn guard_requires_settlement_authority_and_attacks_only_the_issued_hostile() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "archer", "resident-1", 4).unwrap();
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("archer")
        .unwrap()
        .role = Some(hire::Role::Archer);
    let stored = serde_json::to_string(&registry).unwrap();
    let mut plugin = Settlements::default();
    plugin.counter = Some(0);
    plugin.counter_version = Some(0);

    let outsider_commands = plugin.defend(
        7,
        "outsider",
        "oak_hollow",
        "archer",
        "0",
        ["4", "64", "4"],
        ["0", "64", "0"],
        None,
    );
    let [Command::StorageGet(read)] = outsider_commands.as_slice() else {
        panic!("guard command reads settlement authority");
    };
    let denied = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored.clone()),
            version: Some(1),
        }),
    });
    assert!(
        matches!(denied.as_slice(), [Command::SendMessage(message)] if message.text.contains("Only an owner or steward"))
    );
    assert!(
        plugin.queued.is_empty(),
        "outsider did not enqueue an order"
    );

    let owner_commands = plugin.defend(
        7,
        "owner",
        "oak_hollow",
        "archer",
        "0",
        ["4", "64", "4"],
        ["0", "64", "0"],
        None,
    );
    let [Command::StorageGet(read)] = owner_commands.as_slice() else {
        panic!("owner reads registry");
    };
    let guard_counter_commands = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored),
            version: Some(1),
        }),
    });
    let [Command::StorageCas(counter)] = guard_counter_commands.as_slice() else {
        panic!("guard reserves a durable operation id");
    };
    let guard_hold_commands = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(1),
    });
    let [Command::Operation(hold)] = guard_hold_commands.as_slice() else {
        panic!("guard issues the native hold order");
    };
    assert!(matches!(
        &hold.operation,
        DomainOperation::ResidentOrder(residents::ResidentOrderOperation::IssueOrder(order))
            if order.expected_order_revisions == [0]
                && matches!(order.order, residents::Order::Hold(_))
    ));

    let guard_attack_counter_commands = plugin.answered(&OperationAnswered {
        request: hold.request.clone(),
        operation_id: Some("settlement-operation-1".to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 9,
            payload: OperationPayload::ResidentOrder(ResidentOrderResult::Order(
                residents::OrderResult {
                    order_revision: 9,
                    members: vec![residents::OrderMemberOutcome {
                        handle: "resident-1".to_owned(),
                        state: residents::OrderMemberState::Applied,
                        formation_slot: Some(0),
                        targets: vec![residents::OrderTarget {
                            target_ref: "issued-hostile".to_owned(),
                            policy_revision: 0,
                            expires_revision: 29,
                            category: residents::HostileCategory::Hostile,
                            position: residents::BlockPosition { x: 6, y: 64, z: 4 },
                        }],
                    }],
                    combat: Vec::new(),
                },
            )),
        }),
    });
    let [Command::StorageCas(counter)] = guard_attack_counter_commands.as_slice() else {
        panic!("the issued target is continued as a second durable operation");
    };
    let guard_attack_commands = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(2),
    });
    let [Command::Operation(attack)] = guard_attack_commands.as_slice() else {
        panic!("guard issues the native attack order");
    };
    let DomainOperation::ResidentOrder(residents::ResidentOrderOperation::IssueOrder(order)) =
        &attack.operation
    else {
        panic!("attack stays a native resident order");
    };
    assert_eq!(order.expected_order_revisions, [9]);
    let residents::Order::Attack(attack) = &order.order else {
        panic!("guard selected the perceived hostile");
    };
    assert_eq!(attack.targets[0].target_ref, "issued-hostile");
    assert_eq!(attack.targets[0].expires_revision, 29);
    assert_eq!(
        attack.policy.permitted,
        [residents::HostileCategory::Hostile]
    );
    assert_eq!(attack.policy.allies, ["resident-1"]);
}

#[test]
fn squad_order_admits_one_authorized_roster_and_reports_blocked_members() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    for (alias, handle, role) in [
        ("archer", "resident-1", hire::Role::Archer),
        ("spear", "resident-2", hire::Role::Spearman),
    ] {
        record_settlement_resident(&mut registry, "oak_hollow", alias, handle, 4).unwrap();
        registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .get_mut(alias)
            .unwrap()
            .role = Some(role);
    }
    let stored = serde_json::to_string(&registry).unwrap();
    let mut plugin = Settlements::default();
    plugin.counter = Some(0);
    plugin.counter_version = Some(0);

    let outsider = plugin.squad_command(
        7,
        "outsider",
        "oak_hollow",
        "spear:5,archer:0",
        Some(["13", "64", "8"]),
        false,
    );
    let [Command::StorageGet(read)] = outsider.as_slice() else {
        panic!("authority is read before group admission");
    };
    let denied = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored.clone()),
            version: Some(1),
        }),
    });
    assert!(matches!(
        denied.as_slice(),
        [Command::SendMessage(message)] if message.text.contains("Only an owner or steward")
    ));
    assert!(plugin.queued.is_empty());

    let owner = plugin.squad_command(
        7,
        "owner",
        "oak_hollow",
        "spear:5,archer:0",
        Some(["13", "64", "8"]),
        false,
    );
    let [Command::StorageGet(read)] = owner.as_slice() else {
        panic!("authorized squad reads its saved roster");
    };
    let reserved = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored.clone()),
            version: Some(1),
        }),
    });
    let [Command::StorageCas(counter)] = reserved.as_slice() else {
        panic!("one operation id fences the entire roster");
    };
    let issued = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(1),
    });
    let [Command::Operation(request)] = issued.as_slice() else {
        panic!("one native group admission is issued");
    };
    let DomainOperation::ResidentOrder(residents::ResidentOrderOperation::IssueOrder(order)) =
        &request.operation
    else {
        panic!("march uses the native group order");
    };
    assert_eq!(order.handles, ["resident-1", "resident-2"]);
    assert_eq!(order.expected_order_revisions, [0, 5]);
    assert!(matches!(
        &order.order,
        residents::Order::Move(move_order)
            if (move_order.anchor.x, move_order.anchor.y, move_order.anchor.z) == (13, 64, 8)
    ));
    let answered = plugin.answered(&OperationAnswered {
        request: request.request.clone(),
        operation_id: Some("settlement-operation-1".to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 9,
            payload: OperationPayload::ResidentOrder(ResidentOrderResult::Order(
                residents::OrderResult {
                    order_revision: 9,
                    members: vec![
                        residents::OrderMemberOutcome {
                            handle: "resident-1".to_owned(),
                            state: residents::OrderMemberState::Applied,
                            formation_slot: Some(0),
                            targets: Vec::new(),
                        },
                        residents::OrderMemberOutcome {
                            handle: "resident-2".to_owned(),
                            state: residents::OrderMemberState::BlockedRoute,
                            formation_slot: None,
                            targets: Vec::new(),
                        },
                    ],
                    combat: Vec::new(),
                },
            )),
        }),
    });
    assert!(matches!(
        answered.as_slice(),
        [Command::SendMessage(message)]
            if message.text.contains("1/2 applied")
                && message.text.contains("resident-2")
                && message.text.contains("revision 9")
    ));

    let halt = plugin.squad_command(7, "owner", "oak_hollow", "archer:9,spear:9", None, false);
    let [Command::StorageGet(read)] = halt.as_slice() else {
        panic!("halt reads current rights");
    };
    let reserved = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored.clone()),
            version: Some(1),
        }),
    });
    let [Command::StorageCas(counter)] = reserved.as_slice() else {
        panic!("halt reserves a single durable operation");
    };
    let issued = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(2),
    });
    let [Command::Operation(request)] = issued.as_slice() else {
        panic!("halt issues one native batch");
    };
    assert!(matches!(
        &request.operation,
        DomainOperation::ResidentOrder(residents::ResidentOrderOperation::CancelOrder(cancel))
            if cancel.handles == ["resident-1", "resident-2"]
                && cancel.expected_order_revisions == [9, 9]
    ));

    let retreat = plugin.squad_command(
        7,
        "owner",
        "oak_hollow",
        "archer:9,spear:9",
        Some(["2", "64", "-3"]),
        true,
    );
    let [Command::StorageGet(read)] = retreat.as_slice() else {
        panic!("retreat reads current rights");
    };
    let reserved = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored),
            version: Some(1),
        }),
    });
    let [Command::StorageCas(counter)] = reserved.as_slice() else {
        panic!("retreat reserves one durable group operation");
    };
    let issued = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(3),
    });
    let [Command::Operation(request)] = issued.as_slice() else {
        panic!("retreat issues one native group order");
    };
    assert!(matches!(
        &request.operation,
        DomainOperation::ResidentOrder(residents::ResidentOrderOperation::IssueOrder(order))
            if order.handles == ["resident-1", "resident-2"]
                && order.expected_order_revisions == [9, 9]
                && matches!(&order.order, residents::Order::Retreat(retreat)
                    if (retreat.anchor.x, retreat.anchor.y, retreat.anchor.z) == (2, 64, -3))
    ));
}

#[test]
fn treatment_uses_bound_settlement_warehouse_and_authoritative_resident_revision() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "archer", "resident-1", 4).unwrap();
    registry.settlements.get_mut("oak_hollow").unwrap().site_id = Some("site-1".to_owned());
    let stored = serde_json::to_string(&registry).unwrap();
    let mut plugin = Settlements::default();
    plugin.warehouse = Some(WarehouseBinding {
        handle: "warehouse-1".to_owned(),
        source: WarehouseSource::VanillaVillage(settlements::VillageWarehouseSource {
            site_id: "site-1".to_owned(),
            container_id: 0,
        }),
        revision: 1,
    });
    plugin.counter = Some(0);
    plugin.counter_version = Some(0);

    let outsider = plugin.treat(7, "outsider", "oak_hollow", "archer");
    let [Command::StorageGet(read)] = outsider.as_slice() else {
        panic!("treatment reads settlement authority");
    };
    let denied = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored.clone()),
            version: Some(1),
        }),
    });
    assert!(matches!(denied.as_slice(), [Command::SendMessage(message)]
        if message.text.contains("Only an owner or steward")));
    assert!(plugin.pending.is_empty());

    let owner = plugin.treat(7, "owner", "oak_hollow", "archer");
    let [Command::StorageGet(read)] = owner.as_slice() else {
        panic!("owner treatment reads the resident roster");
    };
    let query_commands = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(stored),
            version: Some(1),
        }),
    });
    let [Command::Operation(query)] = query_commands.as_slice() else {
        panic!("authenticated resident is queried for its current revision");
    };
    assert!(matches!(&query.operation,
        DomainOperation::Resident(residents::ResidentOperation::Query(query))
            if query.handles == ["resident-1"]));
    let counter_commands = plugin.answered(&OperationAnswered {
        request: query.request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 11,
            payload: OperationPayload::Resident(ResidentResult::Page(residents::ResidentPage {
                residents: vec![residents::ResidentSnapshot {
                    handle: "resident-1".to_owned(),
                    entity_uuid: "entity-1".to_owned(),
                    lifecycle: residents::ResidentLifecycle::AliveLoaded,
                    revision: 11,
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
                        health: 7.0,
                        carried: Vec::new(),
                    }),
                }],
                cursor: None,
            })),
        }),
    });
    let [Command::StorageCas(counter)] = counter_commands.as_slice() else {
        panic!("a durable operation id is reserved for the actual paid treatment");
    };
    let treatment_commands = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(1),
    });
    let [Command::Operation(treatment)] = treatment_commands.as_slice() else {
        panic!("the native medical operation is issued");
    };
    assert!(matches!(&treatment.operation,
        DomainOperation::Resident(residents::ResidentOperation::Treat(command))
            if command.handle == "resident-1"
                && command.expected_revision == 11
                && matches!(&command.source, InventoryEndpoint::Warehouse(handle) if handle == "warehouse-1")
                && command.material.resource_id == "minecraft:bread"
                && command.material.quantity == 1
                && command.heal_milli == 3_000));
    let message = plugin.answered(&OperationAnswered {
        request: treatment.request.clone(),
        operation_id: Some("settlement-operation-1".to_owned()),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 12,
            payload: OperationPayload::Resident(ResidentResult::Treated("resident-1".to_owned())),
        }),
    });
    assert!(matches!(message.as_slice(), [Command::SendMessage(message)]
        if message.text.contains("one bread spent")));
}
