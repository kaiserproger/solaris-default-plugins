//! Bind authored or generator-authenticated village containers, hold a resident
//! worker, and give that worker real haul and craft orders into the bound store.

mod campaign_compat;
mod capture_runtime;
#[cfg(test)]
mod capture_tests;
mod caravan;
mod caravan_runtime;
#[cfg(test)]
mod caravan_tests;
mod cargo_transfer;
mod charter;
#[cfg(test)]
mod charter_tests;
mod civil_move;
mod combat;
#[cfg(test)]
mod combat_tests;
mod commerce;
mod commerce_player;
mod commerce_player_runtime;
#[cfg(test)]
mod commerce_player_tests;
mod commerce_runtime;
#[cfg(test)]
mod commerce_tests;
mod commerce_trade;
mod demob_runtime;
#[cfg(test)]
mod demob_tests;
mod demography;
mod demography_runtime;
#[cfg(test)]
mod demography_tests;
mod development;
mod evacuation;
mod hire;
mod hire_runtime;
#[cfg(test)]
mod hire_tests;
mod military_catalog;
#[cfg(test)]
mod military_slice_a_tests;
#[cfg(test)]
mod military_slice_b_tests;
mod military_upkeep;
mod morale;
mod morale_runtime;
mod payment_mandate;
mod payment_mandate_runtime;
mod production;
mod production_runtime;
#[cfg(test)]
mod production_runtime_tests;
mod production_storage;
#[cfg(test)]
mod production_tests;
mod registry_runtime;
mod registry_storage;
mod siege_artillery;
mod siege_policy;
#[cfg(test)]
mod siege_tests;
mod technology;
mod technology_runtime;
#[cfg(test)]
mod technology_tests;

mod view;
mod war_policy;
#[cfg(test)]
mod war_tests;
use registry_runtime::{NativeOperationAnswer, StoragePublish, StorageRead};
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
use solaris_plugin_sdk::npcs::{
    AreaToolWork, CraftWork, HaulWork, NpcOrderResult, NpcResult, WorkArea, WorkOrder,
    WorkPauseReason, WorkState,
};
use solaris_plugin_sdk::operation_types::OperationFailure;
use solaris_plugin_sdk::world_structures::{
    self, StructureSnapshot, StructureState, WorksiteInteractionPoint, WorldBounds,
    WorldStructureResult,
};
use solaris_plugin_sdk::{
    advance_structure, assign_npc_work, check_structure, export_plugin, log, message_session, npcs,
    play_client_sound, prepare_structure, query_owned_inventory, reserve_inventory_items,
    storage_cas, storage_get, structure_status, Command, Config, EventContext, Failure,
    InitContext, LogLevel, Plugin, StorageCasOutcome, StorageGetOutcome,
};
use world_structures::{check_worksite, list_features, query_feature, register_worksite, survey};

const WORK_UNITS: u64 = 4_096;
/// One structure portion mirrors the single-cell canonical settlement advance.
const STRUCTURE_PORTION_WORK_UNITS: u64 = 1;
const OPERATION_COUNTER_KEY: &str = "operation-counter";
const WAREHOUSE_BINDING_KEY: &str = "warehouse-binding-v1";
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

const MAX_SETTLEMENTS: usize = 24;
const MAX_SETTLEMENT_ROLES: usize = 8;
const MAX_SETTLEMENT_BUILDINGS: usize = 64;
const MAX_SETTLEMENT_RESIDENTS: usize = 64;

#[derive(Clone, Default, Deserialize, Serialize)]
struct SettlementRegistry {
    settlements: BTreeMap<String, SettlementRecord>,
    #[serde(default)]
    hire_sequence: u64,
    #[serde(default)]
    dismiss_sequence: u64,
    #[serde(default)]
    capture_sequence: u64,
    #[serde(default)]
    evacuation_sequence: u64,
    #[serde(default)]
    morale_sequence: u64,
    /// Households by household id, and the sequence that names them. Both are
    /// additive: a save written before this slice loads unchanged.
    #[serde(default)]
    household_sequence: u64,
    #[serde(default)]
    households: BTreeMap<String, demography::Household>,
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
struct WarehouseBinding {
    handle: String,
    source: WarehouseSource,
    revision: u64,
}

#[derive(Clone, Deserialize, Serialize)]
enum WarehouseSource {
    Authored(AuthoredWarehouseSource),
    VanillaVillage(VillageWarehouseSource),
    Manual(ManualWarehouseSource),
}

#[derive(Clone, Deserialize, Serialize)]
struct AuthoredWarehouseSource {
    structure_id: String,
    container_id: u32,
}

#[derive(Clone, Deserialize, Serialize)]
struct VillageWarehouseSource {
    site_id: String,
    container_id: u32,
}

#[derive(Clone, Deserialize, Serialize)]
struct ManualWarehouseSource {
    position: StoredPosition,
}

/// Durable plugin identity, not a native inventory authority. Old opaque
/// warehouse handles deliberately cannot resolve to a physical inventory.
fn warehouse_endpoint(handle: &str) -> Option<InventoryEndpoint> {
    let identity = handle.strip_prefix("container:")?;
    let mut fields = identity.rsplitn(4, ':');
    let z = fields.next()?.parse::<i32>().ok()?;
    let y = fields.next()?.parse::<i32>().ok()?;
    let x = fields.next()?.parse::<i32>().ok()?;
    let dimension = fields.next()?;
    if !valid_resource_id(dimension) {
        return None;
    }
    Some(InventoryEndpoint::Container(
        solaris_plugin_sdk::inventories::ContainerEndpoint {
            dimension: dimension.to_owned(),
            position: solaris_plugin_sdk::types::BlockPosition { x, y, z },
        },
    ))
}

fn container_identity(endpoint: &InventoryEndpoint) -> Option<String> {
    let InventoryEndpoint::Container(container) = endpoint else {
        return None;
    };
    Some(format!(
        "container:{}:{}:{}:{}",
        container.dimension, container.position.x, container.position.y, container.position.z
    ))
}

#[derive(Clone, Deserialize, Serialize)]
struct SettlementBuilding {
    purpose: String,
    origin: BuildingOrigin,
    #[serde(default)]
    snapshot_revision: u64,
    #[serde(default)]
    storage_positions: Vec<StoredPosition>,
    #[serde(default)]
    station_positions: Vec<StoredPosition>,
    #[serde(default)]
    entrance_positions: Vec<StoredPosition>,
    #[serde(default)]
    worksite_id: Option<String>,
    #[serde(default)]
    dimension: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct SettlementResident {
    handle: String,
    #[serde(default)]
    native_entity_uuid: Option<String>,
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
    capture_recovery: Option<capture_runtime::CaptureIntent>,
    #[serde(default)]
    captive_by: Option<String>,
    #[serde(default)]
    civil_work: Option<hire_runtime::CivilWork>,
    #[serde(default)]
    morale: Option<morale::OperationalMorale>,
    /// Social household membership; native snapshot age is the only age authority.
    #[serde(default)]
    household: Option<String>,
}
impl SettlementResident {
    fn roster_status(&self) -> String {
        if let Some(captor) = &self.captive_by {
            if let Some(intent) = self
                .capturing
                .as_ref()
                .filter(|intent| intent.terminal_cleanup_pending())
            {
                if let Some(refusal) = intent.terminal_cleanup_refusal() {
                    format!(
                        "prisoner of {captor} (terminal original-work cleanup pending: {refusal})"
                    )
                } else {
                    format!("prisoner of {captor} (terminal original-work cleanup pending)")
                }
            } else {
                format!("prisoner of {captor}")
            }
        } else if let Some(intent) = &self.capturing {
            format!("capture pending by {}", intent.captor_settlement)
        } else if self.capture_recovery.is_some() {
            format!(
                "{} (physical control recovery pending; restart retries against current controls)",
                self.role.map_or("civilian", hire::Role::name)
            )
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
    survey_world_revision: Option<u64>,
    #[serde(default)]
    survey_biome_tags: Vec<String>,
    #[serde(default)]
    survey_resource_tags: Vec<String>,
    #[serde(default)]
    survey_purpose: Option<String>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    evacuation: Option<evacuation::Intent>,
    #[serde(default)]
    buildings: BTreeMap<String, SettlementBuilding>,
    #[serde(default)]
    building_sequence: u32,
    #[serde(default)]
    specialties: Vec<String>,
    #[serde(default)]
    residents: BTreeMap<String, SettlementResident>,
    #[serde(default)]
    home_claims: BTreeMap<String, String>,
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
    Status {
        session: u64,
        player: String,
        target: String,
        construction: bool,
    },
    WorkStatus {
        session: u64,
        player: String,
        resident: String,
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
    Attach {
        session: u64,
        player: String,
        settlement: String,
        entity_uuid: String,
    },
    Cargo {
        session: u64,
        player: String,
        settlement: String,
        alias: String,
        inward: bool,
        resource: String,
        count: u32,
    },
    CivilMove {
        session: u64,
        player: String,
        settlement: String,
        alias: String,
        target: Option<solaris_plugin_sdk::types::BlockPosition>,
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
        purpose_name: String,
        bounds: WorldBounds,
    },
    Project {
        session: u64,
        player: String,
        settlement: String,
        blueprint: String,
        anchor: world_structures::BlockPosition,
        rotation: u16,
        preview: Option<view::PreviewOrigin>,
    },
    Recognize {
        session: u64,
        player: String,
        settlement: String,
        building_name: String,
        purpose: String,
        points: Vec<String>,
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
    entity_uuid: String,
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
    Attach {
        session: u64,
        site_id: String,
        attachment: PendingSettlementAttachment,
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
struct PendingSettlementAttachment {
    settlement: String,
    resident_name: String,
    registry: SettlementRegistry,
    version: Option<u64>,
    entity_uuid: String,
}

struct PendingAttachmentQuery {
    request: String,
    session: u64,
    attachment: PendingSettlementAttachment,
}

enum StatusContinuation {
    Read { workplace_id: String, manual: bool },
    Construction { structure_id: String },
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
            | Self::Status { session, .. }
            | Self::Overview { session, .. }
            | Self::Sites { session, .. }
            | Self::Adopt { session, .. }
            | Self::Attach { session, .. }
            | Self::Cargo { session, .. }
            | Self::CivilMove { session, .. }
            | Self::Treat { session, .. }
            | Self::Survey { session, .. }
            | Self::PersistResident { session, .. }
            | Self::Project { session, .. }
            | Self::Recognize { session, .. }
            | Self::WorkStatus { session, .. }
            | Self::Role { session, .. }
            | Self::Condition { session, .. }
            | Self::Fund { session, .. }
            | Self::Advance { session, .. }
            | Self::Specialize { session, .. } => *session,
        }
    }
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
            Self::Carry => InventoryEndpoint::NpcCarry(resident),
            Self::Equipment => InventoryEndpoint::NpcEquipment(resident),
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
        position: world_structures::BlockPosition,
    },
    Recognize {
        session: u64,
        settlement: String,
        building_name: String,
        purpose: String,
        footprint: WorldBounds,
        points: Vec<WorksiteInteractionPoint>,
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
    AreaWork {
        session: u64,
        resident: String,
        expected_revision: u64,
        action: &'static str,
        order: WorkOrder,
    },
    Prepare {
        session: u64,
        dimension: String,
        blueprint: String,
        anchor: world_structures::BlockPosition,
        survey_token: String,
        rotation: u16,
        expected_world_revision: u64,
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
    AttachResident {
        session: u64,
        attachment: PendingSettlementAttachment,
    },
    CargoTransfer {
        session: u64,
        transfers: Vec<solaris_plugin_sdk::inventories::OwnedItemTransfer>,
        fences: Vec<solaris_plugin_sdk::inventories::InventoryExpectedRevision>,
        guard: caravan::StorageGuard,
    },
    CivilMove {
        session: u64,
        handle: String,
        target: Option<solaris_plugin_sdk::types::BlockPosition>,
        order_revision: u64,
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
        scope_guard: solaris_plugin_sdk::types::PluginStorageVersionGuard,
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
            | Self::AreaWork { session, .. }
            | Self::Prepare { session, .. }
            | Self::ReserveMaterials { session, .. }
            | Self::Advance { session, .. }
            | Self::AttachResident { session, .. }
            | Self::CargoTransfer { session, .. }
            | Self::CivilMove { session, .. }
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
    BindInventory {
        source: WarehouseSource,
        endpoint: InventoryEndpoint,
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
    AreaWork {
        resident: String,
        action: &'static str,
    },
    Prepare {
        project: Option<PendingSettlementProject>,
    },
    AttachResident {
        attachment: PendingSettlementAttachment,
    },
    CargoTransfer,
    CivilMove,
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
}

struct PendingCounter {
    request: String,
    next: u64,
    intent: OperationIntent,
    project: Option<PendingSettlementProject>,
}

struct PendingWarehouseWrite {
    request: String,
    session: u64,
    binding: WarehouseBinding,
}

#[derive(Default)]
struct Settlements {
    warehouse: Option<WarehouseBinding>,
    warehouse_version: Option<u64>,
    warehouse_read_request: Option<String>,
    warehouse_storage_ready: bool,
    pending_warehouse_write: Option<PendingWarehouseWrite>,
    pending_funding: Option<PendingFunding>,
    survey_token: Option<String>,
    survey_bounds: Option<WorldBounds>,
    survey_world_revision: Option<u64>,
    pending_status: BTreeMap<String, PendingStatus>,
    pending_work_status: BTreeMap<String, (u64, String)>,
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
    pending_attachment_query: Option<PendingAttachmentQuery>,
    pending_registry_survey: Option<PendingRegistrySurvey>,
    counter: Option<u64>,
    counter_version: Option<u64>,
    counter_read_request: Option<String>,
    counter_read_session: Option<u64>,
    storage_sequence: u64,
    views: BTreeMap<u64, view::ViewState>,
    public_contract_views: BTreeMap<u64, view::PublicContractView>,
    resident_reads: BTreeMap<u64, view::ResidentRead>,
    view_tick: u64,
    pending_view_players: BTreeMap<String, (u64, usize)>,
    requested_view_panes: BTreeMap<u64, usize>,
    pending_view_project_players: BTreeMap<String, view::PendingViewProject>,
    pending_view_stock: BTreeMap<String, (u64, String)>,
    pending_view_buildings: BTreeMap<String, (u64, String, String)>,
    pending_view_role: Option<(String, String, String)>,
    pending_dismiss: Option<demob_runtime::PendingDismiss>,
    pending_hire: Option<hire_runtime::PendingHire>,
    pending_capture: Option<capture_runtime::PendingCapture>,
    pending_evacuation: Option<evacuation::PendingEvacuation>,
    pending_combat: Option<combat::PendingCombat>,
    pending_squad: Option<combat::PendingSquad>,
    morale_runtime: morale_runtime::Runtime,
    cargo_runtime: cargo_transfer::Runtime,
    civil_move_runtime: civil_move::Runtime,
    production_runtime: production_runtime::Runtime,
    commerce_runtime: commerce_runtime::Runtime,
    commerce_player_runtime: commerce_player_runtime::Runtime,
    commerce_trade: commerce_trade::Runtime,
    technology_runtime: technology_runtime::Runtime,
    caravan_runtime: caravan_runtime::Runtime,
    demography_runtime: demography_runtime::Runtime,
    development_runtime: development::Runtime,
    payment_runtime: payment_mandate_runtime::Runtime,
    registry_runtime: registry_runtime::Runtime,
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
                    | PendingKind::BindInventory { .. }
            )
        }) || self
            .pending_counter
            .as_ref()
            .is_some_and(|pending| pending.intent.is_bind())
            || self.queued.iter().any(|queued| queued.intent.is_bind())
            || self.pending_warehouse_write.is_some()
            || self.warehouse_read_request.is_some()
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

    fn attach(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        entity_uuid: &str,
    ) -> Vec<Command> {
        if !valid_settlement_name(settlement) {
            return vec![message_session(
                session,
                "Settlement name must be 2..16 lowercase letters, digits, _ or -.",
            )];
        }
        self.begin_registry_read(RegistryRead::Attach {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            entity_uuid: entity_uuid.to_owned(),
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
        let Some((purpose_name, extent)) = parse_survey_purpose(purpose) else {
            return vec![message_session(
                session,
                "Purpose must be plot, expand, or restore.",
            )];
        };
        self.begin_registry_read(RegistryRead::Survey {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
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
        anchor: world_structures::BlockPosition,
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
            || self.pending_attachment_query.is_some()
            || self.cargo_runtime.busy()
            || self.civil_move_runtime.busy()
            || self.pending.values().any(|pending| {
                matches!(
                    &pending.kind,
                    PendingKind::CargoTransfer | PendingKind::CivilMove
                )
            })
            || self.pending_counter.as_ref().is_some_and(|pending| {
                matches!(
                    &pending.intent,
                    OperationIntent::CargoTransfer { .. } | OperationIntent::CivilMove { .. }
                )
            })
            || self.queued.iter().any(|pending| {
                matches!(
                    &pending.intent,
                    OperationIntent::CargoTransfer { .. } | OperationIntent::CivilMove { .. }
                )
            })
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
            || self
                .pending
                .values()
                .any(|pending| matches!(&pending.kind, PendingKind::AttachResident { .. }))
            || self.pending_counter.as_ref().is_some_and(|pending| {
                matches!(
                    pending.intent,
                    OperationIntent::AttachResident { .. } | OperationIntent::Recognize { .. }
                )
            })
            || self
                .queued
                .iter()
                .any(|queued| matches!(queued.intent, OperationIntent::AttachResident { .. }))
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
        self.registry_read(&request)
    }

    fn registry_get_answer(
        &mut self,
        pending: PendingRegistryRead,
        answered: &impl StorageRead,
    ) -> Vec<Command> {
        let Some(record) = answered.record() else {
            return vec![message_session(
                pending.action.session(),
                "Settlement records are unavailable.",
            )];
        };
        let version = record.version;
        let Some(registry) = record.registry.cloned() else {
            return vec![message_session(
                pending.action.session(),
                "Complete settlement projection unavailable.",
            )];
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
            RegistryRead::Status {
                session,
                player,
                target,
                construction,
            } => {
                let Some(building) = registry
                    .settlements
                    .values()
                    .filter(|record| is_settlement_member(record, &player))
                    .flat_map(|record| record.buildings.iter())
                    .find_map(|(name, building)| {
                        (name == &target || building_origin_id(building) == target)
                            .then_some(building)
                    })
                else {
                    return vec![message_session(session, "Building not found or membership denied. Recognize or project it in your settlement first.")];
                };
                let id = building_origin_id(building).to_owned();
                let manual = matches!(building.origin, BuildingOrigin::Certificate { .. });
                if construction {
                    if manual {
                        return vec![message_session(session, "Manual buildings have no construction journal; use status for live capability.")];
                    }
                    return self.status_with_continuation(
                        session,
                        &id,
                        StatusContinuation::Construction {
                            structure_id: id.clone(),
                        },
                    );
                }
                let Some(request) = self.next_storage_request("settlement-building-status") else {
                    return vec![message_session(session, "Building query unavailable.")];
                };
                self.pending_status.insert(
                    request.clone(),
                    PendingStatus {
                        session,
                        continuation: StatusContinuation::Read {
                            workplace_id: id.clone(),
                            manual,
                        },
                    },
                );
                vec![if manual {
                    check_worksite(&request, &id)
                } else {
                    check_structure(&request, &id)
                }]
            }
            RegistryRead::WorkStatus {
                session,
                player,
                resident,
            } => {
                if !registry.settlements.values().any(|record| {
                    is_settlement_member(record, &player)
                        && record
                            .residents
                            .values()
                            .any(|entry| entry.handle == resident)
                }) {
                    return vec![message_session(
                        session,
                        "Work status refused: resident not found or membership denied.",
                    )];
                }
                let Some(request) = self.next_storage_request("settlement-work-status") else {
                    return vec![message_session(session, "Work status query unavailable.")];
                };
                self.pending_work_status
                    .insert(request.clone(), (session, resident.clone()));
                vec![solaris_plugin_sdk::query_npc_controls(
                    &request,
                    &[resident],
                )]
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
            RegistryRead::Cargo {
                session,
                player,
                settlement,
                alias,
                inward,
                resource,
                count,
            } => self.cargo_registry(
                session,
                &player,
                &settlement,
                &alias,
                inward,
                resource,
                count,
                &registry,
                version,
            ),
            RegistryRead::CivilMove {
                session,
                player,
                settlement,
                alias,
                target,
            } => self.civil_move_registry(session, &player, &settlement, &alias, target, &registry),
            RegistryRead::Attach {
                session,
                player,
                settlement,
                entity_uuid,
            } => {
                let Some(record) = registry.settlements.get(&settlement) else {
                    return vec![message_session(session, "Settlement not found.")];
                };
                if !can_build_settlement(record, &player) {
                    return vec![message_session(
                        session,
                        "Only the owner or a steward attaches existing people.",
                    )];
                }
                if let Some((alias, _)) = record.residents.iter().find(|(_, resident)| {
                    resident.native_entity_uuid.as_deref() == Some(entity_uuid.as_str())
                }) {
                    return vec![message_session(
                        session,
                        format!("Attached {alias} in {settlement}."),
                    )];
                }
                if registry.settlements.values().any(|other| {
                    other.residents.values().any(|resident| {
                        resident.native_entity_uuid.as_deref() == Some(entity_uuid.as_str())
                    })
                }) {
                    return vec![message_session(
                        session,
                        "That original person already belongs to another settlement.",
                    )];
                }
                let Some(site_id) = record.site_id.clone() else {
                    return vec![message_session(
                        session,
                        "Adopt the person's real village feature first.",
                    )];
                };
                let resident_name = match next_settlement_resident_name(record) {
                    Ok(name) => name,
                    Err(reason) => return vec![message_session(session, reason)],
                };
                self.begin_registry_site_query(RegistrySiteQuery::Attach {
                    session,
                    site_id,
                    attachment: PendingSettlementAttachment {
                        settlement,
                        resident_name,
                        registry,
                        version,
                        entity_uuid,
                    },
                })
            }
            RegistryRead::Survey {
                session,
                player,
                settlement,
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
                self.begin_registry_survey(
                    PendingRegistrySurvey {
                        request: String::new(),
                        session,
                        settlement,
                        purpose_name,
                        registry,
                        version,
                    },
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
                let (Some(expected_world_revision), Some(survey_token)) =
                    (record.survey_world_revision, record.survey_token.as_ref())
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
                self.submit_project(
                    OperationIntent::Prepare {
                        session,
                        dimension: STATION_DIMENSION.to_owned(),
                        blueprint,
                        anchor,
                        rotation,
                        survey_token: survey_token.clone(),
                        expected_world_revision,
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
            RegistryRead::Recognize {
                session,
                player,
                settlement,
                building_name,
                purpose,
                points,
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
                        "Points must be kind:x,y,z with kind entrance, station=<namespace:path>, block=<namespace:path>, or storage (example station=minecraft:crafting_table:3,61,3).",
                    )];
                };
                if let Err(reason) = validate_building_points(&purpose, &parsed_points) {
                    return vec![message_session(
                        session,
                        format!("Building recognition refused: {reason}."),
                    )];
                }
                self.submit(OperationIntent::Recognize {
                    session,
                    settlement,
                    building_name,
                    purpose: purpose.clone(),
                    footprint,
                    points: parsed_points,
                    survey_token,
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
        let Err(message) = record_settlement_attachment(
            &mut registry,
            &resident.settlement,
            &resident.resident_name,
            &resident.handle,
            resident.work_revision,
            &resident.entity_uuid,
        ) else {
            let Some(request) = self.next_storage_request("settlement-registry-attach-retry")
            else {
                return vec![message_session(
                    session,
                    "Original person bound, but settlement records are unavailable.",
                )];
            };
            self.pending_registry_write = Some(PendingRegistryWrite {
                request: request.clone(),
                session,
                message: format!(
                    "Attached {} in {}.",
                    resident.resident_name, resident.settlement
                ),
                resident: None,
            });
            return self.registry_publish(&request, version, &registry);
        };
        vec![message_session(
            session,
            format!("Original person bound, but {message}"),
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
        self.registry_publish(&request, version, &registry)
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
        self.registry_publish(&request, version, &registry)
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
            return self.registry_publish(&request, version, &registry);
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
            | RegistrySiteQuery::Attach { session, .. } => *session,
        };
        let Some(request) = self.next_storage_request("settlement-registry-sites") else {
            return vec![message_session(
                session,
                "Settlement sites are unavailable.",
            )];
        };
        let command = match &query {
            RegistrySiteQuery::Adopt { site_id, .. }
            | RegistrySiteQuery::Attach { site_id, .. } => query_feature(&request, site_id),
            RegistrySiteQuery::List { .. } => list_features(&request, None, SITE_PAGE),
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
            | RegistrySiteQuery::Attach { session, .. } => *session,
        };
        let OperationOutcome::Committed(committed) = &answered.outcome else {
            return vec![message_session(session, "The site query was refused.")];
        };
        match pending.query {
            RegistrySiteQuery::List { settlement, .. } => {
                let OperationPayload::WorldStructure(WorldStructureResult::Features(page)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The site listing returned an unexpected result.",
                    )];
                };
                let sites = page
                    .features
                    .iter()
                    .map(|feature| {
                        let min = feature.bounds.min;
                        let max = feature.bounds.max;
                        format!(
                            "{}@{},{},{} bounds={},{},{}:{},{},{}",
                            feature.feature_id,
                            min.x + (max.x - min.x) / 2,
                            max.y + 2,
                            min.z + (max.z - min.z) / 2,
                            min.x,
                            min.y,
                            min.z,
                            max.x,
                            max.y,
                            max.z,
                        )
                    })
                    .collect::<Vec<_>>();
                let text = if sites.is_empty() {
                    format!("No sites available for {settlement}.")
                } else {
                    format!("{settlement} sites: {}.", sites.join("; "))
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
                let OperationPayload::WorldStructure(WorldStructureResult::Feature(site)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The adoption site query returned an unexpected result.",
                    )];
                };
                if site.feature_id != site_id {
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
                let Err(message) = adopt_settlement_site(
                    &mut registry,
                    &settlement,
                    &site.feature_id,
                    committed.revision,
                ) else {
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
                        message: format!("Adopted site {} for {settlement}.", site.feature_id),
                        resident: None,
                    });
                    return self.registry_publish(&request, version, &registry);
                };
                vec![message_session(session, message)]
            }
            RegistrySiteQuery::Attach {
                site_id,
                attachment,
                ..
            } => {
                let OperationPayload::WorldStructure(WorldStructureResult::Feature(site)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "The original-person feature query returned an unexpected result.",
                    )];
                };
                if site.feature_id != site_id || !site.contents_known {
                    return vec![message_session(
                        session,
                        "That site's materialized contents are unavailable.",
                    )];
                }
                if !site
                    .entity_ids
                    .iter()
                    .any(|uuid| uuid == &attachment.entity_uuid)
                {
                    return vec![message_session(session, "The requested UUID is not an observed original person in this adopted native feature.")];
                }
                self.begin_attachment_query(session, attachment, None)
            }
        }
    }

    fn begin_attachment_query(
        &mut self,
        session: u64,
        attachment: PendingSettlementAttachment,
        cursor: Option<&str>,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("settlement-attachment-query") else {
            return vec![message_session(
                session,
                "Original-person binding query is unavailable.",
            )];
        };
        let command = solaris_plugin_sdk::query_npcs(&request, &[], cursor);
        self.pending_attachment_query = Some(PendingAttachmentQuery {
            request,
            session,
            attachment,
        });
        vec![command]
    }

    fn attachment_query_answer(
        &mut self,
        pending: PendingAttachmentQuery,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            return vec![message_session(
                pending.session,
                "Original-person binding query was refused.",
            )];
        };
        let OperationPayload::Npc(NpcResult::Page(page)) = &committed.payload else {
            return vec![message_session(
                pending.session,
                "Original-person binding query returned an unexpected result.",
            )];
        };
        if let Some(snapshot) = page
            .npcs
            .iter()
            .find(|snapshot| snapshot.entity_uuid == pending.attachment.entity_uuid)
        {
            // A crash after native admission but before registry CAS leaves the
            // same owned binding recoverable by its original UUID. It is not a
            // new claim, a new person, or a replayed admission receipt.
            return self.persist_attachment_snapshot(pending.session, pending.attachment, snapshot);
        }
        if let Some(cursor) = &page.cursor {
            return self.begin_attachment_query(pending.session, pending.attachment, Some(cursor));
        }
        self.submit(OperationIntent::AttachResident {
            session: pending.session,
            attachment: pending.attachment,
        })
    }

    fn begin_registry_survey(
        &mut self,
        mut pending: PendingRegistrySurvey,
        bounds: WorldBounds,
    ) -> Vec<Command> {
        let Some(request) = self.next_storage_request("settlement-registry-survey") else {
            return vec![message_session(
                pending.session,
                "Settlement survey is unavailable.",
            )];
        };
        pending.request = request.clone();
        self.pending_registry_survey = Some(pending);
        vec![survey(&request, STATION_DIMENSION, bounds)]
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
        let OperationPayload::WorldStructure(WorldStructureResult::Survey(survey)) =
            &committed.payload
        else {
            return vec![message_session(
                pending.session,
                "Settlement survey returned an unexpected result.",
            )];
        };
        // The player's own plot survey is the footprint `recognize` certifies
        // and the site `bind-manual` binds inside, exactly like the worker
        // survey: remember core's token, bounds and fence here too.
        self.survey_token = Some(survey.survey_token.clone());
        self.survey_bounds = Some(survey.bounds.clone());
        self.survey_world_revision = Some(survey.world_revision);
        let mut registry = pending.registry;
        if let Some(record) = registry.settlements.get_mut(&pending.settlement) {
            record.survey_world_revision = Some(survey.world_revision);
        }
        let Err(message) = record_settlement_survey(
            &mut registry,
            &pending.settlement,
            &survey.survey_token,
            &pending.purpose_name,
            survey.biome_tags.clone(),
            survey.resource_tags.clone(),
        ) else {
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
            return self.registry_publish(&request, pending.version, &registry);
        };
        vec![message_session(pending.session, message)]
    }

    fn command(&mut self, invoked: &CommandInvoked, observed_tick: u64) -> Vec<Command> {
        if invoked.name != "settlement" {
            return Vec::new();
        }
        if let Some(commands) = self.resident_state_command(invoked) {
            return commands;
        }
        if let Some(commands) = self.handle_cargo_command(invoked) {
            return commands;
        }
        if let Some(commands) = self.handle_civil_move_command(invoked) {
            return commands;
        }
        if let Some(commands) = self.handle_production_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_commerce_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_player_contract_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_trade_command(invoked) {
            return commands;
        }
        if let Some(commands) = self.handle_technology_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_caravan_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_demography_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_development_command(invoked, observed_tick) {
            return commands;
        }
        if let Some(commands) = self.handle_paid_shipment_command(invoked, observed_tick) {
            return commands;
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
                None,
            ),
            ["hire", settlement, resident, role, loadout] => self.hire(
                invoked.session,
                &invoked.player,
                settlement,
                resident,
                role,
                Some(loadout),
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
            ["attach", settlement, entity_uuid] => self.attach(invoked.session, &invoked.player, settlement, entity_uuid),
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
                world_structures::BlockPosition {
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
                    world_structures::BlockPosition { x, y, z },
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
            ["status", target] => self.status(invoked.session, &invoked.player, target, false),
            ["construction-status", target] => self.status(invoked.session, &invoked.player, target, true),
            ["work-status", resident] => self.begin_registry_read(RegistryRead::WorkStatus {
                session: invoked.session,
                player: invoked.player.clone(),
                resident: (*resident).to_owned(),
            }),
            [kind @ ("harvest" | "mine" | "cut-tree"), resident, expected_revision, tool, min_x, min_y, min_z, max_x, max_y, max_z] => self.area_work(
                invoked.session,
                kind,
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
                "Usage: /settlement create/list/info/overview; adopt <settlement> <native-feature>; attach <settlement> <original-UUID>; survey/project/recognize/fund/advance; production/production-status/production-receipt/stop-work; technology-practice/technology-transfer; employment/trade-quote/trade-accept; caravan/caravan-status; hire/treat/dismiss/defend/march/retreat/halt; work-status/issue/haul/harvest/mine/cut-tree.",
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
                "Building name must be 3..16 lowercase letters, digits or _.",
            )];
        }
        if !valid_resource_id(purpose) {
            return vec![message_session(
                session,
                "Purpose must be a resource identifier of at most 128 bytes.",
            )];
        }
        self.begin_registry_read(RegistryRead::Recognize {
            session,
            player: player.to_owned(),
            settlement: settlement.to_owned(),
            building_name: building.to_owned(),
            purpose: purpose.to_owned(),
            points: points.iter().map(|point| (*point).to_owned()).collect(),
        })
    }

    fn bind_manual(&mut self, session: u64, x: &str, y: &str, z: &str) -> Vec<Command> {
        let [Ok(x), Ok(y), Ok(z)] = [x.parse::<i32>(), y.parse::<i32>(), z.parse::<i32>()] else {
            return vec![message_session(
                session,
                "Container coordinates must be signed 32-bit integers.",
            )];
        };
        let Some(bounds) = &self.survey_bounds else {
            return vec![message_session(
                session,
                "Survey a site before binding its containers.",
            )];
        };
        if x < bounds.min.x
            || x > bounds.max.x
            || y < bounds.min.y
            || y > bounds.max.y
            || z < bounds.min.z
            || z > bounds.max.z
        {
            return vec![message_session(
                session,
                "Container must lie within the surveyed site.",
            )];
        }
        if self.binding_pending() {
            return vec![message_session(
                session,
                "A warehouse binding is already pending.",
            )];
        }
        self.submit(OperationIntent::BindManual {
            session,
            position: world_structures::BlockPosition { x, y, z },
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

    fn status(
        &mut self,
        session: u64,
        player: &str,
        target: &str,
        construction: bool,
    ) -> Vec<Command> {
        if !valid_identity(target) {
            return vec![message_session(
                session,
                "Building name or ID must be 1..64 bytes.",
            )];
        }
        self.begin_registry_read(RegistryRead::Status {
            session,
            player: player.to_owned(),
            target: target.to_owned(),
            construction,
        })
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
                format!(
                    "{specialty} needs a physically usable {}.",
                    definition.blueprint_id
                ),
            )];
        };
        // Purpose is durable plugin policy; the native query verifies only
        // whether this building's physical points still support it.
        let manual = matches!(candidate.origin, BuildingOrigin::Certificate { .. });
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
        vec![if manual {
            check_worksite(&request, &status_id)
        } else {
            check_structure(&request, &status_id)
        }]
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
            return self.registry_publish(&request, version, &registry);
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
        let Some(endpoint) = warehouse_endpoint(&warehouse.handle) else {
            return vec![message_session(
                session,
                "Funding refused: invalid container identity.",
            )];
        };
        self.fund_from_inventory(session, endpoint)
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

    /// Give a bounded physical harvest, mining or tree-cutting order. The native
    /// executor verifies real cells/tool and preserves items in resident cargo
    /// for the existing `haul` command to deposit into the warehouse.
    #[allow(clippy::too_many_arguments)]
    fn area_work(
        &mut self,
        session: u64,
        kind: &str,
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
                "Work coordinates must be signed 32-bit integers.",
            )];
        };
        if [(min_x, max_x), (min_y, max_y), (min_z, max_z)]
            .into_iter()
            .any(|(min, max)| {
                max.checked_sub(min)
                    .is_none_or(|width| !(0..MAX_WORK_AREA_AXIS).contains(&width))
            })
        {
            return vec![message_session(
                session,
                "Work area must be ordered and no wider than 16 blocks on each axis.",
            )];
        }
        let work = AreaToolWork {
            tool: tool.to_owned(),
            area: WorkArea {
                dimension: STATION_DIMENSION.to_owned(),
                min: npcs::BlockPosition {
                    x: min_x,
                    y: min_y,
                    z: min_z,
                },
                max: npcs::BlockPosition {
                    x: max_x,
                    y: max_y,
                    z: max_z,
                },
            },
        };
        let (action, order) = match kind {
            "harvest" => ("Harvest", WorkOrder::Harvest(work)),
            "mine" => ("Mine", WorkOrder::Mine(work)),
            "cut-tree" => ("Cut-tree", WorkOrder::CutTree(work)),
            _ => unreachable!("area-work command kind already matched"),
        };
        self.submit(OperationIntent::AreaWork {
            session,
            resident: resident.to_owned(),
            expected_revision,
            action,
            order,
        })
    }

    fn submit(&mut self, intent: OperationIntent) -> Vec<Command> {
        self.enqueue_operation(intent, None)
    }

    fn submit_project(
        &mut self,
        intent: OperationIntent,
        project: PendingSettlementProject,
    ) -> Vec<Command> {
        self.enqueue_operation(intent, Some(project))
    }

    fn enqueue_operation(
        &mut self,
        intent: OperationIntent,
        project: Option<PendingSettlementProject>,
    ) -> Vec<Command> {
        let session = intent.session();
        self.queued.push_back(QueuedOperation { intent, project });
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
        });
        vec![storage_cas(
            &request,
            OPERATION_COUNTER_KEY,
            self.counter_version,
            next.to_string(),
        )]
    }

    fn storage_get(&mut self, answered: &StorageGetAnswered) -> Vec<Command> {
        if let Some(commands) = self.registry_native_get(answered) {
            return commands;
        }
        self.storage_read(answered)
    }

    fn storage_read(&mut self, answered: &impl StorageRead) -> Vec<Command> {
        if let Some(commands) = self.view_domain_get(answered) {
            return commands;
        }
        if let Some(commands) = self.resident_state_get(answered) {
            return commands;
        }
        if let Some(commands) = self.production_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.commerce_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.trade_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.technology_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.caravan_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.demography_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.development_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.paid_shipment_get_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.player_contract_get_answer(answered) {
            return self.view_player_contract_outputs(commands);
        }
        if let Some(commands) = self.morale_get_answer(answered) {
            return commands;
        }
        if self.warehouse_read_request.as_deref() == Some(answered.request()) {
            self.warehouse_read_request = None;
            let Some(record) = answered.record() else {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} warehouse-storage-unavailable"),
                );
                return Vec::new();
            };
            self.warehouse_version = record.version;
            self.warehouse = match record.value.as_deref() {
                None => None,
                Some(value) => match serde_json::from_str::<WarehouseBinding>(value) {
                    Ok(binding) if warehouse_endpoint(&binding.handle).is_some() => Some(binding),
                    _ => {
                        log(
                            LogLevel::Info,
                            &format!("{MARKER} warehouse-storage-unreadable"),
                        );
                        return Vec::new();
                    }
                },
            };
            self.warehouse_storage_ready = true;
            return Vec::new();
        }
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
            .is_some_and(|pending| pending.request == answered.request())
        {
            let pending = self
                .pending_registry_read
                .take()
                .expect("matched registry read is present");
            return self.registry_get_answer(pending, answered);
        }
        if self.counter_read_request.as_deref() != Some(answered.request()) {
            return Vec::new();
        }
        self.counter_read_request = None;
        let session = self.counter_read_session.take();
        let Some(record) = answered.record() else {
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
        self.storage_publish(answered)
    }

    fn storage_publish(&mut self, answered: &impl StoragePublish) -> Vec<Command> {
        if let Some(commands) = self.production_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.commerce_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.trade_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.technology_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.caravan_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.demography_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.development_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.paid_shipment_cas_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.player_contract_cas_answer(answered) {
            return self.view_player_contract_outputs(commands);
        }
        if let Some(commands) = self.morale_cas_answer(answered) {
            return commands;
        }
        if self
            .pending_warehouse_write
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request())
        {
            let pending = self
                .pending_warehouse_write
                .take()
                .expect("matched warehouse write");
            let StorageCasOutcome::Committed(version) = answered.outcome() else {
                self.warehouse_storage_ready = false;
                self.warehouse = None;
                self.warehouse_version = None;
                let mut commands = vec![message_session(
                    pending.session,
                    "Warehouse binding storage changed; bind again.",
                )];
                if let Some(request) = self.next_storage_request("settlement-warehouse-load") {
                    self.warehouse_read_request = Some(request.clone());
                    commands.push(storage_get(&request, WAREHOUSE_BINDING_KEY));
                }
                return commands;
            };
            self.warehouse_version = Some(*version);
            self.warehouse = Some(pending.binding);
            let binding = self.warehouse.as_ref().expect("committed binding");
            let provenance = match &binding.source {
                WarehouseSource::Authored(source) => format!(
                    "structure={} container={}",
                    source.structure_id, source.container_id
                ),
                WarehouseSource::VanillaVillage(source) => format!(
                    "village={} container={}",
                    source.site_id, source.container_id
                ),
                WarehouseSource::Manual(source) => format!(
                    "manual={},{},{}",
                    source.position.x, source.position.y, source.position.z
                ),
            };
            log(
                LogLevel::Info,
                &format!(
                    "{MARKER} warehouse {provenance} handle={} revision={}",
                    binding.handle, binding.revision
                ),
            );
            return vec![message_session(pending.session, "Warehouse bound.")];
        }
        if let Some(commands) = self.evacuation_cas_answer(answered) {
            return commands;
        }
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
            .is_some_and(|pending| pending.request == answered.request())
        {
            let pending = self
                .pending_registry_write
                .take()
                .expect("matched registry write is present");
            return match answered.outcome() {
                StorageCasOutcome::Committed(_) => {
                    let mut commands = vec![message_session(pending.session, pending.message)];
                    if let Some((request, settlement, target)) = self.pending_view_role.take() {
                        if request == answered.request() {
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
        if pending.request != answered.request() {
            self.pending_counter = Some(pending);
            return Vec::new();
        }
        let StorageCasOutcome::Committed(version) = answered.outcome() else {
            let session = pending.intent.session();
            self.queued.push_front(QueuedOperation {
                intent: pending.intent,
                project: pending.project,
            });
            self.counter = None;
            self.counter_version = None;
            return self.load_counter(Some(session));
        };
        self.counter = Some(pending.next);
        self.counter_version = Some(*version);
        let mut commands = self.issue_with_context(pending.intent, pending.project, pending.next);
        commands.extend(self.reserve_next_operation());
        commands
    }

    #[cfg(test)]
    fn issue(&mut self, intent: OperationIntent, counter: u64) -> Vec<Command> {
        self.issue_with_context(intent, None, counter)
    }

    fn issue_with_context(
        &mut self,
        intent: OperationIntent,
        project: Option<PendingSettlementProject>,
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
                vec![structure_status(&request, &structure_id)]
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
                vec![query_feature(&request, &site_id)]
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
                vec![register_worksite(
                    &request,
                    &operation_id,
                    world_structures::WorksiteRequest {
                        footprint,
                        interaction_points: points,
                        survey_token,
                        expected_world_revision,
                    },
                )]
            }
            OperationIntent::BindManual {
                session, position, ..
            } => self.query_binding(
                session,
                WarehouseSource::Manual(ManualWarehouseSource {
                    position: StoredPosition {
                        x: position.x,
                        y: position.y,
                        z: position.z,
                    },
                }),
                STATION_DIMENSION,
                position,
            ),
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
                let Some(endpoint) = warehouse_endpoint(&warehouse) else {
                    return vec![message_session(
                        session,
                        "Haul refused: invalid container identity.",
                    )];
                };
                let (source, destination) = match direction {
                    HaulDirection::Deposit => {
                        (InventoryEndpoint::NpcCarry(resident.clone()), endpoint)
                    }
                    HaulDirection::Issue(destination) => {
                        (endpoint, destination.endpoint(resident.clone()))
                    }
                };
                vec![assign_npc_work(
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
            OperationIntent::AreaWork {
                session,
                resident,
                expected_revision,
                action,
                order,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::AreaWork {
                            resident: resident.clone(),
                            action,
                        },
                    },
                );
                vec![assign_npc_work(
                    &request,
                    &operation_id,
                    &resident,
                    order,
                    WORK_UNITS,
                    expected_revision,
                )]
            }
            OperationIntent::Prepare {
                session,
                dimension,
                blueprint,
                anchor,
                rotation,
                survey_token,
                expected_world_revision,
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
                    &dimension,
                    &blueprint,
                    anchor,
                    rotation,
                    &survey_token,
                    expected_world_revision,
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
            OperationIntent::CargoTransfer {
                session,
                transfers,
                fences,
                guard,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::CargoTransfer,
                    },
                );
                vec![solaris_plugin_sdk::with_expected_plugin_storage_versions(
                    solaris_plugin_sdk::transfer_owned_items(
                        &request,
                        &operation_id,
                        session,
                        None,
                        transfers,
                        fences,
                    ),
                    vec![guard.wire()],
                )]
            }
            OperationIntent::CivilMove {
                session,
                handle,
                target,
                order_revision,
            } => {
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::CivilMove,
                    },
                );
                vec![if let Some(target) = target {
                    solaris_plugin_sdk::issue_npc_order(
                        &request,
                        &operation_id,
                        vec![handle],
                        vec![order_revision],
                        npcs::Order::Move(npcs::MoveOrder {
                            dimension: STATION_DIMENSION.to_owned(),
                            anchor: target,
                            heading_degrees: 0,
                            formation: npcs::Formation {
                                kind: npcs::FormationKind::Column,
                                spacing: 4,
                            },
                        }),
                    )
                } else {
                    solaris_plugin_sdk::cancel_npc_order(
                        &request,
                        &operation_id,
                        vec![handle],
                        vec![order_revision],
                    )
                }]
            }
            OperationIntent::AttachResident {
                session,
                attachment,
            } => {
                let entity_uuid = attachment.entity_uuid.clone();
                self.pending.insert(
                    request.clone(),
                    PendingOperation {
                        session,
                        operation_id: operation_id.clone(),
                        kind: PendingKind::AttachResident { attachment },
                    },
                );
                vec![solaris_plugin_sdk::adopt_npc(
                    &request,
                    &operation_id,
                    session,
                    &entity_uuid,
                    0,
                )]
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
                let Some(endpoint) = warehouse_endpoint(&warehouse) else {
                    return vec![message_session(
                        session,
                        "Treatment refused: invalid container identity.",
                    )];
                };
                vec![solaris_plugin_sdk::apply_npc_health_delta(
                    &request,
                    &operation_id,
                    &handle,
                    expected_revision,
                    endpoint,
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
                scope_guard,
            } => self.issue_squad(
                &request,
                &operation_id,
                session,
                handles,
                revisions,
                action,
                scope_guard,
            ),
        }
    }

    fn answered(&mut self, answered: &OperationAnswered) -> Vec<Command> {
        if let Some(commands) = self.registry_native_operation(answered) {
            return commands;
        }
        if let Some(commands) = self.civil_move_query_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.cargo_query_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.work_status_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.production_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.commerce_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.trade_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.technology_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.caravan_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.demography_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.development_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.paid_shipment_operation_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.player_contract_operation_answer(answered) {
            return self.view_player_contract_outputs(commands);
        }
        if let Some(commands) = self.morale_operation_answer(answered) {
            return commands;
        }
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
        if let Some(commands) = self.view_domain_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.resident_state_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.view_stock_answer(answered) {
            return commands;
        }
        if let Some(commands) = self.view_building_answer(answered) {
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
            .pending_attachment_query
            .as_ref()
            .is_some_and(|pending| pending.request == answered.request)
        {
            if answered.operation_id.is_some() {
                return Vec::new();
            }
            let pending = self
                .pending_attachment_query
                .take()
                .expect("matched attachment query");
            return self.attachment_query_answer(pending, &answered.outcome);
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
        let expected_operation_id = if matches!(
            &pending.kind,
            PendingKind::TreatmentQuery { .. }
                | PendingKind::Bind { .. }
                | PendingKind::BindVillage { .. }
                | PendingKind::BindInventory { .. }
        ) {
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
            PendingKind::BindInventory { source, endpoint } => {
                self.binding_inventory_answer(pending.session, source, endpoint, &answered.outcome)
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
            PendingKind::AreaWork { resident, action } => {
                self.area_work_answer(pending.session, &resident, action, &answered.outcome)
            }
            PendingKind::Prepare { project } => {
                self.prepare_answer(pending.session, project, &answered.outcome)
            }
            PendingKind::ReserveMaterials { structure_id } => {
                self.funding_reservation_answer(pending.session, &structure_id, &answered.outcome)
            }
            PendingKind::Advance => self.advance_answer(pending.session, &answered.outcome),
            PendingKind::AttachResident { attachment } => {
                self.attach_answer(pending.session, attachment, &answered.outcome)
            }
            PendingKind::CargoTransfer => {
                self.cargo_transfer_answer(pending.session, &answered.outcome)
            }
            PendingKind::CivilMove => self.civil_move_answer(pending.session, &answered.outcome),
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

    fn prepare_answer(
        &mut self,
        session: u64,
        project: Option<PendingSettlementProject>,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) =
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
                    if let Some(building) = project
                        .registry
                        .settlements
                        .get_mut(&project.settlement)
                        .and_then(|record| record.buildings.get_mut(&project.building_name))
                    {
                        store_building_points(
                            building,
                            &structure.dimension,
                            &structure.interaction_points,
                        );
                    }

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
                    return self.registry_publish(&request, project.version, &project.registry);
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

    fn apply_receipt(&mut self, receipt: &world_structures::StructureReceipt) {
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
                OperationPayload::WorldStructure(WorldStructureResult::Receipt(receipt)) => {
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
                OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) => {
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
                        if let OperationPayload::WorldStructure(WorldStructureResult::Structure(
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
        let session = pending.session;
        let OperationOutcome::Committed(committed) = outcome else {
            let OperationOutcome::Refused(refused) = outcome else {
                unreachable!()
            };
            return match pending.continuation {
                StatusContinuation::Specialize(mut specialization) => {
                    specialization.next_candidate += 1;
                    self.continue_specialization(specialization)
                }
                _ => vec![message_session(
                    session,
                    format!("Structure status refused: {:?}.", refused.reason),
                )],
            };
        };
        let (answered_id, accepted) = match &committed.payload {
            OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) => (
                structure.structure_id.as_str(),
                matches!(structure.state, StructureState::Committed),
            ),
            OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(check)) => {
                (check.worksite_id.as_str(), check.valid)
            }
            OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(check)) => {
                (check.structure_id.as_str(), check.valid)
            }
            _ => {
                return vec![message_session(
                    session,
                    "Structure status returned an unexpected result.",
                )]
            }
        };
        match pending.continuation {
            StatusContinuation::Specialize(mut specialization) => {
                let Some(candidate) = specialization
                    .workplace_candidates
                    .get(specialization.next_specialty)
                    .and_then(|candidates| candidates.get(specialization.next_candidate))
                else {
                    return vec![message_session(
                        session,
                        "Specialization status sequence is invalid.",
                    )];
                };
                if answered_id != candidate.id {
                    return vec![message_session(
                        session,
                        "Specialization status returned an unexpected structure.",
                    )];
                }
                let definition = specialty_definition(
                    &specialization.specialties[specialization.next_specialty],
                )
                .expect("validated specialty");
                let purpose_matches = specialization
                    .registry
                    .settlements
                    .get(&specialization.settlement)
                    .is_some_and(|record| {
                        record.buildings.values().any(|building| {
                            building_origin_id(building) == candidate.id
                                && building.purpose == definition.blueprint_id
                        })
                    });
                let variant_matches = matches!(
                    (&candidate.origin, &committed.payload),
                    (
                        BuildingOrigin::Structure { .. },
                        OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(_))
                    ) | (
                        BuildingOrigin::Certificate { .. },
                        OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(_))
                    )
                );
                if accepted && purpose_matches && variant_matches {
                    specialization.next_specialty += 1;
                    specialization.next_candidate = 0;
                } else {
                    specialization.next_candidate += 1;
                }
                self.continue_specialization(specialization)
            }
            StatusContinuation::Read {
                workplace_id,
                manual,
            } => {
                let points = match &committed.payload {
                    OperationPayload::WorldStructure(WorldStructureResult::WorksiteCheck(
                        check,
                    )) if manual => &check.blocked_points,
                    OperationPayload::WorldStructure(WorldStructureResult::StructureCheck(
                        check,
                    )) if !manual => &check.blocked_points,
                    _ => {
                        return vec![message_session(
                            session,
                            "Building status returned an unexpected result.",
                        )]
                    }
                };
                if answered_id != workplace_id {
                    return vec![message_session(
                        session,
                        "Building status did not match the requested identity.",
                    )];
                }
                vec![message_session(
                    session,
                    if accepted {
                        format!("Building {workplace_id} is usable.")
                    } else {
                        format!(
                            "Building {workplace_id} is unusable: {}.",
                            physical_points_explanation(points)
                        )
                    },
                )]
            }
            StatusContinuation::Construction { structure_id } => {
                let OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Construction status returned an unexpected result.",
                    )];
                };
                if structure.structure_id != structure_id {
                    return vec![message_session(
                        session,
                        "Construction status returned an unexpected identity.",
                    )];
                }
                self.apply_status(structure);
                vec![message_session(session, format!(
                    "Structure {} state={} watermark={} stage={} progress={} origin={},{},{} size={},{},{}. native={}",
                    structure.structure_id, structure_state_name(structure.state),
                    structure.watermark, structure.current_stage_index, structure.completed_work_units,
                    structure.origin.x, structure.origin.y, structure.origin.z,
                    structure.reserved_footprint.x, structure.reserved_footprint.y, structure.reserved_footprint.z,
                    serde_json::json!(structure),
                ))]
            }
            continuation @ (StatusContinuation::Fund | StatusContinuation::Advance) => {
                let OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Structure status returned an unexpected result.",
                    )];
                };
                self.apply_status(structure);
                if matches!(continuation, StatusContinuation::Fund) {
                    self.fund_warehouse(session)
                } else {
                    self.advance(session)
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
        let OperationOutcome::Committed(committed) = outcome else {
            return binding_refusal(session, outcome);
        };
        let OperationPayload::WorldStructure(WorldStructureResult::Structure(structure)) =
            &committed.payload
        else {
            return vec![message_session(
                session,
                "Warehouse binding returned an unexpected result.",
            )];
        };
        if structure.structure_id != structure_id
            || !matches!(structure.state, StructureState::Committed)
        {
            return vec![message_session(
                session,
                "Warehouse binding refused: structure is not committed.",
            )];
        }
        let Some(at) = container_point(&structure.interaction_points, container_id) else {
            return vec![message_session(
                session,
                "Warehouse binding refused: NotFound.",
            )];
        };
        self.query_binding(
            session,
            WarehouseSource::Authored(AuthoredWarehouseSource {
                structure_id: structure_id.to_owned(),
                container_id,
            }),
            &structure.dimension,
            at,
        )
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
                let OperationPayload::WorldStructure(WorldStructureResult::Worksite(certificate)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        "Building recognition returned an unexpected result.",
                    )];
                };
                let storage_positions = certificate
                    .interaction_points
                    .iter()
                    .filter_map(|point| match point {
                        WorksiteInteractionPoint::Container(at) => Some(StoredPosition {
                            x: at.x,
                            y: at.y,
                            z: at.z,
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let Err(message) = record_settlement_certificate(
                    &mut registry,
                    settlement,
                    building_name,
                    purpose,
                    BuildingOrigin::Certificate {
                        certificate_id: certificate.worksite_id.clone(),
                    },
                    certificate.revision,
                    storage_positions,
                ) else {
                    if let Some(building) =
                        registry.settlements.get_mut(settlement).and_then(|record| {
                            record.buildings.values_mut().find(|building| {
                                building_origin_id(building) == certificate.worksite_id
                            })
                        })
                    {
                        building.worksite_id = Some(certificate.worksite_id.clone());
                        store_building_points(
                            building,
                            STATION_DIMENSION,
                            &certificate.interaction_points,
                        );
                    }

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
                            building_name, settlement, certificate.worksite_id
                        ),
                        resident: None,
                    });
                    return self.registry_publish(&request, version, &registry);
                };
                vec![message_session(session, message)]
            }
            OperationOutcome::Refused(refused) => vec![message_session(
                session,
                format!(
                    "Building recognition refused: {}.",
                    building_failure_explanation(refused.reason)
                ),
            )],
        }
    }

    fn bind_village_answer(
        &mut self,
        session: u64,
        site_id: &str,
        container_id: u32,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            return binding_refusal(session, outcome);
        };
        let OperationPayload::WorldStructure(WorldStructureResult::Feature(feature)) =
            &committed.payload
        else {
            return vec![message_session(
                session,
                "Warehouse binding returned an unexpected result.",
            )];
        };
        if feature.feature_id != site_id || !feature.contents_known {
            return vec![message_session(
                session,
                "Warehouse binding refused: village contents are unavailable.",
            )];
        }
        let Some(at) = container_point(&feature.interaction_points, container_id) else {
            return vec![message_session(
                session,
                "Warehouse binding refused: NotFound.",
            )];
        };
        self.query_binding(
            session,
            WarehouseSource::VanillaVillage(VillageWarehouseSource {
                site_id: site_id.to_owned(),
                container_id,
            }),
            &feature.dimension,
            at,
        )
    }

    fn query_binding(
        &mut self,
        session: u64,
        source: WarehouseSource,
        dimension: &str,
        at: world_structures::BlockPosition,
    ) -> Vec<Command> {
        let endpoint =
            InventoryEndpoint::Container(solaris_plugin_sdk::inventories::ContainerEndpoint {
                dimension: dimension.to_owned(),
                position: solaris_plugin_sdk::types::BlockPosition {
                    x: at.x,
                    y: at.y,
                    z: at.z,
                },
            });
        let Some(request) = self.next_storage_request("settlement-binding-inventory") else {
            return vec![message_session(
                session,
                "Warehouse binding query unavailable.",
            )];
        };
        self.pending.insert(
            request.clone(),
            PendingOperation {
                session,
                operation_id: String::new(),
                kind: PendingKind::BindInventory {
                    source,
                    endpoint: endpoint.clone(),
                },
            },
        );
        vec![query_owned_inventory(&request, endpoint, None)]
    }

    fn binding_inventory_answer(
        &mut self,
        session: u64,
        source: WarehouseSource,
        endpoint: InventoryEndpoint,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            return binding_refusal(session, outcome);
        };
        let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
            &committed.payload
        else {
            return vec![message_session(
                session,
                "Warehouse binding returned an unexpected result.",
            )];
        };
        if !same_inventory_endpoint(&snapshot.endpoint, &endpoint) {
            return vec![message_session(
                session,
                "Warehouse binding returned an unexpected endpoint.",
            )];
        }
        if !self.warehouse_storage_ready {
            return vec![message_session(
                session,
                "Warehouse durable storage is unavailable.",
            )];
        }
        let Some(handle) = container_identity(&endpoint) else {
            unreachable!()
        };
        let binding = WarehouseBinding {
            handle,
            source,
            revision: self.warehouse_version.unwrap_or(0).saturating_add(1),
        };
        let Ok(value) = serde_json::to_string(&binding) else {
            return vec![message_session(
                session,
                "Warehouse durable storage is unavailable.",
            )];
        };
        let Some(request) = self.next_storage_request("settlement-binding-write") else {
            return vec![message_session(
                session,
                "Warehouse durable storage is unavailable.",
            )];
        };
        self.pending_warehouse_write = Some(PendingWarehouseWrite {
            request: request.clone(),
            session,
            binding,
        });
        vec![storage_cas(
            &request,
            WAREHOUSE_BINDING_KEY,
            self.warehouse_version,
            value,
        )]
    }

    fn haul_answer(
        &mut self,
        session: u64,
        resident: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::NpcOrder(NpcOrderResult::Work(assignment)) =
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

    fn area_work_answer(
        &mut self,
        session: u64,
        resident: &str,
        action: &str,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed) => {
                let OperationPayload::NpcOrder(NpcOrderResult::Work(assignment)) =
                    &committed.payload
                else {
                    return vec![message_session(
                        session,
                        format!("{action} returned an unexpected result."),
                    )];
                };
                if assignment.handle.as_str() != resident {
                    return vec![message_session(
                        session,
                        format!("{action} returned an unexpected result."),
                    )];
                }
                log(LogLevel::Info, &work_marker(action, assignment));
                vec![message_session(
                    session,
                    format!("{action} assigned. Revision {}.", assignment.revision),
                )]
            }
            OperationOutcome::Refused(refused) => {
                log(
                    LogLevel::Info,
                    &format!("{MARKER} {action}-refused {}", failure_name(refused.reason)),
                );
                vec![message_session(
                    session,
                    format!("{action} refused: {:?}.", refused.reason),
                )]
            }
        }
    }

    /// Bind only the exact loaded original UUID accepted by native adoption.
    /// The registry retry never repeats adoption or creates another person.
    fn attach_answer(
        &mut self,
        session: u64,
        attachment: PendingSettlementAttachment,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        let OperationOutcome::Committed(committed) = outcome else {
            let OperationOutcome::Refused(refused) = outcome else {
                unreachable!()
            };
            return vec![message_session(
                session,
                format!("Original person attachment refused: {:?}.", refused.reason),
            )];
        };
        let OperationPayload::Npc(NpcResult::Snapshot(snapshot)) = &committed.payload else {
            return vec![message_session(
                session,
                "Native attachment returned an unexpected result.",
            )];
        };
        self.persist_attachment_snapshot(session, attachment, snapshot)
    }

    fn persist_attachment_snapshot(
        &mut self,
        session: u64,
        mut attachment: PendingSettlementAttachment,
        snapshot: &npcs::NpcSnapshot,
    ) -> Vec<Command> {
        if snapshot.entity_uuid != attachment.entity_uuid
            || snapshot.lifecycle != npcs::NpcLifecycle::AliveLoaded
        {
            return vec![message_session(
                session,
                "Native attachment did not accept the exact living loaded original UUID.",
            )];
        }
        if let Err(reason) = record_settlement_attachment(
            &mut attachment.registry,
            &attachment.settlement,
            &attachment.resident_name,
            &snapshot.handle,
            0,
            &snapshot.entity_uuid,
        ) {
            return vec![message_session(session, reason)];
        }

        let Some(request) = self.next_storage_request("settlement-registry-attach") else {
            return vec![message_session(
                session,
                "Original person bound; registry request counter exhausted.",
            )];
        };
        self.pending_registry_write = Some(PendingRegistryWrite {
            request: request.clone(),
            session,
            message: format!(
                "Attached {} in {}.",
                attachment.resident_name, attachment.settlement
            ),
            resident: Some(PendingResidentPersistence {
                settlement: attachment.settlement,
                resident_name: attachment.resident_name,
                handle: snapshot.handle.clone(),
                work_revision: 0,
                entity_uuid: snapshot.entity_uuid.clone(),
            }),
        });
        self.registry_publish(&request, attachment.version, &attachment.registry)
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

    fn work_status_answer(&mut self, answer: &OperationAnswered) -> Option<Vec<Command>> {
        let (session, resident) = self.pending_work_status.remove(&answer.request)?;
        let OperationOutcome::Committed(committed) = &answer.outcome else {
            return Some(vec![message_session(
                session,
                "Work status refused by the native authority.",
            )]);
        };
        let OperationPayload::NpcOrder(NpcOrderResult::Controls(page)) = &committed.payload else {
            return Some(vec![message_session(
                session,
                "Work status returned an unexpected result.",
            )]);
        };
        let Some(control) = page.npcs.iter().find(|control| control.handle == resident) else {
            return Some(vec![message_session(
                session,
                "Work status did not include the requested resident.",
            )]);
        };
        let detail = control.work.as_ref().map_or_else(
            || "state=None progress=0/0".to_owned(),
            |work| {
                format!(
                    "state={:?} progress={}/{} reason={:?} due_tick={:?} receipt_sequence={} acknowledged_sequence={} retained_receipts={}",
                    work.state,
                    work.work_units_done,
                    work.work_units_planned,
                    work.reason,
                    work.due_tick,
                    work.receipts
                        .last()
                        .map_or(work.acknowledged_sequence, |receipt| receipt.sequence),
                    work.acknowledged_sequence,
                    work.receipts.len(),
                )
            },
        );
        Some(vec![message_session(
            session,
            format!("Work {resident} revision={} {detail}.", control.revision,),
        )])
    }

    fn clear_unapplied(&mut self) {
        self.morale_runtime.reset_pending();
        self.cargo_runtime.reset();
        self.civil_move_runtime.reset();
        self.pending.clear();
        self.queued.clear();
        self.pending_warehouse_write = None;
        self.pending_counter = None;
        self.pending_registry_read = None;
        self.pending_registry_write = None;
        self.pending_hire = None;
        self.pending_dismiss = None;
        self.pending_registry_sites = None;
        self.pending_attachment_query = None;
        self.pending_registry_survey = None;
        self.counter_read_request = None;
        self.pending_status.clear();
        self.pending_work_status.clear();
        self.resident_reads.clear();
        self.pending_funding = None;
        self.counter_read_session = None;
        self.pending_view_players.clear();
        self.requested_view_panes.clear();
        self.pending_view_project_players.clear();
        self.pending_view_role = None;
        self.pending_view_stock.clear();
        self.pending_view_buildings.clear();
        self.views.retain(|_, state| state.instance.is_some());
    }
}

impl Plugin for Settlements {
    fn configure(
        &mut self,
        _config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Ok(Some(solaris_plugin_sdk::StartupContribution {
            placement: None,
            trees: None,
            clay: None,
            spawning: None,
            items: Some(military_catalog::authored_items()),
            work_recipes: Some(production::authored_recipes()),
        }))
    }

    fn init(&mut self, _config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.cargo_runtime.reset();
        self.civil_move_runtime.reset();
        self.warehouse = None;
        self.warehouse_version = None;
        self.warehouse_storage_ready = false;
        self.pending_warehouse_write = None;
        self.warehouse_read_request = None;
        self.survey_token = None;
        self.pending_status.clear();
        self.pending_work_status.clear();
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
        self.pending_attachment_query = None;
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
        self.resident_reads.clear();
        self.requested_view_panes.clear();
        self.view_tick = 0;
        self.pending_view_role = None;
        self.pending_view_players.clear();
        self.pending_view_project_players.clear();
        self.pending_view_stock.clear();
        self.pending_view_buildings.clear();
        self.storage_sequence = 0;
        let mut commands = self.load_counter(None);
        let request = self
            .next_storage_request("settlement-warehouse-load")
            .expect("fresh storage sequence");
        self.warehouse_read_request = Some(request.clone());
        commands.push(storage_get(&request, WAREHOUSE_BINDING_KEY));
        commands.extend(self.recover_hires());
        commands.extend(self.morale_tick(0));
        commands.extend(self.production_init());
        commands.extend(self.commerce_init());
        commands.extend(self.player_contract_init());
        commands.extend(self.trade_init());
        commands.extend(self.technology_init());
        commands.extend(self.caravan_init());
        commands.extend(self.demography_init());
        commands.extend(self.paid_shipment_init());
        Ok(commands)
    }

    fn on_events(
        &mut self,
        context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        self.commerce_player_runtime.now = context.tick;
        for event in events {
            self.view_tick = context.tick;
            match event {
                Event::CommandBatchRejected => {
                    self.clear_unapplied();
                    commands.extend(self.morale_tick(context.tick));
                    commands.extend(self.production_batch_rejected());
                    commands.extend(self.commerce_init());
                    commands.extend(self.player_contract_init());
                    commands.extend(self.trade_init());
                    commands.extend(self.technology_init());
                    commands.extend(self.caravan_init());
                    commands.extend(self.demography_rejected());
                    commands.extend(self.development_rejected());
                    commands.extend(self.paid_shipment_init());
                }
                Event::TimerFired(timer) if timer.timer_id == "settlement-views" => {
                    commands.extend(self.view_timer());
                }
                Event::TimerFired(timer) if timer.timer_id == "settlement-morale" => {
                    commands.extend(self.morale_tick(timer.fired_tick));
                }
                Event::TimerFired(timer) if timer.timer_id == production::TIMER => {
                    commands.extend(self.production_timer(timer.fired_tick));
                }
                Event::TimerFired(timer) if timer.timer_id == commerce_runtime::TIMER => {
                    commands.extend(self.commerce_timer(timer.fired_tick));
                }
                Event::TimerFired(timer) if timer.timer_id == commerce_player_runtime::TIMER => {
                    let player_contract = self.player_contract_timer(timer.fired_tick);
                    commands.extend(self.view_player_contract_outputs(player_contract));
                }
                Event::TimerFired(timer) if timer.timer_id == caravan_runtime::TIMER => {
                    commands.extend(self.caravan_timer(timer.fired_tick));
                }
                Event::TimerFired(timer) if timer.timer_id == demography_runtime::TIMER => {
                    commands.extend(self.demography_timer(timer.fired_tick));
                }
                Event::TimerFired(timer)
                    if timer.timer_id == payment_mandate_runtime::PAYMENT_TIMER =>
                {
                    commands.extend(self.paid_shipment_timer(timer.fired_tick));
                }
                Event::PlayerLeft(left) => self.view_left(left.session),
                Event::LoaderViewRequest(request) => commands.extend(self.view_request(request)),
                Event::ClientSelectionStarted(answer) => {
                    commands.extend(self.view_selection_started(answer))
                }
                Event::OnlinePlayersAnswered(answer) => {
                    if let Some(player_contract) = self.player_contract_players_answer(answer) {
                        commands.extend(self.view_player_contract_outputs(player_contract));
                    } else {
                        commands.extend(self.view_players_answered(answer));
                    }
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
        (InventoryEndpoint::Container(left), InventoryEndpoint::Container(right)) => {
            left.dimension == right.dimension
                && left.position.x == right.position.x
                && left.position.y == right.position.y
                && left.position.z == right.position.z
        }
        (InventoryEndpoint::NpcEquipment(left), InventoryEndpoint::NpcEquipment(right))
        | (InventoryEndpoint::NpcCarry(left), InventoryEndpoint::NpcCarry(right)) => left == right,
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
        WarehouseSource::Manual(source) => {
            let Some(InventoryEndpoint::Container(endpoint)) = warehouse_endpoint(&binding.handle) else {
                return false;
            };
            record.buildings.values().any(|building| {
                building.dimension.as_deref() == Some(endpoint.dimension.as_str())
                    && building.storage_positions.iter().any(|stored| {
                        stored.x == source.position.x && stored.y == source.position.y && stored.z == source.position.z
                    })
            })
        },
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
            survey_world_revision: None,
            survey_purpose: None,
            survey_biome_tags: Vec::new(),
            survey_resource_tags: Vec::new(),
            condition: None,
            evacuation: None,
            buildings: BTreeMap::new(),
            building_sequence: 0,
            specialties: Vec::new(),
            residents: BTreeMap::new(),
            home_claims: BTreeMap::new(),
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

fn parse_interaction_points(values: &[&str]) -> Option<Vec<WorksiteInteractionPoint>> {
    values
        .iter()
        .map(|value| {
            let (kind, coords) = value.rsplit_once(':')?;
            let [x, y, z] = parse_triple(coords)?;
            let at = world_structures::BlockPosition { x, y, z };
            match kind {
                "entrance" => Some(WorksiteInteractionPoint::Entrance(at)),
                "storage" => Some(WorksiteInteractionPoint::Container(at)),
                _ => {
                    if let Some(block) = kind.strip_prefix("block=") {
                        return valid_resource_id(block).then(|| {
                            WorksiteInteractionPoint::Block(world_structures::WorksiteBlock {
                                at,
                                block: block.to_owned(),
                            })
                        });
                    }
                    let capability = kind.strip_prefix("station=")?;
                    valid_resource_id(capability).then(|| {
                        WorksiteInteractionPoint::Workstation(
                            world_structures::WorksiteWorkstation {
                                at,
                                capability: capability.to_owned(),
                            },
                        )
                    })
                }
            }
        })
        .collect()
}

/// Classification is plugin policy; the native worksite owner validates only
/// the declared geometry. A label must not create a function without its
/// corresponding physical interaction points.
fn validate_building_points(
    purpose: &str,
    points: &[WorksiteInteractionPoint],
) -> Result<(), &'static str> {
    if !points
        .iter()
        .any(|point| matches!(point, WorksiteInteractionPoint::Entrance(_)))
    {
        return Err("declare an entrance with a walkable route to the building");
    }
    if matches!(
        purpose,
        "solaris:workshop" | "solaris:smithy" | "solaris:sawmill"
    ) && !points
        .iter()
        .any(|point| matches!(point, WorksiteInteractionPoint::Workstation(_)))
    {
        return Err("declare the real workstation for this production function");
    }
    if matches!(purpose, "solaris:warehouse" | "solaris:granary")
        && !points
            .iter()
            .any(|point| matches!(point, WorksiteInteractionPoint::Container(_)))
    {
        return Err("declare a real storage container; a building label grants no capacity");
    }
    Ok(())
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
    let origin_id = match &origin {
        BuildingOrigin::Structure { structure_id } => structure_id.as_str(),
        BuildingOrigin::Certificate { certificate_id } => certificate_id.as_str(),
    };
    if registry.settlements.iter().any(|(name, record)| {
        name != settlement
            && record
                .buildings
                .values()
                .any(|building| building_origin_id(building) == origin_id)
    }) {
        return Err("Building already belongs to another settlement.");
    }
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    // Idempotent on the certificate/structure authority: the same origin id
    // returns or refreshes its own record without minting a second building.
    if let Some(name) = record
        .buildings
        .iter()
        .find(|(_, building)| building_origin_id(building) == origin_id)
        .map(|(name, _)| name.clone())
    {
        let entry = record.buildings.get_mut(&name).expect("known building");
        if entry.purpose != purpose {
            return Err("Building purpose conflicts with its existing classification.");
        }
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
            station_positions: Vec::new(),
            entrance_positions: Vec::new(),
            worksite_id: None,
            dimension: None,
        },
    );
    record.building_sequence = record.building_sequence.saturating_add(1);
    record.revision = record.revision.saturating_add(1);
    Ok(())
}

fn record_settlement_attachment(
    registry: &mut SettlementRegistry,
    settlement: &str,
    resident_name: &str,
    handle: &str,
    work_revision: u64,
    entity_uuid: &str,
) -> Result<(), &'static str> {
    if registry.settlements.iter().any(|(name, record)| {
        record.residents.iter().any(|(alias, resident)| {
            (resident.handle == handle
                || resident.native_entity_uuid.as_deref() == Some(entity_uuid))
                && (name != settlement || alias != resident_name)
        })
    }) {
        return Err("Original person already belongs to another roster entry.");
    }
    record_settlement_resident(registry, settlement, resident_name, handle, work_revision)?;
    let resident = registry
        .settlements
        .get_mut(settlement)
        .expect("recorded settlement")
        .residents
        .get_mut(resident_name)
        .expect("recorded resident");
    if resident
        .native_entity_uuid
        .as_deref()
        .is_some_and(|uuid| uuid != entity_uuid)
    {
        return Err("Native UUID cannot change under an existing alias.");
    }
    resident.native_entity_uuid = Some(entity_uuid.to_owned());
    Ok(())
}

fn store_building_points(
    building: &mut SettlementBuilding,
    dimension: &str,
    points: &[WorksiteInteractionPoint],
) {
    building.dimension = Some(dimension.to_owned());
    building.entrance_positions = points
        .iter()
        .filter_map(|point| {
            let WorksiteInteractionPoint::Entrance(at) = point else {
                return None;
            };
            Some(StoredPosition {
                x: at.x,
                y: at.y,
                z: at.z,
            })
        })
        .collect();
    building.storage_positions = points
        .iter()
        .filter_map(|point| {
            let WorksiteInteractionPoint::Container(at) = point else {
                return None;
            };
            Some(StoredPosition {
                x: at.x,
                y: at.y,
                z: at.z,
            })
        })
        .collect();
    building.station_positions = points
        .iter()
        .filter_map(|point| {
            let WorksiteInteractionPoint::Workstation(station) = point else {
                return None;
            };
            Some(StoredPosition {
                x: station.at.x,
                y: station.at.y,
                z: station.at.z,
            })
        })
        .collect();
}

fn container_point(
    points: &[WorksiteInteractionPoint],
    index: u32,
) -> Option<world_structures::BlockPosition> {
    points
        .iter()
        .filter_map(|point| {
            let WorksiteInteractionPoint::Container(at) = point else {
                return None;
            };
            Some(*at)
        })
        .nth(index as usize)
}

fn binding_refusal(session: u64, outcome: &OperationOutcome) -> Vec<Command> {
    let OperationOutcome::Refused(refused) = outcome else {
        return vec![message_session(
            session,
            "Warehouse binding returned an unexpected result.",
        )];
    };
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

fn worksite_explanation(check: &world_structures::WorksiteCheck) -> String {
    physical_points_explanation(&check.blocked_points)
}

fn physical_points_explanation(points: &[world_structures::WorksitePointFailure]) -> String {
    let Some(point) = points.first() else {
        return "physical-conditions-unavailable".to_owned();
    };
    let reason = match point.reason {
        world_structures::WorksiteFailure::Unloaded => "unloaded",
        world_structures::WorksiteFailure::MissingBlock => "missing-block",
        world_structures::WorksiteFailure::MissingWorkstation => "missing-workstation",
        world_structures::WorksiteFailure::MissingContainer => "missing-container",
        world_structures::WorksiteFailure::BlockedEntrance => "blocked-entrance",
        world_structures::WorksiteFailure::Unwalkable => "unwalkable",
    };
    format!("{reason} at {},{},{}", point.at.x, point.at.y, point.at.z)
}

fn record_settlement_resident(
    registry: &mut SettlementRegistry,
    settlement: &str,
    resident_name: &str,
    handle: &str,
    work_revision: u64,
) -> Result<(), &'static str> {
    let Some(record) = registry.settlements.get_mut(settlement) else {
        return Err("Settlement not found.");
    };
    if record.residents.len() >= MAX_SETTLEMENT_RESIDENTS
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
        // A delayed attachment receipt must not change existing employment
        // or discard a durable hiring intent.
        existing.work_revision = existing.work_revision.max(work_revision);
    } else {
        record.residents.insert(
            resident_name.to_owned(),
            SettlementResident {
                handle: handle.to_owned(),
                native_entity_uuid: None,
                work_revision,
                role: None,
                employer: None,
                hiring: None,
                issued: None,
                dismissing: None,
                capturing: None,
                capture_recovery: None,
                captive_by: None,
                civil_work: None,
                morale: None,
                household: None,
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

fn parse_survey_purpose(value: Option<&str>) -> Option<(String, i32)> {
    match value.unwrap_or("plot").to_ascii_lowercase().as_str() {
        "plot" | "settlement" => Some(("settlement".to_owned(), 64)),
        "expand" | "expansion" => Some(("expansion".to_owned(), 128)),
        "restore" | "restoration" => Some(("restoration".to_owned(), 64)),
        _ => None,
    }
}

fn survey_bounds(x: f64, y: f64, z: f64, extent: i32) -> WorldBounds {
    let half = extent / 2;
    let min_x = ((x.floor() as i32) - half).clamp(-30_000_000, 30_000_000 - extent + 1);
    let min_z = ((z.floor() as i32) - half).clamp(-30_000_000, 30_000_000 - extent + 1);
    let min_y = ((y.floor() as i32) - 16).clamp(-64, 288);
    WorldBounds {
        min: world_structures::BlockPosition {
            x: min_x,
            y: min_y,
            z: min_z,
        },
        max: world_structures::BlockPosition {
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
fn work_marker(op: &str, assignment: &npcs::WorkAssignment) -> String {
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
        WorkState::Suspended => "suspended",
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

fn building_failure_explanation(failure: OperationFailure) -> &'static str {
    match failure {
        OperationFailure::Blocked => "blocked: an entrance, station or access route is unusable; repair the building and retry",
        OperationFailure::NotFound => "not-found: a required container or building is missing; restore it and retry",
        OperationFailure::Unloaded => "unloaded: the building's chunks must be loaded before its capability can be checked",
        OperationFailure::Forbidden => "forbidden: the building or protected land belongs to another owner",
        OperationFailure::StaleRevision => "stale-revision: the surveyed blocks changed; survey again before recognition",
        failure => failure_name(failure),
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
        WorkPauseReason::ReceiptBackpressure => "receipt-backpressure",
    }
}

fn lifecycle_name(lifecycle: npcs::NpcLifecycle) -> &'static str {
    match lifecycle {
        npcs::NpcLifecycle::AliveLoaded => "alive-loaded",
        npcs::NpcLifecycle::AliveUnloaded => "alive-unloaded",
        npcs::NpcLifecycle::Dead => "dead",
        npcs::NpcLifecycle::Released => "released",
    }
}

export_plugin!(Settlements);
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
