//! Physical capture is a fenced native decision; the single settlement
//! registry stores only the social custody claim and excludes the same handle
//! from both armies while that decision is in flight.

use super::*;
use solaris_plugin_sdk::capture_resident;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct CaptureIntent {
    operation_id: String,
    pub(super) captor_settlement: String,
    custodian: String,
    expected_revision: u64,
    prior_role: hire::Role,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Registry,
    Recover,
    IntentWrite,
    Native,
    FinalizeRead,
    FinalizeWrite,
    AbortRead,
    AbortWrite,
}

pub(super) struct PendingCapture {
    request: String,
    stage: Stage,
    session: u64,
    player: String,
    captor_settlement: String,
    guard_alias: String,
    victim_settlement: String,
    victim_alias: String,
    expected_revision: u64,
    victim_handle: String,
    intent: Option<CaptureIntent>,
    version: Option<u64>,
}

impl PendingCapture {
    fn new(request: String, stage: Stage) -> Self {
        Self {
            request,
            stage,
            session: 0,
            player: String::new(),
            captor_settlement: String::new(),
            guard_alias: String::new(),
            victim_settlement: String::new(),
            victim_alias: String::new(),
            expected_revision: 0,
            victim_handle: String::new(),
            intent: None,
            version: None,
        }
    }
}

impl Settlements {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn capture(
        &mut self,
        session: u64,
        player: &str,
        captor_settlement: &str,
        guard_alias: &str,
        victim_settlement: &str,
        victim_alias: &str,
        expected_revision: &str,
    ) -> Vec<Command> {
        if self.pending_capture.is_some()
            || self.pending_hire.is_some()
            || self.pending_dismiss.is_some()
            || self.pending_registry_write.is_some()
        {
            return vec![message_session(
                session,
                "Another settlement record update is pending.",
            )];
        }
        let Some(revision) = expected_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Pass the current resident order revision.",
            )];
        };
        if !valid_settlement_name(captor_settlement)
            || !valid_settlement_name(guard_alias)
            || !valid_settlement_name(victim_settlement)
            || !valid_settlement_name(victim_alias)
            || captor_settlement == victim_settlement
        {
            return vec![message_session(
                session,
                "Choose a serving guard and an opposing settlement resident.",
            )];
        }
        let Some(request) = self.next_storage_request("capture-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        let mut pending = PendingCapture::new(request.clone(), Stage::Registry);
        pending.session = session;
        pending.player = player.to_owned();
        pending.captor_settlement = captor_settlement.to_owned();
        pending.guard_alias = guard_alias.to_owned();
        pending.victim_settlement = victim_settlement.to_owned();
        pending.victim_alias = victim_alias.to_owned();
        pending.expected_revision = revision;
        self.pending_capture = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn recover_captures(&mut self) -> Vec<Command> {
        if self.pending_capture.is_some() {
            return Vec::new();
        }
        let Some(request) = self.next_storage_request("capture-recover") else {
            return Vec::new();
        };
        self.pending_capture = Some(PendingCapture::new(request.clone(), Stage::Recover));
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    /// Replay an interrupted capture from a registry snapshot an earlier
    /// recovery stage already read: startup recovery chains its stages off one
    /// read, and only a completed native capture reads the registry again.
    pub(super) fn recover_captures_from_read(
        &mut self,
        registry: &SettlementRegistry,
    ) -> Vec<Command> {
        if self.pending_capture.is_some() {
            return Vec::new();
        }
        self.resume_captures_from_registry(registry)
    }

    fn resume_captures_from_registry(&mut self, registry: &SettlementRegistry) -> Vec<Command> {
        let next = registry.settlements.iter().find_map(|(name, settlement)| {
            settlement.residents.iter().find_map(|(alias, resident)| {
                resident.capturing.as_ref().map(|intent| {
                    (
                        name.clone(),
                        alias.clone(),
                        resident.handle.clone(),
                        intent.clone(),
                    )
                })
            })
        });
        let Some((name, alias, handle, intent)) = next else {
            return Vec::new();
        };
        let mut pending = PendingCapture::new(String::new(), Stage::Recover);
        pending.victim_settlement = name;
        pending.victim_alias = alias;
        pending.victim_handle = handle;
        pending.intent = Some(intent);
        self.issue_capture(pending)
    }

    fn capture_request(
        &mut self,
        pending: &mut PendingCapture,
        stage: Stage,
        prefix: &str,
    ) -> String {
        let request = self
            .next_storage_request(prefix)
            .expect("bounded storage sequence");
        pending.request = request.clone();
        pending.stage = stage;
        request
    }

    fn capture_failure(&self, pending: &PendingCapture, reason: &str) -> Vec<Command> {
        log(
            LogLevel::Info,
            &format!(
                "{MARKER} capture-refused victim={} resident={} reason={reason}",
                pending.victim_settlement, pending.victim_alias
            ),
        );
        if pending.session == 0 {
            Vec::new()
        } else {
            vec![message_session(pending.session, reason)]
        }
    }

    fn issue_capture(&mut self, mut pending: PendingCapture) -> Vec<Command> {
        let request = self.capture_request(&mut pending, Stage::Native, "capture-native");
        let intent = pending.intent.as_ref().expect("durable capture intent");
        let command = capture_resident(
            &request,
            &intent.operation_id,
            &pending.victim_handle,
            &intent.custodian,
            intent.expected_revision,
        );
        self.pending_capture = Some(pending);
        vec![command]
    }

    fn read_capture_registry(&mut self, mut pending: PendingCapture, stage: Stage) -> Vec<Command> {
        let request = self.capture_request(&mut pending, stage, "capture-read");
        self.pending_capture = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn capture_get_answer(
        &mut self,
        answer: &StorageGetAnswered,
    ) -> Option<Vec<Command>> {
        let mut pending = self.pending_capture.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                Stage::Registry | Stage::Recover | Stage::FinalizeRead | Stage::AbortRead
            )
        {
            self.pending_capture = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answer.outcome else {
            return Some(self.capture_failure(&pending, "Settlement records are unavailable."));
        };
        let mut registry = match record.value.as_deref() {
            Some(value) => match serde_json::from_str::<SettlementRegistry>(value) {
                Ok(registry) => registry,
                Err(_) => {
                    return Some(
                        self.capture_failure(&pending, "Settlement records are unreadable."),
                    );
                }
            },
            None => SettlementRegistry::default(),
        };
        pending.version = record.version;
        Some(match pending.stage {
            Stage::Registry => self.start_capture(pending, registry),
            Stage::Recover => self.resume_captures_from_registry(&registry),
            Stage::FinalizeRead | Stage::AbortRead => {
                let Some(victim) = registry
                    .settlements
                    .get_mut(&pending.victim_settlement)
                    .and_then(|settlement| settlement.residents.get_mut(&pending.victim_alias))
                else {
                    return Some(
                        self.capture_failure(
                            &pending,
                            "Resident record disappeared during capture.",
                        ),
                    );
                };
                let intent = pending.intent.as_ref().expect("capture intent");
                if victim.handle != pending.victim_handle
                    || victim
                        .capturing
                        .as_ref()
                        .map(|stored| stored.operation_id.as_str())
                        != Some(intent.operation_id.as_str())
                {
                    return Some(self.capture_failure(
                        &pending,
                        "Capture intent changed; inspect resident custody.",
                    ));
                }
                if pending.stage == Stage::FinalizeRead {
                    victim.captive_by = Some(intent.captor_settlement.clone());
                    victim.role = None;
                } else {
                    victim.role = Some(intent.prior_role);
                }
                victim.capturing = None;
                let Ok(value) = serde_json::to_string(&registry) else {
                    return Some(
                        self.capture_failure(&pending, "Settlement records are unavailable."),
                    );
                };
                let stage = if pending.stage == Stage::FinalizeRead {
                    Stage::FinalizeWrite
                } else {
                    Stage::AbortWrite
                };
                let request = self.capture_request(&mut pending, stage, "capture-write");
                self.pending_capture = Some(pending);
                vec![storage_cas(
                    &request,
                    SETTLEMENT_REGISTRY_KEY,
                    record.version,
                    value,
                )]
            }
            _ => unreachable!(),
        })
    }

    fn start_capture(
        &mut self,
        mut pending: PendingCapture,
        mut registry: SettlementRegistry,
    ) -> Vec<Command> {
        let Some(captor) = registry.settlements.get(&pending.captor_settlement) else {
            return self.capture_failure(&pending, "Guard settlement not found.");
        };
        if !can_build_settlement(captor, &pending.player)
            || captor.condition.as_deref() == Some("ruined")
        {
            return self.capture_failure(
                &pending,
                "Only a serving guard's owner or steward can take custody.",
            );
        }
        let Some(guard) = captor.residents.get(&pending.guard_alias).filter(|guard| {
            guard.role.is_some_and(hire::Role::combat_capable)
                && guard.hiring.is_none()
                && guard.dismissing.is_none()
                && guard.capturing.is_none()
                && guard.captive_by.is_none()
        }) else {
            return self.capture_failure(&pending, "Choose a serving guard of this settlement.");
        };
        let guard_handle = guard.handle.clone();
        let captor_owner = captor.owner.clone();
        let Some(victim_settlement) = registry.settlements.get_mut(&pending.victim_settlement)
        else {
            return self.capture_failure(&pending, "Opposing settlement not found.");
        };
        if victim_settlement.owner == captor_owner {
            return self
                .capture_failure(&pending, "Cannot capture your own settlement's soldiers.");
        }
        let Some(victim) = victim_settlement.residents.get_mut(&pending.victim_alias) else {
            return self.capture_failure(&pending, "Opposing resident not found.");
        };
        let Some(role) = victim.role.filter(|role| role.combat_capable()) else {
            return self.capture_failure(&pending, "Resident is not serving as a soldier.");
        };
        if victim.hiring.is_some()
            || victim.dismissing.is_some()
            || victim.capturing.is_some()
            || victim.captive_by.is_some()
            || victim.handle == guard_handle
        {
            return self
                .capture_failure(&pending, "Resident is already transferring or in custody.");
        }
        let Some(next) = registry
            .capture_sequence
            .checked_add(1)
            .filter(|next| *next <= MAX_SAFE_REVISION)
        else {
            return self.capture_failure(&pending, "Capture sequence is exhausted.");
        };
        registry.capture_sequence = next;
        pending.victim_handle = victim.handle.clone();
        let intent = CaptureIntent {
            operation_id: format!("capture-{next}"),
            captor_settlement: pending.captor_settlement.clone(),
            custodian: guard_handle,
            expected_revision: pending.expected_revision,
            prior_role: role,
        };
        victim.role = None;
        victim.capturing = Some(intent.clone());
        pending.intent = Some(intent);
        let Ok(value) = serde_json::to_string(&registry) else {
            return self.capture_failure(&pending, "Settlement records are unavailable.");
        };
        let request = self.capture_request(&mut pending, Stage::IntentWrite, "capture-intent");
        let version = pending.version;
        self.pending_capture = Some(pending);
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            version,
            value,
        )]
    }

    pub(super) fn capture_cas_answer(
        &mut self,
        answer: &StorageCasAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_capture.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                Stage::IntentWrite | Stage::FinalizeWrite | Stage::AbortWrite
            )
        {
            self.pending_capture = Some(pending);
            return None;
        }
        Some(match answer.outcome {
            StorageCasOutcome::Committed(_) if pending.stage == Stage::IntentWrite => {
                self.issue_capture(pending)
            }
            StorageCasOutcome::Committed(_) => {
                let mut commands = if pending.stage == Stage::FinalizeWrite && pending.session != 0
                {
                    vec![message_session(pending.session, "Captured resident remains the same living person; the original army no longer commands them.")]
                } else {
                    self.capture_failure(
                        &pending,
                        "Native capture was refused; soldier remains on the original roster.",
                    )
                };
                commands.extend(self.recover_captures());
                commands
            }
            StorageCasOutcome::Refused | StorageCasOutcome::Failed(_) => self.capture_failure(
                &pending,
                "Settlement record changed; retry or restart to resume capture.",
            ),
        })
    }

    pub(super) fn capture_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_capture.take()?;
        if pending.request != answer.request || pending.stage != Stage::Native {
            self.pending_capture = Some(pending);
            return None;
        }
        let intent = pending.intent.as_ref().expect("durable capture intent");
        if answer.operation_id.as_deref() != Some(intent.operation_id.as_str()) {
            self.pending_capture = Some(pending);
            return Some(Vec::new());
        }
        Some(match &answer.outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::ResidentOrder(ResidentOrderResult::Captured(result)) =
                    &committed.payload
                else {
                    return Some(self.capture_failure(
                        &pending,
                        "Native capture result is unreadable; custody intent remains fenced.",
                    ));
                };
                if result.handle != pending.victim_handle || result.custodian != intent.custodian {
                    return Some(self.capture_failure(
                        &pending,
                        "Native capture identity mismatch; custody intent remains fenced.",
                    ));
                }
                self.read_capture_registry(pending, Stage::FinalizeRead)
            }
            OperationOutcome::Refused(_) => self.read_capture_registry(pending, Stage::AbortRead),
        })
    }
}
