use super::*;
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
            SettlementResident {
                handle: format!("handle-{index}"),
                work_revision: index,
                role: None,
                employer: None,
                hiring: None,
                issued: None,
                dismissing: None,
                capturing: None,
                captive_by: None,
                household: None,
                age_origin: None,
            },
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
        handle: "warehouse-1".to_owned(),
        source: WarehouseSource::Authored(settlements::AuthoredWarehouseSource {
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
    let reopened = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(2),
        }),
    });
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
    let refused = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(3),
        }),
    });
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
    let issued = plugin.registry_role(
        7,
        "owner".to_owned(),
        "oak_hollow".to_owned(),
        "steward".to_owned(),
        "member".to_owned(),
        registry,
        Some(2),
    );
    let [Command::StorageCas(write)] = issued.as_slice() else {
        panic!("role update must durably compare-and-swap");
    };
    let commands = plugin.storage_cas(&StorageCasAnswered {
        request: write.request.clone(),
        outcome: StorageCasOutcome::Committed(3),
    });
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
    let record = registry.settlements["oak_hollow"].clone();
    let mut plugin = Settlements::default();
    plugin.views.insert(
        7,
        view::ViewState {
            player: "player-1".to_owned(),
            settlement: "oak_hollow".to_owned(),
            record,
            page: 0,
            request: "view-request".to_owned(),
            instance: Some(("solaris:view-1".to_owned(), 2)),
            stock: Vec::new(),
            warehouse_status: String::new(),
            reading: false,
            dirty: false,
            anchor: [32, 64, -16],
            rotation: 90,
            pending_selection: None,
            selection: Some(("solaris:selection-1".to_owned(), 90)),
        },
    );
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
        site_id: "site-1".to_owned(),
        blueprint: view::GATE_BLUEPRINT.to_owned(),
        anchor: settlements::BlockPosition {
            x: 32,
            y: 64,
            z: -16,
        },
        rotation: 90,
        survey_token: "survey-token".to_owned(),
        expected_site_revision: 3,
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
    let admitted = plugin.issue_with_context(intent(), Some(project()), None, 1);
    assert!(matches!(admitted.as_slice(),
        [Command::Operation(operation)] if matches!(
            &operation.operation,
            solaris_plugin_sdk::domain_operations::DomainOperation::Settlement(
                settlements::SettlementOperation::PrepareStructure(prepare)
            ) if prepare.expected_blueprint_hash.as_deref() == Some(view::GATE_BLUEPRINT_HASH)
                && prepare.rotation == 90
        )
    ));

    plugin.view_left(7);
    assert!(plugin
        .issue_with_context(intent(), Some(project()), None, 2)
        .is_empty());
    assert!(!plugin.pending.contains_key("settlement-request-2"));
}
