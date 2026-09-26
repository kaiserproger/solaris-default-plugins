//! Settlement guard intent: authorize a serving resident, hold a player-chosen
//! post, then engage one server-perceived hostile. The native owner alone moves,
//! launches projectiles and decides whether an attack commits.

use super::*;
use solaris_plugin_sdk::issue_resident_order;

#[derive(Clone)]
pub(super) enum CombatPhase {
    Hold {
        anchor: residents::BlockPosition,
        rally: residents::BlockPosition,
        officer: Option<String>,
    },
    Attack {
        target: residents::OrderTargetRef,
        rally: residents::BlockPosition,
        officer: Option<String>,
    },
}

pub(super) struct PendingCombat {
    pub(super) request: String,
    session: u64,
    player: String,
    settlement: String,
    alias: String,
    expected_revision: u64,
    anchor: residents::BlockPosition,
    rally: residents::BlockPosition,
    officer_alias: Option<String>,
}

#[derive(Clone)]
pub(super) enum SquadAction {
    Move(residents::BlockPosition),
    Evacuate(residents::BlockPosition),
    Retreat(residents::BlockPosition),
    Cancel,
}

pub(super) struct PendingSquad {
    pub(super) request: String,
    session: u64,
    player: String,
    settlement: String,
    members: BTreeMap<String, u64>,
    action: SquadAction,
}

impl Settlements {
    pub(super) fn defend(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        alias: &str,
        expected_revision: &str,
        coordinates: [&str; 3],
        rally_coordinates: [&str; 3],
        officer_alias: Option<&str>,
    ) -> Vec<Command> {
        if self.pending_combat.is_some() {
            return vec![message_session(session, "Another guard order is pending.")];
        }
        let Some(revision) = expected_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(session, "Pass the current order revision.")];
        };
        let [Ok(x), Ok(y), Ok(z)] = coordinates.map(str::parse::<i32>) else {
            return vec![message_session(
                session,
                "Pass three integer post coordinates.",
            )];
        };
        let [Ok(rx), Ok(ry), Ok(rz)] = rally_coordinates.map(str::parse::<i32>) else {
            return vec![message_session(
                session,
                "Pass three integer rally coordinates.",
            )];
        };
        if officer_alias.is_some_and(|alias| !valid_settlement_name(alias))
            || !valid_settlement_name(settlement)
            || !valid_settlement_name(alias)
            || [x, y, z, rx, ry, rz]
                .iter()
                .any(|axis| axis.unsigned_abs() > 30_000_000)
            || [x, y, z] == [rx, ry, rz]
        {
            return vec![message_session(
                session,
                "Pass an existing settlement, resident and world post.",
            )];
        }
        let Some(request) = self.next_storage_request("combat-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        self.pending_combat = Some(PendingCombat {
            request: request.clone(),
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            alias: alias.to_owned(),
            officer_alias: officer_alias.map(str::to_owned),
            expected_revision: revision,
            anchor: residents::BlockPosition { x, y, z },
            rally: residents::BlockPosition {
                x: rx,
                y: ry,
                z: rz,
            },
        });
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn combat_get_answer(
        &mut self,
        answered: &StorageGetAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_combat.take()?;
        if pending.request != answered.request {
            self.pending_combat = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answered.outcome else {
            return Some(vec![message_session(
                pending.session,
                "Settlement records are unavailable.",
            )]);
        };
        let Some(registry) = record
            .value
            .as_deref()
            .and_then(|value| serde_json::from_str::<SettlementRegistry>(value).ok())
        else {
            return Some(vec![message_session(
                pending.session,
                "Settlement records are unreadable.",
            )]);
        };
        let Some(settlement) = registry.settlements.get(&pending.settlement) else {
            return Some(vec![message_session(
                pending.session,
                "Settlement not found.",
            )]);
        };
        if !can_build_settlement(settlement, &pending.player)
            || settlement.condition.as_deref() == Some("ruined")
        {
            return Some(vec![message_session(
                pending.session,
                "Only an owner or steward can command an active settlement.",
            )]);
        }
        let Some(resident) = settlement.residents.get(&pending.alias) else {
            return Some(vec![message_session(
                pending.session,
                "Resident not found in this settlement.",
            )]);
        };
        if !resident.role.is_some_and(hire::Role::combat_capable)
            || resident.hiring.is_some()
            || resident.capturing.is_some()
        {
            return Some(vec![message_session(
                pending.session,
                "Hire this resident before commanding it.",
            )]);
        }
        let handle = resident.handle.clone();
        let officer = match pending.officer_alias {
            Some(alias) if alias == pending.alias => {
                return Some(vec![message_session(
                    pending.session,
                    "Choose a different serving resident as officer.",
                )]);
            }
            Some(alias) => {
                let Some(officer) = settlement.residents.get(&alias).filter(|officer| {
                    officer.role.is_some_and(hire::Role::combat_capable)
                        && officer.hiring.is_none()
                        && officer.capturing.is_none()
                }) else {
                    return Some(vec![message_session(
                        pending.session,
                        "Officer must be a serving resident of this settlement.",
                    )]);
                };
                Some(officer.handle.clone())
            }
            None => None,
        };
        let allies = settlement
            .residents
            .values()
            .filter(|resident| {
                resident.role.is_some_and(hire::Role::combat_capable)
                    && resident.capturing.is_none()
            })
            .map(|resident| resident.handle.clone())
            .collect();
        Some(self.enqueue_operation(
            OperationIntent::Combat {
                session: pending.session,
                resident: handle,
                allies,
                expected_revision: pending.expected_revision,
                phase: CombatPhase::Hold {
                    anchor: pending.anchor,
                    rally: pending.rally,
                    officer,
                },
            },
            None,
            None,
        ))
    }

    pub(super) fn issue_combat(
        &mut self,
        request: &str,
        operation_id: &str,
        session: u64,
        resident: String,
        allies: Vec<String>,
        expected_revision: u64,
        phase: CombatPhase,
    ) -> Vec<Command> {
        let order = match &phase {
            CombatPhase::Hold { anchor, .. } => residents::Order::Hold(residents::HoldOrder {
                anchor: *anchor,
                heading_degrees: 0,
                formation: residents::Formation {
                    kind: residents::FormationKind::Line,
                    spacing: 4,
                },
                engagement_radius: 16,
            }),
            CombatPhase::Attack {
                target,
                rally,
                officer,
            } => residents::Order::Attack(residents::AttackOrder {
                targets: vec![target.clone()],
                policy: residents::EngagementPolicy {
                    revision: target.policy_revision,
                    allies: allies.clone(),
                    permitted: vec![residents::HostileCategory::Hostile],
                    officer: officer.clone(),
                    rally: *rally,
                },
            }),
        };
        self.pending.insert(
            request.to_owned(),
            PendingOperation {
                session,
                operation_id: operation_id.to_owned(),
                kind: PendingKind::Combat {
                    resident: resident.clone(),
                    allies,
                    phase,
                },
            },
        );
        vec![issue_resident_order(
            request,
            operation_id,
            vec![resident],
            vec![expected_revision],
            order,
        )]
    }

    pub(super) fn combat_answer(
        &mut self,
        session: u64,
        resident: String,
        allies: Vec<String>,
        phase: CombatPhase,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            let OperationOutcome::Refused(refused) = outcome else {
                unreachable!()
            };
            return vec![message_session(
                session,
                format!("Guard order refused: {:?}.", refused.reason),
            )];
        };
        let OperationPayload::ResidentOrder(ResidentOrderResult::Order(order)) = &committed.payload
        else {
            return vec![message_session(
                session,
                "Guard order returned an unexpected result.",
            )];
        };
        let Some(member) = order
            .members
            .iter()
            .find(|member| member.handle == resident)
        else {
            return vec![message_session(
                session,
                "Guard order returned a different resident.",
            )];
        };
        log(
            LogLevel::Info,
            &format!(
            "{MARKER} guard-order resident={} revision={} state={:?} targets={} damage-events={}",
            resident, order.order_revision, member.state, member.targets.len(), order.combat.len(),
        ),
        );
        if member.state != residents::OrderMemberState::Applied {
            return vec![message_session(
                session,
                format!(
                    "Guard cannot execute: {:?}. Order revision {}.",
                    member.state, order.order_revision
                ),
            )];
        }
        if let CombatPhase::Hold { rally, officer, .. } = phase {
            let Some(target) = member
                .targets
                .iter()
                .find(|target| target.category == residents::HostileCategory::Hostile)
            else {
                return vec![message_session(
                    session,
                    format!(
                        "Guard holding post; no hostile perceived. Order revision {}.",
                        order.order_revision
                    ),
                )];
            };
            return self.enqueue_operation(
                OperationIntent::Combat {
                    session,
                    resident,
                    allies,
                    expected_revision: order.order_revision,
                    phase: CombatPhase::Attack {
                        target: residents::OrderTargetRef {
                            target_ref: target.target_ref.clone(),
                            policy_revision: target.policy_revision,
                            expires_revision: target.expires_revision,
                        },
                        rally,
                        officer,
                    },
                },
                None,
                None,
            );
        }
        vec![message_session(
            session,
            format!(
                "Guard engaging the server-selected hostile. Order revision {}.",
                order.order_revision
            ),
        )]
    }
}

impl Settlements {
    pub(super) fn squad_command(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        roster: &str,
        coordinates: Option<[&str; 3]>,
        retreat: bool,
    ) -> Vec<Command> {
        if self.pending_squad.is_some() {
            return vec![message_session(session, "Another squad order is pending.")];
        }
        let mut members = BTreeMap::new();
        for member in roster.split(',') {
            let Some((alias, revision)) = member.split_once(':') else {
                return vec![message_session(
                    session,
                    "Pass alias:order-revision for each member.",
                )];
            };
            let Some(revision) = revision
                .parse::<u64>()
                .ok()
                .filter(|revision| *revision <= MAX_SAFE_REVISION)
            else {
                return vec![message_session(
                    session,
                    "Pass each member's current order revision.",
                )];
            };
            if !valid_settlement_name(alias) || members.insert(alias.to_owned(), revision).is_some()
            {
                return vec![message_session(session, "Pass distinct resident aliases.")];
            }
        }
        if !valid_settlement_name(settlement) || members.is_empty() || members.len() > 32 {
            return vec![message_session(
                session,
                "Choose one to 32 settlement residents.",
            )];
        }
        let action = if let Some(coordinates) = coordinates {
            let [Ok(x), Ok(y), Ok(z)] = coordinates.map(str::parse::<i32>) else {
                return vec![message_session(
                    session,
                    "Pass three integer destination coordinates.",
                )];
            };
            if [x, y, z]
                .iter()
                .any(|axis| axis.unsigned_abs() > 30_000_000)
            {
                return vec![message_session(
                    session,
                    "Destination is outside the world.",
                )];
            }
            let anchor = residents::BlockPosition { x, y, z };
            if retreat {
                SquadAction::Retreat(anchor)
            } else {
                SquadAction::Move(anchor)
            }
        } else {
            SquadAction::Cancel
        };
        let Some(request) = self.next_storage_request("squad-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        self.pending_squad = Some(PendingSquad {
            request: request.clone(),
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            members,
            action,
        });
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn squad_get_answer(
        &mut self,
        answered: &StorageGetAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_squad.take()?;
        if pending.request != answered.request {
            self.pending_squad = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answered.outcome else {
            return Some(vec![message_session(
                pending.session,
                "Settlement records are unavailable.",
            )]);
        };
        let Some(registry) = record
            .value
            .as_deref()
            .and_then(|value| serde_json::from_str::<SettlementRegistry>(value).ok())
        else {
            return Some(vec![message_session(
                pending.session,
                "Settlement records are unreadable.",
            )]);
        };
        let Some(settlement) = registry.settlements.get(&pending.settlement) else {
            return Some(vec![message_session(
                pending.session,
                "Settlement not found.",
            )]);
        };
        if !can_build_settlement(settlement, &pending.player)
            || settlement.condition.as_deref() == Some("ruined")
        {
            return Some(vec![message_session(
                pending.session,
                "Only an owner or steward can command an active settlement.",
            )]);
        }
        let mut handles = Vec::with_capacity(pending.members.len());
        let mut revisions = Vec::with_capacity(pending.members.len());
        for (alias, revision) in &pending.members {
            let Some(resident) = settlement.residents.get(alias) else {
                return Some(vec![message_session(
                    pending.session,
                    format!("Resident {alias} not found."),
                )]);
            };
            if !resident.role.is_some_and(hire::Role::combat_capable)
                || resident.hiring.is_some()
                || resident.capturing.is_some()
            {
                return Some(vec![message_session(
                    pending.session,
                    format!("Hire {alias} before ordering the squad."),
                )]);
            }
            handles.push(resident.handle.clone());
            revisions.push(*revision);
        }
        Some(self.enqueue_operation(
            OperationIntent::Squad {
                session: pending.session,
                handles,
                revisions,
                action: pending.action,
            },
            None,
            None,
        ))
    }

    pub(super) fn issue_squad(
        &mut self,
        request: &str,
        operation_id: &str,
        session: u64,
        handles: Vec<String>,
        revisions: Vec<u64>,
        action: SquadAction,
    ) -> Vec<Command> {
        self.pending.insert(
            request.to_owned(),
            PendingOperation {
                session,
                operation_id: operation_id.to_owned(),
                kind: PendingKind::Squad {
                    handles: handles.clone(),
                    action: action.clone(),
                },
            },
        );
        let command = match action {
            SquadAction::Move(anchor) | SquadAction::Evacuate(anchor) => issue_resident_order(
                request,
                operation_id,
                handles,
                revisions,
                residents::Order::Move(residents::MoveOrder {
                    dimension: STATION_DIMENSION.to_owned(),
                    anchor,
                    heading_degrees: 0,
                    formation: residents::Formation {
                        kind: residents::FormationKind::Column,
                        spacing: 4,
                    },
                }),
            ),
            SquadAction::Retreat(anchor) => issue_resident_order(
                request,
                operation_id,
                handles,
                revisions,
                residents::Order::Retreat(residents::RetreatOrder {
                    anchor,
                    formation: residents::Formation {
                        kind: residents::FormationKind::Column,
                        spacing: 4,
                    },
                }),
            ),
            SquadAction::Cancel => {
                solaris_plugin_sdk::cancel_resident_order(request, operation_id, handles, revisions)
            }
        };
        vec![command]
    }

    pub(super) fn squad_answer(
        &mut self,
        session: u64,
        handles: Vec<String>,
        action: SquadAction,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            let OperationOutcome::Refused(refused) = outcome else {
                unreachable!()
            };
            return vec![message_session(
                session,
                format!("Squad order refused: {:?}.", refused.reason),
            )];
        };
        let (revision, members) = match (&action, &committed.payload) {
            (
                SquadAction::Move(_) | SquadAction::Evacuate(_) | SquadAction::Retreat(_),
                OperationPayload::ResidentOrder(ResidentOrderResult::Order(result)),
            ) => (result.order_revision, &result.members),
            (
                SquadAction::Cancel,
                OperationPayload::ResidentOrder(ResidentOrderResult::OrderCancelled(result)),
            ) => (result.order_revision, &result.members),
            _ => {
                return vec![message_session(
                    session,
                    "Squad order returned an unexpected result.",
                )]
            }
        };
        if members.len() != handles.len()
            || !handles
                .iter()
                .all(|handle| members.iter().any(|member| &member.handle == handle))
        {
            return vec![message_session(
                session,
                "Squad order returned a different roster.",
            )];
        }
        let moving = members
            .iter()
            .filter(|member| member.state == residents::OrderMemberState::Applied)
            .count();
        let blocked = members
            .iter()
            .filter(|member| member.state != residents::OrderMemberState::Applied)
            .map(|member| format!("{}: {:?}", member.handle, member.state))
            .collect::<Vec<_>>();
        log(
            LogLevel::Info,
            &format!(
                "{MARKER} squad-order revision={revision} applied={moving}/{} blocked={blocked:?}",
                handles.len()
            ),
        );
        if matches!(action, SquadAction::Evacuate(_)) {
            return vec![message_session(
                session,
                if moving == 2 {
                    format!("Evacuation admitted: both medic and patient moving; order revision {revision}.")
                } else {
                    format!("Evacuation partial/blocked: {moving}/2 applied; order revision {revision}. {}",
                    blocked.join(", "))
                },
            )];
        }
        vec![message_session(
            session,
            format!(
                "Squad {}: {moving}/{} applied; order revision {revision}. {}",
                match action {
                    SquadAction::Cancel => "halted",
                    SquadAction::Move(_) | SquadAction::Evacuate(_) => "marching",
                    SquadAction::Retreat(_) => "retreating",
                },
                handles.len(),
                blocked.join(", ")
            ),
        )]
    }
}

impl Settlements {
    /// One bread heals three native health points. The core owns the actual
    /// warehouse debit and health commit; the guest only selects this cost.
    pub(super) fn treat(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        alias: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) || !valid_settlement_name(alias) {
            return vec![message_session(
                session,
                "Choose a settlement and resident alias.",
            )];
        }
        self.begin_registry_read(RegistryRead::Treat {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            alias: alias.to_owned(),
        })
    }

    pub(super) fn treatment_registry_answer(
        &mut self,
        session: u64,
        player: &str,
        settlement_name: &str,
        alias: &str,
        registry: &SettlementRegistry,
    ) -> Vec<Command> {
        let Some(settlement) = registry.settlements.get(settlement_name) else {
            return vec![message_session(session, "Settlement not found.")];
        };
        if !can_build_settlement(settlement, player)
            || settlement.condition.as_deref() == Some("ruined")
        {
            return vec![message_session(
                session,
                "Only an owner or steward can treat residents of an active settlement.",
            )];
        }
        let Some(resident) = settlement.residents.get(alias) else {
            return vec![message_session(session, "Resident not found.")];
        };
        let Some(binding) = self.warehouse.as_ref() else {
            return vec![message_session(
                session,
                "Bind a warehouse before treating a resident.",
            )];
        };
        let belongs = warehouse_belongs_to(settlement, binding);
        if !belongs {
            return vec![message_session(
                session,
                "The bound warehouse belongs to another settlement.",
            )];
        }
        let warehouse = binding.handle.clone();
        let Some(request) = self.next_storage_request("treatment-resident") else {
            return vec![message_session(
                session,
                "Resident records are unavailable.",
            )];
        };
        let handle = resident.handle.clone();
        self.pending.insert(
            request.clone(),
            PendingOperation {
                session,
                operation_id: String::new(),
                kind: PendingKind::TreatmentQuery {
                    handle: handle.clone(),
                    warehouse,
                },
            },
        );
        vec![solaris_plugin_sdk::query_residents(
            &request,
            &[handle],
            None,
        )]
    }

    pub(super) fn treatment_query_answer(
        &mut self,
        session: u64,
        handle: String,
        warehouse: String,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            return vec![message_session(
                session,
                "Resident is unavailable for treatment.",
            )];
        };
        let OperationPayload::Resident(ResidentResult::Page(page)) = &committed.payload else {
            return vec![message_session(
                session,
                "Resident query returned an unexpected result.",
            )];
        };
        let [resident] = page.residents.as_slice() else {
            return vec![message_session(
                session,
                "Resident is unavailable for treatment.",
            )];
        };
        if resident.handle != handle
            || resident.lifecycle != residents::ResidentLifecycle::AliveLoaded
        {
            return vec![message_session(
                session,
                "Resident is not alive and loaded.",
            )];
        }
        self.enqueue_operation(
            OperationIntent::Treatment {
                session,
                handle,
                warehouse,
                expected_revision: resident.revision,
            },
            None,
            None,
        )
    }

    pub(super) fn treatment_answer(
        &self,
        session: u64,
        handle: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed)
                if matches!(&committed.payload,
                    OperationPayload::Resident(ResidentResult::Treated(treated)) if treated == handle) =>
            {
                vec![message_session(
                    session,
                    "Resident treated: one bread spent, up to three health restored.",
                )]
            }
            OperationOutcome::Refused(refused) => vec![message_session(
                session,
                format!(
                    "Treatment refused: {:?}. No medicine spent.",
                    refused.reason
                ),
            )],
            _ => vec![message_session(
                session,
                "Treatment returned an unexpected result.",
            )],
        }
    }
}
