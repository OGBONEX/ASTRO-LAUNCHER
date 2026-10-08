#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tokio::{fs as afs, io::AsyncWriteExt};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Instance {
    name: String,
    version: String,
    loader: String,
    dir: String,
    java: String,
    ram: u32,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    versions: Vec<ManifestVersion>,
}

#[derive(Debug, Deserialize)]
struct ManifestVersion {
    id: String,
    url: String,

    #[serde(rename = "type")]
    _kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VersionJson {
    id: String,
    downloads: Downloads,
    libraries: Vec<Library>,

    #[serde(rename = "assetIndex")]
    asset_index: Option<AssetIndex>,

    assets: Option<String>,

    #[serde(rename = "mainClass")]
    main_class: String,

    #[serde(default)]
    arguments: Option<Arguments>,

    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Downloads {
    client: Download,
}

#[derive(Debug, Serialize, Deserialize)]
struct Download {
    url: String,
    sha1: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Library {
    name: String,
    downloads: LibraryDownloads,

    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LibraryDownloads {
    artifact: Option<DownloadArtifact>,
    classifiers: Option<HashMap<String, DownloadArtifact>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DownloadArtifact {
    path: String,
    url: String,
    sha1: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Rule {
    action: String,

    #[serde(default)]
    os: Option<RuleOs>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct RuleOs {
    name: Option<String>,
    arch: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssetIndex {
    url: String,
    sha1: String,
    id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssetIndexJson {
    objects: HashMap<String, AssetObject>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Arguments {
    #[serde(default)]
    game: Vec<ArgValue>,

    #[serde(default)]
    jvm: Vec<ArgValue>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum ArgValue {
    String(String),
    Rule {
        rules: Vec<Rule>,
        value: ArgValueInner,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum ArgValueInner {
    String(String),
    Array(Vec<String>),
}

fn root_default() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AstroMinecraft")
}

fn sha1_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;

    let mut h = Sha1::new();
    let mut b = [0u8; 1024 * 128];

    loop {
        let n = f.read(&mut b).map_err(|e| e.to_string())?;

        if n == 0 {
            break;
        }

        h.update(&b[..n]);
    }

    Ok(format!("{:x}", h.finalize()))
}

async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    expected: Option<&str>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        afs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }

    if path.exists() {
        if let Some(expected_hash) = expected {
            if sha1_file(path) == Ok(expected_hash.to_string()) {
                return Ok(());
            }
        }
    }

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?;

    let bytes = response.bytes().await.map_err(|e| e.to_string())?;

    let tmp = path.with_extension("astro.part");

    let mut f = afs::File::create(&tmp).await.map_err(|e| e.to_string())?;

    f.write_all(&bytes).await.map_err(|e| e.to_string())?;

    f.flush().await.map_err(|e| e.to_string())?;

    drop(f);

    if let Some(hash) = expected {
        if sha1_file(&tmp)? != hash {
            let _ = afs::remove_file(&tmp).await;

            return Err(format!("Checksum mismatch: {url}"));
        }
    }

    afs::rename(&tmp, path).await.map_err(|e| e.to_string())?;

    Ok(())
}

fn windows_library_allowed(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }

    let current_os = if cfg!(target_os = "windows") {
        Some("windows")
    } else if cfg!(target_os = "linux") {
        Some("linux")
    } else if cfg!(target_os = "macos") {
        Some("osx")
    } else {
        None
    };

    let current_arch = if cfg!(target_arch = "x86_64") {
        Some("x64")
    } else if cfg!(target_arch = "aarch64") {
        Some("arm64")
    } else if cfg!(target_arch = "arm") {
        Some("arm")
    } else if cfg!(target_arch = "x86") {
        Some("x86")
    } else {
        None
    };

    let mut allowed = false;

    for rule in rules {
        let matches_os = rule.os.as_ref().map_or(true, |os| {
            let name_ok = os
                .name
                .as_deref()
                .map(|name| current_os == Some(name))
                .unwrap_or(true);

            let arch_ok = os.arch.as_deref().map_or(true, |arch| {
                current_arch == Some(arch)
                    || (arch == "x64" && current_arch == Some("amd64"))
                    || (arch == "amd64" && current_arch == Some("x64"))
            });

            name_ok && arch_ok
        });

        if matches_os {
            allowed = rule.action == "allow";
        }
    }

    allowed
}

fn offline_uuid(name: &str) -> String {
    let digest = md5::compute(format!("OfflinePlayer:{name}").as_bytes());

    let mut b = digest.0;

    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

fn required_java_major(version: &str) -> u32 {
    if version.starts_with("1.17") {
        return 16;
    }

    if version.starts_with("1.18") || version.starts_with("1.19") {
        return 17;
    }

    if version.starts_with("1.20.") {
        let patch = version
            .split('.')
            .nth(2)
            .and_then(|x| x.parse::<u32>().ok())
            .unwrap_or(0);

        return if patch >= 5 { 21 } else { 17 };
    }

    if version.starts_with("1.21") {
        return 21;
    }

    8
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    link: String,
    checksum: Option<String>,
    name: String,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
}

async fn install_java_runtime(root: &Path, major: u32) -> Result<PathBuf, String> {
    let runtime = root.join("runtime").join(format!("temurin-{major}"));

    let exe = runtime.join("bin").join("java.exe");

    if exe.exists() {
        return Ok(exe);
    }

    let client = reqwest::Client::builder()
        .user_agent("AstroLauncher/0.3")
        .build()
        .map_err(|e| e.to_string())?;

    let api = format!(
        "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse"
    );

    let assets: Vec<AdoptiumAsset> = client
        .get(api)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let asset = assets
        .into_iter()
        .next()
        .ok_or_else(|| format!("No Windows x64 Java {major} JRE is available."))?;

    let archive = root.join("runtime").join(&asset.binary.package.name);

    download_verified(&client, &asset.binary.package.link, &archive, None).await?;

    if let Some(expected) = asset.binary.package.checksum.as_deref() {
        let bytes = afs::read(&archive).await.map_err(|e| e.to_string())?;

        let mut h = Sha256::new();
        h.update(&bytes);

        if format!("{:x}", h.finalize()).to_lowercase() != expected.to_lowercase() {
            let _ = afs::remove_file(&archive).await;

            return Err("Java runtime checksum verification failed.".into());
        }
    }

    let archive2 = archive.clone();
    let runtime2 = runtime.clone();

    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let f = std::fs::File::open(archive2).map_err(|e| e.to_string())?;

        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;

        for i in 0..z.len() {
            let mut file = z.by_index(i).map_err(|e| e.to_string())?;

            let rel = file
                .enclosed_name()
                .ok_or("Unsafe Java archive path.")?
                .to_path_buf();

            let out = runtime2.join(rel);

            if file.is_dir() {
                std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }

                let mut dst = std::fs::File::create(out).map_err(|e| e.to_string())?;

                std::io::copy(&mut file, &mut dst).map_err(|e| e.to_string())?;
            }
        }

        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;

    let _ = afs::remove_file(&archive).await;

    fn find_java(path: &Path) -> Option<PathBuf> {
        if path.is_file()
            && path
                .file_name()
                .map(|name| name == "java.exe")
                .unwrap_or(false)
        {
            return Some(path.to_path_buf());
        }

        for entry in std::fs::read_dir(path).ok()?.flatten() {
            if let Some(found) = find_java(&entry.path()) {
                return Some(found);
            }
        }

        None
    }

    find_java(&runtime).ok_or_else(|| "Java was extracted but java.exe was not found.".to_string())
}

#[tauri::command]
async fn ensure_java(
    minecraft_root: Option<String>,
    minecraft_version: String,
) -> Result<String, String> {
    let root = PathBuf::from(
        minecraft_root.unwrap_or_else(|| root_default().to_string_lossy().to_string()),
    );

    let major = required_java_major(&minecraft_version);

    Ok(install_java_runtime(&root, major)
        .await?
        .to_string_lossy()
        .to_string())
}

#[tauri::command]
async fn java_status(minecraft_root: Option<String>) -> Result<Vec<String>, String> {
    let root = PathBuf::from(
        minecraft_root.unwrap_or_else(|| root_default().to_string_lossy().to_string()),
    );

    Ok([8u32, 16, 17, 21]
        .into_iter()
        .map(|major| {
            let path = root
                .join("runtime")
                .join(format!("temurin-{major}"))
                .join("bin")
                .join("java.exe");

            format!(
                "Java {major}: {}",
                if path.exists() {
                    "installed"
                } else {
                    "not installed"
                }
            )
        })
        .collect())
}

#[tauri::command]
async fn detect_java() -> Result<String, String> {
    for candidate in ["java.exe", "java"] {
        if let Ok(output) = Command::new(candidate).arg("-version").output() {
            if output.status.success() {
                return Ok(candidate.to_string());
            }
        }
    }

    Err("System Java was not found.".into())
}

#[tauri::command]
async fn choose_directory(app: tauri::AppHandle) -> Result<String, String> {
    let directory = app.dialog().file().blocking_pick_folder();

    Ok(directory.map(|path| path.to_string()).unwrap_or_default())
}

#[tauri::command]
async fn create_instance(
    name: String,
    version: String,
    loader: String,
    java: String,
    ram: u32,
    minecraft_root: Option<String>,
) -> Result<Instance, String> {
    let root = PathBuf::from(
        minecraft_root.unwrap_or_else(|| root_default().to_string_lossy().to_string()),
    );

    let safe_name = name
        .trim()
        .replace(['<', '>', '"', ':', '/', '\\', '|', '?', '*'], "_");

    let dir = root.join("instances").join(&safe_name);

    afs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;

    let instance = Instance {
        name: safe_name,
        version,
        loader,
        dir: dir.to_string_lossy().to_string(),
        java,
        ram: ram.max(1),
    };

    let config = serde_json::to_vec_pretty(&instance).map_err(|e| e.to_string())?;

    afs::write(dir.join("instance.json"), config)
        .await
        .map_err(|e| e.to_string())?;

    for directory in ["mods", "resourcepacks", "shaderpacks", "saves", "logs"] {
        afs::create_dir_all(dir.join(directory))
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(instance)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct LoaderSelection {
    loader: String,
    minecraft_version: String,
    loader_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct LoaderVersionInfo {
    loader: String,
    minecraft_version: String,
    version: String,
    stable: bool,
    compatible: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct InstalledLoader {
    loader: String,
    minecraft_version: String,
    loader_version: String,
    profile_path: String,
    main_class: Option<String>,
    libraries: Vec<String>,
    installed_at: u64,
}

fn provider_key(loader: &str) -> String {
    loader.to_ascii_lowercase().replace(' ', "-")
}

fn loader_dir(root: &Path, loader: &str, minecraft_version: &str, version: &str) -> PathBuf {
    root.join("instances").join(format!(
        "_loaders/{}/{}/{}",
        provider_key(loader),
        minecraft_version,
        version
    ))
}

fn parse_maven_coordinate(name: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = name.split(':').collect();

    if parts.len() < 3 {
        return None;
    }

    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

fn coordinate_path(name: &str) -> Option<PathBuf> {
    let (group, artifact, version) = parse_maven_coordinate(name)?;

    Some(PathBuf::from(format!(
        "{}/{}/{}/{}-{}.jar",
        group.replace('.', "/"),
        artifact,
        version,
        artifact,
        version
    )))
}

async fn fetch_json<T>(client: &reqwest::Client, url: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| format!("HTTP error from {url}: {e}"))?
        .json::<T>()
        .await
        .map_err(|e| format!("Invalid JSON from {url}: {e}"))
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| format!("HTTP error from {url}: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())
}

fn stable_loader(version: &str) -> bool {
    !(version.contains("alpha")
        || version.contains("beta")
        || version.contains("snapshot")
        || version.contains("rc"))
}

async fn fabric_versions(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<Vec<LoaderVersionInfo>, String> {
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}");

    let value: serde_json::Value = fetch_json(client, &url).await?;

    let mut output = Vec::new();

    for item in value
        .as_array()
        .ok_or("Fabric returned an invalid loader list.")?
    {
        if let Some(version) = item
            .get("loader")
            .and_then(|loader| loader.get("version"))
            .and_then(|value| value.as_str())
        {
            let stable = item
                .get("loader")
                .and_then(|loader| loader.get("stable"))
                .and_then(|value| value.as_bool())
                .unwrap_or_else(|| stable_loader(version));

            output.push(LoaderVersionInfo {
                loader: "Fabric".into(),
                minecraft_version: minecraft_version.into(),
                version: version.into(),
                stable,
                compatible: true,
            });
        }
    }

    Ok(output)
}

async fn quilt_versions(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<Vec<LoaderVersionInfo>, String> {
    let url = format!("https://meta.quiltmc.org/v3/versions/loader/{minecraft_version}");

    let value: serde_json::Value = fetch_json(client, &url).await?;

    let mut output = Vec::new();

    for item in value
        .as_array()
        .ok_or("Quilt returned an invalid loader list.")?
    {
        if let Some(version) = item
            .get("loader")
            .and_then(|loader| loader.get("version"))
            .and_then(|value| value.as_str())
        {
            let stable = item
                .get("loader")
                .and_then(|loader| loader.get("stable"))
                .and_then(|value| value.as_bool())
                .unwrap_or_else(|| stable_loader(version));

            output.push(LoaderVersionInfo {
                loader: "Quilt".into(),
                minecraft_version: minecraft_version.into(),
                version: version.into(),
                stable,
                compatible: true,
            });
        }
    }

    Ok(output)
}

async fn legacy_fabric_versions(
    client: &reqwest::Client,
    minecraft_version: &str,
) -> Result<Vec<LoaderVersionInfo>, String> {
    let url = format!("https://meta.legacyfabric.net/v2/manifest/{minecraft_version}");

    let value: serde_json::Value = fetch_json(client, &url).await?;

    let array = value
        .get("loaders")
        .and_then(|value| value.as_array())
        .or_else(|| value.as_array())
        .ok_or("Legacy Fabric returned no compatible loaders.")?;

    let mut output = Vec::new();

    for item in array {
        let version = item
            .get("version")
            .and_then(|value| value.as_str())
            .or_else(|| item.as_str());

        if let Some(version) = version {
            output.push(LoaderVersionInfo {
                loader: "Legacy Fabric".into(),
                minecraft_version: minecraft_version.into(),
                version: version.into(),
                stable: stable_loader(version),
                compatible: true,
            });
        }
    }

    Ok(output)
}

async fn maven_versions(
    client: &reqwest::Client,
    base: &str,
    group: &str,
    artifact: &str,
    minecraft_version: &str,
) -> Result<Vec<LoaderVersionInfo>, String> {
    let url = format!(
        "{}/{}/{}/maven-metadata.xml",
        base.trim_end_matches('/'),
        group.replace('.', "/"),
        artifact
    );

    let xml = fetch_text(client, &url).await?;

    let regex =
        regex::Regex::new(r"<version>\s*([^<]+)\s*</version>").map_err(|e| e.to_string())?;

    let mut output = Vec::new();

    for capture in regex.captures_iter(&xml) {
        let version = capture[1].trim().to_string();

        if version.starts_with(minecraft_version) {
            output.push(LoaderVersionInfo {
                loader: artifact.into(),
                minecraft_version: minecraft_version.into(),
                version: version.clone(),
                stable: stable_loader(&version),
                compatible: true,
            });
        }
    }

    Ok(output)
}

async fn resolve_loader_versions(
    client: &reqwest::Client,
    loader: &str,
    minecraft_version: &str,
) -> Result<Vec<LoaderVersionInfo>, String> {
    match loader {
        "Fabric" => fabric_versions(client, minecraft_version).await,

        "Quilt" => quilt_versions(client, minecraft_version).await,

        "Legacy Fabric" => legacy_fabric_versions(client, minecraft_version).await,

        "Forge" => {
            maven_versions(
                client,
                "https://maven.minecraftforge.net",
                "net.minecraftforge",
                "forge",
                minecraft_version,
            )
            .await
        }

        "NeoForge" => {
            maven_versions(
                client,
                "https://maven.neoforged.net/releases",
                "net.neoforged",
                "neoforge",
                minecraft_version,
            )
            .await
        }

        "Babric" => Ok(Vec::new()),

        _ => Err(format!("Unknown loader provider: {loader}")),
    }
}

async fn choose_loader_version(
    client: &reqwest::Client,
    loader: &str,
    minecraft_version: &str,
    requested: &Option<String>,
) -> Result<String, String> {
    if let Some(version) = requested {
        let versions = resolve_loader_versions(client, loader, minecraft_version).await?;

        if versions
            .iter()
            .any(|item| item.version == *version && item.compatible)
        {
            return Ok(version.clone());
        }

        return Err(format!(
            "{loader} {version} is not compatible with Minecraft {minecraft_version}."
        ));
    }

    let versions = resolve_loader_versions(client, loader, minecraft_version).await?;

    versions
        .iter()
        .find(|item| item.stable && item.compatible)
        .map(|item| item.version.clone())
        .or_else(|| {
            versions
                .iter()
                .find(|item| item.compatible)
                .map(|item| item.version.clone())
        })
        .ok_or_else(|| {
            format!("No compatible {loader} version was found for Minecraft {minecraft_version}.")
        })
}

async fn download_coordinate(
    client: &reqwest::Client,
    root: &Path,
    base: &str,
    coordinate: &str,
) -> Result<PathBuf, String> {
    let relative = coordinate_path(coordinate)
        .ok_or_else(|| format!("Invalid Maven coordinate: {coordinate}"))?;

    let destination = root.join("libraries").join(&relative);

    if !destination.exists() {
        let url = format!(
            "{}/{}",
            base.trim_end_matches('/'),
            relative.to_string_lossy().replace('\\', "/")
        );

        download_verified(client, &url, &destination, None).await?;
    }

    Ok(destination)
}

fn save_loader_install(root: &Path, install: &InstalledLoader) -> Result<(), String> {
    let directory = loader_dir(
        root,
        &install.loader,
        &install.minecraft_version,
        &install.loader_version,
    );

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let file = directory.join("installation.json");

    let data = serde_json::to_vec_pretty(install).map_err(|e| e.to_string())?;

    std::fs::write(file, data).map_err(|e| e.to_string())
}

fn load_loader_install(
    root: &Path,
    loader: &str,
    minecraft_version: &str,
    version: &str,
) -> Option<InstalledLoader> {
    let path = loader_dir(root, loader, minecraft_version, version).join("installation.json");

    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

async fn install_fabric_provider(
    client: &reqwest::Client,
    root: &Path,
    minecraft_version: &str,
    version: &str,
) -> Result<InstalledLoader, String> {
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{minecraft_version}/{version}/profile/json"
    );

    let profile: serde_json::Value = fetch_json(client, &url).await?;

    let directory = loader_dir(root, "Fabric", minecraft_version, version);

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let profile_path = directory.join("profile.json");

    let profile_data = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;

    std::fs::write(&profile_path, profile_data).map_err(|e| e.to_string())?;

    let mut libraries = Vec::new();

    if let Some(array) = profile.get("libraries").and_then(|value| value.as_array()) {
        for library in array {
            if let Some(name) = library.get("name").and_then(|value| value.as_str()) {
                let base = library
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("https://maven.fabricmc.net/");

                libraries.push(
                    download_coordinate(client, root, base, name)
                        .await?
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }

    let main_class = profile
        .get("mainClass")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let install = InstalledLoader {
        loader: "Fabric".into(),
        minecraft_version: minecraft_version.into(),
        loader_version: version.into(),
        profile_path: profile_path.to_string_lossy().to_string(),
        main_class,
        libraries,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    save_loader_install(root, &install)?;

    Ok(install)
}

async fn install_quilt_provider(
    client: &reqwest::Client,
    root: &Path,
    minecraft_version: &str,
    version: &str,
) -> Result<InstalledLoader, String> {
    let url = format!(
        "https://meta.quiltmc.org/v3/versions/loader/{minecraft_version}/{version}/profile/json"
    );

    let profile: serde_json::Value = fetch_json(client, &url).await?;

    let directory = loader_dir(root, "Quilt", minecraft_version, version);

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let profile_path = directory.join("profile.json");

    let profile_data = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;

    std::fs::write(&profile_path, profile_data).map_err(|e| e.to_string())?;

    let mut libraries = Vec::new();

    if let Some(array) = profile.get("libraries").and_then(|value| value.as_array()) {
        for library in array {
            if let Some(name) = library.get("name").and_then(|value| value.as_str()) {
                let base = library
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("https://maven.quiltmc.org/repository/release/");

                libraries.push(
                    download_coordinate(client, root, base, name)
                        .await?
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }

    let main_class = profile
        .get("mainClass")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let install = InstalledLoader {
        loader: "Quilt".into(),
        minecraft_version: minecraft_version.into(),
        loader_version: version.into(),
        profile_path: profile_path.to_string_lossy().to_string(),
        main_class,
        libraries,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    save_loader_install(root, &install)?;

    Ok(install)
}

async fn install_legacy_fabric_provider(
    client: &reqwest::Client,
    root: &Path,
    minecraft_version: &str,
    version: &str,
) -> Result<InstalledLoader, String> {
    let url = format!(
        "https://meta.legacyfabric.net/v2/versions/loader/{minecraft_version}/{version}/profile/json"
    );

    let profile: serde_json::Value = fetch_json(client, &url).await?;

    let directory = loader_dir(root, "Legacy Fabric", minecraft_version, version);

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let profile_path = directory.join("profile.json");

    let profile_data = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;

    std::fs::write(&profile_path, profile_data).map_err(|e| e.to_string())?;

    let mut libraries = Vec::new();

    if let Some(array) = profile.get("libraries").and_then(|value| value.as_array()) {
        for library in array {
            if let Some(name) = library.get("name").and_then(|value| value.as_str()) {
                let base = library
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("https://repo.legacyfabric.net/legacyfabric/");

                libraries.push(
                    download_coordinate(client, root, base, name)
                        .await?
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }

    let main_class = profile
        .get("mainClass")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let install = InstalledLoader {
        loader: "Legacy Fabric".into(),
        minecraft_version: minecraft_version.into(),
        loader_version: version.into(),
        profile_path: profile_path.to_string_lossy().to_string(),
        main_class,
        libraries,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    save_loader_install(root, &install)?;

    Ok(install)
}

async fn install_maven_installer_provider(
    client: &reqwest::Client,
    root: &Path,
    minecraft_version: &str,
    loader: &str,
    version: &str,
    java: &Path,
) -> Result<InstalledLoader, String> {
    let (base, group, artifact) = match loader {
        "Forge" => (
            "https://maven.minecraftforge.net",
            "net.minecraftforge",
            "forge",
        ),

        "NeoForge" => (
            "https://maven.neoforged.net/releases",
            "net.neoforged",
            "neoforge",
        ),

        _ => return Err("Not a Maven installer provider.".into()),
    };

    let filename = format!("{artifact}-{version}-installer.jar");

    let url = format!(
        "{}/{}/{}/{}/{}",
        base,
        group.replace('.', "/"),
        artifact,
        version,
        filename
    );

    let installer = root.join("downloads").join(&filename);

    download_verified(client, &url, &installer, None).await?;

    let target = root.join("loader-work").join(format!(
        "{}-{}-{}",
        provider_key(loader),
        minecraft_version,
        version
    ));

    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;

    let status = Command::new(java)
        .arg("-jar")
        .arg(&installer)
        .arg("--installClient")
        .arg("--target")
        .arg(&target)
        .status()
        .map_err(|e| e.to_string())?;

    if !status.success() {
        return Err(format!(
            "{loader} installer failed with exit code {:?}",
            status.code()
        ));
    }

    let directory = loader_dir(root, loader, minecraft_version, version);

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let profile_path = directory.join("installer-output.json");

    let info = serde_json::json!({
        "loader": loader,
        "minecraft": minecraft_version,
        "version": version,
        "target": target.to_string_lossy()
    });

    let info_data = serde_json::to_vec_pretty(&info).map_err(|e| e.to_string())?;

    std::fs::write(&profile_path, info_data).map_err(|e| e.to_string())?;

    let install = InstalledLoader {
        loader: loader.into(),
        minecraft_version: minecraft_version.into(),
        loader_version: version.into(),
        profile_path: profile_path.to_string_lossy().to_string(),
        main_class: None,
        libraries: Vec::new(),
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    save_loader_install(root, &install)?;

    Ok(install)
}

async fn install_babric_provider(
    client: &reqwest::Client,
    root: &Path,
    minecraft_version: &str,
    version: &str,
) -> Result<InstalledLoader, String> {
    let url = format!(
        "https://meta.babric.net/v2/versions/loader/{minecraft_version}/{version}/profile/json"
    );

    let profile: serde_json::Value = fetch_json(client, &url).await?;

    let directory = loader_dir(root, "Babric", minecraft_version, version);

    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let profile_path = directory.join("profile.json");

    let profile_data = serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?;

    std::fs::write(&profile_path, profile_data).map_err(|e| e.to_string())?;

    let mut libraries = Vec::new();

    if let Some(array) = profile.get("libraries").and_then(|value| value.as_array()) {
        for library in array {
            if let Some(name) = library.get("name").and_then(|value| value.as_str()) {
                let base = library
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("https://maven.babric.dev/");

                libraries.push(
                    download_coordinate(client, root, base, name)
                        .await?
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }

    let main_class = profile
        .get("mainClass")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let install = InstalledLoader {
        loader: "Babric".into(),
        minecraft_version: minecraft_version.into(),
        loader_version: version.into(),
        profile_path: profile_path.to_string_lossy().to_string(),
        main_class,
        libraries,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    save_loader_install(root, &install)?;

    Ok(install)
}

#[tauri::command]
async fn loader_versions(
    loader: String,
    minecraft_version: String,
) -> Result<Vec<LoaderVersionInfo>, String> {
    let client = reqwest::Client::builder()
        .user_agent("AstroLauncher/0.5")
        .build()
        .map_err(|e| e.to_string())?;

    resolve_loader_versions(&client, &loader, &minecraft_version).await
}

#[tauri::command]
async fn install_loader(
    selection: LoaderSelection,
    minecraft_root: Option<String>,
) -> Result<InstalledLoader, String> {
    let root = PathBuf::from(
        minecraft_root.unwrap_or_else(|| root_default().to_string_lossy().to_string()),
    );

    let client = reqwest::Client::builder()
        .user_agent("AstroLauncher/0.5")
        .build()
        .map_err(|e| e.to_string())?;

    let version = choose_loader_version(
        &client,
        &selection.loader,
        &selection.minecraft_version,
        &selection.loader_version,
    )
    .await?;

    if let Some(existing) = load_loader_install(
        &root,
        &selection.loader,
        &selection.minecraft_version,
        &version,
    ) {
        return Ok(existing);
    }

    match selection.loader.as_str() {
        "Fabric" => {
            install_fabric_provider(&client, &root, &selection.minecraft_version, &version).await
        }

        "Quilt" => {
            install_quilt_provider(&client, &root, &selection.minecraft_version, &version).await
        }

        "Legacy Fabric" => {
            install_legacy_fabric_provider(&client, &root, &selection.minecraft_version, &version)
                .await
        }

        "Babric" => {
            install_babric_provider(&client, &root, &selection.minecraft_version, &version).await
        }

        "Forge" | "NeoForge" => {
            let java_major = required_java_major(&selection.minecraft_version);

            let java = install_java_runtime(&root, java_major).await?;

            install_maven_installer_provider(
                &client,
                &root,
                &selection.minecraft_version,
                &selection.loader,
                &version,
                &java,
            )
            .await
        }

        _ => Err(format!("Unsupported loader: {}", selection.loader)),
    }
}

#[tauri::command]
async fn install_and_launch_vanilla(
    instance: Instance,
    minecraft_root: Option<String>,
    username: String,
) -> Result<String, String> {
    if instance.loader != "Vanilla" {
        return Err(
            "Use the loader-specific installation pipeline before launching this instance. Loader Play integration is being completed in this build."
                .into(),
        );
    }

    let root = PathBuf::from(
        minecraft_root.unwrap_or_else(|| root_default().to_string_lossy().to_string()),
    );

    let client = reqwest::Client::builder()
        .user_agent("AstroLauncher/0.2")
        .build()
        .map_err(|e| e.to_string())?;

    let manifest: Manifest = client
        .get(MANIFEST_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let manifest_version = manifest
        .versions
        .iter()
        .find(|version| version.id == instance.version)
        .ok_or_else(|| {
            format!(
                "Minecraft version {} was not found in Mojang's version manifest.",
                instance.version
            )
        })?;

    let version_json: VersionJson = client
        .get(&manifest_version.url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let version_dir = root.join("versions").join(&instance.version);

    afs::create_dir_all(&version_dir)
        .await
        .map_err(|e| e.to_string())?;

    let version_json_path = version_dir.join(format!("{}.json", instance.version));

    let raw = serde_json::to_vec_pretty(&version_json).map_err(|e| e.to_string())?;

    afs::write(&version_json_path, raw)
        .await
        .map_err(|e| e.to_string())?;

    let client_jar = version_dir.join(format!("{}.jar", instance.version));

    download_verified(
        &client,
        &version_json.downloads.client.url,
        &client_jar,
        Some(&version_json.downloads.client.sha1),
    )
    .await?;

    let libraries = root.join("libraries");

    for library in &version_json.libraries {
        if !windows_library_allowed(&library.rules) {
            continue;
        }

        if let Some(artifact) = &library.downloads.artifact {
            let destination = libraries.join(&artifact.path);

            download_verified(
                &client,
                &artifact.url,
                &destination,
                artifact.sha1.as_deref(),
            )
            .await?;
        }
    }

    if let Some(asset_index) = &version_json.asset_index {
        let index_path = root
            .join("assets")
            .join("indexes")
            .join(format!("{}.json", asset_index.id));

        download_verified(
            &client,
            &asset_index.url,
            &index_path,
            Some(&asset_index.sha1),
        )
        .await?;

        let bytes = afs::read(&index_path).await.map_err(|e| e.to_string())?;

        let index: AssetIndexJson = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

        for object in index.objects.values() {
            let prefix = &object.hash[..2];

            let destination = root
                .join("assets")
                .join("objects")
                .join(prefix)
                .join(&object.hash);

            let url = format!(
                "https://resources.download.minecraft.net/{}/{}",
                prefix, object.hash
            );

            download_verified(&client, &url, &destination, Some(&object.hash)).await?;
        }
    }

    let java = if instance.java.trim().is_empty() || instance.java == "auto" {
        install_java_runtime(&root, required_java_major(&instance.version)).await?
    } else {
        PathBuf::from(&instance.java)
    };

    let uuid = offline_uuid(&username);

    let game_dir = PathBuf::from(&instance.dir);

    afs::create_dir_all(game_dir.join("logs"))
        .await
        .map_err(|e| e.to_string())?;

    let mut classpath = Vec::<String>::new();

    for library in &version_json.libraries {
        if !windows_library_allowed(&library.rules) {
            continue;
        }

        if let Some(artifact) = &library.downloads.artifact {
            classpath.push(libraries.join(&artifact.path).to_string_lossy().to_string());
        }
    }

    classpath.push(client_jar.to_string_lossy().to_string());

    let classpath_string = if cfg!(windows) {
        classpath.join(";")
    } else {
        classpath.join(":")
    };

    let mut command = Command::new(java);

    command
        .current_dir(&game_dir)
        .arg(format!("-Xmx{}G", instance.ram.max(1)))
        .arg(format!(
            "-Djava.library.path={}",
            root.join("natives").display()
        ))
        .arg("-cp")
        .arg(classpath_string)
        .arg(&version_json.main_class)
        .arg("--username")
        .arg(&username)
        .arg("--version")
        .arg(&instance.version)
        .arg("--gameDir")
        .arg(&game_dir)
        .arg("--assetsDir")
        .arg(root.join("assets"))
        .arg("--assetIndex")
        .arg(
            version_json
                .asset_index
                .as_ref()
                .map(|index| index.id.clone())
                .unwrap_or_default(),
        )
        .arg("--uuid")
        .arg(uuid)
        .arg("--accessToken")
        .arg("0")
        .arg("--userType")
        .arg("legacy")
        .arg("--versionType")
        .arg("Astro");

    let child = command
        .spawn()
        .map_err(|e| format!("Could not start Java/Minecraft: {e}"))?;

    Ok(format!(
        "Minecraft {} launched (PID {}).",
        instance.version,
        child.id()
    ))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let _ = app.path();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_java,
            ensure_java,
            java_status,
            choose_directory,
            create_instance,
            loader_versions,
            install_loader,
            install_and_launch_vanilla
        ])
        .run(tauri::generate_context!())
        .expect("error while running Astro Launcher");
}
