//! Authoring only: reuse the server's strict discovery, startup and item/recipe validators.
use mc_plugin_host::{DeploymentConfig, DiscoveryMode, HostServices, PluginLimits};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: PathBuf,
    grants: BTreeMap<String, Vec<String>>,
    report: PathBuf,
}
struct Services(String);
impl HostServices for Services {
    fn log(
        &mut self,
        _level: mc_plugin_host::bindings::solaris::plugin::types::LogLevel,
        message: &str,
    ) {
        eprintln!("{}: {message}", self.0);
    }
    fn plugin_id(&self) -> &str {
        &self.0
    }
}

fn blueprint_catalog(
    path: &Path,
    owner: &str,
) -> Result<mc_worldgen::BlueprintCatalog, Box<dyn Error>> {
    let mut paths = Vec::new();
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            if paths.len() >= mc_worldgen::MAX_BLUEPRINTS {
                return Err("blueprint file count exceeds native catalog bound".into());
            }
            paths.push(entry?.path());
        }
    } else {
        paths.push(path.to_owned());
    }
    paths.sort();
    let mut decoded = 0usize;
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("blueprint catalog accepts only regular authored files".into());
        }
        decoded = decoded
            .checked_add(usize::try_from(metadata.len())?)
            .ok_or("blueprint byte overflow")?;
        if decoded > mc_worldgen::MAX_CATALOG_DECODED_BYTES {
            return Err("blueprint bytes exceed native catalog bound".into());
        }
        files.push((
            path.file_name()
                .ok_or("blueprint filename required")?
                .to_str()
                .ok_or("blueprint filename must be UTF-8")?
                .to_owned(),
            fs::read_to_string(path)?,
        ));
    }
    let registry =
        mc_world::BlockRegistry::from_report(&mc_data::blocks::solaris_required_blocks_report())?;
    Ok(mc_worldgen::BlueprintCatalog::from_files(
        &registry, owner, &files,
    )?)
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().nth(1).as_deref() == Some("blueprint") {
        let path = PathBuf::from(
            std::env::args_os()
                .nth(2)
                .ok_or("blueprint path required")?,
        );
        let report = PathBuf::from(
            std::env::args_os()
                .nth(3)
                .ok_or("blueprint report path required")?,
        );
        let owner = std::env::args().nth(4).ok_or("blueprint owner required")?;
        let catalog = blueprint_catalog(&path, &owner)?;
        let blueprint = catalog
            .blueprints()
            .next()
            .ok_or("empty blueprint catalog")?;
        let palette = blueprint
            .palette()
            .iter()
            .map(|entry| (entry.index, entry.block.as_str()))
            .collect::<BTreeMap<_, _>>();
        let blocks = blueprint.blocks().iter().map(|block| json!({
            "x": block.x, "y": block.y, "z": block.z,
            "block_id": palette.get(&block.palette).expect("native validated every block palette"),
        })).collect::<Vec<_>>();
        fs::write(
            report,
            serde_json::to_vec(&json!({
                "id": blueprint.id(), "revision": blueprint.revision(),
                "content_hash": blueprint.content_hash(), "size": blueprint.size(),
                "blocks": blocks,
            }))?,
        )?;
        return Ok(());
    }
    let request: Request = serde_json::from_slice(&fs::read(
        std::env::args_os().nth(1).ok_or("request path required")?,
    )?)?;
    let config = DeploymentConfig {
        root: request.root,
        mode: DiscoveryMode::Strict,
        expected: request.grants.keys().cloned().collect(),
        grants: request.grants,
        require_grants: true,
        precommit_hooks: Vec::new(),
    };
    let limits = PluginLimits::default();
    let checked = mc_plugin_host::check_deployment(&config, &limits)?;
    if !checked.skipped().is_empty() {
        return Err("strict discovery skipped a package".into());
    }
    let loader = mc_loader::LoaderManifest::from_script_bundles(checked.client_bundles())?;
    let items = checked
        .checked()
        .iter()
        .flat_map(|package| package.items.iter().cloned())
        .collect::<Vec<_>>();
    // These definitions come only from the candidate plugins, not the core catalog.
    loader.validate_custom_items(&items, |_| false)?;
    loader.encode()?;

    // Read the exact guest startup strings, not a Python reimplementation of production's expansion.
    // Native conversion above already refuses bad declarations; this second bounded startup call
    // supplies measured catalog bytes and immutable recipe revisions for the authoring receipt.
    let engine = mc_plugin_host::engine(&limits)?;
    let _ticker = mc_plugin_host::EpochTicker::start(engine.clone(), Duration::from_millis(50))?;
    let linker = mc_plugin_host::linker::<Services>(&engine)?;
    let mut catalogs = BTreeMap::new();
    let mut blueprints = BTreeMap::new();
    for package in mc_plugin_host::discover(&config, &limits)?.packages() {
        let id = package.manifest().plugin_id();
        let authored = package.root().join("structures");
        if authored.exists() {
            let catalog = blueprint_catalog(&authored, "solaris")?;
            blueprints.insert(
                id.to_owned(),
                catalog
                    .blueprints()
                    .map(|blueprint| {
                        json!({
                            "id": blueprint.id(), "revision": blueprint.revision(),
                            "content_hash": blueprint.content_hash(),
                        })
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let compiled = mc_plugin_host::package::compile_package(&engine, package, &limits)?;
        let mut startup = mc_plugin_host::PluginStartup::instantiate(
            &linker,
            compiled.component(),
            Services(id.to_owned()),
            limits,
        )?;
        let path = package.root().join("config.toml");
        let text = if path.is_file() {
            fs::read_to_string(path)?
        } else {
            String::new()
        };
        if text.len() > limits.text_bytes {
            return Err("config exceeds native limit".into());
        }
        if let Some(contribution) = startup.configure(&text)? {
            let native = mc_plugin_host::convert_startup_contribution(id, &contribution)?;
            let definitions = contribution.work_recipes.as_deref().unwrap_or_default();
            let bytes = definitions
                .iter()
                .try_fold(0usize, |total, value| total.checked_add(value.len()))
                .ok_or("catalog byte overflow")?;
            catalogs.insert(
                id.to_owned(),
                json!({
                    "count": native.work_recipes().len(),
                    "serialized_bytes": bytes,
                    "recipes": native.work_recipes(),
                }),
            );
        }
    }
    fs::write(
        request.report,
        serde_json::to_vec_pretty(&json!({
            "loader_manifest": loader,
            "catalogs": catalogs,
            "blueprints": blueprints,
        }))?,
    )?;
    Ok(())
}
