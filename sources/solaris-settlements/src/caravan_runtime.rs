//! Native finite routes plus physical reserved cargo; every effect has a durable intent.
use super::caravan::{
    plan_deposits, ActorRef, Cell, Container, Contract, Deposit, Effect, EndpointReceipt,
    ExecutionMode, Intent, Party, Receipt, Reservation, Stack, Status, StorageGuard, Store,
    Transfer,
};
use super::*;
use solaris_plugin_sdk::{
    admit_npc_route, cancel_npc_route, check_structure, check_worksite,
    consume_inventory_reservation, handoff_npc_route, inventories, npc_routes,
    query_inventory_placement_room, query_npc_controls, query_npc_route, query_npcs,
    release_inventory_reservation, schedule_timer, start_npc_route, transfer_owned_items,
    with_expected_plugin_storage_versions,
};
pub(super) const TIMER: &str = "settlement-caravans";
pub(super) const KEY: &str = "settlement-caravans-v1";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
    now: u64,
}
#[derive(Clone)]
enum Action {
    Create {
        id: String,
        source: String,
        source_building: String,
        destination: String,
        destination_building: String,
        resident: String,
        item: String,
        quantity: u32,
        poll: u64,
        max_ticks: u64,
        route: Vec<Cell>,
        player: String,
        paid: bool,
    },
    Status {
        id: String,
        player: String,
    },
    Carrier {
        scope: String,
        resident: String,
        player: String,
    },
    Handoff {
        id: String,
        epoch: u64,
        mode: ExecutionMode,
        player: String,
    },
    Wake,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Store,
    Pose,
    Carrier,
    Controls,
    Death,
    Route,
    Live,
    Source,
    Destination,
    Placement,
    PaidEscrow,
    PaidFinish,
    Evidence,
    PlayerStatus,
    PlayerIndex,
    PlayerHead,
    IntentWrite,
    Native,
    StateWrite,
}
struct Pending {
    request: String,
    session: Option<u64>,
    action: Action,
    stage: Stage,
    registry: SettlementRegistry,
    registry_version: Option<u64>,
    store: Store,
    version: Option<u64>,
    id: Option<String>,
    source: Option<inventories::OwnedInventorySnapshot>,
    destination: Option<inventories::OwnedInventorySnapshot>,
    source_slot: Option<u8>,
    deposits: Option<Vec<Deposit>>,
    identity_revision: Option<u64>,
    evidence: Option<String>,
    native_uuid: Option<String>,
    player_ids: Vec<String>,
    player_cursor: usize,
    player_guards: Vec<StorageGuard>,
    player_gate_done: bool,
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} caravan {text}"));
    session
        .map(|s| vec![message_session(s, text)])
        .unwrap_or_default()
}
pub(super) fn binding(
    registry: &SettlementRegistry,
    scope: &str,
    building: &str,
) -> Option<Container> {
    let b = registry.settlements.get(scope)?.buildings.get(building)?;
    let at = b.storage_positions.first()?;
    let live_ref = match &b.origin {
        BuildingOrigin::Certificate { certificate_id } => certificate_id.clone(),
        BuildingOrigin::Structure { structure_id } => structure_id.clone(),
    };
    Some(Container {
        scope: scope.into(),
        building: building.into(),
        live_ref,
        dimension: b.dimension.clone()?,
        at: Cell {
            x: at.x,
            y: at.y,
            z: at.z,
        },
    })
}
pub(super) fn authorized(registry: &SettlementRegistry, target: &Container, owner: &str) -> bool {
    registry
        .settlements
        .get(&target.scope)
        .is_some_and(|s| can_build_settlement(s, owner))
        && binding(registry, &target.scope, &target.building).as_ref() == Some(target)
}
fn parse_route(text: &str) -> Option<Vec<Cell>> {
    text.split(';')
        .map(|p| {
            let mut fields = p.split(',');
            let cell = Cell {
                x: fields.next()?.parse().ok()?,
                y: fields.next()?.parse().ok()?,
                z: fields.next()?.parse().ok()?,
            };
            fields.next().is_none().then_some(cell)
        })
        .collect()
}
fn endpoint_receipt(endpoint: &InventoryEndpoint, fence: &InventoryFence) -> EndpointReceipt {
    let endpoint = container_identity(endpoint).unwrap_or_else(|| match endpoint {
        InventoryEndpoint::NpcCarry(h) => format!("npc_carry:{h}"),
        _ => "unexpected".into(),
    });
    EndpointReceipt {
        endpoint,
        fence: hire::Fence {
            revision: fence.revision,
            snapshot_hash: fence.snapshot_hash.clone(),
        },
    }
}
fn death_evidence(death: &inventories::NpcDeathCargoReceipt) -> serde_json::Value {
    let loot: Vec<_> = death
        .loot
        .iter()
        .map(|l| {
            serde_json::json!({
                "inventory": format!("{:?}", l.inventory), "slot": l.slot,
                "uuid": l.entity_uuid, "count": l.item.count, "stack": Stack::from_native(&l.item)
            })
        })
        .collect();
    serde_json::json!({
        "receipt": death.receipt_ref, "carrier": death.handle, "uuid": death.entity_uuid,
        "dimension": death.dimension,
        "position": {"x": death.position.x, "y": death.position.y, "z": death.position.z},
        "death_tick": death.death_tick, "inventory_revision": death.source_inventory_revision,
        "epoch": death.executor_epoch, "world_decision": death.world_decision_id.to_string(),
        "loot": loot
    })
}
fn native_evidence(c: &Contract, s: &npc_routes::NpcRouteStatus) -> String {
    let pose = s
        .pose
        .as_ref()
        .map(|p| serde_json::json!({"dimension":p.dimension,"x":p.x,"y":p.y,"z":p.z}));
    let arrival=s.arrival.as_ref().map(|a|serde_json::json!({"receipt":a.receipt_ref,"epoch":a.executor_epoch,"tick":a.simulation_tick,"dimension":a.dimension,"inventory_revision":a.inventory_revision,"inventory_hash":a.inventory_image_hash}));
    let handoff=s.handoff.as_ref().map(|h|serde_json::json!({"snapshot":h.snapshot_ref,"source_epoch":h.source_epoch,"target_epoch":h.target_epoch,"tick":h.simulation_tick,"dimension":h.dimension,"inventory_revision":h.inventory_revision,"inventory_hash":h.inventory_image_hash,"geometry_hash":h.geometry_hash,"reservation_refs":h.reservation_refs}));
    format!(
        "Caravan {}: status={:?} epoch={} parked={} loaded={} reason={} native={}",
        c.party.id,
        c.status,
        c.party.executor_epoch,
        c.party.parked,
        c.loaded,
        c.blocked.as_deref().unwrap_or("none"),
        serde_json::json!({"route":s.route_ref,"carrier":s.handle,"uuid":s.entity_uuid,"entity_id":s.entity_id,"epoch":s.executor_epoch,"state":format!("{:?}",s.state),"mode":format!("{:?}",s.mode),"lifecycle":format!("{:?}",s.lifecycle),"pose":pose,"cursor":s.cursor,"segment":s.segment,"blocked":s.blocked_reason.map(|r|format!("{r:?}")),"native_ticks":s.native_ticks,"max_ticks":s.max_ticks,"geometry_cells":s.geometry_cells,"geometry_hash":s.geometry_hash,"arrival":arrival,"handoff":handoff,"death_cargo":s.death_cargo.as_ref().map(death_evidence),"death_ref":c.death_ref,"outcome_tick":s.outcome_tick,"receipts":c.receipts,"reservation_ref":c.reservation.as_ref().map(|r|&r.reference)})
    )
}
impl Settlements {
    pub(super) fn handle_caravan_command(
        &mut self,
        invoked: &CommandInvoked,
        tick: u64,
    ) -> Option<Vec<Command>> {
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if invoked.name != "settlement"
            || !args.first().is_some_and(|a| {
                matches!(
                    *a,
                    "caravan"
                        | "paid-caravan"
                        | "caravan-status"
                        | "caravan-park"
                        | "caravan-resume"
                        | "caravan-remote"
                        | "caravan-carrier"
                )
            })
        {
            return None;
        }
        self.caravan_runtime.now = tick;
        if self.caravan_runtime.pending.is_some() {
            return Some(report(
                Some(invoked.session),
                "Caravan busy; no second executor admitted.",
            ));
        }
        let action=match args.as_slice(){
            [kind,id,source,sb,destination,db,resident,item,quantity,poll,max_ticks,route] if matches!(*kind,"caravan"|"paid-caravan")=>{
                if id.is_empty()||id.len()>12||!id.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_')||!valid_resource_id(item){return Some(report(Some(invoked.session),"Stable lowercase caravan id (1..12 bytes) and resource id required."));}
                let Some(quantity)=quantity.parse::<u32>().ok().filter(|q|(1..=4096).contains(q))else{return Some(report(Some(invoked.session),"Quantity must fit native 4096-item budget."));};
                let Some(poll)=poll.parse::<u64>().ok().filter(|p|*p>0&&tick.checked_add(*p).is_some_and(|t|t<=MAX_SAFE_REVISION))else{return Some(report(Some(invoked.session),"Explicit positive simulation poll ticks required."));};
                let Some(max_ticks)=max_ticks.parse::<u64>().ok().filter(|v|(1..=72000).contains(v))else{return Some(report(Some(invoked.session),"Finite native route tick budget must be 1..72000."));};
                let Some(route)=parse_route(route)else{return Some(report(Some(invoked.session),"Physical route must be x,y,z;x,y,z."));};
                Action::Create{id:(*id).into(),source:(*source).into(),source_building:(*sb).into(),destination:(*destination).into(),destination_building:(*db).into(),resident:(*resident).into(),item:(*item).into(),quantity,poll,max_ticks,route,player:invoked.player.clone(),paid:*kind=="paid-caravan"}
            }
            ["caravan-status",id]=>Action::Status{id:(*id).into(),player:invoked.player.clone()},
            ["caravan-carrier",scope,resident]=>Action::Carrier{scope:(*scope).into(),resident:(*resident).into(),player:invoked.player.clone()},
            [kind,id,epoch]if matches!(*kind,"caravan-park"|"caravan-resume"|"caravan-remote")=>{
                let Some(epoch)=epoch.parse::<u64>().ok().filter(|e|*e>0&&*e<=MAX_SAFE_REVISION)else{return Some(report(Some(invoked.session),"Exact native executor epoch required."));};
                let mode=match *kind{"caravan-park"=>ExecutionMode::Parked,"caravan-resume"=>ExecutionMode::ObservedPhysical,_=>ExecutionMode::RemotePhysical};
                Action::Handoff{id:(*id).into(),epoch,mode,player:invoked.player.clone()}
            }
            _=>return Some(report(Some(invoked.session),"Usage: caravan <id> <source> <source-building> <destination> <destination-building> <resident> <item> <quantity> <poll-simulation-ticks> <max-native-ticks> <x,y,z;x,y,z>; caravan-status <id>; caravan-park/resume/remote <id> <native-epoch>")),
        };
        Some(self.caravan_begin(action, Some(invoked.session)))
    }
    pub(super) fn caravan_init(&mut self) -> Vec<Command> {
        self.caravan_runtime.pending = None;
        // Init has no simulation tick; the first native timer supplies the saved clock.
        vec![schedule_timer(TIMER, 1)]
    }
    pub(super) fn caravan_timer(&mut self, tick: u64) -> Vec<Command> {
        self.caravan_runtime.now = tick;
        if self.caravan_runtime.pending.is_some() {
            return vec![schedule_timer(TIMER, 1)];
        }
        self.caravan_begin(Action::Wake, None)
    }
    fn caravan_begin(&mut self, action: Action, session: Option<u64>) -> Vec<Command> {
        let Some(request) = self.next_storage_request("caravan-registry") else {
            return report(session, "Request sequence exhausted.");
        };
        self.caravan_runtime.pending = Some(Pending {
            request: request.clone(),
            session,
            action,
            stage: Stage::Registry,
            registry: SettlementRegistry::default(),
            registry_version: None,
            store: Store::default(),
            version: None,
            id: None,
            source: None,
            destination: None,
            source_slot: None,
            deposits: None,
            identity_revision: None,
            evidence: None,
            native_uuid: None,
            player_ids: Vec::new(),
            player_cursor: 0,
            player_guards: Vec::new(),
            player_gate_done: false,
        });
        self.registry_read(&request)
    }
    fn caravan_command(
        &mut self,
        mut p: Pending,
        stage: Stage,
        make: impl FnOnce(&str) -> Command,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("caravan-step") else {
            return report(
                p.session,
                "Request sequence exhausted; durable intent retained.",
            );
        };
        let command = make(&request);
        p.request = request;
        p.stage = stage;
        self.caravan_runtime.pending = Some(p);
        vec![command]
    }
    fn caravan_write(&mut self, p: Pending, stage: Stage) -> Vec<Command> {
        let Ok(value) = serde_json::to_string(&p.store) else {
            return report(
                p.session,
                "Caravan serialization refused; no native effect.",
            );
        };
        let version = p.version;
        self.caravan_command(p, stage, |r| storage_cas(r, KEY, version, value))
    }
    fn caravan_custody_guards(&self, p: &Pending) -> Result<Vec<StorageGuard>, &'static str> {
        let c = &p.store.contracts[p.id.as_ref().expect("contract")];
        let scopes = [c.source.scope.as_str(), c.destination.scope.as_str()];
        let mut guards = Vec::with_capacity(2);
        for scope in scopes {
            let (key, version) = self.registry_scope_fence(p.registry_version, scope)?;
            let version = version
                .filter(|v| *v > 0)
                .ok_or("cargo current ownership version unavailable")?;
            guards.push(StorageGuard { key, version });
        }
        guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        if guards
            .windows(2)
            .any(|g| g[0].key == g[1].key && g[0].version != g[1].version)
        {
            return Err("cargo current ownership versions conflict");
        }
        guards.dedup_by(|a, b| a.key == b.key);
        Ok(guards)
    }
    fn caravan_paid_guards(
        &self,
        p: &Pending,
        mandate: &super::payment_mandate::Mandate,
        head_version: Option<u64>,
    ) -> Result<Vec<StorageGuard>, &'static str> {
        let head_version = head_version
            .filter(|v| *v > 0)
            .ok_or("paid head actual native version unavailable")?;
        let c = &p.store.contracts[p.id.as_ref().expect("contract")];
        let mut scopes = [
            mandate.buyer_scope.as_str(),
            c.source.scope.as_str(),
            c.destination.scope.as_str(),
            mandate.recipient_scope.as_str(),
        ];
        scopes.sort_unstable();
        let distinct = 1 + scopes.windows(2).filter(|s| s[0] != s[1]).count();
        let mut guards = Vec::with_capacity(distinct + 1);
        guards.push(StorageGuard {
            key: super::payment_mandate::key(c.paid_mandate.as_deref().expect("paid mandate")),
            version: head_version,
        });
        let mut previous = None;
        for scope in scopes {
            if previous == Some(scope) {
                continue;
            }
            previous = Some(scope);
            let (key, version) = self.registry_scope_fence(p.registry_version, scope)?;
            let version = version
                .filter(|v| *v > 0)
                .ok_or("paid scope actual native version unavailable")?;
            guards.push(StorageGuard { key, version });
        }
        guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        if guards
            .windows(2)
            .any(|g| g[0].key == g[1].key && g[0].version != g[1].version)
        {
            return Err("paid scope native version conflict");
        }
        guards.dedup_by(|a, b| a.key == b.key);
        Ok(guards)
    }
    fn caravan_prepare_cargo(
        &mut self,
        mut p: Pending,
        deposits: Vec<Deposit>,
        storage_guards: Vec<StorageGuard>,
    ) -> Vec<Command> {
        let id = p.id.as_ref().expect("contract");
        let source = p.source.as_ref().expect("source query");
        let destination = p.destination.as_ref().expect("destination query");
        let source_slot = p.source_slot.expect("actual source slot");
        let c = p.store.contracts.get_mut(id).expect("contract");
        if c.reservation.is_none() && !c.loaded {
            let Some(operation) = c.operation(Effect::Reserve) else {
                return self.caravan_block(p, "operation sequence exhausted");
            };
            c.intent = Some(Intent::Reserve {
                operation,
                fence: hire::Fence::from_snapshot(source),
                storage_guards,
            });
            return self.caravan_write(p, Stage::IntentWrite);
        }
        let effect = if c.loaded {
            Effect::Unload
        } else {
            Effect::Load
        };
        let Some(operation) = c.operation(effect) else {
            return self.caravan_block(p, "operation sequence exhausted");
        };
        if !c.loaded {
            c.carry_slot = Some(deposits[0].slot);
        } else {
            c.arrival_rights_revision = p.registry_version;
        }
        let transfer = Transfer {
            operation,
            epoch: c.party.executor_epoch,
            source_slot,
            deposits,
            source_fence: hire::Fence::from_snapshot(source),
            destination_fence: hire::Fence::from_snapshot(destination),
            storage_guards,
        };
        if c.loaded {
            c.intent = Some(Intent::Unload { transfer });
            c.status = Status::Unloading;
        } else {
            c.intent = Some(Intent::Load { transfer });
            c.status = Status::Loading;
        }
        c.blocked = None;
        self.caravan_write(p, Stage::IntentWrite)
    }
    fn caravan_block(&mut self, mut p: Pending, reason: impl Into<String>) -> Vec<Command> {
        let reason = reason.into();
        let c = p
            .store
            .contracts
            .get_mut(p.id.as_ref().expect("selected contract"))
            .expect("contract");
        c.block(reason.clone());
        let mut commands = report(
            p.session,
            format!(
                "Caravan {} blocked: {reason}; cargo remains native.",
                c.party.id
            ),
        );
        commands.extend(self.caravan_write(p, Stage::StateWrite));
        commands
    }
    fn caravan_pose(&mut self, p: Pending) -> Vec<Command> {
        let carrier = p.store.contracts[p.id.as_ref().expect("contract")]
            .carrier
            .clone();
        self.caravan_command(p, Stage::Pose, |r| query_npcs(r, &[carrier], None))
    }
    fn caravan_route(&mut self, p: Pending) -> Vec<Command> {
        let route = p.store.contracts[p.id.as_ref().expect("contract")]
            .route_ref
            .clone()
            .expect("native route");
        self.caravan_command(p, Stage::Route, |r| query_npc_route(r, &route))
    }
    fn caravan_inventory(
        &mut self,
        p: Pending,
        stage: Stage,
        endpoint: InventoryEndpoint,
    ) -> Vec<Command> {
        self.caravan_command(p, stage, |r| query_owned_inventory(r, endpoint, None))
    }
    fn caravan_live(&mut self, p: Pending) -> Vec<Command> {
        let c = &p.store.contracts[p.id.as_ref().expect("contract")];
        let target = if c.loaded { &c.destination } else { &c.source };
        if !authorized(&p.registry, target, c.owner()) {
            return self.caravan_block(
                p,
                "current source/recipient rights or physical binding identity changed",
            );
        }
        let b = &p.registry.settlements[&target.scope].buildings[&target.building];
        match &b.origin {
            BuildingOrigin::Certificate { .. } => {
                let id = b
                    .worksite_id
                    .clone()
                    .unwrap_or_else(|| target.live_ref.clone());
                self.caravan_command(p, Stage::Live, |r| check_worksite(r, &id))
            }
            BuildingOrigin::Structure { structure_id } => {
                let id = structure_id.clone();
                self.caravan_command(p, Stage::Live, |r| check_structure(r, &id))
            }
        }
    }
    fn caravan_player_wait(&mut self, p: Pending, reason: &str) -> Vec<Command> {
        let poll = p.store.contracts[p.id.as_ref().expect("contract")].poll_ticks;
        let mut commands = report(p.session, reason);
        commands.push(schedule_timer(TIMER, poll));
        commands
    }
    fn caravan_player_gate_next(&mut self, mut p: Pending) -> Vec<Command> {
        if let Some(id) = p.player_ids.get(p.player_cursor) {
            let key = super::commerce_player::key(id);
            return self.caravan_command(p, Stage::PlayerHead, |r| storage_get(r, &key));
        }
        p.player_guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        p.player_gate_done = true;
        self.caravan_native(p)
    }
    fn caravan_native(&mut self, p: Pending) -> Vec<Command> {
        let c = &p.store.contracts[p.id.as_ref().expect("contract")];
        let intent = c.intent.as_ref().expect("durable intent");
        if !p.player_gate_done
            && (matches!(intent, Intent::Start { .. })
                || (matches!(intent, Intent::Cancel { .. }) && c.gross_delivery().is_some()))
        {
            let operation = intent.operation().to_owned();
            return self.caravan_command(p, Stage::PlayerStatus, |request| {
                Command::OperationStatus(solaris_plugin_sdk::commands::OperationStatus {
                    request: request.to_owned(),
                    operation_id: operation,
                })
            });
        }
        let operation = intent.operation().to_owned();
        let route = c.route_ref.clone();
        let epoch = c.party.executor_epoch;
        let custody_guards = match intent {
            Intent::Reserve { storage_guards, .. } => Some(storage_guards.as_slice()),
            Intent::Load { transfer } | Intent::Unload { transfer } => {
                Some(transfer.storage_guards.as_slice())
            }
            _ => None,
        };
        if custody_guards.is_some_and(|guards| {
            guards.is_empty() || guards.iter().any(|guard| guard.version == 0)
        }) {
            return self.caravan_block(
                p,
                "cargo custody original native guards absent; no physical effect",
            );
        }
        match intent {
            Intent::Admit {
                identity_revision,
                order_revision,
                ..
            } => {
                let admission = npc_routes::NpcRouteAdmission {
                    handle: c.carrier.clone(),
                    identity_revision: *identity_revision,
                    order_revision: *order_revision,
                    dimension: c.source.dimension.clone(),
                    waypoints: c.route.iter().map(Cell::wire).collect(),
                    max_ticks: c.max_ticks,
                    reservation_refs: c
                        .reservation
                        .as_ref()
                        .map(|r| vec![r.reference.clone()])
                        .unwrap_or_default(),
                };
                self.caravan_command(p, Stage::Native, |r| {
                    admit_npc_route(r, &operation, admission)
                })
            }
            Intent::Reserve {
                fence,
                storage_guards,
                ..
            } => {
                let endpoint = c.source.endpoint();
                let plan = c.plan();
                let fence = fence.wire();
                let guards = storage_guards.iter().map(StorageGuard::wire).collect();
                self.caravan_command(p, Stage::Native, |r| {
                    with_expected_plugin_storage_versions(
                        reserve_inventory_items(r, &operation, endpoint, plan, fence),
                        guards,
                    )
                })
            }
            Intent::Load { transfer } | Intent::Unload { transfer } => {
                if transfer.epoch != epoch {
                    return self.caravan_block(
                        p,
                        "stale cargo executor epoch; exact native intent retained",
                    );
                }
                let load = matches!(intent, Intent::Load { .. });
                let (source, destination) = if load {
                    (
                        c.source.endpoint(),
                        InventoryEndpoint::NpcCarry(c.carrier.clone()),
                    )
                } else {
                    (
                        InventoryEndpoint::NpcCarry(c.carrier.clone()),
                        c.destination.endpoint(),
                    )
                };
                if transfer.deposits.is_empty()
                    || transfer.deposits.len() > 16
                    || (load && transfer.deposits.len() != 1)
                    || transfer.deposits.iter().any(|d| d.count == 0)
                    || transfer
                        .deposits
                        .iter()
                        .map(|d| u64::from(d.count))
                        .sum::<u64>()
                        != u64::from(c.quantity)
                {
                    return self.caravan_block(
                        p,
                        "invalid exact cargo portions; original intent retained",
                    );
                }
                let transfers = transfer
                    .deposits
                    .iter()
                    .map(|deposit| inventories::OwnedItemTransfer {
                        source: source.clone(),
                        source_slot: transfer.source_slot,
                        destination: destination.clone(),
                        destination_slot: deposit.slot,
                        count: deposit.count,
                    })
                    .collect();
                let fences = vec![
                    inventories::InventoryExpectedRevision {
                        endpoint: source,
                        fence: transfer.source_fence.wire(),
                    },
                    inventories::InventoryExpectedRevision {
                        endpoint: destination,
                        fence: transfer.destination_fence.wire(),
                    },
                ];
                let permit = Some(solaris_plugin_sdk::types::NpcExecutionPermit {
                    route_ref: route.expect("admitted route"),
                    executor_epoch: epoch,
                });
                let guards = transfer
                    .storage_guards
                    .iter()
                    .map(StorageGuard::wire)
                    .collect();
                if load {
                    let reservation = c.reservation.as_ref().expect("physical reserve");
                    let reference = reservation.reference.clone();
                    let revision = reservation.revision;
                    let plan = c.plan();
                    self.caravan_command(p, Stage::Native, |r| {
                        with_expected_plugin_storage_versions(
                            consume_inventory_reservation(
                                r, &operation, 0, permit, &reference, revision, 1, plan, transfers,
                                fences,
                            ),
                            guards,
                        )
                    })
                } else {
                    self.caravan_command(p, Stage::Native, |r| {
                        with_expected_plugin_storage_versions(
                            transfer_owned_items(r, &operation, 0, permit, transfers, fences),
                            guards,
                        )
                    })
                }
            }
            Intent::Start {
                epoch,
                cargo_operation,
                ..
            } => {
                let route = route.expect("route");
                let epoch = *epoch;
                let cargo = cargo_operation.clone();
                let guards = p.player_guards.iter().map(StorageGuard::wire).collect();
                self.caravan_command(p, Stage::Native, |r| {
                    with_expected_plugin_storage_versions(
                        start_npc_route(r, &operation, &route, epoch, Some(&cargo)),
                        guards,
                    )
                })
            }
            Intent::Handoff { epoch, mode, .. } => {
                let route = route.expect("route");
                let epoch = *epoch;
                let mode = match mode {
                    ExecutionMode::ObservedPhysical => {
                        npc_routes::NpcExecutorMode::ObservedPhysical
                    }
                    ExecutionMode::RemotePhysical => npc_routes::NpcExecutorMode::RemotePhysical,
                    ExecutionMode::Parked => npc_routes::NpcExecutorMode::Parked,
                };
                self.caravan_command(p, Stage::Native, |r| {
                    handoff_npc_route(r, &operation, &route, epoch, mode)
                })
            }
            Intent::Cancel { epoch, .. } => {
                let route = route.expect("route");
                let epoch = *epoch;
                let guards = p.player_guards.iter().map(StorageGuard::wire).collect();
                self.caravan_command(p, Stage::Native, |r| {
                    with_expected_plugin_storage_versions(
                        cancel_npc_route(r, &operation, &route, epoch),
                        guards,
                    )
                })
            }
            Intent::Release { revision, .. } => {
                let reference = c
                    .reservation
                    .as_ref()
                    .expect("reservation")
                    .reference
                    .clone();
                let revision = *revision;
                self.caravan_command(p, Stage::Native, |r| {
                    release_inventory_reservation(r, &operation, &reference, revision)
                })
            }
        }
    }
    pub(super) fn caravan_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.caravan_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::Registry
                    | Stage::Store
                    | Stage::PaidEscrow
                    | Stage::PaidFinish
                    | Stage::PlayerIndex
                    | Stage::PlayerHead
            )
        {
            self.caravan_runtime.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Caravan storage unavailable; no native effect.",
            ));
        };
        if p.stage == Stage::PlayerIndex {
            let Some(version) = record.version.filter(|v| *v > 0) else {
                return Some(self.caravan_player_wait(
                    p,
                    "Player contract index initialization pending; no start/cancel effect.",
                ));
            };
            let Some(index) = record
                .value
                .and_then(|v| serde_json::from_str::<super::commerce_player::Index>(v).ok())
            else {
                return Some(self.caravan_player_wait(
                    p,
                    "Player contract index unreadable; no start/cancel effect.",
                ));
            };
            if index.ids.len() > super::commerce_player::MAX_OFFERS
                || index.ids.windows(2).any(|ids| ids[0] >= ids[1])
            {
                return Some(self.caravan_player_wait(
                    p,
                    "Player contract index invalid; no start/cancel effect.",
                ));
            }
            let Some(caravan_version) = p.version.filter(|v| *v > 0) else {
                return Some(self.caravan_player_wait(p, "Original caravan fence unavailable."));
            };
            let Some(registry_version) = p.registry_version.filter(|v| *v > 0) else {
                return Some(self.caravan_player_wait(p, "Original registry fence unavailable."));
            };
            p.player_guards = vec![
                StorageGuard {
                    key: registry_storage::ROOT_KEY.to_owned(),
                    version: registry_version,
                },
                StorageGuard {
                    key: KEY.to_owned(),
                    version: caravan_version,
                },
                StorageGuard {
                    key: super::commerce_player::INDEX_KEY.to_owned(),
                    version,
                },
            ];
            p.player_ids = index.ids;
            return Some(self.caravan_player_gate_next(p));
        }
        if p.stage == Stage::PlayerHead {
            let id = &p.player_ids[p.player_cursor];
            let Some(head) = record
                .value
                .and_then(|v| serde_json::from_str::<super::commerce_player::Head>(v).ok())
            else {
                return Some(self.caravan_player_wait(
                    p,
                    "Indexed original player contract unavailable; no start/cancel effect.",
                ));
            };
            if head.id != *id {
                return Some(
                    self.caravan_player_wait(p, "Indexed player contract identity mismatch."),
                );
            }
            let c = &p.store.contracts[p.id.as_ref().expect("contract")];
            if matches!(&head.public.need, super::commerce_player::Need::Escort { shipment, .. } if shipment == &c.party.id)
            {
                let Some(version) = record.version.filter(|v| *v > 0) else {
                    return Some(
                        self.caravan_player_wait(p, "Original escort header fence unavailable."),
                    );
                };
                p.player_guards.push(StorageGuard {
                    key: super::commerce_player::key(id),
                    version,
                });
                let waiting = if matches!(c.intent, Some(Intent::Start { .. })) {
                    matches!(
                        head.state,
                        super::commerce_player::State::ReserveIntent
                            | super::commerce_player::State::Offered
                    )
                } else {
                    matches!(
                        head.state,
                        super::commerce_player::State::Claimed
                            | super::commerce_player::State::PayoutIntent
                            | super::commerce_player::State::ReleaseIntent
                    )
                };
                if waiting {
                    return Some(self.caravan_player_wait(p, "Original player escort admission/payment pending; original native route retained."));
                }
            }
            p.player_cursor += 1;
            return Some(self.caravan_player_gate_next(p));
        }
        if p.stage == Stage::PaidFinish {
            let c = p
                .store
                .contracts
                .get_mut(p.id.as_ref().expect("contract"))
                .expect("contract");
            let paid = record
                .value
                .as_deref()
                .and_then(|v| serde_json::from_str::<super::payment_mandate::Mandate>(v).ok())
                .is_some_and(|m| {
                    m.shipment == c.party.id
                        && m.carrier == c.carrier
                        && m.state == super::payment_mandate::State::Paid
                        && m.payment_receipt.is_some()
                });
            if !paid {
                return Some(self.caravan_block(p, "actual delivery committed; original escrow payment pending, native route principal retained"));
            }
            let Some(operation) = c.operation(Effect::Cancel) else {
                return Some(self.caravan_block(p, "operation sequence exhausted"));
            };
            c.intent = Some(Intent::Cancel {
                operation,
                epoch: c.party.executor_epoch,
            });
            return Some(self.caravan_write(p, Stage::IntentWrite));
        }
        if p.stage == Stage::PaidEscrow {
            let mandate = record
                .value
                .as_deref()
                .and_then(|v| serde_json::from_str::<super::payment_mandate::Mandate>(v).ok());
            let c = &p.store.contracts[p.id.as_ref().expect("contract")];
            if !mandate.as_ref().is_some_and(|m| {
                Some(m.registry_revision) == p.registry_version && m.escrow_ready_for(c)
            }) {
                return Some(self.caravan_block(p, "paid mandate exact escrow/components/rights revision not ready; no cargo reserve or load"));
            }
            let storage_guards = match self.caravan_paid_guards(
                &p,
                mandate.as_ref().expect("validated mandate"),
                record.version,
            ) {
                Ok(guards) => guards,
                Err(reason) => return Some(self.caravan_block(p, reason)),
            };
            let deposits = p.deposits.take().expect("exact native cargo plan");
            return Some(self.caravan_prepare_cargo(p, deposits, storage_guards));
        }
        if p.stage == Stage::Registry {
            p.registry_version = record.version;
            p.registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => return Some(report(p.session, "Settlement registry unreadable.")),
            };
            return Some(self.caravan_command(p, Stage::Store, |r| storage_get(r, KEY)));
        }
        p.store = match record.value.as_deref() {
            None => Store::default(),
            Some(v) => match serde_json::from_str(v) {
                Ok(v) => v,
                Err(_) => {
                    return Some(report(
                        p.session,
                        "Caravan store unreadable; no native effect.",
                    ))
                }
            },
        };
        p.version = record.version;
        match p.action.clone() {
            Action::Create {
                id,
                source,
                source_building,
                destination,
                destination_building,
                resident,
                item,
                quantity,
                poll,
                max_ticks,
                route,
                player,
                paid,
            } => {
                if let Some(c) = p.store.contracts.get(&id) {
                    let same = c.owner() == player
                        && c.source.scope == source
                        && c.source.building == source_building
                        && c.destination.scope == destination
                        && c.destination.building == destination_building
                        && c.item == item
                        && c.quantity == quantity
                        && c.paid_mandate.as_deref() == paid.then_some(id.as_str())
                        && c.route == route
                        && c.poll_ticks == poll
                        && c.max_ticks == max_ticks
                        && matches!(&c.party.leader,ActorRef::Npc{home,resident:r,..}if home==&source&&r==&resident);
                    return Some(report(
                        p.session,
                        if same {
                            format!("Caravan {id} already recorded; no cargo withdrawn twice.")
                        } else {
                            format!("Caravan {id} id conflict; no cargo changed.")
                        },
                    ));
                }
                let Some(source) = binding(&p.registry, &source, &source_building)
                    .filter(|s| authorized(&p.registry, s, &player))
                else {
                    return Some(report(
                        p.session,
                        "Caravan source binding/current rights required.",
                    ));
                };
                let Some(destination) = binding(&p.registry, &destination, &destination_building)
                else {
                    return Some(report(p.session,"Existing destination physical identity required; recipient rights/capacity are checked at native arrival."));
                };
                let Some(worker) = p.registry.settlements[&source.scope]
                    .residents
                    .get(&resident)
                    .filter(|r| r.role.is_none())
                else {
                    return Some(report(
                        p.session,
                        "Available genuine civilian resident required.",
                    ));
                };
                let leader = ActorRef::Npc {
                    handle: worker.handle.clone(),
                    home: source.scope.clone(),
                    resident,
                };
                let c = Contract {
                    party: Party {
                        id: id.clone(),
                        owner: ActorRef::Player { player },
                        leader: leader.clone(),
                        members: vec![leader],
                        executor_epoch: 1,
                        parked: false,
                    },
                    carrier: worker.handle.clone(),
                    carrier_uuid: None,
                    source,
                    destination,
                    item,
                    quantity,
                    paid_mandate: paid.then(|| id.clone()),
                    manifest: None,
                    carry_slot: None,
                    route,
                    cursor: 0,
                    poll_ticks: poll,
                    max_ticks,
                    next_due: self.caravan_runtime.now,
                    status: Status::Loading,
                    blocked: None,
                    loaded: false,
                    attempt: 0,
                    intent: None,
                    reservation: None,
                    route_ref: None,
                    started: false,
                    arrival_ref: None,
                    arrival_epoch: None,
                    arrival_rights_revision: None,
                    handoff_ref: None,
                    death_ref: None,
                    death_epoch: None,
                    death_uuid: None,
                    native_ticks: 0,
                    geometry_cells: 0,
                    geometry_hash: None,
                    receipts: Vec::new(),
                };
                if let Err(reason) = p.store.admit(c) {
                    return Some(report(
                        p.session,
                        format!("Caravan admission refused: {reason}; no cargo changed."),
                    ));
                }
                p.id = Some(id);
                Some(self.caravan_write(p, Stage::StateWrite))
            }
            Action::Status { id, player } => {
                if !p
                    .store
                    .contracts
                    .get(&id)
                    .is_some_and(|c| c.owner() == player)
                {
                    return Some(report(p.session, "Caravan owner required."));
                }
                p.id = Some(id.clone());
                Some(if p.store.contracts[&id].route_ref.is_some() {
                    self.caravan_route(p)
                } else {
                    self.caravan_pose(p)
                })
            }
            Action::Handoff {
                id,
                epoch,
                mode,
                player,
            } => {
                let Some(c) = p.store.contracts.get_mut(&id).filter(|c| {
                    c.owner() == player && c.party.executor_epoch == epoch && !c.terminal()
                }) else {
                    return Some(report(
                        p.session,
                        "Caravan owner/current native executor epoch required.",
                    ));
                };
                if c.intent.is_some() {
                    return Some(report(
                        p.session,
                        "Unresolved native intent: recover original receipt before handoff.",
                    ));
                }
                if c.route_ref.is_none() {
                    return Some(report(
                        p.session,
                        "Native route must be admitted before executor handoff.",
                    ));
                }
                let Some(operation) = c.operation(Effect::Handoff) else {
                    return Some(report(p.session, "Operation sequence exhausted."));
                };
                c.intent = Some(Intent::Handoff {
                    operation,
                    epoch,
                    mode,
                });
                p.id = Some(id);
                Some(self.caravan_write(p, Stage::IntentWrite))
            }
            Action::Carrier {
                scope,
                resident,
                player,
            } => {
                let Some(settlement) = p
                    .registry
                    .settlements
                    .get(&scope)
                    .filter(|s| can_build_settlement(s, &player))
                else {
                    return Some(report(
                        p.session,
                        "Current carrier home settlement rights required.",
                    ));
                };
                let Some(carrier) = settlement
                    .residents
                    .get(&resident)
                    .map(|r| r.handle.clone())
                else {
                    return Some(report(
                        p.session,
                        "Genuine social resident reference required.",
                    ));
                };
                Some(self.caravan_command(p, Stage::Carrier, |r| query_npcs(r, &[carrier], None)))
            }
            Action::Wake => {
                let Some(id) = p.store.due(self.caravan_runtime.now) else {
                    return Some(self.caravan_schedule(&p.store));
                };
                p.store.cursor = Some(id.clone());
                p.id = Some(id.clone());
                let c = p.store.contracts.get_mut(&id).expect("contract");
                c.next_due = self
                    .caravan_runtime
                    .now
                    .checked_add(c.poll_ticks)
                    .unwrap_or(MAX_SAFE_REVISION)
                    .min(MAX_SAFE_REVISION);
                Some(if c.intent.is_some() {
                    self.caravan_native(p)
                } else if c.route_ref.is_some() {
                    self.caravan_route(p)
                } else {
                    self.caravan_pose(p)
                })
            }
        }
    }
    fn caravan_schedule(&self, store: &Store) -> Vec<Command> {
        store
            .contracts
            .values()
            .filter(|c| c.intent.is_some() || !c.terminal())
            .map(|c| c.next_due)
            .min()
            .map(|due| {
                vec![schedule_timer(
                    TIMER,
                    due.saturating_sub(self.caravan_runtime.now).max(1),
                )]
            })
            .unwrap_or_default()
    }
    pub(super) fn caravan_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let mut p = self.caravan_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::IntentWrite | Stage::StateWrite)
        {
            self.caravan_runtime.pending = Some(p);
            return None;
        }
        let StorageCasOutcome::Committed(version) = answer.outcome() else {
            let mut commands = report(
                p.session,
                "Caravan CAS refused; no new native effect. Original intent recovers unchanged.",
            );
            commands.extend(self.caravan_schedule(&p.store));
            return Some(commands);
        };
        if p.stage == Stage::IntentWrite {
            p.version = Some(*version);
            return Some(self.caravan_native(p));
        }
        let mut commands =
            p.id.as_ref()
                .map(|id| {
                    let c = &p.store.contracts[id];
                    report(
                        p.session,
                        format!(
                            "Caravan {id} committed: {:?} epoch={}.",
                            c.status, c.party.executor_epoch
                        ),
                    )
                })
                .unwrap_or_default();
        commands.extend(self.caravan_schedule(&p.store));
        Some(commands)
    }
    fn caravan_route_receipt(
        &mut self,
        mut p: Pending,
        status: &npc_routes::NpcRouteStatus,
    ) -> Vec<Command> {
        let id = p.id.as_ref().expect("contract").clone();
        let c = p.store.contracts.get_mut(&id).expect("contract");
        if status.handle != c.carrier
            || c.route_ref.as_ref().is_some_and(|r| r != &status.route_ref)
            || c.carrier_uuid
                .as_ref()
                .is_some_and(|uuid| uuid != &status.entity_uuid)
        {
            return self.caravan_block(
                p,
                "native route identity mismatch; original intent retained",
            );
        }
        if matches!(p.action, Action::Status { .. }) {
            let text = native_evidence(c, status);
            if status.lifecycle != npcs::NpcLifecycle::AliveLoaded {
                return report(p.session, text);
            }
            let endpoint = InventoryEndpoint::NpcCarry(c.carrier.clone());
            p.evidence = Some(text);
            return self.caravan_inventory(p, Stage::Evidence, endpoint);
        }
        if let Some(intent) = c.intent.clone() {
            let effect = match intent {
                Intent::Admit { .. } => Effect::Admit,
                Intent::Start { .. } => Effect::Start,
                Intent::Handoff { epoch, .. } => {
                    if status.executor_epoch <= epoch
                        || status.handoff.as_ref().is_none_or(|h| {
                            h.source_epoch != epoch || h.target_epoch != status.executor_epoch
                        })
                    {
                        return self.caravan_block(
                            p,
                            "native handoff barrier receipt missing; original intent retained",
                        );
                    }
                    Effect::Handoff
                }
                Intent::Cancel { .. } => {
                    if status.state != npc_routes::NpcRouteState::Cancelled {
                        return self.caravan_block(
                            p,
                            "native cancellation barrier missing; original intent retained",
                        );
                    }
                    Effect::Cancel
                }
                _ => return self.caravan_block(p, "wrong native route result for cargo intent"),
            };
            c.receipts.retain(|r| r.effect != effect);
            c.receipts.push(Receipt {
                operation: intent.operation().into(),
                effect,
                epoch: status.executor_epoch,
                native_revision: Some(status.revision),
                endpoints: Vec::new(),
            });
            c.intent = None;
            c.route_ref = Some(status.route_ref.clone());
            if c.carrier_uuid.is_none() {
                c.carrier_uuid = Some(status.entity_uuid.clone());
            }
            if effect == Effect::Start {
                c.started = true;
            }
            if effect == Effect::Cancel {
                c.party.executor_epoch = status.executor_epoch;
                c.status = Status::Completed;
                return self.caravan_write(p, Stage::StateWrite);
            }
        }
        c.party.executor_epoch = status.executor_epoch;
        c.party.parked = status.mode == npc_routes::NpcExecutorMode::Parked;
        c.cursor = usize::from(status.cursor);
        c.native_ticks = status.native_ticks;
        c.geometry_cells = status.geometry_cells;
        if c.geometry_hash.as_deref() != Some(status.geometry_hash.as_str()) {
            c.geometry_hash = Some(status.geometry_hash.clone());
        }
        let handoff = status.handoff.as_ref().map(|h| h.snapshot_ref.as_str());
        if c.handoff_ref.as_deref() != handoff {
            c.handoff_ref = handoff.map(str::to_owned);
        }
        match status.state {
            npc_routes::NpcRouteState::Dead => {
                let Some(death) = &status.death_cargo else {
                    return self.caravan_block(p, "canonical native death/loot receipt not published; no loss ledger inferred");
                };
                if let Err(reason) =
                    c.observe_death(death, &status.entity_uuid, Some(status.executor_epoch))
                {
                    return self.caravan_block(p, reason);
                }
                if c.status == Status::Lost && !c.loaded {
                    if let Some(reservation) = &c.reservation {
                        let revision = reservation.revision;
                        let Some(operation) = c.operation(Effect::Release) else {
                            return self.caravan_block(p, "operation sequence exhausted");
                        };
                        c.intent = Some(Intent::Release {
                            operation,
                            revision,
                        });
                        return self.caravan_write(p, Stage::IntentWrite);
                    }
                }
                self.caravan_write(p, Stage::StateWrite)
            }
            npc_routes::NpcRouteState::Prepared => {
                if c.loaded {
                    let Some(cargo) = c
                        .receipts
                        .iter()
                        .find(|r| r.effect == Effect::Load)
                        .map(|r| r.operation.clone())
                    else {
                        return self.caravan_block(
                            p,
                            "committed cargo receipt required to start native travel",
                        );
                    };
                    let Some(operation) = c.operation(Effect::Start) else {
                        return self.caravan_block(p, "operation sequence exhausted");
                    };
                    c.intent = Some(Intent::Start {
                        operation,
                        epoch: c.party.executor_epoch,
                        cargo_operation: cargo,
                    });
                    return self.caravan_write(p, Stage::IntentWrite);
                }
                let valid = status.pose.as_ref().is_some_and(|pose| {
                    pose.dimension == c.source.dimension
                        && c.route[0].observed(pose.x, pose.y, pose.z)
                });
                if !valid {
                    return self.caravan_block(
                        p,
                        "actual carrier must occupy admitted source stance before loading",
                    );
                }
                self.caravan_live(p)
            }
            npc_routes::NpcRouteState::Arrived => {
                let Some(arrival) = status.arrival.as_ref().filter(|a| {
                    a.executor_epoch == c.party.executor_epoch
                        && a.dimension == c.destination.dimension
                        && a.pose.dimension == c.destination.dimension
                        && c.route
                            .last()
                            .expect("route")
                            .observed(a.pose.x, a.pose.y, a.pose.z)
                }) else {
                    return self.caravan_block(
                        p,
                        "native arrival receipt/current physical destination stance required",
                    );
                };
                c.status = Status::Arrival;
                if c.receipts.iter().any(|r| r.effect == Effect::Unload) {
                    if let Some(mandate) = c.paid_mandate.as_deref() {
                        let key = super::payment_mandate::key(mandate);
                        return self
                            .caravan_command(p, Stage::PaidFinish, |r| storage_get(r, &key));
                    }
                    let Some(operation) = c.operation(Effect::Cancel) else {
                        return self.caravan_block(p, "operation sequence exhausted");
                    };
                    c.intent = Some(Intent::Cancel {
                        operation,
                        epoch: c.party.executor_epoch,
                    });
                    return self.caravan_write(p, Stage::IntentWrite);
                }
                c.arrival_ref = Some(arrival.receipt_ref.clone());
                c.arrival_epoch = Some(arrival.executor_epoch);
                self.caravan_live(p)
            }
            npc_routes::NpcRouteState::Running => {
                c.status = Status::Transit;
                c.blocked = None;
                self.caravan_write(p, Stage::StateWrite)
            }
            npc_routes::NpcRouteState::Blocked => self.caravan_block(
                p,
                format!(
                    "native route {:?}; no physical advance/cargo handoff",
                    status.blocked_reason
                ),
            ),
            npc_routes::NpcRouteState::Parked | npc_routes::NpcRouteState::HandoffPending => {
                self.caravan_block(p, "native executor parked/handoff pending; no second loop")
            }
            npc_routes::NpcRouteState::Exhausted => self.caravan_block(
                p,
                "finite native route tick budget exhausted; no elapsed/offscreen continuation",
            ),
            npc_routes::NpcRouteState::Cancelled => self.caravan_block(
                p,
                "native route externally cancelled; actual cargo retained",
            ),
        }
    }
    pub(super) fn caravan_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.caravan_runtime.pending.take()?;
        if p.request != answer.request() {
            self.caravan_runtime.pending = Some(p);
            return None;
        }
        if p.stage == Stage::Carrier {
            let Action::Carrier {
                scope, resident, ..
            } = &p.action
            else {
                unreachable!()
            };
            let handle = &p.registry.settlements[scope].residents[resident].handle;
            let snapshot = match answer.outcome() {
                OperationOutcome::Committed(c) => match &c.payload {
                    OperationPayload::Npc(NpcResult::Page(page)) => {
                        page.npcs.iter().find(|n| &n.handle == handle)
                    }
                    _ => None,
                },
                _ => None,
            };
            return Some(match snapshot {
                Some(s) => {
                    let pose = s.loaded.as_ref().map(
                        |p| serde_json::json!({"dimension":p.dimension,"x":p.x,"y":p.y,"z":p.z}),
                    );
                    report(
                        p.session,
                        format!(
                            "Caravan carrier {scope}/{resident}: native={}",
                            serde_json::json!({"handle":s.handle,"uuid":s.entity_uuid,"revision":s.revision,"lifecycle":format!("{:?}",s.lifecycle),"pose":pose})
                        ),
                    )
                }
                None => report(
                    p.session,
                    "Caravan carrier native query refused; no cached/fabricated pose.",
                ),
            });
        }
        let id = p.id.as_ref().expect("contract").clone();
        if p.stage == Stage::PlayerStatus {
            match answer.outcome() {
                OperationOutcome::Refused(refused)
                    if matches!(refused.reason, OperationFailure::NotFound) =>
                {
                    return Some(self.caravan_command(p, Stage::PlayerIndex, |r| {
                        storage_get(r, super::commerce_player::INDEX_KEY)
                    }));
                }
                OperationOutcome::Committed(committed) => {
                    let OperationPayload::NpcRoute(npc_routes::NpcRouteResult::Status(status)) =
                        &committed.payload
                    else {
                        return Some(self.caravan_player_wait(
                            p,
                            "Original route operation receipt payload mismatch.",
                        ));
                    };
                    if answer.operation_id.as_deref()
                        != p.store.contracts[&id]
                            .intent
                            .as_ref()
                            .map(Intent::operation)
                    {
                        return Some(self.caravan_player_wait(
                            p,
                            "Original durable route operation id mismatch.",
                        ));
                    }
                    p.stage = Stage::Native;
                    return Some(self.caravan_route_receipt(p, status));
                }
                _ => {
                    return Some(self.caravan_player_wait(
                        p,
                        "Original start/cancel decision unresolved; no new route operation.",
                    ))
                }
            }
        }
        if p.stage == Stage::Native
            && answer.operation_id.as_deref()
                != p.store.contracts[&id]
                    .intent
                    .as_ref()
                    .map(Intent::operation)
        {
            self.caravan_runtime.pending = Some(p);
            return Some(Vec::new());
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            if p.stage == Stage::Evidence {
                return Some(report(
                    p.session,
                    format!(
                        "{} cargo-query-refused={:?}",
                        p.evidence.as_deref().unwrap_or(""),
                        answer.outcome
                    ),
                ));
            }
            if p.stage == Stage::Native
                && matches!(answer.outcome(), OperationOutcome::Refused(refused)
                if matches!(refused.reason, OperationFailure::RuntimeUnavailable))
                && p.player_gate_done
            {
                return Some(self.caravan_player_wait(p, "Original guarded route decision unavailable; retain and resolve the original operation."));
            }
            if p.stage == Stage::Native {
                p.store.contracts.get_mut(&id).expect("contract").intent = None;
            }
            return Some(self.caravan_block(p, format!("native refusal {:?}", answer.outcome)));
        };
        match p.stage {
            Stage::Route | Stage::Native
                if matches!(&committed.payload, OperationPayload::NpcRoute(_)) =>
            {
                let OperationPayload::NpcRoute(npc_routes::NpcRouteResult::Status(status)) =
                    &committed.payload
                else {
                    unreachable!()
                };
                Some(self.caravan_route_receipt(p, status))
            }
            Stage::Pose => {
                let snapshot = match &committed.payload {
                    OperationPayload::Npc(NpcResult::Page(page)) => page
                        .npcs
                        .iter()
                        .find(|n| n.handle == p.store.contracts[&id].carrier),
                    _ => None,
                };
                let Some(snapshot) = snapshot else {
                    return Some(self.caravan_block(p, "wrong actual carrier query result"));
                };
                let c = &p.store.contracts[&id];
                if snapshot.lifecycle == npcs::NpcLifecycle::Dead {
                    let carrier = c.carrier.clone();
                    p.native_uuid = Some(snapshot.entity_uuid.clone());
                    return Some(
                        self.caravan_command(p, Stage::Death, |r| {
                            query_npc_controls(r, &[carrier])
                        }),
                    );
                }
                if matches!(p.action, Action::Status { .. }) {
                    let pose = snapshot.loaded.as_ref().map(
                        |s| serde_json::json!({"dimension":s.dimension,"x":s.x,"y":s.y,"z":s.z}),
                    );
                    return Some(report(
                        p.session,
                        format!(
                            "Caravan {id}: status={:?} epoch={} loaded={} reason={} native={}",
                            c.status,
                            c.party.executor_epoch,
                            c.loaded,
                            c.blocked.as_deref().unwrap_or("none"),
                            serde_json::json!({"uuid":snapshot.entity_uuid,"lifecycle":format!("{:?}",snapshot.lifecycle),"pose":pose,"route":null})
                        ),
                    ));
                }
                if !snapshot.loaded.as_ref().is_some_and(|s| {
                    s.dimension == c.source.dimension && c.route[0].observed(s.x, s.y, s.z)
                }) {
                    return Some(self.caravan_block(
                        p,
                        "actual loaded source stance required; no route/withdrawal admitted",
                    ));
                }
                p.identity_revision = Some(snapshot.revision);
                let carrier = c.carrier.clone();
                Some(
                    self.caravan_command(p, Stage::Controls, |r| query_npc_controls(r, &[carrier])),
                )
            }
            Stage::Death => {
                let death = match &committed.payload {
                    OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) => page
                        .npcs
                        .iter()
                        .find(|n| n.handle == p.store.contracts[&id].carrier)
                        .and_then(|n| n.death_cargo.as_ref()),
                    _ => None,
                };
                let Some(death) = death else {
                    if matches!(p.action, Action::Status { .. }) {
                        return Some(report(p.session, "Canonical native death/loot receipt not published; no loss ledger inferred."));
                    }
                    return Some(self.caravan_block(p, "canonical native death/loot receipt not published; no loss ledger inferred"));
                };
                if matches!(p.action, Action::Status { .. }) {
                    return Some(report(
                        p.session,
                        format!(
                            "Caravan {id}: status={:?} native={}",
                            p.store.contracts[&id].status,
                            serde_json::json!({"death_cargo":death_evidence(death),"death_ref":p.store.contracts[&id].death_ref})
                        ),
                    ));
                }
                let c = p.store.contracts.get_mut(&id).expect("contract");
                if let Err(reason) = c.observe_death(
                    death,
                    p.native_uuid.as_deref().expect("actual native UUID"),
                    None,
                ) {
                    return Some(self.caravan_block(p, reason));
                }
                if c.status == Status::Completed {
                    return Some(self.caravan_write(p, Stage::StateWrite));
                }
                if let Some(reservation) = &c.reservation {
                    let revision = reservation.revision;
                    let Some(operation) = c.operation(Effect::Release) else {
                        return Some(self.caravan_block(p, "operation sequence exhausted"));
                    };
                    c.intent = Some(Intent::Release {
                        operation,
                        revision,
                    });
                    return Some(self.caravan_write(p, Stage::IntentWrite));
                }
                Some(self.caravan_write(p, Stage::StateWrite))
            }
            Stage::Controls => {
                let control = match &committed.payload {
                    OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) => page
                        .npcs
                        .iter()
                        .find(|n| n.handle == p.store.contracts[&id].carrier),
                    _ => None,
                };
                let Some(control) = control else {
                    return Some(self.caravan_block(p, "wrong actual control query result"));
                };
                if control.work.as_ref().is_some_and(|w| {
                    matches!(
                        w.state,
                        WorkState::Accepted | WorkState::Running | WorkState::Paused
                    )
                }) || control.pending_work.is_some()
                {
                    return Some(
                        self.caravan_block(p, "active local assignment must stop before travel"),
                    );
                }
                if p.store.contracts[&id].reservation.is_none() {
                    return Some(self.caravan_live(p));
                }
                let c = p.store.contracts.get_mut(&id).expect("contract");
                let Some(operation) = c.operation(Effect::Admit) else {
                    return Some(self.caravan_block(p, "operation sequence exhausted"));
                };
                c.intent = Some(Intent::Admit {
                    operation,
                    identity_revision: p.identity_revision.expect("actual identity query"),
                    order_revision: control.order_revision,
                });
                Some(self.caravan_write(p, Stage::IntentWrite))
            }
            Stage::Live => {
                let c = &p.store.contracts[&id];
                let target = if c.loaded { &c.destination } else { &c.source };
                let b = &p.registry.settlements[&target.scope].buildings[&target.building];
                let valid = match &committed.payload {
                    OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(
                        check,
                    )) => {
                        check.valid
                            && b.worksite_id.as_deref().unwrap_or(&target.live_ref)
                                == check.worksite_id
                    }
                    OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(
                        check,
                    )) => check.valid && target.live_ref == check.structure_id,
                    _ => false,
                };
                if !valid {
                    return Some(
                        self.caravan_block(p, "source/recipient physical building unusable"),
                    );
                }
                let endpoint = if c.loaded {
                    InventoryEndpoint::NpcCarry(c.carrier.clone())
                } else {
                    c.source.endpoint()
                };
                Some(self.caravan_inventory(p, Stage::Source, endpoint))
            }
            Stage::Source | Stage::Destination | Stage::Evidence => {
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(self.caravan_block(p, "wrong actual inventory response"));
                };
                let c = &p.store.contracts[&id];
                let expected = match (p.stage, c.loaded) {
                    (Stage::Evidence, _) | (Stage::Source, true) | (Stage::Destination, false) => {
                        InventoryEndpoint::NpcCarry(c.carrier.clone())
                    }
                    (Stage::Source, false) => c.source.endpoint(),
                    _ => c.destination.endpoint(),
                };
                if !same_inventory_endpoint(&expected, &snapshot.endpoint) {
                    return Some(self.caravan_block(p, "wrong actual inventory endpoint"));
                }
                if p.stage == Stage::Evidence {
                    let cargo:Vec<_>=snapshot.slots.iter().filter_map(|s|s.item.as_ref().map(|i|serde_json::json!({"slot":s.slot,"count":i.count,"stack":Stack::from_native(i)}))).collect();
                    return Some(report(
                        p.session,
                        format!(
                            "{} cargo={} cargo-fence={}",
                            p.evidence.as_deref().unwrap_or(""),
                            serde_json::json!(cargo),
                            serde_json::json!({"revision":snapshot.fence.revision,"hash":snapshot.fence.snapshot_hash})
                        ),
                    ));
                }
                if p.stage == Stage::Source {
                    p.source = Some(snapshot.clone());
                    let endpoint = if c.loaded {
                        c.destination.endpoint()
                    } else {
                        InventoryEndpoint::NpcCarry(c.carrier.clone())
                    };
                    return Some(self.caravan_inventory(p, Stage::Destination, endpoint));
                }
                let source = p.source.as_ref().expect("source query");
                let c = &p.store.contracts[&id];
                let slot = source.slots.iter().find(|s| {
                    s.item.as_ref().is_some_and(|i| {
                        if c.loaded {
                            Some(s.slot) == c.carry_slot
                                && i.count == c.quantity
                                && c.manifest.as_ref().is_some_and(|m| m.matches(i))
                        } else {
                            i.count >= c.quantity
                                && i.resource_id == c.item
                                && c.manifest.as_ref().is_none_or(|m| m.matches(i))
                        }
                    })
                });
                let Some(slot) = slot else {
                    return Some(self.caravan_block(p,"authorized exact cargo absent/changed; no substitution or minted replacement"));
                };
                let source_slot = slot.slot;
                let source_revision = inventories::InventoryExpectedRevision {
                    endpoint: source.endpoint.clone(),
                    fence: hire::Fence::from_snapshot(source).wire(),
                };
                let destination_revision = inventories::InventoryExpectedRevision {
                    endpoint: snapshot.endpoint.clone(),
                    fence: hire::Fence::from_snapshot(snapshot).wire(),
                };
                p.source_slot = Some(source_slot);
                p.destination = Some(snapshot.clone());
                Some(self.caravan_command(p, Stage::Placement, |r| {
                    query_inventory_placement_room(
                        r,
                        source_revision,
                        source_slot,
                        destination_revision,
                    )
                }))
            }
            Stage::Placement => {
                let OperationPayload::OwnedInventory(InventoryResult::PlacementRoom(room)) =
                    &committed.payload
                else {
                    return Some(self.caravan_block(p, "wrong native placement capacity response"));
                };
                let source = p.source.as_ref().expect("source query");
                let destination = p.destination.as_ref().expect("destination query");
                let source_slot = p.source_slot.expect("actual source slot");
                if room.source_slot != source_slot
                    || !same_inventory_endpoint(&room.source.endpoint, &source.endpoint)
                    || !same_inventory_endpoint(&room.destination.endpoint, &destination.endpoint)
                    || room.source.fence.revision != source.fence.revision
                    || room.source.fence.snapshot_hash != source.fence.snapshot_hash
                    || room.destination.fence.revision != destination.fence.revision
                    || room.destination.fence.snapshot_hash != destination.fence.snapshot_hash
                    || room.slots.len() != destination.slots.len()
                    || !destination
                        .slots
                        .iter()
                        .all(|s| room.slots.iter().filter(|r| r.slot == s.slot).count() == 1)
                {
                    return Some(self.caravan_block(
                        p,
                        "native placement image/fence mismatch; no cargo effect",
                    ));
                }
                let c = p.store.contracts.get_mut(&id).expect("contract");
                let deposits = if c.loaded {
                    let Some(deposits) = plan_deposits(c.quantity, &room.slots) else {
                        return Some(self.caravan_block(p, "destination full or exceeds native atomic portion bound; actual cargo stays with current native owner"));
                    };
                    deposits
                } else {
                    let Some(target) = room.slots.iter().find(|r| {
                        r.room >= c.quantity
                            && destination
                                .slots
                                .iter()
                                .any(|s| s.slot == r.slot && s.item.is_none())
                    }) else {
                        return Some(self.caravan_block(p, "carrier carry requires one empty canonical slot with exact native room for separate escrow cargo"));
                    };
                    vec![Deposit {
                        slot: target.slot,
                        count: c.quantity,
                    }]
                };
                if !c.loaded && c.manifest.is_none() {
                    let item = source
                        .slots
                        .iter()
                        .find(|s| s.slot == source_slot)
                        .and_then(|s| s.item.as_ref())
                        .expect("authorized actual source stack");
                    c.manifest = Some(Stack::from_native(item));
                }
                {
                    if let Some(mandate) = c.paid_mandate.as_deref() {
                        let key = super::payment_mandate::key(mandate);
                        p.deposits = Some(deposits);
                        return Some(
                            self.caravan_command(p, Stage::PaidEscrow, |r| storage_get(r, &key)),
                        );
                    }
                }
                let guards = match self.caravan_custody_guards(&p) {
                    Ok(guards) => guards,
                    Err(reason) => return Some(self.caravan_block(p, reason)),
                };
                Some(self.caravan_prepare_cargo(p, deposits, guards))
            }
            Stage::Native => {
                let c = p.store.contracts.get_mut(&id).expect("contract");
                let intent = c.intent.clone().expect("intent");
                match intent {
                    Intent::Reserve { operation, .. } => {
                        let OperationPayload::OwnedInventory(InventoryResult::Reservation(r)) =
                            &committed.payload
                        else {
                            return Some(self.caravan_block(
                                p,
                                "unexpected reservation receipt; original intent retained",
                            ));
                        };
                        if !same_inventory_endpoint(&r.endpoint, &c.source.endpoint())
                            || r.released
                            || r.quantities.len() != 1
                            || !r.quantities.iter().any(|q| {
                                q.resource_id == c.item
                                    && q.reserved == u64::from(c.quantity)
                                    && q.remaining == u64::from(c.quantity)
                                    && q.consumed == 0
                            })
                        {
                            return Some(self.caravan_block(
                                p,
                                "exact source escrow receipt mismatch; original intent retained",
                            ));
                        }
                        c.reservation = Some(Reservation {
                            reference: r.reservation_ref.clone(),
                            revision: committed.revision,
                            plan_hash: r.resource_plan_hash.clone(),
                        });
                        c.receipts.push(Receipt {
                            operation,
                            effect: Effect::Reserve,
                            epoch: c.party.executor_epoch,
                            native_revision: Some(committed.revision),
                            endpoints: Vec::new(),
                        });
                        c.intent = None;
                        Some(self.caravan_write(p, Stage::StateWrite))
                    }
                    Intent::Load { transfer } => {
                        let OperationPayload::OwnedInventory(InventoryResult::ReservedTransfer(r)) =
                            &committed.payload
                        else {
                            return Some(self.caravan_block(
                                p,
                                "unexpected reserved load receipt; original intent retained",
                            ));
                        };
                        let carry = InventoryEndpoint::NpcCarry(c.carrier.clone());
                        let reservation = c.reservation.as_ref().expect("reservation");
                        if r.reservation.reservation_ref != reservation.reference
                            || r.reservation.resource_plan_hash != reservation.plan_hash
                            || r.reservation.receipt_watermark != 1
                            || !r.reservation.quantities.iter().any(|q| {
                                q.resource_id == c.item
                                    && q.consumed == u64::from(c.quantity)
                                    && q.remaining == 0
                            })
                            || r.inventories.len() != 2
                            || !r
                                .inventories
                                .iter()
                                .any(|s| same_inventory_endpoint(&s.endpoint, &c.source.endpoint()))
                            || !r.inventories.iter().any(|s| {
                                same_inventory_endpoint(&s.endpoint, &carry)
                                    && s.slots.iter().any(|slot| {
                                        Some(slot.slot) == c.carry_slot
                                            && slot.item.as_ref().is_some_and(|i| {
                                                i.count == c.quantity
                                                    && c.manifest
                                                        .as_ref()
                                                        .is_some_and(|m| m.matches(i))
                                            })
                                    })
                            })
                        {
                            return Some(self.caravan_block(p,"exact physical escrow-to-carrier receipt mismatch; original intent retained"));
                        }
                        c.receipts.push(Receipt {
                            operation: transfer.operation,
                            effect: Effect::Load,
                            epoch: transfer.epoch,
                            native_revision: Some(committed.revision),
                            endpoints: r
                                .inventories
                                .iter()
                                .map(|s| endpoint_receipt(&s.endpoint, &s.fence))
                                .collect(),
                        });
                        c.reservation.as_mut().expect("reservation").revision = committed.revision;
                        c.loaded = true;
                        c.intent = None;
                        c.blocked = None;
                        Some(self.caravan_write(p, Stage::StateWrite))
                    }
                    Intent::Unload { transfer } => {
                        let OperationPayload::OwnedInventory(InventoryResult::Transfer(fences)) =
                            &committed.payload
                        else {
                            return Some(self.caravan_block(
                                p,
                                "unexpected native unload receipt; original intent retained",
                            ));
                        };
                        if fences.len() != 2
                            || !fences.iter().any(|f| {
                                same_inventory_endpoint(
                                    &f.endpoint,
                                    &InventoryEndpoint::NpcCarry(c.carrier.clone()),
                                )
                            })
                            || !fences.iter().any(|f| {
                                same_inventory_endpoint(&f.endpoint, &c.destination.endpoint())
                            })
                        {
                            return Some(self.caravan_block(p,"exact recipient handoff receipt mismatch; original intent retained"));
                        }
                        c.receipts.push(Receipt {
                            operation: transfer.operation,
                            effect: Effect::Unload,
                            epoch: transfer.epoch,
                            native_revision: Some(committed.revision),
                            endpoints: fences
                                .iter()
                                .map(|f| endpoint_receipt(&f.endpoint, &f.fence))
                                .collect(),
                        });
                        c.loaded = false;
                        c.intent = None;
                        c.blocked = None;
                        Some(self.caravan_write(p, Stage::StateWrite))
                    }
                    Intent::Release { operation, .. } => {
                        let OperationPayload::OwnedInventory(InventoryResult::Reservation(r)) =
                            &committed.payload
                        else {
                            return Some(self.caravan_block(p,"unexpected unspent escrow release receipt; original intent retained"));
                        };
                        if c.reservation
                            .as_ref()
                            .is_none_or(|x| x.reference != r.reservation_ref)
                            || !r.released
                            || r.quantities.iter().any(|q| q.remaining != 0)
                        {
                            return Some(self.caravan_block(
                                p,
                                "escrow release accounting conflict; original intent retained",
                            ));
                        }
                        c.receipts.push(Receipt {
                            operation,
                            effect: Effect::Release,
                            epoch: c.party.executor_epoch,
                            native_revision: Some(committed.revision),
                            endpoints: Vec::new(),
                        });
                        c.intent = None;
                        c.status = Status::Lost;
                        Some(self.caravan_write(p, Stage::StateWrite))
                    }
                    _ => Some(self.caravan_block(
                        p,
                        "unexpected native mutation payload; original intent retained",
                    )),
                }
            }
            _ => {
                self.caravan_runtime.pending = Some(p);
                None
            }
        }
    }
}
