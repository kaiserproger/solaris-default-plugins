//! Local knowledge is earned from durable exact accepted native craft receipts.
use super::technology::{Knowledge, Source, Transfer};
use super::*;
use solaris_plugin_sdk::query_npcs;
pub(super) const STORE_KEY: &str = "settlement-technology-v1";
#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub(super) struct Store {
    pub(super) scopes: BTreeMap<String, Knowledge>,
}
#[derive(Clone)]
enum Action {
    Practice {
        scope: String,
        node: String,
        operation: String,
        player: String,
    },
    Transfer {
        source: String,
        destination: String,
        node: String,
        teacher: String,
        operation: String,
        player: String,
    },
    Status {
        scope: String,
        player: String,
    },
    Catalog,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Store,
    Production,
    Teacher,
    Write,
}
struct Pending {
    request: String,
    session: u64,
    action: Action,
    stage: Stage,
    registry: SettlementRegistry,
    store: Store,
    version: Option<u64>,
    teacher: Option<String>,
    production: Option<production_storage::Loader>,
}
fn report(session: u64, text: impl Into<String>) -> Vec<Command> {
    vec![message_session(session, text.into())]
}
pub(super) fn allows(value: Option<&str>, scope: &str, recipe: &str) -> Result<(), String> {
    let definitions = technology::initial_definitions();
    if !definitions
        .values()
        .any(|definition| definition.unlocks(recipe))
    {
        return Ok(());
    }
    technology::validate_initial_catalog()
        .map_err(|e| format!("Invalid technology catalog: {e:?}"))?;
    let store: Store = match value {
        None => Store::default(),
        Some(value) => {
            serde_json::from_str(value).map_err(|_| "Technology store unreadable".to_owned())?
        }
    };
    store
        .scopes
        .get(scope)
        .ok_or_else(|| "Local accepted practice required".to_owned())?
        .can_craft(recipe, &definitions, true)
        .map_err(|e| format!("Local technology gate refused: {e:?}"))
}
impl Settlements {
    pub(super) fn handle_technology_command(
        &mut self,
        invoked: &CommandInvoked,
        _tick: u64,
    ) -> Option<Vec<Command>> {
        if invoked.name != "settlement" {
            return None;
        }
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        if !matches!(
            args.first(),
            Some(&"technology-practice")
                | Some(&"technology-transfer")
                | Some(&"technology-status")
                | Some(&"technology-catalog")
        ) {
            return None;
        }
        if self.technology_runtime.pending.is_some() {
            return Some(report(
                invoked.session,
                "Technology operation busy; no knowledge changed.",
            ));
        }
        let action=match args.as_slice(){
            ["technology-practice",scope,node,operation]=>Action::Practice{scope:(*scope).into(),node:(*node).into(),operation:(*operation).into(),player:invoked.player.clone()},
            ["technology-transfer",source,destination,node,teacher,operation]=>Action::Transfer{source:(*source).into(),destination:(*destination).into(),node:(*node).into(),teacher:(*teacher).into(),operation:(*operation).into(),player:invoked.player.clone()},
            ["technology-status",scope]=>Action::Status{scope:(*scope).into(),player:invoked.player.clone()},
            ["technology-catalog"]=>Action::Catalog,
            _=>return Some(report(invoked.session,"Usage: technology-practice <settlement> <authored-node> <accepted-production-op>; technology-transfer <source> <destination> <node> <resident-teacher-at-destination> <stable-operation-id>; technology-status <settlement>; technology-catalog"))
        };
        if technology::validate_initial_catalog().is_err() {
            return Some(report(
                invoked.session,
                "Canonical technology graph invalid; no operations admitted.",
            ));
        }
        if matches!(action, Action::Catalog) {
            let authored = technology::initial_definitions()
                .values()
                .map(|definition| {
                    format!(
                        "{} parents={:?} practice={:?} unlock={:?}",
                        definition.id,
                        definition.parents,
                        definition.practice_recipes,
                        definition.unlock_recipes
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Some(report(invoked.session,format!("Authored: {authored}. Canonical unresolved authoring: {}. Unauthored nodes have no executable unlock.",technology::missing_authoring().join(","))));
        }
        let Some(request) = self.next_storage_request("technology-registry") else {
            return Some(report(invoked.session, "Request sequence exhausted."));
        };
        self.technology_runtime.pending = Some(Pending {
            request: request.clone(),
            session: invoked.session,
            action,
            stage: Stage::Registry,
            registry: SettlementRegistry::default(),
            store: Store::default(),
            version: None,
            teacher: None,
            production: None,
        });
        Some(self.registry_read(&request))
    }
    pub(super) fn technology_init(&mut self) -> Vec<Command> {
        self.technology_runtime.pending = None;
        vec![]
    }
    fn technology_read(&mut self, mut p: Pending, stage: Stage, key: &str) -> Vec<Command> {
        let Some(request) = self.next_storage_request("technology-read") else {
            return report(
                p.session,
                "Request sequence exhausted; no knowledge changed.",
            );
        };
        p.request = request.clone();
        p.stage = stage;
        self.technology_runtime.pending = Some(p);
        vec![storage_get(&request, key)]
    }
    fn technology_write(&mut self, mut p: Pending) -> Vec<Command> {
        let Ok(value) = serde_json::to_string(&p.store) else {
            return report(p.session, "Knowledge serialization refused.");
        };
        let Some(request) = self.next_storage_request("technology-write") else {
            return report(p.session, "Request sequence exhausted.");
        };
        p.request = request.clone();
        p.stage = Stage::Write;
        let command = storage_cas(&request, STORE_KEY, p.version, value);
        self.technology_runtime.pending = Some(p);
        vec![command]
    }
    pub(super) fn technology_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let mut p = self.technology_runtime.pending.take()?;
        if p.request != answer.request()
            || !matches!(p.stage, Stage::Registry | Stage::Store | Stage::Production)
        {
            self.technology_runtime.pending = Some(p);
            return None;
        }
        let Some(record) = answer.record() else {
            return Some(report(
                p.session,
                "Technology storage unavailable; no progress manufactured.",
            ));
        };
        if p.stage == Stage::Registry {
            p.registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => return Some(report(p.session, "Settlement registry unreadable.")),
            };
            let scopes: Vec<(&str, &str)> = match &p.action {
                Action::Practice { scope, player, .. } | Action::Status { scope, player } => {
                    vec![(scope, player)]
                }
                Action::Transfer {
                    source,
                    destination,
                    player,
                    ..
                } => vec![(source, player), (destination, player)],
                Action::Catalog => vec![],
            };
            if !scopes.into_iter().all(|(scope, player)| {
                p.registry
                    .settlements
                    .get(scope)
                    .is_some_and(|s| can_build_settlement(s, player))
            }) {
                return Some(report(
                    p.session,
                    "Current rights in every knowledge scope required.",
                ));
            }
            return Some(self.technology_read(p, Stage::Store, STORE_KEY));
        }
        if p.stage == Stage::Production {
            let loader = p.production.as_mut().expect("production evidence reader");
            if let Err(reason) = loader.answer(production_storage::Record {
                value: record.value.map(str::to_owned),
                version: record.version,
            }) {
                return Some(report(p.session, reason));
            }
            if let Some(key) = loader.next() {
                let key = key.to_owned();
                return Some(self.technology_read(p, Stage::Production, &key));
            }
            let snapshot = match p.production.take().expect("production reader").finish() {
                Ok(snapshot) => snapshot,
                Err(reason) => return Some(report(p.session, reason)),
            };
            let journal: production::Journal = match serde_json::from_value(snapshot.journal) {
                Ok(journal) => journal,
                Err(_) => return Some(report(p.session, "production_store_unreadable")),
            };
            let Action::Practice {
                scope,
                node,
                operation,
                ..
            } = &p.action
            else {
                unreachable!()
            };
            let Some(receipt) = journal.exact_practice(scope, operation) else {
                return Some(report(p.session,"Production evidence pending/refused or net-only: exact accepted recipe revision, portion and gross output required; no technology progress."));
            };
            let Some(settlement) = p.registry.settlements.get(scope) else {
                return Some(report(p.session, "Practice scope missing."));
            };
            if !settlement
                .residents
                .values()
                .any(|resident| resident.handle == receipt.worker)
            {
                return Some(report(
                    p.session,
                    "Accepted worker is not attached to this local settlement; no local practice.",
                ));
            }
            let definitions = technology::initial_definitions();
            let Some(definition) = definitions.get(node) else {
                return Some(report(p.session,format!("Technology {node} lacks genuine authored recipe/practice definition; no fabricated unlock.")));
            };
            if !definition.practice_recipes.contains(&receipt.recipe) {
                return Some(report(
                    p.session,
                    "Accepted production is not this technology's local practice.",
                ));
            }
            let source = Source::LocalProject {
                worker: receipt.worker.clone(),
                project: receipt.operation_id.clone(),
            };
            let knowledge = p.store.scopes.entry(scope.clone()).or_default();
            if let Err(reason) = knowledge
                .learn(node, source, &definitions)
                .and_then(|_| knowledge.accept_practice(node, receipt, &definitions))
            {
                return Some(report(p.session, format!("Practice refused: {reason:?}")));
            }
            return Some(self.technology_write(p));
        }
        p.store = match record.value.as_deref() {
            None => Store::default(),
            Some(value) => match serde_json::from_str(value) {
                Ok(value) => value,
                Err(_) => return Some(report(p.session, "Knowledge store unreadable; no reset.")),
            },
        };
        p.version = record.version;
        match p.action.clone(){
        Action::Practice{operation,..}=>{
            let loader = match production_storage::Loader::job(&operation) {
                Ok(loader) => loader, Err(reason) => return Some(report(p.session, reason)),
            };
            let key = loader.next().expect("production root").to_owned();
            p.production = Some(loader);
            Some(self.technology_read(p,Stage::Production,&key))
        },
        Action::Status{scope,..}=>Some(report(p.session,match p.store.scopes.get(&scope){None=>format!("Technology {scope}: no accepted local practice"),Some(k)=>format!("Technology {scope} revision {}: {}; knowledge transfers {}. Later {} canonical node definitions unresolved, not unlocked.",k.revision,k.records.iter().map(|(id,r)|format!("{id}: accepted-portions={}",r.accepted_practice.len())).collect::<Vec<_>>().join(", "),k.transfers.len(),technology::missing_authoring().len())})),
        Action::Transfer{source,destination,node,teacher,operation,..}=>{
            if operation.is_empty()||source==destination{return Some(report(p.session,"Knowledge transfer needs distinct scopes and stable operation id."));}
            if let Some(saved) = p.store.scopes.get(&destination)
                .and_then(|knowledge| knowledge.transfers.get(&operation)) {
                let handle = p.registry.settlements.get(&destination)
                    .and_then(|settlement| settlement.residents.get(&teacher))
                    .map(|resident| resident.handle.as_str());
                let same_teacher = matches!(&saved.source, Source::Teacher { worker, .. }
                    if Some(worker.as_str()) == handle);
                return Some(report(p.session, if saved.source_scope == source
                    && saved.destination_scope == destination && saved.technology == node
                    && same_teacher { "Knowledge operation durably accepted." }
                    else { "Knowledge transfer refused: OperationConflict" }));
            }
            let Some(origin)=p.store.scopes.get(&source).filter(|k|k.records.get(&node).is_some_and(|r|!r.accepted_practice.is_empty()))else{return Some(report(p.session,"Source lacks genuine accepted local mastery."));};
            let Some(resident)=p.registry.settlements.get(&destination).and_then(|s|s.residents.get(&teacher))else{return Some(report(p.session,"Teacher must be a genuine resident at destination, not an invented carrier."));};
            let handle=resident.handle.clone();
            if !origin.records[&node].accepted_practice.values().any(|receipt|receipt.worker==handle){return Some(report(p.session,"This resident did not carry source accepted practice; trophy possession does not transfer knowledge."));}
            let Some(request)=self.next_storage_request("technology-teacher")else{return Some(report(p.session,"Request sequence exhausted."));};p.request=request.clone();p.stage=Stage::Teacher;p.teacher=Some(handle.clone());self.technology_runtime.pending=Some(p);Some(vec![query_npcs(&request,&[handle],None)])
        }
        Action::Catalog=>Some(vec![])
    }
    }
    pub(super) fn technology_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut p = self.technology_runtime.pending.take()?;
        if p.request != answer.request() || p.stage != Stage::Teacher {
            self.technology_runtime.pending = Some(p);
            return None;
        }
        let OperationOutcome::Committed(committed) = answer.outcome() else {
            return Some(report(
                p.session,
                "Teacher unavailable; no knowledge transfer.",
            ));
        };
        let OperationPayload::Npc(NpcResult::Page(page)) = &committed.payload else {
            return Some(report(
                p.session,
                "Teacher native response mismatch; no knowledge transfer.",
            ));
        };
        let Some(handle) = p.teacher.as_ref() else {
            return Some(report(p.session, "Teacher identity missing."));
        };
        let live = page.npcs.iter().any(|npc| {
            npc.handle == *handle
                && matches!(
                    npc.lifecycle,
                    npcs::NpcLifecycle::AliveLoaded | npcs::NpcLifecycle::AliveUnloaded
                )
        });
        let Action::Transfer {
            source,
            destination,
            node,
            operation,
            ..
        } = &p.action
        else {
            unreachable!()
        };
        let origin = p.store.scopes.get(source).expect("source mastery").clone();
        let transfer = Transfer {
            technology: node.clone(),
            source_scope: source.clone(),
            source_revision: origin.revision,
            destination_scope: destination.clone(),
            source: Source::Teacher {
                worker: handle.clone(),
                origin_scope: source.clone(),
                origin_revision: origin.revision,
            },
        };
        if let Err(reason) = p
            .store
            .scopes
            .entry(destination.clone())
            .or_default()
            .receive(
                operation,
                transfer,
                &origin,
                live,
                &technology::initial_definitions(),
            )
        {
            return Some(report(
                p.session,
                format!("Knowledge transfer refused: {reason:?}"),
            ));
        }
        Some(self.technology_write(p))
    }
    pub(super) fn technology_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        let p = self.technology_runtime.pending.take()?;
        if p.request != answer.request() || p.stage != Stage::Write {
            self.technology_runtime.pending = Some(p);
            return None;
        }
        match answer.outcome(){StorageCasOutcome::Committed(_)=>Some(report(p.session,"Knowledge operation durably accepted. Actual local practice and live native station remain required for gated manufacture.")),StorageCasOutcome::Refused=>Some(self.technology_read(p,Stage::Store,STORE_KEY)),_=>Some(report(p.session,"Knowledge CAS unavailable; operation did not create free progress."))}
    }
}
