//! Bind authored or generator-authenticated village containers, hold a resident
//! worker, and give that worker real haul and craft orders into the bound store.

mod capture_runtime;
#[cfg(test)]
mod capture_tests;
mod combat;
#[cfg(test)]
mod combat_tests;
mod demob_runtime;
#[cfg(test)]
mod demob_tests;
/// Households, resident social records, and demographic intents. The records
/// are durable from this slice on; the mutation API is unused until the
/// migration, birth, and relocation slices that call it land.
#[allow(dead_code)]
mod demography;
#[cfg(test)]
mod demography_tests;
mod evacuation;
#[cfg(test)]
mod evacuation_tests;
mod hire;
mod hire_runtime;
#[cfg(test)]
mod hire_tests;

mod view;
#[cfg(test)]
mod view_tests;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[cfg(test)]
use solaris_plugin_sdk::events::OperationCommitted;
use solaris_plugin_sdk::events::{
    CommandInvoked, Event, OperationAnswered, OperationOutcome, OperationPayload,
    StorageCasAnswered, StorageGetAnswered,
};
use solaris_plugin_sdk::inventories::{
    InventoryEndpoint, InventoryFence, InventoryMaterial, InventoryResourcePlan, InventoryResult,
    InventoryWorkPortion,
};
use solaris_plugin_sdk::operation_types::OperationFailure;
use solaris_plugin_sdk::residents::{
    AreaToolWork, CraftWork, HaulWork, ResidentKind, ResidentOrderResult, ResidentResult, WorkArea,
    WorkOrder, WorkPauseReason, WorkState,
};
use solaris_plugin_sdk::settlements::{
    BuildingInteractionPoint, SettlementResult, SettlementSite, SitePoiKind, SitePoiState,
    StructureSnapshot, StructureState, SurveyBounds, SurveyPurpose, WarehouseBinding,
    WarehouseSource,
};
use solaris_plugin_sdk::{
    advance_structure, assign_resident_work, bind_manual_warehouse, bind_village_warehouse,
    bind_warehouse, export_plugin, list_settlement_sites, log, message_session, play_client_sound,
    prepare_structure, query_owned_inventory, query_settlement_site, recognize_building,
    release_resident_site, reserve_inventory_items, reserve_resident_site, residents, settlements,
    spawn_resident, storage_cas, storage_get, structure_status, survey_site, Command, Config,
    EventContext, Failure, InitContext, LogLevel, Plugin, StorageCasOutcome, StorageGetOutcome,
};

const WORK_UNITS: u64 = 4_096;
/// One structure portion mirrors the single-cell canonical settlement advance.
const STRUCTURE_PORTION_WORK_UNITS: u64 = 1;
const OPERATION_COUNTER_KEY: &str = "operation-counter";
const MAX_SAFE_REVISION: u64 = (1 << 53) - 1;
/// The dimension every craft station names. The component's own operations never
/// cross dimensions, and the profile's sites are laid out in the overworld.
const STATION_DIMENSION: &str = "minecraft:overworld";
/// How many sites one homes listing asks for: the contract's own largest page.
const MAX_WORK_AREA_AXIS: i32 = 16;
const SITE_PAGE: u8 = 64;
/// The operator-log prefix every answer reports under, so a driver reads the
/// owner's own outcomes instead of parsing chat.
const MARKER: &str = "SETTLEMENT_OPS";

const SETTLEMENT_REGISTRY_KEY: &str = "settlement-registry-v1";
const MAX_SETTLEMENTS: usize = 24;
const MAX_SETTLEMENT_ROLES: usize = 8;
const MAX_SETTLEMENT_BUILDINGS: usize = 64;
const MAX_SETTLEMENT_RESIDENTS: usize = 64;

/// One free home point of interest the homes listing remembered, with the cell
/// it stands at: the values a spawn names and a driver reads from the marker.
struct WorkerHome {
    poi_id: String,
    at: settlements::BlockPosition,
}

/// The site a worker is recruited from: the identity a reservation fences on and
/// the free homes it still holds.
struct WorkerSite {
    site_id: String,
    origin: settlements::BlockPosition,
    revision: u64,
    homes: Vec<WorkerHome>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct SettlementRegistry {
    settlements: BTreeMap<String, SettlementRecord>,
    #[serde(default)]
    hire_sequence: u64,
    #[serde(default)]
    dismiss_sequence: u64,
    #[serde(default)]
    capture_sequence: u64,
    /// Households by household id, and the sequence that names them. Both are
    /// additive: a save written before this slice loads unchanged.
    #[serde(default)]
    household_sequence: u64,
    #[serde(default)]
    households: BTreeMap<String, demography::Household>,
    /// Durable demographic intents by operation id.
    #[serde(default)]
    demography_intents: BTreeMap<String, demography::Intent>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
enum BuildingOrigin {
    Structure { structure_id: String },
    Certificate { certificate_id: String },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct StoredPosition {
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Clone, Deserialize, Serialize)]
struct SettlementBuilding {
    purpose: String,
    origin: BuildingOrigin,
    #[serde(default)]
    snapshot_revision: u64,
    #[serde(default)]
    storage_positions: Vec<StoredPosition>,
}

#[derive(Clone, Deserialize, Serialize)]
struct SettlementResident {
    handle: String,
    #[serde(default)]
    work_revision: u64,
    #[serde(default)]
    role: Option<hire::Role>,
    #[serde(default)]
    employer: Option<String>,
    #[serde(default)]
    hiring: Option<hire::Intent>,
    #[serde(default)]
    issued: Option<hire::IssuedKit>,
    #[serde(default)]
    dismissing: Option<demob_runtime::Intent>,
    #[serde(default)]
    capturing: Option<capture_runtime::CaptureIntent>,
    #[serde(default)]
    captive_by: Option<String>,
    /// The one household this resident belongs to, and the absolute owner tick
    /// their age counts from. Both additive to an existing save.
    #[serde(default)]
    household: Option<String>,
    #[serde(default)]
    age_origin: Option<u64>,
}
impl SettlementResident {
    fn roster_status(&self) -> String {
        if let Some(captor) = &self.captive_by {
            format!("prisoner of {captor}")
        } else if let Some(intent) = &self.capturing {
            format!("capture pending by {}", intent.captor_settlement)
        } else {
            self.role.map_or("civilian", hire::Role::name).to_owned()
        }
    }
}

struct SpecialtyDefinition {
    blueprint_id: &'static str,
    tags: &'static [&'static str],
}
#[derive(Clone, Deserialize, Serialize)]
struct SettlementRecord {
    owner: String,
    size_class: String,
    #[serde(default)]
    roles: BTreeMap<String, String>,
    revision: u64,
    #[serde(default)]
    site_id: Option<String>,
    #[serde(default)]
    site_revision: Option<u64>,
    #[serde(default)]
    survey_token: Option<String>,
    #[serde(default)]
    survey_biome_tags: Vec<String>,
    #[serde(default)]
    survey_resource_tags: Vec<String>,
    #[serde(default)]
    survey_purpose: Option<String>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    buildings: BTreeMap<String, SettlementBuilding>,
    #[serde(default)]
    building_sequence: u32,
    #[serde(default)]
    specialties: Vec<String>,
    #[serde(default)]
    residents: BTreeMap<String, SettlementResident>,
    #[serde(default)]
    resident_sequence: u32,
}

enum RegistryRead {
    Create {
        session: u64,
        player: String,
        name: String,
        size_class: String,
    },
    List {
        session: u64,
        player: String,
    },
    Info {
        session: u64,
        player: String,
        name: Option<String>,
    },
    Overview {
        session: u64,
        player: String,
        name: Option<String>,
        page: usize,
    },
    Sites {
        session: u64,
        player: String,
        settlement: String,
    },
    Adopt {
        session: u64,
        player: String,
        settlement: String,
        site_id: String,
    },
    Populate {
        session: u64,
        player: String,
        settlement: String,
    },
    Treat {
        session: u64,
        player: String,
        settlement: String,
        alias: String,
    },
    PersistResident {
        session: u64,
        resident: PendingResidentPersistence,
    },
    Survey {
        session: u64,
        player: String,
        settlement: String,
        purpose: SurveyPurpose,
        purpose_name: String,
        bounds: SurveyBounds,
    },
    Project {
        session: u64,
        player: String,
        settlement: String,
        blueprint: String,
        anchor: settlements::BlockPosition,
        rotation: u16,
        preview: Option<view::PreviewOrigin>,
    },
    Recognize {
        session: u64,
        player: String,
        settlement: String,
        building_name: String,
        purpose: String,
        footprint: SurveyBounds,
        points: Vec<String>,
        survey_token: String,
        expected_world_revision: u64,
    },
    Role {
        session: u64,
        player: String,
        settlement: String,
        target: String,
        role: String,
    },
    Condition {
        session: u64,
        player: String,
        settlement: String,
        ruined: bool,
    },
    Fund {
        session: u64,
        player: String,
        settlement: String,
        building: String,
    },
    Advance {
        session: u64,
        player: String,
        settlement: String,
        building: String,
    },
    Specialize {
        session: u64,
        player: String,
        settlement: String,
        specialties: Vec<String>,
    },
}

struct PendingRegistryRead {
    request: String,
    action: RegistryRead,
}

struct PendingResidentPersistence {
    settlement: String,
    resident_name: String,
    handle: String,
    work_revision: u64,
}

struct PendingRegistryWrite {
    request: String,
    session: u64,
    message: String,
    resident: Option<PendingResidentPersistence>,
}

enum RegistrySiteQuery {
    List {
        session: u64,
        settlement: String,
    },
    Adopt {
        session: u64,
        settlement: String,
        site_id: String,
        registry: SettlementRegistry,
        version: Option<u64>,
    },
    Project {
        session: u64,
        settlement: String,
        building_name: String,
        site_id: String,
        survey_token: String,
        blueprint: String,
        anchor: settlements::BlockPosition,
        rotation: u16,
        preview: Option<view::PreviewOrigin>,
        registry: SettlementRegistry,
        version: Option<u64>,
    },
    Populate {
        session: u64,
        site_id: String,
        population: PendingSettlementPopulation,
    },
}

struct PendingRegistrySites {
    request: String,
    query: RegistrySiteQuery,
}

struct PendingRegistrySurvey {
    request: String,
    session: u64,
    settlement: String,
    purpose_name: String,
    registry: SettlementRegistry,
    version: Option<u64>,
}

#[derive(Clone)]
struct PendingSettlementProject {
    settlement: String,
    building_name: String,
    registry: SettlementRegistry,
    version: Option<u64>,
}

#[derive(Clone)]
struct WorkplaceCandidate {
    origin: BuildingOrigin,
    id: String,
}

struct PendingSpecialization {
    session: u64,
    settlement: String,
    specialties: Vec<String>,
    workplace_candidates: Vec<Vec<WorkplaceCandidate>>,
    next_specialty: usize,
    next_candidate: usize,
    registry: SettlementRegistry,
    version: Option<u64>,
}

#[derive(Clone)]
struct PendingSettlementPopulation {
    settlement: String,
    resident_name: String,
    registry: SettlementRegistry,
    version: Option<u64>,
}

enum StatusContinuation {
    Read,
    Fund,
    Advance,
    Specialize(PendingSpecialization),
}

struct PendingStatus {
    session: u64,
    continuation: StatusContinuation,
}

impl RegistryRead {
    fn session(&self) -> u64 {
        match self {
            Self::Create { session, .. }
            | Self::List { session, .. }
            | Self::Info { session, .. }
            | Self::Overview { session, .. }
            | Self::Sites { session, .. }
            | Self::Adopt { session, .. }
            | Self::Populate { session, .. }
            | Self::Treat { session, .. }
            | Self::Survey { session, .. }
            | Self::PersistResident { session, .. }
            | Self::Project { session, .. }
            | Self::Recognize { session, .. }
            | Self::Role { session, .. }
            | Self::Condition { session, .. }
            | Self::Fund { session, .. }
            | Self::Advance { session, .. }
            | Self::Specialize { session, .. } => *session,
        }
    }
}
/// The outstanding homes listing: a query, so it carries no durable operation
/// id and is correlated by its request alone.
struct PendingSites {
    request: String,
    session: u64,
}

/// A site survey is a query, so it is tracked by its request rather than the
/// durable operation counter.
struct PendingSurvey {
    request: String,
    session: u64,
}

/// One owned-inventory query that must yield a fence before the component asks
/// core to hold the exact materials of the remembered structure plan.
struct PendingFunding {
    request: String,
    session: u64,
    structure_id: String,
    endpoint: InventoryEndpoint,
    plan: InventoryResourcePlan,
}

#[derive(Clone, Copy)]
enum IssueDestination {
    Carry,
    Equipment,
}

impl IssueDestination {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "carry" => Some(Self::Carry),
            "equipment" => Some(Self::Equipment),
            _ => None,
        }
    }

    fn endpoint(self, resident: String) -> InventoryEndpoint {
        match self {
            Self::Carry => InventoryEndpoint::ResidentCarry(resident),
            Self::Equipment => InventoryEndpoint::ResidentEquipment(resident),
        }
    }
}

#[derive(Clone)]
enum HaulDirection {
    Deposit,
    Issue(IssueDestination),
}

#[derive(Clone)]
enum OperationIntent {
    Bind {
        session: u64,
        structure_id: String,
        container_id: u32,
    },
    BindVillage {
        session: u64,
        site_id: String,
        container_id: u32,
    },
    BindManual {
        session: u64,
        survey_token: String,
        position: settlements::BlockPosition,
    },
    Recognize {
        session: u64,
        settlement: String,
        building_name: String,
        purpose: String,
        footprint: SurveyBounds,
        points: Vec<BuildingInteractionPoint>,
        survey_token: String,
        expected_world_revision: u64,
        registry: SettlementRegistry,
        version: Option<u64>,
    },
    Haul {
        session: u64,
        resident: String,
        warehouse: String,
        expected_revision: u64,
        direction: HaulDirection,
        item: Option<String>,
    },
    Craft {
        session: u64,
        resident: String,
        expected_revision: u64,
        recipe: String,
        count: u32,
        station: WorkArea,
    },
    Harvest {
        session: u64,
        resident: String,
        expected_revision: u64,
        tool: String,
        area: WorkArea,
    },
    ReserveSite {
        session: u64,
        site_id: String,
        poi_id: String,
        expected_revision: u64,
    },
    Prepare {
        session: u64,
        site_id: String,
        blueprint: String,
        anchor: settlements::BlockPosition,
        survey_token: String,
        rotation: u16,
        expected_site_revision: u64,
        preview: Option<view::PreviewOrigin>,
    },
    ReserveMaterials {
        session: u64,
        structure_id: String,
        endpoint: InventoryEndpoint,
        plan: InventoryResourcePlan,
        fence: InventoryFence,
    },
    Advance {
        session: u64,
        structure_id: String,
        stage: String,
        reservation_ref: String,
        expected_revision: u64,
        work_units: u64,
    },
    SpawnResident {
        session: u64,
        token: String,
    },
    ReleaseSite {
        session: u64,
        token: String,
    },
    Combat {
        session: u64,
        resident: String,
        allies: Vec<String>,
        expected_revision: u64,
        phase: combat::CombatPhase,
    },
    Treatment {
        session: u64,
        handle: String,
        warehouse: String,
        expected_revision: u64,
    },
    Squad {
        session: u64,
        handles: Vec<String>,
        revisions: Vec<u64>,
        action: combat::SquadAction,
    },
}

impl OperationIntent {
    fn session(&self) -> u64 {
        match self {
            Self::Bind { session, .. }
            | Self::BindVillage { session, .. }
            | Self::BindManual { session, .. }
            | Self::Recognize { session, .. }
            | Self::Haul { session, .. }
            | Self::Craft { session, .. }
            | Self::Harvest { session, .. }
            | Self::ReserveSite { session, .. }
            | Self::Prepare { session, .. }
            | Self::ReserveMaterials { session, .. }
            | Self::Advance { session, .. }
            | Self::SpawnResident { session, .. }
            | Self::ReleaseSite { session, .. }
            | Self::Squad { session, .. }
            | Self::Combat { session, .. }
            | Self::Treatment { session, .. } => *session,
        }
    }
    fn is_bind(&self) -> bool {
        matches!(
            self,
            Self::Bind { .. } | Self::BindVillage { .. } | Self::BindManual { .. }
        )
    }
}

#[derive(Clone)]
struct PendingOperation {
    session: u64,
    operation_id: String,
    kind: PendingKind,
}

#[derive(Clone)]
enum PendingKind {
    Bind {
        structure_id: String,
        container_id: u32,
    },
    BindVillage {
        site_id: String,
        container_id: u32,
    },
    BindManual {
        position: settlements::BlockPosition,
    },
    Recognize {
        settlement: String,
        building_name: String,
        purpose: String,
        registry: SettlementRegistry,
        version: Option<u64>,
    },
    Haul {
        resident: String,
    },
    ReserveMaterials {
        structure_id: String,
    },
    Advance,
    Craft {
        resident: String,
    },
    Harvest {
        resident: String,
    },
    ReserveSite {
        population: Option<PendingSettlementPopulation>,
    },
    Prepare {
        project: Option<PendingSettlementProject>,
    },
    SpawnResident {
        token: String,
        population: Option<PendingSettlementPopulation>,
    },
    ReleaseSite,
    Combat {
        resident: String,
        allies: Vec<String>,
        phase: combat::CombatPhase,
    },
    TreatmentQuery {
        handle: String,
        warehouse: String,
    },
    Treatment {
        handle: String,
    },
    Squad {
        handles: Vec<String>,
        action: combat::SquadAction,
    },
}

struct QueuedOperation {
    intent: OperationIntent,
    project: Option<PendingSettlementProject>,
    population: Option<PendingSettlementPopulation>,
}

struct PendingCounter {
    request: String,
    next: u64,
    intent: OperationIntent,
    project: Option<PendingSettlementProject>,
    population: Option<PendingSettlementPopulation>,
}

#[derive(Default)]
struct Settlements {
    warehouse: Option<WarehouseBinding>,
    resident_revisions: BTreeMap<String, u64>,
    worker_site: Option<WorkerSite>,
    pending_sites: Option<PendingSites>,
    pending_funding: Option<PendingFunding>,
    survey_token: Option<String>,
    survey_bounds: Option<SurveyBounds>,
    survey_world_revision: Option<u64>,
    pending_survey: Option<PendingSurvey>,
    pending_status: BTreeMap<String, PendingStatus>,
    last_structure: Option<StructureSnapshot>,
    reservation_ref: Option<String>,
    reservation_structure_id: Option<String>,
    stage_index: usize,
    stage_work_done: u64,
    pending: BTreeMap<String, PendingOperation>,
    queued: VecDeque<QueuedOperation>,
    pending_counter: Option<PendingCounter>,
    pending_registry_read: Option<PendingRegistryRead>,
    pending_registry_write: Option<PendingRegistryWrite>,
    pending_registry_sites: Option<PendingRegistrySites>,
    pending_registry_survey: Option<PendingRegistrySurvey>,
    counter: Option<u64>,
    counter_version: Option<u64>,
    counter_read_request: Option<String>,
    counter_read_session: Option<u64>,
    storage_sequence: u64,
    views: BTreeMap<u64, view::ViewState>,
    pending_view_players: BTreeMap<String, u64>,
    pending_view_project_players: BTreeMap<String, view::PendingViewProject>,
    pending_view_stock: BTreeMap<String, (u64, String)>,
    pending_view_role: Option<(String, String, String)>,
    pending_dismiss: Option<demob_runtime::PendingDismiss>,
    pending_hire: Option<hire_runtime::PendingHire>,
    pending_capture: Option<capture_runtime::PendingCapture>,
    pending_evacuation: Option<evacuation::PendingEvacuation>,
    pending_combat: Option<combat::PendingCombat>,
    pending_squad: Option<combat::PendingSquad>,
}

impl Settlements {
    fn next_storage_request(&mut self, prefix: &str) -> Option<String> {
        self.storage_sequence = self.storage_sequence.checked_add(1)?;
        Some(format!("{prefix}-{}", self.storage_sequence))
    }

    fn binding_pending(&self) -> bool {
        self.pending.values().any(|pending| {
            matches!(
                pending.kind,
                PendingKind::Bind { .. }
                    | PendingKind::BindVillage { .. }
                    | PendingKind::BindManual { .. }
            )
        }) || self
            .pending_counter
            .as_ref()
            .is_some_and(|pending| pending.intent.is_bind())
            || self.queued.iter().any(|queued| queued.intent.is_bind())
    }

    fn legacy_create(
        &mut self,
        session: u64,
        player: &str,
        name: &str,
        size_class: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(name) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        if !matches!(size_class, "small" | "medium" | "large") {
            return vec![message_session(
                session,
                "Settlement size must be small, medium, or large.",
            )];
        }
        self.begin_registry_read(RegistryRead::Create {
            session,
            player: player.to_owned(),
            name: name.to_owned(),
            size_class: size_class.to_owned(),
        })
    }

    fn legacy_list(&mut self, session: u64, player: &str) -> Vec<Command> {
        self.begin_registry_read(RegistryRead::List {
            session,
            player: player.to_owned(),
        })
    }

    fn legacy_info(&mut self, session: u64, player: &str, name: Option<&str>) -> Vec<Command> {
        if name.is_some_and(|name| !valid_settlement_name(name)) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        self.begin_registry_read(RegistryRead::Info {
            session,
            player: player.to_owned(),
            name: name.map(str::to_owned),
        })
    }

    fn overview(
        &mut self,
        session: u64,
        player: &str,
        name: Option<&str>,
        page: usize,
        fresh: bool,
    ) -> Vec<Command> {
        if name.is_some_and(|name| !valid_settlement_name(name)) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        let mut commands = if fresh {
            self.view_reopen(session)
        } else {
            Vec::new()
        };
        commands.extend(self.begin_registry_read(RegistryRead::Overview {
            session,
            player: player.to_owned(),
            name: name.map(str::to_owned),
            page,
        }));
        commands
    }

    fn legacy_sites(&mut self, session: u64, player: &str, settlement: &str) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        self.begin_registry_read(RegistryRead::Sites {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
        })
    }

    fn legacy_adopt(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        site_id: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) || !valid_identity(site_id) {
            return vec![message_session(
                session,
                "Pass a valid settlement name and deterministic site id.",
            )];
        }
        self.begin_registry_read(RegistryRead::Adopt {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            site_id: site_id.to_owned(),
        })
    }

    fn legacy_populate(&mut self, session: u64, player: &str, settlement: &str) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        self.begin_registry_read(RegistryRead::Populate {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
        })
    }

    fn legacy_survey(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        purpose: Option<&str>,
        x: f64,
        y: f64,
        z: f64,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        let Some((purpose, purpose_name, extent)) = parse_survey_purpose(purpose) else {
            return vec![message_session(
                session,
                "Purpose must be plot, expand, or restore.",
            )];
        };
        self.begin_registry_read(RegistryRead::Survey {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            purpose,
            purpose_name,
            bounds: survey_bounds(x, y, z, extent),
        })
    }

    fn legacy_project(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        blueprint: &str,
        anchor: settlements::BlockPosition,
        rotation: u16,
        preview: Option<view::PreviewOrigin>,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        if !valid_resource_id(blueprint) {
            return vec![message_session(
                session,
                "Blueprint id must be a resource identifier of at most 128 bytes.",
            )];
        }
        self.begin_registry_read(RegistryRead::Project {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            blueprint: blueprint.to_owned(),
            anchor,
            rotation,
            preview,
        })
    }

    fn legacy_role(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        target: &str,
        role: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) || !valid_identity(target) {
            return vec![message_session(
                session,
                "Pass a valid settlement name and player identity.",
            )];
        }
        let role = role.to_ascii_lowercase();
        if !matches!(role.as_str(), "member" | "steward" | "captain") {
            return vec![message_session(
                session,
                "Role must be steward, captain, or member.",
            )];
        }
        self.begin_registry_read(RegistryRead::Role {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            target: target.to_owned(),
            role,
        })
    }

    fn legacy_condition(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        ruined: bool,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        self.begin_registry_read(RegistryRead::Condition {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            ruined,
        })
    }

    fn legacy_fund(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        building: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) || !valid_building_name(building) {
            return vec![message_session(
                session,
                "Pass a valid settlement and building name.",
            )];
        }
        self.begin_registry_read(RegistryRead::Fund {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            building: building.to_owned(),
        })
    }

    fn legacy_advance(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        building: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) || !valid_building_name(building) {
            return vec![message_session(
                session,
                "Pass a valid settlement and building name.",
            )];
        }
        self.begin_registry_read(RegistryRead::Advance {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            building: building.to_owned(),
        })
    }

    fn legacy_specialize(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        requested: &[&str],
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        if requested.len() > 2 {
            return vec![message_session(session, "At most two specializations.")];
        }
        let specialties = requested
            .iter()
            .map(|specialty| specialty.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if specialties
            .iter()
            .any(|specialty| specialty_definition(specialty).is_none())
            || specialties.windows(2).any(|pair| pair[0] == pair[1])
        {
            return vec![message_session(
                session,
                "Up to two of: farming, forestry, ranching, fishing, mining.",
            )];
        }
        self.begin_registry_read(RegistryRead::Specialize {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            specialties,
        })
    }

    fn begin_registry_read(&mut self, action: RegistryRead) -> Vec<Command> {
        if self.pending_hire.is_some()
            || self.pending_dismiss.is_some()
            || self.pending_registry_read.is_some()
            || self.pending_registry_write.is_some()
            || self.pending_registry_sites.is_some()
            || self.pending_registry_survey.is_some()
            || self
                .pending_status
                .values()
                .any(|pending| matches!(&pending.continuation, StatusContinuation::Specialize(_)))
            || self
                .pending
                .values()
                .any(|pending| matches!(&pending.kind, PendingKind::Prepare { project: Some(_) }))
            || self
                .pending
                .values()
                .any(|pending| matches!(&pending.kind, PendingKind::Recognize { .. }))
            || self
                .pending_counter
                .as_ref()
                .is_some_and(|pending| pending.project.is_some())
            || self.queued.iter().any(|queued| queued.project.is_some())
            || self
                .queued
                .iter()
                .any(|queued| matches!(&queued.intent, OperationIntent::Recognize { .. }))
            || self.pending.values().any(|pending| {
                matches!(
                    &pending.kind,
                    PendingKind::ReserveSite {
                        population: Some(_)
                    } | PendingKind::SpawnResident {
                        population: Some(_),
                        ..
                    }
                )
            })
            || self.pending_counter.as_ref().is_some_and(|pending| {
                pending.population.is_some()
                    || matches!(pending.intent, OperationIntent::Recognize { .. })
            })
            || self.queued.iter().any(|queued| queued.population.is_some())
        {
            return vec![message_session(
                action.session(),
                "Another settlement record update is pending.",
            )];
        }
        let Some(request) = self.next_storage_request("settlement-registry-read") else {
            return vec![message_session(
                action.session(),
                "Settlement records are unavailable.",
            )];
        };
        self.pending_registry_read = Some(PendingRegistryRead {
            request: request.clone(),
            action,
        });
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    fn registry_get_answer(
        &mut self,
        pending: PendingRegistryRead,
        answered: &StorageGetAnswered,
    ) -> Vec<Command> {
        let StorageGetOutcome::Read(record) = &answered.outcome else {
            return vec![message_session(
                pending.action.session(),
                "Settlement records are unavailable.",
            )];
        };
        let version = record.version;
        let registry = match record.value.as_deref() {
            Some(value) => match serde_json::from_str::<SettlementRegistry>(value) {
                Ok(registry) => registry,
                Err(_) => {
                    return vec![message_session(
                        pending.action.session(),
                        "Settlement records are unreadable.",
                    )];
                }
            },
            None => SettlementRegistry::default(),
        };
        match pending.action {
            RegistryRead::Create {
                session,
                player,
                name,
                size_class,
            } => self.registry_create(session, player, name, size_class, registry, version),
            RegistryRead::List { session, player } => {
                let names = registry
                    .settlements
                    .iter()
                    .filter_map(|(name, record)| {
                        is_settlement_member(record, &player).then_some(name.as_str())
                    })
                    .collect::<Vec<_>>();
                let text = if names.is_empty() {
                    "No settlements available to you.".to_owned()
                } else {
                    format!("Settlements: {}.", names.join(", "))
                };
                vec![message_session(session, text)]
            }
            RegistryRead::Info {
                session,
                player,
                name,
            } => {
                let record = name
                    .as_deref()
                    .and_then(|name| registry.settlements.get(name).map(|record| (name, record)))
                    .or_else(|| {
                        registry
                            .settlements
                            .iter()
                            .find(|(_, record)| is_settlement_member(record, &player))
                            .map(|(name, record)| (name.as_str(), record))
                    });
                let Some((name, record)) = record else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !is_settlement_member(record, &player) {
                    return vec![message_session(
                        session,
                        "You are not a member of that settlement.",
                    )];
                }
                let buildings = if record.buildings.is_empty() {
                    "none".to_owned()
                } else {
                    record
                        .buildings
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let residents = if record.residents.is_empty() {
                    "none".to_owned()
                } else {
                    record
                        .residents
                        .iter()
                        .map(|(alias, resident)| {
                            format!(
                                "{alias}={}@{}:{}",
                                resident.handle,
                                resident.work_revision,
                                resident.roster_status()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let specialties = if record.specialties.is_empty() {
                    "DONE".to_owned()
                } else {
                    record.specialties.join("+")
                };
                vec![message_session(
                    session,
                    format!(
                        "{name} | {} tier=hamlet condition={} buildings={buildings} specialties={specialties} residents={residents} revision={}",
                        record.size_class,
                        record.condition.as_deref().unwrap_or("inhabited"),
                        record.revision
                    ),
                )]
            }
            RegistryRead::Overview {
                session,
                player,
                name,
                page,
            } => self.view_registry_answer(session, player, name.as_deref(), page, &registry),
            RegistryRead::Sites {
                session,
                player,
                settlement,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !is_settlement_member(record, &player) {
                    return vec![message_session(
                        session,
                        "You are not a member of that settlement.",
                    )];
                }
                self.begin_registry_site_query(RegistrySiteQuery::List {
                    session,
                    settlement,
                })
            }
            RegistryRead::Adopt {
                session,
                player,
                settlement,
                site_id,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward adopts a site.",
                    )];
                }
                self.begin_registry_site_query(RegistrySiteQuery::Adopt {
                    session,
                    settlement,
                    site_id,
                    registry,
                    version,
                })
            }
            RegistryRead::Treat {
                session,
                player,
                settlement,
                alias,
            } => self.treatment_registry_answer(session, &player, &settlement, &alias, &registry),
            RegistryRead::Populate {
                session,
                player,
                settlement,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !is_settlement_member(record, &player) {
                    return vec![message_session(
                        session,
                        "You are not a member of that settlement.",
                    )];
                }
                let Some(site_id) = record.site_id.as_deref() else {
                    return vec![message_session(
                        session,
                        "Adopt a site first: /settlement site / adopt.",
                    )];
                };
                let resident_name = match next_settlement_resident_name(record) {
                    Ok(name) => name,
                    Err(message) => return vec![message_session(session, message)],
                };
                self.begin_registry_site_query(RegistrySiteQuery::Populate {
                    session,
                    site_id: site_id.to_owned(),
                    population: PendingSettlementPopulation {
                        settlement,
                        resident_name,
                        registry,
                        version,
                    },
                })
            }
            RegistryRead::Survey {
                session,
                player,
                settlement,
                purpose,
                purpose_name,
                bounds,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward surveys a settlement.",
                    )];
                }
                if record.site_id.is_none() {
                    return vec![message_session(
                        session,
                        "Adopt a deterministic site first: /settlement site / adopt.",
                    )];
                }
                self.begin_registry_survey(
                    PendingRegistrySurvey {
                        request: String::new(),
                        session,
                        settlement,
                        purpose_name,
                        registry,
                        version,
                    },
                    purpose,
                    bounds,
                )
            }
            RegistryRead::Project {
                session,
                player,
                settlement,
                blueprint,
                anchor,
                rotation,
                preview,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward projects buildings.",
                    )];
                }
                let (Some(site_id), Some(survey_token)) =
                    (record.site_id.as_ref(), record.survey_token.as_ref())
                else {
                    return vec![message_session(
                        session,
                        "Survey the plot first: /settlement survey <name> plot.",
                    )];
                };
                let building_name = match next_settlement_building_name(record, &blueprint) {
                    Ok(name) => name,
                    Err(message) => return vec![message_session(session, message)],
                };
                self.begin_registry_site_query(RegistrySiteQuery::Project {
                    session,
                    settlement,
                    building_name,
                    site_id: site_id.clone(),
                    survey_token: survey_token.clone(),
                    blueprint,
                    anchor,
                    rotation,
                    preview,
                    registry,
                    version,
                })
            }
            RegistryRead::Recognize {
                session,
                player,
                settlement,
                building_name,
                purpose,
                footprint,
                points,
                survey_token,
                expected_world_revision,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward recognizes buildings.",
                    )];
                };
                if record.buildings.len() >= MAX_SETTLEMENT_BUILDINGS
                    && !record.buildings.contains_key(&building_name)
                {
                    return vec![message_session(
                        session,
                        "Building limit reached for this settlement.",
                    )];
                }
                let borrowed: Vec<&str> = points.iter().map(String::as_str).collect();
                let Some(parsed_points) = parse_interaction_points(&borrowed) else {
                    return vec![message_session(
                        session,
                        "Points must be kind:x,y,z with kind entrance, home, work, meeting, guard, station=<namespace:path>, or storage (example station=minecraft:crafting_table:3,61,3).",
                    )];
                };
                self.submit(OperationIntent::Recognize {
                    session,
                    settlement,
                    building_name,
                    purpose: purpose.clone(),
                    footprint,
                    points: parsed_points,
                    survey_token: survey_token.clone(),
                    expected_world_revision,
                    registry,
                    version,
                })
            }
            RegistryRead::Role {
                session,
                player,
                settlement,
                target,
                role,
            } => self.registry_role(session, player, settlement, target, role, registry, version),
            RegistryRead::Condition {
                session,
                player,
                settlement,
                ruined,
            } => self.registry_condition(session, player, settlement, ruined, registry, version),
            RegistryRead::Fund {
                session,
                player,
                settlement,
                building,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward funds buildings.",
                    )];
                }
                let Some(structure_id) =
                    record
                        .buildings
                        .get(&building)
                        .and_then(|entry| match &entry.origin {
                            BuildingOrigin::Structure { structure_id } => {
                                Some(structure_id.as_str())
                            }
                            BuildingOrigin::Certificate { .. } => None,
                        })
                else {
                    return vec![message_session(
                        session,
                        "Building not found in this settlement.",
                    )];
                };
                self.status_for_funding(session, structure_id)
            }
            RegistryRead::Advance {
                session,
                player,
                settlement,
                building,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward advances buildings.",
                    )];
                }
                let Some(structure_id) =
                    record
                        .buildings
                        .get(&building)
                        .and_then(|entry| match &entry.origin {
                            BuildingOrigin::Structure { structure_id } => {
                                Some(structure_id.as_str())
                            }
                            BuildingOrigin::Certificate { .. } => None,
                        })
                else {
                    return vec![message_session(
                        session,
                        "Building not found in this settlement.",
                    )];
                };
                self.status_for_advance(session, structure_id)
            }
            RegistryRead::PersistResident { session, resident } => {
                self.registry_persist_resident(session, resident, registry, version)
            }
            RegistryRead::Specialize {
                session,
                player,
                settlement,
                specialties,
            } => self.registry_specialize(
                session,
                player,
                settlement,
                specialties,
                registry,
                version,
            ),
        }
    }

    fn registry_persist_resident(
        &mut self,
        session: u64,
        resident: PendingResidentPersistence,
        mut registry: SettlementRegistry,
        version: Option<u64>,
    ) -> Vec<Command> {
        let Err(message) = record_settlement_resident(
            &mut registry,
            &resident.settlement,
            &resident.resident_name,
            &resident.handle,
            resident.work_revision,
        ) else {
            let Ok(value) = serde_json::to_string(&registry) else {
                return vec![message_session(
                    session,
                    "Worker spawned, but settlement records are unavailable.",
                )];
            };
            let Some(request) = self.next_storage_request("settlement-registry-populate-retry")
            else {
                return vec![message_session(
                    session,
                    "Worker spawned, but settlement records are unavailable.",
                )];
            };
            self.pending_registry_write = Some(PendingRegistryWrite {
                request: request.clone(),
                session,
                message: format!(
                    "Settled {} in {}.",
                    resident.resident_name, resident.settlement
                ),
                resident: None,
            });
            return vec![storage_cas(
                &request,
                SETTLEMENT_REGISTRY_KEY,
                version,
                value,
            )];
        };
        vec![message_session(
            session,
            format!("Worker spawned, but {message}"),
        )]
    }

    fn registry_create(
        &mut self,
        session: u64,
        player: String,
        name: String,
        size_class: String,
        mut registry: SettlementRegistry,
        version: Option<u64>,
    ) -> Vec<Command> {
        if let Err(message) = register_settlement(&mut registry, player, &name, &size_class) {
            return vec![message_session(session, message)];
        }
        let Ok(value) = serde_json::to_string(&registry) else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        let Some(request) = self.next_storage_request("settlement-registry-create") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        self.pending_registry_write = Some(PendingRegistryWrite {
            request: request.clone(),
            session,
            resident: None,
            message: format!("Founded {name} ({size_class} hamlet)."),
        });
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            version,
            value,
        )]
    }
    fn registry_role(
        &mut self,
        session: u64,
        player: String,
        settlement: String,
        target: String,
        role: String,
        mut registry: SettlementRegistry,
        version: Option<u64>,
    ) -> Vec<Command> {
        let Some(record) = registry.settlements.get(&settlement) else {
            return vec![message_session(session, "Settlement not found.")];
        };
        if record.owner != player {
            return vec![message_session(session, "Only the owner assigns roles.")];
        }
        if let Err(message) = assign_settlement_role(&mut registry, &settlement, &target, &role) {
            return vec![message_session(session, message)];
        }
        let Ok(value) = serde_json::to_string(&registry) else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        let Some(request) = self.next_storage_request("settlement-registry-role") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        self.pending_registry_write = Some(PendingRegistryWrite {
            request: request.clone(),
            session,
            resident: None,
            message: format!("Roles updated for {settlement}."),
        });
        self.pending_view_role = Some((request.clone(), settlement, target));
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            version,
            value,
        )]
    }

    fn registry_condition(
        &mut self,
        session: u64,
        player: String,
        settlement: String,
        ruined: bool,
        mut registry: SettlementRegistry,
        version: Option<u64>,
    ) -> Vec<Command> {
        let Some(record) = registry.settlements.get(&settlement) else {
            return vec![message_session(session, "Settlement not found.")];
        };
        if !can_build_settlement(record, &player) {
            return vec![message_session(
                session,
                "Only the owner or a steward marks condition.",
            )];
        }
        let condition = if ruined {
            "partially_ruined"
        } else {
            "inhabited"
        };
        let Err(message) = set_settlement_condition(&mut registry, &settlement, condition) else {
            let Ok(value) = serde_json::to_string(&registry) else {
                return vec![message_session(
                    session,
                    "Settlement records are unavailable.",
                )];
            };
            let Some(request) = self.next_storage_request("settlement-registry-condition") else {
                return vec![message_session(
                    session,
                    "Settlement records are unavailable.",
                )];
            };
            self.pending_registry_write = Some(PendingRegistryWrite {
                request: request.clone(),
                session,
                resident: None,
                message: format!("{settlement} is now {condition}."),
            });
            return vec![storage_cas(
                &request,
                SETTLEMENT_REGISTRY_KEY,
                version,
                value,
            )];
        };
        vec![message_session(session, message)]
    }

    fn registry_specialize(
        &mut self,
        session: u64,
        player: String,
        settlement: String,
        specialties: Vec<String>,
        registry: SettlementRegistry,
        version: Option<u64>,
    ) -> Vec<Command> {
        let Some(record) = registry.settlements.get(&settlement) else {
            return vec![message_session(session, "Settlement not found.")];
        };
        if !can_build_settlement(record, &player) {
            return vec![message_session(
                session,
                "Only the owner or a steward changes specializations.",
            )];
        }

        let mut workplace_candidates = Vec::with_capacity(specialties.len());
        for specialty in &specialties {
            let definition =
                specialty_definition(specialty).expect("validated specialty reaches registry");
            if record.survey_token.is_none() {
                return vec![message_session(
                    session,
                    "Survey the site first; suitability is a terrain fact.",
                )];
            }
            if !survey_supports_specialty(record, definition.tags) {
                return vec![message_session(
                    session,
                    format!("Survey tags do not support {specialty} here."),
                )];
            }
            let candidates = record
                .buildings
                .values()
                .filter(|building| building.purpose.as_str() == definition.blueprint_id)
                .map(|building| WorkplaceCandidate {
                    origin: building.origin.clone(),
                    id: building_origin_id(building).to_owned(),
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return vec![message_session(
                    session,
                    format!("{specialty} needs a committed {}.", definition.blueprint_id),
                )];
            }
            workplace_candidates.push(candidates);
        }

        self.continue_specialization(PendingSpecialization {
            session,
            settlement,
            specialties,
            workplace_candidates,
            next_specialty: 0,
            next_candidate: 0,
            registry,
            version,
        })
    }

    fn begin_registry_site_query(&mut self, query: RegistrySiteQuery) -> Vec<Command> {
        let session = match &query {
            RegistrySiteQuery::List { session, .. }
            | RegistrySiteQuery::Adopt { session, .. }
            | RegistrySiteQuery::Project { session, .. }
            | RegistrySiteQuery::Populate { session, .. } => *session,
        };
        let Some(request) = self.next_storage_request("settlement-registry-sites") else {
            return vec![message_session(
                session,
                "Settlement sites are unavailable.",
            )];
        };
        let command = match &query {
            RegistrySiteQuery::Adopt { site_id, .. }
            | RegistrySiteQuery::Project { site_id, .. }
            | RegistrySiteQuery::Populate { site_id, .. } => {
                query_settlement_site(&request, site_id, None, SITE_PAGE)
            }
            RegistrySiteQuery::List { .. } => list_settlement_sites(&request, None, SITE_PAGE),
        };
        self.pending_registry_sites = Some(PendingRegistrySites { request, query });
        vec![command]
    }

    fn registry_sites_answer(
        &mut self,
        pending: PendingRegistrySites,
        answered: &OperationAnswered,
    ) -> Vec<Command> {
        let session = match &pending.query {
            RegistrySiteQuery::List { session, .. }
            | RegistrySiteQuery::Adopt { session, .. }
            | RegistrySiteQuery::Project { session, .. }
            | RegistrySiteQuery::Populate { session, .. } => *session,
        };
        let OperationOutcome::Committed(committed) = &answered.outcome else {
            return vec![message_session(session, "The site query was refused.")];
        };
        match pending.query {
            RegistrySiteQuery::List { settlement, .. } => {
                let OperationPayload::Settlement(SettlementResult::Sites(page)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The site listing returned an unexpected result.",
                    )];
                };
                let sites = page
                    .sites
                    .iter()
                    .map(|site| format!("{}@{}", site.site_id, site.revision))
                    .collect::<Vec<_>>();
                let text = if sites.is_empty() {
                    format!("No sites available for {settlement}.")
                } else {
                    format!("{settlement} sites: {}.", sites.join(", "))
                };
                vec![message_session(session, text)]
            }
            RegistrySiteQuery::Adopt {
                settlement,
                site_id,
                mut registry,
                version,
                ..
            } => {
                let OperationPayload::Settlement(SettlementResult::Site(site)) = &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The adoption site query returned an unexpected result.",
                    )];
                };
                if site.site_id != site_id {
                    return vec![message_session(
                        session,
                        "The adoption site query returned an unexpected site.",
                    )];
                }
                if !site.contents_known {
                    return vec![message_session(
                        session,
                        "That site's materialized contents are unavailable.",
                    )];
                }
                let Err(message) =
                    adopt_settlement_site(&mut registry, &settlement, &site.site_id, site.revision)
                else {
                    let Ok(value) = serde_json::to_string(&registry) else {
                        return vec![message_session(
                            session,
                            "Settlement records are unavailable.",
                        )];
                    };
                    let Some(request) = self.next_storage_request("settlement-registry-adopt")
                    else {
                        return vec![message_session(
                            session,
                            "Settlement records are unavailable.",
                        )];
                    };
                    self.pending_registry_write = Some(PendingRegistryWrite {
                        request: request.clone(),
                        session,
                        message: format!("Adopted site {} for {settlement}.", site.site_id),
                        resident: None,
                    });
                    return vec![storage_cas(
                        &request,
                        SETTLEMENT_REGISTRY_KEY,
                        version,
                        value,
                    )];
                };
                vec![message_session(session, message)]
            }
            RegistrySiteQuery::Populate {
                site_id,
                population,
                ..
            } => {
                let OperationPayload::Settlement(SettlementResult::Site(site)) = &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The population site query returned an unexpected result.",
                    )];
                };
                if site.site_id != site_id {
                    return vec![message_session(
                        session,
                        "The population site query returned an unexpected site.",
                    )];
                }
                if !site.contents_known {
                    return vec![message_session(
                        session,
                        "That site's materialized contents are unavailable.",
                    )];
                }
                let Some(home) = site
                    .pois
                    .iter()
                    .find(|poi| poi.kind == SitePoiKind::Home && poi.state == SitePoiState::Free)
                else {
                    return vec![message_session(
                        session,
                        "No free home point of interest; build or adopt more housing.",
                    )];
                };
                self.submit_population(
                    OperationIntent::ReserveSite {
                        session,
                        site_id: site.site_id.clone(),
                        poi_id: home.poi_id.clone(),
                        expected_revision: site.revision,
                    },
                    population,
                )
            }
            RegistrySiteQuery::Project {
                settlement,
                building_name,
                site_id,
                survey_token,
                blueprint,
                anchor,
                rotation,
                preview,
                registry,
                version,
                ..
            } => {
                let OperationPayload::Settlement(SettlementResult::Site(site)) = &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The project site query returned an unexpected result.",
                    )];
                };
                if site.site_id != site_id {
                    return vec![message_session(
                        session,
                        "The project site query returned an unexpected site.",
                    )];
                }
                self.submit_project(
                    OperationIntent::Prepare {
                        session,
                        site_id,
                        blueprint,
                        anchor,
                        rotation,
                        survey_token,
                        expected_site_revision: site.revision,
                        preview,
                    },
                    PendingSettlementProject {
                        settlement,
                        building_name,
                        registry,
                        version,
                    },
                )
            }
        }
    }

    fn begin_registry_survey(
        &mut self,
        mut pending: PendingRegistrySurvey,
        purpose: SurveyPurpose,
        bounds: SurveyBounds,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("settlement-registry-survey") else {
            return vec![message_session(
                pending.session,
                "Settlement survey is unavailable.",
            )];
        };
        pending.request = request.clone();
        self.pending_registry_survey = Some(pending);
        vec![survey_site(&request, STATION_DIMENSION, bounds, purpose)]
    }

    fn registry_survey_answer(
        &mut self,
        pending: PendingRegistrySurvey,
        answered: &OperationAnswered,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = &answered.outcome else {
            return vec![message_session(
                pending.session,
                "Settlement survey was refused.",
            )];
        };
        let OperationPayload::Settlement(SettlementResult::Survey(survey)) = &committed.payload
        else {
            return vec![message_session(
                pending.session,
                "Settlement survey returned an unexpected result.",
            )];
        };
        let mut registry = pending.registry;
        let Err(message) = record_settlement_survey(
            &mut registry,
            &pending.settlement,
            &survey.survey_token,
            &pending.purpose_name,
            survey.biome_tags.clone(),
            survey.resource_tags.clone(),
        ) else {
            let Ok(value) = serde_json::to_string(&registry) else {
                return vec![message_session(
                    pending.session,
                    "Settlement records are unavailable.",
                )];
            };
            let Some(request) = self.next_storage_request("settlement-registry-survey-write")
            else {
                return vec![message_session(
                    pending.session,
                    "Settlement records are unavailable.",
                )];
            };
            self.pending_registry_write = Some(PendingRegistryWrite {
                request: request.clone(),
                session: pending.session,
                message: format!("{} plot surveyed.", pending.settlement),
                resident: None,
            });
            return vec![storage_cas(
                &request,
                SETTLEMENT_REGISTRY_KEY,
                pending.version,
                value,
            )];
        };
        vec![message_session(pending.session, message)]
    }

    fn command(&mut self, invoked: &CommandInvoked, observed_tick: u64) -> Vec<Command> {
        if invoked.name != "settlement" {
            return Vec::new();
        }
        let arguments = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        match arguments.as_slice() {
            ["create", name, size_class] => {
                self.legacy_create(invoked.session, &invoked.player, name, size_class)
            }
            ["list"] => self.legacy_list(invoked.session, &invoked.player),
            ["info"] => self.legacy_info(invoked.session, &invoked.player, None),
            ["info", name] => self.legacy_info(invoked.session, &invoked.player, Some(name)),
            ["overview"] => self.overview(invoked.session, &invoked.player, None, 0, true),
            ["overview", name] => self.overview(invoked.session, &invoked.player, Some(name), 0, true),
            ["hire", settlement, resident, role] => self.hire(
                invoked.session,
                &invoked.player,
                settlement,
                resident,
                role,
                observed_tick,
            ),
            ["treat", settlement, resident] => {
                self.treat(invoked.session, &invoked.player, settlement, resident)
            }
            ["dismiss", settlement, resident] => {
                self.dismiss(invoked.session, &invoked.player, settlement, resident)
            }
            ["evacuate", settlement, medic, medic_revision, patient, patient_revision, x, y, z] => self.evacuate(
                invoked.session,
                &invoked.player,
                settlement,
                medic,
                medic_revision,
                patient,
                patient_revision,
                [x, y, z],
            ),
            ["capture", captor, guard, victim, resident, revision] => self.capture(
                invoked.session,
                &invoked.player,
                captor,
                guard,
                victim,
                resident,
                revision,
            ),
            ["defend", settlement, resident, revision, x, y, z, rx, ry, rz] => self.defend(
                invoked.session,
                &invoked.player,
                settlement,
                resident,
                revision,
                [x, y, z],
                [rx, ry, rz],
                None,
            ),
            ["defend", settlement, resident, revision, x, y, z, rx, ry, rz, officer] => self.defend(
                invoked.session,
                &invoked.player,
                settlement,
                resident,
                revision,
                [x, y, z],
                [rx, ry, rz],
                Some(officer),
            ),
            ["march", settlement, roster, x, y, z] => self.squad_command(
                invoked.session,
                &invoked.player,
                settlement,
                roster,
                Some([x, y, z]),
                false,
            ),
            ["halt", settlement, roster] => self.squad_command(
                invoked.session,
                &invoked.player,
                settlement,
                roster,
                None,
                false,
            ),
            ["retreat", settlement, roster, x, y, z] => self.squad_command(
                invoked.session,
                &invoked.player,
                settlement,
                roster,
                Some([x, y, z]),
                true,
            ),
            ["role", settlement, target, role] => {
                self.legacy_role(invoked.session, &invoked.player, settlement, target, role)
            }
            ["ruin", settlement] => {
                self.legacy_condition(invoked.session, &invoked.player, settlement, true)
            }
            ["restore", settlement] => {
                self.legacy_condition(invoked.session, &invoked.player, settlement, false)
            }
            ["site", settlement] => self.legacy_sites(invoked.session, &invoked.player, settlement),
            ["adopt", settlement, site_id] => {
                self.legacy_adopt(invoked.session, &invoked.player, settlement, site_id)
            }
            ["populate", settlement] => {
                self.legacy_populate(invoked.session, &invoked.player, settlement)
            }
            ["bind", structure_id, container_id] => {
                self.bind(invoked.session, structure_id, container_id)
            }
            ["bind-village", site_id, container_id] => {
                self.bind_village(invoked.session, site_id, container_id)
            }
            ["bind-manual", x, y, z] => {
                self.bind_manual(invoked.session, x, y, z)
            }
            ["recognize", settlement, building, purpose, points @ ..] => {
                self.recognize(invoked.session, &invoked.player, settlement, building, purpose, points)
            }
            ["haul", resident, expected_revision] => {
                self.haul(invoked.session, resident, expected_revision, None)
            }
            ["haul", resident, expected_revision, item] => {
                self.haul(invoked.session, resident, expected_revision, Some(item))
            }
            ["issue", resident, expected_revision, destination] => {
                self.issue_haul(invoked.session, resident, expected_revision, destination, None)
            }
            ["issue", resident, expected_revision, destination, item] => {
                self.issue_haul(
                    invoked.session,
                    resident,
                    expected_revision,
                    destination,
                    Some(item),
                )
            }
            ["survey", settlement] => self.legacy_survey(
                invoked.session,
                &invoked.player,
                settlement,
                None,
                invoked.position.x,
                invoked.position.y,
                invoked.position.z,
            ),
            ["survey", settlement, purpose] => self.legacy_survey(
                invoked.session,
                &invoked.player,
                settlement,
                Some(purpose),
                invoked.position.x,
                invoked.position.y,
                invoked.position.z,
            ),
            ["project", settlement, blueprint] => self.legacy_project(
                invoked.session,
                &invoked.player,
                settlement,
                blueprint,
                settlements::BlockPosition {
                    x: invoked.position.x.floor() as i32,
                    y: invoked.position.y.floor() as i32,
                    z: invoked.position.z.floor() as i32,
                },
                0,
                None,
            ),
            ["project", settlement, blueprint, x, y, z] => {
                let [Ok(x), Ok(y), Ok(z)] = [x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()]
                else {
                    return vec![message_session(
                        invoked.session,
                        "Structure coordinates must be signed 32-bit integers.",
                    )];
                };
                self.legacy_project(
                    invoked.session,
                    &invoked.player,
                    settlement,
                    blueprint,
                    settlements::BlockPosition { x, y, z },
                    0,
                    None,
                )
            }
            ["fund", settlement, building] => {
                self.legacy_fund(invoked.session, &invoked.player, settlement, building)
            }
            ["advance", settlement, building] => {
                self.legacy_advance(invoked.session, &invoked.player, settlement, building)
            }
            ["specialize", settlement] => {
                self.legacy_specialize(invoked.session, &invoked.player, settlement, &[])
            }
            ["specialize", settlement, first] => {
                self.legacy_specialize(invoked.session, &invoked.player, settlement, &[first])
            }
            ["specialize", settlement, first, second] => self.legacy_specialize(
                invoked.session,
                &invoked.player,
                settlement,
                &[first, second],
            ),
            ["specialize", _, _, _, ..] => {
                vec![message_session(invoked.session, "At most two specializations.")]
            }
            ["homes"] => self.homes(invoked.session),
            ["survey"] => self.survey(invoked.session),
            ["prepare", blueprint, x, y, z] => {
                self.prepare(invoked.session, blueprint, x, y, z)
            }
            ["status", structure_id] => self.status(invoked.session, structure_id),
            ["advance"] => self.advance(invoked.session),
            ["fund-warehouse"] => self.fund_warehouse(invoked.session),
            ["spawn", poi_id] => self.spawn_worker(invoked.session, poi_id),
            ["craft", resident, expected_revision, recipe, count, x, y, z] => self.craft(
                invoked.session,
                resident,
                expected_revision,
                recipe,
                count,
                x,
                y,
                z,
            ),
            ["harvest", resident, expected_revision, tool, min_x, min_y, min_z, max_x, max_y, max_z] => self.harvest(
                invoked.session,
                resident,
                expected_revision,
                tool,
                min_x,
                min_y,
                min_z,
                max_x,
                max_y,
                max_z,
            ),
            _ => vec![message_session(
                invoked.session,
                "Usage: /settlement create <name> <small|medium|large>, list, info [name], hire <name> <resident> <militia|infantry|spearman|archer>, treat <name> <resident>, dismiss <name> <resident>, defend <name> <resident> <order-revision> <x> <y> <z> <rally-x> <rally-y> <rally-z> [officer-alias], march <name> <alias:revision,alias:revision> <x> <y> <z>, retreat <name> <alias:revision,alias:revision> <x> <y> <z>, halt <name> <alias:revision,alias:revision>, role <name> <player> <steward|captain|member>, ruin <name>, restore <name>, site <name>, adopt <name> <site-id>, survey <name> [plot|expand|restore], project <name> <blueprint-id> [x y z], fund <name> <building>, advance <name> <building>, specialize <name> [farming|forestry|ranching|fishing|mining] [second specialty], or populate <name>. Component operations: bind, bind-village, bind-manual <x> <y> <z>, recognize <settlement> <building> <purpose> <point...>, haul, issue, craft, harvest, homes, survey, prepare, status, fund, advance, spawn. Points are kind:x,y,z with kind entrance, home, work, meeting, guard, station=<namespace:path>, or storage (example station=minecraft:crafting_table:3,61,3).",
            )],
        }
    }

    fn bind(&mut self, session: u64, structure_id: &str, container_id: &str) -> Vec<Command> {
        if !valid_identity(structure_id) {
            return vec![message_session(
                session,
                "Structure id must be 1..64 bytes.",
            )];
        }
        let Ok(container_id) = container_id.parse::<u32>() else {
            return vec![message_session(
                session,
                "Container id must be an unsigned 32-bit index.",
            )];
        };
        if self.binding_pending() {
            return vec![message_session(
                session,
                "A warehouse binding is already pending.",
            )];
        }
        self.submit(OperationIntent::Bind {
            session,
            structure_id: structure_id.to_owned(),
            container_id,
        })
    }

    fn recognize(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        building: &str,
        purpose: &str,
        points: &[&str],
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        if !valid_building_name(building) {
            return vec![message_session(
                session,
                "Building name must be 1..64 bytes.",
            )];
        }
        if !valid_resource_id(purpose) {
            return vec![message_session(
                session,
                "Purpose must be a resource identifier of at most 128 bytes.",
            )];
        }
        let Some((survey_token, footprint, expected_world_revision)) = self
            .survey_token
            .clone()
            .zip(self.survey_bounds.clone())
            .zip(self.survey_world_revision)
            .map(|((token, bounds), revision)| (token, bounds, revision))
        else {
            return vec![message_session(
                session,
                "Survey the footprint before recognizing it.",
            )];
        };
        self.begin_registry_read(RegistryRead::Recognize {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            building_name: building.to_owned(),
            purpose: purpose.to_owned(),
            footprint,
            points: points.iter().map(|point| (*point).to_owned()).collect(),
            survey_token,
            expected_world_revision,
        })
    }

    fn bind_manual(&mut self, session: u64, x: &str, y: &str, z: &str) -> Vec<Command> {
        let [Ok(x), Ok(y), Ok(z)] = [x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()] else {
            return vec![message_session(
                session,
                "Container coordinates must be signed 32-bit integers.",
            )];
        };
        let Some(survey_token) = &self.survey_token else {
            return vec![message_session(
                session,
                "Survey a site before binding its containers.",
            )];
        };
        if self.binding_pending() {
            return vec![message_session(
                session,
                "A warehouse binding is already pending.",
            )];
        }
        self.submit(OperationIntent::BindManual {
            session,
            survey_token: survey_token.clone(),
            position: settlements::BlockPosition { x, y, z },
        })
    }

    fn bind_village(&mut self, session: u64, site_id: &str, container_id: &str) -> Vec<Command> {
        if !valid_identity(site_id) {
            return vec![message_session(
                session,
                "Village site id must be 1..64 bytes.",
            )];
        }
        let Ok(container_id) = container_id.parse::<u32>() else {
            return vec![message_session(
                session,
                "Container id must be an unsigned 32-bit index.",
            )];
        };
        if self.binding_pending() {
            return vec![message_session(
                session,
                "A warehouse binding is already pending.",
            )];
        }
        self.submit(OperationIntent::BindVillage {
            session,
            site_id: site_id.to_owned(),
            container_id,
        })
    }

    fn haul(
        &mut self,
        session: u64,
        resident: &str,
        expected_revision: &str,
        item: Option<&str>,
    ) -> Vec<Command> {
        self.assign_haul(
            session,
            resident,
            expected_revision,
            HaulDirection::Deposit,
            item,
        )
    }

    fn issue_haul(
        &mut self,
        session: u64,
        resident: &str,
        expected_revision: &str,
        destination: &str,
        item: Option<&str>,
    ) -> Vec<Command> {
        let Some(destination) = IssueDestination::parse(destination) else {
            return vec![message_session(
                session,
                "Issue destination must be carry or equipment.",
            )];
        };
        self.assign_haul(
            session,
            resident,
            expected_revision,
            HaulDirection::Issue(destination),
            item,
        )
    }

    fn assign_haul(
        &mut self,
        session: u64,
        resident: &str,
        expected_revision: &str,
        direction: HaulDirection,
        item: Option<&str>,
    ) -> Vec<Command> {
        if !valid_identity(resident) {
            return vec![message_session(
                session,
                "Resident handle must be 1..64 bytes.",
            )];
        }
        let Some(expected_revision) = expected_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Expected revision must be an unsigned 53-bit integer.",
            )];
        };
        if item.is_some_and(|item| !valid_resource_id(item)) {
            return vec![message_session(
                session,
                "Item id must be a resource identifier of at most 128 bytes.",
            )];
        }
        let Some(warehouse) = self
            .warehouse
            .as_ref()
            .map(|binding| binding.handle.clone())
        else {
            return vec![message_session(
                session,
                "Bind a warehouse before assigning a haul.",
            )];
        };
        self.submit(OperationIntent::Haul {
            session,
            resident: resident.to_owned(),
            warehouse,
            expected_revision,
            direction,
            item: item.map(str::to_owned),
        })
    }

    /// List the settlement sites and remember the first one that still holds a
    /// free home point of interest, with the cells those homes stand at.
    ///
    /// A listing is a query: it carries no durable operation id and changes
    /// nothing, so it is correlated by its request alone and never by the
    /// operation-id guard the durable answers use.
    fn homes(&mut self, session: u64) -> Vec<Command> {
        let Some(request) = self.next_storage_request("settlement-sites") else {
            return vec![message_session(
                session,
                "No further settlement operations can be issued.",
            )];
        };
        self.pending_sites = Some(PendingSites {
            request: request.clone(),
            session,
        });
        vec![list_settlement_sites(&request, None, SITE_PAGE)]
    }

    /// Survey the listed site's anchor cell before a structure request. The
    /// server creates the short-lived token; the component only retains it.
    fn survey(&mut self, session: u64) -> Vec<Command> {
        let Some(origin) = self.worker_site.as_ref().map(|site| site.origin.clone()) else {
            return vec![message_session(
                session,
                "List homes before surveying a site.",
            )];
        };
        if self.pending_survey.is_some() {
            return vec![message_session(
                session,
                "A site survey is already pending.",
            )];
        }
        let Some(request) = self.next_storage_request("settlement-survey") else {
            return vec![message_session(
                session,
                "No further settlement operations can be issued.",
            )];
        };
        let bounds = SurveyBounds {
            min: origin.clone(),
            max: origin,
        };
        self.pending_survey = Some(PendingSurvey {
            request: request.clone(),
            session,
        });
        vec![survey_site(
            &request,
            STATION_DIMENSION,
            bounds,
            SurveyPurpose::Settlement,
        )]
    }

    /// Plan one authored blueprint after a successful survey. The player supplies
    /// the anchor; the component supplies the remembered core token and revision.
    fn prepare(
        &mut self,
        session: u64,
        blueprint: &str,
        x: &str,
        y: &str,
        z: &str,
    ) -> Vec<Command> {
        if !valid_resource_id(blueprint) {
            return vec![message_session(
                session,
                "Blueprint id must be a resource identifier of at most 128 bytes.",
            )];
        }
        let [Ok(x), Ok(y), Ok(z)] = [x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()] else {
            return vec![message_session(
                session,
                "Structure coordinates must be signed 32-bit integers.",
            )];
        };
        let Some(site) = &self.worker_site else {
            return vec![message_session(
                session,
                "List homes before planning a structure.",
            )];
        };
        let Some(survey_token) = &self.survey_token else {
            return vec![message_session(
                session,
                "Survey a site before planning a structure.",
            )];
        };
        self.submit(OperationIntent::Prepare {
            session,
            site_id: site.site_id.clone(),
            blueprint: blueprint.to_owned(),
            anchor: settlements::BlockPosition { x, y, z },
            rotation: 0,
            survey_token: survey_token.clone(),
            expected_site_revision: site.revision,
            preview: None,
        })
    }
    fn status(&mut self, session: u64, structure_id: &str) -> Vec<Command> {
        self.status_with_continuation(session, structure_id, StatusContinuation::Read)
    }
    fn status_for_funding(&mut self, session: u64, structure_id: &str) -> Vec<Command> {
        self.status_with_continuation(session, structure_id, StatusContinuation::Fund)
    }

    fn status_for_advance(&mut self, session: u64, structure_id: &str) -> Vec<Command> {
        self.status_with_continuation(session, structure_id, StatusContinuation::Advance)
    }

    fn status_with_continuation(
        &mut self,
        session: u64,
        structure_id: &str,
        continuation: StatusContinuation,
    ) -> Vec<Command> {
        if !valid_identity(structure_id) {
            return vec![message_session(
                session,
                "Structure id must be 1..64 bytes.",
            )];
        }
        let Some(request) = self.next_storage_request("settlement-status") else {
            return vec![message_session(
                session,
                "No further settlement operations can be issued.",
            )];
        };
        self.pending_status.insert(
            request.clone(),
            PendingStatus {
                session,
                continuation,
            },
        );
        vec![structure_status(&request, structure_id)]
    }

    fn continue_specialization(&mut self, pending: PendingSpecialization) -> Vec<Command> {
        if pending.next_specialty >= pending.specialties.len() {
            return self.persist_specialization(pending);
        }
        let specialty = &pending.specialties[pending.next_specialty];
        let definition =
            specialty_definition(specialty).expect("validated specialty survives its status query");
        let Some(candidate) = pending
            .workplace_candidates
            .get(pending.next_specialty)
            .and_then(|candidates| candidates.get(pending.next_candidate))
            .cloned()
        else {
            return vec![message_session(
                pending.session,
                format!("{specialty} needs a committed {}.", definition.blueprint_id),
            )];
        };
        // Certificates live in the core `buildings` ledger but share the
        // read-only status query: core answers the immutable snapshot for a
        // certificate id, so the same path validates function without a second
        // authority. Only `Committed`-equivalent acceptance persists: the
        // certificate is immutable, so its presence is the commit.
        let status_id = candidate.id;
        let Some(request) = self.next_storage_request("settlement-specialty-status") else {
            return vec![message_session(
                pending.session,
                "No further settlement operations can be issued.",
            )];
        };
        self.pending_status.insert(
            request.clone(),
            PendingStatus {
                session: pending.session,
                continuation: StatusContinuation::Specialize(pending),
            },
        );
        vec![structure_status(&request, &status_id)]
    }

    fn persist_specialization(&mut self, pending: PendingSpecialization) -> Vec<Command> {
        let PendingSpecialization {
            session,
            settlement,
            specialties,
            mut registry,
            version,
            ..
        } = pending;
        let Err(message) =
            record_settlement_specialties(&mut registry, &settlement, specialties.clone())
        else {
            let Ok(value) = serde_json::to_string(&registry) else {
                return vec![message_session(
                    session,
                    "Settlement records are unavailable.",
                )];
            };
            let Some(request) = self.next_storage_request("settlement-registry-specialize") else {
                return vec![message_session(
                    session,
                    "Settlement records are unavailable.",
                )];
            };
            let selected = if specialties.is_empty() {
                "DONE".to_owned()
            } else {
                specialties.join("+")
            };
            self.pending_registry_write = Some(PendingRegistryWrite {
                request: request.clone(),
                session,
                message: format!("{settlement} specializations: {selected}."),
                resident: None,
            });
            return vec![storage_cas(
                &request,
                SETTLEMENT_REGISTRY_KEY,
                version,
                value,
            )];
        };
        vec![message_session(session, message)]
    }

    /// Hold the complete plan of the latest prepared structure against the
    /// bound warehouse instead of the caller's inventory.
    fn fund_warehouse(&mut self, session: u64) -> Vec<Command> {
        let Some(warehouse) = self.warehouse.as_ref() else {
            return vec![message_session(
                session,
                "Bind a warehouse before funding it.",
            )];
        };
        self.fund_from_inventory(
            session,
            InventoryEndpoint::Warehouse(warehouse.handle.clone()),
        )
    }

    fn fund_from_inventory(&mut self, session: u64, endpoint: InventoryEndpoint) -> Vec<Command> {
        let Some(structure) = self.last_structure.as_ref() else {
            return vec![message_session(
                session,
                "Prepare or read a structure before funding it.",
            )];
        };
        let structure_id = structure.structure_id.clone();
        let plan = plan_of(structure);
        if self.pending_funding.is_some() {
            return vec![message_session(
                session,
                "A funding query is already pending.",
            )];
        }
        let Some(request) = self.next_storage_request("settlement-funding") else {
            return vec![message_session(
                session,
                "No further settlement operations can be issued.",
            )];
        };
        self.pending_funding = Some(PendingFunding {
            request: request.clone(),
            session,
            structure_id,
            endpoint: endpoint.clone(),
            plan,
        });
        vec![query_owned_inventory(&request, endpoint, None)]
    }

    /// Commit the next bounded portion using the last snapshot and the durable
    /// reservation that funded its complete core-authored material plan.
    fn advance(&mut self, session: u64) -> Vec<Command> {
        let Some(structure) = self.last_structure.as_ref() else {
            return vec![message_session(
                session,
                "Prepare or read a structure before advancing it.",
            )];
        };
        let Some(reservation_ref) = self.reservation_ref.as_ref() else {
            return vec![message_session(
                session,
                "Fund the structure before advancing it.",
            )];
        };
        if self.reservation_structure_id.as_deref() != Some(structure.structure_id.as_str()) {
            return vec![message_session(
                session,
                "Fund the current structure before advancing it.",
            )];
        }
        let Some(stage) = structure.stages.get(self.stage_index) else {
            return vec![message_session(session, "The structure has no next stage.")];
        };
        self.submit(OperationIntent::Advance {
            session,
            structure_id: structure.structure_id.clone(),
            stage: stage.stage.clone(),
            reservation_ref: reservation_ref.clone(),
            expected_revision: structure.revision,
            work_units: stage
                .work_units
                .saturating_sub(self.stage_work_done)
                .min(STRUCTURE_PORTION_WORK_UNITS),
        })
    }

    /// Reserve one remembered free home and materialise the resident it is
    /// waiting for: two durable operations, the second submitted from the
    /// first's answer because it consumes the token the first minted.
    fn spawn_worker(&mut self, session: u64, poi_id: &str) -> Vec<Command> {
        if !valid_identity(poi_id) {
            return vec![message_session(
                session,
                "Point-of-interest id must be 1..64 bytes.",
            )];
        }
        let Some(site) = &self.worker_site else {
            log(LogLevel::Info, &format!("{MARKER} spawn-refused no-homes"));
            return vec![message_session(
                session,
                "List homes before spawning a worker.",
            )];
        };
        if !site.homes.iter().any(|home| home.poi_id == poi_id) {
            log(
                LogLevel::Info,
                &format!("{MARKER} spawn-refused unknown-poi"),
            );
            return vec![message_session(
                session,
                "Spawn names a home the last listing did not hold.",
            )];
        }
        self.submit(OperationIntent::ReserveSite {
            session,
            site_id: site.site_id.clone(),
            poi_id: poi_id.to_owned(),
            expected_revision: site.revision,
        })
    }

    /// Give one resident a bounded craft order at a one-cell crafting-table
    /// station: the same durable work assignment a haul takes, with the craft
    /// work order the engine executes against the loaded world.
    fn craft(
        &mut self,
        session: u64,
        resident: &str,
        expected_revision: &str,
        recipe: &str,
        count: &str,
        x: &str,
        y: &str,
        z: &str,
    ) -> Vec<Command> {
        if !valid_identity(resident) {
            return vec![message_session(
                session,
                "Resident handle must be 1..64 bytes.",
            )];
        }
        let Some(expected_revision) = expected_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Expected revision must be an unsigned 53-bit integer.",
            )];
        };
        if !valid_resource_id(recipe) {
            return vec![message_session(
                session,
                "Recipe id must be a resource identifier of at most 128 bytes.",
            )];
        }
        let Some(count) = count.parse::<u32>().ok().filter(|count| *count >= 1) else {
            return vec![message_session(session, "Count must be a non-zero u32.")];
        };
        let [Ok(x), Ok(y), Ok(z)] = [x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()] else {
            return vec![message_session(
                session,
                "Station coordinates must be signed 32-bit integers.",
            )];
        };
        let station = residents::BlockPosition { x, y, z };
        self.submit(OperationIntent::Craft {
            session,
            resident: resident.to_owned(),
            expected_revision,
            recipe: recipe.to_owned(),
            count,
            station: WorkArea {
                dimension: STATION_DIMENSION.to_owned(),
                min: station,
                max: station,
            },
        })
    }

    /// Give one resident a bounded harvest order. The engine verifies the tool,
    /// crops, and loaded cells, then puts the harvested items in resident cargo
    /// for the existing `haul` command to deposit into the warehouse.
    #[allow(clippy::too_many_arguments)]
    fn harvest(
        &mut self,
        session: u64,
        resident: &str,
        expected_revision: &str,
        tool: &str,
        min_x: &str,
        min_y: &str,
        min_z: &str,
        max_x: &str,
        max_y: &str,
        max_z: &str,
    ) -> Vec<Command> {
        if !valid_identity(resident) {
            return vec![message_session(
                session,
                "Resident handle must be 1..64 bytes.",
            )];
        }
        let Some(expected_revision) = expected_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision <= MAX_SAFE_REVISION)
        else {
            return vec![message_session(
                session,
                "Expected revision must be an unsigned 53-bit integer.",
            )];
        };
        if !valid_resource_id(tool) {
            return vec![message_session(
                session,
                "Tool must be a resource identifier of at most 128 bytes.",
            )];
        }
        let [Ok(min_x), Ok(min_y), Ok(min_z), Ok(max_x), Ok(max_y), Ok(max_z)] = [
            min_x.parse::<i32>(),
            min_y.parse::<i32>(),
            min_z.parse::<i32>(),
            max_x.parse::<i32>(),
            max_y.parse::<i32>(),
            max_z.parse::<i32>(),
        ] else {
            return vec![message_session(
                session,
                "Harvest coordinates must be signed 32-bit integers.",
            )];
        };
        if min_x > max_x
            || min_y > max_y
            || min_z > max_z
            || max_x - min_x >= MAX_WORK_AREA_AXIS
            || max_y - min_y >= MAX_WORK_AREA_AXIS
            || max_z - min_z >= MAX_WORK_AREA_AXIS
        {
            return vec![message_session(
                session,
                "Harvest area must be ordered and no wider than 16 blocks on each axis.",
            )];
        }
        self.submit(OperationIntent::Harvest {
            session,
            resident: resident.to_owned(),
            expected_revision,
            tool: tool.to_owned(),
            area: WorkArea {
                dimension: STATION_DIMENSION.to_owned(),
                min: residents::BlockPosition {
                    x: min_x,
                    y: min_y,
                    z: min_z,
                },
                max: residents::BlockPosition {
                    x: max_x,
                    y: max_y,
                    z: max_z,
                },
            },
        })
    }

    fn submit(&mut self, intent: OperationIntent) -> Vec<Command> {
        self.enqueue_operation(intent, None, None)
    }

    fn submit_project(
        &mut self,
        intent: OperationIntent,
        project: PendingSettlementProject,
    ) -> Vec<Command> {
        self.enqueue_operation(intent, Some(project), None)
    }

    fn submit_population(
        &mut self,
        intent: OperationIntent,
        population: PendingSettlementPopulation,
    ) -> Vec<Command> {
        self.enqueue_operation(intent, None, Some(population))
    }

    fn enqueue_operation(
        &mut self,
        intent: OperationIntent,
        project: Option<PendingSettlementProject>,
        population: Option<PendingSettlementPopulation>,
    ) -> Vec<Command> {
        let session = intent.session();
        self.queued.push_back(QueuedOperation {
            intent,
            project,
            population,
        });
        if self.counter.is_some() {
            self.reserve_next_operation()
        } else {
            self.load_counter(Some(session))
        }
    }

    fn load_counter(&mut self, session: Option<u64>) -> Vec<Command> {
        if self.counter_read_request.is_some() {
            return Vec::new();
        }
        let Some(request) = self.next_storage_request("settlement-counter-read") else {
            return self.fail_queued("No further settlement operations can be issued.");
        };
        self.counter_read_request = Some(request.clone());
        self.counter_read_session = session;
        vec![storage_get(&request, OPERATION_COUNTER_KEY)]
    }

    fn reserve_next_operation(&mut self) -> Vec<Command> {
        if self.counter_read_request.is_some() || self.pending_counter.is_some() {
            return Vec::new();
        }
        let Some(queued) = self.queued.pop_front() else {
            return Vec::new();
        };
        let Some(next) = self.counter.and_then(|counter| counter.checked_add(1)) else {
            return vec![message_session(
                queued.intent.session(),
                "No further settlement operations can be issued.",
            )];
        };
        let Some(request) = self.next_storage_request("settlement-counter-write") else {
            self.queued.push_front(queued);
            return self.fail_queued("No further settlement operations can be issued.");
        };
        self.pending_counter = Some(PendingCounter {
            request: request.clone(),
            next,
            intent: queued.intent,
            project: queued.project,
            population: queued.population,
        });
        vec![storage_cas(
            &request,
            OPERATION_COUNTER_KEY,
            self.counter_version,
            next.to_string(),
        )]
    }

    fn storage_get(&mut self, answered: &StorageGetAnswered) -> Vec<Command> {
        if let Some(commands) = self.hire_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.dismiss_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.capture_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.evacuation_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.combat_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.squad_get_answer(answered) {
            return commands;
        }
        if self
            .pending_registry_read
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self
                .pending_registry_read
                .take()
                .expect("matched registry read is present");
            return self.registry_get_answer(pending, answered);
        }
        if self.counter_read_request.as_deref() != Some(answered.request.as_str()) {
            return Vec::new();
        }
        self.counter_read_request = None;
        let session = self.counter_read_session.take();
        let StorageGetOutcome::Read(record) = &answered.outcome else {
            return self.counter_load_failed(session);
        };
        let Some(counter) = record.value.as_deref().unwrap_or("0").parse::<u64>().ok() else {
            return self.counter_load_failed(session);
        };
        self.counter = Some(counter);
        self.counter_version = record.version;
        self.reserve_next_operation()
    }

    fn storage_cas(&mut self, answered: &StorageCasAnswered) -> Vec<Command> {
        if let Some(commands) = self.hire_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.dismiss_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.capture_cas_answer(answered) {
            return commands;
        }
        if self
            .pending_registry_write
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self
                .pending_registry_write
                .take()
                .expect("matched registry write is present");
            return match answered.outcome {
                StorageCasOutcome::Committed(_) => {
                    let mut commands = vec![message_session(pending.session, pending.message)];
                    if let Some((request, settlement, target)) = self.pending_view_role.take() {
                        if request == answered.request {
                            commands.extend(self.view_role_changed(&settlement, &target));
                        }
                    }
                    commands
                }
                StorageCasOutcome::Refused | StorageCasOutcome::Failed(_) => {
                    self.pending_view_role = None;
                    let Some(resident) = pending.resident else {
                        return vec![message_session(
                            pending.session,
                            "Settlement records were not saved; inspect the settlement before retrying.",
                        )];
                    };
                    self.begin_registry_read(RegistryRead::PersistResident {
                        session: pending.session,
                        resident,
                    })
                }
            };
        }
        let Some(pending) = self.pending_counter.take() else {
            return Vec::new();
        };
        if pending.request != answered.request {
            self.pending_counter = Some(pending);
            return Vec::new();
        }
        let StorageCasOutcome::Committed(version) = &answered.outcome else {
            let session = pending.intent.session();
            self.queued.push_front(QueuedOperation {
                intent: pending.intent,
                project: pending.project,
                population: pending.population,
            });
            self.counter = None;
            self.counter_version = None;
            return self.load_counter(Some(session));
        };
        self.counter = Some(pending.next);
        self.counter_version = Some(*version);
        let mut commands = self.issue_with_context(
            pending.intent,
            pending.project,
            pending.population,
            pending.next,
        );
        commands.extend(self.reserve_next_operation());
        commands
    }

    #[cfg(test)]
    fn issue(&mut self, intent: OperationIntent, counter: u64) -> Vec<Command> {
        self.issue_with_context(intent, None, None, counter)
    }

    fn issue_with_context(
        &mut self,
        intent: OperationIntent,
        project: Option<PendingSettlementProject>,
        population: Option<PendingSettlementPopulation>,
        counter: u64,
    ) -> Vec<Command> {
        let request = format!("settlement-request-{counter}");
        let operation_id = format!("settlement-operation-{counter}");
        match intent {
            OperationIntent::Bind {
                session,
                structure_id,
                container_id,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Bind {
                            structure_id: structure_id.clone(),
                            container_id,
                        },
                    },
                );
                vec![bind_warehouse(
                    &request,
                    &operation_id,
                    &structure_id,
                    container_id,
                )]
            }
            OperationIntent::BindVillage {
                session,
                site_id,
                container_id,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::BindVillage {
                            site_id: site_id.clone(),
                            container_id,
                        },
                    },
                );
                vec![bind_village_warehouse(
                    &request,
                    &operation_id,
                    &site_id,
                    container_id,
                )]
            }
            OperationIntent::Recognize {
                session,
                settlement,
                building_name,
                purpose,
                footprint,
                points,
                survey_token,
                expected_world_revision,
                registry,
                version,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Recognize {
                            settlement: settlement.clone(),
                            building_name: building_name.clone(),
                            purpose: purpose.clone(),
                            registry: registry.clone(),
                            version,
                        },
                    },
                );
                vec![recognize_building(
                    &request,
                    &operation_id,
                    settlements::BuildingCertificateRequest {
                        footprint: footprint.clone(),
                        purpose: purpose.clone(),
                        interaction_points: points.clone(),
                        survey_token: survey_token.clone(),
                        expected_world_revision,
                    },
                )]
            }
            OperationIntent::BindManual {
                session,
                survey_token,
                position,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::BindManual {
                            position: position.clone(),
                        },
                    },
                );
                vec![bind_manual_warehouse(
                    &request,
                    &operation_id,
                    &survey_token,
                    position,
                )]
            }
            OperationIntent::Haul {
                session,
                resident,
                warehouse,
                expected_revision,
                direction,
                item,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Haul {
                            resident: resident.clone(),
                        },
                    },
                );
                let (source, destination) = match direction {
                    HaulDirection::Deposit => (
                        InventoryEndpoint::ResidentCarry(resident.clone()),
                        InventoryEndpoint::Warehouse(warehouse),
                    ),
                    HaulDirection::Issue(destination) => (
                        InventoryEndpoint::Warehouse(warehouse),
                        destination.endpoint(resident.clone()),
                    ),
                };
                vec![assign_resident_work(
                    &request,
                    &operation_id,
                    &resident,
                    WorkOrder::Haul(HaulWork {
                        source,
                        destination,
                        item,
                    }),
                    WORK_UNITS,
                    expected_revision,
                )]
            }
            OperationIntent::Craft {
                session,
                resident,
                expected_revision,
                recipe,
                count,
                station,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Craft {
                            resident: resident.clone(),
                        },
                    },
                );
                vec![assign_resident_work(
                    &request,
                    &operation_id,
                    &resident,
                    WorkOrder::Craft(CraftWork {
                        recipe,
                        count,
                        station,
                    }),
                    WORK_UNITS,
                    expected_revision,
                )]
            }
            OperationIntent::Harvest {
                session,
                resident,
                expected_revision,
                tool,
                area,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Harvest {
                            resident: resident.clone(),
                        },
                    },
                );
                vec![assign_resident_work(
                    &request,
                    &operation_id,
                    &resident,
                    WorkOrder::Harvest(AreaToolWork { area, tool }),
                    WORK_UNITS,
                    expected_revision,
                )]
            }
            OperationIntent::Prepare {
                session,
                site_id,
                blueprint,
                anchor,
                rotation,
                survey_token,
                expected_site_revision,
                preview,
            } => {
                if let Some(origin) = &preview {
                    if blueprint != view::GATE_BLUEPRINT
                        || !self.views.get(&session).is_some_and(|state| {
                            state.player == origin.player
                                && state.settlement == origin.settlement
                                && state
                                    .instance
                                    .as_ref()
                                    .is_some_and(|(id, _)| id == &origin.instance)
                        })
                    {
                        return Vec::new();
                    }
                }
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Prepare { project },
                    },
                );
                vec![prepare_structure(
                    &request,
                    &operation_id,
                    &site_id,
                    &blueprint,
                    anchor,
                    rotation,
                    &survey_token,
                    expected_site_revision,
                    preview.as_ref().map(|_| view::GATE_BLUEPRINT_HASH),
                )]
            }
            OperationIntent::ReserveMaterials {
                session,
                structure_id,
                endpoint,
                plan,
                fence,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::ReserveMaterials { structure_id },
                    },
                );
                vec![reserve_inventory_items(
                    &request,
                    &operation_id,
                    endpoint,
                    plan,
                    fence,
                )]
            }
            OperationIntent::Advance {
                session,
                structure_id,
                stage,
                reservation_ref,
                expected_revision,
                work_units,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Advance,
                    },
                );
                vec![advance_structure(
                    &request,
                    &operation_id,
                    &structure_id,
                    &stage,
                    &reservation_ref,
                    expected_revision,
                    work_units,
                )]
            }
            OperationIntent::ReserveSite {
                session,
                site_id,
                poi_id,
                expected_revision,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::ReserveSite { population },
                    },
                );
                vec![reserve_resident_site(
                    &request,
                    &operation_id,
                    &site_id,
                    &poi_id,
                    expected_revision,
                )]
            }
            OperationIntent::SpawnResident { session, token } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::SpawnResident {
                            token: token.clone(),
                            population,
                        },
                    },
                );
                vec![spawn_resident(
                    &request,
                    &operation_id,
                    &token,
                    ResidentKind::Villager,
                )]
            }
            OperationIntent::ReleaseSite { session, token } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::ReleaseSite,
                    },
                );
                vec![release_resident_site(&request, &operation_id, &token)]
            }
            OperationIntent::Treatment {
                session,
                handle,
                warehouse,
                expected_revision,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::Treatment {
                            handle: handle.clone(),
                        },
                    },
                );
                vec![solaris_plugin_sdk::treat_resident(
                    &request,
                    &operation_id,
                    &handle,
                    expected_revision,
                    InventoryEndpoint::Warehouse(warehouse),
                    InventoryMaterial {
                        resource_id: "minecraft:bread".to_owned(),
                        quantity: 1,
                    },
                    3_000,
                )]
            }
            OperationIntent::Combat {
                session,
                resident,
                allies,
                expected_revision,
                phase,
            } => self.issue_combat(
                &request,
                &operation_id,
                session,
                resident,
                allies,
                expected_revision,
                phase,
            ),
            OperationIntent::Squad {
                session,
                handles,
                revisions,
                action,
            } => self.issue_squad(&request, &operation_id, session, handles, revisions, action),
        }
    }

    fn answered(&mut self, answered: &OperationAnswered) -> Vec<Command> {
        if let Some(commands) = self.hire_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.dismiss_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.capture_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.evacuation_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.view_stock_answer(answered) {
            return commands;
        }
        if self
            .pending_registry_survey
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self
                .pending_registry_survey
                .take()
                .expect("matched registry survey is present");
            return self.registry_survey_answer(pending, answered);
        }
        if self
            .pending_registry_sites
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self
                .pending_registry_sites
                .take()
                .expect("matched registry site query is present");
            return self.registry_sites_answer(pending, answered);
        }
        if self
            .pending_sites
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self.pending_sites.take().expect("the pending sites query");
            return self.sites_answer(pending, answered);
        }
        if self
            .pending_survey
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self.pending_survey.take().expect("the pending site survey");
            return self.survey_answer(pending, answered);
        }
        if self.pending_status.contains_key(&answered.request) {
            if answered.operation_id.is_some() {
                return Vec::new();
            }
            let pending = self
                .pending_status
                .remove(&answered.request)
                .expect("the pending status query");
            return self.status_answer(pending, &answered.outcome);
        }
        if self
            .pending_funding
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            let pending = self
                .pending_funding
                .take()
                .expect("the pending funding query");
            return self.funding_answer(pending, &answered.outcome);
        }

        let Some(pending) = self.pending.get(&answered.request).cloned() else {
            return Vec::new();
        };
        let expected_operation_id = if matches!(&pending.kind, PendingKind::TreatmentQuery { .. }) {
            None
        } else {
            Some(pending.operation_id.as_str())
        };
        if answered.operation_id.as_deref() != expected_operation_id {
            return Vec::new();
        }
        self.pending.remove(&answered.request);
        match pending.kind {
            PendingKind::Bind {
                structure_id,
                container_id,
            } => self.bind_answer(
                pending.session,
                &structure_id,
                container_id,
                &answered.outcome,
            ),
            PendingKind::BindVillage {
                site_id,
                container_id,
            } => {
                self.bind_village_answer(pending.session, &site_id, container_id, &answered.outcome)
            }
            PendingKind::BindManual { position } => {
                self.bind_manual_answer(pending.session, &position, &answered.outcome)
            }
            PendingKind::Recognize {
                settlement,
                building_name,
                purpose,
                registry,
                version,
            } => self.recognize_answer(
                pending.session,
                &settlement,
                &building_name,
                &purpose,
                registry,
                version,
                &answered.outcome,
            ),
            PendingKind::Haul { resident } => {
                self.haul_answer(pending.session, &resident, &answered.outcome)
            }
            PendingKind::Craft { resident } => {
                self.craft_answer(pending.session, &resident, &answered.outcome)
            }
            PendingKind::Harvest { resident } => {
                self.harvest_answer(pending.session, &resident, &answered.outcome)
            }
            PendingKind::Prepare { project } => {
                self.prepare_answer(pending.session, project, &answered.outcome)
            }
            PendingKind::ReserveSite { population } => {
                self.reserve_answer(pending.session, population, &answered.outcome)
            }
            PendingKind::ReserveMaterials { structure_id } => {
                self.funding_reservation_answer(pending.session, &structure_id, &answered.outcome)
            }
            PendingKind::Advance => self.advance_answer(pending.session, &answered.outcome),
            PendingKind::SpawnResident { token, population } => {
                self.spawn_answer(pending.session, token, population, &answered.outcome)
            }
            PendingKind::ReleaseSite => self.release_answer(pending.session, &answered.outcome),
            PendingKind::TreatmentQuery { handle, warehouse } => {
                self.treatment_query_answer(pending.session, handle, warehouse, &answered.outcome)
            }
            PendingKind::Treatment { handle } => {
                self.treatment_answer(pending.session, &handle, &answered.outcome)
            }
            PendingKind::Combat {
                resident,
                allies,
                phase,
            } => self.combat_answer(pending.session, resident, allies, phase, &answered.outcome),
            PendingKind::Squad { handles, action } => {
                self.squad_answer(pending.session, handles, action, &answered.outcome)
            }
        }
    }
    fn funding_answer(
        &mut self,
        pending: PendingFunding,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            return vec![message_session(
                pending.session,
                "Funding inventory could not be read.",
            )];
        };
        let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
            &committed.payload
        else {
            return vec![message_session(
                pending.session,
                "Funding inventory returned an unexpected result.",
            )];
        };
        if !same_inventory_endpoint(&snapshot.endpoint, &pending.endpoint) {
            return vec![message_session(
                pending.session,
                "Funding inventory returned an unexpected endpoint.",
            )];
        }
        self.submit(OperationIntent::ReserveMaterials {
            session: pending.session,
            structure_id: pending.structure_id,
            endpoint: snapshot.endpoint.clone(),
            plan: pending.plan,
            fence: snapshot.fence.clone(),
        })
    }

    /// One answer to the homes listing: remember the first site that still holds
    /// a free home and report every candidate it keeps, so a driver can pick one
    /// and a spawn can name it.
    fn sites_answer(
        &mut self,
        pending: PendingSites,
        answered: &OperationAnswered,
    ) -> Vec<Command> {
        let session = pending.session;
        let OperationOutcome::Committed(committed) = &answered.outcome else {
            let reason = match &answered.outcome {
                OperationOutcome::Refused(refused) => failure_name(refused.reason),
                _ => "unknown",
            };
            log(LogLevel::Info, &format!("{MARKER} homes-refused {reason}"));
            return vec![message_session(session, "The sites listing was refused.")];
        };
        let OperationPayload::Settlement(SettlementResult::Sites(page)) = &committed.payload else {
            log(LogLevel::Info, &format!("{MARKER} homes-unexpected"));
            return vec![message_session(
                session,
                "The sites listing returned an unexpected result.",
            )];
        };
        let Some(site) = page
            .sites
            .iter()
            .find(|site| free_homes(site).next().is_some())
        else {
            self.worker_site = None;
            log(LogLevel::Info, &format!("{MARKER} homes none"));
            return vec![message_session(
                session,
                "No listed site still holds a free home.",
            )];
        };
        let homes = free_homes(site)
            .map(|(poi_id, at)| WorkerHome { poi_id, at })
            .collect::<Vec<_>>();
        let home_count = homes.len();
        let listed = homes
            .iter()
            .map(|home| {
                format!(
                    "home={}@{},{},{}",
                    home.poi_id, home.at.x, home.at.y, home.at.z
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.worker_site = Some(WorkerSite {
            site_id: site.site_id.clone(),
            origin: site.footprint_origin.clone(),
            revision: site.revision,
            homes,
        });
        log(
            LogLevel::Info,
            &format!(
                "{MARKER} homes site={} origin={},{},{} revision={} {listed}",
                site.site_id,
                site.footprint_origin.x,
                site.footprint_origin.y,
                site.footprint_origin.z,
                site.revision,
            ),
        );
        vec![message_session(
            session,
            format!(
                "Site {} holds {} free homes: {listed}.",
                site.site_id, home_count
            ),
        )]
    }

    fn survey_answer(
        &mut self,
        pending: PendingSurvey,
        answered: &OperationAnswered,
    ) -> Vec<Command> {
        match &answered.outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Survey(survey)) =
                    &committed.payload
                else {
                    log(LogLevel::Info, &format!("{MARKER} survey-unexpected"));
                    return vec![message_session(
                        pending.session,
                        "The site survey returned an unexpected result.",
                    )];
                };
                self.survey_token = Some(survey.survey_token.clone());
                self.survey_bounds = Some(survey.bounds.clone());
                self.survey_world_revision = Some(survey.world_revision);
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} survey token={} revision={}",
                        survey.survey_token, survey.revision
                    ),
                );
                vec![message_session(pending.session, "Site surveyed.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} survey-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    pending.session,
                    format!("Site survey refused: {:?}.", refused.reason),
                )]
            }
        }
    }
    fn prepare_answer(
        &mut self,
        session: u64,
        project: Option<PendingSettlementProject>,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Structure(structure)) =
                    &committed.payload
                else {
                    log(LogLevel::Info, &format!("{MARKER} prepare-unexpected"));
                    return vec![message_session(
                        session,
                        "Structure planning returned an unexpected result.",
                    )];
                };
                self.last_structure = Some(structure.clone());
                self.stage_index = structure.current_stage_index as usize;
                self.stage_work_done = structure.completed_work_units;
                self.reservation_ref = None;
                self.reservation_structure_id = None;
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} prepare structure={} revision={}",
                        structure.structure_id, structure.revision
                    ),
                );
                let Some(mut project) = project else {
                    return vec![message_session(session, "Structure prepared.")];
                };
                let Err(message) = record_settlement_project(
                    &mut project.registry,
                    &project.settlement,
                    &project.building_name,
                    &structure.structure_id,
                    &structure.blueprint_id,
                    structure.revision,
                ) else {
                    let Ok(value) = serde_json::to_string(&project.registry) else {
                        return vec![message_session(
                            session,
                            "Structure prepared, but settlement records are unavailable.",
                        )];
                    };
                    let Some(request) = self.next_storage_request("settlement-registry-project")
                    else {
                        return vec![message_session(
                            session,
                            "Structure prepared, but settlement records are unavailable.",
                        )];
                    };
                    self.pending_registry_write = Some(PendingRegistryWrite {
                        request: request.clone(),
                        session,
                        message: format!(
                            "Projected {} for {}. Structure {}.",
                            project.building_name, project.settlement, structure.structure_id
                        ),
                        resident: None,
                    });
                    return vec![storage_cas(
                        &request,
                        SETTLEMENT_REGISTRY_KEY,
                        project.version,
                        value,
                    )];
                };
                vec![message_session(session, message)]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} prepare-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Structure planning refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn apply_receipt(&mut self, receipt: &settlements::StructureReceipt) {
        let is_current = self
            .last_structure
            .as_ref()
            .is_some_and(|structure| structure.structure_id == receipt.structure_id);
        if !is_current {
            return;
        }
        if let Some(structure) = self.last_structure.as_mut() {
            structure.revision = receipt.revision;
        }
        if self
            .last_structure
            .as_ref()
            .and_then(|structure| structure.stages.get(self.stage_index))
            .is_some_and(|stage| stage.stage == receipt.stage)
        {
            self.stage_work_done = self.stage_work_done.saturating_add(receipt.work_units);
            if self
                .last_structure
                .as_ref()
                .and_then(|structure| structure.stages.get(self.stage_index))
                .is_some_and(|stage| self.stage_work_done >= stage.work_units)
            {
                self.stage_index = self.stage_index.saturating_add(1);
                self.stage_work_done = 0;
            }
        }
    }

    fn advance_answer(&mut self, session: u64, outcome: &OperationOutcome) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => match &committed.payload {
                OperationPayload::Settlement(SettlementResult::Receipt(receipt)) => {
                    self.apply_receipt(receipt);
                    log(
                        LogLevel::Info,
                        &format!(
                            "{MARKER} advance structure={} stage={} sequence={} revision={}",
                            receipt.structure_id, receipt.stage, receipt.sequence, receipt.revision,
                        ),
                    );
                    let mut commands =
                        vec![message_session(session, "Structure portion committed.")];
                    if receipt.block_count > 0 {
                        commands.push(play_client_sound(
                            session,
                            "solaris-settlements:construction_portion",
                            0.4,
                            1.0,
                            None,
                        ));
                    }
                    commands
                }
                OperationPayload::Settlement(SettlementResult::Structure(structure)) => {
                    self.last_structure = Some(structure.clone());
                    log(
                        LogLevel::Info,
                        &format!(
                            "{MARKER} advance-paused structure={} state={} reason={} revision={}",
                            structure.structure_id,
                            structure_state_name(structure.state),
                            structure.pause_reason.as_deref().unwrap_or("none"),
                            structure.revision,
                        ),
                    );
                    vec![message_session(session, "Structure advance paused.")]
                }
                _ => {
                    log(LogLevel::Info, &format!("{MARKER} advance-unexpected"));
                    vec![message_session(
                        session,
                        "Structure advance returned an unexpected result.",
                    )]
                }
            },
            OperationOutcome::Refused(refused) => {
                let recovered_revision =
                    if matches!(refused.reason, OperationFailure::StaleRevision) {
                        if let OperationPayload::Settlement(SettlementResult::Structure(
                            structure,
                        )) = &refused.payload
                        {
                            self.apply_status(structure);
                            Some(structure.revision)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                log(
                    LogLevel::Info,
                    &match recovered_revision {
                        Some(revision) => {
                            format!("{MARKER} advance-refused stale-revision revision={revision}")
                        }
                        None => {
                            format!("{MARKER} advance-refused {}", failure_name(refused.reason))
                        }
                    },
                );
                vec![message_session(
                    session,
                    format!("Structure advance refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn funding_reservation_answer(
        &mut self,
        session: u64,
        structure_id: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::OwnedInventory(InventoryResult::Reservation(reservation)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Funding reservation returned an unexpected result.",
                    )];
                };
                self.reservation_ref = Some(reservation.reservation_ref.clone());
                self.reservation_structure_id = Some(structure_id.to_owned());
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} funding reservation={}",
                        reservation.reservation_ref
                    ),
                );
                vec![message_session(session, "Structure materials reserved.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} funding-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Funding reservation refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn apply_status(&mut self, structure: &StructureSnapshot) {
        let keeps_local_reservation =
            self.reservation_structure_id.as_deref() == Some(structure.structure_id.as_str());
        self.last_structure = Some(structure.clone());
        self.stage_index = structure.current_stage_index as usize;
        self.stage_work_done = structure.completed_work_units;
        if !keeps_local_reservation {
            self.reservation_ref = structure.reservation_ref.clone();
            self.reservation_structure_id = structure
                .reservation_ref
                .as_ref()
                .map(|_| structure.structure_id.clone());
        }
    }

    fn status_answer(
        &mut self,
        pending: PendingStatus,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                // `Status` answers whichever durable record the id names: a
                // structure snapshot for built workplaces, the immutable
                // certificate for recognized ones. Only a committed structure
                // or a present certificate satisfies the specialty.
                let specialty_outcome = match &committed.payload {
                    OperationPayload::Settlement(SettlementResult::Structure(structure)) => {
                        log(
                            LogLevel::Info,
                            &format!(
                                "{MARKER} status structure={} state={} watermark={} revision={}",
                                structure.structure_id,
                                structure_state_name(structure.state),
                                structure.watermark,
                                structure.revision,
                            ),
                        );
                        Some((
                            structure.structure_id.clone(),
                            matches!(structure.state, StructureState::Committed),
                        ))
                    }
                    OperationPayload::Settlement(SettlementResult::Building(certificate)) => {
                        log(
                            LogLevel::Info,
                            &format!(
                                "{MARKER} status certificate={} revision={}",
                                certificate.certificate_id, certificate.revision,
                            ),
                        );
                        Some((certificate.certificate_id.clone(), true))
                    }
                    _ => None,
                };
                let Some((answered_id, accepted)) = specialty_outcome else {
                    log(LogLevel::Info, &format!("{MARKER} status-unexpected"));
                    return vec![message_session(
                        pending.session,
                        "Structure status returned an unexpected result.",
                    )];
                };
                match pending.continuation {
                    StatusContinuation::Specialize(mut specialization) => {
                        let Some(candidate) = specialization
                            .workplace_candidates
                            .get(specialization.next_specialty)
                            .and_then(|candidates| candidates.get(specialization.next_candidate))
                            .cloned()
                        else {
                            return vec![message_session(
                                specialization.session,
                                "Specialization status sequence is invalid.",
                            )];
                        };
                        // Fail closed on variant and purpose: a certificate must
                        // answer its own snapshot with the specialty's purpose,
                        // a structure its own snapshot; anything else is a
                        // sequence violation, never a pass.
                        let specialty = &specialization.specialties[specialization.next_specialty];
                        let definition = specialty_definition(specialty)
                            .expect("validated specialty survives its status query");
                        let accepted = match (&candidate.origin, &committed.payload) {
                            (
                                BuildingOrigin::Structure { structure_id },
                                OperationPayload::Settlement(SettlementResult::Structure(
                                    structure,
                                )),
                            ) => {
                                structure.structure_id == *structure_id
                                    && answered_id == candidate.id
                                    && accepted
                            }
                            (
                                BuildingOrigin::Certificate { certificate_id },
                                OperationPayload::Settlement(SettlementResult::Building(
                                    certificate,
                                )),
                            ) => {
                                certificate.certificate_id == *certificate_id
                                    && answered_id == candidate.id
                                    && certificate.purpose.as_str() == definition.blueprint_id
                            }
                            _ => false,
                        };
                        if !accepted && answered_id != candidate.id {
                            return vec![message_session(
                                specialization.session,
                                "Specialization status returned an unexpected structure.",
                            )];
                        }
                        if accepted {
                            specialization.next_specialty += 1;
                            specialization.next_candidate = 0;
                        } else {
                            specialization.next_candidate += 1;
                        }
                        self.continue_specialization(specialization)
                    }
                    StatusContinuation::Read => {
                        if let OperationPayload::Settlement(SettlementResult::Structure(
                            structure,
                        )) = &committed.payload
                        {
                            self.apply_status(structure);
                            return vec![message_session(
                                pending.session,
                                format!(
                                    "Structure {} state={} watermark={} stage={} progress={} origin={},{},{} size={},{},{}.",
                                    structure.structure_id,
                                    structure_state_name(structure.state),
                                    structure.watermark,
                                    structure.current_stage_index,
                                    structure.completed_work_units,
                                    structure.origin.x,
                                    structure.origin.y,
                                    structure.origin.z,
                                    structure.reserved_footprint.x,
                                    structure.reserved_footprint.y,
                                    structure.reserved_footprint.z,
                                ),
                            )];
                        }
                        vec![message_session(
                            pending.session,
                            "Structure status returned an unexpected result.",
                        )]
                    }
                    StatusContinuation::Fund => {
                        if let OperationPayload::Settlement(SettlementResult::Structure(
                            structure,
                        )) = &committed.payload
                        {
                            self.apply_status(structure);
                            return self.fund_warehouse(pending.session);
                        }
                        vec![message_session(
                            pending.session,
                            "Structure status returned an unexpected result.",
                        )]
                    }
                    StatusContinuation::Advance => {
                        if let OperationPayload::Settlement(SettlementResult::Structure(
                            structure,
                        )) = &committed.payload
                        {
                            self.apply_status(structure);
                            return self.advance(pending.session);
                        }
                        vec![message_session(
                            pending.session,
                            "Structure status returned an unexpected result.",
                        )]
                    }
                }
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} status-refused {}", failure_name(refused.reason)),
                );
                let session = pending.session;
                match pending.continuation {
                    StatusContinuation::Specialize(mut specialization) => {
                        specialization.next_candidate += 1;
                        self.continue_specialization(specialization)
                    }
                    _ => vec![message_session(
                        session,
                        format!("Structure status refused: {:?}.", refused.reason),
                    )],
                }
            }
        }
    }

    fn bind_answer(
        &mut self,
        session: u64,
        structure_id: &str,
        container_id: u32,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Warehouse(binding)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Warehouse binding returned an unexpected result.",
                    )];
                };
                if !matches!(
                    &binding.source,
                    WarehouseSource::Authored(source)
                        if source.structure_id.as_str() == structure_id
                            && source.container_id == container_id
                ) {
                    log(
                        LogLevel::Info,
                        &format!("{MARKER} warehouse-cross-kind handle={}", binding.handle),
                    );
                }
                self.warehouse = Some(binding.clone());
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse structure={structure_id} container={container_id} \
                         handle={} revision={}",
                        binding.handle, binding.revision
                    ),
                );
                vec![message_session(session, "Warehouse bound.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse-refused {}",
                        failure_name(refused.reason)
                    ),
                );
                vec![message_session(
                    session,
                    format!("Warehouse binding refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn recognize_answer(
        &mut self,
        session: u64,
        settlement: &str,
        building_name: &str,
        purpose: &str,
        mut registry: SettlementRegistry,
        version: Option<u64>,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Building(certificate)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Building recognition returned an unexpected result.",
                    )];
                };
                // The core certificate is immutable: a repeated footprint
                // answers the original purpose, never the new request. Persist
                // the returned purpose and refuse the aliased name, so the
                // plugin record never becomes a conflicting authority.
                if certificate.purpose.as_str() != purpose {
                    return vec![message_session(
                        session,
                        "Building purpose does not match the recognized certificate.",
                    )];
                }
                let storage_positions = certificate
                    .interaction_points
                    .iter()
                    .filter_map(|point| match point {
                        settlements::BuildingInteractionPoint::Storage(storage) => {
                            Some(StoredPosition {
                                x: storage.at.x,
                                y: storage.at.y,
                                z: storage.at.z,
                            })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let Err(message) = record_settlement_certificate(
                    &mut registry,
                    settlement,
                    building_name,
                    certificate.purpose.as_str(),
                    BuildingOrigin::Certificate {
                        certificate_id: certificate.certificate_id.clone(),
                    },
                    certificate.revision,
                    storage_positions,
                ) else {
                    let Ok(value) = serde_json::to_string(&registry) else {
                        return vec![message_session(
                            session,
                            "Building recognized, but settlement records are unavailable.",
                        )];
                    };
                    let Some(request) = self.next_storage_request("settlement-registry-building")
                    else {
                        return vec![message_session(
                            session,
                            "Building recognized, but settlement records are unavailable.",
                        )];
                    };
                    self.pending_registry_write = Some(PendingRegistryWrite {
                        request: request.clone(),
                        session,
                        message: format!(
                            "Recognized {} for {}. Certificate {}.",
                            building_name, settlement, certificate.certificate_id
                        ),
                        resident: None,
                    });
                    return vec![storage_cas(
                        &request,
                        SETTLEMENT_REGISTRY_KEY,
                        version,
                        value,
                    )];
                };
                vec![message_session(session, message)]
            }
            OperationOutcome::Refused(refused) => vec![message_session(
                session,
                format!("Building recognition refused: {:?}.", refused.reason),
            )],
        }
    }

    fn bind_manual_answer(
        &mut self,
        session: u64,
        position: &settlements::BlockPosition,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Warehouse(binding)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Warehouse binding returned an unexpected result.",
                    )];
                };
                if !matches!(
                    &binding.source,
                    WarehouseSource::Manual(source)
                        if source.position.x == position.x
                            && source.position.y == position.y
                            && source.position.z == position.z
                ) {
                    log(
                        LogLevel::Info,
                        &format!("{MARKER} warehouse-cross-kind handle={}", binding.handle),
                    );
                }
                self.warehouse = Some(binding.clone());
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse manual={},{},{} handle={} revision={}",
                        position.x, position.y, position.z, binding.handle, binding.revision
                    ),
                );
                vec![message_session(session, "Warehouse bound.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse-refused {}",
                        failure_name(refused.reason)
                    ),
                );
                vec![message_session(
                    session,
                    format!("Warehouse binding refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn bind_village_answer(
        &mut self,
        session: u64,
        site_id: &str,
        container_id: u32,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::Warehouse(binding)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Warehouse binding returned an unexpected result.",
                    )];
                };
                if !matches!(
                    &binding.source,
                    WarehouseSource::VanillaVillage(source)
                        if source.site_id.as_str() == site_id
                            && source.container_id == container_id
                ) {
                    log(
                        LogLevel::Info,
                        &format!("{MARKER} warehouse-cross-kind handle={}", binding.handle),
                    );
                }
                self.warehouse = Some(binding.clone());
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse village={site_id} container={container_id} \
                         handle={} revision={}",
                        binding.handle, binding.revision
                    ),
                );
                vec![message_session(session, "Warehouse bound.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} warehouse-refused {}",
                        failure_name(refused.reason)
                    ),
                );
                vec![message_session(
                    session,
                    format!("Warehouse binding refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn haul_answer(
        &mut self,
        session: u64,
        resident: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::ResidentOrder(ResidentOrderResult::Work(assignment)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Haul returned an unexpected result.",
                    )];
                };
                if assignment.handle.as_str() != resident {
                    return vec![message_session(
                        session,
                        "Haul returned an unexpected result.",
                    )];
                }
                self.resident_revisions
                    .insert(resident.to_owned(), assignment.revision);
                log(LogLevel::Info, &work_marker("haul", assignment));
                vec![message_session(
                    session,
                    format!("Haul assigned. Revision {}.", assignment.revision),
                )]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} haul-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Haul refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    /// One craft answer: the same work assignment a haul answers with, so the
    /// same receipt shape and the same revision the next order fences on.
    fn craft_answer(
        &mut self,
        session: u64,
        resident: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::ResidentOrder(ResidentOrderResult::Work(assignment)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Craft returned an unexpected result.",
                    )];
                };
                if assignment.handle.as_str() != resident {
                    return vec![message_session(
                        session,
                        "Craft returned an unexpected result.",
                    )];
                }
                self.resident_revisions
                    .insert(resident.to_owned(), assignment.revision);
                log(LogLevel::Info, &work_marker("craft", assignment));
                vec![message_session(
                    session,
                    format!("Craft assigned. Revision {}.", assignment.revision),
                )]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} craft-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Craft refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn harvest_answer(
        &mut self,
        session: u64,
        resident: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::ResidentOrder(ResidentOrderResult::Work(assignment)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Harvest returned an unexpected result.",
                    )];
                };
                if assignment.handle.as_str() != resident {
                    return vec![message_session(
                        session,
                        "Harvest returned an unexpected result.",
                    )];
                }
                self.resident_revisions
                    .insert(resident.to_owned(), assignment.revision);
                log(LogLevel::Info, &work_marker("harvest", assignment));
                vec![message_session(
                    session,
                    format!("Harvest assigned. Revision {}.", assignment.revision),
                )]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} harvest-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Harvest refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    /// One reservation answer: a commit submits the resident spawn the token is
    /// for as the next durable operation, and a refusal reports and stops.
    fn reserve_answer(
        &mut self,
        session: u64,
        population: Option<PendingSettlementPopulation>,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::ResidentSite(reservation)) =
                    &committed.payload
                else {
                    log(LogLevel::Info, &format!("{MARKER} reserve-unexpected"));
                    return vec![message_session(
                        session,
                        "The resident-site reservation returned an unexpected result.",
                    )];
                };
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} reserve poi={} token={} revision={}",
                        reservation.poi_id, reservation.spawn_site_token, reservation.revision
                    ),
                );
                let mut commands = vec![message_session(session, "Resident site reserved.")];
                let intent = OperationIntent::SpawnResident {
                    session,
                    token: reservation.spawn_site_token.clone(),
                };
                commands.extend(match population {
                    Some(population) => self.submit_population(intent, population),
                    None => self.submit(intent),
                });
                commands
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} reserve-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Resident site refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    /// One spawn answer: a commit keeps the handle and the revision every later
    /// order fences on, and a refusal hands the untouched reservation back so a
    /// later spawn can reserve again instead of finding it held.
    fn spawn_answer(
        &mut self,
        session: u64,
        token: String,
        population: Option<PendingSettlementPopulation>,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Resident(ResidentResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    log(LogLevel::Info, &format!("{MARKER} worker-unexpected"));
                    return vec![message_session(
                        session,
                        "The spawn returned an unexpected result.",
                    )];
                };
                self.resident_revisions.insert(snapshot.handle.clone(), 0);
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} worker handle={} snapshot_revision={} work_revision=0 lifecycle={}",
                        snapshot.handle,
                        snapshot.revision,
                        lifecycle_name(snapshot.lifecycle)
                    ),
                );
                let Some(mut population) = population else {
                    return vec![message_session(
                        session,
                        format!(
                            "Worker spawned. Handle {}. Work revision 0.",
                            snapshot.handle
                        ),
                    )];
                };
                let Err(message) = record_settlement_resident(
                    &mut population.registry,
                    &population.settlement,
                    &population.resident_name,
                    &snapshot.handle,
                    0,
                ) else {
                    let Ok(value) = serde_json::to_string(&population.registry) else {
                        return vec![message_session(
                            session,
                            "Worker spawned, but settlement records are unavailable.",
                        )];
                    };
                    let Some(request) = self.next_storage_request("settlement-registry-populate")
                    else {
                        return vec![message_session(
                            session,
                            "Worker spawned, but settlement records are unavailable.",
                        )];
                    };
                    self.pending_registry_write = Some(PendingRegistryWrite {
                        request: request.clone(),
                        session,
                        message: format!(
                            "Settled {} in {}.",
                            population.resident_name, population.settlement
                        ),
                        resident: Some(PendingResidentPersistence {
                            settlement: population.settlement.clone(),
                            resident_name: population.resident_name.clone(),
                            handle: snapshot.handle.clone(),
                            work_revision: 0,
                        }),
                    });
                    return vec![storage_cas(
                        &request,
                        SETTLEMENT_REGISTRY_KEY,
                        population.version,
                        value,
                    )];
                };
                vec![message_session(
                    session,
                    format!("Worker spawned, but {message}"),
                )]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} spawn-refused {}", failure_name(refused.reason)),
                );
                let mut commands = vec![message_session(
                    session,
                    format!("Worker spawn refused: {:?}.", refused.reason),
                )];
                // The refusal touched nothing, so the reservation is still held:
                // hand it back so a later spawn reserves again instead of
                // finding a token it cannot consume.
                commands.extend(self.submit(OperationIntent::ReleaseSite { session, token }));
                commands
            }
        }
    }

    /// One reservation hand-back: bookkeeping the operator log reports, since no
    /// leg of the chain depends on it.
    fn release_answer(&mut self, session: u64, outcome: &OperationOutcome) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::Settlement(SettlementResult::ResidentSite(reservation)) =
                    &committed.payload
                else {
                    log(LogLevel::Info, &format!("{MARKER} release-unexpected"));
                    return vec![message_session(
                        session,
                        "The release returned an unexpected result.",
                    )];
                };
                log(
                    LogLevel::Info,
                    &format!("{MARKER} release token={}", reservation.spawn_site_token),
                );
                vec![message_session(session, "Reservation released.")]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} release-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("Reservation release refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    fn counter_load_failed(&mut self, session: Option<u64>) -> Vec<Command> {
        self.counter = None;
        self.counter_version = None;
        let mut commands = self.fail_queued("Settlement operation counter is unavailable.");
        if commands.is_empty() {
            if let Some(session) = session {
                commands.push(message_session(
                    session,
                    "Settlement operation counter is unavailable.",
                ));
            }
        }
        commands
    }

    fn fail_queued(&mut self, message: &str) -> Vec<Command> {
        self.queued
            .drain(..)
            .map(|queued| message_session(queued.intent.session(), message))
            .collect()
    }

    fn clear_unapplied(&mut self) {
        self.pending.clear();
        self.queued.clear();
        self.pending_sites = None;
        self.pending_counter = None;
        self.pending_registry_read = None;
        self.pending_registry_write = None;
        self.pending_hire = None;
        self.pending_dismiss = None;
        self.pending_registry_sites = None;
        self.pending_registry_survey = None;
        self.counter_read_request = None;
        self.pending_survey = None;
        self.pending_status.clear();
        self.pending_funding = None;
        self.counter_read_session = None;
        self.pending_view_players.clear();
        self.pending_view_project_players.clear();
        self.pending_view_role = None;
        self.pending_view_stock.clear();
        self.views.retain(|_, state| state.instance.is_some());
    }
}

impl Plugin for Settlements {
    fn configure(
        &mut self,
        _config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Ok(None)
    }

    fn init(&mut self, _config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.warehouse = None;
        self.resident_revisions.clear();
        self.worker_site = None;
        self.pending_sites = None;
        self.survey_token = None;
        self.pending_survey = None;
        self.pending_status.clear();
        self.last_structure = None;
        self.reservation_ref = None;
        self.reservation_structure_id = None;
        self.stage_index = 0;
        self.stage_work_done = 0;
        self.pending_funding = None;
        self.pending.clear();
        self.queued.clear();
        self.pending_counter = None;
        self.pending_registry_read = None;
        self.pending_registry_write = None;
        self.pending_registry_sites = None;
        self.pending_registry_survey = None;
        self.counter = None;
        self.pending_dismiss = None;
        self.pending_capture = None;
        self.pending_evacuation = None;
        self.pending_hire = None;
        self.counter_version = None;
        self.counter_read_request = None;
        self.counter_read_session = None;
        self.views.clear();
        self.pending_view_role = None;
        self.pending_view_players.clear();
        self.pending_view_project_players.clear();
        self.pending_view_stock.clear();
        self.storage_sequence = 0;
        let mut commands = self.load_counter(None);
        commands.extend(self.recover_hires());
        Ok(commands)
    }

    fn on_events(
        &mut self,
        context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        for event in events {
            match event {
                Event::CommandBatchRejected => self.clear_unapplied(),
                Event::PlayerLeft(left) => self.view_left(left.session),
                Event::LoaderViewRequest(request) => commands.extend(self.view_request(request)),
                Event::ClientSelectionStarted(answer) => {
                    commands.extend(self.view_selection_started(answer))
                }
                Event::OnlinePlayersAnswered(answer) => {
                    commands.extend(self.view_players_answered(answer))
                }
                Event::ClientViewOpened(answer) => commands.extend(self.view_opened(answer)),
                Event::LoaderViewAction(action) => commands.extend(self.view_action(action)),
                Event::CommandInvoked(invoked) => {
                    commands.extend(self.command(invoked, context.tick))
                }
                Event::StorageGetAnswered(answered) => commands.extend(self.storage_get(answered)),
                Event::StorageCasAnswered(answered) => commands.extend(self.storage_cas(answered)),
                Event::OperationAnswered(answered) => commands.extend(self.answered(answered)),
                _ => {}
            }
        }
        Ok(commands)
    }
}

fn same_inventory_endpoint(left: &InventoryEndpoint, right: &InventoryEndpoint) -> bool {
    match (left, right) {
        (InventoryEndpoint::PlayerInventory(left), InventoryEndpoint::PlayerInventory(right)) => {
            left == right
        }
        (InventoryEndpoint::Warehouse(left), InventoryEndpoint::Warehouse(right))
        | (
            InventoryEndpoint::ResidentEquipment(left),
            InventoryEndpoint::ResidentEquipment(right),
        )
        | (InventoryEndpoint::ResidentCarry(left), InventoryEndpoint::ResidentCarry(right)) => {
            left == right
        }
        _ => false,
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64
}

fn valid_settlement_name(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn valid_building_name(value: &str) -> bool {
    (3..=16).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn building_origin_id(building: &SettlementBuilding) -> &str {
    match &building.origin {
        BuildingOrigin::Structure { structure_id } => structure_id.as_str(),
        BuildingOrigin::Certificate { certificate_id } => certificate_id.as_str(),
    }
}

fn warehouse_belongs_to(record: &SettlementRecord, binding: &WarehouseBinding) -> bool {
    match &binding.source {
        WarehouseSource::Authored(source) => record.buildings.values().any(|building| {
            matches!(&building.origin, BuildingOrigin::Structure { structure_id } if structure_id == &source.structure_id)
        }),
        WarehouseSource::VanillaVillage(source) => {
            record.site_id.as_deref() == Some(source.site_id.as_str())
        }
        WarehouseSource::Manual(source) => record.buildings.values().any(|building| {
            building.storage_positions.iter().any(|stored| {
                stored.x == source.position.x && stored.y == source.position.y && stored.z == source.position.z
            })
        }),
    }
}

fn specialty_definition(specialty: &str) -> Option<SpecialtyDefinition> {
    Some(match specialty {
        "farming" => SpecialtyDefinition {
            blueprint_id: "solaris:farm",
            tags: &["crops", "farmland", "grassland", "soil"],
        },
        "forestry" => SpecialtyDefinition {
            blueprint_id: "solaris:sawmill",
            tags: &["trees", "forest", "timber"],
        },
        "ranching" => SpecialtyDefinition {
            blueprint_id: "solaris:pen",
            tags: &["grassland", "pasture", "soil"],
        },
        "fishing" => SpecialtyDefinition {
            blueprint_id: "solaris:fishing_pier",
            tags: &["water", "ocean", "river", "shore"],
        },
        "mining" => SpecialtyDefinition {
            blueprint_id: "solaris:mine_entrance",
            tags: &["stone", "ore", "rock", "cave"],
        },
        _ => return None,
    })
}

fn survey_supports_specialty(record: &SettlementRecord, tags: &[&str]) -> bool {
    record
        .survey_resource_tags
        .iter()
        .chain(&record.survey_biome_tags)
        .any(|tag| tags.contains(&tag.as_str()))
}

fn is_settlement_member(record: &SettlementRecord, player: &str) -> bool {
    record.owner == player || record.roles.contains_key(player)
}

fn can_build_settlement(record: &SettlementRecord, player: &str) -> bool {
    record.owner == player
        || record
            .roles
            .get(player)
            .is_some_and(|role| role == "steward")
}

fn next_settlement_building_name(
    record: &SettlementRecord,
    blueprint_id: &str,
) -> Result<String, &'static str> {
    if record.buildings.len() >= MAX_SETTLEMENT_BUILDINGS {
        return Err("Building limit reached for this settlement.");
    }
    let short = blueprint_id
        .strip_prefix("solaris:")
        .unwrap_or(blueprint_id)
        .bytes()
        .filter_map(|byte| {
            (byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_').then_some(byte)
        })
        .take(8)
        .map(char::from)
        .collect::<String>();
    let sequence = record
        .building_sequence
        .checked_add(1)
        .ok_or("Building limit reached for this settlement.")?;
    Ok(format!(
        "{}_{sequence}",
        if short.is_empty() { "building" } else { &short }
    ))
}

fn next_settlement_resident_name(record: &SettlementRecord) -> Result<String, &'static str> {
    if record.residents.len() >= MAX_SETTLEMENT_RESIDENTS {
        return Err("Resident limit reached for this settlement.");
    }
    let sequence = record
        .resident_sequence
        .checked_add(1)
        .ok_or("Resident limit reached for this settlement.")?;
    Ok(format!("resident_{sequence}"))
}

fn register_settlement(
    registry: &mut SettlementRegistry,
    owner: String,
    name: &str,
    size_class: &str,
) -> Result<(), &'static str> {
    if registry.settlements.contains_key(name) {
        return Err("That settlement name is taken.");
    }
    if registry.settlements.len() >= MAX_SETTLEMENTS {
        return Err("Settlement limit reached.");
    }
    registry.settlements.insert(
        name.to_owned(),
        SettlementRecord {
            owner: owner.clone(),
            roles: BTreeMap::from([(owner, "owner".to_owned())]),
            size_class: size_class.to_owned(),
            revision: 1,
            site_id: None,
            site_revision: None,
            survey_token: None,
            survey_purpose: None,
            survey_biome_tags: Vec::new(),
            survey_resource_tags: Vec::new(),
            condition: None,
            buildings: BTreeMap::new(),
            building_sequence: 0,
            specialties: Vec::new(),
            residents: BTreeMap::new(),
            resident_sequence: 0,
        },
    );
    Ok(())
}

fn assign_settlement_role(
    registry: &mut SettlementRegistry,
    settlement: &str,
    target: &str,
    role: &str,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    if record.owner == target {
        return Err("Ownership transfer is not supported.");
    }
    match role {
        "member" => {
            record.roles.remove(target);
        }
        "steward" | "captain" => {
            if record.roles.len() >= MAX_SETTLEMENT_ROLES && !record.roles.contains_key(target) {
                return Err("Role limit reached.");
            }
            record.roles.insert(target.to_owned(), role.to_owned());
        }
        _ => return Err("Role must be steward, captain, or member."),
    }
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn set_settlement_condition(
    registry: &mut SettlementRegistry,
    settlement: &str,
    condition: &str,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    record.condition = Some(condition.to_owned());
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn adopt_settlement_site(
    registry: &mut SettlementRegistry,
    settlement: &str,
    site_id: &str,
    site_revision: u64,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    record.site_id = Some(site_id.to_owned());
    record.site_revision = Some(site_revision);
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn record_settlement_survey(
    registry: &mut SettlementRegistry,
    settlement: &str,
    survey_token: &str,
    purpose: &str,
    biome_tags: Vec<String>,
    resource_tags: Vec<String>,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    record.survey_token = Some(survey_token.to_owned());
    record.survey_purpose = Some(purpose.to_owned());
    record.survey_biome_tags = biome_tags;
    record.survey_resource_tags = resource_tags;
    record.revision = record.revision.saturating_add(1);
    Ok(())
}
fn parse_triple(value: &str) -> Option<[i32; 3]> {
    let mut parts = value.split(',');
    let x = parts.next()?.parse::<i32>().ok()?;
    let y = parts.next()?.parse::<i32>().ok()?;
    let z = parts.next()?.parse::<i32>().ok()?;
    parts.next().is_none().then_some([x, y, z])
}

fn parse_interaction_points(values: &[&str]) -> Option<Vec<BuildingInteractionPoint>> {
    values
        .iter()
        .map(|value| {
            let (kind, coords) = value.rsplit_once(':')?;
            let [x, y, z] = parse_triple(coords)?;
            let at = settlements::BlockPosition { x, y, z };
            match kind {
                "entrance" => Some(BuildingInteractionPoint::Entrance(
                    settlements::BuildingEntrance { at },
                )),
                "home" | "work" | "meeting" | "guard" => {
                    let poi_kind = match kind {
                        "home" => settlements::SitePoiKind::Home,
                        "work" => settlements::SitePoiKind::Work,
                        "meeting" => settlements::SitePoiKind::Meeting,
                        _ => settlements::SitePoiKind::Guard,
                    };
                    Some(BuildingInteractionPoint::Poi(settlements::BuildingPoi {
                        poi_kind,
                        at,
                        capacity: 1,
                    }))
                }
                "storage" => Some(BuildingInteractionPoint::Storage(
                    settlements::BuildingStorage { at },
                )),
                _ => {
                    let capability = kind.strip_prefix("station=")?;
                    if !valid_resource_id(capability) {
                        return None;
                    }
                    Some(BuildingInteractionPoint::Workstation(
                        settlements::BuildingWorkstation {
                            at,
                            capability: capability.to_owned(),
                            workplaces: 1,
                        },
                    ))
                }
            }
        })
        .collect()
}

fn record_settlement_project(
    registry: &mut SettlementRegistry,
    settlement: &str,
    building_name: &str,
    structure_id: &str,
    blueprint_id: &str,
    structure_revision: u64,
) -> Result<(), &'static str> {
    record_settlement_building(
        registry,
        settlement,
        building_name,
        blueprint_id,
        BuildingOrigin::Structure {
            structure_id: structure_id.to_owned(),
        },
        structure_revision,
    )
}

fn record_settlement_building(
    registry: &mut SettlementRegistry,
    settlement: &str,
    building_name: &str,
    purpose: &str,
    origin: BuildingOrigin,
    snapshot_revision: u64,
) -> Result<(), &'static str> {
    record_settlement_certificate(
        registry,
        settlement,
        building_name,
        purpose,
        origin,
        snapshot_revision,
        Vec::new(),
    )
}

fn record_settlement_certificate(
    registry: &mut SettlementRegistry,
    settlement: &str,
    building_name: &str,
    purpose: &str,
    origin: BuildingOrigin,
    snapshot_revision: u64,
    storage_positions: Vec<StoredPosition>,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    // Idempotent on the certificate/structure authority: the same origin id
    // returns or refreshes its own record without minting a second building.
    let origin_id = match &origin {
        BuildingOrigin::Structure { structure_id } => structure_id.as_str(),
        BuildingOrigin::Certificate { certificate_id } => certificate_id.as_str(),
    };
    if let Some((name, _)) = record
        .buildings
        .iter()
        .find(|(_, building)| building_origin_id(building) == origin_id)
        .map(|(name, building)| (name.clone(), building.clone()))
    {
        let entry = record.buildings.get_mut(&name).expect("known building");
        entry.snapshot_revision = snapshot_revision;
        entry.storage_positions = storage_positions;
        record.revision = record.revision.saturating_add(1);
        return Ok(());
    }
    if let Some(existing) = record.buildings.get(building_name) {
        if building_origin_id(existing) != origin_id {
            return Err("Building name belongs to another building.");
        }
    }
    if record.buildings.len() >= MAX_SETTLEMENT_BUILDINGS
        && !record.buildings.contains_key(building_name)
    {
        return Err("Building limit reached for this settlement.");
    }
    record.buildings.insert(
        building_name.to_owned(),
        SettlementBuilding {
            purpose: purpose.to_owned(),
            origin,
            snapshot_revision,
            storage_positions,
        },
    );
    record.building_sequence = record.building_sequence.saturating_add(1);
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn record_settlement_resident(
    registry: &mut SettlementRegistry,
    settlement: &str,
    resident_name: &str,
    handle: &str,
    work_revision: u64,
) -> Result<(), &'static str> {
    // Open recorded births already hold their roster slots and aliases: an
    // unrelated arrival over either refuses, so a recorded birth still fits.
    // The facts are computed before the mutable borrow below.
    let (open_births, alias_held) =
        demography::settlement_birth_reservation(registry, settlement, resident_name);
    if alias_held
        && !registry.settlements.get(settlement).is_some_and(|record| {
            record
                .residents
                .get(resident_name)
                .is_some_and(|existing| existing.handle == handle)
        })
    {
        return Err("Resident alias already belongs to another resident.");
    }
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    if record.residents.len().saturating_add(open_births) >= MAX_SETTLEMENT_RESIDENTS
        && !record.residents.contains_key(resident_name)
    {
        return Err("Resident limit reached for this settlement.");
    }
    if record
        .residents
        .get(resident_name)
        .is_some_and(|existing| existing.handle != handle)
    {
        return Err("Resident alias already belongs to another resident.");
    }
    if let Some(existing) = record.residents.get_mut(resident_name) {
        // A delayed population receipt must not turn an employed resident
        // back into a civilian or discard a durable hiring intent.
        existing.work_revision = existing.work_revision.max(work_revision);
    } else {
        record.residents.insert(
            resident_name.to_owned(),
            SettlementResident {
                handle: handle.to_owned(),
                work_revision,
                role: None,
                employer: None,
                hiring: None,
                issued: None,
                dismissing: None,
                capturing: None,
                captive_by: None,
                household: None,
                age_origin: None,
            },
        );
        record.resident_sequence = record.resident_sequence.saturating_add(1);
    }
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn record_settlement_specialties(
    registry: &mut SettlementRegistry,
    settlement: &str,
    specialties: Vec<String>,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    record.specialties = specialties;
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn parse_survey_purpose(value: Option<&str>) -> Option<(SurveyPurpose, String, i32)> {
    match value.unwrap_or("plot").to_ascii_lowercase().as_str() {
        "plot" | "settlement" => Some((SurveyPurpose::Settlement, "settlement".to_owned(), 64)),
        "expand" | "expansion" => Some((SurveyPurpose::Expansion, "expansion".to_owned(), 128)),
        "restore" | "restoration" => {
            Some((SurveyPurpose::Restoration, "restoration".to_owned(), 64))
        }
        _ => None,
    }
}

fn survey_bounds(x: f64, y: f64, z: f64, extent: i32) -> SurveyBounds {
    let half = extent / 2;
    let min_x = ((x.floor() as i32) - half).clamp(-30_000_000, 30_000_000 - extent + 1);
    let min_z = ((z.floor() as i32) - half).clamp(-30_000_000, 30_000_000 - extent + 1);
    let min_y = ((y.floor() as i32) - 16).clamp(-64, 288);
    SurveyBounds {
        min: settlements::BlockPosition {
            x: min_x,
            y: min_y,
            z: min_z,
        },
        max: settlements::BlockPosition {
            x: min_x + extent - 1,
            y: min_y + 31,
            z: min_z + extent - 1,
        },
    }
}

fn valid_resource_id(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    value.len() <= 128
        && !namespace.is_empty()
        && !path.is_empty()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
        && path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'/' | b'-')
        })
}

fn free_homes(
    site: &SettlementSite,
) -> impl Iterator<Item = (String, settlements::BlockPosition)> + '_ {
    site.pois
        .iter()
        .filter(|poi| poi.kind == SitePoiKind::Home && poi.state == SitePoiState::Free)
        .map(|poi| (poi.poi_id.clone(), poi.at))
}

/// The contract's own name for one operation failure, which is what a marker
/// reports: the server's vocabulary instead of a debug rendering.
fn failure_name(failure: OperationFailure) -> &'static str {
    match failure {
        OperationFailure::InvalidRequest => "invalid-request",
        OperationFailure::Forbidden => "forbidden",
        OperationFailure::StaleRevision => "stale-revision",
        OperationFailure::NotFound => "not-found",
        OperationFailure::Unloaded => "unloaded",
        OperationFailure::Blocked => "blocked",
        OperationFailure::InsufficientItems => "insufficient-items",
        OperationFailure::Capacity => "capacity",
        OperationFailure::Busy => "busy",
        OperationFailure::RuntimeUnavailable => "runtime-unavailable",
        OperationFailure::OperationConflict => "operation-conflict",
        OperationFailure::CursorExpired => "cursor-expired",
        OperationFailure::Unknown => "unknown",
    }
}

/// Preserve the core-authored resource plan exactly: one material portion for
/// each structure stage, rather than accepting caller-supplied funding totals.
fn plan_of(structure: &StructureSnapshot) -> InventoryResourcePlan {
    InventoryResourcePlan {
        portions: structure
            .stages
            .iter()
            .map(|stage| InventoryWorkPortion {
                work_units: stage.work_units,
                materials: stage
                    .materials
                    .iter()
                    .map(|material| InventoryMaterial {
                        resource_id: material.resource.clone(),
                        quantity: material.quantity,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// The one line a work answer reports: the worker, the state the engine left the
/// assignment in, the reason a paused one carries, the units it finished, the
/// real item changes it committed, and the revision the next order fences on.
fn work_marker(op: &str, assignment: &residents::WorkAssignment) -> String {
    let changes = if assignment.changes.is_empty() {
        "none".to_owned()
    } else {
        assignment
            .changes
            .iter()
            .map(|change| format!("{}:{}", change.item_id, change.delta))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{MARKER} work kind={op} resident={} state={} reason={} units={}/{} changes={} revision={}",
        assignment.handle,
        state_name(assignment.state),
        assignment.reason.map(pause_name).unwrap_or("none"),
        assignment.work_units_done,
        assignment.work_units_planned,
        changes,
        assignment.revision,
    )
}

fn state_name(state: WorkState) -> &'static str {
    match state {
        WorkState::Accepted => "accepted",
        WorkState::Running => "running",
        WorkState::Paused => "paused",
        WorkState::Committed => "committed",
        WorkState::Cancelled => "cancelled",
    }
}

fn structure_state_name(state: StructureState) -> &'static str {
    match state {
        StructureState::Prepared => "prepared",
        StructureState::Running => "running",
        StructureState::Paused => "paused",
        StructureState::Committed => "committed",
        StructureState::Cancelled => "cancelled",
    }
}

fn pause_name(reason: WorkPauseReason) -> &'static str {
    match reason {
        WorkPauseReason::Unloaded => "unloaded",
        WorkPauseReason::NoWorkers => "no-workers",
        WorkPauseReason::MissingInput => "missing-input",
        WorkPauseReason::MissingTool => "missing-tool",
        WorkPauseReason::MissingStation => "missing-station",
        WorkPauseReason::BlockedRoute => "blocked-route",
        WorkPauseReason::Interrupted => "interrupted",
        WorkPauseReason::Protected => "protected",
        WorkPauseReason::Unsupported => "unsupported",
        WorkPauseReason::NoStorage => "no-storage",
    }
}

fn lifecycle_name(lifecycle: residents::ResidentLifecycle) -> &'static str {
    match lifecycle {
        residents::ResidentLifecycle::AliveLoaded => "alive-loaded",
        residents::ResidentLifecycle::AliveUnloaded => "alive-unloaded",
        residents::ResidentLifecycle::Dead => "dead",
        residents::ResidentLifecycle::Released => "released",
    }
}

export_plugin!(Settlements);
#[cfg(test)]
mod tests {
    use super::*;
    use solaris_plugin_sdk::domain_operations::DomainOperation;
    use solaris_plugin_sdk::settlements::{
        SettlementOperation, StructureReceipt, StructureStagePlan,
    };

    fn invocation(arguments: &[&str]) -> CommandInvoked {
        CommandInvoked {
            player: "player-1".to_owned(),
            session: 7,
            username: "player-1".to_owned(),
            operator: false,
            position: solaris_plugin_sdk::types::Position {
                x: 10.5,
                y: 80.0,
                z: -4.5,
            },
            name: "settlement".to_owned(),
            arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
            raw_arguments: arguments.join(" "),
        }
    }

    #[test]
    fn create_command_reads_the_durable_registry_before_mutating() {
        let commands =
            Settlements::default().command(&invocation(&["create", "oak_hollow", "small"]), 0);
        assert!(matches!(
            commands.as_slice(),
            [Command::StorageGet(read)]
                if read.key == SETTLEMENT_REGISTRY_KEY
                    && read.request.starts_with("settlement-registry-read-")
        ));
    }

    #[test]
    fn named_funding_reads_the_registry_before_selecting_its_building() {
        let commands = Settlements::default().legacy_fund(7, "owner", "oak_hollow", "warehouse_1");

        assert!(matches!(
            commands.as_slice(),
            [Command::StorageGet(read)]
                if read.key == SETTLEMENT_REGISTRY_KEY
                    && read.request.starts_with("settlement-registry-read-")
        ));
    }

    #[test]
    fn named_population_reads_the_settlement_registry_before_selecting_a_home() {
        let commands = Settlements::default().legacy_populate(7, "member", "oak_hollow");

        assert!(matches!(
            commands.as_slice(),
            [Command::StorageGet(read)]
                if read.key == SETTLEMENT_REGISTRY_KEY
                    && read.request.starts_with("settlement-registry-read-")
        ));
    }

    #[test]
    fn named_funding_status_query_remembers_its_funding_continuation() {
        let mut settlements = Settlements::default();
        let commands = settlements.status_for_funding(7, "structure-17");

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::StructureStatus(status))
                        if status.structure_id == "structure-17"
                )
        ));
        assert!(settlements.pending_status.values().all(|pending| matches!(
            pending,
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Fund,
            }
        )));
    }

    #[test]
    fn named_advancement_reads_the_registry_before_selecting_its_building() {
        let commands =
            Settlements::default().legacy_advance(7, "owner", "oak_hollow", "warehouse_1");

        assert!(matches!(
            commands.as_slice(),
            [Command::StorageGet(read)]
                if read.key == SETTLEMENT_REGISTRY_KEY
                    && read.request.starts_with("settlement-registry-read-")
        ));
    }

    #[test]
    fn named_advancement_status_query_remembers_its_continuation() {
        let mut settlements = Settlements::default();
        let commands = settlements.status_for_advance(7, "structure-17");

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::StructureStatus(status))
                        if status.structure_id == "structure-17"
                )
        ));
        assert!(settlements.pending_status.values().all(|pending| matches!(
            pending,
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Advance,
            }
        )));
    }

    #[test]
    fn project_queries_its_exact_site_for_a_fresh_revision_fence() {
        let commands =
            Settlements::default().begin_registry_site_query(RegistrySiteQuery::Project {
                session: 7,
                building_name: "warehouse_1".to_owned(),
                site_id: "site_1".to_owned(),
                survey_token: "survey-token".to_owned(),
                blueprint: "solaris:warehouse".to_owned(),
                anchor: settlements::BlockPosition { x: 1, y: 64, z: 2 },
                rotation: 0,
                preview: None,
                settlement: "oak_hollow".to_owned(),
                registry: SettlementRegistry::default(),
                version: None,
            });

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::QuerySite(query))
                        if query.site_id == "site_1" && query.cursor.is_none()
                )
        ));
    }

    #[test]
    fn population_queries_its_adopted_site_instead_of_a_first_page_listing() {
        let commands =
            Settlements::default().begin_registry_site_query(RegistrySiteQuery::Populate {
                session: 7,
                site_id: "village_3_5_0123456789abcdef".to_owned(),
                population: PendingSettlementPopulation {
                    settlement: "oak_hollow".to_owned(),
                    resident_name: "resident_1".to_owned(),
                    registry: SettlementRegistry::default(),
                    version: None,
                },
            });

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::QuerySite(query))
                        if query.site_id == "village_3_5_0123456789abcdef" && query.cursor.is_none()
                )
        ));
    }

    fn materialized_village_site(contents_known: bool) -> SettlementSite {
        SettlementSite {
            site_id: "village_3_5_0123456789abcdef".to_owned(),
            provenance: settlements::SiteProvenance::VanillaVillage,
            variant: settlements::SiteVariant::Village,
            revision: 5,
            contents_known,
            footprint_origin: settlements::BlockPosition {
                x: 48,
                y: 64,
                z: 80,
            },
            footprint_size: settlements::BlockPosition {
                x: 16,
                y: 16,
                z: 16,
            },
            buildings: Vec::new(),
            pois: vec![
                settlements::SettlementPoi {
                    poi_id: "home-reserved".to_owned(),
                    kind: SitePoiKind::Home,
                    at: settlements::BlockPosition {
                        x: 49,
                        y: 64,
                        z: 81,
                    },
                    capacity: 1,
                    state: SitePoiState::Reserved,
                },
                settlements::SettlementPoi {
                    poi_id: "home-free".to_owned(),
                    kind: SitePoiKind::Home,
                    at: settlements::BlockPosition {
                        x: 50,
                        y: 64,
                        z: 82,
                    },
                    capacity: 1,
                    state: SitePoiState::Free,
                },
            ],
            inhabitant_generation_ids: Vec::new(),
        }
    }

    #[test]
    fn population_reserves_the_first_free_home_from_a_materialized_village() {
        let mut settlements = Settlements::default();
        let answered = OperationAnswered {
            request: "population-site".to_owned(),
            operation_id: None,
            outcome: OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Site(
                    materialized_village_site(true),
                )),
            }),
        };

        let commands = settlements.registry_sites_answer(
            PendingRegistrySites {
                request: "population-site".to_owned(),
                query: RegistrySiteQuery::Populate {
                    session: 7,
                    site_id: "village_3_5_0123456789abcdef".to_owned(),
                    population: PendingSettlementPopulation {
                        settlement: "oak_hollow".to_owned(),
                        resident_name: "resident_1".to_owned(),
                        registry: SettlementRegistry::default(),
                        version: None,
                    },
                },
            },
            &answered,
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::StorageGet(read)] if read.key == OPERATION_COUNTER_KEY
        ));
        assert!(matches!(
            settlements.queued.front().map(|queued| &queued.intent),
            Some(OperationIntent::ReserveSite {
                session: 7,
                site_id,
                poi_id,
                expected_revision: 5,
            }) if site_id == "village_3_5_0123456789abcdef" && poi_id == "home-free"
        ));
    }

    #[test]
    fn adoption_persists_a_materialized_village_site() {
        let mut settlements = Settlements::default();
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        let answered = OperationAnswered {
            request: "adopt-village".to_owned(),
            operation_id: None,
            outcome: OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Site(
                    materialized_village_site(true),
                )),
            }),
        };

        let commands = settlements.registry_sites_answer(
            PendingRegistrySites {
                request: "adopt-village".to_owned(),
                query: RegistrySiteQuery::Adopt {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    site_id: "village_3_5_0123456789abcdef".to_owned(),
                    registry,
                    version: Some(3),
                },
            },
            &answered,
        );

        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("materialized village adoption must persist");
        };
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        let record = registry.settlements.get("oak_hollow").unwrap();
        assert_eq!(
            record.site_id.as_deref(),
            Some("village_3_5_0123456789abcdef")
        );
        assert_eq!(record.site_revision, Some(5));
    }

    #[test]
    fn population_refuses_a_village_with_unavailable_contents() {
        let mut settlements = Settlements::default();
        let answered = OperationAnswered {
            request: "unloaded-village".to_owned(),
            operation_id: None,
            outcome: OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Site(
                    materialized_village_site(false),
                )),
            }),
        };

        let commands = settlements.registry_sites_answer(
            PendingRegistrySites {
                request: "unloaded-village".to_owned(),
                query: RegistrySiteQuery::Populate {
                    session: 7,
                    site_id: "village_3_5_0123456789abcdef".to_owned(),
                    population: PendingSettlementPopulation {
                        settlement: "oak_hollow".to_owned(),
                        resident_name: "resident_1".to_owned(),
                        registry: SettlementRegistry::default(),
                        version: None,
                    },
                },
            },
            &answered,
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::SendMessage(message)]
                if message.text == "That site's materialized contents are unavailable."
        ));
        assert!(settlements.queued.is_empty());
    }
    #[test]
    fn adoption_queries_its_exact_site_instead_of_the_first_listing_page() {
        let commands = Settlements::default().begin_registry_site_query(RegistrySiteQuery::Adopt {
            session: 7,
            settlement: "oak_hollow".to_owned(),
            site_id: "site_1".to_owned(),
            registry: SettlementRegistry::default(),
            version: None,
        });

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::QuerySite(query))
                        if query.site_id == "site_1" && query.cursor.is_none()
                )
        ));
    }
    fn structure(id: &str, revision: u64, reservation_ref: Option<&str>) -> StructureSnapshot {
        StructureSnapshot {
            structure_id: id.to_owned(),
            blueprint_id: "solaris:warehouse".to_owned(),
            site_id: "site-1".to_owned(),
            state: StructureState::Running,
            revision,
            origin: settlements::BlockPosition { x: 0, y: 64, z: 0 },
            rotation: 0,
            reserved_footprint: settlements::BlockPosition { x: 1, y: 1, z: 1 },
            stages: vec![StructureStagePlan {
                stage: "foundation".to_owned(),
                block_count: 4,
                work_units: 4,
                materials: Vec::new(),
            }],
            current_stage_index: 0,
            completed_work_units: 1,
            resource_plan_hash: "a".repeat(64),
            reservation_ref: reservation_ref.map(str::to_owned),
            watermark: 1,
            consumed: Vec::new(),
            remaining: Vec::new(),
            pause_reason: None,
        }
    }

    #[test]
    fn warehouse_funding_queries_the_bound_store() {
        let mut settlements = Settlements {
            last_structure: Some(structure("structure-a", 3, None)),
            ..Default::default()
        };

        let commands = settlements
            .fund_from_inventory(7, InventoryEndpoint::Warehouse("warehouse-1".to_owned()));

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Inventory(
                        solaris_plugin_sdk::inventories::InventoryOperation::Query(query)
                    ) if matches!(
                        &query.endpoint,
                        InventoryEndpoint::Warehouse(handle) if handle == "warehouse-1"
                    )
                )
        ));
        assert!(matches!(
            settlements.pending_funding.as_ref(),
            Some(PendingFunding {
                endpoint: InventoryEndpoint::Warehouse(handle),
                ..
            }) if handle == "warehouse-1"
        ));
    }

    fn specialty_registry() -> SettlementRegistry {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        record_settlement_survey(
            &mut registry,
            "oak_hollow",
            "survey-token",
            "settlement",
            vec!["plains".to_owned()],
            vec!["soil".to_owned()],
        )
        .unwrap();
        record_settlement_project(
            &mut registry,
            "oak_hollow",
            "farm_1",
            "structure-farm",
            "solaris:farm",
            4,
        )
        .unwrap();
        registry
    }

    #[test]
    fn specialty_queries_all_saved_matching_workplaces_before_persisting() {
        let mut registry = specialty_registry();
        record_settlement_project(
            &mut registry,
            "oak_hollow",
            "farm_2",
            "structure-farm-2",
            "solaris:farm",
            5,
        )
        .unwrap();
        let mut settlements = Settlements::default();
        let commands = settlements.registry_specialize(
            7,
            "owner".to_owned(),
            "oak_hollow".to_owned(),
            vec!["farming".to_owned()],
            registry,
            Some(4),
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::StructureStatus(status))
                        if status.structure_id == "structure-farm"
                )
        ));
        assert!(matches!(
            settlements.pending_status.values().next(),
            Some(PendingStatus {
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    specialties,
                    workplace_candidates,
                    next_specialty: 0,
                    next_candidate: 0,
                    ..
                }),
                ..
            }) if specialties.as_slice() == ["farming".to_owned()]
                && workplace_candidates
                    .iter()
                    .map(|candidates| {
                        candidates
                            .iter()
                            .map(|candidate| candidate.id.clone())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
                    == [vec![
                        "structure-farm".to_owned(),
                        "structure-farm-2".to_owned(),
                    ]]
        ));
    }

    #[test]
    fn committed_specialty_workplace_persists_the_specialization() {
        let mut snapshot = structure("structure-farm", 5, None);
        snapshot.state = StructureState::Committed;
        let commands = Settlements::default().status_answer(
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    specialties: vec!["farming".to_owned()],
                    workplace_candidates: vec![vec![WorkplaceCandidate {
                        origin: BuildingOrigin::Structure {
                            structure_id: "structure-farm".to_owned(),
                        },
                        id: "structure-farm".to_owned(),
                    }]],
                    next_specialty: 0,
                    next_candidate: 0,
                    registry: specialty_registry(),
                    version: Some(4),
                }),
            },
            &OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Structure(snapshot)),
            }),
        );

        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("committed specialty workplace must persist the specialization");
        };
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        assert_eq!(
            registry.settlements["oak_hollow"].specialties,
            ["farming".to_owned()]
        );
    }

    #[test]
    fn uncommitted_specialty_workplace_cannot_persist_the_specialization() {
        let commands = Settlements::default().status_answer(
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    specialties: vec!["farming".to_owned()],
                    workplace_candidates: vec![vec![WorkplaceCandidate {
                        origin: BuildingOrigin::Structure {
                            structure_id: "structure-farm".to_owned(),
                        },
                        id: "structure-farm".to_owned(),
                    }]],
                    next_specialty: 0,
                    next_candidate: 0,
                    registry: specialty_registry(),
                    version: Some(4),
                }),
            },
            &OperationOutcome::Committed(OperationCommitted {
                revision: 4,
                payload: OperationPayload::Settlement(SettlementResult::Structure(structure(
                    "structure-farm",
                    4,
                    None,
                ))),
            }),
        );

        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::StorageCas(_))));
    }
    #[test]
    fn certificate_specialty_workplace_persists_through_building_status() {
        // A recognized farm answers the shared status query with its immutable
        // certificate; the specialty path accepts exactly that payload when the
        // origin variant and the purpose both match the specialty.
        let commands = Settlements::default().status_answer(
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    specialties: vec!["farming".to_owned()],
                    workplace_candidates: vec![vec![WorkplaceCandidate {
                        origin: BuildingOrigin::Certificate {
                            certificate_id: "certificate-farm".to_owned(),
                        },
                        id: "certificate-farm".to_owned(),
                    }]],
                    next_specialty: 0,
                    next_candidate: 0,
                    registry: specialty_registry(),
                    version: Some(4),
                }),
            },
            &OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Building(
                    settlements::BuildingCertificate {
                        certificate_id: "certificate-farm".to_owned(),
                        footprint: settlements::SurveyBounds {
                            min: settlements::BlockPosition { x: 0, y: 60, z: 0 },
                            max: settlements::BlockPosition { x: 7, y: 67, z: 7 },
                        },
                        purpose: "solaris:farm".to_owned(),
                        interaction_points: Vec::new(),
                        world_revision: 9,
                        revision: 5,
                    },
                )),
            }),
        );

        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("certificate workplace must persist the specialization");
        };
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        assert_eq!(
            registry.settlements["oak_hollow"].specialties,
            ["farming".to_owned()]
        );
    }

    #[test]
    fn certificate_specialty_workplace_with_wrong_purpose_does_not_persist() {
        // Same origin id but a purpose the specialty does not serve: the
        // candidate is skipped, the list exhausts, and nothing persists.
        let commands = Settlements::default().status_answer(
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    specialties: vec!["farming".to_owned()],
                    workplace_candidates: vec![vec![WorkplaceCandidate {
                        origin: BuildingOrigin::Certificate {
                            certificate_id: "certificate-farm".to_owned(),
                        },
                        id: "certificate-farm".to_owned(),
                    }]],
                    next_specialty: 0,
                    next_candidate: 0,
                    registry: specialty_registry(),
                    version: Some(4),
                }),
            },
            &OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Building(
                    settlements::BuildingCertificate {
                        certificate_id: "certificate-farm".to_owned(),
                        footprint: settlements::SurveyBounds {
                            min: settlements::BlockPosition { x: 0, y: 60, z: 0 },
                            max: settlements::BlockPosition { x: 7, y: 67, z: 7 },
                        },
                        purpose: "solaris:sawmill".to_owned(),
                        interaction_points: Vec::new(),
                        world_revision: 9,
                        revision: 5,
                    },
                )),
            }),
        );

        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::StorageCas(_))));
    }

    #[test]
    fn specialty_accepts_a_later_committed_matching_workplace() {
        let mut registry = specialty_registry();
        record_settlement_project(
            &mut registry,
            "oak_hollow",
            "farm_2",
            "structure-farm-2",
            "solaris:farm",
            5,
        )
        .unwrap();
        let mut settlements = Settlements::default();
        let commands = settlements.status_answer(
            PendingStatus {
                session: 7,
                continuation: StatusContinuation::Specialize(PendingSpecialization {
                    session: 7,
                    settlement: "oak_hollow".to_owned(),
                    specialties: vec!["farming".to_owned()],
                    workplace_candidates: vec![vec![
                        WorkplaceCandidate {
                            origin: BuildingOrigin::Structure {
                                structure_id: "structure-farm".to_owned(),
                            },
                            id: "structure-farm".to_owned(),
                        },
                        WorkplaceCandidate {
                            origin: BuildingOrigin::Structure {
                                structure_id: "structure-farm-2".to_owned(),
                            },
                            id: "structure-farm-2".to_owned(),
                        },
                    ]],
                    next_specialty: 0,
                    next_candidate: 0,
                    registry,
                    version: Some(4),
                }),
            },
            &OperationOutcome::Committed(OperationCommitted {
                revision: 4,
                payload: OperationPayload::Settlement(SettlementResult::Structure(structure(
                    "structure-farm",
                    4,
                    None,
                ))),
            }),
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::StructureStatus(status))
                        if status.structure_id == "structure-farm-2"
                )
        ));
        let request = settlements.pending_status.keys().next().cloned().unwrap();
        let pending = settlements.pending_status.remove(&request).unwrap();
        let mut committed = structure("structure-farm-2", 5, None);
        committed.state = StructureState::Committed;
        let commands = settlements.status_answer(
            pending,
            &OperationOutcome::Committed(OperationCommitted {
                revision: 5,
                payload: OperationPayload::Settlement(SettlementResult::Structure(committed)),
            }),
        );

        assert!(matches!(commands.as_slice(), [Command::StorageCas(_)]));
    }

    #[test]
    fn specialty_status_check_blocks_concurrent_registry_mutation() {
        let mut settlements = Settlements::default();
        let commands = settlements.registry_specialize(
            7,
            "owner".to_owned(),
            "oak_hollow".to_owned(),
            vec!["farming".to_owned()],
            specialty_registry(),
            Some(4),
        );
        assert!(matches!(commands.as_slice(), [Command::Operation(_)]));

        let commands = settlements.legacy_list(8, "owner");
        assert!(!commands
            .iter()
            .any(|command| matches!(command, Command::StorageGet(_))));
    }
    #[test]
    fn status_switch_adopts_only_the_selected_structure_reservation() {
        let mut settlements = Settlements {
            last_structure: Some(structure("structure-a", 3, None)),
            reservation_ref: Some("reservation-a".to_owned()),
            reservation_structure_id: Some("structure-a".to_owned()),
            ..Default::default()
        };

        settlements.apply_status(&structure("structure-b", 4, Some("reservation-b")));

        assert_eq!(
            settlements.reservation_ref.as_deref(),
            Some("reservation-b")
        );
        assert_eq!(
            settlements.reservation_structure_id.as_deref(),
            Some("structure-b")
        );
    }

    #[test]
    fn late_foreign_receipt_cannot_advance_the_selected_structure() {
        let mut settlements = Settlements {
            last_structure: Some(structure("structure-b", 4, Some("reservation-b"))),
            stage_index: 0,
            stage_work_done: 1,
            ..Default::default()
        };
        let receipt = StructureReceipt {
            structure_id: "structure-a".to_owned(),
            stage: "foundation".to_owned(),
            sequence: 2,
            block_count: 1,
            work_units: 1,
            consumed: Vec::new(),
            revision: 5,
        };
        settlements.apply_receipt(&receipt);

        assert_eq!(settlements.last_structure.as_ref().unwrap().revision, 4);
        assert_eq!(settlements.stage_index, 0);
        assert_eq!(settlements.stage_work_done, 1);
    }

    #[test]
    fn construction_sound_follows_only_a_committed_block_portion() {
        use solaris_plugin_sdk::client_presentation::ClientCommand;

        let receipt = StructureReceipt {
            structure_id: "structure-a".to_owned(),
            stage: "foundation".to_owned(),
            sequence: 1,
            block_count: 1,
            work_units: 1,
            consumed: Vec::new(),
            revision: 2,
        };
        let committed = OperationOutcome::Committed(OperationCommitted {
            revision: 2,
            payload: OperationPayload::Settlement(SettlementResult::Receipt(receipt.clone())),
        });
        let mut plugin = Settlements::default();
        let commands = plugin.advance_answer(7, &committed);
        assert!(commands.iter().any(|command| matches!(
            command,
            Command::ClientPresentation(ClientCommand::PlayClientSound(sound))
                if sound.session == 7 && sound.sound_id == "solaris-settlements:construction_portion"
        )));

        let paused = OperationOutcome::Committed(OperationCommitted {
            revision: 3,
            payload: OperationPayload::Settlement(SettlementResult::Structure(structure(
                "structure-a",
                3,
                None,
            ))),
        });
        assert!(plugin
            .advance_answer(7, &paused)
            .iter()
            .all(|command| !matches!(
                command,
                Command::ClientPresentation(ClientCommand::PlayClientSound(_))
            )));
        let no_blocks = OperationOutcome::Committed(OperationCommitted {
            revision: 4,
            payload: OperationPayload::Settlement(SettlementResult::Receipt(StructureReceipt {
                block_count: 0,
                ..receipt
            })),
        });
        assert!(plugin
            .advance_answer(7, &no_blocks)
            .iter()
            .all(|command| !matches!(
                command,
                Command::ClientPresentation(ClientCommand::PlayClientSound(_))
            )));
    }

    #[test]
    fn named_project_prepare_keeps_the_registry_write_context() {
        let mut settlements = Settlements::default();
        let commands = settlements.issue_with_context(
            OperationIntent::Prepare {
                session: 7,
                site_id: "site-1".to_owned(),
                blueprint: "solaris:warehouse".to_owned(),
                anchor: settlements::BlockPosition { x: 9, y: 80, z: 9 },
                survey_token: "survey-token".to_owned(),
                rotation: 0,
                expected_site_revision: 3,
                preview: None,
            },
            Some(PendingSettlementProject {
                building_name: "warehouse_1".to_owned(),
                settlement: "oak_hollow".to_owned(),
                registry: SettlementRegistry::default(),
                version: Some(4),
            }),
            None,
            1,
        );

        assert!(matches!(commands.as_slice(), [Command::Operation(_)]));
        assert!(matches!(
            settlements.pending.values().next().map(|pending| &pending.kind),
            Some(PendingKind::Prepare {
                project: Some(PendingSettlementProject {
                    settlement,
                    version: Some(4),
                    ..
                }),
            }) if settlement == "oak_hollow"
        ));
    }

    #[test]
    fn committed_named_project_writes_its_structure_binding_to_the_registry() {
        let mut settlements = Settlements::default();
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        let outcome = OperationOutcome::Committed(OperationCommitted {
            revision: 4,
            payload: OperationPayload::Settlement(SettlementResult::Structure(structure(
                "structure-17",
                4,
                None,
            ))),
        });

        let commands = settlements.prepare_answer(
            7,
            Some(PendingSettlementProject {
                building_name: "warehouse_1".to_owned(),
                settlement: "oak_hollow".to_owned(),
                registry,
                version: Some(4),
            }),
            &outcome,
        );

        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("committed named project must persist its settlement binding");
        };
        assert_eq!(write.key, SETTLEMENT_REGISTRY_KEY);
        assert_eq!(write.expected_version, Some(4));
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        let record = registry.settlements.get("oak_hollow").unwrap();
        let building = record.buildings.get("warehouse_1").unwrap();
        assert_eq!(building_origin_id(building), "structure-17");
        assert_eq!(building.snapshot_revision, 4);
    }

    #[test]
    fn population_accepts_a_materialized_village_before_reserving_a_home() {
        let mut settlements = Settlements::default();
        let mut registry = specialty_registry();
        let record = registry.settlements.get_mut("oak_hollow").unwrap();
        record.site_id = Some("village_3_5_0123456789abcdef".to_owned());
        record.site_revision = Some(6);
        let answered = StorageGetAnswered {
            request: "settlement-registry-read-1".to_owned(),
            outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                value: Some(serde_json::to_string(&registry).unwrap()),
                version: Some(4),
            }),
        };

        let commands = settlements.registry_get_answer(
            PendingRegistryRead {
                request: "settlement-registry-read-1".to_owned(),
                action: RegistryRead::Populate {
                    session: 7,
                    player: "owner".to_owned(),
                    settlement: "oak_hollow".to_owned(),
                },
            },
            &answered,
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::QuerySite(query))
                        if query.site_id == "village_3_5_0123456789abcdef" && query.cursor.is_none()
                )
        ));
        assert!(matches!(
            settlements.pending_registry_sites.as_ref().map(|pending| &pending.query),
            Some(RegistrySiteQuery::Populate { population, .. })
                if population.resident_name == "resident_1"
                    && population.settlement == "oak_hollow"
                    && population.version == Some(4)
        ));
    }

    #[test]
    fn settlement_info_lists_durable_resident_aliases() {
        let mut settlements = Settlements::default();
        let mut registry = specialty_registry();
        record_settlement_resident(
            &mut registry,
            "oak_hollow",
            "resident_1",
            "resident-handle-1",
            0,
        )
        .unwrap();
        let answered = StorageGetAnswered {
            request: "settlement-registry-read-1".to_owned(),
            outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                value: Some(serde_json::to_string(&registry).unwrap()),
                version: Some(4),
            }),
        };

        let commands = settlements.registry_get_answer(
            PendingRegistryRead {
                request: "settlement-registry-read-1".to_owned(),
                action: RegistryRead::Info {
                    session: 7,
                    player: "owner".to_owned(),
                    name: Some("oak_hollow".to_owned()),
                },
            },
            &answered,
        );

        assert!(matches!(
            commands.as_slice(),
            [Command::SendMessage(message)]
                if message
                    .text
                    .contains("residents=resident_1=resident-handle-1@0")
        ));
    }

    #[test]
    fn registry_reads_wait_for_a_pending_named_population() {
        let mut settlements = Settlements {
            counter: Some(1),
            ..Settlements::default()
        };
        let commands = settlements.submit_population(
            OperationIntent::ReserveSite {
                session: 7,
                site_id: "site_1".to_owned(),
                poi_id: "home-1".to_owned(),
                expected_revision: 2,
            },
            PendingSettlementPopulation {
                settlement: "oak_hollow".to_owned(),
                resident_name: "resident_1".to_owned(),
                registry: SettlementRegistry::default(),
                version: Some(4),
            },
        );
        assert!(matches!(commands.as_slice(), [Command::StorageCas(_)]));

        let commands = settlements.legacy_list(8, "owner");
        assert!(matches!(
            commands.as_slice(),
            [Command::SendMessage(message)]
                if message.text == "Another settlement record update is pending."
        ));
    }
    #[test]
    fn recognize_points_accept_a_namespaced_station_capability() {
        // The wire syntax is `station=<namespace:path>:x,y,z`: the split must
        // run on the last colon, or the namespace colon breaks the triple.
        let points = parse_interaction_points(&[
            "entrance:1,61,1",
            "station=minecraft:crafting_table:3,61,3",
            "storage:4,61,4",
        ])
        .expect("namespaced station points parse");
        assert!(matches!(
            points.as_slice(),
            [
                BuildingInteractionPoint::Entrance(_),
                BuildingInteractionPoint::Workstation(_),
                BuildingInteractionPoint::Storage(_),
            ]
        ));
    }

    #[test]
    fn recognize_points_reject_a_bare_station_capability() {
        assert!(parse_interaction_points(&["station=crafting_table:3,61,3"]).is_none());
    }

    #[test]
    fn recognize_waits_for_a_pending_recognize_operation() {
        // A second recognition while one is queued or in flight must wait on
        // the guard instead of replacing the outstanding registry read.
        // `legacy_list` goes through `begin_registry_read` with no survey
        // precondition, so both guard phases are reachable.
        for counter in [Some(1), None] {
            let mut settlements = Settlements {
                counter,
                ..Settlements::default()
            };
            let commands = settlements.submit(OperationIntent::Recognize {
                session: 7,
                settlement: "oak_hollow".to_owned(),
                building_name: "smithy_1".to_owned(),
                purpose: "solaris:smithy".to_owned(),
                footprint: SurveyBounds {
                    min: settlements::BlockPosition { x: 0, y: 60, z: 0 },
                    max: settlements::BlockPosition { x: 7, y: 67, z: 7 },
                },
                points: Vec::new(),
                survey_token: "survey-token".to_owned(),
                expected_world_revision: 9,
                registry: SettlementRegistry::default(),
                version: Some(4),
            });
            if counter.is_some() {
                assert!(matches!(commands.as_slice(), [Command::StorageCas(_)]));
            }

            let commands = settlements.legacy_list(8, "owner");
            assert!(
                matches!(
                    commands.as_slice(),
                    [Command::SendMessage(message)]
                        if message.text == "Another settlement record update is pending."
                ),
                "counter={counter:?}: {commands:?}"
            );
        }
    }

    #[test]
    fn committed_named_population_writes_its_resident_binding_to_the_registry() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        let outcome = OperationOutcome::Committed(OperationCommitted {
            revision: 8,
            payload: OperationPayload::Resident(ResidentResult::Snapshot(
                residents::ResidentSnapshot {
                    handle: "resident-handle-17".to_owned(),
                    entity_uuid: "00000000-0000-0000-0000-000000000017".to_owned(),
                    lifecycle: residents::ResidentLifecycle::AliveUnloaded,
                    revision: 8,
                    generation_id: None,
                    pois: residents::ResidentPois {
                        home: Some("home-17".to_owned()),
                        work: None,
                        meeting: None,
                    },
                    loaded: None,
                },
            )),
        });

        let commands = Settlements::default().spawn_answer(
            7,
            "reservation-17".to_owned(),
            Some(PendingSettlementPopulation {
                settlement: "oak_hollow".to_owned(),
                resident_name: "resident_1".to_owned(),
                registry,
                version: Some(4),
            }),
            &outcome,
        );

        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("committed named population must persist its resident binding");
        };
        assert_eq!(write.key, SETTLEMENT_REGISTRY_KEY);
        assert_eq!(write.expected_version, Some(4));
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        let resident = registry.settlements["oak_hollow"]
            .residents
            .get("resident_1")
            .unwrap();
        assert_eq!(resident.handle, "resident-handle-17");
        assert_eq!(resident.work_revision, 0);
    }

    #[test]
    fn refused_post_spawn_registry_write_retries_without_another_spawn() {
        let mut settlements = Settlements::default();
        settlements.pending_registry_write = Some(PendingRegistryWrite {
            request: "population-cas".to_owned(),
            session: 7,
            message: "Settled resident_1 in oak_hollow.".to_owned(),
            resident: Some(PendingResidentPersistence {
                settlement: "oak_hollow".to_owned(),
                resident_name: "resident_1".to_owned(),
                handle: "resident-handle-1".to_owned(),
                work_revision: 0,
            }),
        });

        let commands = settlements.storage_cas(&StorageCasAnswered {
            request: "population-cas".to_owned(),
            outcome: StorageCasOutcome::Refused,
        });
        let [Command::StorageGet(read)] = commands.as_slice() else {
            panic!("a refused post-spawn write must re-read the registry");
        };
        assert_eq!(read.key, SETTLEMENT_REGISTRY_KEY);
        assert!(matches!(
            settlements.pending_registry_read.as_ref().map(|pending| &pending.action),
            Some(RegistryRead::PersistResident { resident, .. })
                if resident.handle == "resident-handle-1"
        ));

        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        let commands = settlements.storage_get(&StorageGetAnswered {
            request: read.request.clone(),
            outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                value: Some(serde_json::to_string(&registry).unwrap()),
                version: Some(9),
            }),
        });
        let [Command::StorageCas(write)] = commands.as_slice() else {
            panic!("the fresh registry must receive the existing resident alias");
        };
        assert_eq!(write.expected_version, Some(9));
        let registry: SettlementRegistry = serde_json::from_str(&write.value).unwrap();
        assert_eq!(
            registry.settlements["oak_hollow"].residents["resident_1"].handle,
            "resident-handle-1"
        );
    }

    #[test]
    fn resident_names_are_bounded_and_advance_after_persisting() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        assert_eq!(
            next_settlement_resident_name(&registry.settlements["oak_hollow"]).unwrap(),
            "resident_1"
        );
        record_settlement_resident(
            &mut registry,
            "oak_hollow",
            "resident_1",
            "resident-handle-1",
            0,
        )
        .unwrap();
        assert_eq!(
            next_settlement_resident_name(&registry.settlements["oak_hollow"]).unwrap(),
            "resident_2"
        );

        let record = registry.settlements.get_mut("oak_hollow").unwrap();
        record.residents = (1..=MAX_SETTLEMENT_RESIDENTS)
            .map(|index| {
                (
                    format!("resident_{index}"),
                    SettlementResident {
                        handle: format!("resident-handle-{index}"),
                        work_revision: 0,
                        role: None,
                        employer: None,
                        hiring: None,
                        issued: None,
                        dismissing: None,
                        capturing: None,
                        captive_by: None,
                        household: None,
                        age_origin: None,
                    },
                )
            })
            .collect();
        assert_eq!(
            next_settlement_resident_name(record),
            Err("Resident limit reached for this settlement.")
        );
    }
    #[test]
    fn bind_village_command_issues_a_site_and_ordinal_only() {
        let commands = Settlements::default().issue(
            OperationIntent::BindVillage {
                session: 7,
                site_id: "village_3_5_0123456789abcdef".to_owned(),
                container_id: 2,
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if operation.request == "settlement-request-1"
                    && matches!(
                        &operation.operation,
                        DomainOperation::Settlement(
                            SettlementOperation::BindVillageWarehouse(bind)
                        ) if bind.operation_id == "settlement-operation-1"
                            && bind.site_id == "village_3_5_0123456789abcdef"
                            && bind.container_id == 2
                    )
        ));
    }

    #[test]
    fn bind_manual_command_issues_a_token_and_position_only() {
        let commands = Settlements::default().issue(
            OperationIntent::BindManual {
                session: 7,
                survey_token: "survey-token".to_owned(),
                position: settlements::BlockPosition { x: 1, y: 2, z: 3 },
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if operation.request == "settlement-request-1"
                    && matches!(
                        &operation.operation,
                        DomainOperation::Settlement(
                            SettlementOperation::BindManualWarehouse(bind)
                        ) if bind.operation_id == "settlement-operation-1"
                            && bind.survey_token == "survey-token"
                            && bind.position.x == 1
                            && bind.position.y == 2
                            && bind.position.z == 3
                    )
        ));
    }

    #[test]
    fn recognize_command_issues_the_surveyed_footprint_request() {
        let footprint = SurveyBounds {
            min: settlements::BlockPosition { x: 0, y: 60, z: 0 },
            max: settlements::BlockPosition { x: 7, y: 67, z: 7 },
        };
        let commands = Settlements::default().issue(
            OperationIntent::Recognize {
                session: 7,
                settlement: "oak_hollow".to_owned(),
                building_name: "smithy_1".to_owned(),
                purpose: "solaris:smithy".to_owned(),
                footprint: footprint.clone(),
                points: vec![BuildingInteractionPoint::Entrance(
                    settlements::BuildingEntrance {
                        at: settlements::BlockPosition { x: 1, y: 61, z: 1 },
                    },
                )],
                survey_token: "survey-token".to_owned(),
                expected_world_revision: 9,
                registry: SettlementRegistry::default(),
                version: Some(4),
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if operation.request == "settlement-request-1"
                    && matches!(
                        &operation.operation,
                        DomainOperation::Settlement(
                            SettlementOperation::RecognizeBuilding(recognize)
                        ) if recognize.operation_id == "settlement-operation-1"
                            && recognize.request.purpose == "solaris:smithy"
                            && recognize.request.survey_token == "survey-token"
                            && recognize.request.expected_world_revision == 9
                    )
        ));
    }

    #[test]
    fn recognized_certificate_reuses_its_record_without_a_second_building() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        record_settlement_certificate(
            &mut registry,
            "oak_hollow",
            "smithy_1",
            "solaris:smithy",
            BuildingOrigin::Certificate {
                certificate_id: "certificate-1".to_owned(),
            },
            4,
            Vec::new(),
        )
        .unwrap();
        let before = registry.settlements["oak_hollow"].building_sequence;
        record_settlement_certificate(
            &mut registry,
            "oak_hollow",
            "smithy_1",
            "solaris:smithy",
            BuildingOrigin::Certificate {
                certificate_id: "certificate-1".to_owned(),
            },
            7,
            Vec::new(),
        )
        .unwrap();
        let record = &registry.settlements["oak_hollow"];
        assert_eq!(record.buildings.len(), 1);
        assert_eq!(record.building_sequence, before);
        assert_eq!(
            record.buildings["smithy_1"].snapshot_revision, 7,
            "the same certificate refreshes its own record"
        );
    }

    #[test]
    fn prepare_command_uses_the_survey_token_and_site_revision() {
        let commands = Settlements::default().issue(
            OperationIntent::Prepare {
                session: 7,
                site_id: "site-1".to_owned(),
                blueprint: view::GATE_BLUEPRINT.to_owned(),
                anchor: settlements::BlockPosition { x: 9, y: 80, z: 9 },
                survey_token: "survey-token".to_owned(),
                rotation: 90,
                expected_site_revision: 3,
                preview: None,
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(SettlementOperation::PrepareStructure(prepare))
                        if prepare.operation_id == "settlement-operation-1"
                            && prepare.site_id == "site-1"
                            && prepare.blueprint_id == view::GATE_BLUEPRINT
                            && prepare.anchor.x == 9
                            && prepare.anchor.y == 80
                            && prepare.anchor.z == 9
                            && prepare.survey_token == "survey-token"
                            && prepare.expected_blueprint_hash.is_none()
                            && prepare.rotation == 90
                            && prepare.expected_site_revision == 3
                )
        ));
    }

    #[test]
    fn craft_command_issues_a_real_recipe_order_at_one_station_cell() {
        let commands = Settlements::default().issue(
            OperationIntent::Craft {
                session: 7,
                resident: "resident-1".to_owned(),
                expected_revision: 3,
                recipe: "minecraft:oak_planks".to_owned(),
                count: 2,
                station: WorkArea {
                    dimension: STATION_DIMENSION.to_owned(),
                    min: residents::BlockPosition { x: 9, y: 64, z: 9 },
                    max: residents::BlockPosition { x: 9, y: 64, z: 9 },
                },
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if operation.request == "settlement-request-1"
                    && matches!(
                        &operation.operation,
                        DomainOperation::ResidentOrder(
                            residents::ResidentOrderOperation::AssignWork(work)
                        ) if work.operation_id == "settlement-operation-1"
                            && work.handle == "resident-1"
                            && work.work_units == WORK_UNITS
                            && work.expected_revision == 3
                            && matches!(
                                &work.work,
                                WorkOrder::Craft(craft)
                                    if craft.recipe == "minecraft:oak_planks"
                                        && craft.count == 2
                                        && craft.station.dimension == STATION_DIMENSION
                                        && craft.station.max.x == craft.station.min.x
                                        && craft.station.max.y == craft.station.min.y
                                        && craft.station.max.z == craft.station.min.z
                                        && craft.station.min.x == 9
                                        && craft.station.min.y == 64
                                        && craft.station.min.z == 9
                            )
                    )
        ));
    }

    #[test]
    fn harvest_command_issues_a_bounded_tool_fenced_work_order() {
        let commands = Settlements::default().issue(
            OperationIntent::Harvest {
                session: 7,
                resident: "resident-1".to_owned(),
                expected_revision: 3,
                tool: "minecraft:iron_hoe".to_owned(),
                area: WorkArea {
                    dimension: STATION_DIMENSION.to_owned(),
                    min: residents::BlockPosition { x: 9, y: 64, z: 9 },
                    max: residents::BlockPosition {
                        x: 12,
                        y: 65,
                        z: 13,
                    },
                },
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::ResidentOrder(
                        residents::ResidentOrderOperation::AssignWork(work)
                    ) if work.operation_id == "settlement-operation-1"
                        && work.handle == "resident-1"
                        && work.work_units == WORK_UNITS
                        && work.expected_revision == 3
                        && matches!(
                            &work.work,
                            WorkOrder::Harvest(harvest)
                                if harvest.tool == "minecraft:iron_hoe"
                                    && harvest.area.dimension == STATION_DIMENSION
                                    && harvest.area.min.x == 9
                                    && harvest.area.min.y == 64
                                    && harvest.area.min.z == 9
                                    && harvest.area.max.x == 12
                                    && harvest.area.max.y == 65
                                    && harvest.area.max.z == 13
                        )
                )
        ));
    }

    #[test]
    fn haul_command_deposits_resident_cargo_into_the_bound_warehouse() {
        let commands = Settlements::default().issue(
            OperationIntent::Haul {
                session: 7,
                resident: "resident-1".to_owned(),
                warehouse: "warehouse-1".to_owned(),
                expected_revision: 3,
                direction: HaulDirection::Deposit,
                item: Some("minecraft:oak_planks".to_owned()),
            },
            1,
        );
        assert!(matches!(
            commands.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::ResidentOrder(
                        residents::ResidentOrderOperation::AssignWork(work)
                    ) if work.operation_id == "settlement-operation-1"
                        && work.handle == "resident-1"
                        && matches!(
                            &work.work,
                            WorkOrder::Haul(haul)
                                if matches!(
                                    &haul.source,
                                    InventoryEndpoint::ResidentCarry(handle)
                                        if handle == "resident-1"
                                )
                                    && matches!(
                                        &haul.destination,
                                        InventoryEndpoint::Warehouse(handle)
                                            if handle == "warehouse-1"
                                    )
                                    && haul.item.as_deref() == Some("minecraft:oak_planks")
                        )
                )
        ));
    }

    #[test]
    fn worker_intentions_reserve_and_then_spawn_one_villager() {
        let mut settlements = Settlements::default();
        let reserve = settlements.issue(
            OperationIntent::ReserveSite {
                session: 7,
                site_id: "site_1".to_owned(),
                poi_id: "poi_1".to_owned(),
                expected_revision: 4,
            },
            1,
        );
        assert!(matches!(
            reserve.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Settlement(
                        SettlementOperation::ReserveResidentSite(reserve)
                    ) if reserve.site_id == "site_1"
                        && reserve.poi_id == "poi_1"
                        && reserve.expected_site_revision == 4
                )
        ));
        let spawn = settlements.issue(
            OperationIntent::SpawnResident {
                session: 7,
                token: "spawn-site-token".to_owned(),
            },
            2,
        );
        assert!(matches!(
            spawn.as_slice(),
            [Command::Operation(operation)]
                if matches!(
                    &operation.operation,
                    DomainOperation::Resident(
                        residents::ResidentOperation::Spawn(spawn)
                    ) if spawn.spawn_site_token == "spawn-site-token"
                        && spawn.profile.kind == ResidentKind::Villager
                )
        ));
    }
    #[test]
    fn registry_persists_a_named_owner_scoped_settlement() {
        let mut registry = SettlementRegistry::default();

        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "medium").unwrap();

        let encoded = serde_json::to_string(&registry).unwrap();
        let decoded = serde_json::from_str::<SettlementRegistry>(&encoded).unwrap();
        let record = decoded.settlements.get("oak_hollow").unwrap();
        assert_eq!(record.owner, "player-1");
        assert_eq!(
            record.roles.get("player-1").map(String::as_str),
            Some("owner")
        );
        assert_eq!(record.size_class, "medium");
        assert_eq!(record.revision, 1);
        assert!(valid_settlement_name("oak_hollow"));
        assert!(!valid_settlement_name("Oak Hollow"));
    }

    #[test]
    fn registry_role_assignment_updates_durable_authorization() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();

        assign_settlement_role(&mut registry, "oak_hollow", "steward", "steward").unwrap();
        let record = registry.settlements.get("oak_hollow").unwrap();
        assert!(is_settlement_member(record, "steward"));
        assert!(can_build_settlement(record, "steward"));
        assert_eq!(record.revision, 2);

        assert_eq!(
            assign_settlement_role(&mut registry, "oak_hollow", "owner", "captain"),
            Err("Ownership transfer is not supported.")
        );
        assign_settlement_role(&mut registry, "oak_hollow", "steward", "member").unwrap();
        assert!(!is_settlement_member(
            registry.settlements.get("oak_hollow").unwrap(),
            "steward"
        ));
    }

    #[test]
    fn registry_condition_tracks_ruin_and_restoration() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();

        set_settlement_condition(&mut registry, "oak_hollow", "partially_ruined").unwrap();
        set_settlement_condition(&mut registry, "oak_hollow", "inhabited").unwrap();

        let record = registry.settlements.get("oak_hollow").unwrap();
        assert_eq!(record.condition.as_deref(), Some("inhabited"));
        assert_eq!(record.revision, 3);
    }

    #[test]
    fn registry_adoption_records_the_listed_site_revision() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();

        adopt_settlement_site(
            &mut registry,
            "oak_hollow",
            "village_3_5_0123456789abcdef",
            7,
        )
        .unwrap();

        let record = registry.settlements.get("oak_hollow").unwrap();
        assert_eq!(
            record.site_id.as_deref(),
            Some("village_3_5_0123456789abcdef")
        );
        assert_eq!(record.site_revision, Some(7));
        assert_eq!(record.revision, 2);
    }

    #[test]
    fn registry_survey_records_core_token_and_clamps_plot_bounds() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();
        record_settlement_survey(
            &mut registry,
            "oak_hollow",
            "server-issued-survey-token",
            "settlement",
            vec!["plains".to_owned()],
            vec!["farmland".to_owned()],
        )
        .unwrap();

        let record = registry.settlements.get("oak_hollow").unwrap();
        assert_eq!(
            record.survey_token.as_deref(),
            Some("server-issued-survey-token")
        );
        assert_eq!(record.survey_purpose.as_deref(), Some("settlement"));
        assert_eq!(record.survey_biome_tags, ["plains"]);
        assert_eq!(record.survey_resource_tags, ["farmland"]);
        assert_eq!(record.revision, 2);
        let bounds = survey_bounds(30_000_000.0, 400.0, -30_000_000.0, 64);
        assert_eq!(bounds.min.x, 29_999_937);
        assert_eq!(bounds.max.x, 30_000_000);
        assert_eq!(bounds.min.y, 288);
        assert_eq!(bounds.max.y, 319);
        assert_eq!(bounds.min.z, -30_000_000);
        assert_eq!(bounds.max.z, -29_999_937);
    }

    #[test]
    fn registry_project_records_the_core_structure_in_its_named_building() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();

        record_settlement_project(
            &mut registry,
            "oak_hollow",
            "warehouse_1",
            "structure-17",
            "solaris:warehouse",
            4,
        )
        .unwrap();

        let record = registry.settlements.get("oak_hollow").unwrap();
        let building = record.buildings.get("warehouse_1").unwrap();
        assert_eq!(building_origin_id(building), "structure-17");
        assert_eq!(building.purpose, "solaris:warehouse");
        assert_eq!(building.snapshot_revision, 4);
        assert_eq!(record.building_sequence, 1);
        assert_eq!(record.revision, 2);
    }

    #[test]
    fn settlement_building_index_is_bounded() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();

        for index in 1..=MAX_SETTLEMENT_BUILDINGS {
            record_settlement_project(
                &mut registry,
                "oak_hollow",
                &format!("warehouse_{index}"),
                &format!("structure-{index}"),
                "solaris:warehouse",
                index as u64,
            )
            .unwrap();
        }

        assert_eq!(
            record_settlement_project(
                &mut registry,
                "oak_hollow",
                "warehouse_65",
                "structure-65",
                "solaris:warehouse",
                65,
            ),
            Err("Building limit reached for this settlement.")
        );
    }

    #[test]
    fn registry_refuses_duplicate_names_and_capacity_overflow() {
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "player-1".to_owned(), "oak_hollow", "small").unwrap();
        assert_eq!(
            register_settlement(&mut registry, "player-2".to_owned(), "oak_hollow", "large"),
            Err("That settlement name is taken.")
        );
        for index in 1..MAX_SETTLEMENTS {
            register_settlement(
                &mut registry,
                "player-1".to_owned(),
                &format!("settlement-{index}"),
                "small",
            )
            .unwrap();
        }
        assert_eq!(
            register_settlement(&mut registry, "player-1".to_owned(), "overflow", "small"),
            Err("Settlement limit reached.")
        );
    }
}
