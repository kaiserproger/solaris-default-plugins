use solaris_plugin_sdk::events::{
    ClientSelectionOutcome, ClientSelectionStarted, ClientViewOpened, ClientViewOutcome,
    LoaderViewAction, LoaderViewRequest, OnlinePlayersAnswered, OperationAnswered,
    OperationOutcome, OperationPayload, ViewRequestKind,
};
use solaris_plugin_sdk::inventories::{InventoryEndpoint, InventoryResult};
use solaris_plugin_sdk::{
    close_client_view, list_online_players, message_session, open_client_view, present_client_view,
    query_owned_inventory, Command,
};

use solaris_plugin_sdk::client_presentation::{
    BeginClientSelection, CancelClientSelection, ClientCommand, ViewAction, ViewField,
    ViewFieldValue, ViewMarker, ViewModel, ViewResourceEntry, ViewRow,
};

use crate::{
    building_origin_id, can_build_settlement, is_settlement_member, warehouse_belongs_to,
    SettlementRecord, SettlementRegistry, Settlements,
};

pub(super) const VIEW_ID: &str = "solaris-settlements:overview";
pub(super) const REFRESH: &str = "solaris-settlements:refresh";
pub(super) const NEXT: &str = "solaris-settlements:page_next";
pub(super) const PREVIOUS: &str = "solaris-settlements:page_prev";
pub(super) const FUND: &str = "solaris-settlements:fund";
pub(super) const PREVIEW_GATE: &str = "solaris-settlements:preview_gate";
pub(super) const PROJECT_GATE: &str = "solaris-settlements:project_gate";
pub(super) const GATE_BLUEPRINT: &str = "solaris:palisade_gate";
pub(super) const GATE_BLUEPRINT_HASH: &str =
    "6a27ec4da2a39d40624c1676185ded1b4d0271435f2ae759e39b54fe0c6dcd3a";
const PLACEMENT_RANGE: f64 = 128.0;
const PAGE_ROWS: usize = 8;

#[derive(Clone)]
pub(super) struct StockRow {
    pub(super) slot: u8,
    pub(super) resource_id: String,
    pub(super) count: u32,
}

#[derive(Clone)]
pub(super) struct PreviewOrigin {
    pub(super) player: String,
    pub(super) settlement: String,
    pub(super) instance: String,
}

pub(super) struct PendingViewProject {
    session: u64,
    player: String,
    settlement: String,
    instance: String,
    anchor: solaris_plugin_sdk::settlements::BlockPosition,
    rotation: u16,
}

pub(super) struct ViewState {
    pub(super) player: String,
    pub(super) settlement: String,
    pub(super) record: SettlementRecord,
    pub(super) page: usize,
    pub(super) request: String,
    pub(super) instance: Option<(String, u64)>,
    pub(super) stock: Vec<StockRow>,
    pub(super) warehouse_status: String,
    pub(super) reading: bool,
    pub(super) dirty: bool,
    pub(super) anchor: [i32; 3],
    pub(super) rotation: u16,
    pub(super) pending_selection: Option<(String, u16)>,
    pub(super) selection: Option<(String, u16)>,
}

fn clipped(text: &str) -> String {
    let end = text
        .char_indices()
        .take_while(|(offset, ch)| offset + ch.len_utf8() <= 96)
        .last()
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    text[..end].to_owned()
}

fn row(label: &str, value: impl AsRef<str>, detail: impl AsRef<str>) -> ViewRow {
    ViewRow {
        cells: vec![
            clipped(label),
            clipped(value.as_ref()),
            clipped(detail.as_ref()),
        ],
    }
}

fn action(id: &str, label: &str, enabled: bool, denial: &str) -> ViewAction {
    ViewAction {
        action_id: id.to_owned(),
        enabled,
        label: Some(label.to_owned()),
        deny_reason: (!enabled).then(|| denial.to_owned()),
    }
}

impl ViewState {
    pub(super) fn model(&mut self) -> ViewModel {
        let roster_pages = self.record.residents.len().div_ceil(PAGE_ROWS).max(1);
        let building_pages = self.record.buildings.len().div_ceil(PAGE_ROWS);
        let stock_pages = self.stock.len().div_ceil(PAGE_ROWS).max(1);
        let page_count = 1 + roster_pages + building_pages + stock_pages;
        self.page = self.page.min(page_count - 1);
        let rows = if self.page == 0 {
            vec![
                row("Settlement", &self.settlement, "overview"),
                row(
                    "Site",
                    self.record.site_id.as_deref().unwrap_or("unassigned"),
                    "",
                ),
                row(
                    "Condition",
                    self.record.condition.as_deref().unwrap_or("inhabited"),
                    "",
                ),
                row("Size", &self.record.size_class, ""),
                row("Revision", self.record.revision.to_string(), ""),
                row("Residents", self.record.residents.len().to_string(), ""),
                row("Buildings", self.record.buildings.len().to_string(), ""),
                row("Warehouse", &self.warehouse_status, ""),
            ]
        } else if self.page <= roster_pages {
            let offset = (self.page - 1) * PAGE_ROWS;
            self.record
                .residents
                .iter()
                .skip(offset)
                .take(PAGE_ROWS)
                .map(|(alias, resident)| {
                    row(
                        alias,
                        &resident.handle,
                        format!(
                            "{} | work rev {}",
                            resident.roster_status(),
                            resident.work_revision,
                        ),
                    )
                })
                .collect()
        } else if self.page <= roster_pages + building_pages {
            let offset = (self.page - roster_pages - 1) * PAGE_ROWS;
            self.record
                .buildings
                .iter()
                .skip(offset)
                .take(PAGE_ROWS)
                .map(|(name, building)| row(name, building_origin_id(building), &building.purpose))
                .collect()
        } else {
            let offset = (self.page - roster_pages - building_pages - 1) * PAGE_ROWS;
            self.stock
                .iter()
                .skip(offset)
                .take(PAGE_ROWS)
                .map(|stock| {
                    row(
                        &stock.resource_id,
                        stock.count.to_string(),
                        format!("slot {}", stock.slot),
                    )
                })
                .collect()
        };
        let building = self
            .record
            .buildings
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
        let may_fund = can_build_settlement(&self.record, &self.player) && !building.is_empty();
        ViewModel {
            page: self.page as u32,
            page_count: page_count as u32,
            rows,
            fields: vec![
                ViewField {
                    id: "building".to_owned(),
                    value: ViewFieldValue::Text(building),
                },
                ViewField {
                    id: "rotation".to_owned(),
                    value: ViewFieldValue::Selected(self.rotation.to_string()),
                },
                ViewField {
                    id: "x".to_owned(),
                    value: ViewFieldValue::Number(self.anchor[0] as f64),
                },
                ViewField {
                    id: "y".to_owned(),
                    value: ViewFieldValue::Number(self.anchor[1] as f64),
                },
                ViewField {
                    id: "z".to_owned(),
                    value: ViewFieldValue::Number(self.anchor[2] as f64),
                },
            ],
            actions: vec![
                action(
                    REFRESH,
                    "Refresh",
                    !self.reading,
                    "Warehouse read in progress",
                ),
                action(NEXT, "Next page", self.page + 1 < page_count, "Last page"),
                action(PREVIOUS, "Previous page", self.page > 0, "First page"),
                action(
                    FUND,
                    "Fund building",
                    may_fund,
                    "Steward role or building required",
                ),
                action(
                    PREVIEW_GATE,
                    "Preview palisade gate",
                    can_build_settlement(&self.record, &self.player),
                    "Steward role required",
                ),
                action(
                    PROJECT_GATE,
                    "Project selected gate",
                    can_build_settlement(&self.record, &self.player),
                    "Steward role required",
                ),
            ],
            tabs: Vec::new(),
            resource_entries: vec![
                ViewResourceEntry {
                    id: "residents".to_owned(),
                    have: self.record.residents.len() as f64,
                    need: 0.0,
                },
                ViewResourceEntry {
                    id: "buildings".to_owned(),
                    have: self.record.buildings.len() as f64,
                    need: 0.0,
                },
            ],
            markers: self
                .selection
                .as_ref()
                .map_or_else(Vec::new, |(token, rotation)| {
                    vec![ViewMarker {
                        marker_id: format!("placement_{rotation}"),
                        selection_token: Some(token.clone()),
                        action_id: Some(PROJECT_GATE.to_owned()),
                        formation: None,
                        radius: None,
                    }]
                }),
            reason: Some(clipped(&self.warehouse_status)),
        }
    }
}

fn placement(fields: &[ViewField]) -> Option<([i32; 3], u16)> {
    let number = |id: &str, min: f64, max: f64| {
        fields.iter().find_map(|field| {
            if field.id != id {
                return None;
            }
            match &field.value {
                ViewFieldValue::Number(value)
                    if value.is_finite() && value.fract() == 0.0 && (min..=max).contains(value) =>
                {
                    Some(*value as i32)
                }
                _ => None,
            }
        })
    };
    let rotation = fields.iter().find_map(|field| {
        (field.id == "rotation")
            .then_some(&field.value)
            .and_then(|value| {
                if let ViewFieldValue::Selected(value) = value {
                    match value.as_str() {
                        "0" => Some(0),
                        "90" => Some(90),
                        "180" => Some(180),
                        "270" => Some(270),
                        _ => None,
                    }
                } else {
                    None
                }
            })
    })?;
    Some((
        [
            number("x", -29_999_984.0, 29_999_984.0)?,
            number("y", -64.0, 319.0)?,
            number("z", -29_999_984.0, 29_999_984.0)?,
        ],
        rotation,
    ))
}

impl Settlements {
    pub(super) fn view_registry_answer(
        &mut self,
        session: u64,
        player: String,
        requested: Option<&str>,
        page: usize,
        registry: &SettlementRegistry,
    ) -> Vec<Command> {
        let selected = match requested {
            Some(name) => registry.settlements.get(name).map(|record| (name, record)),
            None => registry
                .settlements
                .iter()
                .find(|(_, record)| is_settlement_member(record, &player))
                .map(|(name, record)| (name.as_str(), record)),
        };
        let Some((name, record)) = selected else {
            let mut commands = Vec::new();
            if let Some((instance, _)) =
                self.views.remove(&session).and_then(|state| state.instance)
            {
                commands.push(close_client_view(session, &instance));
            }
            commands.push(message_session(
                session,
                "Settlement not found or membership revoked.",
            ));
            return commands;
        };
        if !is_settlement_member(record, &player) {
            if let Some(previous) = self.views.remove(&session) {
                if let Some((instance, _)) = previous.instance {
                    return vec![
                        close_client_view(session, &instance),
                        message_session(session, "Settlement membership was revoked."),
                    ];
                }
            }
            return vec![message_session(
                session,
                "You are not a member of that settlement.",
            )];
        }
        if self.views.len() >= 16 && !self.views.contains_key(&session) {
            return vec![message_session(
                session,
                "Too many settlement views are open.",
            )];
        }
        let mut commands = Vec::new();
        let previous = self.views.remove(&session);
        let same_view = previous
            .as_ref()
            .is_some_and(|state| state.settlement == name);
        let (request, instance, stock, anchor, rotation) = if same_view {
            let state = previous.expect("same view implies prior state");
            (
                state.request,
                state.instance,
                state.stock,
                state.anchor,
                state.rotation,
            )
        } else {
            if let Some((instance, _)) = previous.and_then(|state| state.instance) {
                commands.push(close_client_view(session, &instance));
            }
            let Some(request) = self.next_storage_request("view-open") else {
                return vec![message_session(session, "Settlement view is unavailable.")];
            };
            (request, None, Vec::new(), [0, 64, 0], 0)
        };
        self.pending_view_stock
            .retain(|_, (owner, _)| *owner != session);
        self.views.insert(
            session,
            ViewState {
                player,
                settlement: name.to_owned(),
                record: record.clone(),
                page,
                request: request.clone(),
                instance,
                stock,
                warehouse_status: "Reading the bound warehouse".to_owned(),
                reading: false,
                dirty: false,
                anchor,
                rotation,
                pending_selection: None,
                selection: None,
            },
        );
        commands.extend(self.view_query_stock(session));
        if same_view {
            commands.extend(self.view_present(session));
        } else if let Some(state) = self.views.get_mut(&session) {
            commands.push(open_client_view(&request, session, VIEW_ID, state.model()));
        }
        commands
    }

    fn view_query_stock(&mut self, session: u64) -> Vec<Command> {
        let Some(state) = self.views.get(&session) else {
            return Vec::new();
        };
        let handle: Option<String> = self.warehouse.as_ref().and_then(|binding| {
            warehouse_belongs_to(&state.record, binding).then(|| binding.handle.clone())
        });
        let Some(handle) = handle else {
            if let Some(state) = self.views.get_mut(&session) {
                state.stock.clear();
                state.warehouse_status = "No bound warehouse for this settlement".to_owned();
            }
            return Vec::new();
        };
        let Some(request) = self.next_storage_request("view-stock") else {
            if let Some(state) = self.views.get_mut(&session) {
                state.warehouse_status = "Warehouse query budget exhausted".to_owned();
            }
            return Vec::new();
        };
        if let Some(state) = self.views.get_mut(&session) {
            state.reading = true;
            state.warehouse_status = "Reading warehouse contents".to_owned();
        }
        self.pending_view_stock
            .insert(request.clone(), (session, handle.clone()));
        vec![query_owned_inventory(
            &request,
            InventoryEndpoint::Warehouse(handle),
            None,
        )]
    }

    fn view_present(&mut self, session: u64) -> Vec<Command> {
        let Some(state) = self.views.get_mut(&session) else {
            return Vec::new();
        };
        let Some((instance, revision)) = state.instance.clone() else {
            state.dirty = true;
            return Vec::new();
        };
        let model = state.model();
        state.instance = Some((instance.clone(), revision + 1));
        state.dirty = false;
        vec![present_client_view(session, &instance, revision, model)]
    }

    pub(super) fn view_request(&mut self, request: &LoaderViewRequest) -> Vec<Command> {
        if request.request_kind != ViewRequestKind::Settlement {
            return Vec::new();
        }
        if self.pending_view_players.len() >= 16 {
            return vec![message_session(request.session, "Settlement view is busy.")];
        }
        let Some(id) = self.next_storage_request("view-player") else {
            return vec![message_session(
                request.session,
                "Settlement view is unavailable.",
            )];
        };
        self.pending_view_players
            .insert(id.clone(), request.session);
        vec![list_online_players(&id, 256)]
    }

    pub(super) fn view_players_answered(&mut self, answer: &OnlinePlayersAnswered) -> Vec<Command> {
        if let Some(project) = self.pending_view_project_players.remove(&answer.request) {
            if !self.views.get(&project.session).is_some_and(|state| {
                state.player == project.player
                    && state.settlement == project.settlement
                    && state
                        .instance
                        .as_ref()
                        .is_some_and(|(id, _)| id == &project.instance)
            }) {
                return Vec::new();
            }
            let Some(player) = answer.players.iter().find(|player| {
                player.session == project.session && player.player == project.player
            }) else {
                return vec![message_session(
                    project.session,
                    "Placement session is no longer online.",
                )];
            };
            if player.dimension != "minecraft:overworld" {
                return vec![message_session(
                    project.session,
                    "Place gates in the surveyed overworld.",
                )];
            }
            let dx = player.position.x - f64::from(project.anchor.x);
            let dy = player.position.y - f64::from(project.anchor.y);
            let dz = player.position.z - f64::from(project.anchor.z);
            if dx * dx + dy * dy + dz * dz > PLACEMENT_RANGE * PLACEMENT_RANGE {
                return vec![message_session(
                    project.session,
                    "Gate origin is beyond selection range.",
                )];
            }
            return self.legacy_project(
                project.session,
                &project.player,
                &project.settlement,
                GATE_BLUEPRINT,
                project.anchor,
                project.rotation,
                Some(PreviewOrigin {
                    player: project.player.clone(),
                    settlement: project.settlement.clone(),
                    instance: project.instance,
                }),
            );
        }
        let Some(session) = self.pending_view_players.remove(&answer.request) else {
            return Vec::new();
        };
        let Some(player) = answer
            .players
            .iter()
            .find(|player| player.session == session)
        else {
            return vec![message_session(
                session,
                "Your settlement identity is unavailable.",
            )];
        };
        self.overview(session, &player.player, None, 0, true)
    }

    pub(super) fn view_opened(&mut self, answer: &ClientViewOpened) -> Vec<Command> {
        let Some(state) = self.views.get_mut(&answer.session) else {
            return match &answer.outcome {
                ClientViewOutcome::Opened(opened) => {
                    vec![close_client_view(answer.session, &opened.view_instance_id)]
                }
                ClientViewOutcome::Refused(_) => Vec::new(),
            };
        };
        if state.request != answer.request || state.instance.is_some() {
            return match &answer.outcome {
                ClientViewOutcome::Opened(opened) => {
                    vec![close_client_view(answer.session, &opened.view_instance_id)]
                }
                ClientViewOutcome::Refused(_) => Vec::new(),
            };
        }
        match &answer.outcome {
            ClientViewOutcome::Opened(opened) => {
                state.instance = Some((opened.view_instance_id.clone(), opened.revision));
                if state.dirty {
                    self.view_present(answer.session)
                } else {
                    Vec::new()
                }
            }
            ClientViewOutcome::Refused(reason) => {
                self.views.remove(&answer.session);
                vec![message_session(
                    answer.session,
                    format!("Settlement view refused: {reason:?}."),
                )]
            }
        }
    }

    pub(super) fn view_selection_started(
        &mut self,
        answer: &ClientSelectionStarted,
    ) -> Vec<Command> {
        let pending = self.views.get_mut(&answer.session).and_then(|state| {
            state
                .pending_selection
                .as_ref()
                .is_some_and(|(request, _)| request == &answer.request)
                .then(|| state.pending_selection.take())
                .flatten()
        });
        let Some((_, rotation)) = pending else {
            return match &answer.outcome {
                ClientSelectionOutcome::Started(context) => vec![Command::ClientPresentation(
                    ClientCommand::CancelClientSelection(CancelClientSelection {
                        session: answer.session,
                        selection_context_id: context.id.clone(),
                    }),
                )],
                ClientSelectionOutcome::Refused(_) => Vec::new(),
            };
        };
        match &answer.outcome {
            ClientSelectionOutcome::Started(context) => {
                let state = self
                    .views
                    .get_mut(&answer.session)
                    .expect("matched live view");
                state.rotation = rotation;
                state.selection = Some((context.id.clone(), rotation));
                self.view_present(answer.session)
            }
            ClientSelectionOutcome::Refused(reason) => vec![message_session(
                answer.session,
                format!("Gate selection refused: {reason:?}."),
            )],
        }
    }

    pub(super) fn view_action(&mut self, action: &LoaderViewAction) -> Vec<Command> {
        let Some(state) = self.views.get_mut(&action.session) else {
            return Vec::new();
        };
        if !state
            .instance
            .as_ref()
            .is_some_and(|(id, _)| id == &action.view_instance_id)
        {
            return Vec::new();
        }
        state.instance = Some((action.view_instance_id.clone(), action.view_revision));
        let session = action.session;
        let player = state.player.clone();
        let settlement = state.settlement.clone();
        let page = state.page;
        match action.action_id.as_str() {
            REFRESH => self.overview(session, &player, Some(&settlement), page, false),
            NEXT => self.overview(
                session,
                &player,
                Some(&settlement),
                page.saturating_add(1),
                false,
            ),
            PREVIOUS => self.overview(
                session,
                &player,
                Some(&settlement),
                page.saturating_sub(1),
                false,
            ),
            FUND => {
                let building = action.fields.iter().find_map(|field| {
                    (field.id == "building")
                        .then_some(&field.value)
                        .and_then(|value| {
                            if let ViewFieldValue::Text(text) = value {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                });
                let Some(building) = building else {
                    return vec![message_session(session, "Choose a building to fund.")];
                };
                self.legacy_fund(session, &player, &settlement, building)
            }
            PREVIEW_GATE => self.view_preview_gate(action),
            PROJECT_GATE => self.view_project_gate(action),
            _ => Vec::new(),
        }
    }

    fn view_preview_gate(&mut self, action: &LoaderViewAction) -> Vec<Command> {
        let Some((anchor, rotation)) = placement(&action.fields) else {
            return vec![message_session(
                action.session,
                "Choose integral gate origin and rotation.",
            )];
        };
        let state = self
            .views
            .get(&action.session)
            .expect("action matched a live view");
        if !can_build_settlement(&state.record, &state.player) {
            return vec![message_session(
                action.session,
                "Only a steward may preview this site.",
            )];
        }
        if state.pending_selection.is_some() {
            return vec![message_session(
                action.session,
                "Gate preview is already opening.",
            )];
        }
        let Some(request) = self.next_storage_request("gate-selection") else {
            return vec![message_session(
                action.session,
                "Gate preview is unavailable.",
            )];
        };
        let state = self
            .views
            .get_mut(&action.session)
            .expect("matched live view");
        state.anchor = anchor;
        state.rotation = rotation;
        state.selection = None;
        state.pending_selection = Some((request.clone(), rotation));
        vec![Command::ClientPresentation(
            ClientCommand::BeginClientSelection(BeginClientSelection {
                request,
                session: action.session,
                view_instance_id: action.view_instance_id.clone(),
                view_revision: action.view_revision,
                action_id: PROJECT_GATE.to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                range_limit: PLACEMENT_RANGE as u32,
                ttl_ticks: 1200,
                formation: None,
                radius: None,
            }),
        )]
    }

    fn view_project_gate(&mut self, action: &LoaderViewAction) -> Vec<Command> {
        let Some((anchor, rotation)) = placement(&action.fields) else {
            return vec![message_session(
                action.session,
                "Choose integral gate origin and rotation.",
            )];
        };
        let state = self
            .views
            .get(&action.session)
            .expect("action matched a live view");
        if !can_build_settlement(&state.record, &state.player)
            || anchor != state.anchor
            || !state
                .selection
                .as_ref()
                .is_some_and(|(token, preview_rotation)| {
                    action.selection_token.as_deref() == Some(token.as_str())
                        && rotation == *preview_rotation
                })
        {
            return vec![message_session(
                action.session,
                "Preview this gate before placement.",
            )];
        }
        if self.pending_view_project_players.len() >= 16 {
            return vec![message_session(
                action.session,
                "Too many gate placements are pending.",
            )];
        }
        let project = PendingViewProject {
            session: action.session,
            player: state.player.clone(),
            settlement: state.settlement.clone(),
            instance: action.view_instance_id.clone(),
            anchor: solaris_plugin_sdk::settlements::BlockPosition {
                x: anchor[0],
                y: anchor[1],
                z: anchor[2],
            },
            rotation,
        };
        let Some(request) = self.next_storage_request("gate-player") else {
            return vec![message_session(
                action.session,
                "Gate placement is unavailable.",
            )];
        };
        self.views
            .get_mut(&action.session)
            .expect("matched live view")
            .selection = None;
        self.pending_view_project_players
            .insert(request.clone(), project);
        vec![list_online_players(&request, 256)]
    }

    pub(super) fn view_stock_answer(&mut self, answer: &OperationAnswered) -> Option<Vec<Command>> {
        let (session, handle) = self.pending_view_stock.remove(&answer.request)?;
        let state = self.views.get_mut(&session)?;
        state.reading = false;
        state.stock.clear();
        match &answer.outcome {
            OperationOutcome::Committed(committed) => match &committed.payload {
                OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) if matches!(&snapshot.endpoint, InventoryEndpoint::Warehouse(actual) if actual == &handle) =>
                {
                    state.stock.extend(snapshot.slots.iter().filter_map(|slot| {
                        slot.item.as_ref().map(|item| StockRow {
                            slot: slot.slot,
                            resource_id: item.resource_id.clone(),
                            count: item.count,
                        })
                    }));
                    state.warehouse_status =
                        format!("{} occupied warehouse slots", state.stock.len());
                }
                _ => {
                    state.warehouse_status = "Warehouse returned an unexpected snapshot".to_owned();
                }
            },
            OperationOutcome::Refused(_) => {
                state.warehouse_status = "Warehouse read refused".to_owned();
            }
        }
        Some(self.view_present(session))
    }

    pub(super) fn view_role_changed(&mut self, settlement: &str, target: &str) -> Vec<Command> {
        let sessions: Vec<u64> = self
            .views
            .iter()
            .filter(|(_, state)| state.settlement == settlement && state.player == target)
            .map(|(session, _)| *session)
            .collect();
        let mut commands = Vec::new();
        for session in sessions {
            if let Some((instance, _)) = self
                .views
                .get(&session)
                .and_then(|state| state.instance.as_ref())
            {
                commands.push(close_client_view(session, instance));
            }
            self.view_left(session);
            commands.push(message_session(
                session,
                "Settlement role changed. Reopen the view.",
            ));
        }
        commands
    }

    pub(super) fn view_reopen(&mut self, session: u64) -> Vec<Command> {
        let instance = self
            .views
            .get(&session)
            .and_then(|state| state.instance.as_ref())
            .map(|(id, _)| id.clone());
        self.view_left(session);
        instance.map_or_else(Vec::new, |id| vec![close_client_view(session, &id)])
    }

    pub(super) fn view_left(&mut self, session: u64) {
        self.views.remove(&session);
        self.pending_view_players
            .retain(|_, owner| *owner != session);
        self.pending_view_project_players
            .retain(|_, project| project.session != session);
        self.pending_view_stock
            .retain(|_, (owner, _)| *owner != session);
    }
}
