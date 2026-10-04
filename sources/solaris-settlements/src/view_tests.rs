use super::*;
use crate::*;
use solaris_plugin_sdk::client_presentation::{ClientCommand, ViewField, ViewFieldValue};
use solaris_plugin_sdk::events::LoaderViewAction;

fn registry() -> SettlementRegistry {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_project(
        &mut registry,
        "oak_hollow",
        "warehouse_1",
        "structure-17",
        "solaris:warehouse",
        4,
    )
    .unwrap();
    registry
}

#[test]
fn view_pages_roster_and_warehouse_without_losing_member_action_rights() {
    let mut registry = registry();
    let record = registry.settlements.get_mut("oak_hollow").unwrap();
    for index in 0..10 {
        record.residents.insert(
            format!("resident_{index}"),
            serde_json::from_value(serde_json::json!({
                "handle": format!("handle-{index}"),
                "work_revision": index,
            }))
            .unwrap(),
        );
    }
    record.residents.get_mut("resident_8").unwrap().role = Some(hire::Role::Militia);
    let mut plugin = Settlements::default();
    let commands =
        plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    assert!(commands.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::OpenClientView(open))
            if open.owned_view_id == view::VIEW_ID && open.model.page_count == 5
                && open.model.rows.iter().any(|row| row.cells[0] == "Residents" && row.cells[1] == "10")
                && open.model.actions.iter().any(|action| action.action_id == view::FUND && action.enabled)
    )));
    let state = plugin.views.get_mut(&7).unwrap();
    state.stock = (0..11)
        .map(|slot| view::StockRow {
            slot,
            resource_id: format!("minecraft:item_{slot}"),
            count: slot as u32 + 1,
        })
        .collect();
    state.page = 4;
    let model = state.model();
    assert_eq!((model.page, model.page_count), (4, 6));
    assert_eq!(model.rows.len(), 8);
    assert_eq!(model.rows[0].cells[0], "minecraft:item_0");
    state.page = 5;
    let model = state.model();
    assert_eq!(model.rows.len(), 3);
    assert_eq!(model.rows[0].cells[0], "minecraft:item_8");
    state.page = 2;
    let model = state.model();
    assert_eq!(model.rows.len(), 2);
    assert_eq!(model.rows[0].cells[0], "resident_8");
    assert!(model.rows[0].cells[2].contains("militia"));
    state.page = 3;
    let model = state.model();
    assert_eq!(model.rows.len(), 1);
    assert_eq!(model.rows[0].cells[0], "warehouse_1");
    assert_eq!(model.rows[0].cells[1], "structure-17");
    plugin.warehouse = Some(WarehouseBinding {
        handle: "container:minecraft:overworld:0:64:0".to_owned(),
        source: WarehouseSource::Authored(AuthoredWarehouseSource {
            structure_id: "structure-17".to_owned(),
            container_id: 0,
        }),
        revision: 1,
    });
    plugin.views.get_mut(&7).unwrap().instance = Some(("instance-1".to_owned(), 1));
    let updated =
        plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 5, &registry);
    assert!(updated.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::PresentClientView(present))
            if present.model.page == 5
                && present.model.rows.len() == 3
                && present.model.rows[0].cells[0] == "minecraft:item_8"
    )));
}

#[test]
fn explicit_overview_after_dismissal_reopens_instead_of_presenting_to_invisible_screen() {
    let registry = registry();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    plugin.views.get_mut(&7).unwrap().instance = Some(("dismissed-1".to_owned(), 1));
    let commands = plugin.overview(7, "owner", Some("oak_hollow"), 0, true);
    assert!(commands.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::CloseClientView(close))
            if close.view_instance_id == "dismissed-1"
    )));
    let Some(read) = commands.iter().find_map(|command| {
        if let Command::StorageGet(read) = command {
            Some(read)
        } else {
            None
        }
    }) else {
        panic!("an explicit reopen must read current settlement membership");
    };
    assert!(!plugin.views.contains_key(&7));
    let reopened = plugin.test_registry_read(&registry);
    assert!(reopened.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::OpenClientView(open))
            if open.session == 7 && open.owned_view_id == view::VIEW_ID
    )));
    assert!(reopened.iter().all(|command| !matches!(
        command,
        Command::ClientPresentation(ClientCommand::PresentClientView(_))
    )));
}

#[test]
fn revoked_member_loses_open_view_and_funding_is_rechecked_against_registry() {
    let mut registry = registry();
    assign_settlement_role(&mut registry, "oak_hollow", "steward", "steward").unwrap();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(7, "steward".to_owned(), Some("oak_hollow"), 0, &registry);
    plugin.views.get_mut(&7).unwrap().instance = Some(("instance-1".to_owned(), 0));
    assign_settlement_role(&mut registry, "oak_hollow", "steward", "member").unwrap();
    let attempted = plugin.view_action(&LoaderViewAction {
        session: 7,
        view_instance_id: "instance-1".to_owned(),
        view_revision: 0,
        action_id: view::FUND.to_owned(),
        action_sequence: 1,
        fields: vec![ViewField {
            id: "building".to_owned(),
            value: ViewFieldValue::Text("warehouse_1".to_owned()),
        }],
        selection_token: None,
    });
    let [Command::StorageGet(read)] = attempted.as_slice() else {
        panic!("an action must re-read the authoritative registry");
    };
    let refused = plugin.test_registry_read(&registry);
    assert!(refused
        .iter()
        .all(|command| !matches!(command, Command::Operation(_))));
    assert!(refused.iter().any(|command| matches!(command,
        Command::SendMessage(message) if message.text.contains("owner or a steward")
    )));

    let rejected =
        plugin.view_registry_answer(7, "steward".to_owned(), Some("oak_hollow"), 0, &registry);
    assert!(rejected.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::CloseClientView(close))
            if close.session == 7 && close.view_instance_id == "instance-1"
    )));
    assert!(!plugin.views.contains_key(&7));
    let rejected =
        plugin.view_registry_answer(7, "stranger".to_owned(), Some("oak_hollow"), 0, &registry);
    assert!(rejected.iter().all(|command| !matches!(
        command,
        Command::ClientPresentation(ClientCommand::OpenClientView(_))
    )));
}

#[test]
fn committed_role_change_closes_open_view_and_late_open() {
    let mut registry = registry();
    assign_settlement_role(&mut registry, "oak_hollow", "steward", "steward").unwrap();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(8, "steward".to_owned(), Some("oak_hollow"), 0, &registry);
    plugin.views.get_mut(&8).unwrap().instance = Some(("instance-1".to_owned(), 0));
    let version = plugin.test_registry_seed(&registry);
    let issued = plugin.registry_role(
        7,
        "owner".to_owned(),
        "oak_hollow".to_owned(),
        "steward".to_owned(),
        "member".to_owned(),
        registry,
        version,
    );
    let commands = plugin.test_registry_commit(&issued);
    assert!(commands.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::CloseClientView(close))
            if close.session == 8 && close.view_instance_id == "instance-1"
    )));
    assert!(!plugin.views.contains_key(&8));

    let commands = plugin.view_opened(&solaris_plugin_sdk::events::ClientViewOpened {
        request: "late-open".to_owned(),
        session: 8,
        outcome: solaris_plugin_sdk::events::ClientViewOutcome::Opened(
            solaris_plugin_sdk::events::ViewOpened {
                view_instance_id: "instance-2".to_owned(),
                revision: 1,
            },
        ),
    });
    assert!(commands.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::CloseClientView(close))
            if close.session == 8 && close.view_instance_id == "instance-2"
    )));
}
#[test]
fn gate_selection_cannot_project_a_different_anchor_than_the_preview() {
    use solaris_plugin_sdk::client_presentation::{ViewField, ViewFieldValue};
    use solaris_plugin_sdk::events::LoaderViewAction;

    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(7, "player-1".to_owned(), Some("oak_hollow"), 0, &registry);
    let state = plugin.views.get_mut(&7).unwrap();
    state.instance = Some(("solaris:view-1".to_owned(), 2));
    state.anchor = [32, 64, -16];
    state.rotation = 90;
    state.selection = Some(("solaris:selection-1".to_owned(), 90));
    let mut action = LoaderViewAction {
        session: 7,
        view_instance_id: "solaris:view-1".to_owned(),
        view_revision: 2,
        action_id: view::PROJECT_GATE.to_owned(),
        action_sequence: 1,
        fields: vec![
            ViewField {
                id: "rotation".to_owned(),
                value: ViewFieldValue::Selected("90".to_owned()),
            },
            ViewField {
                id: "x".to_owned(),
                value: ViewFieldValue::Number(33.0),
            },
            ViewField {
                id: "y".to_owned(),
                value: ViewFieldValue::Number(64.0),
            },
            ViewField {
                id: "z".to_owned(),
                value: ViewFieldValue::Number(-16.0),
            },
        ],
        selection_token: Some("solaris:selection-1".to_owned()),
    };
    assert!(matches!(
        plugin.view_action(&action).as_slice(),
        [Command::SendMessage(_)]
    ));
    assert!(plugin.pending_view_project_players.is_empty());
    action.fields[1].value = ViewFieldValue::Number(32.0);
    action.action_sequence = 2;
    assert!(matches!(
        plugin.view_action(&action).as_slice(),
        [Command::ListOnlinePlayers(_)]
    ));
}

#[test]
fn preview_prepare_requires_the_same_live_view_through_final_dispatch() {
    let registry = registry();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    plugin.views.get_mut(&7).unwrap().instance = Some(("solaris:view-1".to_owned(), 2));
    let intent = || OperationIntent::Prepare {
        session: 7,
        dimension: "minecraft:overworld".to_owned(),
        blueprint: view::GATE_BLUEPRINT.to_owned(),
        anchor: world_structures::BlockPosition {
            x: 32,
            y: 64,
            z: -16,
        },
        rotation: 90,
        survey_token: "survey-token".to_owned(),
        expected_world_revision: 3,
        preview: Some(view::PreviewOrigin {
            player: "owner".to_owned(),
            settlement: "oak_hollow".to_owned(),
            instance: "solaris:view-1".to_owned(),
        }),
    };
    let project = || PendingSettlementProject {
        settlement: "oak_hollow".to_owned(),
        building_name: "gate_1".to_owned(),
        registry: registry.clone(),
        version: Some(1),
    };
    let admitted = plugin.issue_with_context(intent(), Some(project()), 1);
    assert!(matches!(admitted.as_slice(),
        [Command::Operation(operation)] if matches!(
            &operation.operation,
            solaris_plugin_sdk::domain_operations::DomainOperation::WorldStructure(
                world_structures::WorldStructureOperation::PrepareStructure(prepare)
            ) if prepare.expected_blueprint_hash.as_deref() == Some(view::GATE_BLUEPRINT_HASH)
                && prepare.rotation == 90
        )
    ));

    plugin.view_left(7);
    assert!(plugin
        .issue_with_context(intent(), Some(project()), 2)
        .is_empty());
    assert!(!plugin.pending.contains_key("settlement-request-2"));
}

#[test]
fn malformed_container_binding_refuses_stock_read_and_clears_stale_stock() {
    let registry = registry();
    let mut plugin = Settlements::default();
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    plugin
        .views
        .get_mut(&7)
        .unwrap()
        .stock
        .push(view::StockRow {
            slot: 0,
            resource_id: "minecraft:diamond".to_owned(),
            count: 64,
        });
    plugin.warehouse = Some(WarehouseBinding {
        handle: "container:minecraft:overworld:not-an-x:64:0".to_owned(),
        source: WarehouseSource::Authored(AuthoredWarehouseSource {
            structure_id: "structure-17".to_owned(),
            container_id: 0,
        }),
        revision: 1,
    });
    let commands =
        plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    assert!(commands
        .iter()
        .all(|command| !matches!(command, Command::Operation(_))));
    assert!(plugin.pending_view_stock.is_empty());
    let state = plugin.views.get(&7).unwrap();
    assert!(state.stock.is_empty());
    assert!(!state.reading);
    assert!(commands.iter().any(|command| matches!(command,
        Command::ClientPresentation(ClientCommand::OpenClientView(open))
            if open.model.rows.iter().any(|row| row.cells.iter().any(|cell| cell.contains("no valid physical container")))
    )));
}

#[test]
fn stock_read_rejects_a_different_physical_container() {
    use solaris_plugin_sdk::inventories::{InventoryItem, InventorySlot, OwnedInventorySnapshot};
    let registry = registry();
    let mut plugin = Settlements::default();
    plugin.warehouse = Some(WarehouseBinding {
        handle: "container:minecraft:overworld:0:64:0".to_owned(),
        source: WarehouseSource::Authored(AuthoredWarehouseSource {
            structure_id: "structure-17".to_owned(),
            container_id: 0,
        }),
        revision: 1,
    });
    let commands =
        plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    let request = commands
        .iter()
        .find_map(|command| match command {
            Command::Operation(operation) => Some(operation.request.clone()),
            _ => None,
        })
        .expect("physical stock query");
    plugin
        .view_stock_answer(&OperationAnswered {
            request,
            operation_id: None,
            outcome: OperationOutcome::Committed(OperationCommitted {
                revision: 2,
                payload: OperationPayload::OwnedInventory(InventoryResult::Snapshot(
                    OwnedInventorySnapshot {
                        endpoint: warehouse_endpoint("container:minecraft:overworld:1:64:0")
                            .unwrap(),
                        fence: InventoryFence {
                            revision: 2,
                            snapshot_hash: "a".repeat(64),
                        },
                        slots: vec![InventorySlot {
                            slot: 0,
                            item: Some(InventoryItem {
                                resource_id: "minecraft:diamond".to_owned(),
                                count: 64,
                                damage: None,
                                enchantments: Vec::new(),
                                custom_name: None,
                                item_model: None,
                                max_damage: None,
                                custom_data: None,
                                stew_effects: Vec::new(),
                            }),
                        }],
                    },
                )),
            }),
        })
        .expect("matching request");
    assert!(plugin.views.get(&7).unwrap().stock.is_empty());
    assert!(!plugin.views.get(&7).unwrap().reading);
}

#[test]
fn building_damage_reason_is_visible_and_delayed_success_cannot_cross_reopen() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    record_settlement_certificate(
        &mut registry,
        "oak_hollow",
        "workshop",
        "solaris:workshop",
        BuildingOrigin::Certificate {
            certificate_id: "worksite-1".to_owned(),
        },
        1,
        Vec::new(),
    )
    .unwrap();
    let mut plugin = Settlements::default();
    let commands =
        plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 2, &registry);
    let request = commands
        .iter()
        .find_map(|command| match command {
            Command::Operation(operation) => Some(operation.request.clone()),
            _ => None,
        })
        .unwrap();
    let answer = |valid| OperationAnswered {
        request: request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 2,
            payload: OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(
                world_structures::WorksiteCheck {
                    worksite_id: "worksite-1".to_owned(),
                    world_revision: 2,
                    valid,
                    blocked_points: vec![world_structures::WorksitePointFailure {
                        at: world_structures::BlockPosition { x: 2, y: 64, z: 3 },
                        reason: world_structures::WorksiteFailure::BlockedEntrance,
                    }],
                },
            )),
        }),
    };
    plugin.view_building_answer(&answer(false)).unwrap();
    let state = plugin.views.get_mut(&7).unwrap();
    let model = state.model();
    assert!(model.rows[0].cells[2].contains("blocked-entrance at 2,64,3"));
    plugin.view_left(7);
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 2, &registry);
    assert!(plugin.view_building_answer(&answer(true)).is_none());
    assert!(plugin.views[&7].building_status.is_empty());
}

fn domain_fixture() -> (Settlements, SettlementRegistry) {
    let mut registry = registry();
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .insert(
            "ada".to_owned(),
            serde_json::from_value(serde_json::json!({"handle":"npc-ada","work_revision":0}))
                .unwrap(),
        );
    let mut plugin = Settlements::default();
    plugin.requested_view_panes.insert(7, 6);
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    let state = plugin.views.get_mut(&7).unwrap();
    state.instance = Some(("instance-1".to_owned(), 0));
    state.domain.pending.clear();
    state.domain.errors.clear();
    state.domain.player_context = Some(
        serde_json::from_value(serde_json::json!({
            "session":7,"player":"owner","name":"Owner","dimension":"minecraft:overworld",
            "position":{"x":0.0,"y":64.0,"z":0.0}
        }))
        .unwrap(),
    );
    state.domain.native.insert("npc".to_owned(), serde_json::json!({
        "handle":"npc-ada","revision":1,"loaded":{"x":0,"y":64,"z":0,"health":20,"max_health":20}
    }));
    state.domain.native.insert(
        "controls".to_owned(),
        serde_json::json!([
            {"handle":"npc-ada","order_revision":0,"order":null}
        ]),
    );
    for (key, endpoint, slots) in [
        (
            "warehouse",
            warehouse_endpoint(&"container:minecraft:overworld:0:64:0".to_owned()).unwrap(),
            27,
        ),
        (
            "equipment",
            InventoryEndpoint::NpcEquipment("npc-ada".to_owned()),
            6,
        ),
        (
            "carry",
            InventoryEndpoint::NpcCarry("npc-ada".to_owned()),
            8,
        ),
    ] {
        let mut image = solaris_plugin_sdk::inventories::OwnedInventorySnapshot {
            endpoint,
            fence: InventoryFence {
                revision: 7,
                snapshot_hash: "a".repeat(64),
            },
            slots: (0..slots)
                .map(|slot| solaris_plugin_sdk::inventories::InventorySlot { slot, item: None })
                .collect(),
        };
        if key == "warehouse" {
            for (slot, id) in [
                military_catalog::ARMING_SWORD,
                military_catalog::LONGSWORD,
                military_catalog::MAIL_HAUBERK,
                military_catalog::PAVISE,
                "minecraft:emerald",
            ]
            .into_iter()
            .enumerate()
            {
                image.slots[slot].item = Some(solaris_plugin_sdk::inventories::InventoryItem {
                    resource_id: if id == "minecraft:emerald" {
                        id.to_owned()
                    } else {
                        "minecraft:paper".to_owned()
                    },
                    count: 1,
                    damage: None,
                    enchantments: Vec::new(),
                    custom_name: None,
                    item_model: (id != "minecraft:emerald").then(|| id.to_owned()),
                    max_damage: None,
                    custom_data: None,
                    stew_effects: Vec::new(),
                });
            }
        }
        state
            .domain
            .native
            .insert(key.to_owned(), serde_json::to_value(image).unwrap());
    }
    (plugin, registry)
}

fn domain_button(plugin: &Settlements, operation: &str, sequence: u64) -> LoaderViewAction {
    let (instance, revision) = plugin.views[&7].instance.as_ref().unwrap();
    LoaderViewAction {
        session: 7,
        view_instance_id: instance.clone(),
        view_revision: *revision,
        action_id: format!("solaris-settlements:{operation}"),
        action_sequence: sequence,
        fields: Vec::new(),
        selection_token: None,
    }
}

#[test]
fn repeated_button_sequence_cannot_cancel_a_newer_confirmation() {
    let (mut plugin, _) = domain_fixture();
    let cancel = domain_button(&plugin, "cancel_terms", 10);
    plugin.view_action(&cancel);
    let hire = domain_button(&plugin, "hire", 11);
    plugin.view_action(&hire);
    assert!(plugin.views[&7].domain.terms.is_some());
    let replay = domain_button(&plugin, "cancel_terms", 10);
    plugin.view_action(&replay);
    assert!(plugin.views[&7].domain.terms.is_some());
}

#[test]
fn kit_choice_uses_current_submitted_form_not_previous_form() {
    let (mut plugin, _) = domain_fixture();
    plugin.views.get_mut(&7).unwrap().domain.option = 1;
    plugin.views.get_mut(&7).unwrap().domain.form.insert(
        "loadout".to_owned(),
        ViewFieldValue::Text("invalid-old-value".to_owned()),
    );
    let mut hire = domain_button(&plugin, "hire", 1);
    hire.fields.push(ViewField {
        id: "loadout".to_owned(),
        value: ViewFieldValue::Text("longsword".to_owned()),
    });
    plugin.view_action(&hire);
    assert_eq!(
        plugin.views[&7]
            .domain
            .terms
            .as_ref()
            .unwrap()
            .arguments
            .last()
            .unwrap(),
        "longsword"
    );
}

#[test]
fn changed_roster_cannot_retarget_selected_resident_on_confirmation() {
    let (mut plugin, mut registry) = domain_fixture();
    let hire = domain_button(&plugin, "hire", 1);
    plugin.view_action(&hire);
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .insert(
            "aaron".to_owned(),
            serde_json::from_value(serde_json::json!({"handle":"npc-aaron","work_revision":0}))
                .unwrap(),
        );
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    let state = plugin.views.get_mut(&7).unwrap();
    assert!(state.domain.terms.is_none());
    assert_eq!(state.resident_alias(), Some("ada"));
}

#[test]
fn confirmed_hire_rechecks_consumed_resources_and_revoked_rights() {
    for revoke in [false, true] {
        let (mut plugin, _) = domain_fixture();
        let hire = domain_button(&plugin, "hire", 1);
        plugin.view_action(&hire);
        let state = plugin.views.get_mut(&7).unwrap();
        state.domain.terms.as_mut().unwrap().confirming = true;
        if revoke {
            state.record.owner = "new-owner".to_owned();
        } else {
            let mut stock = state.inventory_image("warehouse").unwrap();
            stock.slots.iter_mut().for_each(|slot| slot.item = None);
            stock.fence.revision += 1;
            state
                .domain
                .native
                .insert("warehouse".to_owned(), serde_json::to_value(stock).unwrap());
        }
        let commands = plugin.view_domain_ready(7);
        assert!(plugin.views[&7].domain.terms.is_none());
        assert!(commands.iter().all(|command| !matches!(
            command,
            Command::Operation(_) | Command::StorageGet(_) | Command::StorageCas(_)
        )));
        assert!(plugin.pending_registry_read.is_none());
    }
}

#[test]
fn a_quote_repriced_after_selection_cannot_submit_the_old_confirmation() {
    let (mut plugin, _) = domain_fixture();
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.pane = 4;
    state
        .domain
        .trade
        .ledger
        .propose(commerce::Quote {
            id: "quote-1".into(),
            owner: "owner".into(),
            seller: "warehouse".into(),
            buyer: "npc-ada".into(),
            item: "minecraft:bread".into(),
            quantity: 1,
            payment_item: "minecraft:emerald".into(),
            payment_quantity: 2,
            owner_revision: 3,
            seller_revision: 7,
            buyer_revision: 7,
            source_hash: "a".repeat(64),
            buyer_hash: "b".repeat(64),
        })
        .unwrap();
    state.domain.trade.plans.insert(
        "quote-1".into(),
        serde_json::from_value(serde_json::json!({
            "scope":"oak_hollow","resident":"ada","seller_slot":0,"buyer_slot":0,
            "seller_receive":1,"buyer_receive":1,"accepted":false
        }))
        .unwrap(),
    );
    state.domain.versions.insert("trade".to_owned(), Some(1));
    let accept = domain_button(&plugin, "trade_accept", 1);
    plugin.view_action(&accept);
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.terms.as_mut().unwrap().confirming = true;
    state
        .domain
        .trade
        .ledger
        .trades
        .get_mut("quote-1")
        .unwrap()
        .quote
        .payment_quantity = 3;
    state.domain.versions.insert("trade".to_owned(), Some(2));
    let commands = plugin.view_domain_ready(7);
    assert!(plugin.views[&7].domain.terms.is_none());
    assert!(plugin.pending_registry_read.is_none());
    assert!(commands.iter().all(|command| !matches!(
        command,
        Command::Operation(_) | Command::StorageGet(_) | Command::StorageCas(_)
    )));
}

#[test]
fn removed_selected_resident_stays_unselected_until_explicit_reselection() {
    let (mut plugin, mut registry) = domain_fixture();
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .remove("ada");
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .insert(
            "beatrice".to_owned(),
            serde_json::from_value(serde_json::json!({"handle":"npc-beatrice","work_revision":0}))
                .unwrap(),
        );
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    assert_eq!(plugin.views[&7].resident_alias(), None);
    assert!(plugin.views[&7].operation_reason("hire").is_err());
}

#[test]
fn kit_comparison_exposes_signing_wage_and_refuses_foreign_equipment() {
    let (mut plugin, _) = domain_fixture();
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.option = 1;
    let mut rows = Vec::new();
    state.kit_comparison_rows(&mut rows);
    assert!(rows
        .iter()
        .any(|row| row.cells[0] == military_catalog::LONGSWORD));
    assert!(rows
        .iter()
        .any(|row| row.cells[0] == military_catalog::PAVISE));
    assert!(rows
        .iter()
        .any(|row| row.cells[0] == "minecraft:emerald" && row.cells[1] == "1"));
    let mut equipment = state.inventory_image("equipment").unwrap();
    equipment.endpoint = InventoryEndpoint::NpcEquipment("foreign-npc".to_owned());
    state.domain.native.insert(
        "equipment".to_owned(),
        serde_json::to_value(equipment).unwrap(),
    );
    assert!(state
        .kit_plan(hire::Role::Infantry, Some("longsword"))
        .is_err());
}

#[test]
fn only_delivered_owner_scoped_facts_enter_the_bounded_payload() {
    let (mut plugin, mut registry) = domain_fixture();
    register_settlement(&mut registry, "enemy".to_owned(), "hidden-city", "small").unwrap();
    record_settlement_project(
        &mut registry,
        "hidden-city",
        "secret-stock",
        "secret-ref",
        "solaris:warehouse",
        4,
    )
    .unwrap();
    registry
        .settlements
        .get_mut("hidden-city")
        .unwrap()
        .buildings
        .get_mut("secret-stock")
        .unwrap()
        .entrance_positions = vec![StoredPosition {
        x: 9000,
        y: 64,
        z: 9000,
    }];
    plugin.view_tick = 100;
    plugin.view_registry_answer(7, "owner".to_owned(), Some("oak_hollow"), 0, &registry);
    let state = plugin.views.get_mut(&7).unwrap();
    assert!(!state.known_route_cells().contains(&[9000, 64, 9000]));
    state.domain.pending.clear();
    plugin.view_tick = 120;
    plugin.view_domain_ready(7);
    let state = plugin.views.get_mut(&7).unwrap();
    let page_count = state.model().page_count;
    for page in 0..page_count {
        state.page = page as usize;
        let model = state.model();
        assert!(model.rows.iter().any(|row| row.cells[0] == "Source"));
        assert!(model
            .rows
            .iter()
            .any(|row| row.cells[0] == "observed_at" && row.cells[1] == "100"));
        assert!(model
            .rows
            .iter()
            .any(|row| row.cells[0] == "delivered_at" && row.cells[1] == "120"));
        assert!(model.rows.len() <= PAGE_ROWS);
        let payload = serde_json::to_string(&model).unwrap();
        assert!(!payload.contains("hidden-city"));
        assert!(!payload.contains("secret-stock"));
        assert!(!payload.contains("9000"));
    }
}

fn payment_projection_fixture() -> (Settlements, payment_mandate::Mandate, Vec<String>) {
    let (mut plugin, _) = domain_fixture();
    let source = serde_json::json!({"scope":"oak_hollow","building":"warehouse_1","live_ref":"structure-17","dimension":"minecraft:overworld","at":{"x":0,"y":64,"z":0}});
    let destination = serde_json::json!({"scope":"buyer-home","building":"escrow","live_ref":"structure-18","dimension":"minecraft:overworld","at":{"x":10,"y":64,"z":0}});
    let commodity =
        serde_json::json!({"resource":"minecraft:bread","enchantments":[],"stew_effects":[]});
    let terms: payment_mandate::Terms = serde_json::from_value(serde_json::json!({
        "shipment":"shipment-1","buyer":"buyer","buyer_scope":"buyer-home","authorized_registry_revision":3,
        "seller":"owner","source":source,"destination":destination,"escrow":destination,"carrier":"npc-ada",
        "recipient":"npc-ada","recipient_scope":"oak_hollow","recipient_entity_uuid":"original-recipient-uuid",
        "quantity":2,"commodity":commodity,"price":7,
        "emerald":{"resource":"minecraft:emerald","enchantments":[],"stew_effects":[]},
        "escrow_slot":0,"escrow_fence":{"revision":7,"snapshot_hash":"a".repeat(64)},
        "reserve_operation":"reserve-1","payout_operation":"payout-1","storage_guards":[{"key":"scope-1","version":1}]
    })).unwrap();
    let (blob, chunks) = payment_mandate::Blob::encode(&terms).unwrap();
    let header: payment_mandate::Mandate = serde_json::from_value(serde_json::json!({
        "shipment":terms.shipment,"buyer":terms.buyer,"buyer_scope":terms.buyer_scope,"registry_revision":3,
        "seller":terms.seller,"source":source,"destination":destination,"carrier":terms.carrier,
        "recipient":terms.recipient,"recipient_scope":terms.recipient_scope,"recipient_entity_uuid":terms.recipient_entity_uuid,
        "quantity":2,"commodity_digest":"commodity-digest","contract":blob,"state":"reserve_intent"
    })).unwrap();
    let contract: caravan::Contract = serde_json::from_value(serde_json::json!({
        "party":{"id":"shipment-1","owner":{"kind":"player","player":"owner"},"leader":{"kind":"npc","handle":"npc-ada","home":"oak_hollow","resident":"ada"},"members":[],"executor_epoch":1,"parked":false},
        "carrier":"npc-ada","source":source,"destination":destination,"item":"minecraft:bread","quantity":2,
        "manifest":commodity,"paid_mandate":"shipment-1","route":[{"x":0,"y":64,"z":0},{"x":10,"y":64,"z":0}],
        "cursor":0,"poll_ticks":10,"max_ticks":2000,"next_due":0,"status":"loading","loaded":false,
        "attempt":0,"started":false,"native_ticks":0,"geometry_cells":0,"receipts":[]
    })).unwrap();
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.pane = 5;
    state
        .domain
        .caravan
        .contracts
        .insert("shipment-1".to_owned(), contract);
    state
        .domain
        .pending
        .insert("paid-header".to_owned(), DomainRead::PaidHeader);
    (plugin, header, chunks)
}

fn payment_answer(plugin: &mut Settlements, value: String) -> Vec<Command> {
    let request = plugin.views[&7]
        .domain
        .pending
        .keys()
        .next()
        .unwrap()
        .clone();
    plugin
        .view_domain_get(&StorageGetAnswered {
            request,
            outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                value: Some(value),
                version: Some(11),
            }),
        })
        .unwrap()
}

#[test]
fn payment_report_exposes_only_digest_verified_exact_terms_not_invented_payment() {
    for corrupt in [false, true] {
        let (mut plugin, header, mut chunks) = payment_projection_fixture();
        plugin.view_tick = 150;
        payment_answer(&mut plugin, serde_json::to_string(&header).unwrap());
        if corrupt {
            chunks[0] = chunks[0].replace("\"price\":7", "\"price\":8");
        }
        for chunk in chunks {
            payment_answer(&mut plugin, chunk);
        }
        let state = plugin.views.get_mut(&7).unwrap();
        if corrupt {
            assert!(state.domain.paid_terms.is_none());
            assert!(state.domain.paid_header.is_none());
        } else {
            assert_eq!(state.domain.paid_terms.as_ref().unwrap().price, 7);
            assert_eq!(state.domain.delivered_tick, Some(150));
            let pages = state.model().page_count;
            let mut cells = Vec::new();
            for page in 0..pages {
                state.page = page as usize;
                cells.extend(state.model().rows.into_iter().flat_map(|row| row.cells));
            }
            assert!(cells.iter().any(|cell| cell == "7 minecraft:emerald"));
            assert!(cells.iter().any(|cell| cell == &header.contract.digest));
            assert!(!cells.iter().any(|cell| cell == "Paid"));
        }
    }
}

#[test]
fn unauthorized_paid_header_never_reads_or_exposes_its_body() {
    let (mut plugin, mut header, _) = payment_projection_fixture();
    header.buyer = "foreign-buyer".to_owned();
    header.seller = "foreign-seller".to_owned();
    let commands = payment_answer(&mut plugin, serde_json::to_string(&header).unwrap());
    assert!(plugin.views[&7].domain.paid_header.is_none());
    assert!(plugin.views[&7].domain.paid_terms.is_none());
    assert!(commands
        .iter()
        .all(|command| !matches!(command, Command::StorageGet(_))));
}

#[test]
fn confirmation_waits_for_slow_stock_read_and_dispatches_once_after_completion() {
    let (mut plugin, _) = domain_fixture();
    let hire = domain_button(&plugin, "hire", 1);
    plugin.view_action(&hire);
    let stock = plugin.views[&7].inventory_image("warehouse").unwrap();
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.terms.as_mut().unwrap().confirming = true;
    state.reading = true;
    assert!(plugin.view_domain_ready(7).is_empty());
    assert!(plugin.views[&7].domain.terms.is_some());
    assert!(plugin.pending_hire.is_none());
    plugin.pending_view_stock.insert(
        "late-stock".to_owned(),
        (7, "container:minecraft:overworld:0:64:0".to_owned()),
    );
    let answer = OperationAnswered {
        request: "late-stock".to_owned(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 7,
            payload: OperationPayload::OwnedInventory(InventoryResult::Snapshot(stock)),
        }),
    };
    let commands = plugin.view_stock_answer(&answer).unwrap();
    assert!(plugin.views[&7].domain.terms.is_none());
    assert!(plugin.pending_hire.is_some());
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, Command::StorageGet(_)))
            .count(),
        1
    );
    assert!(plugin.view_stock_answer(&answer).is_none());
}

#[test]
fn known_map_pages_match_report_sources_and_do_not_reveal_foreign_or_unproven_positions() {
    let (mut plugin, mut registry) = domain_fixture();
    let record = registry.settlements.get_mut("oak_hollow").unwrap();
    let building = record.buildings.get_mut("warehouse_1").unwrap();
    building.dimension = Some("minecraft:overworld".to_owned());
    building.entrance_positions = (0..20).map(|x| StoredPosition { x, y: 64, z: 0 }).collect();
    record_settlement_project(
        &mut registry,
        "oak_hollow",
        "unproven-dimension",
        "structure-unproven",
        "solaris:warehouse",
        4,
    )
    .unwrap();
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .buildings
        .get_mut("unproven-dimension")
        .unwrap()
        .entrance_positions = vec![StoredPosition {
        x: 2000,
        y: 64,
        z: 2000,
    }];
    register_settlement(&mut registry, "enemy".to_owned(), "hidden-map", "small").unwrap();
    record_settlement_project(
        &mut registry,
        "hidden-map",
        "secret-stock",
        "secret-structure",
        "solaris:warehouse",
        4,
    )
    .unwrap();
    let secret = registry
        .settlements
        .get_mut("hidden-map")
        .unwrap()
        .buildings
        .get_mut("secret-stock")
        .unwrap();
    secret.dimension = Some("minecraft:overworld".to_owned());
    secret.entrance_positions = vec![StoredPosition {
        x: 9000,
        y: 64,
        z: 9000,
    }];
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.pane = 5;
    state.domain.observed_tick = 100;
    state.domain.delivered_tick = Some(120);
    state.domain.known = registry.settlements.into_iter().collect();
    state.domain.native.get_mut("npc").unwrap()["loaded"]["dimension"] =
        serde_json::json!("minecraft:the_nether");
    state.domain.native.get_mut("npc").unwrap()["loaded"]["x"] = serde_json::json!(50.25);
    let page_count = state.model().page_count;
    let mut entrances = std::collections::BTreeSet::new();
    let mut observed_pose = false;
    for page in 0..page_count {
        state.page = page as usize;
        let model = state.model();
        assert!(model.markers.len() <= 4);
        assert!(model.rows.len() <= PAGE_ROWS);
        assert!(model
            .rows
            .iter()
            .any(|row| row.cells[0] == "observed_at" && row.cells[1] == "100"));
        assert!(model
            .rows
            .iter()
            .any(|row| row.cells[0] == "delivered_at" && row.cells[1] == "120"));
        let mut slots = std::collections::BTreeSet::new();
        for marker in &model.markers {
            assert!(slots.insert(marker.marker_id.clone()));
            let slot: usize = marker
                .marker_id
                .strip_prefix("known-map-")
                .unwrap()
                .parse()
                .unwrap();
            assert!(slot < MAP_SLOTS);
            assert!(marker.selection_token.is_none() && marker.action_id.is_none());
            assert!(model
                .rows
                .iter()
                .any(|row| row.cells[0] == marker.marker_id));
            let position = marker.position.as_ref().unwrap();
            assert_ne!((position.x, position.z), (9000.0, 9000.0));
            assert_ne!((position.x, position.z), (2000.0, 2000.0));
            if position.dimension == "minecraft:overworld" {
                entrances.insert((position.x as i32, position.y as i32, position.z as i32));
            } else {
                assert_eq!(position.dimension, "minecraft:the_nether");
                assert_eq!(position.x, 50.25);
                observed_pose = true;
            }
        }
        let payload = serde_json::to_string(&model).unwrap();
        assert!(!payload.contains("hidden-map"));
        assert!(!payload.contains("secret-stock"));
    }
    assert_eq!(entrances, (0..20).map(|x| (x, 64, 0)).collect());
    assert!(observed_pose);
    state.domain.delivered_tick = None;
    state.page = 0;
    assert!(state.model().markers.is_empty());
}

#[test]
fn pre_admission_production_pause_preserves_the_physical_reason_and_retry_deadline() {
    let (mut plugin, _) = domain_fixture();
    let state = plugin.views.get_mut(&7).unwrap();
    state.domain.pane = 1;
    let intent = crate::production_tests::intent("paused-order");
    state.domain.production.jobs.insert(
        "paused-order".to_owned(),
        production::Job {
            intent,
            outcome: None,
            pause: Some(production::Pause {
                reason: "observed workstation demolished at 12,64,9".to_owned(),
                retry_tick: 4321,
            }),
        },
    );
    let pages = state.model().page_count;
    let mut rows = Vec::new();
    for page in 0..pages {
        state.page = page as usize;
        rows.extend(state.model().rows);
    }
    assert!(rows.iter().any(|row| row.cells[1] == "paused"
        && row.cells[2] == "observed workstation demolished at 12,64,9"));
    assert!(rows.iter().any(|row| row.cells[1] == "4321"));
    assert!(state.domain.production.jobs["paused-order"]
        .outcome
        .is_none());
}

fn public_player() -> solaris_plugin_sdk::events::PlayerSnapshot {
    solaris_plugin_sdk::events::PlayerSnapshot {
        player: "visitor".to_owned(),
        session: 71,
        name: "Visitor".to_owned(),
        dimension: "minecraft:overworld".to_owned(),
        position: solaris_plugin_sdk::types::Position {
            x: 12.0,
            y: 64.0,
            z: 9.0,
        },
    }
}

fn public_projection(remaining: u32) -> serde_json::Value {
    use crate::commerce_player::{Head, Need, Offer, State};
    let head = Head {
        id: "food".to_owned(),
        owner: "owner".to_owned(),
        terms: payment_mandate::Blob::encode(&serde_json::json!({"private": "warehouse-image"}))
            .unwrap()
            .0,
        public: Offer {
            id: "food".to_owned(),
            owner: "owner".to_owned(),
            location: caravan::Container {
                scope: "home".to_owned(),
                building: "stock".to_owned(),
                live_ref: "native-stock".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                at: caravan::Cell { x: 0, y: 64, z: 0 },
            },
            need: Need::Supply {
                resource: "minecraft:wheat".to_owned(),
                target: 3,
            },
            fixed_emerald_reward: 2,
        },
        state: State::Offered,
        participant: None,
        reservation: None,
        escort: None,
        intent: None,
        receipt: None,
        release_operation: None,
        blocked: None,
        attempt: 0,
    };
    serde_json::json!({"contract": head.projection("visitor").unwrap(), "remaining_supply": remaining})
}

fn public_fixture() -> Settlements {
    let mut plugin = Settlements::default();
    let player = public_player();
    plugin.view_public_open(&player);
    let state = plugin
        .public_contract_views
        .get_mut(&player.session)
        .unwrap();
    state.identity = None;
    state.pending = None;
    state.instance = Some(("public-view".to_owned(), 4));
    plugin
}

#[test]
fn public_contracts_do_not_require_or_disclose_membership_and_do_not_invent_empty_offers() {
    let mut plugin = public_fixture();
    assert!(plugin.views.is_empty());
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    assert!(state.contracts.is_none());
    let unobserved = serde_json::to_value(state.model()).unwrap().to_string();
    assert!(!unobserved.contains("warehouse-image"));
    assert!(!state
        .model()
        .actions
        .iter()
        .any(|action| action.enabled && action.action_id == "solaris-settlements:trade_quote"));
    state.pending = Some("player-contracts".to_owned());
    plugin.view_player_contract_outputs(vec![message_session(
        71,
        r#"{"contracts":[],"truncated":false}"#,
    )]);
    assert_eq!(
        plugin.public_contract_views[&71]
            .contracts
            .as_ref()
            .unwrap(),
        &Vec::<serde_json::Value>::new()
    );
}

#[test]
fn public_receipt_fragments_are_reassembled_and_foreign_inventory_is_never_presented() {
    let mut plugin = public_fixture();
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    state.selected = "food".to_owned();
    state.pending = Some("player-contract-receipt".to_owned());
    let own = serde_json::json!({
        "id":"food", "participant":{"player":"visitor","session":71},
        "own_inventory":{"endpoint":{"player_inventory":"visitor"},"custom_name":"émeraude"}
    })
    .to_string();
    let split = own.find("émeraude").unwrap();
    plugin.view_player_contract_outputs(vec![
        message_session(71, &own[..split]),
        message_session(71, &own[split..]),
    ]);
    assert_eq!(
        plugin.public_contract_views[&71].receipt.as_ref().unwrap()["participant"]["player"],
        "visitor"
    );
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    state.pending = Some("player-contract-receipt".to_owned());
    plugin.view_player_contract_outputs(vec![message_session(
        71,
        serde_json::json!({
            "id":"food", "participant":{"player":"stranger","session":9},
            "own_inventory":{"private":"foreign-warehouse-afterimage"}
        })
        .to_string(),
    )]);
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    assert!(state.receipt.is_none());
    assert!(!serde_json::to_value(state.model())
        .unwrap()
        .to_string()
        .contains("foreign-warehouse-afterimage"));
}

#[test]
fn changed_current_need_cancels_confirmation_before_any_intent_and_replay_cannot_restore_it() {
    let mut plugin = public_fixture();
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    let original = public_projection(3);
    state.selected = "food".to_owned();
    state.contracts = Some(vec![original.clone()]);
    state.terms = Some(("player-contract-accept".to_owned(), original, true));
    state.pending = Some("player-contract-status".to_owned());
    plugin
        .view_player_contract_outputs(vec![message_session(71, public_projection(1).to_string())]);
    let state = &plugin.public_contract_views[&71];
    assert!(state.terms.is_none());
    assert!(state.pending.is_none());
    assert_eq!(state.selected_contract().unwrap()["remaining_supply"], 1);
    let revision = state.instance.as_ref().unwrap().1;
    let action = LoaderViewAction {
        session: 71,
        view_instance_id: "public-view".to_owned(),
        view_revision: revision,
        action_sequence: 1,
        action_id: CONFIRM_TERMS.to_owned(),
        fields: Vec::new(),
        selection_token: None,
    };
    plugin.view_action(&action);
    plugin.view_action(&action);
    assert!(plugin.public_contract_views[&71].terms.is_none());
    assert!(plugin.public_contract_views[&71].identity.is_none());
}

#[test]
fn selected_status_is_independent_of_full_active_index_and_zero_need_cannot_be_accepted() {
    let mut plugin = public_fixture();
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    state.contracts = Some(
        (0..crate::commerce_player::MAX_OFFERS)
            .map(|index| {
                let mut value = public_projection(3);
                value["contract"]["offer"]["id"] = serde_json::json!(format!("offer_{index}"));
                value
            })
            .collect(),
    );
    state.selected = "food".to_owned();
    state.pending = Some("player-contract-status".to_owned());
    plugin
        .view_player_contract_outputs(vec![message_session(71, public_projection(0).to_string())]);
    let state = plugin.public_contract_views.get_mut(&71).unwrap();
    assert_eq!(state.selected_contract().unwrap()["remaining_supply"], 0);
    assert!(!state
        .model()
        .actions
        .iter()
        .any(|action| action.enabled && action.action_id == "solaris-settlements:trade_quote"));
    assert_eq!(
        state.contracts.as_ref().unwrap().len(),
        crate::commerce_player::MAX_OFFERS
    );
}

#[test]
fn public_entry_opens_observed_public_view_not_overview() {
    use solaris_plugin_sdk::events::{
        ClientViewOpened, ClientViewOutcome, LoaderViewRequest, OnlinePlayersAnswered, ViewOpened,
    };

    let mut plugin = Settlements::default();
    // Guest (non-member) asks for the public screen by its own key.
    let commands = plugin.view_request(&LoaderViewRequest {
        session: 71,
        request_kind: view::PUBLIC_VIEW_ID.to_owned(),
    });
    assert!(commands
        .iter()
        .any(|command| matches!(command, Command::ListOnlinePlayers(_))));
    // Overview must not win: no member view opens and no member state exists.
    assert!(plugin.views.is_empty());
    assert!(!commands.iter().any(|command| matches!(
        command,
        Command::ClientPresentation(ClientCommand::OpenClientView(open))
            if open.owned_view_id == view::VIEW_ID
    )));
    let player_request = plugin
        .pending_view_players
        .keys()
        .next()
        .cloned()
        .expect("public player lookup queued");
    // Authoritative online-players answer for the same guest session.
    let commands = plugin.view_players_answered(&OnlinePlayersAnswered {
        request: player_request,
        players: vec![public_player()],
        truncated: false,
    });
    let open = commands
        .iter()
        .find_map(|command| match command {
            Command::ClientPresentation(ClientCommand::OpenClientView(open)) => Some(open),
            _ => None,
        })
        .expect("public entry must open the owned public screen");
    // Wire title is the raw owned id (attempt-6 receipt), never the authoring title.
    assert_eq!(open.session, 71);
    assert_eq!(open.owned_view_id, view::PUBLIC_VIEW_ID);
    assert!(plugin.views.is_empty());
    assert!(plugin.public_contract_views.contains_key(&71));
    // Observed open completes the handshake: the plugin binds the instance and presents.
    let request = plugin.public_contract_views[&71].request.clone();
    let commands = plugin.view_opened(&ClientViewOpened {
        request,
        session: 71,
        outcome: ClientViewOutcome::Opened(ViewOpened {
            view_instance_id: "public-open-1".to_owned(),
            revision: 1,
        }),
    });
    assert!(commands.iter().any(|command| matches!(
        command,
        Command::ClientPresentation(ClientCommand::PresentClientView(_))
    )));
    assert_eq!(
        plugin.public_contract_views[&71].instance,
        Some(("public-open-1".to_owned(), 2))
    );
}

#[test]
fn overview_key_still_routes_to_member_pane_not_public() {
    use solaris_plugin_sdk::events::LoaderViewRequest;

    let mut plugin = Settlements::default();
    let _ = plugin.view_request(&LoaderViewRequest {
        session: 7,
        request_kind: view::VIEW_ID.to_owned(),
    });
    assert_eq!(plugin.pending_view_players.values().next(), Some(&(7, 0)),);
    assert!(plugin.public_contract_views.is_empty());
}
