//! Source-built first-party payment through a real component, TCP client and
//! world-backed storage owner. Explicit sibling gate; no graphical client.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mc_nbt::Tag;
use mc_plugin_host::{
    DeploymentConfig, DiscoveryMode, HostQueues, NoSessions, PluginHost, PluginLimits, discover,
    start_deployment,
};
use mc_protocol::packets::Packet;
use mc_protocol::packets::play::{
    ClientboundCommands, ClientboundSystemChat, ConfirmTeleportation, ServerboundChatCommand,
    SynchronizePlayerPosition,
};
use mc_test_harness::client::Client;

const ECONOMY: &str = "solaris-economy";
const TIMEOUT: Duration = Duration::from_secs(30);
const CONFIG: &str = "currency_name = \"coins\"\nstarting_balance = 100\nmaximum_balance = 1000000000\nmaximum_accounts = 48\nmaximum_transfer_tokens = 1\n";

fn source_package() -> PathBuf {
    let root = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("integration test crate belongs to the plugin repository")
                .to_path_buf()
        });
    root.join(ECONOMY)
}

struct Registries {
    data: Arc<mc_data::VanillaData>,
    blocks: Arc<mc_world::BlockRegistry>,
    items: Arc<mc_data::items::ItemRegistry>,
    tags: Arc<mc_data::tags::TagsData>,
    recipes: Arc<Vec<mc_data::recipes::Recipe>>,
    loot: Arc<mc_data::loot::LootTables>,
    item_facts: Arc<mc_data::item_components::ItemFactsTable>,
    block_facts: Arc<mc_data::block_facts::BlockFactsTable>,
    entity_types: Arc<mc_data::entity_types::EntityTypeRegistry>,
}

impl Registries {
    fn new() -> Self {
        let report = mc_data::blocks::solaris_required_blocks_report();
        let items = Arc::new(mc_data::items::solaris_required_items());
        Self {
            data: Arc::new(mc_data::solaris_required_data()),
            blocks: Arc::new(mc_world::BlockRegistry::from_report(&report).unwrap()),
            tags: Arc::new(mc_data::tags::solaris_required_item_tags(&items)),
            items,
            recipes: Arc::new(mc_data::recipes::solaris_required_recipes()),
            loot: Arc::new(mc_data::loot::builtin().clone()),
            item_facts: Arc::new(mc_data::item_components::solaris_required_item_facts()),
            block_facts: Arc::new(mc_data::block_facts::BlockFactsTable::from_blocks_report(
                &report,
            )),
            entity_types: Arc::new(mc_data::entity_types::solaris_required_entity_types()),
        }
    }
}

struct Running {
    _package: tempfile::TempDir,
    address: SocketAddr,
    shutdown: mc_net::ShutdownHandle,
    host: PluginHost,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

fn economy_packages(root: &Path, limits: &PluginLimits) -> Vec<mc_plugin_host::LoadedPackage> {
    discover(
        &DeploymentConfig {
            root: root.to_path_buf(),
            mode: DiscoveryMode::Strict,
            expected: vec![ECONOMY.to_owned()],
            grants: BTreeMap::from([(
                ECONOMY.to_owned(),
                vec!["storage".to_owned(), "storage_batches".to_owned()],
            )]),
            require_grants: true,
            precommit_hooks: Vec::new(),
        },
        limits,
    )
    .unwrap()
    .into_packages()
}

impl Running {
    async fn start(world_dir: &Path, registries: &Registries) -> Self {
        let package = tempfile::tempdir().unwrap();
        let package_path = package.path().join(ECONOMY);
        std::fs::create_dir(&package_path).unwrap();
        for file in ["plugin.toml", "plugin.wasm"] {
            std::fs::copy(source_package().join(file), package_path.join(file)).unwrap();
        }
        std::fs::write(package_path.join("config.toml"), CONFIG).unwrap();
        let limits = PluginLimits::default();
        let packages = economy_packages(package.path(), &limits);
        let host = start_deployment(
            packages,
            limits,
            HostQueues::default(),
            Arc::new(NoSessions),
        )
        .unwrap();
        let world = mc_world::WorldStorage::open_with_capacity(
            world_dir,
            Arc::clone(&registries.blocks),
            49,
        )
        .unwrap()
        .with_item_registry(Arc::clone(&registries.items))
        .with_generator(Arc::new(mc_worldgen::TerrainGenerator::new(
            0,
            Arc::clone(&registries.blocks),
        )));
        let shutdown = mc_net::ShutdownHandle::default();
        let bound = mc_net::bind_with_scripts(
            mc_net::ServerConfig {
                tab_list: mc_net::TabListConfig::default(),
                bind_address: "127.0.0.1:0".parse().unwrap(),
                motd: "durable component payment".to_owned(),
                max_players: 2,
                view_distance: 2,
                data: Arc::clone(&registries.data),
                blocks: Arc::clone(&registries.blocks),
                world: Some(Arc::new(tokio::sync::Mutex::new(world))),
                tags: Arc::clone(&registries.tags),
                recipes: Arc::clone(&registries.recipes),
                loot: Arc::clone(&registries.loot),
                block_light: None,
                items: Arc::clone(&registries.items),
                item_facts: Arc::clone(&registries.item_facts),
                block_facts: Arc::clone(&registries.block_facts),
                entity_types: Arc::clone(&registries.entity_types),
                biome_spawns: Arc::new(mc_data::biomes::BiomeSpawnRules::default()),
                chunk_pipeline: mc_net::ChunkPipelinePolicy::default(),
                random_tick: mc_net::RandomTickPolicy::default(),
                command_permissions: mc_net::CommandPermissionConfig::new(
                    Vec::<String>::new(),
                    true,
                ),
                loader_manifest: None,
                shutdown: shutdown.clone(),
            },
            host.boundary().clone(),
        )
        .await
        .unwrap();
        let address = bound.local_addr().unwrap();
        let server = tokio::spawn(async move { bound.serve().await });
        Self {
            _package: package,
            address,
            shutdown,
            host,
            server,
        }
    }

    async fn reload(&self) {
        self.host
            .reload(economy_packages(
                self._package.path(),
                &PluginLimits::default(),
            ))
            .await
            .expect("same-contract payment component reload");
    }

    async fn stop(self) {
        self.shutdown.request();
        tokio::time::timeout(TIMEOUT, self.server)
            .await
            .expect("server stops on its shutdown event")
            .unwrap()
            .unwrap();
        tokio::task::spawn_blocking(move || self.host.stop())
            .await
            .unwrap();
    }
}

async fn login(address: SocketAddr, name: &str) -> (Client, String) {
    let mut client = Client::connect(address).await.unwrap();
    let profile = client.drive_login(address, name).await.unwrap();
    client.drive_configuration().await.unwrap();
    client.read_play_login().await.unwrap();
    let _: ClientboundCommands = client.read_typed().await.unwrap();
    let sync: SynchronizePlayerPosition = client.read_typed().await.unwrap();
    client
        .write_packet(&ConfirmTeleportation {
            teleport_id: sync.teleport_id,
        })
        .await
        .unwrap();
    (client, profile.uuid.simple().to_string())
}

fn literal_chat(content: &[u8]) -> String {
    let mut bytes = Bytes::copy_from_slice(content);
    let Tag::Compound(fields) = mc_nbt::read_network(&mut bytes).unwrap() else {
        panic!("chat must have a compound root");
    };
    fields
        .into_iter()
        .find_map(|(key, value)| match (key.as_str(), value) {
            ("text", Tag::String(text)) => Some(text),
            _ => None,
        })
        .unwrap()
}

async fn command(client: &mut Client, raw: &str) -> String {
    client
        .write_packet(&ServerboundChatCommand {
            command: raw.to_owned(),
        })
        .await
        .unwrap();
    loop {
        let mut frame = client.read_frame_with_timeout(TIMEOUT).await.unwrap();
        if frame.id == ClientboundSystemChat::ID {
            let chat = ClientboundSystemChat::decode(&mut frame.body).unwrap();
            let text = literal_chat(&chat.content_nbt);
            if text.starts_with("Balance:")
                || text.starts_with("Paid ")
                || text.starts_with("That transfer token")
                || text.starts_with("Transfer ")
                || text.starts_with("Payment ")
                || text == "Economy is still loading."
            {
                return text;
            }
        }
    }
}

async fn balance(client: &mut Client) -> String {
    for _ in 0..8 {
        let answer = command(client, "money").await;
        if answer != "Economy is still loading." {
            return answer;
        }
    }
    panic!("economy never answered its world-backed startup read")
}

#[tokio::test]
#[ignore = "requires a source-built first-party sibling component"]
async fn payment_receipt_prevents_second_debit_after_token_eviction_reload_and_restart() {
    let world = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(world.path().join("region")).unwrap();
    let registries = Registries::new();
    let first = Running::start(world.path(), &registries).await;
    let (mut sender, _) = login(first.address, "Steward").await;
    let (mut recipient, recipient_uuid) = login(first.address, "Receiver").await;
    assert_eq!(balance(&mut sender).await, "Balance: 100 coins.");
    assert_eq!(balance(&mut recipient).await, "Balance: 100 coins.");
    assert_eq!(
        command(&mut sender, &format!("pay {recipient_uuid} 7 first")).await,
        "Paid 7 coins."
    );
    assert_eq!(
        command(&mut sender, &format!("pay {recipient_uuid} 1 second")).await,
        "Paid 1 coins."
    );
    first.reload().await;
    assert_eq!(balance(&mut sender).await, "Balance: 92 coins.");
    assert_eq!(
        command(&mut sender, &format!("pay {recipient_uuid} 7 first")).await,
        "That transfer token was already committed."
    );
    assert_eq!(balance(&mut sender).await, "Balance: 92 coins.");
    assert_eq!(balance(&mut recipient).await, "Balance: 108 coins.");
    drop(sender);
    drop(recipient);
    first.stop().await;

    let second = Running::start(world.path(), &registries).await;
    let (mut sender, _) = login(second.address, "Steward").await;
    let (mut recipient, recipient_uuid) = login(second.address, "Receiver").await;
    assert_eq!(balance(&mut sender).await, "Balance: 92 coins.");
    assert_eq!(balance(&mut recipient).await, "Balance: 108 coins.");
    assert_eq!(
        command(&mut sender, &format!("pay {recipient_uuid} 6 first")).await,
        "That transfer token was already committed."
    );
    assert_eq!(balance(&mut sender).await, "Balance: 92 coins.");
    assert_eq!(balance(&mut recipient).await, "Balance: 108 coins.");
    drop(sender);
    drop(recipient);
    second.stop().await;
}
