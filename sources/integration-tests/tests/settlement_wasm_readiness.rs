//! CP-014 current-Wasm regression for a complete physical warehouse chain.
//!
//! Drives the shipped `solaris-settlements` package through a materialized
//! production vanilla village: adopt and populate, issue a real tool, harvest
//! food, withdraw/craft/deposit a material, then fund and advance the unchanged
//! current blueprint. It reads the physical village chest before funding and
//! reopens both world and Wasm host to prove the cargo, resident, and building
//! survive restart.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mc_plugin_host::{
    DeploymentConfig, DiscoveryMode, HostQueues, PlayerSessions, PluginLimits, discover,
    start_deployment,
};
use mc_protocol::packets::Packet;
use mc_protocol::packets::play::{
    ClientboundCommands, ClientboundSystemChat, ConfirmTeleportation, ServerboundChatCommand,
    SynchronizePlayerPosition,
};
use mc_test_harness::client::Client;

const NAME: &str = "fundrepro";
const BLUEPRINT: &str = "solaris:watch_post";
const PLAYER: &str = "FundRepro";
const STARTING_OAK_PLANKS: i32 = 252;
const PRODUCED_OAK_PLANKS: i32 = 4;
const MATERIALS: &[(&str, i32)] = &[
    ("minecraft:cobblestone", 192),
    ("minecraft:iron_bars", 192),
    ("minecraft:iron_hoe", 1),
    ("minecraft:ladder", 192),
    ("minecraft:oak_door", 192),
    ("minecraft:oak_log", 192),
    ("minecraft:oak_planks", STARTING_OAK_PLANKS),
    ("minecraft:oak_wood", 1),
    ("minecraft:torch", 192),
];

type VillageChunk = (i32, i32);
type VillageChunkBounds = (VillageChunk, VillageChunk);

struct VillageWorksite {
    chest: mc_world::BlockPos,
    crop: mc_world::BlockPos,
    station: mc_world::BlockPos,
}

type MaterializedVillage = (
    mc_world::WorldStorage,
    VillageChunk,
    VillageChunk,
    VillageChunkBounds,
    VillageWorksite,
    Arc<mc_worldgen::TerrainGenerator>,
);

struct ServerSessions(mc_net::PlayerSessionsHandle);

impl PlayerSessions for ServerSessions {
    fn session_of(&self, player: &str) -> Option<u64> {
        self.0.session_of(player)
    }
}

/// Construct the same vanilla village plan source and terrain attachment used
/// by server startup, then seed a real generated chest as the test fixture's
/// starting warehouse stock. Cargo admission and chest accounting themselves
/// are covered separately by `settlement-cargo`; this scenario proves that the
/// deployed Wasm command path consumes that physical stock exactly once.
fn materialized_vanilla_village_world(
    world_dir: &Path,
    vanilla_dir: &Path,
    blocks: &Arc<mc_world::BlockRegistry>,
    items: &Arc<mc_data::items::ItemRegistry>,
    data: &Arc<mc_data::VanillaData>,
    tags: &Arc<mc_data::tags::TagsData>,
) -> MaterializedVillage {
    let villages = mc_data::Identifier::parse("minecraft:villages")
        .expect("static village structure-set identifier");
    let closure = Arc::new(
        mc_worldgen::village::load_village_closure(vanilla_dir, &villages, blocks)
            .expect("load production vanilla village closure"),
    );
    let cache_tags = Arc::new(mc_worldgen::vanilla_features::CacheTags::new(
        Arc::clone(blocks),
        Arc::clone(data),
        Arc::clone(tags),
    ));
    let semantics =
        mc_worldgen::vanilla_features::BlockSemantics::new(blocks.as_ref(), cache_tags.as_ref());
    let decor = Arc::new(
        mc_worldgen::village::VillageDecor::compile(vanilla_dir, &closure, &semantics)
            .expect("compile production vanilla village decor"),
    );
    let village_plans = Arc::new(
        mc_worldgen::village::plan_source::VillagePlanSource::new(
            closure,
            decor,
            81,
            Arc::clone(blocks),
            cache_tags.clone(),
            cache_tags,
        )
        .expect("validate production vanilla village source"),
    );
    let generator = Arc::new(
        mc_worldgen::TerrainGenerator::new(81, Arc::clone(blocks))
            .with_village_plans(village_plans),
    );
    let village = generator
        .village_sites_in_region((0, 0), (2_047, 31))
        .into_iter()
        .next()
        .expect("seed has a production vanilla village in the first settlement page");
    let start_chunk = village.start_chunk;
    let origin = (
        (village.min.x + village.max.x) / 2,
        (village.min.z + village.max.z) / 2,
    );
    let min_chunk_x = village.min.x.div_euclid(16);
    let max_chunk_x = village.max.x.div_euclid(16);
    let min_chunk_z = village.min.z.div_euclid(16);
    let max_chunk_z = village.max.z.div_euclid(16);
    let capacity = usize::try_from(max_chunk_x - min_chunk_x + 1)
        .expect("village chunk span is positive")
        * usize::try_from(max_chunk_z - min_chunk_z + 1).expect("village chunk span is positive");
    let capacity = capacity.saturating_add(1_024);
    let mut world =
        mc_world::WorldStorage::open_with_capacity(world_dir, Arc::clone(blocks), capacity)
            .expect("open world")
            .with_item_registry(Arc::clone(items))
            .with_generator(Arc::clone(&generator) as Arc<dyn mc_world::ChunkGenerator>);
    let mut containers = Vec::new();
    for x in min_chunk_x..=max_chunk_x {
        for z in min_chunk_z..=max_chunk_z {
            let position = mc_world::ChunkPos { x, z };
            let chunk = mc_world::ChunkGenerator::generate(generator.as_ref(), position);
            containers.extend(chunk.chests.keys().copied());
            world
                .insert_generated_chunk(position, chunk)
                .expect("materialize village chunk");
        }
    }
    containers.sort_by_key(|position| (position.x, position.y, position.z));
    let chest_position = containers
        .into_iter()
        .next()
        .expect("production vanilla village has a chest");
    let chest = seeded_chest(items);
    world
        .set_chest_block_entity(chest_position, chest)
        .expect("seed materialized village warehouse");
    let worksite = seed_village_worksite(&mut world, origin, chest_position, blocks);
    (
        world,
        origin,
        start_chunk,
        ((min_chunk_x, min_chunk_z), (max_chunk_x, max_chunk_z)),
        worksite,
        generator,
    )
}

/// Each slot obeys the authoritative item-stack cap. The 27-slot village chest
/// holds all stock needed by the unchanged revision-1 watch-post blueprint.
fn seeded_chest(items: &mc_data::items::ItemRegistry) -> mc_world::ChestBlockEntity {
    let mut chest = mc_world::ChestBlockEntity::default();
    let mut slot_index = 0;
    for (item, total) in MATERIALS {
        let item_id = items
            .id_of(&mc_data::Identifier::parse(*item).expect("valid fixture item"))
            .expect("fixture item is registered");
        let mut remaining = *total;
        while remaining > 0 {
            let count = remaining.min(64);
            let slot = chest
                .slots
                .get_mut(slot_index)
                .expect("fixture stock fits in one village chest");
            slot.item_id = item_id;
            slot.count = count;
            remaining -= count;
            slot_index += 1;
        }
    }
    chest
}

/// Keep the production village and its real chest, but give its resident one
/// real crop and one real crafting table to work through the existing command
/// surface.
fn seed_village_worksite(
    world: &mut mc_world::WorldStorage,
    origin: (i32, i32),
    chest: mc_world::BlockPos,
    blocks: &mc_world::BlockRegistry,
) -> VillageWorksite {
    let air = block_state(blocks, "minecraft:air", &[]);
    let farmland = block_state(blocks, "minecraft:farmland", &[]);
    let carrots = block_state(blocks, "minecraft:carrots", &[("age", "7")]);
    let crafting_table = block_state(blocks, "minecraft:crafting_table", &[]);
    let support_y = (mc_world::MIN_Y..=mc_world::MAX_Y)
        .rev()
        .find(|y| {
            world
                .get_block(mc_world::BlockPos {
                    x: origin.0,
                    y: *y,
                    z: origin.1,
                })
                .expect("read production village surface")
                .is_some_and(|state| state != air)
        })
        .expect("production village has a surface");
    let crop = mc_world::BlockPos {
        x: origin.0,
        y: support_y + 1,
        z: origin.1,
    };
    let station = mc_world::BlockPos {
        x: origin.0 + 1,
        y: support_y + 1,
        z: origin.1,
    };
    world
        .set_block_at(
            mc_world::BlockPos {
                y: support_y,
                ..crop
            },
            farmland,
        )
        .expect("seed production crop support")
        .expect("production village crop support is materialized");
    world
        .set_block_at(crop, carrots)
        .expect("seed mature production crop")
        .expect("production village crop is materialized");
    world
        .set_block_at(station, crafting_table)
        .expect("seed production crafting station")
        .expect("production village station is materialized");
    VillageWorksite {
        chest,
        crop,
        station,
    }
}

fn block_state(
    blocks: &mc_world::BlockRegistry,
    name: &str,
    properties: &[(&str, &str)],
) -> mc_world::BlockStateId {
    let id = mc_data::Identifier::parse(name).expect("static block identifier");
    if properties.is_empty() {
        return blocks
            .block(&id)
            .unwrap_or_else(|| panic!("missing fixture block {name}"))
            .default;
    }
    let properties = properties
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    blocks
        .by_name_and_props(&id, &properties)
        .unwrap_or_else(|| panic!("missing fixture state {name} {properties:?}"))
}

#[tokio::test]
#[ignore = "requires populated local 26.1.2 data/vanilla sidecars"]
async fn settlement_vanilla_village_warehouse_funds_and_commits_through_wasm() {
    let plugins = tempfile::tempdir().expect("plugin tempdir");
    copy_settlement_package(plugins.path());
    let limits = PluginLimits::default();
    let deployment = DeploymentConfig {
        root: plugins.path().to_path_buf(),
        mode: DiscoveryMode::Strict,
        expected: vec!["solaris-settlements".to_owned()],
        grants: std::collections::BTreeMap::from([(
            "solaris-settlements".to_owned(),
            vec![
                "storage".to_owned(),
                "player_queries".to_owned(),
                "inventory_transfers".to_owned(),
                "persistent_residents".to_owned(),
                "resident_work".to_owned(),
                "resident_orders".to_owned(),
                "world_sites".to_owned(),
                "structure_operations".to_owned(),
            ],
        )]),
        require_grants: true,
        precommit_hooks: Vec::new(),
    };
    let packages = discover(&deployment, &limits)
        .expect("strict Wasm deployment")
        .into_packages();
    let sessions = mc_net::PlayerSessionsHandle::new();
    let host = start_deployment(
        packages,
        limits,
        HostQueues::default(),
        Arc::new(ServerSessions(sessions.clone())),
    )
    .expect("start Wasm host");
    let boundary = host.boundary().clone();

    let vanilla_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../solaris/data/vanilla");
    let data = Arc::new(mc_data::load(&vanilla_dir).expect("load local vanilla data"));
    let tags = Arc::new(mc_data::tags::load(&vanilla_dir, &data).expect("load local vanilla tags"));
    let report_dir = vanilla_dir.join("reports");
    let block_report = mc_data::blocks::load_blocks_report(report_dir.join("blocks.json"))
        .expect("load local vanilla block report");
    let blocks = Arc::new(
        mc_world::BlockRegistry::from_report(&block_report).expect("local vanilla block registry"),
    );
    let item_report = mc_data::items::load_items_report(report_dir.join("registries.json"))
        .expect("load local vanilla item report");
    let items = Arc::new(mc_data::items::ItemRegistry::from_report(&item_report));
    let world_dir = tempfile::tempdir().expect("world tempdir");
    std::fs::create_dir_all(world_dir.path().join("region")).expect("create world region");
    let (world, village_origin, village_start_chunk, village_chunks, worksite, generator) =
        materialized_vanilla_village_world(
            world_dir.path(),
            &vanilla_dir,
            &blocks,
            &items,
            &data,
            &tags,
        );
    let world = Arc::new(tokio::sync::Mutex::new(world));
    let recipes = Arc::new(mc_data::recipes::solaris_required_recipes());
    let loot = Arc::new(mc_data::loot::builtin().clone());
    let item_facts = Arc::new(mc_data::item_components::solaris_required_item_facts());
    let block_facts = Arc::new(mc_data::block_facts::BlockFactsTable::from_blocks_report(
        &block_report,
    ));
    let entity_types = Arc::new(mc_data::entity_types::solaris_required_entity_types());
    let biome_spawns = Arc::new(mc_data::biomes::solaris_required_biome_spawn_rules());
    let shutdown = mc_net::ShutdownHandle::default();
    let cfg = mc_net::ServerConfig {
        tab_list: mc_net::TabListConfig::default(),
        bind_address: "127.0.0.1:0".parse().unwrap(),
        motd: "settlement warehouse wasm repro".into(),
        max_players: 1,
        view_distance: 10,
        data: Arc::clone(&data),
        blocks: Arc::clone(&blocks),
        world: Some(Arc::clone(&world)),
        tags: Arc::clone(&tags),
        recipes: Arc::clone(&recipes),
        loot: Arc::clone(&loot),
        block_light: None,
        items: Arc::clone(&items),
        item_facts: Arc::clone(&item_facts),
        block_facts: Arc::clone(&block_facts),
        entity_types: Arc::clone(&entity_types),
        biome_spawns: Arc::clone(&biome_spawns),
        chunk_pipeline: mc_net::ChunkPipelinePolicy::default(),
        random_tick: mc_net::RandomTickPolicy::default(),
        command_permissions: mc_net::CommandPermissionConfig::new([PLAYER], false),
        loader_manifest: None,
        shutdown: shutdown.clone(),
    };
    let bound = mc_net::bind_with_scripts(cfg, boundary)
        .await
        .expect("bind scripted server");
    bound.register_player_sessions(&sessions);
    let addr = bound.local_addr().expect("local address");
    let save = bound.save_handle();
    let server = tokio::spawn(async move { bound.serve().await });

    let mut client = connect_play(addr).await;
    command_and_wait(
        &mut client,
        &format!("settlement create {NAME} small"),
        &format!("Founded {NAME} (small hamlet)."),
    )
    .await;
    let site_line =
        command_and_wait_any(&mut client, &format!("settlement site {NAME}"), " sites: ").await;
    let village_prefix = format!(
        "village_{}_{}_",
        village_start_chunk.0, village_start_chunk.1
    );
    let site_id = site_line
        .split(": ")
        .nth(1)
        .into_iter()
        .flat_map(|sites| sites.split(','))
        .map(str::trim)
        .map(|site| site.split('@').next().unwrap_or_default())
        .find(|site| site.starts_with(&village_prefix))
        .expect("the production component lists the specifically materialized vanilla village");
    let (origin_x, origin_z) = village_origin;
    command_and_wait(
        &mut client,
        &format!("tp {origin_x} 250 {origin_z}"),
        &format!("Teleported to {origin_x} 250 {origin_z}"),
    )
    .await;
    wait_for_chunks(&mut client, village_chunks.0, village_chunks.1).await;
    command_and_wait(
        &mut client,
        &format!("settlement adopt {NAME} {site_id}"),
        &format!("Adopted site {site_id}"),
    )
    .await;
    command_and_wait(
        &mut client,
        &format!("settlement populate {NAME}"),
        &format!("Settled resident_1 in {NAME}."),
    )
    .await;
    let resident = resident_handle(
        &command_and_wait_any(
            &mut client,
            &format!("settlement info {NAME}"),
            "residents=",
        )
        .await,
    );
    command_and_wait(
        &mut client,
        &format!("settlement bind-village {site_id} 0"),
        "Warehouse bound.",
    )
    .await;

    let issued_tool = command_and_wait_any(
        &mut client,
        &format!("settlement issue {resident} 0 equipment minecraft:iron_hoe"),
        "Haul assigned. Revision ",
    )
    .await;
    let harvested = command_and_wait_any(
        &mut client,
        &format!(
            "settlement harvest {resident} {} minecraft:iron_hoe {} {} {} {} {} {}",
            assigned_work_revision(&issued_tool),
            worksite.crop.x,
            worksite.crop.y,
            worksite.crop.z,
            worksite.crop.x,
            worksite.crop.y,
            worksite.crop.z,
        ),
        "Harvest assigned. Revision ",
    )
    .await;
    let hauled_food = command_and_wait_any(
        &mut client,
        &format!(
            "settlement haul {resident} {} minecraft:carrot",
            assigned_work_revision(&harvested)
        ),
        "Haul assigned. Revision ",
    )
    .await;
    let issued_material = command_and_wait_any(
        &mut client,
        &format!(
            "settlement issue {resident} {} carry minecraft:oak_wood",
            assigned_work_revision(&hauled_food)
        ),
        "Haul assigned. Revision ",
    )
    .await;
    let crafted = command_and_wait_any(
        &mut client,
        &format!(
            "settlement craft {resident} {} minecraft:oak_planks 1 {} {} {}",
            assigned_work_revision(&issued_material),
            worksite.station.x,
            worksite.station.y,
            worksite.station.z,
        ),
        "Craft assigned. Revision ",
    )
    .await;
    let hauled_material = command_and_wait_any(
        &mut client,
        &format!(
            "settlement haul {resident} {} minecraft:oak_planks",
            assigned_work_revision(&crafted)
        ),
        "Haul assigned. Revision ",
    )
    .await;

    // Verify the production resident's food and material results in the actual
    // bound chest before the unchanged current blueprint reserves its stock.
    let chest = world
        .lock()
        .await
        .chest_block_entity(worksite.chest)
        .expect("read bound village warehouse")
        .expect("bound village chest remains materialized");
    assert!(
        chest_item_count(&chest, &items, "minecraft:carrot") > 0,
        "the resident's mature crop reaches the physical warehouse"
    );
    assert_eq!(
        chest_item_count(&chest, &items, "minecraft:oak_wood"),
        0,
        "the physical raw material leaves the warehouse for the resident craft"
    );
    assert_eq!(
        chest_item_count(&chest, &items, "minecraft:oak_planks"),
        STARTING_OAK_PLANKS + PRODUCED_OAK_PLANKS,
        "the crafted material returns to the physical warehouse exactly once"
    );

    command_and_wait(
        &mut client,
        &format!("settlement survey {NAME} plot"),
        &format!("{NAME} plot surveyed."),
    )
    .await;
    let projected = command_and_wait_any(
        &mut client,
        &format!("settlement project {NAME} {BLUEPRINT}"),
        "Projected ",
    )
    .await;
    let (project_name, structure_id) = projected
        .split_once(". Structure ")
        .expect("project response includes authoritative structure id");
    let building = project_name
        .split_whitespace()
        .nth(1)
        .expect("projected building name")
        .to_owned();
    let structure_id = structure_id
        .strip_suffix('.')
        .expect("structure id terminator");
    assert!(
        project_name.ends_with(&format!("for {NAME}")),
        "the current Wasm guest did not correlate the project to its player: {projected:?}"
    );
    command_and_wait(
        &mut client,
        &format!("settlement fund {NAME} {building}"),
        "Structure materials reserved.",
    )
    .await;
    let prepared = command_and_wait_any(
        &mut client,
        &format!("settlement status {structure_id}"),
        "Structure ",
    )
    .await;
    assert!(
        prepared.contains("state=prepared watermark=0"),
        "the funded structure has not yet committed a portion: {prepared:?}"
    );
    let origin = structure_coordinates(&prepared, "origin=");
    let size = structure_coordinates(&prepared, "size=");
    let before_blocks = structure_blocks(&mut *world.lock().await, origin, size);
    command_and_wait(
        &mut client,
        &format!("settlement advance {NAME} {building}"),
        "Structure portion committed.",
    )
    .await;
    let committed = command_and_wait_any(
        &mut client,
        &format!("settlement status {structure_id}"),
        "Structure ",
    )
    .await;
    assert!(
        committed.contains("state=running watermark=1"),
        "the structure reports one committed portion: {committed:?}"
    );
    let mut guard = world.lock().await;
    let (placed_position, placed_before) = before_blocks
        .into_iter()
        .find(|(pos, previous)| {
            guard.get_block(*pos).expect("read committed block") != Some(*previous)
        })
        .expect("the committed structure portion changes a physical block");
    let placed_after = guard
        .get_block(placed_position)
        .expect("read committed structure block")
        .expect("committed structure block stays loaded");
    assert_ne!(
        placed_after, placed_before,
        "the committed portion replaces one physical block"
    );
    drop(guard);

    assert!(
        save.save_all().await.is_ok(),
        "the canonical production chain saves before restart"
    );
    drop(client);
    shutdown.request();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shutdown timeout")
        .expect("server task")
        .expect("server result");
    tokio::task::spawn_blocking(move || host.stop())
        .await
        .expect("Wasm host stop task");
    drop(save);
    drop(world);

    let mut reopened =
        mc_world::WorldStorage::open_with_capacity(world_dir.path(), Arc::clone(&blocks), 2_048)
            .expect("reopen production village world")
            .with_item_registry(Arc::clone(&items))
            .with_generator(generator);
    let chest = reopened
        .chest_block_entity(worksite.chest)
        .expect("read restarted village warehouse")
        .expect("bound village chest persists");
    assert!(
        chest_item_count(&chest, &items, "minecraft:carrot") > 0,
        "the food deposit survives the restart"
    );
    assert_eq!(
        chest_item_count(&chest, &items, "minecraft:oak_wood"),
        0,
        "the crafted material input stays consumed after restart"
    );
    reopened
        .get_chunk(mc_world::ChunkPos {
            x: placed_position.x.div_euclid(16),
            z: placed_position.z.div_euclid(16),
        })
        .expect("load restarted building chunk")
        .expect("committed building chunk persists");
    assert_eq!(
        reopened
            .get_block(placed_position)
            .expect("read restarted building block"),
        Some(placed_after),
        "the physical committed structure portion survives restart"
    );
    for x in village_chunks.0.0..=village_chunks.1.0 {
        for z in village_chunks.0.1..=village_chunks.1.1 {
            reopened
                .get_chunk(mc_world::ChunkPos { x, z })
                .expect("load saved vanilla village chunk")
                .expect("the saved vanilla village chunk persists");
        }
    }

    let restarted_world = Arc::new(tokio::sync::Mutex::new(reopened));
    let restart_sessions = mc_net::PlayerSessionsHandle::new();
    let restart_limits = PluginLimits::default();
    let restarted_host = start_deployment(
        discover(&deployment, &restart_limits)
            .expect("strict Wasm redeployment")
            .into_packages(),
        restart_limits,
        HostQueues::default(),
        Arc::new(ServerSessions(restart_sessions.clone())),
    )
    .expect("restart Wasm host");
    let restart_shutdown = mc_net::ShutdownHandle::default();
    let restart_cfg = mc_net::ServerConfig {
        tab_list: mc_net::TabListConfig::default(),
        bind_address: "127.0.0.1:0".parse().unwrap(),
        motd: "settlement warehouse wasm restart".into(),
        max_players: 1,
        view_distance: 10,
        data,
        blocks,
        world: Some(restarted_world),
        tags,
        recipes,
        loot,
        block_light: None,
        items,
        item_facts,
        block_facts,
        entity_types,
        biome_spawns,
        chunk_pipeline: mc_net::ChunkPipelinePolicy::default(),
        random_tick: mc_net::RandomTickPolicy::default(),
        command_permissions: mc_net::CommandPermissionConfig::new([PLAYER], false),
        loader_manifest: None,
        shutdown: restart_shutdown.clone(),
    };
    let restarted_bound = mc_net::bind_with_scripts(restart_cfg, restarted_host.boundary().clone())
        .await
        .expect("bind restarted scripted server");
    restarted_bound.register_player_sessions(&restart_sessions);
    let restarted_addr = restarted_bound
        .local_addr()
        .expect("restarted local address");
    let restarted_server = tokio::spawn(async move { restarted_bound.serve().await });
    let mut restarted_client = connect_play(restarted_addr).await;
    command_and_wait(
        &mut restarted_client,
        &format!("settlement bind-village {site_id} 0"),
        "Warehouse bound.",
    )
    .await;
    let restarted_info = command_and_wait_any(
        &mut restarted_client,
        &format!("settlement info {NAME}"),
        "residents=",
    )
    .await;
    assert!(
        restarted_info.contains(&building),
        "the durable building survives the restarted host: {restarted_info:?}"
    );
    assert!(
        restarted_info.contains(&resident),
        "the durable resident survives the restarted host: {restarted_info:?}"
    );
    let restored_status = command_and_wait_any(
        &mut restarted_client,
        &format!("settlement status {structure_id}"),
        "Structure ",
    )
    .await;
    assert_eq!(
        restored_status, committed,
        "the core's exact construction progress survives the restarted host"
    );
    let restored_worker = command_and_wait_any(
        &mut restarted_client,
        &format!(
            "settlement haul {resident} {} minecraft:carrot",
            assigned_work_revision(&hauled_material)
        ),
        "Haul assigned. Revision ",
    )
    .await;
    assert!(
        assigned_work_revision(&restored_worker) > assigned_work_revision(&hauled_material),
        "the restored resident accepts new native work"
    );
    drop(restarted_client);
    restart_shutdown.request();
    tokio::time::timeout(Duration::from_secs(5), restarted_server)
        .await
        .expect("restarted server shutdown timeout")
        .expect("restarted server task")
        .expect("restarted server result");
    tokio::task::spawn_blocking(move || restarted_host.stop())
        .await
        .expect("restarted Wasm host stop task");
}

fn structure_coordinates(status: &str, field: &str) -> [i32; 3] {
    let value = status
        .split_once(field)
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .expect("status includes structure coordinates")
        .trim_end_matches('.');
    let parts = value
        .split(',')
        .map(|part| part.parse::<i32>().expect("integer structure coordinate"))
        .collect::<Vec<_>>();
    parts.try_into().expect("three structure coordinates")
}

fn structure_blocks(
    world: &mut mc_world::WorldStorage,
    origin: [i32; 3],
    size: [i32; 3],
) -> Vec<(mc_world::BlockPos, mc_world::BlockStateId)> {
    let mut blocks = Vec::new();
    for x in origin[0]..origin[0] + size[0] {
        for y in origin[1]..origin[1] + size[1] {
            for z in origin[2]..origin[2] + size[2] {
                let pos = mc_world::BlockPos { x, y, z };
                let state = world
                    .get_block(pos)
                    .expect("read structure footprint")
                    .expect("structure footprint is materialized");
                blocks.push((pos, state));
            }
        }
    }
    blocks
}

async fn connect_play(addr: std::net::SocketAddr) -> Client {
    let mut client = Client::connect(addr).await.expect("client connect");
    let _ = client
        .drive_login(addr, PLAYER)
        .await
        .expect("login succeeds");
    client.drive_configuration().await.expect("configuration");
    let _ = client.read_play_login().await.expect("play entry");
    let _: ClientboundCommands = client.read_typed().await.expect("Commands");
    let sync: SynchronizePlayerPosition = client.read_typed().await.expect("SyncPlayerPos");
    client
        .write_packet(&ConfirmTeleportation {
            teleport_id: sync.teleport_id,
        })
        .await
        .expect("ack teleport");
    client
}

fn resident_handle(info: &str) -> String {
    info.split_once("residents=resident_1=")
        .and_then(|(_, record)| record.split_once('@').map(|(handle, _)| handle))
        .filter(|handle| !handle.is_empty())
        .unwrap_or_else(|| panic!("settlement info did not expose resident_1: {info:?}"))
        .to_owned()
}

fn assigned_work_revision(message: &str) -> u64 {
    message
        .rsplit_once("Revision ")
        .and_then(|(_, revision)| revision.strip_suffix('.'))
        .and_then(|revision| revision.parse().ok())
        .unwrap_or_else(|| panic!("work assignment did not expose its revision: {message:?}"))
}

fn chest_item_count(
    chest: &mc_world::ChestBlockEntity,
    items: &mc_data::items::ItemRegistry,
    item: &str,
) -> i32 {
    let item_id = items
        .id_of(&mc_data::Identifier::parse(item).expect("static item identifier"))
        .unwrap_or_else(|| panic!("missing fixture item {item}"));
    chest
        .slots
        .iter()
        .filter(|slot| slot.item_id == item_id)
        .map(|slot| slot.count)
        .sum()
}

/// Wait until the full generated village footprint is resident on the client.
///
/// The server reports contents only after every covered chunk is resident, so
/// this waits for the same physical chunks the adoption query reads.
async fn wait_for_chunks(client: &mut Client, first: (i32, i32), last: (i32, i32)) {
    let mut wanted = Vec::new();
    for x in first.0..=last.0 {
        for z in first.1..=last.1 {
            wanted.push((x, z));
        }
    }
    let total = wanted.len();
    tokio::time::timeout(Duration::from_secs(60), async {
        while !wanted.is_empty() {
            let mut frame = client.read_frame().await.expect("chunk stream frame");
            if frame.id == SynchronizePlayerPosition::ID {
                let sync = SynchronizePlayerPosition::decode(&mut frame.body).expect("sync decode");
                let _ = client
                    .write_packet(&ConfirmTeleportation {
                        teleport_id: sync.teleport_id,
                    })
                    .await;
            } else if frame.id == mc_protocol::packets::play::LevelChunkWithLight::ID {
                let chunk =
                    mc_protocol::packets::play::LevelChunkWithLight::decode(&mut frame.body)
                        .expect("level chunk decode");
                wanted.retain(|cell| *cell != (chunk.chunk_x, chunk.chunk_z));
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "only {} of {total} village chunks streamed",
            total - wanted.len()
        )
    });
}

async fn command_and_wait(client: &mut Client, command: &str, expected: &str) {
    command_and_wait_any(client, command, expected).await;
}

/// Issue one player command and return the chat line containing `needle`.
async fn command_and_wait_any(client: &mut Client, command: &str, needle: &str) -> String {
    let before = drain_chat(client).await;
    client
        .write_packet(&ServerboundChatCommand {
            command: command.to_owned(),
        })
        .await
        .unwrap_or_else(|error| panic!("send /{command}: {error}"));
    let mut seen = Vec::new();
    let mut ids: Vec<i32> = Vec::new();
    let line = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut frame = client.read_frame().await.expect("frame during command");
            if ids.len() < 40 {
                ids.push(frame.id);
            }
            if frame.id == SynchronizePlayerPosition::ID {
                let sync = SynchronizePlayerPosition::decode(&mut frame.body).expect("sync decode");
                let _ = client
                    .write_packet(&ConfirmTeleportation {
                        teleport_id: sync.teleport_id,
                    })
                    .await;
            } else if frame.id == ClientboundSystemChat::ID {
                let chat = ClientboundSystemChat::decode(&mut frame.body).expect("chat decode");
                let message = literal_text(&chat.content_nbt);
                if seen.len() < 24 {
                    seen.push(message.clone());
                }
                if message.contains(needle) && !before.contains(&message) {
                    return message;
                }
            }
        }
    })
    .await;
    line.unwrap_or_else(|_| {
        panic!(
            "/{command} did not produce a line containing {needle:?}; new chat: {seen:?}; frame ids: {ids:x?}"
        )
    })
}

/// Read whatever chat is already queued without blocking (best-effort baseline).
async fn drain_chat(client: &mut Client) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        let Ok(frame) = client
            .read_frame_with_timeout(Duration::from_millis(1))
            .await
        else {
            break;
        };
        if frame.id == ClientboundSystemChat::ID {
            let mut frame = frame;
            if let Ok(chat) = ClientboundSystemChat::decode(&mut frame.body) {
                lines.push(literal_text(&chat.content_nbt));
            }
        }
    }
    lines
}

fn literal_text(component: &[u8]) -> String {
    let mut bytes = Bytes::copy_from_slice(component);
    let tag = mc_nbt::read_network(&mut bytes).expect("read text component nbt");
    let mc_nbt::Tag::Compound(fields) = tag else {
        panic!("text component root must be a compound");
    };
    fields
        .into_iter()
        .find_map(|(name, value)| match (name.as_str(), value) {
            ("text", mc_nbt::Tag::String(text)) => Some(text),
            _ => None,
        })
        .expect("literal text component")
}

fn copy_settlement_package(root: &Path) {
    let source = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("solaris-settlements");
    let source = source
        .canonicalize()
        .unwrap_or_else(|error| panic!("settlement package at {}: {error}", source.display()));
    let target = root.join("solaris-settlements");
    copy_dir(&source, &target);
}

fn copy_dir(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).expect("create plugin directory");
    for entry in std::fs::read_dir(source).expect("read plugin directory") {
        let entry = entry.expect("plugin directory entry");
        let file_type = entry.file_type().expect("plugin entry type");
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).expect("copy plugin file");
        }
    }
}
