//! Explicit quotes exchange physical inventory stacks atomically; no virtual currency.
use super::commerce::{Ledger, Outcome, Quote};
use super::*;
use solaris_plugin_sdk::{
    inventories, query_owned_inventory, transfer_owned_items, with_expected_plugin_storage_versions,
};
pub(super) const KEY: &str = "settlement-trades-v1";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Store {
    pub(super) ledger: Ledger,
    pub(super) plans: BTreeMap<String, Plan>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Plan {
    pub(super) scope: String,
    pub(super) resident: String,
    seller_slot: u8,
    buyer_slot: u8,
    seller_receive: u8,
    buyer_receive: u8,
    pub(super) accepted: bool,
    #[serde(default)]
    storage_guard: Option<caravan::StorageGuard>,
}
#[derive(Clone)]
enum Action {
    Quote {
        scope: String,
        id: String,
        resident: String,
        item: String,
        quantity: u32,
        payment: String,
        price: u32,
        player: String,
    },
    Accept {
        id: String,
        player: String,
    },
    Status {
        id: String,
        player: String,
    },
    Recover,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Store,
    Seller,
    Buyer,
    QuoteWrite,
    AcceptWrite,
    Native,
    ReceiptRead,
    ReceiptWrite,
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
    seller: Option<inventories::OwnedInventorySnapshot>,
    buyer: Option<inventories::OwnedInventorySnapshot>,
    id: Option<String>,
    outcome: Option<Outcome>,
    native_quote: Option<Quote>,
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} trade {text}"));
    session
        .map(|session| vec![message_session(session, text)])
        .unwrap_or_default()
}
impl Settlements {
    pub(super) fn handle_trade_command(
        &mut self,
        invoked: &CommandInvoked,
    ) -> Option<Vec<Command>> {
        if invoked.name != "settlement" {
            return None;
        }
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if !matches!(
            args.first(),
            Some(&"trade-quote") | Some(&"trade-accept") | Some(&"trade-status")
        ) {
            return None;
        }
        if self.commerce_trade.pending.is_some() {
            return Some(report(
                Some(invoked.session),
                "Trade busy; no inventory changed.",
            ));
        }
        let action = match args.as_slice() {
            ["trade-quote",scope,id,resident,item,quantity,payment,price] => {
                let Some((quantity,price))=quantity.parse::<u32>().ok().filter(|v|*v>0).zip(price.parse::<u32>().ok().filter(|v|*v>0)) else{return Some(report(Some(invoked.session),"Explicit positive item quantity and physical quoted price required."));};
                Action::Quote { scope:(*scope).into(),id:(*id).into(),resident:(*resident).into(),item:(*item).into(),quantity,payment:(*payment).into(),price,player:invoked.player.clone() }
            }
            ["trade-accept",id] => Action::Accept{id:(*id).into(),player:invoked.player.clone()},
            ["trade-status",id] => Action::Status{id:(*id).into(),player:invoked.player.clone()},
            _=>return Some(report(Some(invoked.session),"Usage: trade-quote <settlement> <id> <resident> <item> <quantity> <physical-payment-item> <quoted-price>; trade-accept/status <id>"))
        };
        Some(self.trade_begin(action, Some(invoked.session)))
    }
    pub(super) fn trade_init(&mut self) -> Vec<Command> {
        self.commerce_trade.pending = None;
        self.trade_begin(Action::Recover, None)
    }
    fn trade_begin(&mut self, action: Action, session: Option<u64>) -> Vec<Command> {
        let Some(request) = self.next_storage_request("trade-registry") else {
            return report(session, "Request sequence exhausted.");
        };
        self.commerce_trade.pending = Some(Pending {
            request: request.clone(),
            session,
            action,
            stage: Stage::Registry,
            registry: SettlementRegistry::default(),
            registry_version: None,
            store: Store::default(),
            version: None,
            seller: None,
            buyer: None,
            id: None,
            outcome: None,
            native_quote: None,
        });
        self.registry_read(&request)
    }
    fn trade_read(&mut self, mut p: Pending, stage: Stage) -> Vec<Command> {
        let Some(request) = self.next_storage_request("trade-read") else {
            return report(
                p.session,
                "Request sequence exhausted; accepted intent retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_trade.pending = Some(p);
        vec![storage_get(&request, KEY)]
    }
    fn trade_write(&mut self, mut p: Pending, stage: Stage) -> Vec<Command> {
        let Ok(value) = serde_json::to_string(&p.store) else {
            return report(p.session, "Trade serialization refused.");
        };
        let Some(request) = self.next_storage_request("trade-write") else {
            return report(
                p.session,
                "Request sequence exhausted; accepted intent retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        let command = storage_cas(&request, KEY, p.version, value);
        self.commerce_trade.pending = Some(p);
        vec![command]
    }
    fn trade_query(
        &mut self,
        mut p: Pending,
        stage: Stage,
        endpoint: InventoryEndpoint,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("trade-inventory") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_trade.pending = Some(p);
        vec![query_owned_inventory(&request, endpoint, None)]
    }
    fn trade_native(&mut self, mut p: Pending) -> Vec<Command> {
        let id = p.id.as_ref().expect("accepted quote");
        let trade = &p.store.ledger.trades[id];
        let plan = &p.store.plans[id];
        let Some(guard) = plan.storage_guard.as_ref().filter(|g| g.version > 0) else {
            return report(
                p.session,
                "Accepted trade lacks original rights fence; intent retained without exchange.",
            );
        };
        let Some(seller) = warehouse_endpoint(&trade.quote.seller) else {
            return report(
                p.session,
                "Accepted treasury endpoint invalid; intent retained.",
            );
        };
        let buyer = InventoryEndpoint::NpcCarry(trade.quote.buyer.clone());
        let Some(request) = self.next_storage_request("trade-transfer") else {
            return report(
                p.session,
                "Request sequence exhausted; accepted intent retained.",
            );
        };
        let operation = format!("settlement-trade:{id}");
        let command = with_expected_plugin_storage_versions(
            transfer_owned_items(
                &request,
                &operation,
                0,
                None,
                vec![
                    inventories::OwnedItemTransfer {
                        source: seller.clone(),
                        source_slot: plan.seller_slot,
                        destination: buyer.clone(),
                        destination_slot: plan.buyer_receive,
                        count: trade.quote.quantity,
                    },
                    inventories::OwnedItemTransfer {
                        source: buyer.clone(),
                        source_slot: plan.buyer_slot,
                        destination: seller.clone(),
                        destination_slot: plan.seller_receive,
                        count: trade.quote.payment_quantity,
                    },
                ],
                vec![
                    inventories::InventoryExpectedRevision {
                        endpoint: seller,
                        fence: inventories::InventoryFence {
                            revision: trade.quote.seller_revision,
                            snapshot_hash: trade.quote.source_hash.clone(),
                        },
                    },
                    inventories::InventoryExpectedRevision {
                        endpoint: buyer,
                        fence: inventories::InventoryFence {
                            revision: trade.quote.buyer_revision,
                            snapshot_hash: trade.quote.buyer_hash.clone(),
                        },
                    },
                ],
            ),
            vec![guard.wire()],
        );
        p.native_quote = Some(trade.quote.clone());
        p.request = request;
        p.stage = Stage::Native;
        self.commerce_trade.pending = Some(p);
        vec![command]
    }
    pub(super) fn trade_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_trade.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::Registry | Stage::Store | Stage::ReceiptRead)
        {
            self.commerce_trade.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Storage unavailable; accepted trade intent retained.",
            ));
        };
        if p.stage == Stage::Registry {
            p.registry_version = record.version;
            p.registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => return Some(report(p.session, "Settlement store unreadable.")),
            };
            return Some(self.trade_read(p, Stage::Store));
        }
        p.store = match record.value.as_deref() {
            None => Store::default(),
            Some(value) => match serde_json::from_str(value) {
                Ok(value) => value,
                Err(_) => {
                    return Some(report(
                        p.session,
                        "Trade store unreadable; no currency created.",
                    ))
                }
            },
        };
        p.version = record.version;
        if p.stage == Stage::ReceiptRead {
            let quote = p.native_quote.as_ref().expect("accepted native quote");
            let outcome = p.outcome.clone().expect("native outcome");
            if let Err(reason) = p.store.ledger.finish_trade(quote, outcome) {
                return Some(report(
                    p.session,
                    format!("Trade terminal receipt conflict: {reason:?}; no ledger overwrite."),
                ));
            }
            return Some(self.trade_write(p, Stage::ReceiptWrite));
        }
        // Replay only explicitly accepted durable quote IDs; quotes alone never execute.
        if let Some(id) = p
            .store
            .plans
            .iter()
            .find(|(id, plan)| {
                plan.accepted
                    && p.store
                        .ledger
                        .trades
                        .get(*id)
                        .is_some_and(|t| t.outcome == Outcome::Pending)
            })
            .map(|(id, _)| id.clone())
        {
            p.id = Some(id);
            return Some(self.trade_native(p));
        }
        match p.action.clone() {
            Action::Quote {
                scope,
                id,
                resident,
                item,
                quantity,
                payment,
                price,
                player,
            } => {
                if id.is_empty() {
                    return Some(report(p.session, "Stable quote id required."));
                }
                if let Some(t) = p.store.ledger.trades.get(&id) {
                    return Some(report(
                        p.session,
                        if t.quote.owner == player {
                            format!("Quote {id} already recorded; {:?}; original price and fences unchanged",t.outcome)
                        } else {
                            "Quote id unavailable.".into()
                        },
                    ));
                }
                let Some(s) = p
                    .registry
                    .settlements
                    .get(&scope)
                    .filter(|s| can_build_settlement(s, &player))
                else {
                    return Some(report(
                        p.session,
                        "Trade refused: settlement rights required.",
                    ));
                };
                let Some(r) = s.residents.get(&resident).filter(|r| r.role.is_none()) else {
                    return Some(report(
                        p.session,
                        "Trade refused: civilian counterparty required.",
                    ));
                };
                let Some(w) = self
                    .warehouse
                    .as_ref()
                    .filter(|w| warehouse_belongs_to(s, w))
                else {
                    return Some(report(
                        p.session,
                        "Trade requires settlement-bound physical treasury.",
                    ));
                };
                let Some(endpoint) = warehouse_endpoint(&w.handle) else {
                    return Some(report(p.session, "Treasury endpoint invalid."));
                };
                // The quote does not reserve or create inventory. Native fences admit the final exchange.
                let quote = Quote {
                    id: id.clone(),
                    owner: player,
                    seller: w.handle.clone(),
                    buyer: r.handle.clone(),
                    item,
                    quantity,
                    payment_item: payment,
                    payment_quantity: price,
                    owner_revision: p.store.ledger.revision,
                    seller_revision: 0,
                    buyer_revision: 0,
                    source_hash: String::new(),
                    buyer_hash: String::new(),
                };
                p.store.ledger.trades.insert(
                    id.clone(),
                    commerce::Trade {
                        quote,
                        outcome: Outcome::Pending,
                    },
                );
                p.store.plans.insert(
                    id.clone(),
                    Plan {
                        scope,
                        resident,
                        seller_slot: 0,
                        buyer_slot: 0,
                        seller_receive: 0,
                        buyer_receive: 0,
                        accepted: false,
                        storage_guard: None,
                    },
                );
                p.id = Some(id);
                Some(self.trade_query(p, Stage::Seller, endpoint))
            }
            Action::Accept { id, player } => {
                let Some(trade) = p
                    .store
                    .ledger
                    .trades
                    .get(&id)
                    .filter(|t| t.quote.owner == player)
                else {
                    return Some(report(p.session, "Quote owner required."));
                };
                if trade.outcome != Outcome::Pending {
                    return Some(report(
                        p.session,
                        format!(
                            "Trade {id} terminal: {:?}; no replayed payment",
                            trade.outcome
                        ),
                    ));
                }
                let Some(plan) = p.store.plans.get(&id) else {
                    return Some(report(p.session, "Quote plan missing; no exchange."));
                };
                let authorized = p.registry.settlements.get(&plan.scope).is_some_and(|s| {
                    can_build_settlement(s, &player)
                        && s.residents
                            .get(&plan.resident)
                            .is_some_and(|r| r.handle == trade.quote.buyer && r.role.is_none())
                        && self.warehouse.as_ref().is_some_and(|w| {
                            w.handle == trade.quote.seller && warehouse_belongs_to(s, w)
                        })
                });
                if !authorized {
                    return Some(report(
                        p.session,
                        "Quote ownership/counterparty revoked; no exchange.",
                    ));
                }
                // A fresh physical read detects stale quotes before accepting an intent.
                let Some(endpoint) = warehouse_endpoint(&trade.quote.seller) else {
                    return Some(report(p.session, "Treasury endpoint invalid."));
                };
                p.id = Some(id);
                Some(self.trade_query(p, Stage::Seller, endpoint))
            }
            Action::Status { id, player } => Some(
                match p
                    .store
                    .ledger
                    .trades
                    .get(&id)
                    .filter(|t| t.quote.owner == player)
                {
                    Some(t) => report(
                        p.session,
                        format!(
                            "Quote {id}: {} {} for {} {}; {:?}; physical endpoint fences {} / {}",
                            t.quote.quantity,
                            t.quote.item,
                            t.quote.payment_quantity,
                            t.quote.payment_item,
                            t.outcome,
                            t.quote.seller_revision,
                            t.quote.buyer_revision
                        ),
                    ),
                    None => report(p.session, "Quote owner required."),
                },
            ),
            Action::Recover => Some(vec![]),
        }
    }
    pub(super) fn trade_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_trade.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::Seller | Stage::Buyer | Stage::Native)
        {
            self.commerce_trade.pending = Some(p);
            return None;
        }
        let id = p.id.as_ref().expect("quote selected").clone();
        if p.stage == Stage::Native {
            let operation = format!("settlement-trade:{id}");
            if answer.operation_id.as_deref() != Some(operation.as_str()) {
                self.commerce_trade.pending = Some(p);
                return Some(vec![]);
            }
            if matches!(answer.outcome(), OperationOutcome::Refused(refused)
                if matches!(refused.reason, OperationFailure::RuntimeUnavailable))
            {
                return Some(report(
                    p.session,
                    "Trade decision unavailable; accepted original intent retained for replay.",
                ));
            }
            p.outcome = Some(match answer.outcome() {
                OperationOutcome::Refused(_) => Outcome::Refused,
                OperationOutcome::Committed(committed) => {
                    let OperationPayload::OwnedInventory(InventoryResult::Transfer(fences)) =
                        &committed.payload
                    else {
                        return Some(report(
                            p.session,
                            "Unexpected native receipt; accepted trade intent retained.",
                        ));
                    };
                    let q = &p.store.ledger.trades[&id].quote;
                    let Some(seller) = warehouse_endpoint(&q.seller) else {
                        return Some(report(
                            p.session,
                            "Receipt treasury endpoint invalid; accepted intent retained.",
                        ));
                    };
                    let buyer = InventoryEndpoint::NpcCarry(q.buyer.clone());
                    if fences.len() != 2
                        || !fences
                            .iter()
                            .any(|f| same_inventory_endpoint(&f.endpoint, &seller))
                        || !fences
                            .iter()
                            .any(|f| same_inventory_endpoint(&f.endpoint, &buyer))
                    {
                        return Some(report(p.session,"Native exchange receipt omitted endpoint fences; accepted intent retained."));
                    }
                    Outcome::Committed
                }
            });
            return Some(self.trade_read(p, Stage::ReceiptRead));
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            return Some(report(
                p.session,
                "Quoted inventory unavailable; no exchange.",
            ));
        };
        let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
            &committed.payload
        else {
            return Some(report(
                p.session,
                "Inventory response mismatch; no exchange.",
            ));
        };
        let q = &p.store.ledger.trades[&id].quote;
        let expected = if p.stage == Stage::Seller {
            warehouse_endpoint(&q.seller)
        } else {
            Some(InventoryEndpoint::NpcCarry(q.buyer.clone()))
        };
        if !expected
            .as_ref()
            .is_some_and(|endpoint| same_inventory_endpoint(endpoint, &snapshot.endpoint))
        {
            return Some(report(p.session, "Wrong quoted endpoint; no exchange."));
        }
        if p.stage == Stage::Seller {
            p.seller = Some(snapshot.clone());
            let endpoint = InventoryEndpoint::NpcCarry(q.buyer.clone());
            return Some(self.trade_query(p, Stage::Buyer, endpoint));
        }
        p.buyer = Some(snapshot.clone());
        let seller = p.seller.as_ref().expect("seller snapshot");
        if matches!(p.action, Action::Accept { .. }) {
            if seller.fence.revision != q.seller_revision
                || seller.fence.snapshot_hash != q.source_hash
                || snapshot.fence.revision != q.buyer_revision
                || snapshot.fence.snapshot_hash != q.buyer_hash
            {
                return Some(report(p.session,"Stale quote: price/quantity retained but inventory changed; make a new quote id."));
            }
            let plan = p.store.plans.get_mut(&id).expect("quote plan");
            let Ok((key, Some(version))) =
                self.registry_scope_fence(p.registry_version, &plan.scope)
            else {
                return Some(report(
                    p.session,
                    "Current quote rights fence unavailable; no exchange.",
                ));
            };
            if version == 0 {
                return Some(report(
                    p.session,
                    "Current quote rights fence unavailable; no exchange.",
                ));
            }
            plan.storage_guard = Some(caravan::StorageGuard { key, version });
            plan.accepted = true;
            return Some(self.trade_write(p, Stage::AcceptWrite));
        }
        let Some(seller_slot) = seller
            .slots
            .iter()
            .find(|slot| {
                slot.item
                    .as_ref()
                    .is_some_and(|item| item.resource_id == q.item && item.count >= q.quantity)
            })
            .map(|s| s.slot)
        else {
            return Some(report(
                p.session,
                "Seller lacks quoted physical stock; quote not recorded.",
            ));
        };
        let Some(buyer_slot) = snapshot
            .slots
            .iter()
            .find(|slot| {
                slot.item.as_ref().is_some_and(|item| {
                    item.resource_id == q.payment_item && item.count >= q.payment_quantity
                })
            })
            .map(|s| s.slot)
        else {
            return Some(report(
                p.session,
                "Poor counterparty: quoted physical payment unavailable; no quote or debt created.",
            ));
        };
        let Some(seller_receive) = seller
            .slots
            .iter()
            .find(|s| s.item.is_none())
            .map(|s| s.slot)
        else {
            return Some(report(
                p.session,
                "Seller cannot receive physical payment; quote not recorded.",
            ));
        };
        let Some(buyer_receive) = snapshot
            .slots
            .iter()
            .find(|s| s.item.is_none())
            .map(|s| s.slot)
        else {
            return Some(report(
                p.session,
                "Buyer cannot receive goods; quote not recorded.",
            ));
        };
        let trade = p.store.ledger.trades.get_mut(&id).expect("new quote");
        trade.quote.seller_revision = seller.fence.revision;
        trade.quote.source_hash = seller.fence.snapshot_hash.clone();
        trade.quote.buyer_revision = snapshot.fence.revision;
        trade.quote.buyer_hash = snapshot.fence.snapshot_hash.clone();
        if trade.quote.validate().is_err() {
            return Some(report(
                p.session,
                "Invalid physical quote; nothing recorded.",
            ));
        }
        let Some(revision) = p.store.ledger.revision.checked_add(1) else {
            return Some(report(p.session, "Trade revision exhausted."));
        };
        p.store.ledger.revision = revision;
        let plan = p.store.plans.get_mut(&id).expect("new quote plan");
        plan.seller_slot = seller_slot;
        plan.buyer_slot = buyer_slot;
        plan.seller_receive = seller_receive;
        plan.buyer_receive = buyer_receive;
        Some(self.trade_write(p, Stage::QuoteWrite))
    }
    pub(super) fn trade_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let p = self.commerce_trade.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::QuoteWrite | Stage::AcceptWrite | Stage::ReceiptWrite
            )
        {
            self.commerce_trade.pending = Some(p);
            return None;
        }
        match answer.outcome() {
            StorageCasOutcome::Committed(_) => Some(match p.stage {
                Stage::AcceptWrite => self.trade_native(p),
                Stage::QuoteWrite => report(
                    p.session,
                    "Physical quote recorded, not executed. Explicit trade-accept required.",
                ),
                _ => {
                    let mut commands = report(
                        p.session,
                        "Native trade receipt durably recorded; no virtual currency issued.",
                    );
                    commands.extend(self.trade_init());
                    commands
                }
            }),
            StorageCasOutcome::Refused => {
                let stage = if p.stage == Stage::ReceiptWrite {
                    Stage::ReceiptRead
                } else {
                    Stage::Store
                };
                Some(self.trade_read(p, stage))
            }
            _ => Some(report(
                p.session,
                "Trade storage unavailable; accepted intent retained.",
            )),
        }
    }
}
