//! First-party component workload lives beside its packages, not in core tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mc_plugin_host::{
    DeploymentConfig, DiscoveryMode, HostQueues, PlayerSessions, PluginHost, PluginLimits,
    discover, start_deployment,
};
use mc_script::{PlayerCommandAdmission, ScriptBoundary, ScriptPlayerContext, ScriptPlayerId};
const P7_STANDARD_PACK: [(&str, &[&str]); 5] = [
    ("solaris-permissions", &["storage"]),
    (
        "solaris-essentials",
        &["storage", "player_teleport", "player_queries"],
    ),
    ("solaris-economy", &["storage", "storage_batches"]),
    ("solaris-towns", &["storage", "zones", "player_queries"]),
    ("solaris-audit", &["storage"]),
];
const P7_PLAYER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const P7_SESSION: u64 = 7;

struct P7Sessions;

impl PlayerSessions for P7Sessions {
    fn session_of(&self, player: &str) -> Option<u64> {
        (player == P7_PLAYER).then_some(P7_SESSION)
    }
}

fn p7_plugins_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT") {
        return PathBuf::from(root);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("integration test crate belongs to the plugin repository")
        .to_path_buf()
}

fn p7_staged_standard_pack() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("stage standard-pack components");
    for (id, _) in P7_STANDARD_PACK {
        let source = p7_plugins_root().join(id);
        let target = root.path().join(id);
        std::fs::create_dir(&target).expect("create staged package directory");
        for name in ["plugin.toml", "plugin.wasm", "config.toml"] {
            std::fs::copy(source.join(name), target.join(name))
                .unwrap_or_else(|error| panic!("stage {id}/{name}: {error}"));
        }
    }
    root
}

fn p7_standard_pack_packages(
    root: &Path,
    limits: &PluginLimits,
) -> Vec<mc_plugin_host::LoadedPackage> {
    let deployment = DeploymentConfig {
        root: root.to_path_buf(),
        mode: DiscoveryMode::Strict,
        expected: P7_STANDARD_PACK
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect(),
        grants: P7_STANDARD_PACK
            .into_iter()
            .map(|(id, capabilities)| {
                (
                    id.to_owned(),
                    capabilities
                        .iter()
                        .map(|capability| (*capability).to_owned())
                        .collect(),
                )
            })
            .collect::<BTreeMap<_, _>>(),
        require_grants: true,
        precommit_hooks: Vec::new(),
    };
    discover(&deployment, limits)
        .expect("discover strict first-party component pack")
        .into_packages()
}

fn p7_player_context() -> ScriptPlayerContext {
    ScriptPlayerContext::try_new(P7_PLAYER, "P7Metric", false, 32.0, 64.0, -16.0)
        .expect("bounded first-party workload player context")
}

fn p7_enqueue_money(boundary: &ScriptBoundary) {
    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(P7_SESSION),
            p7_player_context(),
            "money",
        ),
        Ok(PlayerCommandAdmission::Enqueued),
        "the actual component command is admitted to the live server boundary"
    );
}

fn p7_peak_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("Linux reports process RSS");
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("Linux process status includes VmHWM")
}

fn p7_process_cpu_ticks() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("Linux reports process CPU time");
    let (_, fields) = stat
        .rsplit_once(") ")
        .expect("Linux process stat includes process name terminator");
    let values = fields.split_whitespace().collect::<Vec<_>>();
    (
        values[11].parse().expect("Linux process user CPU ticks"),
        values[12].parse().expect("Linux process system CPU ticks"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local data/vanilla and the explicit first-party sibling integration gate"]
async fn p7_first_party_components_run_on_live_server_tick_workload() {
    const NORMAL_CALLS: usize = 100;
    const STORM_CALLS: usize = 64;
    const MEASURE_TICKS: u64 = 100;

    let staged = p7_staged_standard_pack();
    let limits = PluginLimits::default();
    let host = start_deployment(
        p7_standard_pack_packages(staged.path(), &limits),
        limits,
        HostQueues::default(),
        Arc::new(P7Sessions),
    )
    .expect("strict first-party component host starts");
    let boundary = host.boundary().clone();
    let server = start_plugin_workload_server(host).await;
    let mut ticks = server.runtime_telemetry.subscribe_simulation_ticks();
    let peak_before_kib = p7_peak_rss_kib();
    let (cpu_user_before, cpu_system_before) = p7_process_cpu_ticks();
    let normal_started = Instant::now();
    for _ in 0..NORMAL_CALLS {
        let submitted_at_tick = *ticks.borrow_and_update();
        p7_enqueue_money(&boundary);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "the tick-separated normal command did not observe the next live server tick"
            );
            tokio::time::timeout(remaining, ticks.changed())
                .await
                .expect("normal command tick wait timed out")
                .expect("normal command tick sender remains active");
            if *ticks.borrow_and_update() > submitted_at_tick {
                break;
            }
        }
    }
    let normal_elapsed = normal_started.elapsed();
    let start_tick = *ticks.borrow_and_update();
    let mut storm_peak_depth = boundary.event_queue_depth();
    for _ in 0..STORM_CALLS {
        p7_enqueue_money(&boundary);
        storm_peak_depth = storm_peak_depth.max(boundary.event_queue_depth());
    }

    let target_tick = start_tick.saturating_add(MEASURE_TICKS);
    let deadline = Instant::now() + Duration::from_secs(15);
    let tick_profile = loop {
        if let Some(profile) = server.runtime_telemetry.snapshot().tick_percentiles
            && profile.source_tick >= target_tick
            && *ticks.borrow_and_update() >= target_tick
        {
            break profile;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "live first-party workload did not publish tick telemetry by tick {target_tick}"
        );
        tokio::time::timeout(remaining, ticks.changed())
            .await
            .expect("live first-party workload tick wait timed out")
            .expect("live first-party workload tick sender remains active");
    };
    let peak_after_kib = p7_peak_rss_kib();
    let (cpu_user_after, cpu_system_after) = p7_process_cpu_ticks();
    let queue_depth_at_sample = boundary.event_queue_depth();
    let diagnostics = shutdown_plugin_workload_server(server).await;
    let economy = diagnostics
        .iter()
        .find(|(id, _)| id == "solaris-economy")
        .map(|(_, diagnostics)| diagnostics)
        .expect("the strict component workload hosts solaris-economy");

    assert!(
        tick_profile.tick.samples > 0,
        "the live server published runtime tick samples"
    );
    assert!(
        tick_profile.tick.p50_us <= tick_profile.tick.p95_us
            && tick_profile.tick.p95_us <= tick_profile.tick.p99_us
            && tick_profile.tick.p99_us <= tick_profile.tick.max_us,
        "runtime tick percentiles remain ordered: {:?}",
        tick_profile.tick
    );
    assert!(
        economy.events_delivered >= (NORMAL_CALLS + STORM_CALLS) as u64,
        "every normal and storm command reaches the live economy component: {economy:?}"
    );
    assert_eq!(
        economy.commands_refused, 0,
        "the comparable live normal/storm workload is not refused by the host"
    );
    assert!(
        economy.callback_latency.samples > 0
            && economy.callback_latency.p50_us <= economy.callback_latency.p95_us
            && economy.callback_latency.p95_us <= economy.callback_latency.p99_us
            && economy.callback_latency.p99_us <= economy.callback_latency.max_us,
        "live component callback percentiles remain ordered: {:?}",
        economy.callback_latency
    );

    eprintln!("--- P7 first-party live server workload ---");
    eprintln!(
        "environment: {} {} | logical CPUs {} | build: cargo test debug | seed: 0 | clients: 0 synthetic script command source | duration: {MEASURE_TICKS} simulation ticks",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(0),
    );
    eprintln!(
        "workload: strict five-package pack; {NORMAL_CALLS} tick-separated /money commands in {:.3} ms, then {STORM_CALLS} queued /money commands",
        normal_elapsed.as_secs_f64() * 1e3,
    );
    eprintln!(
        "runtime ticks: samples {} | p50 {} us | p95 {} us | p99 {} us | max {} us | source tick {}",
        tick_profile.tick.samples,
        tick_profile.tick.p50_us,
        tick_profile.tick.p95_us,
        tick_profile.tick.p99_us,
        tick_profile.tick.max_us,
        tick_profile.source_tick,
    );
    eprintln!(
        "economy callback latency: samples {} | p50 {} us | p95 {} us | p99 {} us | max {} us",
        economy.callback_latency.samples,
        economy.callback_latency.p50_us,
        economy.callback_latency.p95_us,
        economy.callback_latency.p99_us,
        economy.callback_latency.max_us,
    );
    eprintln!(
        "event queue depth: storm peak {storm_peak_depth} | sampled before graceful shutdown {queue_depth_at_sample} | CPU delta user {} ticks system {} ticks | peak RSS {} KiB -> {} KiB | host refusals {}",
        cpu_user_after.saturating_sub(cpu_user_before),
        cpu_system_after.saturating_sub(cpu_system_before),
        peak_before_kib,
        peak_after_kib,
        economy.commands_refused,
    );
}

struct PluginWorkloadServer {
    runtime_telemetry: mc_net::RuntimeTelemetryHandle,
    shutdown: mc_net::ShutdownHandle,
    serve_task: tokio::task::JoinHandle<std::io::Result<()>>,
    component_host: PluginHost,
}

async fn start_plugin_workload_server(component_host: PluginHost) -> PluginWorkloadServer {
    let vanilla_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../solaris/data/vanilla");
    let blocks_json = vanilla_dir.join("reports/blocks.json");
    let registries_json = vanilla_dir.join("reports/registries.json");
    assert!(
        blocks_json.exists() && registries_json.exists(),
        "plugin workload requires local vanilla sidecars"
    );
    let data = Arc::new(mc_data::load(&vanilla_dir).expect("vanilla data"));
    let blocks = Arc::new(
        mc_world::BlockRegistry::from_report(
            &mc_data::blocks::load_blocks_report(blocks_json).expect("block report"),
        )
        .expect("block registry"),
    );
    let items_report = mc_data::items::load_items_report(registries_json).expect("item report");
    let items = Arc::new(mc_data::items::ItemRegistry::from_report(&items_report));
    let tags = Arc::new(mc_data::tags::load(&vanilla_dir, &data).expect("vanilla tags"));
    let generator = Arc::new(mc_worldgen::TerrainGenerator::new(0, Arc::clone(&blocks)));
    let world = Arc::new(tokio::sync::Mutex::new(
        mc_world::WorldStorage::in_memory_with_capacity(Arc::clone(&blocks), 49)
            .with_generator(generator as Arc<dyn mc_world::ChunkGenerator>)
            .with_item_registry(Arc::clone(&items)),
    ));
    let entity_report = mc_data::entity_types::load_entity_types_report(
        &vanilla_dir.join("reports/registries.json"),
    )
    .expect("entity report");
    let entity_types = Arc::new(
        mc_data::entity_types::EntityTypeRegistry::try_from_report_26_1_2(&entity_report)
            .expect("entity registry"),
    );
    let mut chunk_pipeline = mc_net::ChunkPipelinePolicy::default();
    chunk_pipeline.chunk_prepare_batch_size = 2;
    chunk_pipeline.chunk_io_threads = 1;
    chunk_pipeline.chunk_worker_threads = 2;
    chunk_pipeline.chunk_result_queue_size = 8;
    let cfg = mc_net::ServerConfig {
        tab_list: mc_net::TabListConfig::default(),
        bind_address: "127.0.0.1:0".parse().unwrap(),
        motd: "first-party component workload".into(),
        max_players: 8,
        view_distance: 1,
        data,
        blocks: Arc::clone(&blocks),
        world: Some(world),
        tags,
        recipes: Arc::new(mc_data::recipes::solaris_required_recipes()),
        loot: Arc::new(mc_data::loot::LootTables::default()),
        block_light: mc_data::block_light::load(vanilla_dir.join("reports/block_light.json"))
            .ok()
            .map(Arc::new),
        items,
        item_facts: Arc::new(mc_data::item_components::ItemFactsTable::default()),
        block_facts: Arc::new(mc_data::block_facts::BlockFactsTable::default()),
        entity_types,
        biome_spawns: Arc::new(mc_data::biomes::BiomeSpawnRules::default()),
        chunk_pipeline,
        random_tick: mc_net::RandomTickPolicy {
            simulation_distance: 1,
            random_tick_speed: 3,
            chunk_budget: 8,
            fluid_tick_budget: 64,
            save_interval_ticks: 20,
            friendly_spawn_interval_ticks: 400,
            hostile_spawn_interval_ticks: 20,
            seed: 0,
            ..mc_net::RandomTickPolicy::default()
        },
        command_permissions: mc_net::CommandPermissionConfig::new(Vec::<String>::new(), true),
        loader_manifest: None,
        shutdown: mc_net::ShutdownHandle::default(),
    };
    let shutdown = cfg.shutdown.clone();
    let bound = mc_net::bind_with_scripts(cfg, component_host.boundary().clone())
        .await
        .expect("bind scripted workload server");
    let runtime_telemetry = bound.runtime_telemetry_handle();
    let serve_task = tokio::spawn(async move { bound.serve().await });
    PluginWorkloadServer {
        runtime_telemetry,
        shutdown,
        serve_task,
        component_host,
    }
}

async fn shutdown_plugin_workload_server(
    server: PluginWorkloadServer,
) -> Vec<(String, mc_plugin_host::InstanceDiagnostics)> {
    server.shutdown.request();
    tokio::time::timeout(Duration::from_secs(5), server.serve_task)
        .await
        .expect("plugin workload shutdown timed out")
        .expect("server task failed")
        .expect("server failed");
    server.component_host.stop()
}
