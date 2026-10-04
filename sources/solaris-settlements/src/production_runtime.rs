//! Write-ahead production scheduling and native receipt recovery.
//! Simulation deadlines decide when to request work, never how many items exist.
use super::production::{self, Intent, ItemChange, Journal, Outcome, Receipt, Station};
use super::production_storage::{self as disk, Loader, Record, Snapshot, Step, Writer};
use super::*;
use solaris_plugin_sdk::{commands::OperationStatus, query_npc_controls, schedule_timer};

#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
    journal: Option<Journal>,
    now: u64,
    stop: stop_work::Runtime,
    observed: BTreeMap<String, serde_json::Value>,
    recovered: bool,
    refresh: std::collections::BTreeSet<String>,
}
struct Pending {
    request: String,
    session: Option<u64>,
    stage: Stage,
    action: Action,
    journal: Journal,
    storage: Option<Snapshot>,
    loader: Option<Loader>,
    writer: Option<Writer>,
    storage_operation: Option<String>,
    storage_changes: Vec<(String, bool)>,
    intent: Option<Intent>,
    outcome: Option<Outcome>,
    pause: Option<production::Pause>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    RegistryCreate,
    Controls,
    Technology,
    TechnologyExecute,
    WorkControls,
    Journal,
    IntentWrite,
    Status,
    RegistryExecute,
    Station,
    Native,
    ReceiptRead,
    ReceiptWrite,
}
enum Action {
    Create {
        player: String,
        settlement: String,
        job: String,
        resident: String,
        building: String,
        recipe: String,
        count: u32,
        delay: u64,
    },
    Inspect {
        player: String,
        settlement: String,
        job: String,
    },
    Receipt {
        player: String,
        settlement: String,
        job: String,
        sequence: Option<u64>,
    },
    Wake,
}

pub(super) fn eligible_civilian(resident: &SettlementResident) -> bool {
    resident.role.is_none()
        && resident.hiring.is_none()
        && resident.dismissing.is_none()
        && resident.capturing.is_none()
        && resident.capture_recovery.is_none()
        && resident.captive_by.is_none()
}
fn status(request: &str, operation: &str) -> Command {
    Command::OperationStatus(OperationStatus {
        request: request.to_owned(),
        operation_id: operation.to_owned(),
    })
}
fn report(session: Option<u64>, text: impl Into<String>) -> Vec<Command> {
    let text = text.into();
    log(LogLevel::Info, &format!("{MARKER} production {text}"));
    session
        .map(|session| vec![message_session(session, text)])
        .unwrap_or_default()
}
fn operation_id(settlement: &str, job: &str) -> Option<String> {
    let id = format!("prod-{settlement}-{job}");
    (valid_identity(settlement) && valid_identity(job) && valid_identity(&id)).then_some(id)
}
fn native_command(request: &str, intent: &Intent) -> Command {
    let position = npcs::BlockPosition {
        x: intent.station.x,
        y: intent.station.y,
        z: intent.station.z,
    };
    assign_npc_work(
        request,
        &intent.operation_id,
        &intent.worker,
        WorkOrder::Craft(CraftWork {
            recipe: intent.recipe_id.clone(),
            count: intent.count,
            station: WorkArea {
                dimension: intent.station.dimension.clone(),
                min: position,
                max: position,
            },
        }),
        u64::from(intent.count),
        intent.expected_revision,
    )
}
fn job_text(job: &production::Job) -> String {
    if let Some(pause) = &job.pause {
        return format!(
            "{} paused retry={} reason={}",
            job.intent.operation_id, pause.retry_tick, pause.reason
        );
    }
    match &job.outcome {
        None => format!(
            "{} pending due={}",
            job.intent.operation_id, job.intent.due_tick
        ),
        Some(Outcome::Refused { reason }) => {
            format!("{} refused {reason}", job.intent.operation_id)
        }
        Some(Outcome::Native(receipt)) => format!(
            "{} {} done={}/{} revision={} reason={:?} changes={:?}",
            receipt.operation_id,
            receipt.state,
            receipt.work_units_done,
            receipt.work_units_planned,
            receipt.native_revision,
            receipt.reason,
            receipt.changes
        ),
    }
}

fn receipt_text(job: &production::Job, sequence: Option<u64>) -> String {
    let native = match &job.outcome {
        Some(Outcome::Native(receipt)) => Some(receipt),
        _ => None,
    };
    let portion = native.and_then(|receipt| match sequence {
        Some(sequence) => receipt.native_portions.iter().find(|portion| {
            portion.get("sequence").and_then(serde_json::Value::as_u64) == Some(sequence)
        }),
        None => receipt
            .native_portions
            .iter()
            .max_by_key(|portion| portion.get("sequence").and_then(serde_json::Value::as_u64)),
    });
    format!(
        "Production receipt {}",
        serde_json::json!({
            "intent": job.intent,
            "state": native.map(|receipt| receipt.state.as_str()),
            "reason": match &job.outcome {
                Some(Outcome::Refused { reason }) => Some(reason.as_str()),
                Some(Outcome::Native(receipt)) => receipt.reason.as_deref(),
                None => None,
            },
            "work_units_done": native.map(|receipt| receipt.work_units_done),
            "work_units_planned": native.map(|receipt| receipt.work_units_planned),
            "native_revision": native.map(|receipt| receipt.native_revision),
            "acknowledged_sequence": native.map(|receipt| receipt.acknowledged_sequence),
            "native_portion": portion,
        })
    )
}

fn receipt_report(session: Option<u64>, text: String) -> Vec<Command> {
    const CHUNK_BYTES: usize = 3_500;
    let Some(session) = session else {
        return Vec::new();
    };
    if text.len() <= CHUNK_BYTES {
        return vec![message_session(session, text)];
    }
    // A single immutable storage-read snapshot is emitted in ordered bounded
    // chat frames. Never truncate component bytes or re-read between pieces.
    let json = text
        .strip_prefix("Production receipt ")
        .expect("receipt prefix");
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < json.len() {
        let mut end = (start + CHUNK_BYTES).min(json.len());
        while !json.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&json[start..end]);
        start = end;
    }
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            message_session(
                session,
                format!(
                    "Production receipt part={}/{} {chunk}",
                    index + 1,
                    chunks.len()
                ),
            )
        })
        .collect()
}
impl Settlements {
    pub(super) fn production_snapshot(&self) -> Option<&Journal> {
        self.production_runtime.journal.as_ref()
    }
    pub(super) fn accepted_production_practice(
        &self,
        scope: &str,
        operation: &str,
    ) -> Option<&Receipt> {
        self.production_runtime
            .journal
            .as_ref()?
            .accepted_practice(scope, operation)
    }

    pub(super) fn handle_production_command(
        &mut self,
        invoked: &CommandInvoked,
        tick: u64,
    ) -> Option<Vec<Command>> {
        if let Some(commands) = stop_work::command(self, invoked) {
            return Some(commands);
        }
        if invoked.name != "settlement" {
            return None;
        }
        let args = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !matches!(
            args.first(),
            Some(&"production") | Some(&"production-status") | Some(&"production-receipt")
        ) {
            return None;
        }
        self.production_runtime.now = tick;
        if self.production_runtime.pending.is_some()
            || self.production_runtime.stop.pending.is_some()
        {
            return Some(report(
                Some(invoked.session),
                "Production request busy; no new work was issued.",
            ));
        }
        let action = match args.as_slice() {
            ["production", settlement, job, resident, building, recipe_name, count, delay] => {
                let parsed = count.parse::<u32>().ok().filter(|count| (1..=16).contains(count))
                    .zip(delay.parse::<u64>().ok().filter(|delay| *delay >= 1 && tick.checked_add(*delay).is_some()));
                let (Some(recipe), Some((count, delay)), Some(_)) = (production::recipe(recipe_name), parsed, operation_id(settlement, job)) else {
                    return Some(report(Some(invoked.session), "Production refused: recipe, count (1..16), delay (>=1), or stable job id invalid."));
                };
                if !production::selectable_recipes().any(|allowed| allowed == recipe) {
                    return Some(report(Some(invoked.session),"Production refused: tool quality is determined by saved admission, not selected by caller."));
                }
                Action::Create { player: invoked.player.clone(), settlement: (*settlement).to_owned(), job: (*job).to_owned(),
                    resident: (*resident).to_owned(), building: (*building).to_owned(), recipe: recipe.to_owned(), count, delay }
            }
            ["production-status", settlement, job] if operation_id(settlement, job).is_some() => {
                Action::Inspect { player: invoked.player.clone(), settlement: (*settlement).to_owned(), job: (*job).to_owned() }
            }
            ["production-receipt", settlement, job] if operation_id(settlement, job).is_some() => {
                Action::Receipt { player: invoked.player.clone(), settlement: (*settlement).to_owned(), job: (*job).to_owned(), sequence: None }
            }
            ["production-receipt", settlement, job, sequence] if operation_id(settlement, job).is_some() => {
                let Some(sequence) = sequence.parse::<u64>().ok().filter(|sequence| (1..=16).contains(sequence)) else {
                    return Some(report(Some(invoked.session), "Production receipt sequence must be 1..16."));
                };
                Action::Receipt { player: invoked.player.clone(), settlement: (*settlement).to_owned(), job: (*job).to_owned(), sequence: Some(sequence) }
            }
            _ => return Some(report(Some(invoked.session), "Usage: production <settlement> <stable-job> <resident-alias> <building> <recipe> <count> <simulation-delay>; production-status <settlement> <job>; production-receipt <settlement> <job> [portion-sequence]")),
        };
        Some(self.production_begin(
            action,
            Some(invoked.session),
            Stage::RegistryCreate,
            registry_storage::ROOT_KEY,
        ))
    }

    pub(super) fn production_init(&mut self) -> Vec<Command> {
        self.production_runtime = Runtime::default();
        vec![schedule_timer(production::TIMER, 1)]
    }
    pub(super) fn production_batch_rejected(&mut self) -> Vec<Command> {
        // A rejected/lost transport may hide a committed final manifest. Reload
        // the bounded index before selecting any next owner; never trust cache.
        self.production_runtime.pending = None;
        self.production_runtime.journal = None;
        self.production_runtime.recovered = false;
        self.production_runtime.stop.pending = None;
        self.production_runtime.stop.loaded = false;
        vec![schedule_timer(production::TIMER, 1)]
    }
    pub(super) fn production_timer(&mut self, tick: u64) -> Vec<Command> {
        self.production_runtime.now = tick;
        if let Some(commands) = stop_work::timer(self) {
            return commands;
        }
        if self.production_runtime.pending.is_some() {
            return vec![schedule_timer(production::TIMER, 1)];
        }
        self.production_begin(Action::Wake, None, Stage::Journal, disk::ROOT_KEY)
    }
    fn production_begin(
        &mut self,
        action: Action,
        session: Option<u64>,
        stage: Stage,
        key: &str,
    ) -> Vec<Command> {
        let pending = Pending {
            request: String::new(),
            session,
            stage,
            action,
            journal: Journal::default(),
            storage: None,
            loader: None,
            writer: None,
            storage_operation: None,
            storage_changes: Vec::new(),
            intent: None,
            outcome: None,
            pause: None,
        };
        self.production_read(pending, stage, key)
    }
    fn production_read(&mut self, mut pending: Pending, stage: Stage, key: &str) -> Vec<Command> {
        pending.stage = stage;
        if key == disk::ROOT_KEY {
            if stage == Stage::Journal
                && matches!(pending.action, Action::Wake)
                && self.production_runtime.journal.is_some()
            {
                return self.production_loaded(pending).unwrap_or_default();
            }
            let id = pending
                .intent
                .as_ref()
                .map(|intent| intent.operation_id.clone())
                .or_else(|| match &pending.action {
                    Action::Inspect {
                        settlement, job, ..
                    }
                    | Action::Receipt {
                        settlement, job, ..
                    }
                    | Action::Create {
                        settlement, job, ..
                    } => operation_id(settlement, job),
                    Action::Wake => None,
                });
            pending.loader = Some(if self.production_runtime.journal.is_none() {
                Loader::all()
            } else if let Some(id) = id {
                match Loader::job(&id) {
                    Ok(loader) => loader,
                    Err(reason) => return report(pending.session, reason),
                }
            } else {
                Loader::all()
            });
        }
        let key = pending
            .loader
            .as_ref()
            .and_then(Loader::next)
            .unwrap_or(key)
            .to_owned();
        let Some(request) = self.next_storage_request("production-read") else {
            return report(pending.session, "Production request counter exhausted.");
        };
        pending.request = request.clone();
        self.production_runtime.pending = Some(pending);
        if matches!(stage, Stage::RegistryCreate | Stage::RegistryExecute) {
            self.registry_read(&request)
        } else {
            vec![storage_get(&request, &key)]
        }
    }
    fn production_write(&mut self, mut pending: Pending, stage: Stage) -> Vec<Command> {
        let id = pending
            .intent
            .as_ref()
            .expect("durable intent")
            .operation_id
            .clone();
        let job = pending.journal.jobs.get(&id).expect("production job");
        let Ok(value) = serde_json::to_value(job) else {
            return report(pending.session, "production_serialization");
        };
        let Some(snapshot) = pending.storage.take() else {
            return report(pending.session, "production_storage_snapshot_missing");
        };
        let mut writer = match Writer::new(snapshot, &id, &value) {
            Ok(writer) => writer,
            Err(reason) => return report(pending.session, reason),
        };
        let step = match writer.next() {
            Ok(step) => step,
            Err(reason) => return report(pending.session, reason),
        };
        pending.stage = stage;
        pending.writer = Some(writer);
        self.production_storage_step(pending, step)
    }
    fn production_storage_step(&mut self, mut pending: Pending, step: Step) -> Vec<Command> {
        if matches!(step, Step::Complete) {
            return self.production_published(pending);
        }
        let Some(request) = self.next_storage_request("production-storage-v2") else {
            return report(pending.session, "Production request counter exhausted.");
        };
        let command = match step {
            Step::Read(key) => storage_get(&request, &key),
            Step::Batch {
                operation,
                mutations,
            } => {
                pending.storage_operation = Some(operation.clone());
                pending.storage_changes = mutations
                    .iter()
                    .map(|mutation| {
                        (
                            mutation.key().to_owned(),
                            matches!(mutation, disk::Mutation::Delete { .. }),
                        )
                    })
                    .collect();
                pending.storage_changes.sort_unstable();
                let mutations = mutations
                    .into_iter()
                    .map(|mutation| match mutation {
                        disk::Mutation::CompareAndSwap {
                            key,
                            expected_version,
                            value,
                        } => solaris_plugin_sdk::commands::StorageMutation::Cas(
                            solaris_plugin_sdk::storage::StorageCasMutation {
                                key,
                                expected_version,
                                value,
                            },
                        ),
                        disk::Mutation::Delete {
                            key,
                            expected_version,
                        } => solaris_plugin_sdk::commands::StorageMutation::Delete(
                            solaris_plugin_sdk::storage::StorageDeleteMutation {
                                key,
                                expected_version,
                            },
                        ),
                    })
                    .collect();
                Command::StorageBatchCas(solaris_plugin_sdk::commands::StorageBatchCas {
                    request: request.clone(),
                    operation_id: operation,
                    mutations,
                })
            }
            Step::Complete => unreachable!(),
        };
        pending.request = request;
        self.production_runtime.pending = Some(pending);
        vec![command]
    }
    fn production_arm(&self, journal: &Journal) -> Vec<Command> {
        journal
            .next_deadline()
            .map(|due| {
                vec![schedule_timer(
                    production::TIMER,
                    due.saturating_sub(self.production_runtime.now).max(1),
                )]
            })
            .unwrap_or_default()
    }

    pub(super) fn production_get_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        if let Some(commands) = stop_work::get(self, answer) {
            return Some(commands);
        }
        let mut pending = self.production_runtime.pending.take()?;
        if pending.request != answer.request()
            || (pending.writer.is_none()
                && !matches!(
                    pending.stage,
                    Stage::RegistryCreate
                        | Stage::RegistryExecute
                        | Stage::Journal
                        | Stage::ReceiptRead
                        | Stage::Technology
                        | Stage::TechnologyExecute
                ))
        {
            self.production_runtime.pending = Some(pending);
            return None;
        }
        let Some(record) = answer.record() else {
            self.production_runtime.journal = None;
            self.production_runtime.recovered = false;
            let mut commands = report(
                pending.session,
                "Production storage unavailable; intent not discarded.",
            );
            commands.push(schedule_timer(production::TIMER, 20));
            return Some(commands);
        };
        let storage_record = Record {
            value: record.value.map(str::to_owned),
            version: record.version,
        };
        if let Some(writer) = pending.writer.as_mut() {
            let step = match writer.answer_read(storage_record) {
                Ok(step) => step,
                Err(reason) => return Some(report(pending.session, reason)),
            };
            return Some(self.production_storage_step(pending, step));
        }
        if let Some(loader) = pending.loader.as_mut() {
            if let Err(reason) = loader.answer(storage_record) {
                return Some(report(pending.session, reason));
            }
            if let Some(key) = loader.next() {
                let key = key.to_owned();
                let Some(request) = self.next_storage_request("production-read-v2") else {
                    return Some(report(
                        pending.session,
                        "Production request counter exhausted.",
                    ));
                };
                pending.request = request.clone();
                self.production_runtime.pending = Some(pending);
                return Some(vec![storage_get(&request, &key)]);
            }
            let mut snapshot = match pending.loader.take().expect("production loader").finish() {
                Ok(snapshot) => snapshot,
                Err(reason) => return Some(report(pending.session, reason)),
            };
            let loaded: Journal =
                match serde_json::from_value(std::mem::take(&mut snapshot.journal)) {
                    Ok(journal) => journal,
                    Err(_) => return Some(report(pending.session, "production_store_unreadable")),
                };
            self.production_runtime
                .journal
                .get_or_insert_with(Journal::default)
                .jobs
                .extend(loaded.jobs);
            let id = pending
                .intent
                .as_ref()
                .map(|intent| intent.operation_id.clone())
                .or_else(|| match &pending.action {
                    Action::Create {
                        settlement, job, ..
                    }
                    | Action::Inspect {
                        settlement, job, ..
                    }
                    | Action::Receipt {
                        settlement, job, ..
                    } => operation_id(settlement, job),
                    Action::Wake => None,
                });
            pending.journal = Journal::default();
            if let Some(id) = id {
                if let Some(job) = self
                    .production_runtime
                    .journal
                    .as_ref()
                    .expect("complete cache")
                    .jobs
                    .get(&id)
                {
                    pending.journal.jobs.insert(id, job.clone());
                }
            }
            pending.storage = Some(snapshot);
            return self.production_loaded(pending);
        }
        if matches!(pending.stage, Stage::Technology | Stage::TechnologyExecute) {
            let intent = pending
                .intent
                .as_ref()
                .expect("authorized production intent");
            if let Err(reason) = technology_runtime::allows(
                record.value.as_deref(),
                &intent.settlement,
                &intent.recipe_id,
            ) {
                if pending.stage == Stage::TechnologyExecute {
                    return Some(self.production_pause(pending, reason));
                }
                return Some(report(pending.session, reason));
            }
            if pending.stage == Stage::TechnologyExecute {
                return Some(self.production_live_station(pending));
            }
            if let Err(reason) = self
                .production_runtime
                .journal
                .as_ref()
                .expect("complete cache")
                .can_insert(intent)
            {
                return Some(report(pending.session, reason));
            }
            if let Err(reason) = pending.journal.insert(intent.clone()) {
                return Some(report(pending.session, reason));
            }
            return Some(self.production_write(pending, Stage::IntentWrite));
        }
        if matches!(
            pending.stage,
            Stage::RegistryCreate | Stage::RegistryExecute
        ) {
            let registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => {
                    return Some(report(
                        pending.session,
                        "Production settlement store unreadable.",
                    ))
                }
            };
            if pending.stage == Stage::RegistryExecute {
                let intent = pending.intent.as_ref().expect("durable intent");
                let authorized =
                    registry
                        .settlements
                        .get(&intent.settlement)
                        .is_some_and(|settlement| {
                            can_build_settlement(settlement, &intent.authorized_by)
                                && settlement.condition.as_deref() != Some("ruined")
                                && settlement.residents.values().any(|resident| {
                                    resident.handle == intent.worker && eligible_civilian(resident)
                                })
                                && settlement
                                    .buildings
                                    .get(&intent.station.building)
                                    .is_some_and(|building| {
                                        building_origin_id(building) == intent.station.origin_id
                                            && building.dimension.as_deref()
                                                == Some(intent.station.dimension.as_str())
                                            && building.station_positions.iter().any(|at| {
                                                at.x == intent.station.x
                                                    && at.y == intent.station.y
                                                    && at.z == intent.station.z
                                            })
                                    })
                        });
                if !authorized {
                    pending.outcome = Some(Outcome::Refused {
                        reason: "mandate_or_workplace_revoked".to_owned(),
                    });
                    return Some(self.production_read(pending, Stage::ReceiptRead, disk::ROOT_KEY));
                }
                if requires_technology(&intent.recipe_id) {
                    return Some(self.production_read(
                        pending,
                        Stage::TechnologyExecute,
                        technology_runtime::STORE_KEY,
                    ));
                }
                return Some(self.production_live_station(pending));
            }
            let (player, settlement_name) = match &pending.action {
                Action::Create {
                    player, settlement, ..
                }
                | Action::Inspect {
                    player, settlement, ..
                }
                | Action::Receipt {
                    player, settlement, ..
                } => (player, settlement),
                Action::Wake => unreachable!(),
            };
            let Some(settlement) = registry.settlements.get(settlement_name) else {
                return Some(report(pending.session, "Production settlement missing."));
            };
            if !can_build_settlement(settlement, player)
                || settlement.condition.as_deref() == Some("ruined")
            {
                return Some(report(
                    pending.session,
                    "Production refused: current owner/steward and habitable settlement required.",
                ));
            }
            if let Action::Create {
                job,
                resident,
                building,
                recipe,
                count,
                delay,
                ..
            } = &pending.action
            {
                let Some(worker) = settlement
                    .residents
                    .get(resident)
                    .filter(|resident| eligible_civilian(resident))
                else {
                    return Some(report(
                        pending.session,
                        "Production refused: civilian worker missing or in service.",
                    ));
                };
                let Some(workplace) = settlement.buildings.get(building) else {
                    return Some(report(pending.session, "Production workplace missing."));
                };
                let (Some(at), Some(dimension)) = (
                    workplace.station_positions.first(),
                    workplace.dimension.as_ref(),
                ) else {
                    return Some(report(
                        pending.session,
                        "Production station capability missing.",
                    ));
                };
                let operation = operation_id(settlement_name, job).expect("validated job");
                let (recipe_id, quality_seed, quality) =
                    production::admitted_recipe(recipe, &operation);
                pending.intent = Some(Intent {
                    operation_id: operation,
                    settlement: settlement_name.clone(),
                    authorized_by: player.clone(),
                    worker: worker.handle.clone(),
                    recipe_id,
                    quality_seed,
                    quality,
                    count: *count,
                    expected_revision: worker.work_revision,
                    accepted_tick: self.production_runtime.now,
                    due_tick: self
                        .production_runtime
                        .now
                        .checked_add(*delay)
                        .expect("validated delay"),
                    station: Station {
                        dimension: dimension.clone(),
                        x: at.x,
                        y: at.y,
                        z: at.z,
                        building: building.clone(),
                        origin_id: building_origin_id(workplace).to_owned(),
                        worksite_id: workplace.worksite_id.clone(),
                        revision: workplace.snapshot_revision,
                    },
                });
            }
            return Some(self.production_read(pending, Stage::Journal, disk::ROOT_KEY));
        }
        Some(report(pending.session, "production_read_unexpected"))
    }
    fn production_loaded(&mut self, mut pending: Pending) -> Option<Vec<Command>> {
        if !self.production_runtime.recovered {
            self.production_runtime.refresh.extend(
                self.production_runtime
                    .journal
                    .as_ref()
                    .expect("complete cache")
                    .jobs
                    .iter()
                    .filter(|(_, job)| matches!(job.outcome, Some(Outcome::Native(_))))
                    .map(|(id, _)| id.clone()),
            );
            self.production_runtime.recovered = true;
        }
        if pending.stage == Stage::ReceiptRead {
            let intent = pending.intent.as_ref().expect("durable intent");
            if let Some(pause) = pending.pause.take() {
                if let Err(reason) =
                    pending
                        .journal
                        .pause(&intent.operation_id, pause.reason, pause.retry_tick)
                {
                    return Some(report(pending.session, reason));
                }
                return Some(self.production_write(pending, Stage::ReceiptWrite));
            }
            let mut outcome = pending.outcome.clone().expect("native outcome");
            if let (Some(Outcome::Native(saved)), Outcome::Native(next)) = (
                pending
                    .journal
                    .jobs
                    .get(&intent.operation_id)
                    .and_then(|job| job.outcome.as_ref()),
                &mut outcome,
            ) {
                if next.native_revision < saved.native_revision {
                    return Some(self.production_arm(&pending.journal));
                }
                if next.work_units_done == saved.work_units_done && next.changes.is_empty() {
                    next.changes = saved.changes.clone();
                }
                for portion in &saved.native_portions {
                    if let Some(existing) = next
                        .native_portions
                        .iter()
                        .find(|actual| actual.get("portion_id") == portion.get("portion_id"))
                    {
                        if existing != portion {
                            return Some(report(pending.session, "production_portion_conflict"));
                        }
                    } else {
                        next.native_portions.push(portion.clone());
                    }
                }
            }
            if let Outcome::Native(receipt) = &mut outcome {
                receipt.native_portions.sort_by_key(|portion| {
                    portion.get("sequence").and_then(serde_json::Value::as_u64)
                });
            }
            if let Err(reason) = pending.journal.finish(&intent.operation_id, outcome) {
                return Some(report(pending.session, reason));
            }
            return Some(self.production_write(pending, Stage::ReceiptWrite));
        }
        match &pending.action {
            Action::Inspect {
                settlement, job, ..
            } => {
                let id = operation_id(settlement, job).expect("validated job");
                return Some(report(
                    pending.session,
                    pending
                        .journal
                        .jobs
                        .get(&id)
                        .map(job_text)
                        .unwrap_or_else(|| "Production job not found.".to_owned()),
                ));
            }
            Action::Receipt {
                settlement,
                job,
                sequence,
                ..
            } => {
                let id = operation_id(settlement, job).expect("validated job");
                return Some(receipt_report(
                    pending.session,
                    pending
                        .journal
                        .jobs
                        .get(&id)
                        .map(|job| receipt_text(job, *sequence))
                        .unwrap_or_else(|| "Production job not found.".to_owned()),
                ));
            }
            Action::Create { .. } => {
                let intent = pending.intent.as_ref().expect("validated intent");
                if let Some(existing) = pending.journal.jobs.get(&intent.operation_id) {
                    // Runtime fences can advance since the original admission.
                    // Replay addresses its original immutable native request.
                    let original = &existing.intent;
                    if original.settlement != intent.settlement
                        || original.authorized_by != intent.authorized_by
                        || original.worker != intent.worker
                        || original.recipe_id != intent.recipe_id
                        || original.count != intent.count
                        || original.station.building != intent.station.building
                        || original.station.origin_id != intent.station.origin_id
                    {
                        return Some(report(pending.session, "operation_conflict"));
                    }
                    return Some(report(pending.session, job_text(existing)));
                }
                let Some(request) = self.next_storage_request("production-controls") else {
                    return Some(report(pending.session, "Production request exhausted."));
                };
                let command = query_npc_controls(&request, &[intent.worker.clone()]);
                pending.request = request;
                pending.stage = Stage::Controls;
                self.production_runtime.pending = Some(pending);
                return Some(vec![command]);
            }
            Action::Wake => {}
        }
        while let Some((id, snapshot)) = self.production_runtime.observed.pop_first() {
            let Some(job) = self
                .production_runtime
                .journal
                .as_ref()
                .expect("complete cache")
                .jobs
                .get(&id)
            else {
                continue;
            };
            let Ok(work) = serde_json::from_value::<npcs::WorkAssignment>(snapshot) else {
                continue;
            };
            let Some(receipt) = native_receipt(&job.intent, &work) else {
                continue;
            };
            pending.intent = Some(job.intent.clone());
            pending.outcome = Some(Outcome::Native(receipt));
            return Some(self.production_read(pending, Stage::ReceiptRead, disk::ROOT_KEY));
        }
        let refreshed = self.production_runtime.refresh.pop_first().and_then(|id| {
            self.production_runtime
                .journal
                .as_ref()
                .expect("complete cache")
                .jobs
                .get(&id)
        });
        let Some(job) = refreshed.or_else(|| {
            self.production_runtime
                .journal
                .as_ref()
                .expect("complete cache")
                .due(self.production_runtime.now)
        }) else {
            return Some(
                self.production_arm(
                    self.production_runtime
                        .journal
                        .as_ref()
                        .expect("complete cache"),
                ),
            );
        };
        let has_native = matches!(job.outcome, Some(Outcome::Native(_)));
        pending.intent = Some(job.intent.clone());
        let Some(request) = self.next_storage_request("production-status") else {
            return Some(report(pending.session, "Production request exhausted."));
        };
        let intent = pending.intent.as_ref().expect("selected job");
        let (command, stage) = if has_native {
            (
                query_npc_controls(&request, &[intent.worker.clone()]),
                Stage::WorkControls,
            )
        } else {
            (status(&request, &intent.operation_id), Stage::Status)
        };
        pending.request = request;
        pending.stage = stage;
        self.production_runtime.pending = Some(pending);
        Some(vec![command])
    }

    pub(super) fn production_cas_answer(
        &mut self,
        answer: &impl crate::registry_runtime::StoragePublish,
    ) -> Option<Vec<Command>> {
        stop_work::cas(self, answer)
    }
    fn production_published(&mut self, pending: Pending) -> Vec<Command> {
        let mut commands = Vec::new();
        let intent = pending.intent.as_ref().expect("intent");
        if let Some(job) = pending.journal.jobs.get(&intent.operation_id) {
            commands.extend(report(pending.session, job_text(job)));
        }
        if pending.stage == Stage::ReceiptWrite {
            if let Some(Outcome::Native(receipt)) = pending
                .journal
                .jobs
                .get(&intent.operation_id)
                .and_then(|job| job.outcome.as_ref())
            {
                if let Some(through) = receipt
                    .native_portions
                    .iter()
                    .filter_map(|portion| {
                        portion.get("sequence").and_then(serde_json::Value::as_u64)
                    })
                    .max()
                    .filter(|sequence| *sequence > receipt.acknowledged_sequence)
                {
                    if let Some(request) = self.next_storage_request("production-ack") {
                        let hash = intent
                            .operation_id
                            .bytes()
                            .fold(0xcbf29ce484222325u64, |hash, byte| {
                                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                            });
                        commands.push(solaris_plugin_sdk::ack_npc_work_receipts(
                            &request,
                            &format!("prod-ack-{hash:x}-{through}-{}", receipt.native_revision),
                            &intent.worker,
                            &intent.operation_id,
                            through,
                            receipt.native_revision,
                        ));
                    }
                }
            }
        }
        self.production_runtime
            .journal
            .get_or_insert_with(Journal::default)
            .jobs
            .extend(pending.journal.jobs);
        let mut armed = self.production_arm(
            self.production_runtime
                .journal
                .as_ref()
                .expect("complete cache"),
        );
        armed.extend(commands);
        armed
    }

    pub(super) fn production_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        if let Some(commands) = stop_work::operation(self, answer) {
            return Some(commands);
        }
        if self
            .production_runtime
            .pending
            .as_ref()
            .is_some_and(|p| p.request == answer.request() && p.writer.is_some())
        {
            let mut pending = self
                .production_runtime
                .pending
                .take()
                .expect("production storage writer");
            if let OperationOutcome::Committed(committed) = answer.outcome() {
                let valid = answer.operation_id == pending.storage_operation
                    && matches!(&committed.payload, OperationPayload::StorageBatch(changes)
                        if changes.len() == pending.storage_changes.len()
                        && changes.iter().zip(&pending.storage_changes).all(|(actual, wanted)| actual.key == wanted.0 && actual.deleted == wanted.1));
                if !valid {
                    self.production_runtime.journal = None;
                    self.production_runtime.recovered = false;
                    return Some(vec![schedule_timer(production::TIMER, 20)]);
                }
                let step = match pending
                    .writer
                    .as_mut()
                    .expect("writer")
                    .committed(committed.revision)
                {
                    Ok(step) => step,
                    Err(reason) => return Some(report(pending.session, reason)),
                };
                return Some(self.production_storage_step(pending, step));
            }
            if let OperationOutcome::Refused(refused) = answer.outcome() {
                if matches!(refused.reason, OperationFailure::StaleRevision) {
                    pending.writer = None;
                    let stage = if pending.stage == Stage::ReceiptWrite {
                        Stage::ReceiptRead
                    } else {
                        Stage::Journal
                    };
                    return Some(self.production_read(pending, stage, disk::ROOT_KEY));
                }
            }
            self.production_runtime.journal = None;
            self.production_runtime.recovered = false;
            let mut commands = report(
                pending.session,
                "Production storage unavailable; full native receipt retained, no ACK issued.",
            );
            commands.push(schedule_timer(production::TIMER, 20));
            return Some(commands);
        }
        let matched = self
            .production_runtime
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request == answer.request());
        if !matched {
            if let OperationOutcome::Committed(committed) = answer.outcome() {
                if let OperationPayload::NpcOrder(NpcOrderResult::Work(work)) = &committed.payload {
                    if let Some(id) = work
                        .operation_id
                        .as_ref()
                        .filter(|id| id.starts_with("prod-") && !id.starts_with("prod-ack-"))
                    {
                        if self.production_runtime.observed.len() < production::MAX_JOBS
                            || self.production_runtime.observed.contains_key(id)
                        {
                            if let Ok(snapshot) = serde_json::to_value(work) {
                                let newer = self
                                    .production_runtime
                                    .observed
                                    .get(id)
                                    .and_then(|saved| saved.get("revision"))
                                    .and_then(serde_json::Value::as_u64)
                                    .is_none_or(|revision| revision < work.revision);
                                if newer {
                                    self.production_runtime
                                        .observed
                                        .insert(id.clone(), snapshot);
                                }
                            }
                        }
                        return Some(vec![schedule_timer(production::TIMER, 1)]);
                    }
                }
            }
        }
        let mut pending = self.production_runtime.pending.take()?;
        if pending.request != answer.request()
            || !matches!(
                pending.stage,
                Stage::Controls
                    | Stage::WorkControls
                    | Stage::Status
                    | Stage::Station
                    | Stage::Native
            )
        {
            self.production_runtime.pending = Some(pending);
            return None;
        }
        if pending.stage == Stage::WorkControls {
            let OperationOutcome::Committed(committed) = answer.outcome() else {
                return Some(vec![schedule_timer(production::TIMER, 20)]);
            };
            let OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) = &committed.payload
            else {
                return Some(vec![schedule_timer(production::TIMER, 20)]);
            };
            let intent = pending.intent.as_ref().expect("durable admitted work");
            let Some(work) = page
                .npcs
                .iter()
                .find(|control| control.handle == intent.worker)
                .and_then(|control| control.work.as_ref())
            else {
                return Some(vec![schedule_timer(production::TIMER, 20)]);
            };
            let Some(receipt) = native_receipt(intent, work) else {
                return Some(vec![schedule_timer(production::TIMER, 20)]);
            };
            pending.outcome = Some(Outcome::Native(receipt));
            return Some(self.production_read(pending, Stage::ReceiptRead, disk::ROOT_KEY));
        }
        if pending.stage == Stage::Controls {
            let OperationOutcome::Committed(committed) = answer.outcome() else {
                return Some(report(pending.session, "Production controls refused."));
            };
            let OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) = &committed.payload
            else {
                return Some(report(
                    pending.session,
                    "Production control payload mismatch.",
                ));
            };
            let intent = pending.intent.as_mut().expect("intent");
            let Some(control) = page
                .npcs
                .iter()
                .find(|control| control.handle == intent.worker)
            else {
                return Some(report(
                    pending.session,
                    "Production controls missing worker.",
                ));
            };
            if control.pending_work.is_some() || control.order.is_some() {
                return Some(report(
                    pending.session,
                    "Production worker already has native work/order.",
                ));
            }
            intent.expected_revision = control.revision;
            if requires_technology(&intent.recipe_id) {
                return Some(self.production_read(
                    pending,
                    Stage::Technology,
                    technology_runtime::STORE_KEY,
                ));
            }
            if let Err(reason) = self
                .production_runtime
                .journal
                .as_ref()
                .expect("complete cache")
                .can_insert(intent)
            {
                return Some(report(pending.session, reason));
            }
            if let Err(reason) = pending.journal.insert(intent.clone()) {
                return Some(report(pending.session, reason));
            }
            return Some(self.production_write(pending, Stage::IntentWrite));
        }
        if pending.stage == Stage::Status {
            if let OperationOutcome::Refused(refused) = answer.outcome() {
                if matches!(refused.reason, OperationFailure::NotFound) {
                    return Some(self.production_read(
                        pending,
                        Stage::RegistryExecute,
                        registry_storage::ROOT_KEY,
                    ));
                }
                // Unknown/unavailable status is not proof of non-execution.
                return Some(vec![schedule_timer(production::TIMER, 20)]);
            }
        }
        if pending.stage == Stage::Station {
            let intent = pending.intent.as_ref().expect("intent");
            let usable = match answer.outcome() {
                OperationOutcome::Committed(committed) => match &committed.payload {
                    OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(
                        check,
                    )) => {
                        intent.station.worksite_id.as_deref() == Some(check.worksite_id.as_str())
                            && check.valid
                    }
                    OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(
                        check,
                    )) => check.structure_id == intent.station.origin_id && check.valid,
                    _ => false,
                },
                _ => false,
            };
            if !usable {
                let reason = match answer.outcome() {
                    OperationOutcome::Committed(committed) => match &committed.payload {
                        OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(
                            check,
                        )) => format!(
                            "live_station_or_access_unusable {}",
                            physical_points_explanation(&check.blocked_points)
                        ),
                        OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(
                            check,
                        )) => format!(
                            "live_station_or_access_unusable {}",
                            physical_points_explanation(&check.blocked_points)
                        ),
                        _ => "live_station_or_access_unusable payload_mismatch".to_owned(),
                    },
                    OperationOutcome::Refused(refused) => format!(
                        "live_station_or_access_unusable {}",
                        building_failure_explanation(refused.reason)
                    ),
                };
                return Some(self.production_pause(pending, reason));
            }
            let Some(request) = self.next_storage_request("production-native") else {
                return Some(report(pending.session, "Production request exhausted."));
            };
            let command = native_command(&request, intent);
            pending.request = request;
            pending.stage = Stage::Native;
            self.production_runtime.pending = Some(pending);
            return Some(vec![command]);
        }
        let intent = pending.intent.as_ref().expect("intent");
        pending.outcome = Some(match answer.outcome() {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::NpcOrder(NpcOrderResult::Work(work)) = &committed.payload
                else {
                    // A corrupt response cannot turn an uncertain native effect
                    // into a refusal. Recover by original operation status.
                    return Some(vec![schedule_timer(production::TIMER, 20)]);
                };
                if work.handle != intent.worker {
                    return Some(vec![schedule_timer(production::TIMER, 20)]);
                }
                let Some(receipt) = native_receipt(intent, work) else {
                    return Some(vec![schedule_timer(production::TIMER, 1)]);
                };
                Outcome::Native(receipt)
            }
            OperationOutcome::Refused(refused) => Outcome::Refused {
                reason: format!("{:?}", refused.reason),
            },
        });
        Some(self.production_read(pending, Stage::ReceiptRead, disk::ROOT_KEY))
    }
}

/// Explicit, authorized release of real native work. Cancellation has its own
/// durable intent: a craft job never impersonates a cancellation or overwrites
/// an occupied worker to make ingredient loading convenient.
mod stop_work {
    use super::*;
    const KEY: &str = "production-work-cancellations-v1";
    #[derive(Default)]
    pub(super) struct Runtime {
        pub(super) pending: Option<PendingStop>,
        pub(super) loaded: bool,
    }
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct Job {
        operation: String,
        settlement: String,
        authorized_by: String,
        worker: String,
        expected_revision: u64,
        result: Option<String>,
    }
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct Store {
        sequence: u64,
        jobs: BTreeMap<String, Job>,
    }
    impl Store {
        fn load(value: Option<&str>) -> Result<Self, &'static str> {
            let store: Self = match value {
                None => Self::default(),
                Some(value) => {
                    serde_json::from_str(value).map_err(|_| "stop_work_store_unreadable")?
                }
            };
            if store.jobs.len() > production::MAX_JOBS
                || store.jobs.iter().any(|(id, job)| {
                    id != &job.operation
                        || !valid_identity(id)
                        || job.worker.is_empty()
                        || job.settlement.is_empty()
                        || job.authorized_by.is_empty()
                })
            {
                return Err("stop_work_store_invalid");
            }
            Ok(store)
        }
        fn reserve(&mut self, mut job: Job) -> Result<Job, &'static str> {
            if self
                .jobs
                .values()
                .any(|existing| existing.worker == job.worker && existing.result.is_none())
            {
                return Err("stop_work_already_pending");
            }
            if self.jobs.len() >= production::MAX_JOBS {
                let completed = self
                    .jobs
                    .iter()
                    .find(|(_, job)| job.result.is_some())
                    .map(|(id, _)| id.clone());
                let Some(id) = completed else {
                    return Err("stop_work_capacity");
                };
                self.jobs.remove(&id);
            }
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or("stop_work_sequence_exhausted")?;
            job.operation = format!("stop-work-{}", self.sequence);
            self.jobs.insert(job.operation.clone(), job.clone());
            Ok(job)
        }
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Stage {
        Registry,
        Controls,
        Admission,
        IntentWrite,
        Status,
        Authorize,
        Native,
        ReceiptRead,
        ReceiptWrite,
    }
    pub(super) struct PendingStop {
        request: String,
        session: Option<u64>,
        stage: Stage,
        player: String,
        settlement: String,
        resident: String,
        job: Option<Job>,
        store: Store,
        version: Option<u64>,
    }
    fn read(
        owner: &mut Settlements,
        mut pending: PendingStop,
        stage: Stage,
        key: &str,
    ) -> Vec<Command> {
        let Some(request) = owner.next_storage_request("stop-work-read") else {
            return report(pending.session, "Stop-work request exhausted.");
        };
        pending.request = request.clone();
        pending.stage = stage;
        owner.production_runtime.stop.pending = Some(pending);
        if matches!(stage, Stage::Registry | Stage::Authorize) {
            owner.registry_read(&request)
        } else {
            vec![storage_get(&request, key)]
        }
    }
    fn write(owner: &mut Settlements, mut pending: PendingStop, stage: Stage) -> Vec<Command> {
        let Ok(value) = serde_json::to_string(&pending.store) else {
            return report(pending.session, "Stop-work serialization refused.");
        };
        let Some(request) = owner.next_storage_request("stop-work-write") else {
            return report(pending.session, "Stop-work request exhausted.");
        };
        pending.request = request.clone();
        pending.stage = stage;
        let command = storage_cas(&request, KEY, pending.version, value);
        owner.production_runtime.stop.pending = Some(pending);
        vec![command]
    }
    fn retry(owner: &mut Settlements, session: Option<u64>, reason: &str) -> Vec<Command> {
        owner.production_runtime.stop.loaded = false;
        let mut commands = report(session, reason);
        commands.push(schedule_timer(production::TIMER, 1));
        commands
    }
    pub(super) fn command(
        owner: &mut Settlements,
        invoked: &CommandInvoked,
    ) -> Option<Vec<Command>> {
        if invoked.name != "settlement"
            || invoked.arguments.first().map(String::as_str) != Some("stop-work")
        {
            return None;
        }
        if owner.production_runtime.pending.is_some()
            || owner.production_runtime.stop.pending.is_some()
        {
            return Some(report(
                Some(invoked.session),
                "Stop-work request busy; no native work overwritten.",
            ));
        }
        let args = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let ["stop-work", settlement, resident] = args.as_slice() else {
            return Some(report(
                Some(invoked.session),
                "Usage: stop-work <settlement> <resident-alias>",
            ));
        };
        let pending = PendingStop {
            request: String::new(),
            session: Some(invoked.session),
            stage: Stage::Registry,
            player: invoked.player.clone(),
            settlement: (*settlement).to_owned(),
            resident: (*resident).to_owned(),
            job: None,
            store: Store::default(),
            version: None,
        };
        Some(read(
            owner,
            pending,
            Stage::Registry,
            registry_storage::ROOT_KEY,
        ))
    }
    pub(super) fn timer(owner: &mut Settlements) -> Option<Vec<Command>> {
        if owner.production_runtime.stop.pending.is_some() {
            return Some(vec![schedule_timer(production::TIMER, 1)]);
        }
        if owner.production_runtime.pending.is_some() || owner.production_runtime.stop.loaded {
            return None;
        }
        let pending = PendingStop {
            request: String::new(),
            session: None,
            stage: Stage::Admission,
            player: String::new(),
            settlement: String::new(),
            resident: String::new(),
            job: None,
            store: Store::default(),
            version: None,
        };
        Some(read(owner, pending, Stage::Admission, KEY))
    }
    pub(super) fn get(owner: &mut Settlements, answer: &impl StorageRead) -> Option<Vec<Command>> {
        if owner.production_runtime.stop.pending.as_ref()?.request != answer.request() {
            return None;
        }
        let mut pending = owner.production_runtime.stop.pending.take()?;
        let Some(record) = answer.record() else {
            return Some(retry(
                owner,
                pending.session,
                "Stop-work storage unavailable; durable intent retained.",
            ));
        };
        if matches!(pending.stage, Stage::Registry | Stage::Authorize) {
            let registry = match record.registry.cloned() {
                Some(registry) => registry,
                None => {
                    return Some(retry(
                        owner,
                        pending.session,
                        "Stop-work registry unreadable.",
                    ))
                }
            };
            let (scope, player) = pending
                .job
                .as_ref()
                .map(|job| (job.settlement.as_str(), job.authorized_by.as_str()))
                .unwrap_or((&pending.settlement, &pending.player));
            let Some(settlement) = registry
                .settlements
                .get(scope)
                .filter(|scope| can_build_settlement(scope, player))
            else {
                if let Some(job) = pending.job.as_mut() {
                    job.result = Some("refused mandate_revoked".to_owned());
                    return Some(read(owner, pending, Stage::ReceiptRead, KEY));
                }
                return Some(report(
                    pending.session,
                    "Stop-work refused: owner or steward mandate required.",
                ));
            };
            if pending.stage == Stage::Authorize {
                let job = pending.job.as_mut().expect("durable cancellation");
                if !settlement
                    .residents
                    .values()
                    .any(|resident| resident.handle == job.worker)
                {
                    job.result = Some("refused resident_removed".to_owned());
                    return Some(read(owner, pending, Stage::ReceiptRead, KEY));
                }
                let Some(request) = owner.next_storage_request("stop-work-native") else {
                    return Some(retry(
                        owner,
                        pending.session,
                        "Stop-work request exhausted.",
                    ));
                };
                let command = solaris_plugin_sdk::cancel_npc_work(
                    &request,
                    &job.operation,
                    &job.worker,
                    job.expected_revision,
                );
                pending.request = request;
                pending.stage = Stage::Native;
                owner.production_runtime.stop.pending = Some(pending);
                return Some(vec![command]);
            }
            let Some(resident) = settlement.residents.get(&pending.resident) else {
                return Some(report(
                    pending.session,
                    "Stop-work refused: resident alias missing.",
                ));
            };
            pending.job = Some(Job {
                operation: String::new(),
                settlement: pending.settlement.clone(),
                authorized_by: pending.player.clone(),
                worker: resident.handle.clone(),
                expected_revision: 0,
                result: None,
            });
            let Some(request) = owner.next_storage_request("stop-work-controls") else {
                return Some(report(pending.session, "Stop-work request exhausted."));
            };
            let command = query_npc_controls(&request, &[resident.handle.clone()]);
            pending.request = request;
            pending.stage = Stage::Controls;
            owner.production_runtime.stop.pending = Some(pending);
            return Some(vec![command]);
        }
        pending.store = match Store::load(record.value.as_deref()) {
            Ok(store) => store,
            Err(reason) => return Some(report(pending.session, reason)),
        };
        pending.version = record.version;
        if pending.stage == Stage::ReceiptRead {
            let job = pending.job.as_ref().expect("cancellation result");
            let Some(saved) = pending.store.jobs.get_mut(&job.operation) else {
                return Some(retry(
                    owner,
                    pending.session,
                    "Stop-work durable intent missing.",
                ));
            };
            if saved.worker != job.worker || saved.expected_revision != job.expected_revision {
                return Some(report(
                    pending.session,
                    "Stop-work receipt identity conflict.",
                ));
            }
            if let Some(result) = &saved.result {
                if Some(result) != job.result.as_ref() {
                    return Some(report(pending.session, "Stop-work receipt conflict."));
                }
            }
            saved.result = job.result.clone();
            return Some(write(owner, pending, Stage::ReceiptWrite));
        }
        if pending.session.is_some() {
            let job = match pending
                .store
                .reserve(pending.job.take().expect("authorized cancellation"))
            {
                Ok(job) => job,
                Err(reason) => return Some(report(pending.session, reason)),
            };
            pending.job = Some(job);
            return Some(write(owner, pending, Stage::IntentWrite));
        }
        let Some(job) = pending
            .store
            .jobs
            .values()
            .find(|job| job.result.is_none())
            .cloned()
        else {
            owner.production_runtime.stop.loaded = true;
            return Some(vec![schedule_timer(production::TIMER, 1)]);
        };
        let Some(request) = owner.next_storage_request("stop-work-status") else {
            return Some(retry(owner, None, "Stop-work request exhausted."));
        };
        let command = status(&request, &job.operation);
        pending.job = Some(job);
        pending.request = request;
        pending.stage = Stage::Status;
        owner.production_runtime.stop.pending = Some(pending);
        Some(vec![command])
    }
    pub(super) fn cas(
        owner: &mut Settlements,
        answer: &impl StoragePublish,
    ) -> Option<Vec<Command>> {
        if owner.production_runtime.stop.pending.as_ref()?.request != answer.request() {
            return None;
        }
        let pending = owner.production_runtime.stop.pending.take()?;
        match answer.outcome() {
            StorageCasOutcome::Committed(_) => Some(retry(
                owner,
                pending.session,
                &format!(
                    "Stop-work {} {}",
                    pending.job.as_ref().expect("job").operation,
                    pending
                        .job
                        .as_ref()
                        .and_then(|job| job.result.as_deref())
                        .unwrap_or("pending")
                ),
            )),
            StorageCasOutcome::Refused => {
                let stage = if pending.stage == Stage::ReceiptWrite {
                    Stage::ReceiptRead
                } else {
                    Stage::Admission
                };
                Some(read(owner, pending, stage, KEY))
            }
            _ => Some(retry(
                owner,
                pending.session,
                "Stop-work CAS unavailable; original intent retained.",
            )),
        }
    }
    pub(super) fn operation(
        owner: &mut Settlements,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        if owner.production_runtime.stop.pending.as_ref()?.request != answer.request() {
            return None;
        }
        let mut pending = owner.production_runtime.stop.pending.take()?;
        if pending.stage == Stage::Controls {
            let OperationOutcome::Committed(committed) = answer.outcome() else {
                return Some(report(pending.session, "Stop-work controls refused."));
            };
            let OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) = &committed.payload
            else {
                return Some(report(
                    pending.session,
                    "Stop-work controls payload mismatch.",
                ));
            };
            let job = pending.job.as_mut().expect("resolved worker");
            let Some(control) = page
                .npcs
                .iter()
                .find(|control| control.handle == job.worker)
            else {
                return Some(report(pending.session, "Stop-work worker unavailable."));
            };
            if control.pending_work.is_none() {
                return Some(report(
                    pending.session,
                    "Stop-work: no pending native work.",
                ));
            }
            job.expected_revision = control.revision;
            return Some(read(owner, pending, Stage::Admission, KEY));
        }
        if pending.stage == Stage::Status {
            if let OperationOutcome::Refused(refused) = answer.outcome() {
                if matches!(
                    refused.reason,
                    solaris_plugin_sdk::operation_types::OperationFailure::NotFound
                ) {
                    return Some(read(
                        owner,
                        pending,
                        Stage::Authorize,
                        registry_storage::ROOT_KEY,
                    ));
                }
                return Some(retry(
                    owner,
                    pending.session,
                    "Stop-work original operation status unavailable.",
                ));
            }
        }
        let job = pending.job.as_mut().expect("durable cancellation");
        job.result = Some(match answer.outcome() {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::NpcOrder(NpcOrderResult::WorkCancelled(cancelled)) =
                    &committed.payload
                else {
                    return Some(retry(
                        owner,
                        pending.session,
                        "Stop-work receipt unavailable; recover original identity.",
                    ));
                };
                if cancelled.handle != job.worker {
                    return Some(retry(
                        owner,
                        pending.session,
                        "Stop-work receipt worker mismatch.",
                    ));
                }
                if let Some(journal) = &owner.production_runtime.journal {
                    owner.production_runtime.refresh.extend(
                        journal
                            .jobs
                            .iter()
                            .filter(|(_, production)| production.intent.worker == job.worker)
                            .map(|(id, _)| id.clone()),
                    );
                }
                format!("cancelled revision={}", cancelled.revision)
            }
            OperationOutcome::Refused(refused) => format!("refused {:?}", refused.reason),
        });
        Some(read(owner, pending, Stage::ReceiptRead, KEY))
    }
}

fn native_receipt(intent: &Intent, work: &npcs::WorkAssignment) -> Option<Receipt> {
    if work.operation_id.as_deref() != Some(intent.operation_id.as_str())
        || work.handle != intent.worker
    {
        return None;
    }
    let native_portions = work
        .receipts
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(Receipt {
        operation_id: intent.operation_id.clone(),
        settlement: intent.settlement.clone(),
        worker: intent.worker.clone(),
        recipe_id: intent.recipe_id.clone(),
        station: intent.station.clone(),
        native_revision: work.revision,
        work_units_done: work.work_units_done,
        work_units_planned: work.work_units_planned,
        state: format!("{:?}", work.state),
        reason: work.reason.map(|reason| format!("{reason:?}")),
        changes: work
            .changes
            .iter()
            .map(|change| ItemChange {
                item_id: change.item_id.clone(),
                delta: change.delta,
            })
            .collect(),
        native_portions,
        acknowledged_sequence: work.acknowledged_sequence,
        due_tick: work.due_tick,
    })
}

fn requires_technology(recipe: &str) -> bool {
    technology::initial_definitions()
        .values()
        .any(|definition| definition.unlocks(recipe))
}
impl Settlements {
    fn production_pause(&mut self, mut pending: Pending, reason: String) -> Vec<Command> {
        pending.pause = Some(production::Pause {
            reason,
            retry_tick: self.production_runtime.now.saturating_add(20),
        });
        self.production_read(pending, Stage::ReceiptRead, disk::ROOT_KEY)
    }
    fn production_live_station(&mut self, mut pending: Pending) -> Vec<Command> {
        let Some(request) = self.next_storage_request("production-station") else {
            return report(pending.session, "Production request exhausted.");
        };
        let intent = pending.intent.as_ref().expect("durable authorized intent");
        let command = if let Some(id) = &intent.station.worksite_id {
            check_worksite(&request, id)
        } else {
            solaris_plugin_sdk::check_structure(&request, &intent.station.origin_id)
        };
        pending.request = request;
        pending.stage = Stage::Station;
        self.production_runtime.pending = Some(pending);
        vec![command]
    }
}
