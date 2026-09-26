//! An ambulatory wounded resident and a hired medic receive one native group
//! movement admission. No second patient entity, inventory or campaign timer is
//! manufactured by the guest.

use super::*;

pub(super) struct PendingEvacuation {
    request: String,
    session: u64,
    player: String,
    settlement: String,
    medic: String,
    patient: String,
    medic_revision: u64,
    patient_revision: u64,
    anchor: residents::BlockPosition,
    medic_handle: String,
    patient_handle: String,
}

impl Settlements {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn evacuate(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        medic: &str,
        medic_revision: &str,
        patient: &str,
        patient_revision: &str,
        coordinates: [&str; 3],
    ) -> Vec<Command> {
        if self.pending_evacuation.is_some() {
            return vec![message_session(session, "Another evacuation is pending.")];
        }
        let Some(medic_revision) = medic_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Pass the medic's current order revision.",
            )];
        };
        let Some(patient_revision) = patient_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Pass the patient's current order revision.",
            )];
        };
        let [Ok(x), Ok(y), Ok(z)] = coordinates.map(str::parse::<i32>) else {
            return vec![message_session(
                session,
                "Pass three integer destination coordinates.",
            )];
        };
        if !valid_settlement_name(settlement)
            || !valid_settlement_name(medic)
            || !valid_settlement_name(patient)
            || medic == patient
            || [x, y, z]
                .iter()
                .any(|axis| axis.unsigned_abs() > 30_000_000)
        {
            return vec![message_session(
                session,
                "Choose a distinct medic and wounded resident of this settlement.",
            )];
        }
        let Some(request) = self.next_storage_request("evacuate-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        self.pending_evacuation = Some(PendingEvacuation {
            request: request.clone(),
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            medic: medic.to_owned(),
            patient: patient.to_owned(),
            medic_revision,
            patient_revision,
            anchor: residents::BlockPosition { x, y, z },
            medic_handle: String::new(),
            patient_handle: String::new(),
        });
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn evacuation_get_answer(
        &mut self,
        answer: &StorageGetAnswered,
    ) -> Option<Vec<Command>> {
        let mut pending = self.pending_evacuation.take()?;
        if pending.request != answer.request || !pending.medic_handle.is_empty() {
            self.pending_evacuation = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answer.outcome else {
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
                "Only the owner or steward can evacuate this settlement's wounded.",
            )]);
        }
        let Some(medic) = settlement.residents.get(&pending.medic).filter(|resident| {
            resident.role == Some(hire::Role::Medic)
                && resident.hiring.is_none()
                && resident.capturing.is_none()
                && resident.dismissing.is_none()
        }) else {
            return Some(vec![message_session(
                pending.session,
                "Assign an active hired medic first.",
            )]);
        };
        let Some(patient) = settlement
            .residents
            .get(&pending.patient)
            .filter(|resident| {
                resident.role.is_some()
                    && resident.hiring.is_none()
                    && resident.capturing.is_none()
                    && resident.dismissing.is_none()
                    && resident.captive_by.is_none()
            })
        else {
            return Some(vec![message_session(
                pending.session,
                "Patient is not a serving resident of this settlement.",
            )]);
        };
        pending.medic_handle = medic.handle.clone();
        pending.patient_handle = patient.handle.clone();
        if pending.medic_handle == pending.patient_handle {
            return Some(vec![message_session(
                pending.session,
                "Medic and patient must be different residents.",
            )]);
        }
        let Some(request) = self.next_storage_request("evacuate-residents") else {
            return Some(vec![message_session(
                pending.session,
                "Resident records are unavailable.",
            )]);
        };
        pending.request = request.clone();
        let handles = vec![pending.medic_handle.clone(), pending.patient_handle.clone()];
        self.pending_evacuation = Some(pending);
        Some(vec![solaris_plugin_sdk::query_residents(
            &request, &handles, None,
        )])
    }

    pub(super) fn evacuation_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_evacuation.take()?;
        if pending.request != answer.request || pending.medic_handle.is_empty() {
            self.pending_evacuation = Some(pending);
            return None;
        }
        let OperationOutcome::Committed(committed) = &answer.outcome else {
            return Some(vec![message_session(
                pending.session,
                "Medic and patient must be alive and loaded.",
            )]);
        };
        let OperationPayload::Resident(ResidentResult::Page(page)) = &committed.payload else {
            return Some(vec![message_session(
                pending.session,
                "Resident query returned an unexpected result.",
            )]);
        };
        let medic = page
            .residents
            .iter()
            .find(|resident| resident.handle == pending.medic_handle);
        let patient = page
            .residents
            .iter()
            .find(|resident| resident.handle == pending.patient_handle);
        let Some((medic, patient)) = medic.zip(patient) else {
            return Some(vec![message_session(
                pending.session,
                "Medic or patient is unavailable.",
            )]);
        };
        let Some((medic, patient)) = medic.loaded.as_ref().zip(patient.loaded.as_ref()) else {
            return Some(vec![message_session(
                pending.session,
                "Medic and patient must be alive and loaded.",
            )]);
        };
        if !(patient.health > 0.0 && patient.health < 20.0) || medic.health <= 0.0 {
            return Some(vec![message_session(
                pending.session,
                "Evacuation needs a living, wounded patient and a living medic.",
            )]);
        }
        let dx = patient.x - medic.x;
        let dy = patient.y - medic.y;
        let dz = patient.z - medic.z;
        if dx * dx + dy * dy + dz * dz > 16.0 {
            return Some(vec![message_session(
                pending.session,
                "Medic must reach the wounded resident before evacuation.",
            )]);
        }
        let mut members = [
            (pending.medic_handle, pending.medic_revision),
            (pending.patient_handle, pending.patient_revision),
        ];
        members.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Some(self.enqueue_operation(
            OperationIntent::Squad {
                session: pending.session,
                handles: members.iter().map(|(handle, _)| handle.clone()).collect(),
                revisions: members.iter().map(|(_, revision)| *revision).collect(),
                action: combat::SquadAction::Evacuate(pending.anchor),
            },
            None,
            None,
        ))
    }
}
