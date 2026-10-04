//! Explicit paid-shipment escrow and immutable delivery consumption.
//! No route preparation, cached boolean, or elapsed time is a delivery receipt.
use super::caravan::{Container, Contract, Stack, StorageGuard, Store};
use super::payment_mandate::{key, Blob, Mandate, Payout, Reservation, State, Terms, VALUE_BYTES};
use super::*;
use solaris_plugin_sdk::{
    commands::OperationStatus, consume_inventory_reservation, inventories, query_npc_controls,
    query_owned_inventory, reserve_inventory_items, schedule_timer, types,
    with_expected_plugin_storage_versions,
};

pub(super) const PAYMENT_TIMER: &str = "settlement-paid-shipment";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
    cursor: Option<String>,
    now: u64,
}
#[derive(Clone)]
enum Action {
    Create {
        id: String,
        buyer: String,
        scope: String,
        building: String,
        home: String,
        resident: String,
        price: u32,
    },
    Status {
        id: String,
        player: String,
    },
    Wake,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Caravans,
    Head,
    BlobRead,
    BlobWriteRead,
    BlobWriteCas,
    HeadCas,
    Commodity,
    Escrow,
    ReserveStatus,
    Reserve,
    Death,
    PayoutEscrow,
    Recipient,
    PayoutBlobRead,
    PayoutStatus,
    Payout,
}
#[derive(Clone, Copy)]
enum After {
    Reserve,
    Delivery,
    Loss,
    PayoutIntent,
    Payment,
}
struct Publication {
    blob: Blob,
    chunks: Vec<String>,
    cursor: usize,
    after: After,
}
struct Pending {
    request: String,
    session: Option<u64>,
    action: Action,
    stage: Stage,
    registry: SettlementRegistry,
    registry_version: Option<u64>,
    caravan: Option<Contract>,
    id: Option<String>,
    head: Option<Mandate>,
    version: Option<u64>,
    terms: Option<Terms>,
    commodity: Option<Stack>,
    chunks: Vec<String>,
    publication: Option<Publication>,
    payout: Option<Payout>,
    escrow: Option<inventories::OwnedInventorySnapshot>,
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} paid-shipment {text}"));
    let mut commands = Vec::new();
    if let Some(session) = session {
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + VALUE_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            commands.push(message_session(session, text[start..end].to_owned()));
            start = end;
        }
    }
    commands.push(schedule_timer(PAYMENT_TIMER, 1));
    commands
}
fn current_binding(registry: &SettlementRegistry, target: &Container, player: &str) -> bool {
    let Some(s) = registry
        .settlements
        .get(&target.scope)
        .filter(|s| can_build_settlement(s, player))
    else {
        return false;
    };
    let Some(b) = s.buildings.get(&target.building) else {
        return false;
    };
    let live_ref = match &b.origin {
        BuildingOrigin::Certificate { certificate_id } => certificate_id,
        BuildingOrigin::Structure { structure_id } => structure_id,
    };
    live_ref == &target.live_ref
        && b.dimension.as_deref() == Some(target.dimension.as_str())
        && b.storage_positions
            .first()
            .is_some_and(|p| p.x == target.at.x && p.y == target.at.y && p.z == target.at.z)
}
fn plan(price: u32) -> inventories::InventoryResourcePlan {
    inventories::InventoryResourcePlan {
        portions: vec![inventories::InventoryWorkPortion {
            work_units: 1,
            materials: vec![inventories::InventoryMaterial {
                resource_id: "minecraft:emerald".into(),
                quantity: u64::from(price),
            }],
        }],
    }
}
impl Settlements {
    pub(super) fn handle_paid_shipment_command(
        &mut self,
        invoked: &CommandInvoked,
        tick: u64,
    ) -> Option<Vec<Command>> {
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if invoked.name != "settlement"
            || !matches!(
                args.first(),
                Some(&"paid-shipment") | Some(&"paid-shipment-status")
            )
        {
            return None;
        }
        self.payment_runtime.now = tick;
        if self.payment_runtime.pending.is_some() {
            return Some(report(
                Some(invoked.session),
                "Paid shipment busy; no second effect issued.",
            ));
        }
        let action=match args.as_slice() {
            ["paid-shipment",id,scope,building,home,resident,price]=>{
                let Some(price)=price.parse::<u32>().ok().filter(|p|*p>0 && *p<=4096) else {return Some(report(Some(invoked.session),"Explicit positive negotiated emerald price must fit native 4096-item vector."));};
                Action::Create {id:(*id).into(),buyer:invoked.player.clone(),scope:(*scope).into(),building:(*building).into(),home:(*home).into(),resident:(*resident).into(),price}
            },
            ["paid-shipment-status",id]=>Action::Status {id:(*id).into(),player:invoked.player.clone()},
            _=>return Some(report(Some(invoked.session),"Usage: paid-shipment <shipment> <buyer-scope> <escrow-building> <recipient-home> <recipient-resident> <emerald-price>; paid-shipment-status <shipment>")),
        };
        Some(self.paid_begin(action, Some(invoked.session)))
    }
    pub(super) fn paid_shipment_init(&mut self) -> Vec<Command> {
        self.payment_runtime.pending = None;
        vec![schedule_timer(PAYMENT_TIMER, 1)]
    }
    pub(super) fn paid_shipment_timer(&mut self, tick: u64) -> Vec<Command> {
        self.payment_runtime.now = tick;
        if self.payment_runtime.pending.is_some() {
            return vec![schedule_timer(PAYMENT_TIMER, 1)];
        }
        self.paid_begin(Action::Wake, None)
    }
    fn paid_begin(&mut self, action: Action, session: Option<u64>) -> Vec<Command> {
        let p = Pending {
            request: String::new(),
            session,
            action,
            stage: Stage::Registry,
            registry: SettlementRegistry::default(),
            registry_version: None,
            caravan: None,
            id: None,
            head: None,
            version: None,
            terms: None,
            commodity: None,
            chunks: Vec::new(),
            publication: None,
            payout: None,
            escrow: None,
        };
        self.paid_read(p, Stage::Registry, registry_storage::ROOT_KEY.to_owned())
    }
    fn paid_read(&mut self, mut p: Pending, stage: Stage, key: String) -> Vec<Command> {
        let Some(request) = self.next_storage_request("paid-read") else {
            return report(
                p.session,
                "Request sequence exhausted; original intents retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        self.payment_runtime.pending = Some(p);
        if stage == Stage::Registry {
            self.registry_read(&request)
        } else {
            vec![storage_get(&request, &key)]
        }
    }
    fn paid_inventory(
        &mut self,
        mut p: Pending,
        stage: Stage,
        endpoint: InventoryEndpoint,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("paid-inventory") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = stage;
        self.payment_runtime.pending = Some(p);
        vec![query_owned_inventory(&request, endpoint, None)]
    }
    fn paid_head(&mut self, mut p: Pending) -> Vec<Command> {
        let head = p.head.as_ref().expect("head");
        let Ok(value) = serde_json::to_string(head) else {
            return report(p.session, "Paid mandate serialization refused.");
        };
        if value.len() > VALUE_BYTES {
            return report(p.session,"Paid mandate header exceeds native value bound; immutable gross fragments retained.");
        }
        let Some(request) = self.next_storage_request("paid-head") else {
            return report(p.session, "Request sequence exhausted.");
        };
        let command = storage_cas(&request, &key(&head.shipment), p.version, value);
        p.request = request;
        p.stage = Stage::HeadCas;
        self.payment_runtime.pending = Some(p);
        vec![command]
    }
    fn paid_publish<T: Serialize>(
        &mut self,
        mut p: Pending,
        value: &T,
        after: After,
    ) -> Vec<Command> {
        let Ok((blob, chunks)) = Blob::encode(value) else {
            return report(
                p.session,
                "Complete immutable receipt serialization refused; no ACK.",
            );
        };
        p.publication = Some(Publication {
            blob,
            chunks,
            cursor: 0,
            after,
        });
        let key = p.publication.as_ref().expect("publication").blob.key(0);
        self.paid_read(p, Stage::BlobWriteRead, key)
    }
    fn paid_publish_next(&mut self, mut p: Pending) -> Vec<Command> {
        let publication = p.publication.as_mut().expect("publication");
        publication.cursor += 1;
        if publication.cursor < publication.chunks.len() {
            let key = publication.blob.key(publication.cursor);
            return self.paid_read(p, Stage::BlobWriteRead, key);
        }
        let publication = p.publication.take().expect("publication");
        match publication.after {
            After::Reserve => p.head.as_mut().expect("head").contract = publication.blob,
            After::Delivery => {
                let h = p.head.as_mut().expect("head");
                h.gross = Some(publication.blob);
                h.state = State::DeliveryRecorded;
                // Only the acknowledged native unload makes this eligible for payment.
            }
            After::Loss => {
                let h = p.head.as_mut().expect("head");
                h.loss = Some(publication.blob);
                h.state = State::Lost;
                h.blocked=Some("Actual native death cargo remains world loot; no escrow payout or source refund.".into());
            }
            After::PayoutIntent => {
                let h = p.head.as_mut().expect("head");
                h.payout = Some(publication.blob);
                h.state = State::PayoutIntent;
                h.blocked = None;
            }
            After::Payment => {
                let h = p.head.as_mut().expect("head");
                h.payment_receipt = Some(publication.blob);
                h.state = State::Paid;
                h.blocked = None;
            }
        }
        self.paid_head(p)
    }
    fn paid_reserve_status(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(request) = self.next_storage_request("paid-reserve-status") else {
            return report(
                p.session,
                "Request sequence exhausted; original escrow intent retained.",
            );
        };
        let command = Command::OperationStatus(OperationStatus {
            request: request.clone(),
            operation_id: p.terms.as_ref().expect("terms").reserve_operation.clone(),
        });
        p.request = request;
        p.stage = Stage::ReserveStatus;
        self.payment_runtime.pending = Some(p);
        vec![command]
    }
    fn paid_reserve(&mut self, mut p: Pending) -> Vec<Command> {
        let terms = p.terms.as_ref().expect("terms");
        let Some(head_version) = p.version.filter(|version| *version > 0) else {
            return report(
                p.session,
                "Actual positive paid mandate version required; no escrow effect.",
            );
        };
        if terms.storage_guards.is_empty()
            || terms.storage_guards.iter().any(|guard| guard.version == 0)
        {
            return report(
                p.session,
                "Original actual settlement guards required; no escrow effect.",
            );
        }
        let mut guards: Vec<_> = terms
            .storage_guards
            .iter()
            .map(StorageGuard::wire)
            .collect();
        guards.push(types::PluginStorageVersionGuard {
            key: key(&terms.shipment),
            version: head_version,
        });
        guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        let Some(request) = self.next_storage_request("paid-reserve") else {
            return report(
                p.session,
                "Request sequence exhausted; escrow intent retained.",
            );
        };
        let command = with_expected_plugin_storage_versions(
            reserve_inventory_items(
                &request,
                &terms.reserve_operation,
                terms.escrow.endpoint(),
                plan(terms.price),
                terms.escrow_fence.wire(),
            ),
            guards,
        );
        p.request = request;
        p.stage = Stage::Reserve;
        self.payment_runtime.pending = Some(p);
        vec![command]
    }
    fn paid_payout_status(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(request) = self.next_storage_request("paid-payout-status") else {
            return report(
                p.session,
                "Request sequence exhausted; original payout retained.",
            );
        };
        let command = Command::OperationStatus(OperationStatus {
            request: request.clone(),
            operation_id: p.payout.as_ref().expect("payout").operation.clone(),
        });
        p.request = request;
        p.stage = Stage::PayoutStatus;
        self.payment_runtime.pending = Some(p);
        vec![command]
    }
    fn paid_payout(&mut self, mut p: Pending) -> Vec<Command> {
        let terms = p.terms.as_ref().expect("terms");
        let payout = p.payout.as_ref().expect("original payout");
        let head = p.head.as_ref().expect("head");
        if head.gross.is_none()
            || head.loss.is_some()
            || head.state != State::PayoutIntent
            || payout.recipient_uuid != terms.recipient_entity_uuid
            || payout.storage_guards.is_empty()
            || payout.storage_guards.iter().any(|g| g.version == 0)
            || head.reservation.as_ref() != Some(&payout.reservation)
        {
            return report(
                p.session,
                "Original delivered escrow payout identity invalid; no payment.",
            );
        }
        let Some(version) = p.version.filter(|v| *v > 0) else {
            return report(
                p.session,
                "Original payout header fence unavailable; no payment.",
            );
        };
        let Some(request) = self.next_storage_request("paid-payout") else {
            return report(
                p.session,
                "Request sequence exhausted; original payout retained.",
            );
        };
        let source = terms.escrow.endpoint();
        let destination = InventoryEndpoint::NpcCarry(terms.recipient.clone());
        let mut guards: Vec<_> = payout
            .storage_guards
            .iter()
            .map(StorageGuard::wire)
            .collect();
        guards.push(types::PluginStorageVersionGuard {
            key: key(&terms.shipment),
            version,
        });
        guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        let command = with_expected_plugin_storage_versions(
            consume_inventory_reservation(
                &request,
                &payout.operation,
                0,
                Some(types::NpcExecutionPermit {
                    route_ref: payout.route_ref.clone(),
                    executor_epoch: payout.executor_epoch,
                }),
                &payout.reservation.reference,
                payout.reservation.revision,
                1,
                plan(terms.price),
                vec![inventories::OwnedItemTransfer {
                    source: source.clone(),
                    source_slot: payout.source_slot,
                    destination: destination.clone(),
                    destination_slot: payout.destination_slot,
                    count: terms.price,
                }],
                vec![
                    inventories::InventoryExpectedRevision {
                        endpoint: source,
                        fence: payout.source_fence.wire(),
                    },
                    inventories::InventoryExpectedRevision {
                        endpoint: destination,
                        fence: payout.destination_fence.wire(),
                    },
                ],
            ),
            guards,
        );
        p.request = request;
        p.stage = Stage::Payout;
        self.payment_runtime.pending = Some(p);
        vec![command]
    }
    fn paid_loaded(&mut self, mut p: Pending) -> Vec<Command> {
        let terms = p.terms.as_ref().expect("complete immutable terms");
        let h = p.head.as_ref().expect("head");
        let c = p.caravan.as_ref().expect("caravan");
        if terms.validate().is_err()
            || h.shipment != terms.shipment
            || h.buyer != terms.buyer
            || h.buyer_scope != terms.buyer_scope
            || h.recipient != terms.recipient
            || h.recipient_scope != terms.recipient_scope
            || h.recipient_entity_uuid != terms.recipient_entity_uuid
            || c.paid_mandate.as_deref() != Some(terms.shipment.as_str())
        {
            return report(
                p.session,
                "Immutable paid mandate identity mismatch; no effect.",
            );
        }
        if let Action::Status { player, .. } = &p.action {
            if player != &terms.buyer && player != &terms.seller {
                return report(p.session, "Actual buyer or seller required.");
            }
            return report(
                p.session,
                format!(
                    "Paid shipment {}: native={}",
                    terms.shipment,
                    serde_json::json!({"mandate":h,
                        "reserve_operation":terms.reserve_operation,"payout_operation":terms.payout_operation,
                        "price":terms.price,"quantity":terms.quantity,"recipient":terms.recipient,
                        "escrow_slot":terms.escrow_slot,"escrow_fence":{"revision":terms.escrow_fence.revision,"snapshot_hash":terms.escrow_fence.snapshot_hash},
                        "payout_attempt":h.payout_attempt})
                ),
            );
        }
        if h.state == State::Paid {
            return report(
                p.session,
                "Paid shipment already completed; original payment receipt retained.",
            );
        }
        if h.loss.is_some() {
            return report(
                p.session,
                "Original native physical loss receipt retained; no payout or source refund.",
            );
        }
        // Resolve original native operation first even if policy changed after it
        // was admitted. Reserve installs a hold, never delivers goods or money.
        if h.state == State::ReserveIntent {
            return self.paid_reserve_status(p);
        }
        if h.state == State::PayoutIntent {
            let Some(blob) = h.payout.as_ref() else {
                return report(p.session, "Original payout intent missing; no payment.");
            };
            let key = blob.key(0);
            p.chunks.clear();
            return self.paid_read(p, Stage::PayoutBlobRead, key);
        }
        if !terms.matches_caravan(c) && c.manifest.is_some() {
            return report(
                p.session,
                "Shipment full component/source/recipient contract mismatch; escrow retained.",
            );
        }
        if c.cargo_loss().is_some() {
            if h.gross.is_some() {
                return report(
                    p.session,
                    "Gross delivery predates carrier death; delivery remains recorded.",
                );
            }
            let carrier = c.carrier.clone();
            let Some(request) = self.next_storage_request("paid-death") else {
                return report(
                    p.session,
                    "Request sequence exhausted; native loss remains unacknowledged.",
                );
            };
            p.request = request.clone();
            p.stage = Stage::Death;
            self.payment_runtime.pending = Some(p);
            return vec![query_npc_controls(&request, &[carrier])];
        }
        if h.gross.is_some() {
            if c.gross_delivery().is_none()
                || c.arrival_rights_revision != Some(terms.authorized_registry_revision)
            {
                return report(
                    p.session,
                    "Delivery rights stale; original escrow retained without payout.",
                );
            }
            let recipient_current = p
                .registry
                .settlements
                .get(&terms.recipient_scope)
                .filter(|s| can_build_settlement(s, &terms.seller))
                .is_some_and(|s| {
                    s.residents.values().any(|r| {
                        r.handle == terms.recipient
                            && r.native_entity_uuid.as_deref()
                                == Some(terms.recipient_entity_uuid.as_str())
                    })
                });
            if !recipient_current
                || p.registry_version != Some(terms.authorized_registry_revision)
                || !current_binding(&p.registry, &terms.escrow, &terms.buyer)
                || !current_binding(&p.registry, &terms.destination, &terms.seller)
            {
                return report(
                    p.session,
                    "Original recipient/current shipment rights unavailable; escrow retained.",
                );
            }
            let endpoint = terms.escrow.endpoint();
            return self.paid_inventory(p, Stage::PayoutEscrow, endpoint);
        }
        let Some(gross) = c.gross_delivery() else {
            return report(
                p.session,
                "Escrow remains physical/reserved; no durable actual arrival/unload yet.",
            );
        };
        let rights_fresh = gross.executor_epoch == c.arrival_epoch.unwrap_or(0)
            && c.arrival_rights_revision == p.registry_version
            && p.registry_version == Some(terms.authorized_registry_revision)
            && current_binding(&p.registry, &terms.escrow, &terms.buyer)
            && current_binding(&p.registry, &terms.source, &terms.seller)
            && current_binding(&p.registry, &terms.destination, &terms.seller);
        let value = serde_json::json!({
            "gross":gross, "rights_fresh":rights_fresh,
            "delivery_rights_revision":c.arrival_rights_revision,
            "observed_registry_revision":p.registry_version,
            "mandated_registry_revision":terms.authorized_registry_revision
        });
        if !rights_fresh {
            p.head.as_mut().expect("head").blocked = Some(
                "Exact buyer/seller rights revision stale at actual delivery; no payout.".into(),
            );
        }
        self.paid_publish(p, &value, After::Delivery)
    }
    pub(super) fn paid_shipment_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.payment_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::Registry
                    | Stage::Caravans
                    | Stage::Head
                    | Stage::BlobRead
                    | Stage::BlobWriteRead
                    | Stage::PayoutBlobRead
            )
        {
            self.payment_runtime.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Paid storage unavailable; original intent/receipt retained.",
            ));
        };
        match p.stage {
            Stage::Registry => {
                p.registry_version = record.version;
                p.registry = match record.registry.cloned() {
                    Some(registry) => registry,
                    None => return Some(report(p.session, "Settlement registry unreadable.")),
                };
                Some(self.paid_read(p, Stage::Caravans, caravan_runtime::KEY.into()))
            }
            Stage::Caravans => {
                let store: Store = match record.value.as_deref() {
                    None => Store::default(),
                    Some(v) => match serde_json::from_str(v) {
                        Ok(s) => s,
                        Err(_) => return Some(report(p.session, "Caravan store unreadable.")),
                    },
                };
                let id = match &p.action {
                    Action::Create { id, .. } | Action::Status { id, .. } => Some(id.clone()),
                    Action::Wake => store
                        .contracts
                        .keys()
                        .filter(|id| store.contracts[*id].paid_mandate.is_some())
                        .find(|id| {
                            self.payment_runtime
                                .cursor
                                .as_ref()
                                .is_none_or(|cursor| *id > cursor)
                        })
                        .cloned()
                        .or_else(|| {
                            store
                                .contracts
                                .keys()
                                .find(|id| store.contracts[*id].paid_mandate.is_some())
                                .cloned()
                        }),
                };
                let Some(id) = id else {
                    return Some(report(p.session, "No paid shipment admitted."));
                };
                let Some(c) = store
                    .contracts
                    .get(&id)
                    .filter(|c| c.paid_mandate.as_deref() == Some(id.as_str()))
                    .cloned()
                else {
                    return Some(report(
                        p.session,
                        "Exact paid-caravan shipment required before mandate; no stock changed.",
                    ));
                };
                self.payment_runtime.cursor = Some(id.clone());
                p.id = Some(id.clone());
                p.caravan = Some(c);
                Some(self.paid_read(p, Stage::Head, key(&id)))
            }
            Stage::Head => {
                p.version = record.version;
                if let Some(value) = &record.value {
                    let Ok(h) = serde_json::from_str::<Mandate>(value) else {
                        return Some(report(
                            p.session,
                            "Paid mandate unreadable; no receipt fabricated.",
                        ));
                    };
                    if h.shipment != *p.id.as_ref().expect("id") || h.contract.fragments == 0 {
                        return Some(report(p.session, "Paid mandate corrupt; no native effect."));
                    }
                    if let Action::Create {
                        buyer,
                        scope,
                        building,
                        price,
                        ..
                    } = &p.action
                    {
                        if h.buyer != *buyer
                            || h.destination.scope != *scope
                            || h.destination.building != *building
                        {
                            return Some(report(
                                p.session,
                                "Paid mandate immutable conflicting replay.",
                            ));
                        }
                    }
                    let key = h.contract.key(0);
                    p.head = Some(h);
                    p.chunks.clear();
                    return Some(self.paid_read(p, Stage::BlobRead, key));
                }
                let Action::Create {
                    buyer,
                    scope,
                    building,
                    home,
                    resident,
                    ..
                } = &p.action
                else {
                    return Some(report(
                        p.session,
                        "Paid mandate not yet created; paid caravan remains blocked.",
                    ));
                };
                let c = p.caravan.as_ref().expect("caravan");
                let recipient = p
                    .registry
                    .settlements
                    .get(home)
                    .filter(|settlement| can_build_settlement(settlement, c.owner()))
                    .and_then(|settlement| settlement.residents.get(resident));
                if recipient.is_none_or(|recipient| recipient.native_entity_uuid.is_none()) {
                    return Some(report(p.session, "Original registered native payment recipient and seller rights required; no actor substituted."));
                }
                if c.loaded
                    || c.reservation.is_some()
                    || c.intent.is_some()
                    || !c.receipts.is_empty()
                    || c.terminal()
                    || c.destination.scope != *scope
                    || c.destination.building != *building
                    || p.registry_version.is_none()
                    || !current_binding(&p.registry, &c.destination, buyer)
                    || !current_binding(&p.registry, &c.source, c.owner())
                    || !current_binding(&p.registry, &c.destination, c.owner())
                {
                    return Some(report(p.session,"Fresh buyer escrow/source seller rights and unstarted paid shipment required; no money changed."));
                }
                let endpoint = c.source.endpoint();
                Some(self.paid_inventory(p, Stage::Commodity, endpoint))
            }
            Stage::BlobRead => {
                let Some(value) = &record.value else {
                    return Some(report(
                        p.session,
                        "Immutable paid component fragment missing; no native effect.",
                    ));
                };
                p.chunks.push((*value).to_owned());
                let blob = &p.head.as_ref().expect("head").contract;
                if p.chunks.len() < blob.fragments {
                    let key = blob.key(p.chunks.len());
                    return Some(self.paid_read(p, Stage::BlobRead, key));
                }
                p.terms = match blob.decode::<Terms>(&p.chunks) {
                    Ok(t) => Some(t),
                    Err(reason) => return Some(report(p.session, reason)),
                };
                if let Action::Create {
                    price,
                    home,
                    resident,
                    ..
                } = &p.action
                {
                    let terms = p.terms.as_ref().expect("terms");
                    let recipient = p
                        .registry
                        .settlements
                        .get(home)
                        .and_then(|s| s.residents.get(resident))
                        .map(|r| r.handle.as_str());
                    if *price != terms.price || recipient != Some(terms.recipient.as_str()) {
                        return Some(report(
                        p.session,
                        "Negotiated price/recipient replay conflict; original escrow unchanged.",
                    ));
                    }
                }
                Some(self.paid_loaded(p))
            }
            Stage::PayoutBlobRead => {
                let Some(value) = &record.value else {
                    return Some(report(
                        p.session,
                        "Original payout fragment missing; no payment.",
                    ));
                };
                p.chunks.push((*value).to_owned());
                let blob = p
                    .head
                    .as_ref()
                    .expect("head")
                    .payout
                    .as_ref()
                    .expect("payout blob");
                if p.chunks.len() < blob.fragments {
                    let key = blob.key(p.chunks.len());
                    return Some(self.paid_read(p, Stage::PayoutBlobRead, key));
                }
                p.payout = match blob.decode::<Payout>(&p.chunks) {
                    Ok(payout) => Some(payout),
                    Err(reason) => return Some(report(p.session, reason)),
                };
                Some(self.paid_payout_status(p))
            }
            Stage::BlobWriteRead => {
                let publication = p.publication.as_ref().expect("publication");
                let value = &publication.chunks[publication.cursor];
                if let Some(existing) = &record.value {
                    return Some(if existing == value {
                        self.paid_publish_next(p)
                    } else {
                        report(
                            p.session,
                            "Immutable native receipt blob collision; no ACK.",
                        )
                    });
                }
                let key = publication.blob.key(publication.cursor);
                let value = value.clone();
                let Some(request) = self.next_storage_request("paid-fragment") else {
                    return Some(report(p.session, "Request sequence exhausted."));
                };
                let command = storage_cas(&request, &key, None, value);
                p.request = request;
                p.stage = Stage::BlobWriteCas;
                self.payment_runtime.pending = Some(p);
                Some(vec![command])
            }
            _ => unreachable!(),
        }
    }
    pub(super) fn paid_shipment_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let mut p = self.payment_runtime.pending.take()?;
        if p.request != answer.request() || !matches!(p.stage, Stage::HeadCas | Stage::BlobWriteCas)
        {
            self.payment_runtime.pending = Some(p);
            return None;
        }
        if p.stage == Stage::BlobWriteCas {
            return Some(match answer.outcome() {
                StorageCasOutcome::Committed(_) => self.paid_publish_next(p),
                _ => {
                    let publication = p.publication.as_ref().expect("publication");
                    let key = publication.blob.key(publication.cursor);
                    self.paid_read(p, Stage::BlobWriteRead, key)
                }
            });
        }
        let StorageCasOutcome::Committed(version) = answer.outcome() else {
            return Some(report(
                p.session,
                "Paid header CAS refused; no new native effect. Replay original IDs.",
            ));
        };
        p.version = Some(*version);
        if p.head.as_ref().expect("head").state == State::ReserveIntent {
            return Some(self.paid_reserve_status(p));
        }
        if p.head.as_ref().expect("head").state == State::PayoutIntent {
            return Some(self.paid_payout_status(p));
        }
        Some(report(
            p.session,
            format!(
                "Paid shipment {} committed: native={}",
                p.id.as_deref().unwrap_or(""),
                serde_json::to_string(p.head.as_ref().expect("head")).expect("head JSON")
            ),
        ))
    }
    pub(super) fn paid_shipment_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.payment_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::Commodity
                    | Stage::Escrow
                    | Stage::ReserveStatus
                    | Stage::Reserve
                    | Stage::Death
                    | Stage::PayoutEscrow
                    | Stage::Recipient
                    | Stage::PayoutStatus
                    | Stage::Payout
            )
        {
            self.payment_runtime.pending = Some(p);
            return None;
        }
        if matches!(p.stage, Stage::PayoutStatus | Stage::Payout)
            && answer.operation_id.as_deref() != p.payout.as_ref().map(|p| p.operation.as_str())
        {
            self.payment_runtime.pending = Some(p);
            return Some(Vec::new());
        }
        if p.stage == Stage::PayoutStatus {
            if let OperationOutcome::Refused(refused) = answer.outcome() {
                if matches!(refused.reason, OperationFailure::NotFound) {
                    return Some(self.paid_payout(p));
                }
            }
            p.stage = Stage::Payout;
        }
        if matches!(p.stage, Stage::ReserveStatus | Stage::Reserve)
            && answer.operation_id.as_deref()
                != p.terms.as_ref().map(|t| t.reserve_operation.as_str())
        {
            self.payment_runtime.pending = Some(p);
            return Some(Vec::new());
        }
        if p.stage == Stage::ReserveStatus {
            if let OperationOutcome::Refused(refused) = answer.outcome() {
                if matches!(refused.reason, OperationFailure::NotFound) {
                    return Some(self.paid_reserve(p));
                }
            }
            p.stage = Stage::Reserve;
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            if matches!(answer.outcome(), OperationOutcome::Refused(refused)
                if matches!(refused.reason, OperationFailure::RuntimeUnavailable))
            {
                return Some(report(p.session, "Payout decision unavailable; replay original operation before any new attempt."));
            }
            if p.stage == Stage::Payout {
                // A known refusal may be replanned; unknown decisions only replay the original.
                let h = p.head.as_mut().expect("head");
                h.state = State::DeliveryRecorded;
                h.payout = None;
                return Some(self.paid_head(p));
            }
            return Some(report(
                p.session,
                format!("Paid native refusal {:?}; escrow retained.", answer.outcome),
            ));
        };
        match p.stage {
            Stage::PayoutEscrow | Stage::Recipient => {
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Full native payout inventory snapshot required; escrow retained.",
                    ));
                };
                let terms = p.terms.as_ref().expect("terms");
                let expected = if p.stage == Stage::PayoutEscrow {
                    terms.escrow.endpoint()
                } else {
                    InventoryEndpoint::NpcCarry(terms.recipient.clone())
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &expected) {
                    return Some(report(
                        p.session,
                        "Original payout inventory endpoint mismatch; escrow retained.",
                    ));
                }
                if p.stage == Stage::PayoutEscrow {
                    p.escrow = Some(snapshot.clone());
                    let endpoint = InventoryEndpoint::NpcCarry(terms.recipient.clone());
                    return Some(self.paid_inventory(p, Stage::Recipient, endpoint));
                }
                let escrow = p.escrow.as_ref().expect("escrow");
                let Some(source_slot) = escrow
                    .slots
                    .iter()
                    .find(|s| {
                        s.slot == terms.escrow_slot
                            && s.item
                                .as_ref()
                                .is_some_and(|i| i.count >= terms.price && terms.emerald.matches(i))
                    })
                    .map(|s| s.slot)
                else {
                    return Some(report(
                        p.session,
                        "Original physical escrow stack unavailable; no payment minted.",
                    ));
                };
                let Some(destination_slot) = snapshot
                    .slots
                    .iter()
                    .find(|s| s.item.is_none())
                    .map(|s| s.slot)
                else {
                    return Some(report(
                        p.session,
                        "Original payment recipient carry full; escrow remains reserved.",
                    ));
                };
                let c = p.caravan.as_ref().expect("caravan");
                let Some(route_ref) = c.route_ref.clone() else {
                    return Some(report(
                        p.session,
                        "Original admitted route missing; no payout.",
                    ));
                };
                let h = p.head.as_mut().expect("head");
                let Some(reservation) = h.reservation.clone() else {
                    return Some(report(
                        p.session,
                        "Original escrow reservation missing; no payout.",
                    ));
                };
                let Some(attempt) = h.payout_attempt.checked_add(1) else {
                    return Some(report(
                        p.session,
                        "Payout sequence exhausted; escrow retained.",
                    ));
                };
                h.payout_attempt = attempt;
                let payout = Payout {
                    operation: format!("{}_{attempt:x}", terms.payout_operation),
                    source_slot,
                    destination_slot,
                    source_fence: hire::Fence::from_snapshot(escrow),
                    destination_fence: hire::Fence::from_snapshot(snapshot),
                    recipient_uuid: terms.recipient_entity_uuid.clone(),
                    route_ref,
                    executor_epoch: c.party.executor_epoch,
                    reservation,
                    storage_guards: terms.storage_guards.clone(),
                };
                p.payout = Some(payout.clone());
                Some(self.paid_publish(p, &payout, After::PayoutIntent))
            }
            Stage::Payout => {
                let OperationPayload::OwnedInventory(InventoryResult::ReservedTransfer(receipt)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Exact native reserved payout receipt required; original intent retained.",
                    ));
                };
                let terms = p.terms.as_ref().expect("terms");
                let payout = p.payout.as_ref().expect("original payout");
                let destination = InventoryEndpoint::NpcCarry(terms.recipient.clone());
                if receipt.reservation.reservation_ref != payout.reservation.reference
                    || receipt.reservation.resource_plan_hash != payout.reservation.plan_hash
                    || receipt.reservation.receipt_watermark != 1
                    || receipt.reservation.released
                    || receipt.reservation.quantities.len() != 1
                    || !receipt.reservation.quantities.iter().any(|q| {
                        q.resource_id == "minecraft:emerald"
                            && q.consumed == u64::from(terms.price)
                            && q.remaining == 0
                            && q.reserved == u64::from(terms.price)
                    })
                    || receipt.inventories.len() != 2
                    || !receipt
                        .inventories
                        .iter()
                        .any(|s| same_inventory_endpoint(&s.endpoint, &terms.escrow.endpoint()))
                    || !receipt.inventories.iter().any(|s| {
                        same_inventory_endpoint(&s.endpoint, &destination)
                            && s.slots.iter().any(|slot| {
                                slot.slot == payout.destination_slot
                                    && slot.item.as_ref().is_some_and(|i| {
                                        i.count == terms.price && terms.emerald.matches(i)
                                    })
                            })
                    })
                {
                    return Some(report(
                        p.session,
                        "Native reserved payout conservation/recipient mismatch; no ACK.",
                    ));
                }
                let inventories: Vec<_> = receipt.inventories.iter().map(|s| {
                    let slots: Vec<_> = s.slots.iter().map(|slot| serde_json::json!({
                        "slot":slot.slot, "item":slot.item.as_ref().map(|i|
                            serde_json::json!({"count":i.count,"stack":Stack::from_native(i)}))
                    })).collect();
                    serde_json::json!({"endpoint":if same_inventory_endpoint(&s.endpoint,&destination) {
                        serde_json::json!({"npc_carry":terms.recipient})
                    } else {serde_json::json!({"container":terms.escrow})},
                        "revision":s.fence.revision,"hash":s.fence.snapshot_hash,"slots":slots})
                }).collect();
                let value = serde_json::json!({
                    "shipment":terms.shipment,"operation":payout.operation,"recipient":terms.recipient,
                    "recipient_uuid":terms.recipient_entity_uuid,"price":terms.price,
                    "native_revision":committed.revision,"payout":payout,
                    "reservation":{"reference":receipt.reservation.reservation_ref,
                        "hash":receipt.reservation.resource_plan_hash,"watermark":receipt.reservation.receipt_watermark},
                    "inventories":inventories,"gross":p.head.as_ref().expect("head").gross,
                });
                Some(self.paid_publish(p, &value, After::Payment))
            }
            Stage::Commodity | Stage::Escrow => {
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Actual full native inventory snapshot required.",
                    ));
                };
                let c = p.caravan.as_ref().expect("caravan");
                let expected = if p.stage == Stage::Commodity {
                    c.source.endpoint()
                } else {
                    c.destination.endpoint()
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &expected) {
                    return Some(report(p.session, "Native inventory endpoint mismatch."));
                }
                if p.stage == Stage::Commodity {
                    let Some(item) = snapshot
                        .slots
                        .iter()
                        .filter_map(|s| s.item.as_ref())
                        .find(|i| i.resource_id == c.item && i.count >= c.quantity)
                    else {
                        return Some(report(
                            p.session,
                            "Exact actual source commodity stack required; no copied stock.",
                        ));
                    };
                    p.commodity = Some(Stack::from_native(item));
                    let endpoint = c.destination.endpoint();
                    return Some(self.paid_inventory(p, Stage::Escrow, endpoint));
                }
                let Action::Create {
                    buyer,
                    scope,
                    home,
                    resident,
                    price,
                    ..
                } = &p.action
                else {
                    unreachable!()
                };
                let Some(slot) = snapshot.slots.iter().find(|s| {
                    s.item
                        .as_ref()
                        .is_some_and(|i| i.resource_id == "minecraft:emerald" && i.count >= *price)
                }) else {
                    return Some(report(
                        p.session,
                        "Actual buyer physical emerald escrow missing; no currency issued.",
                    ));
                };
                let Some(recipient) = p
                    .registry
                    .settlements
                    .get(home)
                    .filter(|settlement| can_build_settlement(settlement, c.owner()))
                    .and_then(|settlement| settlement.residents.get(resident))
                    .filter(|recipient| recipient.native_entity_uuid.is_some())
                else {
                    return Some(report(
                        p.session,
                        "Original native payment recipient unavailable; no substitute or teleport.",
                    ));
                };
                let scopes: std::collections::BTreeSet<_> = [
                    scope.as_str(),
                    c.source.scope.as_str(),
                    c.destination.scope.as_str(),
                    home.as_str(),
                ]
                .into_iter()
                .collect();
                let mut storage_guards = Vec::with_capacity(scopes.len());
                for scope in scopes {
                    let Ok((key, Some(version))) =
                        self.registry_scope_fence(p.registry_version, scope)
                    else {
                        return Some(report(
                            p.session,
                            "Actual settlement scope version required; no escrow mandate.",
                        ));
                    };
                    if version == 0 {
                        return Some(report(
                            p.session,
                            "Positive actual settlement scope version required; no escrow mandate.",
                        ));
                    }
                    storage_guards.push(StorageGuard { key, version });
                }
                storage_guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
                let terms = Terms {
                    shipment: c.party.id.clone(),
                    buyer: buyer.clone(),
                    buyer_scope: scope.clone(),
                    authorized_registry_revision: p.registry_version.expect("registry revision"),
                    seller: c.owner().into(),
                    source: c.source.clone(),
                    destination: c.destination.clone(),
                    escrow: c.destination.clone(),
                    carrier: c.carrier.clone(),
                    recipient: recipient.handle.clone(),
                    recipient_scope: home.clone(),
                    recipient_entity_uuid: recipient
                        .native_entity_uuid
                        .clone()
                        .expect("actual native recipient UUID"),
                    quantity: c.quantity,
                    commodity: p.commodity.clone().expect("component contract"),
                    price: *price,
                    emerald: Stack::from_native(slot.item.as_ref().expect("emerald")),
                    escrow_slot: slot.slot,
                    escrow_fence: hire::Fence::from_snapshot(snapshot),
                    reserve_operation: format!("pm_{}_escrow", c.party.id),
                    payout_operation: format!("pm_{}_payout", c.party.id),
                    storage_guards,
                };
                if let Err(reason) = terms.validate() {
                    return Some(report(p.session, reason));
                }
                let (blob, _) = Blob::encode(&terms).expect("terms serializable");
                let (commodity_blob, _) =
                    Blob::encode(&terms.commodity).expect("components serializable");
                p.head = Some(Mandate {
                    shipment: terms.shipment.clone(),
                    buyer: terms.buyer.clone(),
                    buyer_scope: terms.buyer_scope.clone(),
                    registry_revision: terms.authorized_registry_revision,
                    seller: terms.seller.clone(),
                    source: terms.source.clone(),
                    destination: terms.destination.clone(),
                    carrier: terms.carrier.clone(),
                    recipient: terms.recipient.clone(),
                    recipient_scope: terms.recipient_scope.clone(),
                    recipient_entity_uuid: terms.recipient_entity_uuid.clone(),
                    quantity: terms.quantity,
                    commodity_digest: commodity_blob.digest,
                    contract: blob,
                    state: State::ReserveIntent,
                    reservation: None,
                    gross: None,
                    payout: None,
                    payment_receipt: None,
                    loss: None,
                    blocked: None,
                    payout_attempt: 0,
                });
                p.terms = Some(terms.clone());
                Some(self.paid_publish(p, &terms, After::Reserve))
            }
            Stage::Reserve => {
                let OperationPayload::OwnedInventory(InventoryResult::Reservation(r)) =
                    &committed.payload
                else {
                    return Some(report(p.session,"Exact native escrow reservation receipt required; original operation retained."));
                };
                let terms = p.terms.as_ref().expect("terms");
                if !same_inventory_endpoint(&r.endpoint, &terms.escrow.endpoint())
                    || r.released
                    || r.quantities.len() != 1
                    || !r.quantities.iter().any(|q| {
                        q.resource_id == "minecraft:emerald"
                            && q.reserved == u64::from(terms.price)
                            && q.remaining == u64::from(terms.price)
                            && q.consumed == 0
                    })
                {
                    return Some(report(
                        p.session,
                        "Native escrow price/full plan mismatch; no load authorized.",
                    ));
                }
                let h = p.head.as_mut().expect("head");
                h.reservation = Some(Reservation {
                    reference: r.reservation_ref.clone(),
                    revision: committed.revision,
                    plan_hash: r.resource_plan_hash.clone(),
                });
                h.state = State::Escrowed;
                Some(self.paid_head(p))
            }
            Stage::Death => {
                let c = p.caravan.as_ref().expect("caravan");
                let Some(loss) = c.cargo_loss() else {
                    return Some(report(
                        p.session,
                        "Original committed physical loss citation required.",
                    ));
                };
                let death = match &committed.payload {
                    OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) => page
                        .npcs
                        .iter()
                        .find(|n| n.handle == c.carrier)
                        .and_then(|n| n.death_cargo.as_ref()),
                    _ => None,
                };
                let Some(death) = death.filter(|d| {
                    d.handle == loss.carrier
                        && d.entity_uuid == loss.entity_uuid
                        && d.receipt_ref == loss.death_ref
                        && d.executor_epoch == loss.executor_epoch
                }) else {
                    return Some(report(p.session, "Canonical native death identity/epoch/receipt mismatch; no inferred loss or refund."));
                };
                if c.loaded {
                    let count = death
                        .loot
                        .iter()
                        .filter(|l| {
                            l.inventory == inventories::NpcDeathInventory::Carry
                                && Some(l.slot) == c.carry_slot.map(u16::from)
                                && p.terms.as_ref().expect("terms").commodity.matches(&l.item)
                        })
                        .try_fold(0u32, |count, l| count.checked_add(l.item.count));
                    if count != Some(c.quantity) {
                        return Some(report(p.session, "Native physical death loot must conserve the exact withdrawn full-component cargo; no source refund."));
                    }
                }
                let loot: Vec<_> = death.loot.iter().map(|l|serde_json::json!({
                    "inventory":format!("{:?}",l.inventory), "slot":l.slot,
                    "uuid":l.entity_uuid, "count":l.item.count, "stack":Stack::from_native(&l.item)
                })).collect();
                let value = serde_json::json!({
                    "shipment":c.party.id, "receipt":death.receipt_ref,
                    "carrier":death.handle, "uuid":death.entity_uuid, "epoch":death.executor_epoch,
                    "dimension":death.dimension, "position":{"x":death.position.x,"y":death.position.y,"z":death.position.z},
                    "death_tick":death.death_tick, "inventory_revision":death.source_inventory_revision,
                    "world_decision":death.world_decision_id.to_string(), "loot":loot
                });
                Some(self.paid_publish(p, &value, After::Loss))
            }
            _ => unreachable!(),
        }
    }
}
