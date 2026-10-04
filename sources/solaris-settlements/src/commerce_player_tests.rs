use super::caravan::{self, ActorRef, Cell, Container, Stack, StorageGuard};
use super::commerce_player::*;
use super::payment_mandate::Blob;
use solaris_plugin_sdk::{events, inventories, npc_routes, npcs, types};

fn container(x: i32) -> Container {
    Container {
        scope: "home".into(),
        building: format!("stock{x}"),
        live_ref: format!("native{x}"),
        dimension: "minecraft:overworld".into(),
        at: Cell { x, y: 64, z: 1 },
    }
}
fn item(resource: &str, count: u32) -> inventories::InventoryItem {
    inventories::InventoryItem {
        resource_id: resource.into(),
        count,
        damage: None,
        enchantments: Vec::new(),
        custom_name: None,
        item_model: None,
        max_damage: None,
        custom_data: None,
        stew_effects: Vec::new(),
    }
}
fn snapshot(
    endpoint: inventories::InventoryEndpoint,
    slots: Vec<(u8, Option<inventories::InventoryItem>)>,
    revision: u64,
) -> inventories::OwnedInventorySnapshot {
    inventories::OwnedInventorySnapshot {
        endpoint,
        fence: inventories::InventoryFence {
            revision,
            snapshot_hash: format!("{revision:064x}"),
        },
        slots: slots
            .into_iter()
            .map(|(slot, item)| inventories::InventorySlot { slot, item })
            .collect(),
    }
}
fn terms(stock: &inventories::OwnedInventorySnapshot) -> Terms {
    Terms {
        id: "food".into(),
        owner: "owner".into(),
        stock: container(0),
        need: Need::Supply {
            resource: "minecraft:wheat".into(),
            target: 3,
        },
        reward: 2,
        coins: allocate_coins(stock, 2).unwrap(),
        reserve_operation: "pc_food_reserve".into(),
        stock_fence: stock.fence.clone(),
        guards: vec![StorageGuard {
            key: "registry".into(),
            version: 1,
        }],
    }
}
#[test]
fn current_stock_closes_need_even_when_another_actor_delivered() {
    let stock = snapshot(
        container(0).endpoint(),
        vec![
            (0, Some(item("minecraft:emerald", 2))),
            (1, Some(item("minecraft:wheat", 1))),
        ],
        1,
    );
    let terms = terms(&stock);
    assert_eq!(terms.remaining(&stock), Ok(2));
    let delivered = snapshot(
        container(0).endpoint(),
        vec![
            (0, Some(item("minecraft:emerald", 2))),
            (1, Some(item("minecraft:wheat", 3))),
        ],
        2,
    );
    assert_eq!(terms.remaining(&delivered), Ok(0));
    let foreign = snapshot(
        container(24).endpoint(),
        delivered
            .slots
            .into_iter()
            .map(|s| (s.slot, s.item))
            .collect(),
        2,
    );
    assert!(terms.remaining(&foreign).is_err());
}
#[test]
fn paid_ack_requires_exact_components_and_whole_exchange_afterimages() {
    let mut coin = item("minecraft:emerald", 2);
    coin.custom_name = Some("Allocated wages".into());
    coin.custom_data = Some(vec![0, 255, 17]);
    let stock = snapshot(
        container(0).endpoint(),
        vec![
            (0, Some(coin.clone())),
            (1, Some(item("minecraft:wheat", 1))),
        ],
        1,
    );
    let player = snapshot(
        inventories::InventoryEndpoint::PlayerInventory(7),
        vec![(9, Some(item("minecraft:wheat", 2))), (10, None)],
        1,
    );
    let terms = terms(&stock);
    let intent = Intent {
        operation: inventories::InventoryConsumeReservation {
            operation_id: "pay1".into(),
            actor_id: 7,
            reservation_ref: "hold1".into(),
            execution_permit: None,
            expected_reservation_revision: 1,
            portion: 1,
            resource_plan: terms.plan(),
            transfers: vec![
                inventories::OwnedItemTransfer {
                    source: player.endpoint.clone(),
                    source_slot: 9,
                    destination: stock.endpoint.clone(),
                    destination_slot: 1,
                    count: 2,
                },
                inventories::OwnedItemTransfer {
                    source: stock.endpoint.clone(),
                    source_slot: 0,
                    destination: player.endpoint.clone(),
                    destination_slot: 10,
                    count: 2,
                },
            ],
            expected_revisions: vec![expected(&player), expected(&stock)],
        },
        participant: Participant {
            player: "worker".into(),
            session: 7,
        },
        guards: terms.guards.clone(),
        before: vec![player, stock],
        supplied: 2,
        delivery: None,
    };
    let receipt = inventories::InventoryReservedTransfer {
        reservation: inventories::InventoryReservationSnapshot {
            reservation_ref: "hold1".into(),
            endpoint: container(0).endpoint(),
            resource_plan_hash: "a".repeat(64),
            quantities: vec![inventories::InventoryReservationQuantity {
                resource_id: "minecraft:emerald".into(),
                reserved: 2,
                consumed: 2,
                returned: 0,
                remaining: 0,
            }],
            bound_to: None,
            released: false,
            receipt_watermark: 1,
        },
        inventories: vec![
            snapshot(
                inventories::InventoryEndpoint::PlayerInventory(7),
                vec![(9, None), (10, Some(coin))],
                2,
            ),
            snapshot(
                container(0).endpoint(),
                vec![(0, None), (1, Some(item("minecraft:wheat", 3)))],
                2,
            ),
        ],
    };
    assert_eq!(validate_paid_receipt(&terms, &intent, &receipt), Ok(()));
    let mut substituted = receipt.clone();
    substituted.inventories[0].slots[1]
        .item
        .as_mut()
        .unwrap()
        .custom_data = None;
    assert!(validate_paid_receipt(&terms, &intent, &substituted).is_err());
    let mut missing_goods = receipt.clone();
    missing_goods.inventories[1].slots[1]
        .item
        .as_mut()
        .unwrap()
        .count = 1;
    assert!(validate_paid_receipt(&terms, &intent, &missing_goods).is_err());
    let mut wrong_session = receipt;
    wrong_session.inventories[0].endpoint = inventories::InventoryEndpoint::PlayerInventory(8);
    assert!(validate_paid_receipt(&terms, &intent, &wrong_session).is_err());
}
#[test]
fn private_claim_and_receipt_are_not_exposed_to_another_player() {
    let stock = snapshot(
        container(0).endpoint(),
        vec![(0, Some(item("minecraft:emerald", 2)))],
        1,
    );
    let terms = terms(&stock);
    let blob = Blob::encode(&terms).unwrap().0;
    let mut head = Head {
        id: terms.id.clone(),
        owner: terms.owner.clone(),
        terms: blob.clone(),
        public: Offer {
            id: terms.id.clone(),
            owner: terms.owner.clone(),
            location: terms.stock.clone(),
            need: terms.need.clone(),
            fixed_emerald_reward: 2,
        },
        state: State::Claimed,
        participant: Some(Participant {
            player: "worker".into(),
            session: 7,
        }),
        reservation: None,
        escort: None,
        intent: None,
        receipt: Some(blob),
        release_operation: None,
        blocked: None,
        attempt: 0,
    };
    assert!(head.projection("stranger").is_none());
    assert!(head.projection("worker").unwrap().own_receipt.is_some());
    assert!(head.projection("owner").unwrap().own_receipt.is_none());
    head.state = State::Offered;
    let projection = head.projection("stranger").unwrap();
    assert!(projection.own_claim.is_none() && projection.own_receipt.is_none());
}
fn caravan() -> caravan::Contract {
    caravan::Contract {
        party: caravan::Party {
            id: "real_cargo".into(),
            owner: ActorRef::Player {
                player: "owner".into(),
            },
            leader: ActorRef::Player {
                player: "owner".into(),
            },
            members: Vec::new(),
            executor_epoch: 1,
            parked: false,
        },
        carrier: "native_carrier".into(),
        carrier_uuid: Some("original_uuid".into()),
        source: container(-24),
        destination: container(0),
        item: "minecraft:wheat".into(),
        quantity: 3,
        paid_mandate: None,
        manifest: Some(Stack::from_native(&item("minecraft:wheat", 3))),
        carry_slot: Some(0),
        route: vec![
            Cell {
                x: -24,
                y: 64,
                z: 0,
            },
            Cell { x: 0, y: 64, z: 0 },
        ],
        cursor: 0,
        poll_ticks: 20,
        max_ticks: 1000,
        next_due: 20,
        status: caravan::Status::Transit,
        blocked: None,
        loaded: true,
        attempt: 1,
        intent: None,
        reservation: None,
        route_ref: Some("original_route".into()),
        started: false,
        arrival_ref: None,
        arrival_epoch: None,
        arrival_rights_revision: None,
        handoff_ref: None,
        death_ref: None,
        death_epoch: None,
        death_uuid: None,
        native_ticks: 0,
        geometry_cells: 10,
        geometry_hash: Some("geometry".into()),
        receipts: vec![caravan::Receipt {
            operation: "physical_load".into(),
            effect: caravan::Effect::Load,
            epoch: 1,
            native_revision: Some(10),
            endpoints: Vec::new(),
        }],
    }
}
fn route(
    state: npc_routes::NpcRouteState,
    epoch: u64,
    cursor: u16,
    native_ticks: u64,
) -> npc_routes::NpcRouteStatus {
    npc_routes::NpcRouteStatus {
        route_ref: "original_route".into(),
        handle: "native_carrier".into(),
        entity_uuid: "original_uuid".into(),
        entity_id: Some(1),
        identity_revision: 1,
        order_revision: 1,
        revision: 1,
        executor_epoch: epoch,
        mode: npc_routes::NpcExecutorMode::ObservedPhysical,
        state,
        lifecycle: npcs::NpcLifecycle::AliveLoaded,
        pose: Some(npcs::NpcLoadedState {
            dimension: "minecraft:overworld".into(),
            x: -24.0,
            y: 64.0,
            z: 0.0,
            health: 20.0,
            max_health: 20.0,
            yaw: 0.0,
            carried: Vec::new(),
            native_age_ticks: None,
            birth_origin: None,
        }),
        segment: 0,
        cursor,
        blocked_reason: None,
        due_tick: Some(20),
        native_ticks,
        geometry_cells: 10,
        geometry_hash: "geometry".into(),
        max_ticks: 1000,
        handoff: None,
        arrival: None,
        outcome_tick: None,
        death_cargo: None,
    }
}
#[test]
fn escort_requires_causal_original_start_progress_identity_and_live_session() {
    let stock = snapshot(
        container(0).endpoint(),
        vec![(0, Some(item("minecraft:emerald", 2)))],
        1,
    );
    let mut terms = terms(&stock);
    terms.need = Need::Escort {
        shipment: "real_cargo".into(),
        carrier: "native_carrier".into(),
        source: container(-24),
        item: "minecraft:wheat".into(),
        quantity: 3,
        radius: 4,
        sample_ticks: 20,
    };
    let participant = Participant {
        player: "worker".into(),
        session: 7,
    };
    let player = events::PlayerSnapshot {
        player: "worker".into(),
        session: 7,
        name: "Worker".into(),
        dimension: "minecraft:overworld".into(),
        position: types::Position {
            x: -24.0,
            y: 64.0,
            z: 0.0,
        },
    };
    let mut caravan = caravan();
    let checkpoint = Blob::encode(&participant).unwrap().0;
    let prepared = route(npc_routes::NpcRouteState::Prepared, 1, 0, 0);
    let accepted = observe_escort(
        &terms,
        &participant,
        &caravan,
        &prepared,
        &player,
        100,
        None,
        checkpoint.clone(),
    )
    .unwrap();
    caravan.started = true;
    caravan.receipts.push(caravan::Receipt {
        operation: "original_start".into(),
        effect: caravan::Effect::Start,
        epoch: 2,
        native_revision: Some(11),
        endpoints: Vec::new(),
    });
    let running = route(npc_routes::NpcRouteState::Running, 2, 1, 20);
    let progressed = observe_escort(
        &terms,
        &participant,
        &caravan,
        &running,
        &player,
        120,
        Some(&accepted),
        checkpoint.clone(),
    )
    .unwrap();
    assert_eq!(progressed.progress_samples, 1);
    let mut foreign = running.clone();
    foreign.entity_uuid = "substitute_uuid".into();
    assert!(observe_escort(
        &terms,
        &participant,
        &caravan,
        &foreign,
        &player,
        140,
        Some(&progressed),
        checkpoint.clone()
    )
    .is_err());
    let mut rebound = player.clone();
    rebound.session = 8;
    assert!(observe_escort(
        &terms,
        &participant,
        &caravan,
        &running,
        &rebound,
        140,
        Some(&progressed),
        checkpoint.clone()
    )
    .is_err());
    let mut changed_epoch = running.clone();
    changed_epoch.executor_epoch = 3;
    assert!(observe_escort(
        &terms,
        &participant,
        &caravan,
        &changed_epoch,
        &player,
        140,
        Some(&progressed),
        checkpoint.clone()
    )
    .is_err());
    assert!(observe_escort(
        &terms,
        &participant,
        &caravan,
        &running,
        &player,
        161,
        Some(&progressed),
        checkpoint
    )
    .is_err());
}
