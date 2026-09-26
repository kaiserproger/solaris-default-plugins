use super::*;
use solaris_plugin_sdk::domain_operations::DomainOperation;

#[test]
fn wounded_pair_requires_medic_and_reports_partial_move_as_not_evacuated() {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    for (alias, handle, role) in [
        ("medic", "resident-medic", hire::Role::Medic),
        ("patient", "resident-patient", hire::Role::Infantry),
    ] {
        record_settlement_resident(&mut registry, "oak_hollow", alias, handle, 4).unwrap();
        let resident = registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .get_mut(alias)
            .unwrap();
        resident.role = Some(role);
        resident.employer = Some("oak_hollow".to_owned());
    }
    let mut plugin = Settlements::default();
    plugin.counter = Some(0);
    plugin.counter_version = Some(0);
    let command = plugin.evacuate(
        7,
        "owner",
        "oak_hollow",
        "medic",
        "3",
        "patient",
        "5",
        ["10", "64", "8"],
    );
    let [Command::StorageGet(read)] = command.as_slice() else {
        panic!("read roster")
    };
    let query = plugin.storage_get(&StorageGetAnswered {
        request: read.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(&registry).unwrap()),
            version: Some(1),
        }),
    });
    let [Command::Operation(query)] = query.as_slice() else {
        panic!("query live pair")
    };
    assert!(matches!(&query.operation,
        DomainOperation::Resident(residents::ResidentOperation::Query(query))
            if query.handles == ["resident-medic", "resident-patient"]));
    let loaded = |handle: &str, uuid: &str, x: f64, health: f32| residents::ResidentSnapshot {
        handle: handle.to_owned(),
        entity_uuid: uuid.to_owned(),
        lifecycle: residents::ResidentLifecycle::AliveLoaded,
        revision: 4,
        generation_id: None,
        pois: residents::ResidentPois {
            home: None,
            work: None,
            meeting: None,
        },
        loaded: Some(residents::ResidentLoadedState {
            x,
            y: 64.0,
            z: 8.0,
            health,
            carried: Vec::new(),
        }),
    };
    let reserve = plugin.answered(&OperationAnswered {
        request: query.request.clone(),
        operation_id: None,
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 4,
            payload: OperationPayload::Resident(ResidentResult::Page(residents::ResidentPage {
                residents: vec![
                    loaded("resident-medic", "medic-uuid", 1.0, 20.0),
                    loaded("resident-patient", "patient-uuid", 2.0, 7.0),
                ],
                cursor: None,
            })),
        }),
    });
    let [Command::StorageCas(counter)] = reserve.as_slice() else {
        panic!("reserve native operation id")
    };
    let issued = plugin.storage_cas(&StorageCasAnswered {
        request: counter.request.clone(),
        outcome: StorageCasOutcome::Committed(1),
    });
    let [Command::Operation(request)] = issued.as_slice() else {
        panic!("one native pair order")
    };
    assert!(matches!(&request.operation,
        DomainOperation::ResidentOrder(residents::ResidentOrderOperation::IssueOrder(order))
            if order.handles == ["resident-medic", "resident-patient"]
                && order.expected_order_revisions == [3, 5]
                && matches!(order.order, residents::Order::Move(_))));
    let outcome = |medic_state, patient_state| {
        OperationOutcome::Committed(OperationCommitted {
            revision: 9,
            payload: OperationPayload::ResidentOrder(ResidentOrderResult::Order(
                residents::OrderResult {
                    order_revision: 9,
                    members: [
                        ("resident-medic", medic_state),
                        ("resident-patient", patient_state),
                    ]
                    .into_iter()
                    .map(|(handle, state)| residents::OrderMemberOutcome {
                        handle: handle.to_owned(),
                        state,
                        formation_slot: None,
                        targets: Vec::new(),
                    })
                    .collect(),
                    combat: Vec::new(),
                },
            )),
        })
    };
    let partial = plugin.answered(&OperationAnswered {
        request: request.request.clone(),
        operation_id: Some("settlement-operation-1".to_owned()),
        outcome: outcome(
            residents::OrderMemberState::Applied,
            residents::OrderMemberState::BlockedRoute,
        ),
    });
    assert!(matches!(partial.as_slice(), [Command::SendMessage(message)]
        if message.text.contains("Evacuation partial/blocked: 1/2 applied")
            && message.text.contains("resident-patient")));
    // A separate, fully applied native receipt is the only positive admission.
    let admitted = plugin.squad_answer(
        7,
        vec!["resident-medic".to_owned(), "resident-patient".to_owned()],
        combat::SquadAction::Evacuate(residents::BlockPosition { x: 10, y: 64, z: 8 }),
        &outcome(
            residents::OrderMemberState::Applied,
            residents::OrderMemberState::Applied,
        ),
    );
    assert!(
        matches!(admitted.as_slice(), [Command::SendMessage(message)]
        if message.text.contains("Evacuation admitted: both medic and patient moving"))
    );
}
