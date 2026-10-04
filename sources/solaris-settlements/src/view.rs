use solaris_plugin_sdk::events::{
    ClientSelectionOutcome, ClientSelectionStarted, ClientViewOpened, ClientViewOutcome,
    LoaderViewAction, LoaderViewRequest, OnlinePlayersAnswered, OperationAnswered,
    OperationOutcome, OperationPayload,
};
use solaris_plugin_sdk::inventories::{InventoryEndpoint, InventoryResult};
use solaris_plugin_sdk::{
    close_client_view, list_online_players, message_session, open_client_view, present_client_view,
    query_owned_inventory, Command,
};

use solaris_plugin_sdk::client_presentation::{
    BeginClientSelection, CancelClientSelection, ClientCommand, ViewAction, ViewField,
    ViewFieldValue, ViewMarker, ViewMarkerPosition, ViewModel, ViewResourceEntry, ViewRow,
};

use crate::{
    building_origin_id, can_build_settlement, is_settlement_member, warehouse_belongs_to,
    SettlementRecord, SettlementRegistry, Settlements,
};

pub(super) const VIEW_ID: &str = "solaris-settlements:overview";
pub(super) const PUBLIC_VIEW_ID: &str = "solaris-settlements:player_contracts";
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
const MAP_SLOTS: usize = 10;

const OPEN_DOMAIN: &str = "solaris-settlements:open_domain";
const CONFIRM_TERMS: &str = "solaris-settlements:confirm_terms";
const CANCEL_TERMS: &str = "solaris-settlements:cancel_terms";
const DOMAINS: &[&str] = &[
    "overview",
    "production",
    "technology",
    "wages",
    "trade",
    "caravan",
    "squads",
];

#[derive(Clone, Copy)]
enum DomainRead {
    Technology,
    Wages,
    Trade,
    Caravan,
    PaidHeader,
    PaidTerms,
    Npc,
    Controls,
    Carry,
    Equipment,
    Warehouse,
    Building,
    Destination,
    Route,
    PlayerContext,
}

struct Terms {
    operation: String,
    arguments: Vec<String>,
    signature: String,
    confirming: bool,
}

#[derive(Default)]
struct DomainState {
    pane: usize,
    option: usize,
    resident: usize,
    selected_resident: Option<String>,
    building: usize,
    destination: usize,
    record: usize,
    known: Vec<(String, SettlementRecord)>,
    pending: std::collections::BTreeMap<String, DomainRead>,
    versions: std::collections::BTreeMap<String, Option<u64>>,
    production: crate::production::Journal,
    technology: crate::technology_runtime::Store,
    wages: crate::commerce_runtime::Store,
    trade: crate::commerce_trade::Store,
    caravan: crate::caravan::Store,
    paid_header: Option<crate::payment_mandate::Mandate>,
    paid_terms: Option<crate::payment_mandate::Terms>,
    paid_chunks: Vec<String>,
    player_context: Option<solaris_plugin_sdk::events::PlayerSnapshot>,
    native: std::collections::BTreeMap<String, serde_json::Value>,
    errors: std::collections::BTreeMap<String, String>,
    form: std::collections::BTreeMap<String, ViewFieldValue>,
    squad: std::collections::BTreeSet<String>,
    terms: Option<Terms>,
    notice: String,
    observed_tick: u64,
    delivered_tick: Option<u64>,
    last_action_sequence: Option<u64>,
}

pub(super) struct ResidentRead {
    pub(super) player: String,
    pub(super) scope: String,
    pub(super) alias: String,
    pub(super) request: String,
    handle: Option<String>,
    pending: std::collections::BTreeMap<String, DomainRead>,
    native: std::collections::BTreeMap<String, serde_json::Value>,
    observed_tick: u64,
}

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
    anchor: solaris_plugin_sdk::world_structures::BlockPosition,
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
    pub(super) building_status: std::collections::BTreeMap<String, String>,
    pub(super) reading: bool,
    pub(super) dirty: bool,
    pub(super) anchor: [i32; 3],
    pub(super) rotation: u16,
    pub(super) pending_selection: Option<(String, u16)>,
    pub(super) selection: Option<(String, u16)>,
    domain: DomainState,
    view_key: String,
}

pub(super) struct PublicContractView {
    player: solaris_plugin_sdk::events::PlayerSnapshot,
    request: String,
    instance: Option<(String, u64)>,
    identity: Option<(String, Vec<String>)>,
    pending: Option<String>,
    contracts: Option<Vec<serde_json::Value>>,
    status: Option<serde_json::Value>,
    selected: String,
    receipt: Option<serde_json::Value>,
    terms: Option<(String, serde_json::Value, bool)>,
    page: usize,
    sequence: Option<u64>,
    notice: String,
    observed: u64,
    delivered: Option<u64>,
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
        if self.domain.pane != 0 {
            return self.domain_model();
        }
        let roster_pages = self.record.residents.len().div_ceil(PAGE_ROWS).max(1);
        let building_pages = self.record.buildings.len().div_ceil(PAGE_ROWS);
        let stock_pages = self.stock.len().div_ceil(PAGE_ROWS).max(1);
        let mut status_rows = Vec::new();
        if can_build_settlement(&self.record, &self.player) {
            for kind in ["family-status", "development-status"] {
                if let Some(value) = self.domain.native.get(kind) {
                    fact_rows(&mut status_rows, kind, value);
                }
                if let Some(reason) = self.domain.errors.get(kind) {
                    status_rows.push(row("Status unavailable", kind, reason));
                }
            }
        }
        let page_count =
            1 + roster_pages + building_pages + stock_pages + status_rows.len().div_ceil(PAGE_ROWS);
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
                .map(|(name, building)| {
                    row(
                        name,
                        building_origin_id(building),
                        self.building_status
                            .get(name)
                            .map(String::as_str)
                            .unwrap_or("Checking live building conditions"),
                    )
                })
                .collect()
        } else if self.page <= roster_pages + building_pages + stock_pages {
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
        } else {
            status_rows
                .into_iter()
                .skip((self.page - roster_pages - building_pages - stock_pages - 1) * PAGE_ROWS)
                .take(PAGE_ROWS)
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
                    id: "domain".to_owned(),
                    value: ViewFieldValue::Selected("overview".to_owned()),
                },
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
                action(OPEN_DOMAIN, "Open domain", true, ""),
                action(
                    REFRESH,
                    "Refresh",
                    !self.reading,
                    "Warehouse read in progress",
                ),
                action(
                    NEXT,
                    "Next records",
                    self.page + 1 < page_count,
                    "Last page",
                ),
                action(PREVIOUS, "Previous records", self.page > 0, "First page"),
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
                        position: None,
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
        if self.views.len() + self.public_contract_views.len() >= 16
            && !self.views.contains_key(&session)
            && !self.public_contract_views.contains_key(&session)
        {
            return vec![message_session(
                session,
                "Too many settlement views are open.",
            )];
        }
        let mut commands = Vec::new();
        if let Some(public) = self.public_contract_views.remove(&session) {
            if let Some((instance, _)) = public.instance {
                commands.push(close_client_view(session, &instance));
            }
        }
        let previous = self.views.remove(&session);
        let same_settlement = previous
            .as_ref()
            .is_some_and(|state| state.settlement == name);
        let pane = self
            .requested_view_panes
            .remove(&session)
            .unwrap_or_else(|| {
                previous
                    .as_ref()
                    .filter(|_| same_settlement)
                    .map_or(0, |state| state.domain.pane)
            });
        let view_key = format!("solaris-settlements:{}", DOMAINS[pane]);
        let same_view = same_settlement
            && previous
                .as_ref()
                .is_some_and(|state| state.view_key == view_key && state.instance.is_some());
        let (request, instance, stock, anchor, rotation, mut domain) = if same_settlement {
            let state = previous.expect("same settlement implies prior view");
            let selected_resident = state.resident_alias().map(str::to_owned);
            let mut request = state.request;
            let mut instance = state.instance;
            if !same_view {
                if let Some((id, _)) = instance.take() {
                    commands.push(close_client_view(session, &id));
                }
                let Some(next) = self.next_storage_request("view-open") else {
                    return vec![message_session(session, "Settlement view is unavailable.")];
                };
                request = next;
            }
            let mut domain = state.domain;
            domain.selected_resident = selected_resident;
            if state.record.revision != record.revision
                || state.record.residents.len() != record.residents.len()
                || state.record.residents.iter().any(|(alias, resident)| {
                    record.residents.get(alias).is_none_or(|current| {
                        current.handle != resident.handle || current.role != resident.role
                    })
                })
            {
                domain.terms = None;
                domain.notice =
                    "Roster or rights changed; review the selected resident and terms again."
                        .to_owned();
            }
            if !same_view {
                domain.last_action_sequence = None;
            }
            domain
                .squad
                .retain(|alias| record.residents.contains_key(alias));
            if let Some(alias) = &domain.selected_resident {
                if let Some(index) = record
                    .residents
                    .keys()
                    .position(|candidate| candidate == alias)
                {
                    domain.resident = index;
                }
            }
            (
                request,
                instance,
                state.stock,
                state.anchor,
                state.rotation,
                domain,
            )
        } else {
            if let Some((instance, _)) = previous.and_then(|state| state.instance) {
                commands.push(close_client_view(session, &instance));
            }
            let Some(request) = self.next_storage_request("view-open") else {
                return vec![message_session(session, "Settlement view is unavailable.")];
            };
            (
                request,
                None,
                Vec::new(),
                [0, 64, 0],
                0,
                DomainState::default(),
            )
        };
        domain.pane = pane;
        self.pending_view_stock
            .retain(|_, (owner, _)| *owner != session);
        self.pending_view_buildings
            .retain(|_, (owner, _, _)| *owner != session);
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
                building_status: Default::default(),
                reading: false,
                dirty: false,
                anchor,
                rotation,
                pending_selection: None,
                selection: None,
                domain,
                view_key: view_key.clone(),
            },
        );
        if let Some(state) = self.views.get_mut(&session) {
            state.domain.known = registry
                .settlements
                .iter()
                .filter(|(_, record)| is_settlement_member(record, &state.player))
                .map(|(name, record)| (name.clone(), record.clone()))
                .collect();
        }
        commands.extend(self.view_query_stock(session));
        commands.extend(self.view_query_buildings(session));
        commands.extend(self.view_query_domain(session));
        if same_view {
            commands.extend(self.view_present(session));
        } else if let Some(state) = self.views.get_mut(&session) {
            commands.push(open_client_view(
                &request,
                session,
                &view_key,
                state.model(),
            ));
        }
        commands
    }

    fn view_query_buildings(&mut self, session: u64) -> Vec<Command> {
        let Some(state) = self.views.get(&session) else {
            return Vec::new();
        };
        if state.domain.pane != 0 {
            return Vec::new();
        }
        let roster_pages = state.record.residents.len().div_ceil(PAGE_ROWS).max(1);
        let building_pages = state.record.buildings.len().div_ceil(PAGE_ROWS);
        if state.page <= roster_pages || state.page > roster_pages + building_pages {
            return Vec::new();
        }
        let offset = (state.page - roster_pages - 1) * PAGE_ROWS;
        let buildings: Vec<_> = state
            .record
            .buildings
            .iter()
            .skip(offset)
            .take(PAGE_ROWS)
            .map(|(name, building)| {
                (
                    name.clone(),
                    building_origin_id(building).to_owned(),
                    matches!(building.origin, crate::BuildingOrigin::Certificate { .. }),
                )
            })
            .collect();
        let mut commands = Vec::new();
        for (name, id, manual) in buildings {
            let Some(request) = self.next_storage_request("view-building") else {
                break;
            };
            self.pending_view_buildings
                .insert(request.clone(), (session, name, id.clone()));
            commands.push(if manual {
                crate::check_worksite(&request, &id)
            } else {
                crate::check_structure(&request, &id)
            });
        }
        commands
    }

    pub(super) fn view_building_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let (session, name, expected_id) =
            self.pending_view_buildings.get(&answer.request)?.clone();
        if answer.operation_id.is_some() {
            return Some(Vec::new());
        }
        self.pending_view_buildings.remove(&answer.request);
        let Some(state) = self.views.get_mut(&session) else {
            return Some(Vec::new());
        };
        if !state
            .record
            .buildings
            .get(&name)
            .is_some_and(|building| building_origin_id(building) == expected_id)
        {
            return Some(Vec::new());
        }
        let status = match &answer.outcome {
            OperationOutcome::Committed(committed) => match &committed.payload {
                OperationPayload::WorldStructure(crate::WorldStructureResult::WorksiteCheck(
                    check,
                )) if check.worksite_id == expected_id => {
                    if check.valid {
                        "usable — live physical check".to_owned()
                    } else {
                        format!(
                            "Blocked: {}; restore and refresh",
                            crate::worksite_explanation(check)
                        )
                    }
                }
                OperationPayload::WorldStructure(crate::WorldStructureResult::StructureCheck(
                    check,
                )) if check.structure_id == expected_id => {
                    if check.valid {
                        "usable — live physical check".to_owned()
                    } else {
                        format!(
                            "Blocked: {}; restore and refresh",
                            crate::physical_points_explanation(&check.blocked_points)
                        )
                    }
                }
                _ => "Unavailable: building query did not match; refresh".to_owned(),
            },
            OperationOutcome::Refused(refused) => format!(
                "Unavailable: {}",
                crate::building_failure_explanation(refused.reason)
            ),
        };
        state.building_status.insert(name, status);
        Some(self.view_present(session))
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
        let Some(endpoint) = crate::warehouse_endpoint(&handle) else {
            if let Some(state) = self.views.get_mut(&session) {
                state.stock.clear();
                state.warehouse_status =
                    "Bound warehouse has no valid physical container".to_owned();
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
        vec![query_owned_inventory(&request, endpoint, None)]
    }

    pub(super) fn view_status_audience(&self, session: u64) -> Option<(String, String)> {
        let state = self.views.get(&session)?;
        if state.domain.pane != 0 || !can_build_settlement(&state.record, &state.player) {
            return None;
        }
        Some((state.settlement.clone(), state.instance.as_ref()?.0.clone()))
    }

    pub(super) fn view_status_observed(
        &mut self,
        session: u64,
        audience: &(String, String),
        kind: &str,
        result: Result<serde_json::Value, String>,
    ) -> Vec<Command> {
        let key = match kind {
            "family" => "family-status",
            "development" => "development-status",
            _ => return Vec::new(),
        };
        let Some(state) = self.views.get_mut(&session).filter(|state| {
            state.domain.pane == 0
                && state.settlement == audience.0
                && state
                    .instance
                    .as_ref()
                    .is_some_and(|(instance, _)| instance == &audience.1)
                && can_build_settlement(&state.record, &state.player)
        }) else {
            return Vec::new();
        };
        match result {
            Ok(value) => {
                let scope = if kind == "family" {
                    value
                        .get("household")
                        .and_then(|household| household.get("settlement"))
                } else {
                    value.get("scope")
                };
                if scope.and_then(serde_json::Value::as_str) != Some(state.settlement.as_str()) {
                    return Vec::new();
                }
                state.domain.errors.remove(key);
                state.domain.native.insert(key.to_owned(), value);
            }
            Err(reason) => {
                state.domain.native.remove(key);
                state.domain.errors.insert(key.to_owned(), reason);
            }
        }
        self.view_present(session)
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
        let pane = if request.request_kind == PUBLIC_VIEW_ID {
            DOMAINS.len()
        } else if let Some(pane) = DOMAINS
            .iter()
            .position(|domain| request.request_kind == format!("solaris-settlements:{domain}"))
        {
            pane
        } else {
            return Vec::new();
        };
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
            .insert(id.clone(), (request.session, pane));
        vec![list_online_players(&id, 256)]
    }

    pub(super) fn view_players_answered(&mut self, answer: &OnlinePlayersAnswered) -> Vec<Command> {
        if let Some(commands) = self.view_public_players(answer) {
            return commands;
        }
        let context_session = self.views.iter().find_map(|(session, state)| {
            state
                .domain
                .pending
                .get(&answer.request)
                .is_some_and(|read| matches!(read, DomainRead::PlayerContext))
                .then_some(*session)
        });
        if let Some(session) = context_session {
            let state = self.views.get_mut(&session).expect("matched live view");
            state.domain.pending.remove(&answer.request);
            state.domain.player_context = answer
                .players
                .iter()
                .find(|player| player.session == session && player.player == state.player)
                .cloned();
            if state.domain.player_context.is_none() {
                state.domain.errors.insert(
                    DomainRead::PlayerContext.key().to_owned(),
                    "Current native player identity and pose are unavailable.".to_owned(),
                );
            }
            return self.view_domain_ready(session);
        }
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
        let Some((session, pane)) = self.pending_view_players.remove(&answer.request) else {
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
        if pane == DOMAINS.len() {
            return self.view_public_open(player);
        }
        let commands = self.overview(session, &player.player, None, 0, true);
        self.requested_view_panes.insert(session, pane);
        commands
    }

    pub(super) fn view_opened(&mut self, answer: &ClientViewOpened) -> Vec<Command> {
        if let Some(state) = self.public_contract_views.get_mut(&answer.session) {
            if state.request != answer.request || state.instance.is_some() {
                return match &answer.outcome {
                    ClientViewOutcome::Opened(opened) => {
                        vec![close_client_view(answer.session, &opened.view_instance_id)]
                    }
                    ClientViewOutcome::Refused(_) => Vec::new(),
                };
            }
            return match &answer.outcome {
                ClientViewOutcome::Opened(opened) => {
                    state.instance = Some((opened.view_instance_id.clone(), opened.revision));
                    self.view_public_present(answer.session)
                }
                ClientViewOutcome::Refused(reason) => {
                    self.public_contract_views.remove(&answer.session);
                    vec![message_session(
                        answer.session,
                        format!("Player contract view refused: {reason:?}."),
                    )]
                }
            };
        }
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
        if self.public_contract_views.contains_key(&action.session) {
            return self.view_public_action(action);
        }
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
        if !state
            .instance
            .as_ref()
            .is_some_and(|(_, revision)| *revision == action.view_revision)
        {
            return vec![message_session(
                action.session,
                "View revision changed; refresh before acting.",
            )];
        }
        if state
            .domain
            .last_action_sequence
            .is_some_and(|sequence| action.action_sequence <= sequence)
        {
            return Vec::new();
        }
        state.domain.last_action_sequence = Some(action.action_sequence);
        if action.action_id == OPEN_DOMAIN {
            let pane = action.fields.iter().find_map(|field| {
                if field.id == "domain" {
                    if let ViewFieldValue::Selected(value) = &field.value {
                        return DOMAINS.iter().position(|domain| *domain == value);
                    }
                }
                None
            });
            let Some(pane) = pane else {
                return vec![message_session(
                    action.session,
                    "Choose a declared settlement domain.",
                )];
            };
            state.domain.pane = pane;
            state.domain.terms = None;
            state.page = 0;
            let player = state.player.clone();
            let settlement = state.settlement.clone();
            return self.overview(action.session, &player, Some(&settlement), 0, false);
        }
        if state.domain.pane != 0 && !matches!(action.action_id.as_str(), REFRESH | NEXT | PREVIOUS)
        {
            return self.view_domain_action(action);
        }
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
            anchor: solaris_plugin_sdk::world_structures::BlockPosition {
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
                OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot))
                    if crate::container_identity(&snapshot.endpoint).as_deref()
                        == Some(handle.as_str()) =>
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
        Some(if state.domain.pane == 0 {
            self.view_present(session)
        } else {
            self.view_domain_ready(session)
        })
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
            .or_else(|| {
                self.public_contract_views
                    .get(&session)
                    .and_then(|state| state.instance.as_ref())
            })
            .map(|(id, _)| id.clone());
        self.view_left(session);
        instance.map_or_else(Vec::new, |id| vec![close_client_view(session, &id)])
    }

    pub(super) fn view_left(&mut self, session: u64) {
        self.views.remove(&session);
        self.public_contract_views.remove(&session);
        self.resident_reads.remove(&session);
        self.pending_view_players
            .retain(|_, (owner, _)| *owner != session);
        self.pending_view_project_players
            .retain(|_, project| project.session != session);
        self.pending_view_stock
            .retain(|_, (owner, _)| *owner != session);
        self.pending_view_buildings
            .retain(|_, (owner, _, _)| *owner != session);
    }
}

impl DomainRead {
    fn key(self) -> &'static str {
        match self {
            Self::Technology => "technology",
            Self::Wages => "wages",
            Self::Trade => "trade",
            Self::Caravan => "caravan",
            Self::Npc => "npc",
            Self::Controls => "controls",
            Self::Carry => "carry",
            Self::Equipment => "equipment",
            Self::Warehouse => "warehouse",
            Self::Building => "building",
            Self::Destination => "destination",
            Self::Route => "route",
            Self::PlayerContext => "player-context",
            Self::PaidHeader | Self::PaidTerms => "payment",
        }
    }
}

fn authored_building_policy(blueprint_id: &str) -> Option<&'static serde_json::Value> {
    static CATALOG: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("building_policy.json"))
            .expect("canonical guest-owned authored building policy")
    });
    CATALOG.get("blueprints")?.get(blueprint_id)
}

fn native_value(
    read: DomainRead,
    answer: &OperationAnswered,
    handle: &str,
) -> Result<serde_json::Value, String> {
    use solaris_plugin_sdk::events::{OperationOutcome, OperationPayload};
    use solaris_plugin_sdk::npcs::{NpcOrderResult, NpcResult};
    if answer.operation_id.is_some() {
        return Err("Expected a native read, not a mutation receipt".to_owned());
    }
    let OperationOutcome::Committed(committed) = &answer.outcome else {
        let OperationOutcome::Refused(refused) = &answer.outcome else {
            unreachable!()
        };
        return Err(format!(
            "Native {} refused: {:?}",
            read.key(),
            refused.reason
        ));
    };
    let value = match (read, &committed.payload) {
        (DomainRead::Npc, OperationPayload::Npc(NpcResult::Page(page))) => page
            .npcs
            .iter()
            .find(|npc| npc.handle == handle)
            .and_then(|npc| serde_json::to_value(npc).ok()),
        (DomainRead::Controls, OperationPayload::NpcOrder(NpcOrderResult::Controls(page))) => {
            serde_json::to_value(&page.npcs).ok()
        }
        (
            DomainRead::Carry | DomainRead::Equipment | DomainRead::Warehouse,
            OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)),
        ) => serde_json::to_value(snapshot).ok(),
        (
            DomainRead::Building | DomainRead::Destination,
            OperationPayload::WorldStructure(crate::WorldStructureResult::WorksiteCheck(check)),
        ) => serde_json::to_value(check).ok(),
        (
            DomainRead::Building | DomainRead::Destination,
            OperationPayload::WorldStructure(crate::WorldStructureResult::StructureCheck(check)),
        ) => serde_json::to_value(check).ok(),
        (
            DomainRead::Route,
            OperationPayload::NpcRoute(solaris_plugin_sdk::npc_routes::NpcRouteResult::Status(
                status,
            )),
        ) => serde_json::to_value(status).ok(),
        _ => None,
    };
    value.ok_or_else(|| {
        format!(
            "Native {} answer did not match the requested facts",
            read.key()
        )
    })
}

impl ViewState {
    fn resident_alias(&self) -> Option<&str> {
        if let Some(alias) = &self.domain.selected_resident {
            return self
                .record
                .residents
                .contains_key(alias)
                .then_some(alias.as_str());
        }
        self.record
            .residents
            .keys()
            .nth(self.domain.resident % self.record.residents.len().max(1))
            .map(String::as_str)
    }
    fn handle(&self) -> Option<&str> {
        self.record
            .residents
            .get(self.resident_alias()?)
            .map(|resident| resident.handle.as_str())
    }
    fn building_name(&self) -> Option<&str> {
        self.record
            .buildings
            .keys()
            .nth(self.domain.building % self.record.buildings.len().max(1))
            .map(String::as_str)
    }
    fn destinations(&self) -> Vec<(String, String)> {
        self.domain
            .known
            .iter()
            .filter(|(_, record)| can_build_settlement(record, &self.player))
            .flat_map(|(scope, record)| {
                record
                    .buildings
                    .iter()
                    .filter(|(_, building)| !building.storage_positions.is_empty())
                    .map(move |(name, _)| (scope.clone(), name.clone()))
            })
            .collect()
    }
    fn destination(&self) -> Option<(String, String)> {
        let known = self.destinations();
        known
            .get(self.domain.destination % known.len().max(1))
            .cloned()
    }
    fn records(&self) -> Vec<String> {
        let domain = &self.domain;
        match domain.pane {
            1 | 2 => domain
                .production
                .jobs
                .iter()
                .filter(|(_, job)| job.intent.settlement == self.settlement)
                .map(|(id, _)| id.clone())
                .collect(),
            3 => domain
                .wages
                .ledger
                .employment
                .iter()
                .filter(|(_, employment)| {
                    employment.owner == self.player
                        && self
                            .record
                            .residents
                            .values()
                            .any(|resident| resident.handle == employment.worker)
                })
                .map(|(id, _)| id.clone())
                .collect(),
            4 => domain
                .trade
                .ledger
                .trades
                .iter()
                .filter(|(id, trade)| {
                    trade.quote.owner == self.player
                        && domain
                            .trade
                            .plans
                            .get(*id)
                            .is_some_and(|plan| plan.scope == self.settlement)
                })
                .map(|(id, _)| id.clone())
                .collect(),
            5 => domain
                .caravan
                .contracts
                .iter()
                .filter(|(_, contract)| {
                    contract.owner() == self.player && contract.source.scope == self.settlement
                })
                .map(|(id, _)| id.clone())
                .collect(),
            _ => Vec::new(),
        }
    }
    fn selected_record(&self) -> Option<String> {
        let records = self.records();
        records
            .get(self.domain.record % records.len().max(1))
            .cloned()
    }
    fn options(&self) -> Vec<String> {
        match self.domain.pane {
            1 => crate::production::selectable_recipes()
                .map(str::to_owned)
                .collect(),
            2 => crate::technology::initial_definitions()
                .into_keys()
                .collect(),
            4 | 5 => self
                .stock
                .iter()
                .map(|stock| stock.resource_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
            6 => ["militia", "infantry", "spearman", "archer", "medic"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    }
    fn selected_option(&self) -> Option<String> {
        let options = self.options();
        options
            .get(self.domain.option % options.len().max(1))
            .cloned()
    }
    fn controls(&self, handle: &str) -> Option<&serde_json::Value> {
        self.domain
            .native
            .get("controls")?
            .as_array()?
            .iter()
            .find(|control| {
                control.get("handle").and_then(serde_json::Value::as_str) == Some(handle)
            })
    }
    fn signature(&self) -> String {
        let key = DOMAINS[self.domain.pane];
        let npc = self.domain.native.get("npc");
        serde_json::json!({
            "registry": self.record, "known_destinations": self.domain.known,
            "npc_binding": npc.map(|npc| serde_json::json!({
                "handle": npc.get("handle"), "entity_uuid": npc.get("entity_uuid"),
                "lifecycle": npc.get("lifecycle"), "revision": npc.get("revision"),
                "health": npc.get("loaded").and_then(|pose| pose.get("health")),
                "max_health": npc.get("loaded").and_then(|pose| pose.get("max_health")),
            })),
            "controls": self.domain.native.get("controls"),
            "carry": self.domain.native.get("carry"), "equipment": self.domain.native.get("equipment"),
            "warehouse": self.domain.native.get("warehouse"), "building_native": self.domain.native.get("building"),
            "destination_native": self.domain.native.get("destination"), "route_native": self.domain.native.get("route"),
            "errors": self.domain.errors,
            "store": self.domain.versions.get(key),
            "payment": self.domain.versions.get("payment"),
            "technology": (self.domain.pane == 1).then(|| self.domain.versions.get("technology")),
            "production_projection": matches!(self.domain.pane, 1 | 2).then(|| self.selected_record().and_then(|id| self.domain.production.jobs.get(&id))),
            "option": self.selected_option(), "resident": self.resident_alias(),
            "building": self.building_name(), "destination": self.destination(),
            "record": self.selected_record(), "squad": self.domain.squad,
        })
        .to_string()
    }
}

impl Settlements {
    fn view_query_domain(&mut self, session: u64) -> Vec<Command> {
        let Some(pane) = self.views.get(&session).map(|state| state.domain.pane) else {
            return Vec::new();
        };
        if pane == 0 {
            return Vec::new();
        }
        let production = self.production_snapshot().cloned();
        let state = self.views.get_mut(&session).expect("view remains owned");
        state.domain.pending.clear();
        state.domain.native.clear();
        state.domain.errors.clear();
        state.domain.player_context = None;
        state.domain.paid_header = None;
        state.domain.paid_terms = None;
        state.domain.paid_chunks.clear();
        state.domain.versions.remove("payment");
        if let Some(production) = production {
            state.domain.production = production;
        } else {
            state.domain.errors.insert("production".to_owned(), "Production metadata and exact per-job receipts are still being reconstructed; no empty or completed projection is invented".to_owned());
        }
        state.domain.observed_tick = self.view_tick;
        state.domain.delivered_tick = None;
        let handle = state.handle().map(str::to_owned);
        let mut handles: Vec<String> = state
            .domain
            .squad
            .iter()
            .filter_map(|alias| state.record.residents.get(alias))
            .map(|resident| resident.handle.clone())
            .collect();
        if let Some(handle) = &handle {
            if !handles.contains(handle) {
                handles.push(handle.clone());
            }
        }
        let selected_building = state
            .building_name()
            .and_then(|name| state.record.buildings.get(name))
            .cloned();
        let destination = state.destination().and_then(|(scope, name)| {
            state
                .domain
                .known
                .iter()
                .find(|(candidate, _)| *candidate == scope)
                .and_then(|(_, record)| record.buildings.get(&name))
                .cloned()
        });
        let warehouse = self
            .warehouse
            .as_ref()
            .filter(|binding| warehouse_belongs_to(&state.record, binding))
            .and_then(|binding| crate::warehouse_endpoint(&binding.handle));
        let mut reads: Vec<(DomainRead, Command)> = Vec::new();
        for (kind, key) in [
            (DomainRead::Technology, crate::technology_runtime::STORE_KEY),
            (DomainRead::Wages, crate::commerce_runtime::KEY),
            (DomainRead::Trade, crate::commerce_trade::KEY),
            (DomainRead::Caravan, crate::caravan_runtime::KEY),
        ] {
            let Some(request) = self.next_storage_request("view-domain") else {
                break;
            };
            reads.push((kind, solaris_plugin_sdk::storage_get(&request, key)));
        }
        if let Some(handle) = handle {
            for kind in [
                DomainRead::Npc,
                DomainRead::Controls,
                DomainRead::Carry,
                DomainRead::Equipment,
            ] {
                let Some(request) = self.next_storage_request("view-native") else {
                    break;
                };
                let command = match kind {
                    DomainRead::Npc => solaris_plugin_sdk::query_npcs(
                        &request,
                        std::slice::from_ref(&handle),
                        None,
                    ),
                    DomainRead::Controls => {
                        solaris_plugin_sdk::query_npc_controls(&request, &handles)
                    }
                    DomainRead::Carry => query_owned_inventory(
                        &request,
                        InventoryEndpoint::NpcCarry(handle.clone()),
                        None,
                    ),
                    DomainRead::Equipment => query_owned_inventory(
                        &request,
                        InventoryEndpoint::NpcEquipment(handle.clone()),
                        None,
                    ),
                    _ => unreachable!(),
                };
                reads.push((kind, command));
            }
        }
        if let Some(endpoint) = warehouse {
            if let Some(request) = self.next_storage_request("view-warehouse-image") {
                reads.push((
                    DomainRead::Warehouse,
                    query_owned_inventory(&request, endpoint, None),
                ));
            }
        }
        for (kind, building) in [
            (DomainRead::Building, selected_building),
            (DomainRead::Destination, destination),
        ] {
            if let Some(building) = building {
                if let Some(request) = self.next_storage_request("view-capability") {
                    let id = building_origin_id(&building);
                    let command =
                        if matches!(building.origin, crate::BuildingOrigin::Certificate { .. }) {
                            crate::check_worksite(&request, id)
                        } else {
                            crate::check_structure(&request, id)
                        };
                    reads.push((kind, command));
                }
            }
        }
        let state = self.views.get_mut(&session).expect("view remains owned");
        let mut commands = Vec::new();
        for (kind, command) in reads {
            let request = match &command {
                Command::StorageGet(read) => &read.request,
                Command::Operation(operation) => &operation.request,
                _ => unreachable!(),
            };
            state.domain.pending.insert(request.clone(), kind);
            commands.push(command);
        }
        if let Some(request) = self.next_storage_request("view-player-context") {
            self.views
                .get_mut(&session)
                .expect("view remains owned")
                .domain
                .pending
                .insert(request.clone(), DomainRead::PlayerContext);
            commands.push(list_online_players(&request, 256));
        } else {
            self.views
                .get_mut(&session)
                .expect("view remains owned")
                .domain
                .errors
                .insert(
                    DomainRead::PlayerContext.key().to_owned(),
                    "Native player context request is unavailable.".to_owned(),
                );
        }
        commands.push(solaris_plugin_sdk::schedule_timer("settlement-views", 20));
        commands
    }

    pub(super) fn view_domain_get(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let session = self
            .views
            .iter()
            .find(|(_, state)| state.domain.pending.contains_key(answer.request()))
            .map(|(session, _)| *session)?;
        let state = self.views.get_mut(&session)?;
        let kind = state.domain.pending.remove(answer.request())?;
        if matches!(kind, DomainRead::PaidHeader | DomainRead::PaidTerms) {
            return Some(self.view_paid_get(session, kind, answer));
        }
        let result = if let Some(record) = answer.record() {
            state
                .domain
                .versions
                .insert(kind.key().to_owned(), record.version);
            let value = record.value;
            match kind {
                DomainRead::Technology => value
                    .map(serde_json::from_str)
                    .transpose()
                    .map(|store| state.domain.technology = store.unwrap_or_default())
                    .map_err(|_| "Technology store unreadable".to_owned()),
                DomainRead::Wages => value
                    .map(serde_json::from_str)
                    .transpose()
                    .map(|store| state.domain.wages = store.unwrap_or_default())
                    .map_err(|_| "Wage store unreadable".to_owned()),
                DomainRead::Trade => value
                    .map(serde_json::from_str)
                    .transpose()
                    .map(|store| state.domain.trade = store.unwrap_or_default())
                    .map_err(|_| "Trade store unreadable".to_owned()),
                DomainRead::Caravan => value
                    .map(serde_json::from_str)
                    .transpose()
                    .map(|store| state.domain.caravan = store.unwrap_or_default())
                    .map_err(|_| "Caravan store unreadable".to_owned()),
                _ => return Some(Vec::new()),
            }
        } else {
            Err("Durable domain read unavailable".to_owned())
        };
        if let Err(reason) = result {
            state.domain.errors.insert(kind.key().to_owned(), reason);
        }
        let route = matches!(kind, DomainRead::Caravan)
            .then(|| {
                state
                    .selected_record()
                    .and_then(|id| state.domain.caravan.contracts.get(&id))
                    .and_then(|contract| contract.route_ref.clone())
            })
            .flatten();
        let paid_key = matches!(kind, DomainRead::Caravan)
            .then(|| state.selected_record())
            .flatten()
            .filter(|id| {
                state
                    .domain
                    .caravan
                    .contracts
                    .get(id)
                    .is_some_and(|contract| contract.paid_mandate.as_deref() == Some(id.as_str()))
            })
            .map(|id| crate::payment_mandate::key(&id));
        let mut commands = Vec::new();
        if let Some(key) = paid_key {
            if let Some(request) = self.next_storage_request("view-paid-header") {
                self.views
                    .get_mut(&session)
                    .expect("view remains owned")
                    .domain
                    .pending
                    .insert(request.clone(), DomainRead::PaidHeader);
                commands.push(solaris_plugin_sdk::storage_get(&request, &key));
            }
        }
        if let Some(route) = route {
            if let Some(request) = self.next_storage_request("view-route") {
                let state = self.views.get_mut(&session).expect("view remains owned");
                state
                    .domain
                    .pending
                    .insert(request.clone(), DomainRead::Route);
                commands.push(solaris_plugin_sdk::query_npc_route(&request, &route));
            }
        }
        commands.extend(self.view_domain_ready(session));
        Some(commands)
    }

    fn view_paid_get(
        &mut self,
        session: u64,
        kind: DomainRead,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Vec<Command> {
        let state = self.views.get_mut(&session).expect("matched payment read");
        let result = (|| -> Result<Option<String>, String> {
            let record = answer
                .record()
                .ok_or_else(|| "Paid shipment storage unavailable".to_owned())?;
            let value = record
                .value
                .ok_or_else(|| "Paid shipment record unavailable".to_owned())?;
            if matches!(kind, DomainRead::PaidHeader) {
                let header: crate::payment_mandate::Mandate = serde_json::from_str(value)
                    .map_err(|_| "Paid shipment header unreadable".to_owned())?;
                if state.selected_record().as_deref() != Some(header.shipment.as_str())
                    || (header.buyer != state.player && header.seller != state.player)
                    || header.contract.fragments == 0
                    || header.contract.fragments > 16
                    || header.contract.bytes > 16 * crate::payment_mandate::VALUE_BYTES
                {
                    return Err("Paid shipment is unknown, unauthorized or exceeds the bounded report budget".to_owned());
                }
                state
                    .domain
                    .versions
                    .insert("payment".to_owned(), record.version);
                let key = header.contract.key(0);
                state.domain.paid_header = Some(header);
                return Ok(Some(key));
            }
            if value.len() > crate::payment_mandate::VALUE_BYTES {
                return Err("Paid shipment fragment exceeds its bounded budget".to_owned());
            }
            state.domain.paid_chunks.push(value.to_owned());
            let header = state
                .domain
                .paid_header
                .as_ref()
                .ok_or_else(|| "Paid shipment header absent".to_owned())?;
            if state.domain.paid_chunks.len() < header.contract.fragments {
                return Ok(Some(header.contract.key(state.domain.paid_chunks.len())));
            }
            let terms: crate::payment_mandate::Terms = header
                .contract
                .decode(&state.domain.paid_chunks)
                .map_err(str::to_owned)?;
            terms.validate().map_err(str::to_owned)?;
            if terms.shipment != header.shipment
                || terms.buyer != header.buyer
                || terms.seller != header.seller
                || terms.buyer_scope != header.buyer_scope
                || terms.recipient != header.recipient
                || terms.recipient_scope != header.recipient_scope
                || terms.recipient_entity_uuid != header.recipient_entity_uuid
                || !state
                    .domain
                    .caravan
                    .contracts
                    .get(&terms.shipment)
                    .is_some_and(|contract| terms.matches_caravan(contract))
            {
                return Err(
                    "Paid shipment terms do not match the original selected contract".to_owned(),
                );
            }
            state.domain.paid_terms = Some(terms);
            state.domain.paid_chunks.clear();
            Ok(None)
        })();
        match result {
            Ok(Some(key)) => {
                if let Some(request) = self.next_storage_request("view-paid-terms") {
                    self.views
                        .get_mut(&session)
                        .expect("view remains owned")
                        .domain
                        .pending
                        .insert(request.clone(), DomainRead::PaidTerms);
                    return vec![solaris_plugin_sdk::storage_get(&request, &key)];
                }
                self.views
                    .get_mut(&session)
                    .expect("view remains owned")
                    .domain
                    .errors
                    .insert(
                        "payment".to_owned(),
                        "Paid shipment read request unavailable".to_owned(),
                    );
            }
            Err(reason) => {
                state.domain.paid_header = None;
                state.domain.paid_terms = None;
                state.domain.paid_chunks.clear();
                state.domain.errors.insert("payment".to_owned(), reason);
            }
            Ok(None) => {}
        }
        self.view_domain_ready(session)
    }

    pub(super) fn view_domain_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let session = self
            .views
            .iter()
            .find(|(_, state)| state.domain.pending.contains_key(&answer.request))
            .map(|(session, _)| *session)?;
        let state = self.views.get_mut(&session)?;
        let kind = state.domain.pending.remove(&answer.request)?;
        match native_value(kind, answer, state.handle().unwrap_or("")) {
            Ok(value) => {
                state.domain.native.insert(kind.key().to_owned(), value);
            }
            Err(reason) => {
                state.domain.errors.insert(kind.key().to_owned(), reason);
            }
        }
        Some(self.view_domain_ready(session))
    }

    fn view_domain_ready(&mut self, session: u64) -> Vec<Command> {
        let Some(state) = self.views.get_mut(&session) else {
            return Vec::new();
        };
        if !state.domain.pending.is_empty() || state.reading {
            return Vec::new();
        }
        state.domain.delivered_tick = Some(self.view_tick);
        let terms = state.domain.terms.take();
        if let Some(terms) = terms {
            if terms.confirming {
                if terms.signature != state.signature() {
                    state.domain.notice = "Rights, revisions, inventory images or selected terms changed; review again.".to_owned();
                    return self.view_present(session);
                }
                if let Err(reason) = state.operation_reason(&terms.operation) {
                    state.domain.notice = reason;
                    return self.view_present(session);
                }
                let Some(player) = state.domain.player_context.as_ref() else {
                    state.domain.notice =
                        "Current native player context is unavailable; no intent submitted."
                            .to_owned();
                    return self.view_present(session);
                };
                let invoked = solaris_plugin_sdk::events::CommandInvoked {
                    session,
                    player: state.player.clone(),
                    username: player.name.clone(),
                    // A view action does not authorize operator command privileges.
                    operator: false,
                    position: player.position,
                    name: "settlement".to_owned(),
                    raw_arguments: terms.arguments.join(" "),
                    arguments: terms.arguments,
                };
                state.domain.notice = "Intent submitted; only native receipts establish the result. Refresh for the outcome.".to_owned();
                let mut commands = self.command(&invoked, self.view_tick);
                commands.extend(self.view_present(session));
                commands.push(solaris_plugin_sdk::schedule_timer("settlement-views", 20));
                return commands;
            }
            state.domain.terms = Some(terms);
        }
        self.view_present(session)
    }

    pub(super) fn view_timer(&mut self) -> Vec<Command> {
        if self.pending_registry_read.is_some() {
            return vec![solaris_plugin_sdk::schedule_timer("settlement-views", 20)];
        }
        let candidate = self
            .views
            .iter()
            .filter(|(_, state)| {
                state.domain.pane != 0
                    && state.domain.pending.is_empty()
                    && !state.reading
                    && state.domain.terms.is_none()
            })
            .min_by_key(|(_, state)| state.domain.observed_tick)
            .map(|(session, state)| {
                (
                    *session,
                    state.player.clone(),
                    state.settlement.clone(),
                    state.page,
                )
            });
        candidate.map_or_else(Vec::new, |(session, player, scope, page)| {
            self.overview(session, &player, Some(&scope), page, false)
        })
    }

    pub(super) fn resident_state_command(
        &mut self,
        invoked: &solaris_plugin_sdk::events::CommandInvoked,
    ) -> Option<Vec<Command>> {
        if invoked.arguments.first().map(String::as_str) != Some("resident-state") {
            return None;
        }
        let [_, scope, alias] = invoked.arguments.as_slice() else {
            return Some(vec![message_session(
                invoked.session,
                "Usage: resident-state <settlement> <resident-alias>",
            )]);
        };
        if self.resident_reads.contains_key(&invoked.session) || self.resident_reads.len() >= 16 {
            return Some(vec![message_session(
                invoked.session,
                "Resident state read busy.",
            )]);
        }
        let request = self.next_storage_request("resident-state-registry")?;
        self.resident_reads.insert(
            invoked.session,
            ResidentRead {
                player: invoked.player.clone(),
                scope: scope.clone(),
                alias: alias.clone(),
                request: request.clone(),
                handle: None,
                pending: Default::default(),
                native: Default::default(),
                observed_tick: self.view_tick,
            },
        );
        Some(self.registry_read(&request))
    }

    pub(super) fn resident_state_get(
        &mut self,
        answer: &impl crate::registry_runtime::StorageRead,
    ) -> Option<Vec<Command>> {
        let session = self
            .resident_reads
            .iter()
            .find(|(_, read)| read.request == answer.request())
            .map(|(session, _)| *session)?;
        let mut read = self.resident_reads.remove(&session)?;
        let registry = match answer.record() {
            Some(record) => match record.registry {
                Some(registry) => registry,
                None => {
                    return Some(vec![message_session(
                        session,
                        "Resident registry unavailable or unreadable.",
                    )])
                }
            },
            None => {
                return Some(vec![message_session(
                    session,
                    "Resident registry unavailable.",
                )])
            }
        };
        let handle = registry
            .settlements
            .get(&read.scope)
            .filter(|record| is_settlement_member(record, &read.player))
            .and_then(|record| record.residents.get(&read.alias))
            .map(|resident| resident.handle.clone());
        let Some(handle) = handle else {
            return Some(vec![message_session(
                session,
                "Resident is unknown or settlement membership is absent.",
            )]);
        };
        let mut commands = Vec::new();
        for kind in [
            DomainRead::Npc,
            DomainRead::Controls,
            DomainRead::Carry,
            DomainRead::Equipment,
        ] {
            let Some(request) = self.next_storage_request("resident-state-native") else {
                break;
            };
            commands.push(match kind {
                DomainRead::Npc => {
                    solaris_plugin_sdk::query_npcs(&request, std::slice::from_ref(&handle), None)
                }
                DomainRead::Controls => {
                    solaris_plugin_sdk::query_npc_controls(&request, std::slice::from_ref(&handle))
                }
                DomainRead::Carry => query_owned_inventory(
                    &request,
                    InventoryEndpoint::NpcCarry(handle.clone()),
                    None,
                ),
                DomainRead::Equipment => query_owned_inventory(
                    &request,
                    InventoryEndpoint::NpcEquipment(handle.clone()),
                    None,
                ),
                _ => unreachable!(),
            });
            read.pending.insert(request, kind);
        }
        read.handle = Some(handle);
        self.resident_reads.insert(session, read);
        Some(commands)
    }

    pub(super) fn resident_state_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let session = self
            .resident_reads
            .iter()
            .find(|(_, read)| read.pending.contains_key(&answer.request))
            .map(|(session, _)| *session)?;
        let read = self.resident_reads.get_mut(&session)?;
        let kind = read.pending.remove(&answer.request)?;
        let mut value = native_value(kind, answer, read.handle.as_deref().unwrap_or(""))
            .unwrap_or_else(|reason| serde_json::json!({"refused": reason}));
        if matches!(kind, DomainRead::Controls) {
            value = value
                .as_array()
                .and_then(|values| values.iter().find(|control| {
                    control.get("handle").and_then(serde_json::Value::as_str) == read.handle.as_deref()
                }))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"refused":"Native controls omitted the selected original handle"}));
        }
        read.native.insert(kind.key().to_owned(), value);
        if !read.pending.is_empty() {
            return Some(Vec::new());
        }
        let read = self.resident_reads.remove(&session)?;
        Some(vec![message_session(
            session,
            format!(
                "Resident state {}/{} native={}",
                read.scope,
                read.alias,
                serde_json::json!({"npc":read.native.get("npc"),"controls":read.native.get("controls"),
                "carry":read.native.get("carry"),"equipment":read.native.get("equipment"),
                "observed_tick":read.observed_tick,"delivered_tick":self.view_tick})
            ),
        )])
    }
}

fn domain_action(id: &str, label: &str, enabled: bool, denial: &str) -> ViewAction {
    action(&format!("solaris-settlements:{id}"), label, enabled, denial)
}

fn fact_rows(rows: &mut Vec<ViewRow>, label: &str, value: &serde_json::Value) {
    let text = value.to_string();
    let mut rest = text.as_str();
    let mut part = 1;
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .take_while(|(offset, ch)| offset + ch.len_utf8() <= 92)
            .last()
            .map_or(rest.len(), |(offset, ch)| offset + ch.len_utf8());
        rows.push(row(label, format!("part {part}"), &rest[..end]));
        rest = &rest[end..];
        part += 1;
    }
}

impl ViewState {
    fn form_value(&self, id: &str) -> ViewFieldValue {
        self.domain
            .form
            .get(id)
            .cloned()
            .unwrap_or_else(|| match id {
                "job" | "payment" | "route" | "loadout" => ViewFieldValue::Text(String::new()),
                "quantity" | "delay" => ViewFieldValue::Number(1.0),
                "x" => ViewFieldValue::Number(f64::from(self.anchor[0])),
                "y" => ViewFieldValue::Number(f64::from(self.anchor[1])),
                "z" => ViewFieldValue::Number(f64::from(self.anchor[2])),
                _ => ViewFieldValue::Number(0.0),
            })
    }

    fn operation_reason(&self, operation: &str) -> Result<(), String> {
        if !self.domain.pending.is_empty() || self.reading {
            return Err("Reading fresh native facts".to_owned());
        }
        if self.domain.player_context.is_none() {
            return Err("Current native player identity and pose are unavailable".to_owned());
        }
        if matches!(
            operation,
            "squad_toggle" | "squad_march" | "squad_retreat" | "squad_halt"
        ) {
            if !crate::combat::can_command_settlement(&self.record, &self.player) {
                return Err("Owner, steward or captain command rights required".to_owned());
            }
        } else if !can_build_settlement(&self.record, &self.player) {
            return Err("Owner or steward rights required".to_owned());
        }
        for key in [
            DOMAINS[self.domain.pane],
            if self.domain.pane == 1 {
                "technology"
            } else if self.domain.pane == 2 {
                "production"
            } else {
                DOMAINS[self.domain.pane]
            },
        ] {
            if let Some(reason) = self.domain.errors.get(key) {
                return Err(reason.clone());
            }
        }
        if operation == "employment_stop" {
            return self
                .selected_record()
                .map(|_| ())
                .ok_or_else(|| "Select an existing employment contract".to_owned());
        }
        let resident = self
            .resident_alias()
            .and_then(|alias| self.record.residents.get(alias))
            .ok_or_else(|| "Select a known resident".to_owned())?;
        if resident.captive_by.is_some()
            || resident.capturing.is_some()
            || resident.hiring.is_some()
            || resident.dismissing.is_some()
            || resident.capture_recovery.is_some()
        {
            return Err("Resident is captive or changing service/custody".to_owned());
        }
        if matches!(
            operation,
            "production"
                | "employment"
                | "trade_quote"
                | "trade_accept"
                | "caravan"
                | "move_workplace"
                | "stop_move"
        ) && !crate::production_runtime::eligible_civilian(resident)
        {
            return Err("An available civilian resident is required".to_owned());
        }
        if operation == "hire" {
            if resident.role.is_some() {
                return Err("Resident already serves a role".to_owned());
            }
            let role = self
                .selected_option()
                .and_then(|name| crate::hire::Role::parse(&name))
                .ok_or_else(|| "Choose an existing military role".to_owned())?;
            let ViewFieldValue::Text(loadout) = self.form_value("loadout") else {
                return Err("Infantry loadout must be text".to_owned());
            };
            self.kit_plan(role, (!loadout.is_empty()).then_some(loadout.as_str()))?;
        }
        if matches!(operation, "dismiss" | "squad_toggle") && resident.role.is_none() {
            return Err("Select a serving resident".to_owned());
        }
        let handle = self
            .handle()
            .ok_or_else(|| "Select a known resident".to_owned())?;
        let control = self
            .controls(handle)
            .ok_or_else(|| "Fresh native controls unavailable".to_owned())?;
        if operation == "stop_work" {
            return Ok(());
        }
        let alive = self
            .domain
            .native
            .get("npc")
            .and_then(|npc| npc.get("loaded"))
            .is_some_and(|pose| !pose.is_null());
        if !alive {
            return Err(
                "Resident is dead or unloaded; no physical effect can be claimed".to_owned(),
            );
        }
        match operation {
            "move_workplace" => {
                if self
                    .building_name()
                    .and_then(|name| self.record.buildings.get(name))
                    .and_then(|building| building.entrance_positions.first())
                    .is_none()
                {
                    return Err(
                        "Select a registered workplace with a known native entrance".to_owned()
                    );
                }
            }
            "stop_move" => {
                if !control.get("order").is_some_and(|order| !order.is_null()) {
                    return Err("No active native movement/order to release".to_owned());
                }
            }
            "production" => {
                if control.get("order").is_some_and(|order| !order.is_null()) {
                    return Err("Native movement/order remains active; observe arrival, then stop resident movement before production".to_owned());
                }
                if control
                    .get("pending_work")
                    .is_some_and(|value| !value.is_null())
                    || control
                        .get("work")
                        .and_then(|work| work.get("state"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|state| {
                            matches!(
                                state.to_ascii_lowercase().as_str(),
                                "accepted" | "running" | "paused"
                            )
                        })
                {
                    return Err("Original admitted work is active; stop it explicitly before a different job".to_owned());
                }
                let building = self
                    .domain
                    .native
                    .get("building")
                    .ok_or_else(|| "Select a registered building".to_owned())?;
                if building.get("valid").and_then(serde_json::Value::as_bool) != Some(true) {
                    return Err("Building capability is physically blocked; inspect native points and repair".to_owned());
                }
                let recipe = self
                    .selected_option()
                    .ok_or_else(|| "Choose an authored recipe".to_owned())?;
                self.domain
                    .technology
                    .scopes
                    .get(&self.settlement)
                    .cloned()
                    .unwrap_or_default()
                    .can_craft(&recipe, &crate::technology::initial_definitions(), true)
                    .map_err(|reason| format!("Local technology: {reason:?}"))?;
            }
            "technology_practice" => {
                let operation = self
                    .selected_record()
                    .ok_or_else(|| "Select an accepted production record".to_owned())?;
                if self
                    .domain
                    .production
                    .exact_practice(&self.settlement, &operation)
                    .is_none()
                {
                    return Err(
                        "Practice requires durable canonical gross native output receipts"
                            .to_owned(),
                    );
                }
            }
            "technology_transfer" | "caravan" => {
                if self.destination().is_none() {
                    return Err("No authorized known destination".to_owned());
                }
            }
            "trade_accept" => {
                let id = self
                    .selected_record()
                    .ok_or_else(|| "Select an actual quote".to_owned())?;
                let trade = &self.domain.trade.ledger.trades[&id];
                if trade.outcome != crate::commerce::Outcome::Pending {
                    return Err("Quote is already resolved".to_owned());
                }
            }
            "employment_stop" | "caravan_park" | "caravan_resume" | "caravan_remote" => {
                if self.selected_record().is_none() {
                    return Err("Select an existing contract".to_owned());
                }
            }
            "squad_march" | "squad_retreat" | "squad_halt" => {
                if self.domain.squad.is_empty() {
                    return Err("Select serving squad members first".to_owned());
                }
                for alias in &self.domain.squad {
                    let resident = self
                        .record
                        .residents
                        .get(alias)
                        .ok_or_else(|| "Squad member no longer belongs here".to_owned())?;
                    if resident.role.is_none() || self.controls(&resident.handle).is_none() {
                        return Err("Each selected member requires serving role and fresh native order revision".to_owned());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn domain_model(&mut self) -> ViewModel {
        let pane = self.domain.pane;
        let reports = [
            row("Domain", DOMAINS[pane], &self.settlement),
            row(
                "Source",
                "Owner-scoped native reads and durable plugin receipts",
                "Only confirmed member data; no foreign inventory",
            ),
            row(
                "observed_at",
                self.domain.observed_tick.to_string(),
                "Server tick when this bounded read began",
            ),
            row(
                "delivered_at",
                self.domain
                    .delivered_tick
                    .map_or_else(|| "pending".to_owned(), |tick| tick.to_string()),
                "Pending facts are not delivered reports",
            ),
        ];
        let mut rows = vec![
            row(
                "Rights",
                if can_build_settlement(&self.record, &self.player) {
                    "owner/steward"
                } else {
                    "member — read only"
                },
                format!("registry revision {}", self.record.revision),
            ),
            row(
                "Resident",
                self.resident_alias().unwrap_or("none"),
                self.handle().unwrap_or("No known resident"),
            ),
        ];
        if let Some(option) = self.selected_option() {
            rows.push(row(
                "Selected option",
                option,
                "Actual authored catalog or observed warehouse stock",
            ));
        }
        if let Some(name) = self.building_name() {
            rows.push(row(
                "Selected building",
                name,
                "Capability is distinct from construction progress",
            ));
            if let Some(building) = self.record.buildings.get(name).filter(|building| {
                matches!(&building.origin, crate::BuildingOrigin::Structure { .. })
            }) {
                if let Some(policy) = authored_building_policy(&building.purpose) {
                    rows.push(row("Authored policy", &building.purpose,
                        "Plugin semantic functions/capacities/restoration plans are not live native capability or inventory"));
                    fact_rows(&mut rows, "Plugin building policy", policy);
                }
            }
        }
        if let Some((scope, building)) = self.destination() {
            rows.push(row("Known destination", scope, building));
        }
        if let Some(id) = self.selected_record() {
            rows.push(row(
                "Selected record",
                id,
                "Durable native result, not admission-as-effect",
            ));
        }
        match pane {
            1 => {
                if let Some(plan) = self
                    .selected_option()
                    .and_then(|recipe| crate::production::recipe_plan(&recipe))
                {
                    let quantity = match self.form_value("quantity") {
                        ViewFieldValue::Number(value) => value.to_string(),
                        _ => "Integral quantity required".to_owned(),
                    };
                    rows.push(row("Quantified request", format!("{quantity} recipe portions"), "Each variant below is an exact native per-portion plan; manufacturing tier is saved at admission, never selected here"));
                    fact_rows(&mut rows, "Authored recipe plan", &plan);
                }
                for id in self.records() {
                    let job = &self.domain.production.jobs[&id];
                    rows.push(row(
                        "Production order",
                        &id,
                        format!(
                            "{} x{} due {}",
                            job.intent.recipe_id, job.intent.count, job.intent.due_tick
                        ),
                    ));
                    match &job.outcome {
                        None => {
                            if let Some(pause) = &job.pause {
                                rows.push(row("Outcome", "paused", &pause.reason));
                                rows.push(row("Retry tick", pause.retry_tick.to_string(), "Original intent remains; no native work or output receipt invented"));
                            } else {
                                rows.push(row("Outcome", "pending", "No completed output claimed"));
                            }
                        }
                        Some(crate::production::Outcome::Refused { reason }) => {
                            rows.push(row("Outcome", "refused", reason))
                        }
                        Some(crate::production::Outcome::Native(receipt)) => {
                            rows.push(row(
                                "Native outcome",
                                &receipt.state,
                                format!(
                                    "{}/{} rev {} ACK {} {}",
                                    receipt.work_units_done,
                                    receipt.work_units_planned,
                                    receipt.native_revision,
                                    receipt.acknowledged_sequence,
                                    receipt.reason.as_deref().unwrap_or("no blocked reason")
                                ),
                            ));
                            if self.selected_record().as_deref() == Some(id.as_str()) {
                                for portion in &receipt.native_portions {
                                    fact_rows(&mut rows, "Native gross receipt", portion);
                                }
                            }
                        }
                    }
                }
            }
            2 => {
                let definitions = crate::technology::initial_definitions();
                let knowledge = self.domain.technology.scopes.get(&self.settlement);
                rows.push(row(
                    "Local knowledge revision",
                    knowledge
                        .map_or(0, |knowledge| knowledge.revision)
                        .to_string(),
                    "Practice is local, never global",
                ));
                for (id, definition) in definitions {
                    let record = knowledge.and_then(|knowledge| knowledge.records.get(&id));
                    rows.push(row(
                        &id,
                        if record.is_some_and(|record| !record.accepted_practice.is_empty()) {
                            "accepted practice"
                        } else {
                            "locked — real practice required"
                        },
                        definition.unlock_recipes.join(", "),
                    ));
                    if self.selected_option().as_deref() == Some(id.as_str()) {
                        if let Some(record) = record {
                            fact_rows(
                                &mut rows,
                                "Knowledge source",
                                &serde_json::json!(record.source),
                            );
                            for practice in record.accepted_practice.values() {
                                fact_rows(
                                    &mut rows,
                                    "Accepted practice",
                                    &serde_json::json!(practice),
                                );
                            }
                        }
                    }
                }
                rows.push(row(
                    "Catalog boundary",
                    "CR1 / AG1 authored",
                    "Other technology definitions remain locked, not inferred",
                ));
            }
            3 => {
                for id in self.records() {
                    let employment = &self.domain.wages.ledger.employment[&id];
                    rows.push(row(
                        &id,
                        if employment.active {
                            "active"
                        } else {
                            "stopped"
                        },
                        format!(
                            "{} emerald / {} ticks; budget {}",
                            employment.wage, employment.period_ticks, employment.budget
                        ),
                    ));
                    rows.push(row(
                        "Earned / paid",
                        format!("{} / {}", employment.earned, employment.paid),
                        format!(
                            "next due {} payable {:?}",
                            employment.next_due,
                            employment.payable()
                        ),
                    ));
                    for payment in self
                        .domain
                        .wages
                        .intents
                        .values()
                        .filter(|payment| payment.contract == id)
                    {
                        rows.push(row(
                            "Pending native payment",
                            &payment.operation,
                            format!("{} emerald; no debit claimed yet", payment.amount),
                        ));
                    }
                }
            }
            4 => {
                for id in self.records() {
                    let trade = &self.domain.trade.ledger.trades[&id];
                    rows.push(row(
                        &id,
                        format!("{:?}", trade.outcome),
                        format!(
                            "{} {} for {} {}",
                            trade.quote.quantity,
                            trade.quote.item,
                            trade.quote.payment_quantity,
                            trade.quote.payment_item
                        ),
                    ));
                    if self.selected_record().as_deref() == Some(id.as_str()) {
                        fact_rows(
                            &mut rows,
                            "Exact quote fences",
                            &serde_json::json!(trade.quote),
                        );
                    }
                }
            }
            5 => {
                if let (Some(header), Some(terms)) =
                    (&self.domain.paid_header, &self.domain.paid_terms)
                {
                    rows.push(row(
                        "Paid shipment",
                        &header.shipment,
                        format!("{:?}", header.state),
                    ));
                    rows.push(row(
                        "Exact negotiated price",
                        format!("{} minecraft:emerald", terms.price),
                        format!("quantity {}; never payment minted by UI", terms.quantity),
                    ));
                    rows.push(row(
                        "Original recipient",
                        &terms.recipient,
                        &terms.recipient_scope,
                    ));
                    rows.push(row(
                        "Payment source",
                        &header.contract.digest,
                        "Verified complete immutable terms; source and read timestamps above",
                    ));
                    if let Some(reason) = &header.blocked {
                        rows.push(row("Payment blocked", &header.shipment, reason));
                    }
                    if let Some(receipt) = &header.payment_receipt {
                        rows.push(row(
                            "Payment receipt citation",
                            &receipt.digest,
                            "Durable native receipt reference, not a timer/proximity reward",
                        ));
                    }
                    if let Some(loss) = &header.loss {
                        rows.push(row(
                            "Shipment loss citation",
                            &loss.digest,
                            "Loss preserves the original cargo/payment obligation",
                        ));
                    }
                }
                for [x, y, z] in self.known_route_cells() {
                    rows.push(row("Known route cell", format!("{x},{y},{z}"), "Observed native pose or registered entrance; route field uses semicolon-separated cells"));
                }
                for id in self.records() {
                    let contract = &self.domain.caravan.contracts[&id];
                    rows.push(row(
                        &id,
                        format!("{:?}", contract.status),
                        contract
                            .blocked
                            .as_deref()
                            .unwrap_or("No native blocked reason"),
                    ));
                    rows.push(row(
                        "Manifest / executor",
                        format!("{} {}", contract.quantity, contract.item),
                        format!(
                            "epoch {} loaded={} parked={}",
                            contract.party.executor_epoch, contract.loaded, contract.party.parked
                        ),
                    ));
                    rows.push(row(
                        "Arrival receipt",
                        contract.arrival_ref.as_deref().unwrap_or("none"),
                        "Prepared route / movement is NOT arrival",
                    ));
                    if self.selected_record().as_deref() == Some(id.as_str()) {
                        fact_rows(
                            &mut rows,
                            "Known physical route",
                            &serde_json::json!(contract.route),
                        );
                        for receipt in &contract.receipts {
                            fact_rows(
                                &mut rows,
                                "Accepted native effect",
                                &serde_json::json!(receipt),
                            );
                        }
                    }
                }
            }
            6 => {
                self.kit_comparison_rows(&mut rows);
                for (alias, resident) in &self.record.residents {
                    rows.push(row(
                        alias,
                        resident.roster_status(),
                        if self.domain.squad.contains(alias) {
                            "selected squad member"
                        } else {
                            "not selected"
                        },
                    ));
                    if let Some(control) = self.controls(&resident.handle) {
                        rows.push(row(
                            "Native order revision",
                            control
                                .get("order_revision")
                                .map_or_else(|| "unavailable".to_owned(), ToString::to_string),
                            alias,
                        ));
                    }
                }
            }
            _ => {}
        }
        for (kind, value) in &self.domain.native {
            if matches!(
                kind.as_str(),
                "npc"
                    | "controls"
                    | "carry"
                    | "equipment"
                    | "warehouse"
                    | "building"
                    | "destination"
                    | "route"
            ) {
                fact_rows(&mut rows, &format!("Native {kind}"), value);
            }
        }
        for (kind, reason) in &self.domain.errors {
            rows.push(row("Unavailable", kind, reason));
        }
        if !self.domain.notice.is_empty() {
            rows.push(row("Result", "native outcome pending", &self.domain.notice));
        }
        if let Some(terms) = &self.domain.terms {
            rows.insert(
                0,
                row(
                    "Confirmation",
                    "Exact quantified terms",
                    "Changes in rights/native revisions/images require review again",
                ),
            );
            let mut terms_rows = Vec::new();
            fact_rows(
                &mut terms_rows,
                "Command terms",
                &serde_json::json!(terms.arguments),
            );
            rows.splice(1..1, terms_rows);
        }
        let map_points = if pane == 5 && self.domain.terms.is_none() {
            self.known_map_points()
        } else {
            Vec::new()
        };
        rows.splice(
            0..0,
            map_points.iter().enumerate().map(|(index, point)| {
                row(
                    &format!("known-map-{}", index % MAP_SLOTS),
                    format!("{}: {},{},{}", point.dimension, point.x, point.y, point.z),
                    &point.source,
                )
            }),
        );
        let data_rows = PAGE_ROWS - reports.len();
        let page_count = rows.len().div_ceil(data_rows).max(1);
        self.page = self.page.min(page_count - 1);
        let page_start = self.page * data_rows;
        let markers = map_points
            .iter()
            .enumerate()
            .skip(page_start)
            .take(data_rows)
            .map(|(index, point)| ViewMarker {
                marker_id: format!("known-map-{}", index % MAP_SLOTS),
                selection_token: None,
                action_id: None,
                formation: None,
                radius: None,
                position: Some(ViewMarkerPosition {
                    dimension: point.dimension.clone(),
                    x: point.x,
                    y: point.y,
                    z: point.z,
                }),
            })
            .collect();
        let mut page_rows = Vec::with_capacity(PAGE_ROWS);
        page_rows.extend(reports);
        page_rows.extend(rows.into_iter().skip(page_start).take(data_rows));
        let rows = page_rows;
        let mut fields = vec![ViewField {
            id: "domain".to_owned(),
            value: ViewFieldValue::Selected(DOMAINS[pane].to_owned()),
        }];
        let ids: &[&str] = match pane {
            1 => &["job", "quantity", "delay"],
            2 => &["job"],
            3 => &["job", "wage", "period", "budget"],
            4 => &["job", "quantity", "payment", "price"],
            5 => &["job", "quantity", "delay", "route", "native_ticks"],
            6 => &["loadout", "x", "y", "z"],
            _ => &[],
        };
        fields.extend(ids.iter().map(|id| ViewField {
            id: (*id).to_owned(),
            value: self.form_value(id),
        }));
        let mut actions = vec![
            action(
                OPEN_DOMAIN,
                "Open domain",
                self.domain.pending.is_empty(),
                "Reading native domain",
            ),
            action(
                REFRESH,
                "Refresh",
                self.domain.pending.is_empty(),
                "Reading native domain",
            ),
            action(
                NEXT,
                "Next records",
                self.page + 1 < page_count,
                "Last page",
            ),
            action(PREVIOUS, "Previous records", self.page > 0, "First page"),
        ];
        if self.domain.terms.is_some() {
            actions.push(action(
                CONFIRM_TERMS,
                "Confirm exact terms",
                self.domain.pending.is_empty(),
                "Rechecking exact terms",
            ));
            actions.push(action(CANCEL_TERMS, "Cancel terms", true, ""));
        } else {
            let mut choices = vec![("resident", "resident")];
            if matches!(pane, 1 | 2 | 4 | 5 | 6) {
                choices.push(("option", "option"));
            }
            if matches!(pane, 1 | 5) {
                choices.push(("building", "building"));
            }
            if matches!(pane, 2 | 5) {
                choices.push(("destination", "destination"));
            }
            if matches!(pane, 1 | 2 | 3 | 4 | 5) {
                choices.push(("record", "record"));
            }
            for (id, label) in choices {
                let count = match id {
                    "resident" => self.record.residents.len(),
                    "option" => self.options().len(),
                    "building" => self.record.buildings.len(),
                    "destination" => self.destinations().len(),
                    _ => self.records().len(),
                };
                if pane != 5 && !(pane == 6 && id == "option") {
                    actions.push(domain_action(
                        &format!("{id}_previous"),
                        &format!("Previous {label}"),
                        count > 1 && self.domain.pending.is_empty(),
                        "No alternative known option",
                    ));
                }
                actions.push(domain_action(
                    &format!("{id}_next"),
                    &format!("Next {label}"),
                    count > 1 && self.domain.pending.is_empty(),
                    "No alternative known option",
                ));
            }
            let operations: &[(&str, &str)] = match pane {
                1 => &[
                    ("production", "Produce"),
                    ("stop_work", "Stop original work"),
                    ("move_workplace", "Move to selected workplace"),
                    ("stop_move", "Stop resident movement"),
                ],
                2 => &[
                    ("technology_practice", "Accept native practice"),
                    ("technology_transfer", "Transfer by teacher"),
                ],
                3 => &[
                    ("employment", "Contract wages"),
                    ("employment_stop", "Stop future wages"),
                ],
                4 => &[
                    ("trade_quote", "Quote physical trade"),
                    ("trade_accept", "Accept exact quote"),
                ],
                5 => &[
                    ("caravan", "Dispatch physical caravan"),
                    ("caravan_park", "Park exact route"),
                    ("caravan_resume", "Resume exact route"),
                    ("caravan_remote", "Remote exact route"),
                ],
                6 => &[
                    ("hire", "Hire selected role"),
                    ("treat", "Treat selected resident"),
                    ("dismiss", "Demobilize selected resident"),
                    ("squad_toggle", "Add/remove squad member"),
                    ("squad_march", "March selected squad"),
                    ("squad_retreat", "Retreat selected squad"),
                    ("squad_halt", "Halt selected squad"),
                ],
                _ => &[],
            };
            for (id, label) in operations {
                let reason = self.operation_reason(id);
                actions.push(domain_action(
                    id,
                    label,
                    reason.is_ok(),
                    reason.as_ref().err().map(String::as_str).unwrap_or(""),
                ));
            }
        }
        ViewModel {
            page: self.page as u32,
            page_count: page_count as u32,
            rows,
            fields,
            actions,
            tabs: Vec::new(),
            resource_entries: Vec::new(),
            markers,
            reason: Some(clipped(if self.domain.pending.is_empty() {
                &self.domain.notice
            } else {
                "Reading fresh native domain facts"
            })),
        }
    }
}

impl Settlements {
    fn view_domain_action(&mut self, action: &LoaderViewAction) -> Vec<Command> {
        let state = self
            .views
            .get_mut(&action.session)
            .expect("matched live view");
        let operation = action
            .action_id
            .strip_prefix("solaris-settlements:")
            .unwrap_or("");
        if operation == "cancel_terms" {
            state.domain.terms = None;
            return self.view_present(action.session);
        }
        if operation == "confirm_terms" {
            if !state.domain.pending.is_empty() || state.reading {
                state.domain.notice =
                    "Confirmation already being checked; wait for fresh facts.".to_owned();
                return self.view_present(action.session);
            }
            let Some(terms) = state.domain.terms.as_mut() else {
                state.domain.notice = "Review exact terms before confirmation.".to_owned();
                return self.view_present(action.session);
            };
            terms.confirming = true;
            let player = state.player.clone();
            let scope = state.settlement.clone();
            let page = state.page;
            return self.overview(action.session, &player, Some(&scope), page, false);
        }
        if state.domain.terms.is_some() || !state.domain.pending.is_empty() {
            state.domain.notice =
                "Finish or cancel current terms before changing selection.".to_owned();
            return self.view_present(action.session);
        }
        if let Some((choice, direction)) = operation
            .rsplit_once('_')
            .filter(|(_, direction)| matches!(*direction, "previous" | "next"))
        {
            let (index, count) = match choice {
                "resident" => (&mut state.domain.resident, state.record.residents.len()),
                "option" => {
                    let count = state.options().len();
                    (&mut state.domain.option, count)
                }
                "building" => (&mut state.domain.building, state.record.buildings.len()),
                "destination" => {
                    let count = state.destinations().len();
                    (&mut state.domain.destination, count)
                }
                "record" => {
                    let count = state.records().len();
                    (&mut state.domain.record, count)
                }
                _ => return Vec::new(),
            };
            if count > 0 {
                *index = (*index + if direction == "next" { 1 } else { count - 1 }) % count;
            }
            if choice == "resident" {
                state.domain.selected_resident = None;
            }
            let player = state.player.clone();
            let scope = state.settlement.clone();
            return self.overview(action.session, &player, Some(&scope), 0, false);
        }
        for field in &action.fields {
            if field.id != "domain" {
                state
                    .domain
                    .form
                    .insert(field.id.clone(), field.value.clone());
            }
        }
        let reason = state.operation_reason(operation);
        if let Err(reason) = reason {
            state.domain.notice = reason;
            return self.view_present(action.session);
        }
        if operation == "squad_toggle" {
            let alias = state
                .resident_alias()
                .expect("known selected resident")
                .to_owned();
            if !state.domain.squad.remove(&alias) {
                if state.domain.squad.len() >= 32 {
                    state.domain.notice = "Native squad is bounded to 32 members.".to_owned();
                    return self.view_present(action.session);
                }
                state.domain.squad.insert(alias);
            }
            let player = state.player.clone();
            let scope = state.settlement.clone();
            return self.overview(action.session, &player, Some(&scope), 0, false);
        }
        let arguments = match state.domain_arguments(operation) {
            Ok(arguments) => arguments,
            Err(reason) => {
                state.domain.notice = reason;
                return self.view_present(action.session);
            }
        };
        state.domain.terms = Some(Terms {
            operation: operation.to_owned(),
            arguments,
            signature: state.signature(),
            confirming: false,
        });
        state.page = 0;
        self.view_present(action.session)
    }
}

impl ViewState {
    fn inventory_image(
        &self,
        key: &str,
    ) -> Result<solaris_plugin_sdk::inventories::OwnedInventorySnapshot, String> {
        self.domain
            .native
            .get(key)
            .ok_or_else(|| format!("Fresh native {key} inventory image unavailable"))
            .and_then(|value| {
                serde::Deserialize::deserialize(value)
                    .map_err(|_| format!("Native {key} inventory image unreadable"))
            })
    }

    fn kit_plan(
        &self,
        role: crate::hire::Role,
        loadout: Option<&str>,
    ) -> Result<
        (
            solaris_plugin_sdk::inventories::OwnedInventorySnapshot,
            Vec<crate::hire::Transfer>,
        ),
        String,
    > {
        let loadout = crate::hire::Loadout::parse(role, loadout).map_err(str::to_owned)?;
        let warehouse = self.inventory_image("warehouse")?;
        let equipment = self.inventory_image("equipment")?;
        let carry = self.inventory_image("carry")?;
        let handle = self
            .handle()
            .ok_or_else(|| "Known resident required".to_owned())?;
        if !matches!(&equipment.endpoint, InventoryEndpoint::NpcEquipment(candidate) if candidate == handle)
            || !matches!(&carry.endpoint, InventoryEndpoint::NpcCarry(candidate) if candidate == handle)
        {
            return Err("Native kit inventories do not belong to the selected resident".to_owned());
        }
        let plan = crate::hire::plan(role, loadout, &warehouse, &equipment, &carry)
            .map_err(str::to_owned)?;
        Ok((warehouse, plan))
    }

    fn kit_comparison_rows(&self, rows: &mut Vec<ViewRow>) {
        let Some(role) = self
            .selected_option()
            .and_then(|name| crate::hire::Role::parse(&name))
        else {
            return;
        };
        let choices: &[Option<&str>] = if role == crate::hire::Role::Infantry {
            &[Some("arming_sword"), Some("longsword")]
        } else {
            &[None]
        };
        for &loadout in choices {
            let label = loadout.unwrap_or(role.name());
            match self.kit_plan(role, loadout) {
                Err(reason) => rows.push(row("Kit blocked", label, reason)),
                Ok((warehouse, transfers)) => {
                    rows.push(row(
                        "Kit comparison",
                        label,
                        "Exact existing hiring plan; signing wage included",
                    ));
                    for transfer in transfers {
                        if let Some(item) = warehouse
                            .slots
                            .iter()
                            .find(|slot| slot.slot == transfer.source_slot)
                            .and_then(|slot| slot.item.as_ref())
                        {
                            let id = item.item_model.as_deref().unwrap_or(&item.resource_id);
                            let destination = match transfer.destination {
                                crate::hire::Destination::Equipment(slot) => {
                                    format!("equipment slot {slot}")
                                }
                                crate::hire::Destination::Carry(slot) => {
                                    format!("personal carry slot {slot}")
                                }
                            };
                            rows.push(row(id, transfer.count.to_string(), destination));
                        }
                    }
                }
            }
        }
    }
}

impl ViewState {
    fn domain_arguments(&self, operation: &str) -> Result<Vec<String>, String> {
        let text = |id: &str| match self.form_value(id) {
            ViewFieldValue::Text(text) if !text.is_empty() && text.len() <= 256 => Ok(text),
            _ => Err(format!("Enter explicit {id}")),
        };
        let number = |id: &str, min: u64, max: u64| match self.form_value(id) {
            ViewFieldValue::Number(value)
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= min as f64
                    && value <= max as f64 =>
            {
                Ok((value as u64).to_string())
            }
            _ => Err(format!("{id} must be an explicit integer {min}..{max}")),
        };
        let alias = self
            .resident_alias()
            .ok_or_else(|| "Known resident required".to_owned())?
            .to_owned();
        let building = || {
            self.building_name()
                .map(str::to_owned)
                .ok_or_else(|| "Known building required".to_owned())
        };
        let option = || {
            self.selected_option()
                .ok_or_else(|| "Known authored/observed option required".to_owned())
        };
        let record = || {
            self.selected_record()
                .ok_or_else(|| "Existing durable record required".to_owned())
        };
        let destination = || {
            self.destination()
                .ok_or_else(|| "Known authorized destination required".to_owned())
        };
        let scope = self.settlement.clone();
        let arguments = match operation {
            "production" => vec![
                "production".to_owned(),
                scope,
                text("job")?,
                alias,
                building()?,
                option()?,
                number("quantity", 1, 16)?,
                number("delay", 1, crate::MAX_SAFE_REVISION)?,
            ],
            "stop_work" => vec!["stop-work".to_owned(), scope, alias],
            "stop_move" => vec!["stop-resident".to_owned(), scope, alias],
            "move_workplace" => {
                let at = self
                    .building_name()
                    .and_then(|name| self.record.buildings.get(name))
                    .and_then(|building| building.entrance_positions.first())
                    .ok_or_else(|| {
                        "A known registered workplace entrance is required".to_owned()
                    })?;
                vec![
                    "move-resident".to_owned(),
                    scope,
                    alias,
                    at.x.to_string(),
                    at.y.to_string(),
                    at.z.to_string(),
                ]
            }
            "technology_practice" => vec![
                "technology-practice".to_owned(),
                scope,
                option()?,
                record()?,
            ],
            "technology_transfer" => vec![
                "technology-transfer".to_owned(),
                scope,
                destination()?.0,
                option()?,
                alias,
                text("job")?,
            ],
            "employment" => vec![
                "employment".to_owned(),
                scope,
                text("job")?,
                alias,
                number("wage", 1, u64::from(u32::MAX))?,
                number("period", 1, crate::MAX_SAFE_REVISION)?,
                number("budget", 1, crate::MAX_SAFE_REVISION)?,
            ],
            "employment_stop" => vec!["employment-stop".to_owned(), record()?],
            "trade_quote" => vec![
                "trade-quote".to_owned(),
                scope,
                text("job")?,
                alias,
                option()?,
                number("quantity", 1, 4096)?,
                text("payment")?,
                number("price", 1, u64::from(u32::MAX))?,
            ],
            "trade_accept" => vec!["trade-accept".to_owned(), record()?],
            "caravan" => {
                let (destination_scope, destination_building) = destination()?;
                let route = text("route")?;
                let known = self.known_route_cells();
                for point in route.split(';') {
                    let cell: Option<Vec<i32>> = point
                        .split(',')
                        .map(|coordinate| coordinate.parse::<i32>().ok())
                        .collect();
                    if !cell.as_ref().is_some_and(|cell| {
                        cell.len() == 3 && known.contains(&[cell[0], cell[1], cell[2]])
                    }) {
                        return Err("Route must select observed carrier pose or registered known entrance cells shown in this view".to_owned());
                    }
                }
                vec![
                    "caravan".to_owned(),
                    text("job")?,
                    scope,
                    building()?,
                    destination_scope,
                    destination_building,
                    alias,
                    option()?,
                    number("quantity", 1, 4096)?,
                    number("delay", 1, crate::MAX_SAFE_REVISION)?,
                    number("native_ticks", 1, 72000)?,
                    route,
                ]
            }
            "caravan_park" | "caravan_resume" | "caravan_remote" => {
                let id = record()?;
                let contract = &self.domain.caravan.contracts[&id];
                let native_epoch = self
                    .domain
                    .native
                    .get("route")
                    .and_then(|route| route.get("executor_epoch"))
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        "Fresh native route epoch required; refresh selected contract".to_owned()
                    })?;
                if native_epoch != contract.party.executor_epoch || contract.route_ref.is_none() {
                    return Err("Route executor changed; refresh".to_owned());
                }
                vec![operation.replace('_', "-"), id, native_epoch.to_string()]
            }
            "hire" => {
                let mut arguments = vec!["hire".to_owned(), scope, alias, option()?];
                let ViewFieldValue::Text(loadout) = self.form_value("loadout") else {
                    return Err("Infantry loadout must be text".to_owned());
                };
                if !loadout.is_empty() {
                    arguments.push(loadout);
                }
                arguments
            }
            "treat" | "dismiss" => vec![operation.to_owned(), scope, alias],
            "squad_march" | "squad_retreat" | "squad_halt" => {
                let roster = self
                    .domain
                    .squad
                    .iter()
                    .map(|alias| {
                        let resident = &self.record.residents[alias];
                        let revision = self
                            .controls(&resident.handle)
                            .and_then(|control| control.get("order_revision"))
                            .and_then(serde_json::Value::as_u64)
                            .ok_or_else(|| {
                                "Fresh order revision required for every selected member".to_owned()
                            })?;
                        Ok(format!("{alias}:{revision}"))
                    })
                    .collect::<Result<Vec<_>, String>>()?
                    .join(",");
                let mut arguments = vec![
                    operation
                        .strip_prefix("squad_")
                        .expect("matched squad command")
                        .to_owned(),
                    scope,
                    roster,
                ];
                if operation != "squad_halt" {
                    for id in ["x", "y", "z"] {
                        let ViewFieldValue::Number(value) = self.form_value(id) else {
                            return Err("Integral squad coordinates required".to_owned());
                        };
                        if !value.is_finite()
                            || value.fract() != 0.0
                            || value < f64::from(i32::MIN)
                            || value > f64::from(i32::MAX)
                        {
                            return Err("Squad target outside signed native positions".to_owned());
                        }
                        arguments.push((value as i32).to_string());
                    }
                }
                arguments
            }
            _ => return Err("Action is not a declared domain operation".to_owned()),
        };
        Ok(arguments)
    }

    fn known_route_cells(&self) -> std::collections::BTreeSet<[i32; 3]> {
        let mut known = std::collections::BTreeSet::new();
        for (_, record) in &self.domain.known {
            for building in record.buildings.values() {
                for at in &building.entrance_positions {
                    known.insert([at.x, at.y, at.z]);
                }
            }
        }
        if let Some(pose) = self
            .domain
            .native
            .get("npc")
            .and_then(|npc| npc.get("loaded"))
        {
            let values: Option<Vec<i32>> = ["x", "y", "z"]
                .into_iter()
                .map(|axis| {
                    pose.get(axis)?
                        .as_f64()
                        .filter(|value| {
                            value.is_finite()
                                && *value >= f64::from(i32::MIN)
                                && *value <= f64::from(i32::MAX)
                        })
                        .map(|value| value.floor() as i32)
                })
                .collect();
            if let Some(values) = values {
                known.insert([values[0], values[1], values[2]]);
            }
        }
        known
    }
}

struct KnownMapPoint {
    dimension: String,
    x: f64,
    y: f64,
    z: f64,
    source: String,
}

impl ViewState {
    fn known_map_points(&self) -> Vec<KnownMapPoint> {
        if self.domain.delivered_tick.is_none() {
            return Vec::new();
        }
        let mut entrances = std::collections::BTreeMap::new();
        for (scope, record) in &self.domain.known {
            if !is_settlement_member(record, &self.player) {
                continue;
            }
            for (name, building) in &record.buildings {
                let Some(dimension) = building.dimension.as_deref() else {
                    continue;
                };
                for at in &building.entrance_positions {
                    entrances
                        .entry((dimension, at.x, at.y, at.z))
                        .or_insert((scope, name));
                }
            }
        }
        let mut points: Vec<_> = entrances
            .into_iter()
            .map(|((dimension, x, y, z), (scope, name))| KnownMapPoint {
                dimension: dimension.to_owned(),
                x: f64::from(x),
                y: f64::from(y),
                z: f64::from(z),
                source: format!("confirmed registry {scope}/{name}"),
            })
            .collect();
        if let Some(pose) = self
            .domain
            .native
            .get("npc")
            .and_then(|npc| npc.get("loaded"))
        {
            if let (Some(dimension), Some(x), Some(y), Some(z)) = (
                pose.get("dimension").and_then(serde_json::Value::as_str),
                pose.get("x").and_then(serde_json::Value::as_f64),
                pose.get("y").and_then(serde_json::Value::as_f64),
                pose.get("z").and_then(serde_json::Value::as_f64),
            ) {
                if !points.iter().any(|point| {
                    point.dimension == dimension && point.x == x && point.y == y && point.z == z
                }) {
                    points.push(KnownMapPoint {
                        dimension: dimension.to_owned(),
                        x,
                        y,
                        z,
                        source: format!(
                            "native observed resident {}",
                            self.resident_alias().unwrap_or("unavailable")
                        ),
                    });
                }
            }
        }
        points
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;

impl PublicContractView {
    fn selected_contract(&self) -> Option<&serde_json::Value> {
        if let Some(value) = &self.status {
            if value
                .pointer("/contract/offer/id")
                .and_then(serde_json::Value::as_str)
                == Some(self.selected.as_str())
            {
                return Some(value);
            }
        }
        self.contracts.as_ref()?.iter().find(|value| {
            value
                .pointer("/contract/offer/id")
                .and_then(serde_json::Value::as_str)
                == Some(self.selected.as_str())
        })
    }

    fn idle(&self) -> bool {
        self.identity.is_none() && self.pending.is_none()
    }

    fn model(&mut self) -> ViewModel {
        let mut rows = vec![
            row(
                "Source",
                "Live player-contract provider",
                "No settlement membership or private registry",
            ),
            row("Actor", &self.player.player, "Current native session"),
            row(
                "Observed tick",
                self.observed.to_string(),
                "Fresh request began",
            ),
            row(
                "Delivered tick",
                self.delivered
                    .map_or_else(|| "pending".to_owned(), |tick| tick.to_string()),
                "Filtered provider response",
            ),
        ];
        let mut facts = Vec::new();
        if let Some((operation, terms, _)) = &self.terms {
            facts.push(row(
                "Confirm",
                operation,
                "Recheck current need and exact terms before one intent",
            ));
            public_contract_rows(&mut facts, terms);
            fact_rows(&mut facts, "Exact contract", terms);
        } else if let Some(receipt) = &self.receipt {
            fact_rows(&mut facts, "Own immutable receipt", receipt);
        } else if let Some(contract) = self.selected_contract() {
            public_contract_rows(&mut facts, contract);
            fact_rows(&mut facts, "Selected contract", contract);
        } else if let Some(contracts) = &self.contracts {
            for contract in contracts {
                facts.push(row(
                    "Public contract",
                    contract
                        .pointer("/contract/offer/id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unavailable"),
                    contract
                        .pointer("/contract/state")
                        .map_or_else(|| "unavailable".to_owned(), ToString::to_string),
                ));
            }
            if contracts.is_empty() {
                facts.push(row(
                    "Public offers",
                    "None currently available",
                    "Actual current-need provider result",
                ));
            }
        } else {
            facts.push(row(
                "Public offers",
                "Not yet observed",
                "Waiting for authoritative provider response",
            ));
        }
        if !self.notice.is_empty() {
            facts.insert(
                0,
                row(
                    "Result",
                    &self.notice,
                    "Provider decision; no inferred payout",
                ),
            );
        }
        let count = facts.len().div_ceil(PAGE_ROWS - rows.len()).max(1);
        self.page = self.page.min(count - 1);
        rows.extend(facts.into_iter().skip(self.page * 4).take(4));
        let idle = self.idle();
        let selected = self.selected_contract();
        let offered = selected.is_some_and(public_offered);
        let claimed = selected.is_some_and(|value| public_active_claim(value, &self.player.player));
        let same_session = selected.is_some_and(|value| {
            value
                .pointer("/contract/own_claim/session")
                .and_then(serde_json::Value::as_u64)
                == Some(self.player.session)
        });
        let mut actions = vec![
            domain_action(
                "option_next",
                "Browse all current offers",
                idle,
                "Reading live provider",
            ),
            action(
                REFRESH,
                "Refresh current offers / selected ID",
                idle,
                "Reading live provider",
            ),
            action(NEXT, "Next page", self.page + 1 < count, "Last page"),
            action(PREVIOUS, "Previous page", self.page > 0, "First page"),
            domain_action(
                "record_next",
                "Next contract",
                idle && self
                    .contracts
                    .as_ref()
                    .is_some_and(|values| values.len() > 1),
                "No alternative observed contract",
            ),
            domain_action(
                "record_previous",
                "Previous contract",
                idle && self
                    .contracts
                    .as_ref()
                    .is_some_and(|values| values.len() > 1),
                "No alternative observed contract",
            ),
            domain_action(
                "trade_quote",
                "Accept fixed-reward contract",
                idle && offered && self.terms.is_none(),
                "Select a current offered contract",
            ),
            domain_action(
                "trade_accept",
                "Fulfill own contract",
                idle && claimed && same_session && self.terms.is_none(),
                "An own claim is required",
            ),
            domain_action(
                "caravan_park",
                "Cancel own claim",
                idle && claimed && self.terms.is_none(),
                "An own claim is required",
            ),
            domain_action(
                "caravan_resume",
                "Read own immutable receipt by ID",
                idle && !self.selected.is_empty(),
                "Enter a contract ID",
            ),
        ];
        if self.terms.is_some() {
            actions.push(action(
                CONFIRM_TERMS,
                "Confirm exact terms",
                idle,
                "Rechecking authoritative terms",
            ));
            actions.push(action(CANCEL_TERMS, "Cancel terms", true, ""));
        }
        ViewModel {
            page: self.page as u32,
            page_count: count as u32,
            rows,
            fields: vec![ViewField {
                id: "job".to_owned(),
                value: ViewFieldValue::Text(self.selected.clone()),
            }],
            actions,
            tabs: Vec::new(),
            resource_entries: Vec::new(),
            markers: Vec::new(),
            reason: Some(clipped(if idle {
                &self.notice
            } else {
                "Reading fresh native actor and public provider data"
            })),
        }
    }
}

impl Settlements {
    fn view_public_open(
        &mut self,
        player: &solaris_plugin_sdk::events::PlayerSnapshot,
    ) -> Vec<Command> {
        let session = player.session;
        if self.views.len() + self.public_contract_views.len() >= 16
            && !self.views.contains_key(&session)
            && !self.public_contract_views.contains_key(&session)
        {
            return vec![message_session(session, "Settlement view is busy.")];
        }
        let Some(request) = self.next_storage_request("public-contract-view") else {
            return Vec::new();
        };
        let mut commands = self.view_reopen(session);
        let mut state = PublicContractView {
            player: player.clone(),
            request: request.clone(),
            instance: None,
            identity: None,
            pending: None,
            contracts: None,
            status: None,
            selected: String::new(),
            receipt: None,
            terms: None,
            page: 0,
            sequence: None,
            notice: String::new(),
            observed: self.view_tick,
            delivered: None,
        };
        commands.push(open_client_view(
            &request,
            session,
            PUBLIC_VIEW_ID,
            state.model(),
        ));
        self.public_contract_views.insert(session, state);
        commands.extend(self.view_public_query(session, vec!["player-contracts".to_owned()]));
        commands
    }

    fn view_public_present(&mut self, session: u64) -> Vec<Command> {
        let Some(state) = self.public_contract_views.get_mut(&session) else {
            return Vec::new();
        };
        let Some((id, revision)) = state.instance.clone() else {
            return Vec::new();
        };
        let Some(next) = revision.checked_add(1) else {
            return vec![close_client_view(session, &id)];
        };
        state.instance = Some((id.clone(), next));
        vec![present_client_view(session, &id, revision, state.model())]
    }

    fn view_public_query(&mut self, session: u64, arguments: Vec<String>) -> Vec<Command> {
        if !self
            .public_contract_views
            .get(&session)
            .is_some_and(PublicContractView::idle)
        {
            return Vec::new();
        }
        let Some(request) = self.next_storage_request("public-contract-actor") else {
            return Vec::new();
        };
        let state = self
            .public_contract_views
            .get_mut(&session)
            .expect("live public view");
        state.observed = self.view_tick;
        state.delivered = None;
        state.identity = Some((request.clone(), arguments));
        let mut commands = vec![list_online_players(&request, 256)];
        commands.extend(self.view_public_present(session));
        commands
    }

    fn view_public_invoke(&mut self, session: u64, arguments: Vec<String>) -> Vec<Command> {
        let Some(state) = self.public_contract_views.get_mut(&session) else {
            return Vec::new();
        };
        state.pending = arguments.first().cloned();
        let invoked = solaris_plugin_sdk::events::CommandInvoked {
            session,
            player: state.player.player.clone(),
            username: state.player.name.clone(),
            operator: false,
            position: state.player.position,
            name: "settlement".to_owned(),
            raw_arguments: arguments.join(" "),
            arguments,
        };
        let Some(commands) = self.handle_player_contract_command(&invoked, self.view_tick) else {
            self.public_contract_views
                .get_mut(&session)
                .expect("live public view")
                .pending = None;
            return Vec::new();
        };
        self.view_player_contract_outputs(commands)
    }

    fn view_public_players(&mut self, answer: &OnlinePlayersAnswered) -> Option<Vec<Command>> {
        let session = self
            .public_contract_views
            .iter()
            .find_map(|(session, state)| {
                state
                    .identity
                    .as_ref()
                    .is_some_and(|(request, _)| request == &answer.request)
                    .then_some(*session)
            })?;
        let state = self
            .public_contract_views
            .get_mut(&session)
            .expect("correlated public view");
        let (_, arguments) = state.identity.take().expect("correlated native identity");
        let Some(player) = answer
            .players
            .iter()
            .find(|player| player.session == session && player.player == state.player.player)
        else {
            state.terms = None;
            state.notice = "Current native actor unavailable; no intent submitted.".to_owned();
            return Some(self.view_public_present(session));
        };
        state.player = player.clone();
        Some(self.view_public_invoke(session, arguments))
    }

    // Only player-contract provider callback outputs enter this adapter. Ordinary CLI
    // command replies must bypass it, even while a view has an outstanding request.
    pub(super) fn view_player_contract_outputs(&mut self, commands: Vec<Command>) -> Vec<Command> {
        let session = commands.iter().find_map(|command| match command {
            Command::SendMessage(message) => match message.target {
                solaris_plugin_sdk::MessageTarget::Session(session)
                    if self
                        .public_contract_views
                        .get(&session)
                        .is_some_and(|state| state.pending.is_some()) =>
                {
                    Some(session)
                }
                _ => None,
            },
            _ => None,
        });
        let Some(session) = session else {
            return commands;
        };
        let bytes: usize = commands.iter().filter_map(|command| match command {
            Command::SendMessage(message) if matches!(message.target, solaris_plugin_sdk::MessageTarget::Session(target) if target == session) => Some(message.text.len()),
            _ => None,
        }).sum();
        let mut text = String::with_capacity(bytes);
        let mut retained = Vec::new();
        for command in commands {
            match command {
                Command::SendMessage(message) if matches!(message.target, solaris_plugin_sdk::MessageTarget::Session(target) if target == session) =>
                {
                    text.push_str(&message.text);
                }
                command => retained.push(command),
            }
        }
        let state = self
            .public_contract_views
            .get_mut(&session)
            .expect("correlated provider reply");
        let pending = state.pending.take().expect("correlated provider request");
        state.delivered = Some(self.view_tick);
        let mut parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
        let mut submit = None;
        if pending == "player-contracts" {
            if let Some(values) = parsed
                .as_mut()
                .and_then(|value| value.get_mut("contracts"))
                .and_then(serde_json::Value::as_array_mut)
            {
                if values.len() <= 13 && values.iter().all(public_contract_projection) {
                    state.receipt = None;
                    state.status = None;
                    state.notice.clear();
                    if state.selected.is_empty() {
                        state.selected = values
                            .first()
                            .and_then(|value| value.pointer("/contract/offer/id"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                    }
                    state.contracts = Some(std::mem::take(values));
                } else {
                    state.notice = "Provider projection unavailable.".to_owned();
                }
            } else {
                state.notice = public_provider_notice(&text);
            }
        } else if pending == "player-contract-status" {
            if let Some(value) = parsed.filter(public_contract_projection) {
                let id = value
                    .pointer("/contract/offer/id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if id == state.selected {
                    if let Some((operation, expected, true)) = state.terms.take() {
                        if value == expected {
                            submit = Some(vec![operation, state.selected.clone()]);
                        } else {
                            state.notice = "Current need, rights or terms changed; review again. No intent submitted.".to_owned();
                        }
                    }
                    state.status = Some(value);
                    state.receipt = None;
                }
            } else {
                state.terms = None;
                state.notice = public_provider_notice(&text);
            }
        } else if pending == "player-contract-receipt" {
            if let Some(value) = parsed.filter(|value| {
                value.get("id").and_then(serde_json::Value::as_str) == Some(state.selected.as_str())
                    && value
                        .pointer("/participant/player")
                        .and_then(serde_json::Value::as_str)
                        == Some(state.player.player.as_str())
                    && value.get("own_inventory").is_some()
            }) {
                state.receipt = Some(value);
                state.page = 0;
                state.notice.clear();
            } else {
                state.receipt = None;
                state.notice = public_provider_notice(&text);
            }
        } else {
            state.notice = public_provider_notice(&text);
            state.terms = None;
            if !state.selected.is_empty() {
                submit = Some(vec![
                    "player-contract-status".to_owned(),
                    state.selected.clone(),
                ]);
            }
        }
        if let Some(arguments) = submit {
            // The plan was consumed before dispatch; a replay cannot submit twice.
            retained.extend(self.view_public_invoke(session, arguments));
        }
        retained.extend(self.view_public_present(session));
        retained
    }

    fn view_public_action(&mut self, action: &LoaderViewAction) -> Vec<Command> {
        let state = self
            .public_contract_views
            .get_mut(&action.session)
            .expect("public view routed");
        if !state.instance.as_ref().is_some_and(|(id, revision)| {
            id == &action.view_instance_id && *revision == action.view_revision
        }) || state
            .sequence
            .is_some_and(|sequence| action.action_sequence <= sequence)
        {
            return Vec::new();
        }
        state.sequence = Some(action.action_sequence);
        let id = action.action_id.as_str();
        if id == CANCEL_TERMS {
            state.terms = None;
            return self.view_public_present(action.session);
        }
        if !state.idle() {
            return Vec::new();
        }
        if let Some(value) =
            action
                .fields
                .iter()
                .find_map(|field| match (&*field.id, &field.value) {
                    ("job", ViewFieldValue::Text(value)) => Some(value),
                    _ => None,
                })
        {
            if value != &state.selected && !crate::commerce_player::valid_id(value) {
                state.terms = None;
                state.notice =
                    "Choose a valid public or archived contract ID; no intent submitted."
                        .to_owned();
                return self.view_public_present(action.session);
            }
            if value != &state.selected && crate::commerce_player::valid_id(value) {
                state.status = None;
                state.selected = value.clone();
                state.receipt = None;
                state.terms = None;
                state.page = 0;
                return self.view_public_query(
                    action.session,
                    vec!["player-contract-status".to_owned(), value.clone()],
                );
            }
        }
        match id {
            "solaris-settlements:option_next" => {
                state.selected.clear();
                state.status = None;
                state.terms = None;
                state.receipt = None;
                state.page = 0;
                return self.view_public_query(action.session, vec!["player-contracts".to_owned()]);
            }
            NEXT => state.page = state.page.saturating_add(1),
            PREVIOUS => state.page = state.page.saturating_sub(1),
            REFRESH => {
                state.terms = None;
                let arguments = if state.selected.is_empty() {
                    vec!["player-contracts".to_owned()]
                } else {
                    vec!["player-contract-status".to_owned(), state.selected.clone()]
                };
                return self.view_public_query(action.session, arguments);
            }
            "solaris-settlements:record_next" | "solaris-settlements:record_previous" => {
                if let Some(values) = state.contracts.as_ref().filter(|values| !values.is_empty()) {
                    let old = values
                        .iter()
                        .position(|value| {
                            value
                                .pointer("/contract/offer/id")
                                .and_then(serde_json::Value::as_str)
                                == Some(state.selected.as_str())
                        })
                        .unwrap_or(0);
                    let index = if id.ends_with("_next") {
                        (old + 1) % values.len()
                    } else {
                        (old + values.len() - 1) % values.len()
                    };
                    state.selected = values[index]
                        .pointer("/contract/offer/id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    state.receipt = None;
                    state.terms = None;
                    state.page = 0;
                    state.status = None;
                    let arguments =
                        vec!["player-contract-status".to_owned(), state.selected.clone()];
                    return self.view_public_query(action.session, arguments);
                }
            }
            "solaris-settlements:caravan_resume" => {
                let arguments = vec!["player-contract-receipt".to_owned(), state.selected.clone()];
                return self.view_public_query(action.session, arguments);
            }
            CONFIRM_TERMS => {
                if let Some((_, _, confirming)) = &mut state.terms {
                    *confirming = true;
                    let arguments =
                        vec!["player-contract-status".to_owned(), state.selected.clone()];
                    return self.view_public_query(action.session, arguments);
                }
            }
            "solaris-settlements:trade_quote"
            | "solaris-settlements:trade_accept"
            | "solaris-settlements:caravan_park" => {
                if let Some(value) = state.selected_contract() {
                    let offered = public_offered(value);
                    let own = public_active_claim(value, &state.player.player);
                    let same_session = value
                        .pointer("/contract/own_claim/session")
                        .and_then(serde_json::Value::as_u64)
                        == Some(state.player.session);
                    let operation = match id {
                        "solaris-settlements:trade_quote" if offered => {
                            Some("player-contract-accept")
                        }
                        "solaris-settlements:trade_accept" if own && same_session => {
                            Some("player-contract-fulfill")
                        }
                        "solaris-settlements:caravan_park" if own => Some("player-contract-cancel"),
                        _ => None,
                    };
                    if let Some(operation) = operation {
                        state.terms = Some((operation.to_owned(), value.clone(), false));
                        state.page = 0;
                    }
                }
            }
            _ => return Vec::new(),
        }
        self.view_public_present(action.session)
    }
}

fn public_contract_projection(value: &serde_json::Value) -> bool {
    value
        .pointer("/contract/offer/id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(crate::commerce_player::valid_id)
        && value
            .pointer("/contract/state")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && value.get("remaining_supply").is_some()
        && value
            .get("contract")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|contract| {
                contract.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "offer"
                            | "state"
                            | "own_claim"
                            | "own_receipt"
                            | "own_checkpoint"
                            | "blocked"
                    )
                })
            })
}

fn public_provider_notice(text: &str) -> String {
    if text.trim_start().starts_with(['{', '[']) {
        "Provider result received; refresh authoritative status or request own receipt.".to_owned()
    } else {
        clipped(text)
    }
}

fn public_own_claim(value: &serde_json::Value, player: &str) -> bool {
    value
        .pointer("/contract/own_claim/player")
        .and_then(serde_json::Value::as_str)
        == Some(player)
}

fn public_offered(value: &serde_json::Value) -> bool {
    value
        .pointer("/contract/state")
        .and_then(serde_json::Value::as_str)
        == Some("offered")
        && value
            .get("remaining_supply")
            .and_then(serde_json::Value::as_u64)
            != Some(0)
}

fn public_active_claim(value: &serde_json::Value, player: &str) -> bool {
    public_own_claim(value, player)
        && value
            .pointer("/contract/state")
            .and_then(serde_json::Value::as_str)
            == Some("claimed")
}

fn public_contract_rows(rows: &mut Vec<ViewRow>, value: &serde_json::Value) {
    let text = |path| {
        value
            .pointer(path)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable")
    };
    rows.push(row(
        "Contract",
        text("/contract/offer/id"),
        text("/contract/state"),
    ));
    rows.push(row(
        "Fixed reward",
        value
            .pointer("/contract/offer/fixed_emerald_reward")
            .map_or_else(|| "unavailable".to_owned(), ToString::to_string),
        "Physical emerald; original allocated currency only",
    ));
    rows.push(row(
        "Current need",
        value
            .get("remaining_supply")
            .filter(|need| !need.is_null())
            .map_or_else(
                || {
                    if text("/contract/offer/need/kind") == "escort" {
                        "Original shipment escort".to_owned()
                    } else {
                        "Not observed".to_owned()
                    }
                },
                ToString::to_string,
            ),
        if text("/contract/offer/need/kind") == "supply" {
            text("/contract/offer/need/resource")
        } else {
            text("/contract/offer/need/shipment")
        },
    ));
    rows.push(row(
        "Stock location",
        text("/contract/offer/location/scope"),
        text("/contract/offer/location/building"),
    ));
}
