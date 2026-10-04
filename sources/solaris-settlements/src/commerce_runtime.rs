//! Simulation-clock payroll: durable intent before native physical emerald transfer.
use super::commerce::{Employment, Ledger};
use super::*;
use solaris_plugin_sdk::{
    inventories, query_owned_inventory, schedule_timer, transfer_owned_items,
    with_expected_plugin_storage_versions,
};
pub(super) const KEY: &str = "settlement-commerce-v1";
pub(super) const TIMER: &str = "settlement-commerce";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
    now: u64,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub(super) struct Store {
    pub(super) ledger: Ledger,
    pub(super) intents: BTreeMap<String, Payment>,
    #[serde(default)]
    attempt: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Payment {
    pub(super) contract: String,
    pub(super) operation: String,
    pub(super) amount: u32,
    pub(super) treasury: String,
    pub(super) worker: String,
    source_slot: u8,
    destination_slot: u8,
    source_fence: hire::Fence,
    destination_fence: hire::Fence,
    #[serde(default)]
    storage_guard: Option<caravan::StorageGuard>,
}
#[derive(Clone)]
enum Action {
    Create {
        scope: String,
        id: String,
        resident: String,
        player: String,
        wage: u32,
        period: u64,
        budget: u64,
    },
    Stop {
        id: String,
        player: String,
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
    Read,
    Source,
    Destination,
    IntentWrite,
    Native,
    ReceiptRead,
    ReceiptWrite,
    ContractWrite,
}
struct Pending {
    request: String,
    session: Option<u64>,
    stage: Stage,
    action: Action,
    store: Store,
    version: Option<u64>,
    registry: SettlementRegistry,
    registry_version: Option<u64>,
    contract: Option<String>,
    source: Option<inventories::OwnedInventorySnapshot>,
    payment: Option<Payment>,
    accepted: bool,
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} commerce {text}"));
    session
        .map(|session| vec![message_session(session, text)])
        .unwrap_or_default()
}
impl Settlements {
    pub(super) fn handle_commerce_command(
        &mut self,
        invoked: &CommandInvoked,
        tick: u64,
    ) -> Option<Vec<Command>> {
        if invoked.name != "settlement" {
            return None;
        }
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if !matches!(
            args.first(),
            Some(&"employment") | Some(&"employment-stop") | Some(&"employment-status")
        ) {
            return None;
        }
        self.commerce_runtime.now = tick;
        if self.commerce_runtime.pending.is_some() {
            return Some(report(
                Some(invoked.session),
                "Commerce busy; no new effect issued.",
            ));
        }
        let action = match args.as_slice() {
            ["employment",scope,id,resident,wage,period,budget] => {
                let values = wage.parse::<u32>().ok().filter(|v| *v>0).zip(period.parse::<u64>().ok().filter(|v| *v>0 && tick.checked_add(*v).is_some())).zip(budget.parse::<u64>().ok().filter(|v| *v>0));
                let Some(((wage,period),budget)) = values else { return Some(report(Some(invoked.session),"Explicit positive wage, simulation period and budget required.")); };
                if id.is_empty() { return Some(report(Some(invoked.session),"Stable employment id required.")); }
                Action::Create { scope:(*scope).into(),id:(*id).into(),resident:(*resident).into(),player:invoked.player.clone(),wage,period,budget }
            }
            ["employment-stop",id] => Action::Stop { id:(*id).into(),player:invoked.player.clone() },
            ["employment-status",id] => Action::Status { id:(*id).into(),player:invoked.player.clone() },
            _ => return Some(report(Some(invoked.session),"Usage: employment <settlement> <id> <resident> <emerald-wage> <simulation-period> <budget>; employment-stop/status <id>"))
        };
        Some(self.commerce_begin(action, Some(invoked.session)))
    }
    pub(super) fn commerce_init(&mut self) -> Vec<Command> {
        self.commerce_runtime.pending = None;
        self.commerce_begin(Action::Wake, None)
    }
    pub(super) fn commerce_timer(&mut self, tick: u64) -> Vec<Command> {
        self.commerce_runtime.now = tick;
        if self.commerce_runtime.pending.is_some() {
            return vec![schedule_timer(TIMER, 1)];
        }
        self.commerce_begin(Action::Wake, None)
    }
    fn commerce_begin(&mut self, action: Action, session: Option<u64>) -> Vec<Command> {
        let Some(request) = self.next_storage_request("commerce-registry") else {
            return report(session, "Request sequence exhausted.");
        };
        self.commerce_runtime.pending = Some(Pending {
            request: request.clone(),
            session,
            stage: Stage::Registry,
            action,
            store: Store::default(),
            version: None,
            registry: SettlementRegistry::default(),
            registry_version: None,
            contract: None,
            source: None,
            payment: None,
            accepted: false,
        });
        self.registry_read(&request)
    }
    fn commerce_read(&mut self, mut p: Pending, stage: Stage) -> Vec<Command> {
        let Some(request) = self.next_storage_request("commerce-read") else {
            return report(
                p.session,
                "Request sequence exhausted; durable intent retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_runtime.pending = Some(p);
        vec![storage_get(&request, KEY)]
    }
    fn commerce_write(&mut self, mut p: Pending, stage: Stage) -> Vec<Command> {
        let Ok(value) = serde_json::to_string(&p.store) else {
            return report(
                p.session,
                "Commerce serialization refused; durable intent retained.",
            );
        };
        let Some(request) = self.next_storage_request("commerce-write") else {
            return report(
                p.session,
                "Request sequence exhausted; durable intent retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        let command = storage_cas(&request, KEY, p.version, value);
        self.commerce_runtime.pending = Some(p);
        vec![command]
    }
    fn commerce_defer(&mut self, p: Pending, text: &str) -> Vec<Command> {
        let mut commands = report(p.session, text);
        commands.extend(self.commerce_write(p, Stage::ContractWrite));
        commands
    }
    fn commerce_query(
        &mut self,
        mut p: Pending,
        stage: Stage,
        endpoint: InventoryEndpoint,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("commerce-inventory") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_runtime.pending = Some(p);
        vec![query_owned_inventory(&request, endpoint, None)]
    }
    fn commerce_native(&mut self, mut p: Pending) -> Vec<Command> {
        let payment = p.payment.as_ref().expect("durable payment");
        let Some(guard) = payment.storage_guard.as_ref().filter(|g| g.version > 0) else {
            return report(
                p.session,
                "Payroll lacks original rights fence; intent retained without payment.",
            );
        };
        let Some(source) = warehouse_endpoint(&payment.treasury) else {
            return report(p.session, "Treasury endpoint invalid; intent retained.");
        };
        let destination = InventoryEndpoint::NpcCarry(payment.worker.clone());
        let Some(request) = self.next_storage_request("commerce-transfer") else {
            return report(p.session, "Request sequence exhausted; intent retained.");
        };
        let command = with_expected_plugin_storage_versions(
            transfer_owned_items(
                &request,
                &payment.operation,
                0,
                None,
                vec![inventories::OwnedItemTransfer {
                    source: source.clone(),
                    source_slot: payment.source_slot,
                    destination: destination.clone(),
                    destination_slot: payment.destination_slot,
                    count: payment.amount,
                }],
                vec![
                    inventories::InventoryExpectedRevision {
                        endpoint: source,
                        fence: payment.source_fence.wire(),
                    },
                    inventories::InventoryExpectedRevision {
                        endpoint: destination,
                        fence: payment.destination_fence.wire(),
                    },
                ],
            ),
            vec![guard.wire()],
        );
        p.request = request;
        p.stage = Stage::Native;
        self.commerce_runtime.pending = Some(p);
        vec![command]
    }
    pub(super) fn commerce_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::Registry | Stage::Read | Stage::ReceiptRead)
        {
            self.commerce_runtime.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Storage unavailable; durable intent retained.",
            ));
        };
        if p.stage == Stage::Registry {
            p.registry_version = record.version;
            p.registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => return Some(report(p.session, "Settlement store unreadable.")),
            };
            return Some(self.commerce_read(p, Stage::Read));
        }
        p.store = match record.value.as_deref() {
            None => Store::default(),
            Some(value) => match serde_json::from_str(value) {
                Ok(value) => value,
                Err(_) => {
                    return Some(report(
                        p.session,
                        "Commerce store unreadable; no money changed.",
                    ))
                }
            },
        };
        p.version = record.version;
        if p.stage == Stage::ReceiptRead {
            let payment = p.payment.as_ref().expect("native decision").clone();
            if p.accepted
                && p.store
                    .ledger
                    .record_payment(&payment.operation, &payment.contract, payment.amount)
                    .is_err()
            {
                return Some(report(
                    p.session,
                    "Payment receipt accounting conflict; intent retained.",
                ));
            }
            p.store.intents.remove(&payment.operation);
            return Some(self.commerce_write(p, Stage::ReceiptWrite));
        }
        // Durable transfers are replayed unchanged, including their original fences.
        if let Some(payment) = p.store.intents.values().next().cloned() {
            p.payment = Some(payment);
            return Some(self.commerce_native(p));
        }
        match p.action.clone(){
        Action::Create{scope,id,resident,player,wage,period,budget}=>{
            let Some(settlement)=p.registry.settlements.get(&scope).filter(|s|can_build_settlement(s,&player)) else{return Some(report(p.session,"Employment refused: current settlement rights required."));};
            let Some(worker)=settlement.residents.get(&resident).filter(|r|r.role.is_none()) else{return Some(report(p.session,"Employment refused: available civilian resident required."));};
            let Some(warehouse)=self.warehouse.as_ref().filter(|w|warehouse_belongs_to(settlement,w)) else{return Some(report(p.session,"Employment refused: settlement-bound physical treasury required."));};
            if let Some(existing)=p.store.ledger.employment.get(&id){
                let matches=existing.owner==player&&existing.worker==worker.handle&&existing.treasury==warehouse.handle&&existing.wage==wage&&existing.period_ticks==period;
                return Some(report(p.session,if matches{"Employment already recorded; no earnings or budget reset."}else{"Employment id conflict; contract unchanged."}));
            }
            p.store.ledger.employment.insert(id,Employment{owner:player,worker:worker.handle.clone(),treasury:warehouse.handle.clone(),wage,period_ticks:period,next_due:self.commerce_runtime.now+period,budget,earned:0,paid:0,active:true});
            Some(self.commerce_write(p,Stage::ContractWrite))
        }
        Action::Stop{id,player}=>{
            let Some(contract)=p.store.ledger.employment.get_mut(&id).filter(|c|c.owner==player) else{return Some(report(p.session,"Employment owner required."));};
            if let Err(reason)=contract.accrue(self.commerce_runtime.now){return Some(report(p.session,format!("Employment accrual refused: {reason:?}")));}
            contract.active=false;
            Some(self.commerce_write(p,Stage::ContractWrite))
        }
        Action::Status{id,player}=>{
            Some(match p.store.ledger.employment.get(&id).filter(|c|c.owner==player){Some(c)=>report(p.session,format!("Employment {id}: active={} earned={} paid={} physical-emerald-budget={} next-simulation-due={}",c.active,c.earned,c.paid,c.budget,c.next_due)),None=>report(p.session,"Employment owner required.")})
        }
        Action::Wake=>{
            // No wall-clock/downtime catch-up: now is exclusively the delivered simulation tick.
            for (id,c) in &mut p.store.ledger.employment {
                let available=p.registry.settlements.values().any(|s|can_build_settlement(s,&c.owner)
                    && s.residents.values().any(|r|r.handle==c.worker&&r.role.is_none())
                    && self.warehouse.as_ref().is_some_and(|w|w.handle==c.treasury&&warehouse_belongs_to(s,w)));
                if !available {c.active=false;continue;}
                if c.accrue(self.commerce_runtime.now).is_err(){continue;}
                if c.payable().is_ok_and(|amount|amount>0){p.contract=Some(id.clone());break;}
            }
            let Some(id)=p.contract.as_ref() else{return Some(self.commerce_write(p,Stage::ContractWrite));};
            let contract=&p.store.ledger.employment[id];
            let Some(endpoint)=warehouse_endpoint(&contract.treasury) else{return Some(report(p.session,"Physical treasury endpoint invalid."));};
            Some(self.commerce_query(p,Stage::Source,endpoint))
        }
    }
    }
    pub(super) fn commerce_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::Source | Stage::Destination | Stage::Native)
        {
            self.commerce_runtime.pending = Some(p);
            return None;
        }
        if p.stage == Stage::Native {
            if answer.operation_id.as_deref()
                != p.payment.as_ref().map(|payment| payment.operation.as_str())
            {
                self.commerce_runtime.pending = Some(p);
                return Some(vec![]);
            }
            let payment = p.payment.as_ref().expect("payment");
            if matches!(answer.outcome(), OperationOutcome::Refused(refused)
                if matches!(refused.reason, OperationFailure::RuntimeUnavailable))
            {
                let mut commands = report(
                    p.session,
                    "Payroll decision unavailable; original intent retained for replay.",
                );
                commands.push(schedule_timer(TIMER, 1));
                return Some(commands);
            }
            p.accepted = match answer.outcome() {
                OperationOutcome::Refused(_) => false,
                OperationOutcome::Committed(committed) => {
                    let OperationPayload::OwnedInventory(InventoryResult::Transfer(fences)) =
                        &committed.payload
                    else {
                        return Some(report(
                            p.session,
                            "Unexpected payroll receipt; original intent retained.",
                        ));
                    };
                    let Some(source) = warehouse_endpoint(&payment.treasury) else {
                        return Some(report(
                            p.session,
                            "Payroll receipt endpoint invalid; original intent retained.",
                        ));
                    };
                    let destination = InventoryEndpoint::NpcCarry(payment.worker.clone());
                    if fences.len() != 2
                        || !fences
                            .iter()
                            .any(|f| same_inventory_endpoint(&f.endpoint, &source))
                        || !fences
                            .iter()
                            .any(|f| same_inventory_endpoint(&f.endpoint, &destination))
                    {
                        return Some(report(
                            p.session,
                            "Payroll receipt endpoint mismatch; original intent retained.",
                        ));
                    }
                    true
                }
            };
            return Some(self.commerce_read(p, Stage::ReceiptRead));
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            return Some(self.commerce_defer(
                p,
                "Physical payroll endpoint unavailable; no payment recorded.",
            ));
        };
        let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
            &committed.payload
        else {
            return Some(
                self.commerce_defer(p, "Unexpected inventory response; no payment recorded."),
            );
        };
        let id = p.contract.as_ref().expect("selected contract");
        let contract = &p.store.ledger.employment[id];
        let expected = if p.stage == Stage::Source {
            warehouse_endpoint(&contract.treasury)
        } else {
            Some(InventoryEndpoint::NpcCarry(contract.worker.clone()))
        };
        if !expected
            .as_ref()
            .is_some_and(|endpoint| same_inventory_endpoint(endpoint, &snapshot.endpoint))
        {
            return Some(self.commerce_defer(p, "Wrong inventory endpoint; no payment recorded."));
        }
        if p.stage == Stage::Source {
            p.source = Some(snapshot.clone());
            let endpoint = InventoryEndpoint::NpcCarry(contract.worker.clone());
            return Some(self.commerce_query(p, Stage::Destination, endpoint));
        }
        let source = p.source.as_ref().expect("source snapshot");
        let amount = match contract.payable() {
            Ok(amount) if amount > 0 => amount,
            _ => return Some(report(p.session, "Payroll has no payable earned amount.")),
        };
        let Some(source_slot) = source
            .slots
            .iter()
            .find(|slot| {
                slot.item.as_ref().is_some_and(|item| {
                    item.resource_id == "minecraft:emerald" && item.count >= amount
                })
            })
            .map(|slot| slot.slot)
        else {
            return Some(self.commerce_defer(
                p,
                "Treasury lacks earned physical emerald wage in one slot; no payment recorded.",
            ));
        };
        // Empty destination avoids component/stack-cap assumptions: native authority still admits it.
        let Some(destination_slot) = snapshot
            .slots
            .iter()
            .find(|slot| slot.item.is_none())
            .map(|slot| slot.slot)
        else {
            return Some(self.commerce_defer(p, "Personal carry full; earned wages retained."));
        };
        let Some(scope) = p
            .registry
            .settlements
            .iter()
            .find(|(_, s)| {
                can_build_settlement(s, &contract.owner)
                    && s.residents
                        .values()
                        .any(|r| r.handle == contract.worker && r.role.is_none())
                    && self.warehouse.as_ref().is_some_and(|w| {
                        w.handle == contract.treasury && warehouse_belongs_to(s, w)
                    })
            })
            .map(|(scope, _)| scope)
        else {
            return Some(
                self.commerce_defer(p, "Payroll ownership revoked; earned wages retained."),
            );
        };
        let Ok((key, Some(version))) = self.registry_scope_fence(p.registry_version, scope) else {
            return Some(
                self.commerce_defer(p, "Payroll current rights fence unavailable; no payment."),
            );
        };
        if version == 0 {
            return Some(
                self.commerce_defer(p, "Payroll current rights fence unavailable; no payment."),
            );
        }
        let operation = format!("settlement-payroll:{id}:{}:{amount}", contract.paid);
        let Some(attempt) = p.store.attempt.checked_add(1) else {
            return Some(report(
                p.session,
                "Payroll operation sequence exhausted; no payment issued.",
            ));
        };
        p.store.attempt = attempt;
        let operation = format!("{operation}:{attempt}");
        let payment = Payment {
            contract: id.clone(),
            operation: operation.clone(),
            amount,
            treasury: contract.treasury.clone(),
            worker: contract.worker.clone(),
            source_slot,
            destination_slot,
            source_fence: hire::Fence::from_snapshot(source),
            destination_fence: hire::Fence::from_snapshot(snapshot),
            storage_guard: Some(caravan::StorageGuard { key, version }),
        };
        p.store.intents.insert(operation, payment.clone());
        p.payment = Some(payment);
        Some(self.commerce_write(p, Stage::IntentWrite))
    }
    pub(super) fn commerce_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let p = self.commerce_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::IntentWrite | Stage::ReceiptWrite | Stage::ContractWrite
            )
        {
            self.commerce_runtime.pending = Some(p);
            return None;
        }
        if !matches!(answer.outcome(), StorageCasOutcome::Committed(_)) {
            return Some(report(
                p.session,
                "Commerce CAS refused; no new native effect; retry reads durable intent.",
            ));
        }
        if p.stage == Stage::IntentWrite {
            return Some(self.commerce_native(p));
        }
        let mut commands = report(
            p.session,
            "Commerce accounting committed; currency remains native physical property.",
        );
        if let Some(due) = p
            .store
            .ledger
            .employment
            .values()
            .filter(|c| c.active)
            .map(|c| c.next_due)
            .min()
        {
            commands.push(schedule_timer(
                TIMER,
                due.saturating_sub(self.commerce_runtime.now).max(1),
            ));
        }
        Some(commands)
    }
}
