"""Authoring-only staging and bridges to the native server and Loader validators."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import tomllib

import zipfile
TOOLS = Path(__file__).resolve().parent


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def read_json(path: Path):
    def invalid_constant(value):
        raise ValueError(f"non-finite JSON value: {value}")
    return json.loads(path.read_text(), object_pairs_hook=unique_object,
                      parse_constant=invalid_constant)


def positive(value: str) -> int:
    result = int(value)
    if result < 1:
        raise argparse.ArgumentTypeError("jobs must be positive")
    return result


def build_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--profile", choices=("debug", "release"),
                        default=os.environ.get("SOLARIS_BUILD_PROFILE", "release"))
    parser.add_argument("--jobs", type=positive,
                        default=positive(os.environ.get("SOLARIS_BUILD_JOBS", "4")))
    parser.add_argument("--loader-root", type=Path, default=TOOLS.parents[1] / "solaris-loader")


def profile_flags(profile: str) -> list[str]:
    return ["--release"] if profile == "release" else []


def run(argv: list[str], cwd: Path, env: dict[str, str] | None = None) -> None:
    subprocess.run(argv, cwd=cwd, env=env, check=True)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_hashes(root: Path) -> dict[str, str]:
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"authoring root must be a real directory: {root}")
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"authoring symlink is not supported: {path}")
        if path.is_file() and "target" not in path.relative_to(root).parts:
            result[path.relative_to(root).as_posix()] = digest(path.read_bytes())
    return result


def source_revision(files: dict[str, str]) -> str:
    return digest(json.dumps(files, sort_keys=True, separators=(",", ":")).encode())


def publish(candidate: Path, destination: Path) -> None:
    # Candidate and destination are siblings: the only visible publication is one rename.
    # Immutable destinations also prevent an authoring command overwriting an active deployment.
    if destination.exists():
        raise ValueError(f"immutable output already exists: {destination}")
    candidate.rename(destination)


def loader_manifest(package: Path) -> dict:
    manifest = tomllib.loads((package / "plugin.toml").read_text())
    owner = manifest["id"]
    bundles = []
    for bundle in manifest.get("client", {}).get("bundles", []):
        bundles.append({"owner": owner, **bundle,
                        "cache_key": f'{owner}:{bundle["id"]}/{bundle["version"]}/{bundle["sha256"]}'})
    return {"protocol": 3, "bundles": bundles}


def validate_loader(loader_root: Path, deployment: Path, manifest: dict, jobs: int) -> None:
    """Compile only an authoring bridge; native activation remains the sole schema authority."""
    if not manifest["bundles"]:
        return  # No client content: the Loader receives nothing to validate.
    loader_root = loader_root.resolve()
    if not (loader_root / "loader-core" / "build.gradle.kts").is_file():
        raise ValueError(f"missing Loader validator source: {loader_root}")
    with tempfile.TemporaryDirectory(prefix="solaris-content-loader-") as temporary:
        stage = Path(temporary)
        request = stage / "manifest.json"
        request.write_text(json.dumps(manifest))
        init = stage / "validator.gradle"
        # An init script adds throwaway tasks without modifying the Loader checkout.
        def quoted(path: Path) -> str:
            return json.dumps(str(path))
        init.write_text(f'''gradle.projectsEvaluated {{
    def p = gradle.rootProject.project(':loader-core')
    def bridge = p.tasks.register('compileSolarisContentValidator', JavaCompile) {{
        dependsOn p.tasks.named('classes')
        source = p.files({quoted(TOOLS / 'content-validator' / 'SolarisContentValidator.java')})
        classpath = p.sourceSets.main.runtimeClasspath
        destinationDirectory.set(p.layout.dir(p.provider {{ new File({quoted(stage / 'classes')}) }}))
        javaCompiler.set(p.javaToolchains.compilerFor(p.java.toolchain))
    }}
    p.tasks.register('validateSolarisContent', JavaExec) {{
        dependsOn bridge
        classpath = p.files({quoted(stage / 'classes')}) + p.sourceSets.main.runtimeClasspath
        mainClass.set('dev.solaris.loader.SolarisContentValidator')
        javaLauncher.set(p.javaToolchains.launcherFor(p.java.toolchain))
        args {quoted(request)}, {quoted(deployment.resolve())}, {quoted(stage / 'cache')}
    }}
}}
''')
        run(["nice", "-n", "19", str(loader_root / "gradlew"), "--no-daemon",
             f"--max-workers={jobs}", "--init-script", str(init),
             ":loader-core:validateSolarisContent"], loader_root)


def validate_native(core_root: Path, deployment: Path, profile: str, jobs: int,
                    target_dir: Path, validator: Path | None = None) -> dict:
    grants = deployment_grants(deployment)
    with tempfile.TemporaryDirectory(prefix="solaris-content-native-") as temporary:
        stage = Path(temporary)
        report = stage / "report.json"
        request = stage / "request.json"
        request.write_text(json.dumps({"root": str(deployment.resolve()), "grants": grants,
                                       "report": str(report)}))
        run_native_bridge(core_root, profile, jobs, target_dir, [str(request)], validator)
        return read_json(report)



def deployment_grants(deployment: Path) -> dict[str, list[str]]:
    """Preflight identities without replacing the server's strict discovery."""
    grants = {}
    for package in sorted(deployment.iterdir()):
        if package.is_symlink() or not package.is_dir():
            raise ValueError(f"deployment accepts only package directories: {package}")
        manifest = tomllib.loads((package / "plugin.toml").read_text())
        identity = manifest["id"]
        if identity in grants:
            raise ValueError(f"duplicate package identity: {identity}")
        if identity != package.name:
            raise ValueError(f"package directory differs from identity: {package.name}: {identity}")
        if manifest["api"] != "0.7.0" or manifest["entry"] != "plugin.wasm":
            raise ValueError(f"package must preserve API 0.7.0 and plugin.wasm entry: {identity}")
        grants[identity] = manifest.get("capabilities", [])
    if not grants:
        raise ValueError("empty deployment")
    return grants


def client_costs(deployment: Path, manifest: dict) -> dict:
    """Measure the whole validated activation set, not independent per-ZIP budgets."""
    bundles = []
    aggregate = {"archive_bytes": 0, "decoded_bytes": 0, "asset_bytes": 0,
                 "index_bytes": 0, "entries": 0, "items": 0, "blocks": 0,
                 "assets": 0, "sounds": 0, "screens": 0, "world_previews": 0}
    for bundle in manifest["bundles"]:
        data = (deployment / bundle["owner"] / bundle["artifact"]).read_bytes()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            index_data = archive.read("solaris-client.json")
            index = json.loads(index_data, object_pairs_hook=unique_object)
            costs = {"archive_bytes": len(data),
                     "decoded_bytes": sum(entry.file_size for entry in archive.infolist()),
                     "asset_bytes": sum(asset["size_bytes"] for asset in index.get("assets", [])),
                     "index_bytes": len(index_data), "entries": len(archive.infolist())}
            costs.update({kind: len(index.get(kind, [])) for kind in
                          ("items", "blocks", "assets", "sounds", "screens", "world_previews")})
        bundles.append({"owner": bundle["owner"], "id": bundle["id"],
                        "revision": bundle["version"], "sha256": digest(data), **costs})
        for field, value in costs.items():
            aggregate[field] += value
    return {"aggregate": aggregate, "bundles": bundles,
            "budget_authority": "native LoaderContentArchive.activate over the complete manifest"}

def run_native_bridge(core_root: Path, profile: str, jobs: int, target_dir: Path,
                      arguments: list[str], validator: Path | None = None) -> None:
    with tempfile.TemporaryDirectory(prefix="solaris-content-validator-source-") as temporary:
        stage = Path(temporary)
        if validator is None:
            dependencies = "\n".join(
                f'{name} = {{ path = {json.dumps(str(core_root / "crates" / name))} }}'
                for name in ("mc-data", "mc-world", "mc-worldgen", "mc-loader", "mc-plugin-host"))
            manifest = stage / "Cargo.toml"
            manifest.write_text(f'''[package]
name = "solaris-content-validator"
version = "0.1.0"
edition = "2024"
[workspace]
[[bin]]
name = "solaris-content-validator"
path = {json.dumps(str(TOOLS / 'content-validator' / 'native.rs'))}
[dependencies]
{dependencies}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
''')
            lock = core_root / "Cargo.lock"
            if lock.is_file():
                shutil.copyfile(lock, stage / "Cargo.lock")
            run(["nice", "-n", "19", "cargo", "build", "--manifest-path", str(manifest),
                 "-j", str(jobs), *profile_flags(profile)], core_root,
                os.environ | {"CARGO_TARGET_DIR": str(target_dir.resolve())})
            validator = target_dir / profile / "solaris-content-validator"
        run(["nice", "-n", "19", str(validator.resolve()), *arguments], core_root)


def validate_blueprint(core_root: Path, blueprint: Path, profile: str, jobs: int,
                       validator: Path | None = None) -> dict:
    with tempfile.TemporaryDirectory(prefix="solaris-blueprint-report-") as temporary:
        report = Path(temporary) / "blueprint.json"
        run_native_bridge(core_root, profile, jobs, core_root / "target",
                          ["blueprint", str(blueprint.resolve()), str(report), "solaris"], validator)
        return read_json(report)


def validate_practices(definitions: list[dict], recipes: list[dict]) -> None:
    require_unique_ids(recipes, "recipe")
    recipe_ids = {recipe["id"] for recipe in recipes}
    tech_ids = [definition["id"] for definition in definitions]
    if len(set(tech_ids)) != len(tech_ids):
        raise ValueError("duplicate technology identity")
    for definition in definitions:
        if any(parent not in tech_ids for parent in definition["parents"]):
            raise ValueError(f'unknown technology prerequisite: {definition["id"]}')
        for field in ("practice_recipes", "unlock_recipes"):
            missing = set(definition[field]) - recipe_ids
            if missing:
                raise ValueError(f'unknown {field} references: {sorted(missing)}')
    remaining = {definition["id"]: set(definition["parents"]) for definition in definitions}
    while remaining:
        ready = {identity for identity, parents in remaining.items() if not parents}
        if not ready:
            raise ValueError("cyclic executable technology prerequisites")
        remaining = {identity: parents - ready for identity, parents in remaining.items() if identity not in ready}


def require_unique_ids(records: list[dict], kind: str) -> set[str]:
    identities = [record["id"] for record in records]
    if any(not isinstance(identity, str) or not identity for identity in identities):
        raise ValueError(f"missing {kind} identity")
    if len(set(identities)) != len(identities):
        raise ValueError(f"duplicate {kind} identity")
    return set(identities)


def validate_reachability(content: dict, recipes: list[dict]) -> dict:
    """Return deterministic dependency evidence, never physical acquisition/use proof."""
    ids = require_unique_ids(recipes, "recipe")
    sources = content["ordinary_play_sources"]
    available = {source["item"] for source in sources}
    if len(available) != len(sources):
        raise ValueError("duplicate ordinary-play source identity")
    if any(not source["item"].startswith("minecraft:") or not source["origin"].strip() for source in sources):
        raise ValueError("ordinary-play source needs an explicit vanilla acquisition origin")
    for recipe in recipes:
        for field in ("inputs", "tools", "outputs", "remainders"):
            if any(not stack.get("item") or stack.get("tag") or stack.get("alternatives")
                   for stack in recipe[field]):
                raise ValueError(f'tagged/alternative reachability needs authored source expansion: {recipe["id"]}')
    pending = sorted(recipes, key=lambda recipe: recipe["id"])
    waves = []
    while pending:
        ready = [recipe for recipe in pending if {
            stack["item"] for stack in recipe["inputs"] + recipe["tools"]} <= available]
        if not ready:
            raise ValueError(f'unreachable authored recipes: {[recipe["id"] for recipe in pending]}')
        waves.append([recipe["id"] for recipe in ready])
        for recipe in ready:
            available.update(stack["item"] for stack in recipe["outputs"] + recipe["remainders"])
        ready_ids = {recipe["id"] for recipe in ready}
        pending = [recipe for recipe in pending if recipe["id"] not in ready_ids]
    representatives = content["representative_recipes"]
    representative_ids = require_unique_ids(representatives, "representative recipe")
    if not representatives or representative_ids - ids:
        raise ValueError("empty or unknown representative recipe reference")
    if any(not entry.get("use", "").strip() for entry in representatives):
        raise ValueError("representative recipe needs an explicit physical use scenario")
    by_id = {recipe["id"]: recipe for recipe in recipes}
    return {"revision": content["revision"], "recipe_count": len(recipes),
            "dependency_waves": waves, "reachable_items": sorted(available),
            "ordinary_play_sources": sorted(sources, key=lambda source: source["item"]),
            "representatives": [
                {"id": entry["id"], "revision": by_id[entry["id"]]["revision"],
                 "outputs": by_id[entry["id"]]["outputs"], "use": entry["use"],
                 "runtime_smoke": "not exercised"}
                for entry in sorted(representatives, key=lambda entry: entry["id"])]}
