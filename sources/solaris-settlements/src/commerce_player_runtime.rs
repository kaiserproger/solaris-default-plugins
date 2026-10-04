//! Durable intent -> native atomic exchange -> complete immutable receipt -> ACK.
use super::commerce_player::{self as policy, *};
use super::payment_mandate::{Blob, Reservation, VALUE_BYTES};
use super::*;
use solaris_plugin_sdk::{
    commands::OperationStatus, consume_inventory_reservation, events, inventories,
    list_online_players, npc_routes, query_inventory_placement_room, query_npc_route,
    release_inventory_reservation, schedule_timer, types, with_expected_plugin_storage_versions,
};

pub(super) const TIMER: &str = "settlement-player-contracts";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
    pub(super) now: u64,
    deadlines: BTreeMap<String, u64>,
}
#[derive(Clone)]
enum Action {
    Supply {
        id: String,
        owner: String,
        scope: String,
        building: String,
        resource: String,
        target: u32,
        reward: u32,
    },
    Escort {
        id: String,
        owner: String,
        shipment: String,
        scope: String,
        building: String,
        reward: u32,
        radius: u32,
        sample: u64,
    },
    List {
        player: String,
    },
    Status {
        id: String,
        player: String,
    },
    Receipt {
        id: String,
        player: String,
    },
    Accept {
        id: String,
        participant: Participant,
    },
    Fulfill {
        id: String,
        participant: Participant,
    },
    Cancel {
        id: String,
        player: String,
    },
    Wake,
}
impl Action {
    fn id(&self) -> Option<&str> {
        match self {
            Self::Supply { id, .. }
            | Self::Escort { id, .. }
            | Self::Status { id, .. }
            | Self::Receipt { id, .. }
            | Self::Accept { id, .. }
            | Self::Fulfill { id, .. }
            | Self::Cancel { id, .. } => Some(id),
            _ => None,
        }
    }
    fn player(&self) -> Option<&str> {
        match self {
            Self::Supply { owner, .. } | Self::Escort { owner, .. } => Some(owner),
            Self::List { player }
            | Self::Status { player, .. }
            | Self::Receipt { player, .. }
            | Self::Cancel { player, .. } => Some(player),
            Self::Accept { participant, .. } | Self::Fulfill { participant, .. } => {
                Some(&participant.player)
            }
            Self::Wake => None,
        }
    }
    fn create(&self) -> bool {
        matches!(self, Self::Supply { .. } | Self::Escort { .. })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Index,
    Head,
    Terms,
    Caravans,
    Stock,
    PlayerInventory,
    Route,
    Players,
    Placement,
    BlobRead,
    BlobCas,
    HeadCas,
    IndexCas,
    OperationStatus,
    Native,
    ActorReceipt,
}
#[derive(Clone, Copy)]
enum After {
    Create,
    Claim,
    Checkpoint,
    Intent,
    Receipt,
}
struct Publication {
    blob: Blob,
    chunks: Vec<String>,
    cursor: usize,
    after: After,
}
struct Pending {
    request: String,
    stage: Stage,
    action: Action,
    session: Option<u64>,
    id: Option<String>,
    index: Index,
    index_version: Option<u64>,
    registry: SettlementRegistry,
    registry_version: Option<u64>,
    head: Option<Head>,
    version: Option<u64>,
    terms: Option<Terms>,
    chunks: Vec<String>,
    publication: Option<Publication>,
    stock: Option<inventories::OwnedInventorySnapshot>,
    player_inventory: Option<inventories::OwnedInventorySnapshot>,
    caravan: Option<caravan::Contract>,
    caravan_version: Option<u64>,
    route: Option<npc_routes::NpcRouteStatus>,
    escort: Option<policy::Escort>,
    intent: Option<policy::Intent>,
    legs: Vec<inventories::OwnedItemTransfer>,
    sources: Vec<(u8, u32)>,
    source_index: usize,
    supplying: bool,
    supplied: u32,
    offers: Vec<serde_json::Value>,
    list_cursor: usize,
    pruning: bool,
}
#[derive(Serialize, Deserialize)]
struct PaidReceipt {
    participant: Participant,
    terms_digest: String,
    intent_digest: String,
    operation: String,
    native: events::OperationCommitted,
}
#[derive(Serialize)]
struct PaidReceiptRef<'a> {
    participant: &'a Participant,
    terms_digest: &'a str,
    intent_digest: &'a str,
    operation: &'a str,
    native: &'a events::OperationCommitted,
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} player-contract {text}"));
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
    commands.push(schedule_timer(TIMER, 1));
    commands
}
impl Settlements {
    pub(super) fn handle_player_contract_command(
        &mut self,
        invoked: &CommandInvoked,
        tick: u64,
    ) -> Option<Vec<Command>> {
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if invoked.name != "settlement"
            || !matches!(
                args.first(),
                Some(&"player-supply")
                    | Some(&"player-escort")
                    | Some(&"player-contracts")
                    | Some(&"player-contract-status")
                    | Some(&"player-contract-receipt")
                    | Some(&"player-contract-accept")
                    | Some(&"player-contract-fulfill")
                    | Some(&"player-contract-cancel")
            )
        {
            return None;
        }
        self.commerce_player_runtime.now = tick;
        if self.commerce_player_runtime.pending.is_some() {
            return Some(report(
                Some(invoked.session),
                "Player contract provider busy; no second effect admitted.",
            ));
        }
        let participant = Participant {
            player: invoked.player.clone(),
            session: invoked.session,
        };
        let action = match args.as_slice() {
            ["player-supply", id, scope, building, resource, target, reward] => {
                let values = target.parse::<u32>().ok().zip(reward.parse::<u32>().ok());
                let Some((target, reward)) = values.filter(|(target, reward)| *target > 0 && *reward > 0 && target.checked_add(*reward).is_some_and(|n| n <= ITEM_BUDGET)) else {
                    return Some(report(Some(invoked.session), "Explicit positive target and physical reward must fit one 4096-item native exchange."));
                };
                Action::Supply { id:(*id).into(), owner:invoked.player.clone(), scope:(*scope).into(), building:(*building).into(), resource:(*resource).into(), target, reward }
            }
            ["player-escort", id, shipment, scope, building, reward, radius, sample] => {
                let values = reward.parse::<u32>().ok().zip(radius.parse::<u32>().ok()).zip(sample.parse::<u64>().ok());
                let Some(((reward, radius), sample)) = values.filter(|((reward, radius), sample)| *reward > 0 && *reward <= ITEM_BUDGET && *radius > 0 && *radius <= 128 && *sample > 0 && sample.checked_mul(2).is_some()) else {
                    return Some(report(Some(invoked.session), "Explicit physical reward, radius 1..128 and positive simulation sampling period required."));
                };
                Action::Escort { id:(*id).into(), owner:invoked.player.clone(), shipment:(*shipment).into(), scope:(*scope).into(), building:(*building).into(), reward, radius, sample }
            }
            ["player-contracts"] => Action::List { player:invoked.player.clone() },
            ["player-contract-status", id] => Action::Status { id:(*id).into(), player:invoked.player.clone() },
            ["player-contract-receipt", id] => Action::Receipt { id:(*id).into(), player:invoked.player.clone() },
            ["player-contract-accept", id] => Action::Accept { id:(*id).into(), participant },
            ["player-contract-fulfill", id] => Action::Fulfill { id:(*id).into(), participant },
            ["player-contract-cancel", id] => Action::Cancel { id:(*id).into(), player:invoked.player.clone() },
            _ => return Some(report(Some(invoked.session), "Usage: player-supply <id> <settlement> <stock-building> <resource> <target-stock> <fixed-emerald-reward>; player-escort <id> <caravan> <settlement> <destination-stock-building> <fixed-emerald-reward> <checkpoint-radius> <sample-simulation-ticks>; player-contracts; player-contract-status/receipt/accept/fulfill/cancel <id>")),
        };
        if action.id().is_some_and(|id| !valid_id(id)) {
            return Some(report(
                Some(invoked.session),
                "Contract id requires 1..40 lowercase ASCII letters, digits, '_' or '-'.",
            ));
        }
        Some(self.pc_begin(action, Some(invoked.session)))
    }
    pub(super) fn player_contract_init(&mut self) -> Vec<Command> {
        self.commerce_player_runtime.pending = None;
        self.commerce_player_runtime.deadlines.clear();
        vec![schedule_timer(TIMER, 1)]
    }
    pub(super) fn player_contract_timer(&mut self, tick: u64) -> Vec<Command> {
        self.commerce_player_runtime.now = tick;
        if self.commerce_player_runtime.pending.is_some() {
            return vec![schedule_timer(TIMER, 1)];
        }
        self.pc_begin(Action::Wake, None)
    }
    fn pc_begin(&mut self, action: Action, session: Option<u64>) -> Vec<Command> {
        let id = action.id().map(str::to_owned);
        let p = Pending {
            request: String::new(),
            stage: Stage::Registry,
            action,
            session,
            id,
            index: Index::default(),
            index_version: None,
            registry: SettlementRegistry::default(),
            registry_version: None,
            head: None,
            version: None,
            terms: None,
            chunks: Vec::new(),
            publication: None,
            stock: None,
            player_inventory: None,
            caravan: None,
            caravan_version: None,
            route: None,
            escort: None,
            intent: None,
            legs: Vec::new(),
            sources: Vec::new(),
            source_index: 0,
            supplying: false,
            supplied: 0,
            offers: Vec::new(),
            list_cursor: 0,
            pruning: false,
        };
        self.pc_read(p, Stage::Registry, registry_storage::ROOT_KEY.to_owned())
    }
    fn pc_read(&mut self, mut p: Pending, stage: Stage, key: String) -> Vec<Command> {
        let Some(request) = self.next_storage_request("player-read") else {
            return report(
                p.session,
                "Request sequence exhausted; durable intents retained.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_player_runtime.pending = Some(p);
        if stage == Stage::Registry {
            self.registry_read(&request)
        } else {
            vec![storage_get(&request, &key)]
        }
    }
    fn pc_inventory(
        &mut self,
        mut p: Pending,
        stage: Stage,
        endpoint: InventoryEndpoint,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("player-inventory") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = stage;
        self.commerce_player_runtime.pending = Some(p);
        vec![query_owned_inventory(&request, endpoint, None)]
    }
    fn pc_head_write(&mut self, mut p: Pending) -> Vec<Command> {
        let head = p.head.as_ref().expect("head");
        let Ok(value) = serde_json::to_string(head) else {
            return report(p.session, "Header serialization refused; no ACK.");
        };
        if value.len() > VALUE_BYTES {
            return report(
                p.session,
                "Contract header exceeds native bound; no effect admitted.",
            );
        }
        let Some(request) = self.next_storage_request("player-head") else {
            return report(
                p.session,
                "Request sequence exhausted; durable intent retained.",
            );
        };
        let command = storage_cas(&request, &key(&head.id), p.version, value);
        p.request = request;
        p.stage = Stage::HeadCas;
        self.commerce_player_runtime.pending = Some(p);
        vec![command]
    }
    fn pc_index_write(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(request) = self.next_storage_request("player-index") else {
            return report(p.session, "Request sequence exhausted.");
        };
        let Ok(value) = serde_json::to_string(&p.index) else {
            return report(p.session, "Index serialization refused.");
        };
        let command = storage_cas(&request, INDEX_KEY, p.index_version, value);
        p.request = request;
        p.stage = Stage::IndexCas;
        self.commerce_player_runtime.pending = Some(p);
        vec![command]
    }
    fn pc_publish<T: Serialize>(&mut self, p: Pending, value: &T, after: After) -> Vec<Command> {
        let Ok((blob, chunks)) = Blob::encode(value) else {
            return report(
                p.session,
                "Complete immutable record serialization refused; no ACK.",
            );
        };
        self.pc_publish_encoded(p, blob, chunks, after)
    }
    fn pc_publish_encoded(
        &mut self,
        mut p: Pending,
        blob: Blob,
        chunks: Vec<String>,
        after: After,
    ) -> Vec<Command> {
        p.publication = Some(Publication {
            blob,
            chunks,
            cursor: 0,
            after,
        });
        self.pc_blob_step(p)
    }
    fn pc_blob_step(&mut self, mut p: Pending) -> Vec<Command> {
        let publication = p.publication.as_ref().expect("publication");
        if publication.cursor < publication.chunks.len() {
            let key = publication.blob.key(publication.cursor);
            return self.pc_read(p, Stage::BlobRead, key);
        }
        let publication = p.publication.take().expect("publication");
        let head = p.head.as_mut().expect("head");
        match publication.after {
            After::Create => head.terms = publication.blob,
            After::Claim | After::Checkpoint => {
                let mut escort = p.escort.take().expect("causal escort");
                escort.checkpoint = publication.blob;
                head.escort = Some(escort);
                head.state = State::Claimed;
            }
            After::Intent => {
                head.intent = Some(publication.blob);
                head.state = State::PayoutIntent;
            }
            After::Receipt => {
                head.receipt = Some(publication.blob);
                head.state = State::Paid;
                head.blocked = None;
            }
        }
        self.pc_head_write(p)
    }
    fn pc_load_terms(&mut self, mut p: Pending) -> Vec<Command> {
        p.chunks.clear();
        let key = p.head.as_ref().expect("head").terms.key(0);
        self.pc_read(p, Stage::Terms, key)
    }
    fn pc_finish(&mut self, mut p: Pending) -> Vec<Command> {
        if let Some(head) = p.head.as_ref() {
            let now = self.commerce_player_runtime.now;
            let deadline = match head.state {
                State::Claimed => head
                    .escort
                    .as_ref()
                    .map_or(now.saturating_add(20), |e| e.next_due),
                State::Offered => now.saturating_add(20),
                State::ReserveIntent | State::PayoutIntent | State::ReleaseIntent => {
                    now.saturating_add(1)
                }
                State::Paid | State::Closed => u64::MAX,
            };
            self.commerce_player_runtime
                .deadlines
                .insert(head.id.clone(), deadline);
        }
        if let Action::List { player } = &p.action {
            if let Some(head) = &p.head {
                if let Some(projection) = head.projection(player) {
                    let remaining = p
                        .terms
                        .as_ref()
                        .and_then(|t| p.stock.as_ref().and_then(|s| t.remaining(s).ok()));
                    if head.state != State::Offered || remaining != Some(0) {
                        p.offers.push(serde_json::json!({ "contract": projection, "remaining_supply": remaining }));
                    }
                }
            }
            p.list_cursor += 1;
            if let Some(id) = p.index.ids.get(p.list_cursor).cloned() {
                p.id = Some(id.clone());
                p.head = None;
                p.terms = None;
                p.stock = None;
                return self.pc_read(p, Stage::Head, key(&id));
            }
            return report(
                p.session,
                serde_json::json!({"contracts":p.offers,"truncated":false}).to_string(),
            );
        }
        if matches!(p.action, Action::Wake) {
            let now = self.commerce_player_runtime.now;
            let delay = p
                .index
                .ids
                .iter()
                .map(|id| {
                    self.commerce_player_runtime
                        .deadlines
                        .get(id)
                        .copied()
                        .unwrap_or(now)
                })
                .min()
                .map_or(20, |due| due.saturating_sub(now).clamp(1, 20));
            return vec![schedule_timer(TIMER, delay)];
        }
        let Some(head) = p.head.as_ref() else {
            return report(p.session, "No such published contract.");
        };
        let Some(player) = p.action.player() else {
            return report(p.session, "Actor unavailable.");
        };
        let Some(projection) = head.projection(player) else {
            return report(
                p.session,
                "No public offer or actor-owned contract; private claim and receipt withheld.",
            );
        };
        let remaining = p
            .terms
            .as_ref()
            .and_then(|t| p.stock.as_ref().and_then(|s| t.remaining(s).ok()));
        report(
            p.session,
            serde_json::json!({"contract":projection,"remaining_supply":remaining}).to_string(),
        )
    }
    fn pc_close(&mut self, mut p: Pending, reason: &str) -> Vec<Command> {
        let head = p.head.as_mut().expect("head");
        head.blocked = Some(reason.to_owned());
        if head.reservation.is_none() {
            head.state = State::Closed;
        } else {
            head.state = State::ReleaseIntent;
            head.release_operation
                .get_or_insert_with(|| format!("pc_{}_release", head.id));
        }
        self.pc_head_write(p)
    }
    fn pc_ensure_index(&mut self, mut p: Pending) -> Vec<Command> {
        let id = p.id.as_ref().expect("id");
        if p.index.ids.contains(id) {
            return self.pc_status(p);
        }
        if p.index.ids.len() >= MAX_OFFERS {
            return report(
                p.session,
                "Active offer bound reached; reserve intent retained without native effect.",
            );
        }
        p.index.ids.push(id.clone());
        p.index.ids.sort_unstable();
        self.pc_index_write(p)
    }
    fn pc_status(&mut self, mut p: Pending) -> Vec<Command> {
        let head = p.head.as_ref().expect("head");
        let operation = match head.state {
            State::ReserveIntent => p.terms.as_ref().expect("terms").reserve_operation.clone(),
            State::PayoutIntent => p
                .intent
                .as_ref()
                .expect("intent")
                .operation
                .operation_id
                .clone(),
            State::ReleaseIntent => head.release_operation.clone().expect("release operation"),
            _ => return self.pc_finish(p),
        };
        let Some(request) = self.next_storage_request("player-status") else {
            return report(
                p.session,
                "Request sequence exhausted; original operation retained.",
            );
        };
        let command = Command::OperationStatus(OperationStatus {
            request: request.clone(),
            operation_id: operation,
        });
        p.request = request;
        p.stage = Stage::OperationStatus;
        self.commerce_player_runtime.pending = Some(p);
        vec![command]
    }
    fn pc_native(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(version) = p.version.filter(|v| *v > 0) else {
            return report(
                p.session,
                "Original contract header fence unavailable; no effect.",
            );
        };
        let head = p.head.as_ref().expect("head");
        let terms = p.terms.as_ref().expect("terms");
        let Some(request) = self.next_storage_request("player-native") else {
            return report(
                p.session,
                "Request sequence exhausted; original operation retained.",
            );
        };
        let mut guards: Vec<_> = match head.state {
            State::ReserveIntent => terms
                .guards
                .iter()
                .map(caravan::StorageGuard::wire)
                .collect(),
            State::PayoutIntent => p
                .intent
                .as_ref()
                .expect("intent")
                .guards
                .iter()
                .map(caravan::StorageGuard::wire)
                .collect(),
            State::ReleaseIntent => Vec::new(),
            _ => return self.pc_finish(p),
        };
        guards.push(types::PluginStorageVersionGuard {
            key: key(&head.id),
            version,
        });
        guards.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        let command = match head.state {
            State::ReserveIntent => reserve_inventory_items(
                &request,
                &terms.reserve_operation,
                terms.stock.endpoint(),
                terms.plan(),
                terms.stock_fence.clone(),
            ),
            State::ReleaseIntent => {
                let reservation = head.reservation.as_ref().expect("reservation");
                release_inventory_reservation(
                    &request,
                    head.release_operation.as_deref().expect("release"),
                    &reservation.reference,
                    reservation.revision,
                )
            }
            State::PayoutIntent => {
                let op = &p.intent.as_ref().expect("intent").operation;
                consume_inventory_reservation(
                    &request,
                    &op.operation_id,
                    op.actor_id,
                    op.execution_permit.clone(),
                    &op.reservation_ref,
                    op.expected_reservation_revision,
                    op.portion,
                    op.resource_plan.clone(),
                    op.transfers.clone(),
                    op.expected_revisions.clone(),
                )
            }
            _ => unreachable!(),
        };
        p.request = request;
        p.stage = Stage::Native;
        self.commerce_player_runtime.pending = Some(p);
        vec![with_expected_plugin_storage_versions(command, guards)]
    }
    fn pc_route(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(route_ref) = p.caravan.as_ref().and_then(|c| c.route_ref.clone()) else {
            return report(
                p.session,
                "Original caravan has not admitted a real native route yet; no escort accepted.",
            );
        };
        let Some(request) = self.next_storage_request("player-route") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = Stage::Route;
        self.commerce_player_runtime.pending = Some(p);
        vec![query_npc_route(&request, &route_ref)]
    }
    fn pc_players(&mut self, mut p: Pending) -> Vec<Command> {
        let Some(request) = self.next_storage_request("player-observation") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = Stage::Players;
        self.commerce_player_runtime.pending = Some(p);
        vec![list_online_players(&request, 256)]
    }
    fn pc_drive(&mut self, mut p: Pending) -> Vec<Command> {
        let head = p.head.as_ref().expect("head");
        let terms = p.terms.as_ref().expect("terms");
        if head.state == State::PayoutIntent {
            p.chunks.clear();
            let key = head.intent.as_ref().expect("intent").key(0);
            return self.pc_read(p, Stage::Terms, key);
        }
        if head.state == State::ReleaseIntent {
            return self.pc_status(p);
        }
        if matches!(head.state, State::Paid | State::Closed) {
            if let Action::Receipt { player, .. } = &p.action {
                if head
                    .participant
                    .as_ref()
                    .is_none_or(|a| a.player != *player)
                {
                    return report(
                        p.session,
                        "Only the actual paid participant can read their inventory receipt.",
                    );
                }
                let Some(blob) = &head.receipt else {
                    return report(p.session, "No paid receipt; no currency issued.");
                };
                let key = blob.key(0);
                p.chunks.clear();
                return self.pc_read(p, Stage::ActorReceipt, key);
            }
            return self.pc_finish(p);
        }
        if !caravan_runtime::authorized(&p.registry, &terms.stock, &terms.owner) {
            return self.pc_close(p, "Original advertised owner no longer controls original physical stock; unused budget released.");
        }
        if let Action::Cancel { player, .. } = &p.action {
            if player != &terms.owner
                && head
                    .participant
                    .as_ref()
                    .is_none_or(|a| &a.player != player)
            {
                return report(
                    p.session,
                    "Only original owner or actual participant may cancel.",
                );
            }
            return self.pc_close(
                p,
                "Contract cancelled before payout; unused physical budget released.",
            );
        }
        if head.state == State::ReserveIntent {
            return self.pc_ensure_index(p);
        }
        let endpoint = terms.stock.endpoint();
        self.pc_inventory(p, Stage::Stock, endpoint)
    }
    fn pc_after_stock(&mut self, mut p: Pending) -> Vec<Command> {
        let head = p.head.as_ref().expect("head");
        let terms = p.terms.as_ref().expect("terms");
        let stock = p.stock.as_ref().expect("stock");
        if matches!(terms.need, Need::Supply { .. }) && terms.remaining(stock) == Ok(0) {
            if matches!(p.action, Action::List { .. } | Action::Status { .. }) {
                return self.pc_finish(p);
            }
            return self.pc_close(
                p,
                "Actual required stock already delivered by another participant; no second reward.",
            );
        }
        if head.state == State::Offered
            && matches!(terms.need, Need::Escort { .. })
            && matches!(
                p.action,
                Action::List { .. } | Action::Status { .. } | Action::Wake
            )
        {
            return self.pc_read(p, Stage::Caravans, caravan_runtime::KEY.to_owned());
        }
        if matches!(
            p.action,
            Action::List { .. } | Action::Status { .. } | Action::Receipt { .. }
        ) {
            return self.pc_finish(p);
        }
        match &p.action {
            Action::Accept { participant, .. } => {
                if head.state != State::Offered
                    && !(head.state == State::Claimed
                        && matches!(terms.need, Need::Supply { .. })
                        && head
                            .participant
                            .as_ref()
                            .is_some_and(|a| a.player == participant.player))
                {
                    return report(p.session, "Contract is not an unclaimed offer or your effect-free reconnectable supply claim.");
                }
                p.head.as_mut().expect("head").participant = Some(participant.clone());
                if matches!(terms.need, Need::Supply { .. }) {
                    p.head.as_mut().expect("head").state = State::Claimed;
                    return self.pc_head_write(p);
                }
                return self.pc_read(p, Stage::Caravans, caravan_runtime::KEY.to_owned());
            }
            Action::Fulfill { participant, .. } => {
                if head.state != State::Claimed || head.participant.as_ref() != Some(participant) {
                    return report(p.session, "Exact live participant claim required; pending operations cannot rebind to a reconnect.");
                }
                if head.escort.as_ref().is_some_and(|escort| {
                    escort.arrival.is_none() && self.commerce_player_runtime.now < escort.next_due
                }) {
                    return report(p.session, "Next declared native escort checkpoint is not due; no payout or cancellation.");
                }
            }
            Action::Wake => {
                if head.state != State::Claimed {
                    return self.pc_finish(p);
                }
                if let Some(escort) = &head.escort {
                    if self.commerce_player_runtime.now < escort.next_due {
                        return self.pc_finish(p);
                    }
                } else {
                    return self.pc_finish(p);
                }
            }
            _ => return self.pc_finish(p),
        }
        if matches!(terms.need, Need::Escort { .. }) {
            return self.pc_read(p, Stage::Caravans, caravan_runtime::KEY.to_owned());
        }
        let endpoint = InventoryEndpoint::PlayerInventory(
            head.participant.as_ref().expect("participant").session,
        );
        self.pc_inventory(p, Stage::PlayerInventory, endpoint)
    }
    fn pc_plan_supply(&mut self, mut p: Pending) -> Vec<Command> {
        let terms = p.terms.as_ref().expect("terms");
        let Need::Supply { resource, .. } = &terms.need else {
            return self.pc_prepare_money(p);
        };
        let Ok(mut remaining) = terms.remaining(p.stock.as_ref().expect("stock")) else {
            return report(p.session, "Original physical stock unavailable.");
        };
        if remaining == 0 {
            return self.pc_close(p, "Actual need already closed; no reward.");
        }
        p.supplied = remaining;
        for slot in &p.player_inventory.as_ref().expect("player inventory").slots {
            if remaining == 0 {
                break;
            }
            let Some(item) = slot.item.as_ref().filter(|i| &i.resource_id == resource) else {
                continue;
            };
            let count = item.count.min(remaining);
            p.sources.push((slot.slot, count));
            remaining -= count;
        }
        if remaining != 0 || p.sources.len() + terms.coins.len() > MAX_LEGS {
            return report(p.session, "Actual participant goods cannot close current deficit within native leg bound; no items or budget consumed.");
        }
        p.supplying = true;
        p.source_index = 0;
        self.pc_placement(p)
    }
    fn pc_prepare_money(&mut self, mut p: Pending) -> Vec<Command> {
        let mut remaining = p.terms.as_ref().expect("terms").reward;
        p.sources.clear();
        // The native hold reserves currency quantities, not a frozen slot alias.
        // Choose current REAL stacks; the atomic owner preserves their components.
        for slot in &p.stock.as_ref().expect("stock").slots {
            if remaining == 0 {
                break;
            }
            let Some(item) = slot
                .item
                .as_ref()
                .filter(|item| item.resource_id == "minecraft:emerald")
            else {
                continue;
            };
            let count = item.count.min(remaining);
            p.sources.push((slot.slot, count));
            remaining -= count;
        }
        if remaining != 0 || p.sources.len() + p.legs.len() > MAX_LEGS {
            return report(p.session, "Current physical allocated currency cannot fit the native exchange; no goods or budget consumed.");
        }
        p.source_index = 0;
        p.supplying = false;
        self.pc_placement(p)
    }
    fn pc_placement(&mut self, mut p: Pending) -> Vec<Command> {
        if p.source_index == p.sources.len() {
            if p.supplying {
                return self.pc_prepare_money(p);
            }
            return self.pc_intent(p);
        }
        let (source, destination) = if p.supplying {
            (
                p.player_inventory.as_ref().expect("player"),
                p.stock.as_ref().expect("stock"),
            )
        } else {
            (
                p.stock.as_ref().expect("stock"),
                p.player_inventory.as_ref().expect("player"),
            )
        };
        let Some(request) = self.next_storage_request("player-room") else {
            return report(p.session, "Request sequence exhausted.");
        };
        let command = query_inventory_placement_room(
            &request,
            policy::expected(source),
            p.sources[p.source_index].0,
            policy::expected(destination),
        );
        p.request = request;
        p.stage = Stage::Placement;
        self.commerce_player_runtime.pending = Some(p);
        vec![command]
    }
    fn pc_apply_room(
        &mut self,
        mut p: Pending,
        room: &inventories::InventoryPlacementRoom,
    ) -> Vec<Command> {
        let (source, destination) = if p.supplying {
            (
                p.player_inventory.as_ref().expect("player"),
                p.stock.as_ref().expect("stock"),
            )
        } else {
            (
                p.stock.as_ref().expect("stock"),
                p.player_inventory.as_ref().expect("player"),
            )
        };
        let (source_slot, mut left) = p.sources[p.source_index];
        if !policy::same_endpoint(&room.source.endpoint, &source.endpoint)
            || !policy::same_endpoint(&room.destination.endpoint, &destination.endpoint)
            || room.source_slot != source_slot
            || room.source.fence.revision != source.fence.revision
            || room.source.fence.snapshot_hash != source.fence.snapshot_hash
            || room.destination.fence.revision != destination.fence.revision
            || room.destination.fence.snapshot_hash != destination.fence.snapshot_hash
        {
            return report(
                p.session,
                "Native placement fences changed; no exchange issued.",
            );
        }
        let stack = source
            .slots
            .iter()
            .find(|s| s.slot == source_slot)
            .and_then(|s| s.item.as_ref())
            .expect("actual source");
        let source_stack = caravan::Stack::from_native(stack);
        for slot in &room.slots {
            if left == 0 {
                break;
            }
            let existing_legs = p.legs.iter().filter(|l| {
                policy::same_endpoint(&l.destination, &destination.endpoint)
                    && l.destination_slot == slot.slot
            });
            if existing_legs.clone().any(|l| {
                let prior_source = if policy::same_endpoint(&l.source, &source.endpoint) {
                    source
                } else {
                    destination
                };
                prior_source
                    .slots
                    .iter()
                    .find(|s| s.slot == l.source_slot)
                    .and_then(|s| s.item.as_ref())
                    .is_none_or(|i| !source_stack.matches(i))
            }) {
                continue;
            }
            let allocated = existing_legs.map(|l| l.count).sum::<u32>();
            let fully_spent = !p.supplying
                && destination
                    .slots
                    .iter()
                    .find(|s| s.slot == slot.slot)
                    .and_then(|s| s.item.as_ref())
                    .is_some_and(|item| {
                        p.legs
                            .iter()
                            .filter(|leg| {
                                policy::same_endpoint(&leg.source, &destination.endpoint)
                                    && leg.source_slot == slot.slot
                            })
                            .map(|leg| leg.count)
                            .sum::<u32>()
                            == item.count
                    });
            // A previous actual goods leg can empty this slot in the SAME atomic
            // exchange. This is a candidate, not invented native capacity: the
            // owner checks its canonical post-leg placement before any commit.
            let planned_room = if fully_spent {
                slot.room.max(stack.count)
            } else {
                slot.room
            };
            let count = left.min(planned_room.saturating_sub(allocated));
            if count == 0 {
                continue;
            }
            if p.legs.len() == MAX_LEGS {
                return report(p.session, "Exact component placement exceeds native leg bound; no partial transfer or reward.");
            }
            p.legs.push(inventories::OwnedItemTransfer {
                source: source.endpoint.clone(),
                source_slot,
                destination: destination.endpoint.clone(),
                destination_slot: slot.slot,
                count,
            });
            left -= count;
        }
        if left != 0 {
            return report(p.session, "Actual destination has insufficient native component-aware room; clear room before fulfillment. No goods or reward consumed.");
        }
        p.source_index += 1;
        self.pc_placement(p)
    }
    fn pc_intent(&mut self, mut p: Pending) -> Vec<Command> {
        let terms = p.terms.as_ref().expect("terms");
        let head = p.head.as_mut().expect("head");
        let participant = head.participant.clone().expect("participant");
        let reservation = head.reservation.as_ref().expect("actual reservation");
        head.attempt = match head.attempt.checked_add(1) {
            Some(n) => n,
            None => return report(p.session, "Operation sequence exhausted."),
        };
        let mut guards = Vec::new();
        let Some(version) = p.registry_version.filter(|v| *v > 0) else {
            return report(p.session, "Current genuine registry fence required.");
        };
        guards.push(caravan::StorageGuard {
            key: registry_storage::ROOT_KEY.to_owned(),
            version,
        });
        let permit = if let Some(escort) = &head.escort {
            let Some(caravan_version) = p.caravan_version.filter(|v| *v > 0) else {
                return report(p.session, "Original committed caravan fence required.");
            };
            guards.push(caravan::StorageGuard {
                key: caravan_runtime::KEY.to_owned(),
                version: caravan_version,
            });
            Some(types::NpcExecutionPermit {
                route_ref: escort.route_ref.clone(),
                executor_epoch: escort.epoch,
            })
        } else {
            None
        };
        let delivery = match &head.escort {
            Some(escort) => {
                match policy::escort_delivery(terms, escort, p.caravan.as_ref().expect("caravan")) {
                    Ok(gross) => match serde_json::to_value(gross) {
                        Ok(gross) => Some(gross),
                        Err(_) => {
                            return report(
                                p.session,
                                "Full committed gross delivery serialization refused.",
                            )
                        }
                    },
                    Err(reason) => return report(p.session, reason),
                }
            }
            None => None,
        };
        let mut expected_revisions = vec![
            policy::expected(p.player_inventory.as_ref().expect("player")),
            policy::expected(p.stock.as_ref().expect("stock")),
        ];
        // Native endpoint order is player then container; no aliases or extra endpoints.
        expected_revisions.sort_unstable_by_key(|e| match e.endpoint {
            InventoryEndpoint::PlayerInventory(_) => 0,
            _ => 1,
        });
        let intent = policy::Intent {
            operation: inventories::InventoryConsumeReservation {
                operation_id: format!("pc_{}_pay_{:x}", head.id, head.attempt),
                actor_id: participant.session,
                reservation_ref: reservation.reference.clone(),
                execution_permit: permit,
                expected_reservation_revision: reservation.revision,
                portion: 1,
                resource_plan: terms.plan(),
                transfers: std::mem::take(&mut p.legs),
                expected_revisions,
            },
            participant,
            guards,
            before: vec![
                p.player_inventory.take().expect("player"),
                p.stock.take().expect("stock"),
            ],
            supplied: p.supplied,
            delivery,
        };
        let Ok((blob, chunks)) = Blob::encode(&intent) else {
            return report(
                p.session,
                "Full immutable actual-player exchange intent serialization refused.",
            );
        };
        p.intent = Some(intent);
        self.pc_publish_encoded(p, blob, chunks, After::Intent)
    }
    pub(super) fn player_contract_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_player_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::Registry
                    | Stage::Index
                    | Stage::Head
                    | Stage::Terms
                    | Stage::Caravans
                    | Stage::BlobRead
                    | Stage::ActorReceipt
            )
        {
            self.commerce_player_runtime.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Durable storage unavailable; no new effect.",
            ));
        };
        match p.stage {
            Stage::Registry => {
                let Some(registry) = record.registry.cloned() else {
                    return Some(report(
                        p.session,
                        "Registry unreadable; no policy decision.",
                    ));
                };
                p.registry = registry;
                p.registry_version = record.version;
                Some(self.pc_read(p, Stage::Index, INDEX_KEY.to_owned()))
            }
            Stage::Index => {
                let index = match &record.value {
                    Some(value) => serde_json::from_str(value),
                    None => Ok(Index::default()),
                };
                let Ok(index) = index else {
                    return Some(report(p.session, "Player offer index unreadable."));
                };
                let index: Index = index;
                if index.ids.len() > MAX_OFFERS
                    || index.ids.iter().any(|id| !valid_id(id))
                    || index.ids.windows(2).any(|ids| ids[0] >= ids[1])
                {
                    return Some(report(
                        p.session,
                        "Bounded offer index invalid; no effects.",
                    ));
                }
                p.index = index;
                p.index_version = record.version;
                if p.index_version.is_none() {
                    return Some(self.pc_index_write(p));
                }
                if p.pruning {
                    let id = p.id.as_ref().expect("id");
                    p.index.ids.retain(|old| old != id);
                    return Some(self.pc_index_write(p));
                }
                if matches!(p.action, Action::List { .. }) {
                    p.id = p.index.ids.first().cloned();
                } else if matches!(p.action, Action::Wake) {
                    let now = self.commerce_player_runtime.now;
                    p.id = p
                        .index
                        .ids
                        .iter()
                        .min_by_key(|id| {
                            self.commerce_player_runtime
                                .deadlines
                                .get(*id)
                                .copied()
                                .unwrap_or(0)
                        })
                        .filter(|id| {
                            self.commerce_player_runtime
                                .deadlines
                                .get(*id)
                                .copied()
                                .unwrap_or(0)
                                <= now
                        })
                        .cloned();
                }
                let Some(id) = p.id.clone() else {
                    return Some(if matches!(p.action, Action::List { .. }) {
                        report(p.session, "{\"contracts\":[],\"truncated\":false}")
                    } else {
                        self.pc_finish(p)
                    });
                };
                Some(self.pc_read(p, Stage::Head, key(&id)))
            }
            Stage::Head => {
                p.version = record.version;
                if let Some(value) = record.value {
                    let Ok(head) = serde_json::from_str::<Head>(&value) else {
                        return Some(report(
                            p.session,
                            "Player contract header unreadable; no effects.",
                        ));
                    };
                    if Some(head.id.as_str()) != p.id.as_deref() || head.terms.fragments == 0 {
                        return Some(report(p.session, "Original contract identity mismatch."));
                    }
                    if p.action.create() {
                        return Some(report(
                            p.session,
                            "Contract id already exists; original terms cannot be overwritten.",
                        ));
                    }
                    p.head = Some(head);
                    return Some(self.pc_load_terms(p));
                }
                if !p.action.create() {
                    return Some(self.pc_finish(p));
                }
                if p.index.ids.len() >= MAX_OFFERS {
                    return Some(report(
                        p.session,
                        "Active offer bound reached; no budget allocated.",
                    ));
                }
                let (scope, building, owner) = match &p.action {
                    Action::Supply {
                        scope,
                        building,
                        owner,
                        ..
                    }
                    | Action::Escort {
                        scope,
                        building,
                        owner,
                        ..
                    } => (scope, building, owner),
                    _ => unreachable!(),
                };
                let Some(container) = caravan_runtime::binding(&p.registry, scope, building)
                    .filter(|c| caravan_runtime::authorized(&p.registry, c, owner))
                else {
                    return Some(report(
                        p.session,
                        "Genuine owner-authorized physical stock building required.",
                    ));
                };
                let endpoint = container.endpoint();
                Some(self.pc_inventory(p, Stage::Stock, endpoint))
            }
            Stage::Terms => {
                let head = p.head.as_ref().expect("head");
                let loading_intent = p.terms.is_some() && head.state == State::PayoutIntent;
                let blob = if loading_intent {
                    head.intent.as_ref().expect("intent")
                } else {
                    &head.terms
                };
                let Some(value) = record.value else {
                    return Some(report(
                        p.session,
                        "Original immutable contract/intent fragment absent; no effect.",
                    ));
                };
                p.chunks.push(value.to_owned());
                if p.chunks.len() < blob.fragments {
                    let key = blob.key(p.chunks.len());
                    return Some(self.pc_read(p, Stage::Terms, key));
                }
                if loading_intent {
                    let Ok(intent) = blob.decode::<policy::Intent>(&p.chunks) else {
                        return Some(report(
                            p.session,
                            "Original intent digest mismatch; no effect.",
                        ));
                    };
                    if head.participant.as_ref() != Some(&intent.participant)
                        || intent.operation.actor_id != intent.participant.session
                    {
                        return Some(report(
                            p.session,
                            "Original actual-player intent authority mismatch.",
                        ));
                    }
                    p.intent = Some(intent);
                    return Some(self.pc_status(p));
                }
                let Ok(terms) = blob.decode::<Terms>(&p.chunks) else {
                    return Some(report(p.session, "Original terms digest mismatch."));
                };
                if terms.validate().is_err() || terms.id != head.id || terms.owner != head.owner {
                    return Some(report(p.session, "Original physical terms invalid."));
                }
                p.terms = Some(terms);
                Some(self.pc_drive(p))
            }
            Stage::Caravans => {
                let Some(value) = record.value else {
                    return Some(report(
                        p.session,
                        "Original committed caravan store absent.",
                    ));
                };
                let Ok(store) = serde_json::from_str::<caravan::Store>(&value) else {
                    return Some(report(
                        p.session,
                        "Original committed caravan store unreadable.",
                    ));
                };
                let shipment = match &p.action {
                    Action::Escort { shipment, .. } => shipment.as_str(),
                    _ => match &p.terms.as_ref().expect("terms").need {
                        Need::Escort { shipment, .. } => shipment.as_str(),
                        _ => unreachable!(),
                    },
                };
                let Some(caravan) = store.contracts.get(shipment).cloned() else {
                    return Some(report(
                        p.session,
                        "Exact original caravan absent; no escort reward.",
                    ));
                };
                p.caravan = Some(caravan);
                p.caravan_version = record.version;
                if p.action.create() {
                    return Some(self.pc_create(p));
                }
                if !p
                    .terms
                    .as_ref()
                    .expect("terms")
                    .matches_caravan(p.caravan.as_ref().expect("caravan"))
                {
                    return Some(self.pc_close(
                        p,
                        "Original shipment terms or identity changed; no escort payout.",
                    ));
                }
                if p.head.as_ref().expect("head").state == State::Offered
                    && !matches!(p.action, Action::Accept { .. })
                {
                    let caravan = p.caravan.as_ref().expect("caravan");
                    if caravan.started || caravan.terminal() {
                        if matches!(p.action, Action::Wake) {
                            return Some(self.pc_close(p, "Original caravan departed without an accepted escort; unused budget released."));
                        }
                        if matches!(p.action, Action::List { .. }) {
                            p.head = None;
                            return Some(self.pc_finish(p));
                        }
                        return Some(report(p.session, "No available escort offer: original caravan already departed/completed without an accepted claim."));
                    }
                    return Some(self.pc_finish(p));
                }
                Some(self.pc_route(p))
            }
            Stage::BlobRead => {
                let publication = p.publication.as_ref().expect("publication");
                let expected = &publication.chunks[publication.cursor];
                if let Some(value) = record.value {
                    if value != expected.as_str() {
                        return Some(report(
                            p.session,
                            "Immutable content-addressed fragment conflict; no ACK.",
                        ));
                    }
                    p.publication.as_mut().expect("publication").cursor += 1;
                    return Some(self.pc_blob_step(p));
                }
                let Some(request) = self.next_storage_request("player-blob") else {
                    return Some(report(p.session, "Request sequence exhausted."));
                };
                let command = storage_cas(
                    &request,
                    &publication.blob.key(publication.cursor),
                    record.version,
                    expected.clone(),
                );
                p.request = request;
                p.stage = Stage::BlobCas;
                self.commerce_player_runtime.pending = Some(p);
                Some(vec![command])
            }
            Stage::ActorReceipt => {
                let head = p.head.as_ref().expect("head");
                let blob = head.receipt.as_ref().expect("receipt");
                let Some(value) = record.value else {
                    return Some(report(
                        p.session,
                        "Complete original receipt fragment missing; no substitute.",
                    ));
                };
                p.chunks.push(value.to_owned());
                if p.chunks.len() < blob.fragments {
                    let key = blob.key(p.chunks.len());
                    return Some(self.pc_read(p, Stage::ActorReceipt, key));
                }
                let Ok(receipt) = blob.decode::<PaidReceipt>(&p.chunks) else {
                    return Some(report(
                        p.session,
                        "Original complete receipt digest mismatch.",
                    ));
                };
                if p.action.player() != Some(receipt.participant.player.as_str()) {
                    return Some(report(p.session, "Actual receipt participant only."));
                }
                let OperationPayload::OwnedInventory(InventoryResult::ReservedTransfer(native)) =
                    &receipt.native.payload
                else {
                    return Some(report(
                        p.session,
                        "Original physical receipt payload invalid.",
                    ));
                };
                let Some(own_inventory) = native.inventories.iter().find(|s| {
                    policy::same_endpoint(
                        &s.endpoint,
                        &InventoryEndpoint::PlayerInventory(receipt.participant.session),
                    )
                }) else {
                    return Some(report(
                        p.session,
                        "Original paid player after-image missing.",
                    ));
                };
                Some(report(p.session, serde_json::json!({"id":head.id,"operation":receipt.operation,"participant":receipt.participant,
                    "decision_revision":receipt.native.revision,"terms_digest":receipt.terms_digest,"intent_digest":receipt.intent_digest,
                    "receipt_digest":blob.digest,"own_inventory":own_inventory}).to_string()))
            }
            _ => {
                self.commerce_player_runtime.pending = Some(p);
                None
            }
        }
    }
    fn pc_create(&mut self, mut p: Pending) -> Vec<Command> {
        let (id, owner, scope, building, reward) = match &p.action {
            Action::Supply {
                id,
                owner,
                scope,
                building,
                reward,
                ..
            }
            | Action::Escort {
                id,
                owner,
                scope,
                building,
                reward,
                ..
            } => (id, owner, scope, building, *reward),
            _ => unreachable!(),
        };
        let Some(stock) = caravan_runtime::binding(&p.registry, scope, building)
            .filter(|c| caravan_runtime::authorized(&p.registry, c, owner))
        else {
            return report(p.session, "Actual original stock authority missing.");
        };
        let snapshot = p.stock.as_ref().expect("stock");
        if !policy::same_endpoint(&snapshot.endpoint, &stock.endpoint()) {
            return report(p.session, "Native returned another container; no offer.");
        }
        let Ok(coins) = policy::allocate_coins(snapshot, reward) else {
            return report(
                p.session,
                "Actual physical budget insufficient or outside native leg bounds.",
            );
        };
        let need = match &p.action {
            Action::Supply {
                resource, target, ..
            } => Need::Supply {
                resource: resource.clone(),
                target: *target,
            },
            Action::Escort {
                shipment,
                radius,
                sample,
                ..
            } => {
                let c = p.caravan.as_ref().expect("caravan");
                if c.owner() != owner
                    || c.destination != stock
                    || c.terminal()
                    || c.started
                    || c.death_ref.is_some()
                {
                    return report(p.session, "Owner's genuine undelivered caravan and its actual destination budget container required.");
                }
                Need::Escort {
                    shipment: shipment.clone(),
                    carrier: c.carrier.clone(),
                    source: c.source.clone(),
                    item: c.item.clone(),
                    quantity: c.quantity,
                    radius: *radius,
                    sample_ticks: *sample,
                }
            }
            _ => unreachable!(),
        };
        let Some(registry_version) = p.registry_version.filter(|v| *v > 0) else {
            return report(p.session, "Committed genuine registry fence required.");
        };
        let guards = vec![caravan::StorageGuard {
            key: registry_storage::ROOT_KEY.to_owned(),
            version: registry_version,
        }];
        let terms = Terms {
            id: id.clone(),
            owner: owner.clone(),
            stock: stock.clone(),
            need: need.clone(),
            reward,
            coins,
            reserve_operation: format!("pc_{id}_reserve"),
            stock_fence: snapshot.fence.clone(),
            guards,
        };
        if let Err(reason) = terms.validate() {
            return report(p.session, reason);
        }
        if matches!(need, Need::Supply { .. }) && terms.remaining(snapshot) == Ok(0) {
            return report(p.session, "Required stock already present; no fictitious contract or reward budget allocated.");
        }
        let Ok((blob, _)) = Blob::encode(&terms) else {
            return report(p.session, "Complete terms serialization refused.");
        };
        p.head = Some(Head {
            id: id.clone(),
            owner: owner.clone(),
            terms: blob,
            public: Offer {
                id: id.clone(),
                owner: owner.clone(),
                location: stock,
                need,
                fixed_emerald_reward: reward,
            },
            state: State::ReserveIntent,
            participant: None,
            reservation: None,
            escort: None,
            intent: None,
            receipt: None,
            release_operation: None,
            blocked: None,
            attempt: 0,
        });
        p.terms = Some(terms.clone());
        self.pc_publish(p, &terms, After::Create)
    }
    pub(super) fn player_contract_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_player_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::BlobCas | Stage::HeadCas | Stage::IndexCas)
        {
            self.commerce_player_runtime.pending = Some(p);
            return None;
        }
        if p.stage == Stage::BlobCas {
            if matches!(answer.outcome(), StorageCasOutcome::Committed(_)) {
                p.publication.as_mut().expect("publication").cursor += 1;
                return Some(self.pc_blob_step(p));
            }
            let publication = p.publication.as_ref().expect("publication");
            let key = publication.blob.key(publication.cursor);
            return Some(self.pc_read(p, Stage::BlobRead, key));
        }
        let StorageCasOutcome::Committed(version) = answer.outcome() else {
            return Some(report(
                p.session,
                "Header/index CAS refused; no new native effect. Original operation IDs retained.",
            ));
        };
        if p.stage == Stage::IndexCas {
            p.index_version = Some(*version);
            if p.head.is_none() {
                return Some(match p.id.clone() {
                    Some(id) => self.pc_read(p, Stage::Head, key(&id)),
                    None => self.pc_finish(p),
                });
            }
            return Some(if p.pruning {
                self.pc_finish(p)
            } else {
                self.pc_status(p)
            });
        }
        p.version = Some(*version);
        let head = p.head.as_ref().expect("head");
        match head.state {
            State::ReserveIntent => Some(self.pc_ensure_index(p)),
            State::PayoutIntent => Some(self.pc_status(p)),
            State::ReleaseIntent => Some(self.pc_status(p)),
            State::Paid | State::Closed => {
                self.commerce_player_runtime.deadlines.remove(&head.id);
                p.pruning = true;
                Some(self.pc_read(p, Stage::Index, INDEX_KEY.to_owned()))
            }
            _ => Some(self.pc_finish(p)),
        }
    }
    pub(super) fn player_contract_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_player_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(
                p.stage,
                Stage::Stock
                    | Stage::PlayerInventory
                    | Stage::Route
                    | Stage::Placement
                    | Stage::OperationStatus
                    | Stage::Native
            )
        {
            self.commerce_player_runtime.pending = Some(p);
            return None;
        }
        if p.stage == Stage::OperationStatus {
            if matches!(answer.outcome(), OperationOutcome::Refused(r) if matches!(r.reason, OperationFailure::NotFound))
            {
                if p.head.as_ref().expect("head").state == State::PayoutIntent {
                    return Some(self.pc_players(p));
                }
                return Some(self.pc_native(p));
            }
            p.stage = Stage::Native;
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            if matches!(answer.outcome(), OperationOutcome::Refused(r) if matches!(r.reason, OperationFailure::RuntimeUnavailable))
            {
                return Some(report(p.session, "Native decision unavailable; original operation must be resolved before replan or reconnect."));
            }
            if p.stage == Stage::Native {
                let head = p.head.as_mut().expect("head");
                if head.state == State::PayoutIntent {
                    head.state = State::Claimed;
                    head.intent = None;
                    head.blocked = Some(format!(
                        "Known native refusal {:?}; no reward paid",
                        answer.outcome
                    ));
                    return Some(self.pc_head_write(p));
                }
                if head.state == State::ReserveIntent {
                    head.state = State::Closed;
                    head.blocked = Some(format!(
                        "Native budget reservation refused {:?}",
                        answer.outcome
                    ));
                    return Some(self.pc_head_write(p));
                }
            }
            return Some(report(
                p.session,
                format!(
                    "Native player contract refusal {:?}; no new items or currency",
                    answer.outcome
                ),
            ));
        };
        match p.stage {
            Stage::Stock | Stage::PlayerInventory => {
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Whole actual native inventory snapshot required.",
                    ));
                };
                if p.stage == Stage::Stock {
                    p.stock = Some(snapshot.clone());
                    if p.action.create() {
                        if matches!(p.action, Action::Escort { .. }) {
                            return Some(self.pc_read(
                                p,
                                Stage::Caravans,
                                caravan_runtime::KEY.to_owned(),
                            ));
                        }
                        return Some(self.pc_create(p));
                    }
                    if !policy::same_endpoint(
                        &snapshot.endpoint,
                        &p.terms.as_ref().expect("terms").stock.endpoint(),
                    ) {
                        return Some(report(p.session, "Not original advertised physical stock."));
                    }
                    return Some(self.pc_after_stock(p));
                }
                let participant = p
                    .head
                    .as_ref()
                    .expect("head")
                    .participant
                    .as_ref()
                    .expect("participant");
                if !policy::same_endpoint(
                    &snapshot.endpoint,
                    &InventoryEndpoint::PlayerInventory(participant.session),
                ) {
                    return Some(report(
                        p.session,
                        "Actual player inventory/session mismatch.",
                    ));
                }
                p.player_inventory = Some(snapshot.clone());
                Some(self.pc_plan_supply(p))
            }
            Stage::Placement => {
                let OperationPayload::OwnedInventory(InventoryResult::PlacementRoom(room)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Actual component-aware native placement answer required.",
                    ));
                };
                Some(self.pc_apply_room(p, room))
            }
            Stage::Route => {
                let OperationPayload::NpcRoute(npc_routes::NpcRouteResult::Status(route)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Original native route observation required.",
                    ));
                };
                p.route = Some(route.clone());
                Some(self.pc_players(p))
            }
            Stage::Native => {
                let head = p.head.as_mut().expect("head");
                let expected_operation = match head.state {
                    State::ReserveIntent => {
                        p.terms.as_ref().expect("terms").reserve_operation.as_str()
                    }
                    State::PayoutIntent => p
                        .intent
                        .as_ref()
                        .expect("intent")
                        .operation
                        .operation_id
                        .as_str(),
                    State::ReleaseIntent => head.release_operation.as_deref().expect("release"),
                    _ => {
                        return Some(report(
                            p.session,
                            "No native effect expected in this contract state.",
                        ))
                    }
                };
                if answer.operation_id.as_deref() != Some(expected_operation) {
                    return Some(report(
                        p.session,
                        "Original durable operation id mismatch; no ACK.",
                    ));
                }
                if head.state == State::PayoutIntent {
                    let OperationPayload::OwnedInventory(InventoryResult::ReservedTransfer(
                        receipt,
                    )) = &committed.payload
                    else {
                        return Some(report(p.session, "Complete atomic native exchange receipt required; original intent retained."));
                    };
                    let terms = p.terms.as_ref().expect("terms");
                    let intent = p.intent.as_ref().expect("intent");
                    if receipt.reservation.resource_plan_hash
                        != head.reservation.as_ref().expect("reservation").plan_hash
                    {
                        return Some(report(
                            p.session,
                            "Original reserved budget plan hash mismatch; no ACK.",
                        ));
                    }
                    if let Err(reason) = policy::validate_paid_receipt(terms, intent, receipt) {
                        return Some(report(p.session, reason));
                    }
                    let paid = PaidReceiptRef {
                        participant: &intent.participant,
                        terms_digest: &head.terms.digest,
                        intent_digest: &head.intent.as_ref().expect("intent blob").digest,
                        operation: &intent.operation.operation_id,
                        native: committed,
                    };
                    let Ok((blob, chunks)) = Blob::encode(&paid) else {
                        return Some(report(
                            p.session,
                            "Complete native receipt serialization refused; no ACK.",
                        ));
                    };
                    return Some(self.pc_publish_encoded(p, blob, chunks, After::Receipt));
                }
                let OperationPayload::OwnedInventory(InventoryResult::Reservation(reservation)) =
                    &committed.payload
                else {
                    return Some(report(
                        p.session,
                        "Original native budget reservation receipt required.",
                    ));
                };
                if !policy::same_endpoint(
                    &reservation.endpoint,
                    &p.terms.as_ref().expect("terms").stock.endpoint(),
                ) {
                    return Some(report(p.session, "Actual budget endpoint mismatch."));
                }
                if head.state == State::ReserveIntent {
                    if reservation.released
                        || reservation.receipt_watermark != 0
                        || reservation.quantities.len() != 1
                        || reservation.quantities[0].resource_id != "minecraft:emerald"
                        || reservation.quantities[0].remaining
                            != u64::from(p.terms.as_ref().expect("terms").reward)
                        || reservation.quantities[0].consumed != 0
                        || reservation.quantities[0].returned != 0
                    {
                        return Some(report(
                            p.session,
                            "Actual allocated budget receipt mismatch.",
                        ));
                    }
                    head.reservation = Some(Reservation {
                        reference: reservation.reservation_ref.clone(),
                        revision: committed.revision,
                        plan_hash: reservation.resource_plan_hash.clone(),
                    });
                    head.state = State::Offered;
                } else {
                    if !reservation.released
                        || reservation
                            .quantities
                            .iter()
                            .any(|q| q.remaining != 0 || q.consumed != 0)
                    {
                        return Some(report(
                            p.session,
                            "Unused native budget release receipt mismatch.",
                        ));
                    }
                    head.state = State::Closed;
                }
                Some(self.pc_head_write(p))
            }
            _ => {
                self.commerce_player_runtime.pending = Some(p);
                None
            }
        }
    }
    pub(super) fn player_contract_players_answer(
        &mut self,
        answer: &events::OnlinePlayersAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.commerce_player_runtime.pending.take()?;
        if p.stage != Stage::Players || p.request != answer.request {
            self.commerce_player_runtime.pending = Some(p);
            return None;
        }
        let head = p.head.as_ref().expect("head");
        let participant = head.participant.as_ref().expect("participant");
        if head.state == State::PayoutIntent {
            let same_actor = answer.players.iter().any(|player| {
                player.player == participant.player && player.session == participant.session
            });
            let old_session_absent = !answer.truncated
                && !answer
                    .players
                    .iter()
                    .any(|player| player.session == participant.session);
            if same_actor || old_session_absent {
                // A genuinely ended old session is resolved by the native owner,
                // producing its durable refusal, never an invented plugin outcome.
                return Some(self.pc_native(p));
            }
            return Some(report(p.session, "Original actual-player session witness unavailable or belongs to another UUID; immutable intent retained without payout."));
        }
        let Some(player) = answer.players.iter().find(|player| {
            player.player == participant.player && player.session == participant.session
        }) else {
            return Some(self.pc_close(
                p,
                if answer.truncated {
                    "Bounded actual online player observation incomplete; escort fails closed."
                } else {
                    "Original live escort participant disconnected; unused reward released."
                },
            ));
        };
        let terms = p.terms.as_ref().expect("terms");
        let previous = head.escort.as_ref();
        let route = p.route.as_ref().expect("route");
        let now = self.commerce_player_runtime.now;
        // Once a genuine arrival checkpoint is durably sampled, only the same
        // live player near its original arrived carrier may claim the gross unload.
        if let Some(previous) = previous.filter(|e| e.arrival.is_some()) {
            if player.dimension != terms.stock.dimension
                || route.route_ref != previous.route_ref
                || route.entity_uuid != previous.entity_uuid
                || route.executor_epoch != previous.epoch
                || route.arrival.as_ref().map(|a| a.receipt_ref.as_str())
                    != previous.arrival.as_deref()
            {
                return Some(self.pc_close(
                    p,
                    "Original escort arrival or actor changed before gross payout.",
                ));
            }
            if matches!(p.action, Action::Fulfill { .. } | Action::Wake) {
                if let Err(reason) =
                    policy::escort_delivery(terms, previous, p.caravan.as_ref().expect("caravan"))
                {
                    return Some(report(p.session, reason));
                }
                let endpoint = InventoryEndpoint::PlayerInventory(participant.session);
                return Some(self.pc_inventory(p, Stage::PlayerInventory, endpoint));
            }
            return Some(self.pc_finish(p));
        }
        let checkpoint = Checkpoint {
            player,
            route,
            simulation_tick: now,
            previous: previous.map(|e| &e.checkpoint),
        };
        let Ok((blob, chunks)) = Blob::encode(&checkpoint) else {
            return Some(report(
                p.session,
                "Full genuine checkpoint serialization refused.",
            ));
        };
        let escort = match policy::observe_escort(
            terms,
            participant,
            p.caravan.as_ref().expect("caravan"),
            route,
            player,
            now,
            previous,
            blob.clone(),
        ) {
            Ok(escort) => escort,
            Err(reason) => {
                if matches!(p.action, Action::Accept { .. }) {
                    return Some(report(p.session, reason));
                }
                return Some(self.pc_close(p, reason));
            }
        };
        let after = if previous.is_none() {
            After::Claim
        } else {
            After::Checkpoint
        };
        p.escort = Some(escort);
        Some(self.pc_publish_encoded(p, blob, chunks, after))
    }
}
