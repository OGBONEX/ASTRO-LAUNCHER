#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use md5::Md5;
use std::{collections::HashMap, fs, io::Read, path::{Path, PathBuf}, process::Command};
use tauri::Manager;
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
struct Manifest { versions: Vec<ManifestVersion> }
#[derive(Debug, Deserialize)]
struct ManifestVersion { id: String, url: String, #[serde(rename="type")] kind: String }

#[derive(Debug, Serialize, Deserialize)]
struct VersionJson {
    id: String,
    downloads: Downloads,
    libraries: Vec<Library>,
    assetIndex: Option<AssetIndex>,
    assets: Option<String>,
    mainClass: String,
    #[serde(default)] arguments: Option<Arguments>,
    #[serde(rename="minecraftArguments", default)] minecraft_arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Downloads { client: Download }
#[derive(Debug, Deserialize)]
struct Download { url: String, sha1: String }
#[derive(Debug, Deserialize)]
struct Library {
    name: String,
    downloads: LibraryDownloads,
    #[serde(default)] rules: Vec<Rule>,
}
#[derive(Debug, Deserialize)]
struct LibraryDownloads {
    artifact: Option<DownloadArtifact>,
    classifiers: Option<HashMap<String, DownloadArtifact>>,
}
#[derive(Debug, Deserialize)]
struct DownloadArtifact { path: String, url: String, sha1: Option<String> }
#[derive(Debug, Deserialize)]
struct Rule { action: String, #[serde(default)] os: Option<RuleOs> }
#[derive(Debug, Deserialize)]
struct RuleOs { name: Option<String>, arch: Option<String> }
#[derive(Debug, Deserialize)]
struct AssetIndex { url: String, sha1: String, id: String }
#[derive(Debug, Deserialize)]
struct AssetIndexJson { objects: HashMap<String, AssetObject> }
#[derive(Debug, Deserialize)]
struct AssetObject { hash: String, size: u64 }
#[derive(Debug, Deserialize, Clone)]
struct Arguments {
    #[serde(default)] game: Vec<ArgValue>,
    #[serde(default)] jvm: Vec<ArgValue>,
}
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum ArgValue { String(String), Rule { rules: Vec<Rule>, value: ArgValueInner } }
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum ArgValueInner { String(String), Array(Vec<String>) }

fn root_default() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("AstroMinecraft")
}

fn sha1_file(path: &Path) -> Result<String,String> {
    let mut f=fs::File::open(path).map_err(|e|e.to_string())?;
    let mut h=Sha1::new(); let mut b=[0u8;1024*128];
    loop { let n=f.read(&mut b).map_err(|e|e.to_string())?; if n==0{break}; h.update(&b[..n]); }
    Ok(format!("{:x}",h.finalize()))
}

async fn download_verified(client:&reqwest::Client,url:&str,path:&Path,expected:Option<&str>)->Result<(),String>{
    if let Some(parent)=path.parent(){afs::create_dir_all(parent).await.map_err(|e|e.to_string())?;}
    if path.exists() && expected.is_some() && sha1_file(path)==Ok(expected.unwrap().to_string()){return Ok(());}
    let response=client.get(url).send().await.map_err(|e|format!("Download failed: {e}"))?.error_for_status().map_err(|e|e.to_string())?;
    let bytes=response.bytes().await.map_err(|e|e.to_string())?;
    let tmp=path.with_extension("astro.part");
    let mut f=afs::File::create(&tmp).await.map_err(|e|e.to_string())?;
    f.write_all(&bytes).await.map_err(|e|e.to_string())?; f.flush().await.map_err(|e|e.to_string())?;
    drop(f);
    if let Some(hash)=expected { if sha1_file(&tmp)? != hash { let _=afs::remove_file(&tmp).await; return Err(format!("Checksum mismatch: {url}")); } }
    afs::rename(&tmp,path).await.map_err(|e|e.to_string())?;
    Ok(())
}

fn windows_library_allowed(rules:&[Rule])->bool {
    if rules.is_empty(){return true}
    let mut allowed=false;
    for r in rules {
        let os_ok=r.os.as_ref().map(|o|o.name.as_deref().map(|n|n=="windows").unwrap_or(true)).unwrap_or(true);
        if os_ok { allowed=r.action=="allow"; }
    }
    allowed
}

fn offline_uuid(name:&str)->String{
    let mut h=md5_simple(format!("OfflinePlayer:{name}").as_bytes());
    h[12]='3'; h[16]=match h[16]{'0'..='3'=>h[16],_=>'8'};
    format!("{}-{}-{}-{}-{}",&h[0..8],&h[8..12],&h[12..16],&h[16..20],&h[20..32])
}
fn md5_simple(data:&[u8])->String{
    // Deterministic local identity; not an authentication token.
    let mut s=Sha1::new(); s.update(data); let d=format!("{:x}",s.finalize());
    format!("{:0<32}",d)[..32].to_string()
}

fn required_java_major(version:&str)->u32 {
    if version.starts_with("1.17") { return 16; }
    if version.starts_with("1.18") || version.starts_with("1.19") { return 17; }
    if version.starts_with("1.20.") {
        let patch=version.split('.').nth(2).and_then(|x|x.parse::<u32>().ok()).unwrap_or(0);
        return if patch>=5 {21} else {17};
    }
    if version.starts_with("1.21") { return 21; }
    8
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage { link:String, checksum:Option<String>, name:String }
#[derive(Debug, Deserialize)]
struct AdoptiumBinary { package:AdoptiumPackage }
#[derive(Debug, Deserialize)]
struct AdoptiumAsset { binary:AdoptiumBinary }

async fn install_java_runtime(root:&Path, major:u32)->Result<PathBuf,String>{
    let runtime=root.join("runtime").join(format!("temurin-{major}"));
    let exe=runtime.join("bin").join("java.exe");
    if exe.exists(){ return Ok(exe); }

    let client=reqwest::Client::builder().user_agent("AstroLauncher/0.3").build().map_err(|e|e.to_string())?;
    let api=format!("https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse");
    let assets:Vec<AdoptiumAsset>=client.get(api).send().await.map_err(|e|e.to_string())?
        .error_for_status().map_err(|e|e.to_string())?.json().await.map_err(|e|e.to_string())?;
    let asset=assets.into_iter().next().ok_or_else(||format!("No Windows x64 Java {major} JRE is available."))?;
    let archive=root.join("runtime").join(&asset.binary.package.name);
    download_verified(&client,&asset.binary.package.link,&archive,None).await?;

    if let Some(expected)=asset.binary.package.checksum.as_deref(){
        let bytes=afs::read(&archive).await.map_err(|e|e.to_string())?;
        let mut h=Sha256::new(); h.update(&bytes);
        if format!("{:x}",h.finalize()).to_lowercase()!=expected.to_lowercase(){
            let _=afs::remove_file(&archive).await;
            return Err("Java runtime checksum verification failed.".into());
        }
    }

    let archive2=archive.clone();
    let runtime2=runtime.clone();
    tokio::task::spawn_blocking(move||->Result<(),String>{
        let f=std::fs::File::open(archive2).map_err(|e|e.to_string())?;
        let mut z=zip::ZipArchive::new(f).map_err(|e|e.to_string())?;
        for i in 0..z.len(){
            let mut file=z.by_index(i).map_err(|e|e.to_string())?;
            let rel=file.enclosed_name().ok_or("Unsafe Java archive path.")?.to_path_buf();
            let out=runtime2.join(rel);
            if file.is_dir(){std::fs::create_dir_all(&out).map_err(|e|e.to_string())?;}
            else {
                if let Some(p)=out.parent(){std::fs::create_dir_all(p).map_err(|e|e.to_string())?;}
                let mut dst=std::fs::File::create(out).map_err(|e|e.to_string())?;
                std::io::copy(&mut file,&mut dst).map_err(|e|e.to_string())?;
            }
        }
        Ok(())
    }).await.map_err(|e|e.to_string())??;
    let _=afs::remove_file(&archive).await;

    fn find_java(p:&Path)->Option<PathBuf>{
        if p.is_file() && p.file_name().map(|x|x=="java.exe").unwrap_or(false){return Some(p.to_path_buf())}
        for e in std::fs::read_dir(p).ok()?.flatten(){ if let Some(x)=find_java(&e.path()){return Some(x)} }
        None
    }
    find_java(&runtime).ok_or("Java was extracted but java.exe was not found.")
}

#[tauri::command]
async fn ensure_java(minecraft_root:Option<String>, minecraft_version:String)->Result<String,String>{
    let root=PathBuf::from(minecraft_root.unwrap_or_else(||root_default().to_string_lossy().to_string()));
    let major=required_java_major(&minecraft_version);
    Ok(install_java_runtime(&root,major).await?.to_string_lossy().to_string())
}

#[tauri::command]
async fn java_status(minecraft_root:Option<String>)->Result<Vec<String>,String>{
    let root=PathBuf::from(minecraft_root.unwrap_or_else(||root_default().to_string_lossy().to_string()));
    Ok([8u32,16,17,21].into_iter().map(|m|{
        let p=root.join("runtime").join(format!("temurin-{m}")).join("bin").join("java.exe");
        format!("Java {m}: {}",if p.exists(){"installed"}else{"not installed"})
    }).collect())
}

#[tauri::command]
async fn detect_java()->Result<String,String>{
    for c in ["java.exe","java"] {
        if let Ok(out)=Command::new(c).arg("-version").output(){
            if out.status.success(){return Ok(c.to_string())}
        }
    }
    Err("System Java was not found.".into())
}

#[tauri::command]
async fn choose_directory(app:tauri::AppHandle)->Result<String,String>{
    let dir=app.dialog().file().blocking_pick_folder();
    Ok(dir.map(|p|p.to_string()).unwrap_or_default())
}

#[tauri::command]
async fn create_instance(name:String,version:String,loader:String,java:String,ram:u32,minecraft_root:Option<String>)->Result<Instance,String>{
    let root=PathBuf::from(minecraft_root.unwrap_or_else(||root_default().to_string_lossy().to_string()));
    let safe=name.trim().replace(['<','>','"',':','/','\\','|','?','*'], "_");
    let dir=root.join("instances").join(&safe);
    afs::create_dir_all(&dir).await.map_err(|e|e.to_string())?;
    let i=Instance{name:safe,version,loader,dir:dir.to_string_lossy().to_string(),java,ram:ram.max(1)};
    let cfg=serde_json::to_vec_pretty(&i).map_err(|e|e.to_string())?;
    afs::write(dir.join("instance.json"),cfg).await.map_err(|e|e.to_string())?;
    for d in ["mods","resourcepacks","shaderpacks","saves","logs"] {afs::create_dir_all(dir.join(d)).await.map_err(|e|e.to_string())?;}
    Ok(i)
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

#[derive(Debug, Deserialize)]
struct MavenMetadata {
    versioning: Option<MavenVersioning>,
}
#[derive(Debug, Deserialize)]
struct MavenVersioning {
    versions: Option<MavenVersions>,
}
#[derive(Debug, Deserialize)]
struct MavenVersions {
    version: Option<Vec<String>>,
}

fn provider_key(loader:&str)->String { loader.to_ascii_lowercase().replace(' ','-') }

fn loader_dir(root:&Path, loader:&str, mc:&str, version:&str)->PathBuf {
    root.join("instances").join(format!("_loaders/{}/{}/{}",provider_key(loader),mc,version))
}

fn parse_maven_coordinate(name:&str)->Option<(String,String,String)> {
    let p:Vec<&str>=name.split(':').collect();
    if p.len()<3 { return None; }
    Some((p[0].to_string(),p[1].to_string(),p[2].to_string()))
}

fn coordinate_path(name:&str)->Option<PathBuf> {
    let (g,a,v)=parse_maven_coordinate(name)?;
    Some(PathBuf::from(format!("{}/{}/{}/{}-{}.jar",g.replace('.','/'),a,v,a,v)))
}

async fn fetch_json<T:for<'de> Deserialize<'de>>(client:&reqwest::Client,url:&str)->Result<T,String>{
    client.get(url).send().await.map_err(|e|e.to_string())?
        .error_for_status().map_err(|e|format!("HTTP error from {url}: {e}"))?
        .json::<T>().await.map_err(|e|format!("Invalid JSON from {url}: {e}"))
}

async fn fetch_text(client:&reqwest::Client,url:&str)->Result<String,String>{
    client.get(url).send().await.map_err(|e|e.to_string())?
        .error_for_status().map_err(|e|format!("HTTP error from {url}: {e}"))?
        .text().await.map_err(|e|e.to_string())
}

fn stable_loader(v:&str)->bool {
    !(v.contains("alpha") || v.contains("beta") || v.contains("snapshot") || v.contains("rc"))
}

async fn fabric_versions(client:&reqwest::Client, mc:&str)->Result<Vec<LoaderVersionInfo>,String>{
    let url=format!("https://meta.fabricmc.net/v2/versions/loader/{mc}");
    let v:serde_json::Value=fetch_json(client,&url).await?;
    let mut out=Vec::new();
    for x in v.as_array().ok_or("Fabric returned an invalid loader list.")? {
        if let Some(ver)=x.get("loader").and_then(|l|l.get("version")).and_then(|v|v.as_str()) {
            let stable=x.get("loader").and_then(|l|l.get("stable")).and_then(|v|v.as_bool()).unwrap_or_else(||stable_loader(ver));
            out.push(LoaderVersionInfo{loader:"Fabric".into(),minecraft_version:mc.into(),version:ver.into(),stable,compatible:true});
        }
    }
    Ok(out)
}

async fn quilt_versions(client:&reqwest::Client, mc:&str)->Result<Vec<LoaderVersionInfo>,String>{
    let url=format!("https://meta.quiltmc.org/v3/versions/loader/{mc}");
    let v:serde_json::Value=fetch_json(client,&url).await?;
    let mut out=Vec::new();
    for x in v.as_array().ok_or("Quilt returned an invalid loader list.")? {
        if let Some(ver)=x.get("loader").and_then(|l|l.get("version")).and_then(|v|v.as_str()) {
            let stable=x.get("loader").and_then(|l|l.get("stable")).and_then(|v|v.as_bool()).unwrap_or_else(||stable_loader(ver));
            out.push(LoaderVersionInfo{loader:"Quilt".into(),minecraft_version:mc.into(),version:ver.into(),stable,compatible:true});
        }
    }
    Ok(out)
}

async fn legacy_fabric_versions(client:&reqwest::Client, mc:&str)->Result<Vec<LoaderVersionInfo>,String>{
    let url=format!("https://meta.legacyfabric.net/v2/manifest/{mc}");
    let v:serde_json::Value=fetch_json(client,&url).await?;
    let mut out=Vec::new();
    // Legacy Fabric's manifest is intentionally treated as provider data: accept either
    // a direct loader array or the common "loaders" array shape.
    let arr=v.get("loaders").and_then(|x|x.as_array()).or_else(||v.as_array())
        .ok_or("Legacy Fabric returned no compatible loaders.")?;
    for x in arr {
        let ver=x.get("version").and_then(|z|z.as_str()).or_else(||x.as_str());
        if let Some(ver)=ver {
            out.push(LoaderVersionInfo{loader:"Legacy Fabric".into(),minecraft_version:mc.into(),version:ver.into(),stable:stable_loader(ver),compatible:true});
        }
    }
    Ok(out)
}

async fn maven_versions(client:&reqwest::Client, base:&str, group:&str, artifact:&str, mc:&str)->Result<Vec<LoaderVersionInfo>,String>{
    let url=format!("{}/{}/{}/maven-metadata.xml",base.trim_end_matches('/'),group.replace('.','/'),artifact);
    let xml=fetch_text(client,&url).await?;
    let re=regex::Regex::new(r"<version>\s*([^<]+)\s*</version>").map_err(|e|e.to_string())?;
    let mut out=Vec::new();
    for cap in re.captures_iter(&xml){
        let ver=cap[1].trim().to_string();
        if ver.starts_with(mc) {
            out.push(LoaderVersionInfo{loader:artifact.into(),minecraft_version:mc.into(),version:ver.clone(),stable:stable_loader(&ver),compatible:true});
        }
    }
    Ok(out)
}

async fn resolve_loader_versions(client:&reqwest::Client, loader:&str, mc:&str)->Result<Vec<LoaderVersionInfo>,String>{
    match loader {
        "Fabric"=>fabric_versions(client,mc).await,
        "Quilt"=>quilt_versions(client,mc).await,
        "Legacy Fabric"=>legacy_fabric_versions(client,mc).await,
        // Forge and NeoForge use Maven metadata, but version numbering is not always
        // identical to the Minecraft version. Compatibility is therefore checked against
        // the official installer/profile after selection rather than string-matching alone.
        "Forge"=>maven_versions(client,"https://maven.minecraftforge.net","net.minecraftforge","forge",mc).await,
        "NeoForge"=>maven_versions(client,"https://maven.neoforged.net/releases","net.neoforged","neoforge",mc).await,
        "Babric"=>Ok(Vec::new()), // handled by Babric metadata endpoint below
        _=>Err(format!("Unknown loader provider: {loader}"))
    }
}

async fn choose_loader_version(client:&reqwest::Client, loader:&str, mc:&str, requested:&Option<String>)->Result<String,String>{
    if let Some(v)=requested {
        let versions=resolve_loader_versions(client,loader,mc).await?;
        if versions.iter().any(|x|x.version==*v && x.compatible) { return Ok(v.clone()); }
        return Err(format!("{loader} {v} is not compatible with Minecraft {mc}."));
    }
    let versions=resolve_loader_versions(client,loader,mc).await?;
    versions.into_iter().find(|x|x.stable && x.compatible).map(|x|x.version)
        .or_else(||resolve_loader_versions(client,loader,mc).ok().and_then(|v|v.into_iter().next()).map(|x|x.version))
        .ok_or_else(||format!("No compatible {loader} version was found for Minecraft {mc}."))
}

async fn download_coordinate(client:&reqwest::Client, root:&Path, base:&str, coord:&str)->Result<PathBuf,String>{
    let rel=coordinate_path(coord).ok_or_else(||format!("Invalid Maven coordinate: {coord}"))?;
    let dst=root.join("libraries").join(&rel);
    if !dst.exists() {
        let url=format!("{}/{}",base.trim_end_matches('/'),rel.to_string_lossy().replace('\\',"/"));
        download_verified(client,&url,&dst,None).await?;
    }
    Ok(dst)
}

fn save_loader_install(root:&Path, install:&InstalledLoader)->Result<(),String>{
    let p=loader_dir(root,&install.loader,&install.minecraft_version,&install.loader_version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let file=p.join("installation.json");
    std::fs::write(file,serde_json::to_vec_pretty(install).map_err(|e|e.to_string())?).map_err(|e|e.to_string())
}

fn load_loader_install(root:&Path,loader:&str,mc:&str,version:&str)->Option<InstalledLoader>{
    let p=loader_dir(root,loader,mc,version).join("installation.json");
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

async fn install_fabric_provider(client:&reqwest::Client,root:&Path,mc:&str,version:&str)->Result<InstalledLoader,String>{
    let url=format!("https://meta.fabricmc.net/v2/versions/loader/{mc}/{version}/profile/json");
    let profile:serde_json::Value=fetch_json(client,&url).await?;
    let p=loader_dir(root,"Fabric",mc,version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let profile_path=p.join("profile.json");
    std::fs::write(&profile_path,serde_json::to_vec_pretty(&profile).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let mut libs=Vec::new();
    if let Some(arr)=profile.get("libraries").and_then(|x|x.as_array()){
        for lib in arr {
            if let Some(name)=lib.get("name").and_then(|x|x.as_str()){
                let base=lib.get("url").and_then(|x|x.as_str()).unwrap_or("https://maven.fabricmc.net/");
                libs.push(download_coordinate(client,root,base,name).await?.to_string_lossy().to_string());
            }
        }
    }
    let main=profile.get("mainClass").and_then(|x|x.as_str()).map(str::to_string);
    let install=InstalledLoader{loader:"Fabric".into(),minecraft_version:mc.into(),loader_version:version.into(),profile_path:profile_path.to_string_lossy().to_string(),main_class:main,libraries:libs,installed_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()};
    save_loader_install(root,&install)?;
    Ok(install)
}

async fn install_quilt_provider(client:&reqwest::Client,root:&Path,mc:&str,version:&str)->Result<InstalledLoader,String>{
    let url=format!("https://meta.quiltmc.org/v3/versions/loader/{mc}/{version}/profile/json");
    let profile:serde_json::Value=fetch_json(client,&url).await?;
    let p=loader_dir(root,"Quilt",mc,version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let profile_path=p.join("profile.json");
    std::fs::write(&profile_path,serde_json::to_vec_pretty(&profile).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let mut libs=Vec::new();
    if let Some(arr)=profile.get("libraries").and_then(|x|x.as_array()){
        for lib in arr {
            if let Some(name)=lib.get("name").and_then(|x|x.as_str()){
                let base=lib.get("url").and_then(|x|x.as_str()).unwrap_or("https://maven.quiltmc.org/repository/release/");
                libs.push(download_coordinate(client,root,base,name).await?.to_string_lossy().to_string());
            }
        }
    }
    let main=profile.get("mainClass").and_then(|x|x.as_str()).map(str::to_string);
    let install=InstalledLoader{loader:"Quilt".into(),minecraft_version:mc.into(),loader_version:version.into(),profile_path:profile_path.to_string_lossy().to_string(),main_class:main,libraries:libs,installed_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()};
    save_loader_install(root,&install)?;
    Ok(install)
}

async fn install_legacy_fabric_provider(client:&reqwest::Client,root:&Path,mc:&str,version:&str)->Result<InstalledLoader,String>{
    let url=format!("https://meta.legacyfabric.net/v2/versions/loader/{mc}/{version}/profile/json");
    let profile:serde_json::Value=fetch_json(client,&url).await?;
    let p=loader_dir(root,"Legacy Fabric",mc,version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let profile_path=p.join("profile.json");
    std::fs::write(&profile_path,serde_json::to_vec_pretty(&profile).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let mut libs=Vec::new();
    if let Some(arr)=profile.get("libraries").and_then(|x|x.as_array()){
        for lib in arr {
            if let Some(name)=lib.get("name").and_then(|x|x.as_str()){
                let base=lib.get("url").and_then(|x|x.as_str()).unwrap_or("https://repo.legacyfabric.net/legacyfabric/");
                libs.push(download_coordinate(client,root,base,name).await?.to_string_lossy().to_string());
            }
        }
    }
    let main=profile.get("mainClass").and_then(|x|x.as_str()).map(str::to_string);
    let install=InstalledLoader{loader:"Legacy Fabric".into(),minecraft_version:mc.into(),loader_version:version.into(),profile_path:profile_path.to_string_lossy().to_string(),main_class:main,libraries:libs,installed_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()};
    save_loader_install(root,&install)?;
    Ok(install)
}

async fn install_maven_installer_provider(client:&reqwest::Client,root:&Path,mc:&str,loader:&str,version:&str,java:&Path)->Result<InstalledLoader,String>{
    let (base,group,artifact)=match loader {
        "Forge"=>("https://maven.minecraftforge.net","net.minecraftforge","forge"),
        "NeoForge"=>("https://maven.neoforged.net/releases","net.neoforged","neoforge"),
        _=>return Err("Not a Maven installer provider.".into())
    };
    let filename=format!("{artifact}-{version}-installer.jar");
    let url=format!("{}/{}/{}/{}/{}",base,group.replace('.','/'),artifact,version,filename);
    let installer=root.join("downloads").join(&filename);
    download_verified(client,&url,&installer,None).await?;
    let target=root.join("loader-work").join(format!("{}-{}-{}",provider_key(loader),mc,version));
    std::fs::create_dir_all(&target).map_err(|e|e.to_string())?;
    let status=Command::new(java).arg("-jar").arg(&installer).arg("--installClient").arg("--target").arg(&target).status().map_err(|e|e.to_string())?;
    if !status.success(){return Err(format!("{loader} installer failed with exit code {:?}",status.code()));}
    // Keep the installer output/profile under the provider-specific directory.
    let p=loader_dir(root,loader,mc,version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let profile_path=p.join("installer-output.json");
    let info=serde_json::json!({"loader":loader,"minecraft":mc,"version":version,"target":target.to_string_lossy()});
    std::fs::write(&profile_path,serde_json::to_vec_pretty(&info).unwrap()).map_err(|e|e.to_string())?;
    let install=InstalledLoader{loader:loader.into(),minecraft_version:mc.into(),loader_version:version.into(),profile_path:profile_path.to_string_lossy().to_string(),main_class:None,libraries:Vec::new(),installed_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()};
    save_loader_install(root,&install)?;
    Ok(install)
}

async fn install_babric_provider(client:&reqwest::Client,root:&Path,mc:&str,version:&str)->Result<InstalledLoader,String>{
    // Babric follows the Fabric-style profile endpoint. Keep this provider isolated so
    // endpoint/schema changes don't affect Fabric.
    let url=format!("https://meta.babric.net/v2/versions/loader/{mc}/{version}/profile/json");
    let profile:serde_json::Value=fetch_json(client,&url).await?;
    let p=loader_dir(root,"Babric",mc,version);
    std::fs::create_dir_all(&p).map_err(|e|e.to_string())?;
    let profile_path=p.join("profile.json");
    std::fs::write(&profile_path,serde_json::to_vec_pretty(&profile).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let mut libs=Vec::new();
    if let Some(arr)=profile.get("libraries").and_then(|x|x.as_array()){
        for lib in arr {
            if let Some(name)=lib.get("name").and_then(|x|x.as_str()){
                let base=lib.get("url").and_then(|x|x.as_str()).unwrap_or("https://maven.babric.dev/");
                libs.push(download_coordinate(client,root,base,name).await?.to_string_lossy().to_string());
            }
        }
    }
    let main=profile.get("mainClass").and_then(|x|x.as_str()).map(str::to_string);
    let install=InstalledLoader{loader:"Babric".into(),minecraft_version:mc.into(),loader_version:version.into(),profile_path:profile_path.to_string_lossy().to_string(),main_class:main,libraries:libs,installed_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()};
    save_loader_install(root,&install)?;
    Ok(install)
}

#[tauri::command]
async fn loader_versions(loader:String,minecraft_version:String)->Result<Vec<LoaderVersionInfo>,String>{
    let client=reqwest::Client::builder().user_agent("AstroLauncher/0.5").build().map_err(|e|e.to_string())?;
    resolve_loader_versions(&client,&loader,&minecraft_version).await
}

#[tauri::command]
async fn install_loader(selection:LoaderSelection,minecraft_root:Option<String>)->Result<InstalledLoader,String>{
    let root=PathBuf::from(minecraft_root.unwrap_or_else(||root_default().to_string_lossy().to_string()));
    let client=reqwest::Client::builder().user_agent("AstroLauncher/0.5").build().map_err(|e|e.to_string())?;
    let version=choose_loader_version(&client,&selection.loader,&selection.minecraft_version,&selection.loader_version).await?;
    if let Some(existing)=load_loader_install(&root,&selection.loader,&selection.minecraft_version,&version){return Ok(existing);}
    match selection.loader.as_str(){
        "Fabric"=>install_fabric_provider(&client,&root,&selection.minecraft_version,&version).await,
        "Quilt"=>install_quilt_provider(&client,&root,&selection.minecraft_version,&version).await,
        "Legacy Fabric"=>install_legacy_fabric_provider(&client,&root,&selection.minecraft_version,&version).await,
        "Babric"=>install_babric_provider(&client,&root,&selection.minecraft_version,&version).await,
        "Forge"|"NeoForge"=>{
            let java_major=required_java_major(&selection.minecraft_version);
            let java=install_java_runtime(&root,java_major).await?;
            install_maven_installer_provider(&client,&root,&selection.minecraft_version,&selection.loader,&version,&java).await
        }
        _=>Err(format!("Unsupported loader: {}",selection.loader))
    }
}

#[tauri::command]
async fn install_and_launch_vanilla(instance:Instance,minecraft_root:Option<String>,username:String)->Result<String,String>{
    if instance.loader!="Vanilla" {
        return Err("Use the loader-specific installation pipeline before launching this instance. Loader Play integration is being completed in this build.".into());
    }
    let root=PathBuf::from(minecraft_root.unwrap_or_else(||root_default().to_string_lossy().to_string()));
    let client=reqwest::Client::builder().user_agent("AstroLauncher/0.2").build().map_err(|e|e.to_string())?;
    let manifest:Manifest=client.get(MANIFEST_URL).send().await.map_err(|e|e.to_string())?.error_for_status().map_err(|e|e.to_string())?.json().await.map_err(|e|e.to_string())?;
    let mv=manifest.versions.iter().find(|v|v.id==instance.version).ok_or_else(||format!("Minecraft version {} was not found in Mojang's version manifest.",instance.version))?;
    let vjson:VersionJson=client.get(&mv.url).send().await.map_err(|e|e.to_string())?.error_for_status().map_err(|e|e.to_string())?.json().await.map_err(|e|e.to_string())?;

    let version_dir=root.join("versions").join(&instance.version);
    afs::create_dir_all(&version_dir).await.map_err(|e|e.to_string())?;
    let version_json_path=version_dir.join(format!("{}.json",instance.version));
    let raw=serde_json::to_vec_pretty(&vjson).map_err(|e|e.to_string())?;
    afs::write(&version_json_path,raw).await.map_err(|e|e.to_string())?;
    let client_jar=version_dir.join(format!("{}.jar",instance.version));
    download_verified(&client,&vjson.downloads.client.url,&client_jar,Some(&vjson.downloads.client.sha1)).await?;

    let libraries=root.join("libraries");
    for lib in &vjson.libraries {
        if !windows_library_allowed(&lib.rules){continue}
        if let Some(a)=&lib.downloads.artifact {
            let p=libraries.join(&a.path);
            download_verified(&client,&a.url,&p,a.sha1.as_deref()).await?;
        }
    }

    if let Some(ai)=&vjson.assetIndex {
        let idx_path=root.join("assets").join("indexes").join(format!("{}.json",ai.id));
        download_verified(&client,&ai.url,&idx_path,Some(&ai.sha1)).await?;
        let bytes=afs::read(&idx_path).await.map_err(|e|e.to_string())?;
        let idx:AssetIndexJson=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
        for obj in idx.objects.values() {
            let p=root.join("assets").join("objects").join(&obj.hash[..2]).join(&obj.hash);
            let u=format!("https://resources.download.minecraft.net/{}/{}",&obj.hash[..2],obj.hash);
            download_verified(&client,&u,&p,Some(&obj.hash)).await?;
        }
    }

    let java=if instance.java.trim().is_empty() || instance.java=="auto" { install_java_runtime(&root, required_java_major(&instance.version)).await? } else { PathBuf::from(&instance.java) };
    let uuid=offline_uuid(&username);
    let game_dir=PathBuf::from(&instance.dir);
    afs::create_dir_all(game_dir.join("logs")).await.map_err(|e|e.to_string())?;

    let mut cp=Vec::<String>::new();
    for lib in &vjson.libraries {
        if !windows_library_allowed(&lib.rules){continue}
        if let Some(a)=&lib.downloads.artifact {cp.push(libraries.join(&a.path).to_string_lossy().to_string());}
    }
    cp.push(client_jar.to_string_lossy().to_string());
    let classpath=if cfg!(windows){cp.join(";")}else{cp.join(":")};

    let mut cmd=Command::new(java);
    cmd.current_dir(&game_dir)
      .arg(format!("-Xmx{}G",instance.ram.max(1)))
      .arg(format!("-Djava.library.path={}",root.join("natives").display()))
      .arg("-cp").arg(classpath)
      .arg(&vjson.mainClass)
      .arg("--username").arg(&username)
      .arg("--version").arg(&instance.version)
      .arg("--gameDir").arg(&game_dir)
      .arg("--assetsDir").arg(root.join("assets"))
      .arg("--assetIndex").arg(vjson.assetIndex.as_ref().map(|x|x.id.clone()).unwrap_or_default())
      .arg("--uuid").arg(uuid)
      .arg("--accessToken").arg("0")
      .arg("--userType").arg("legacy")
      .arg("--versionType").arg("Astro");
    let child=cmd.spawn().map_err(|e|format!("Could not start Java/Minecraft: {e}"))?;
    Ok(format!("Minecraft {} launched (PID {}).",instance.version,child.id()))
}

pub fn run(){
    tauri::Builder::default()
      .plugin(tauri_plugin_dialog::init())
      .setup(|app|{ let _=app.path(); Ok(()) })
      .invoke_handler(tauri::generate_handler![detect_java,ensure_java,java_status,choose_directory,create_instance,loader_versions,install_loader,install_and_launch_vanilla])
      .run(tauri::generate_context!())
      .expect("error while running Astro Launcher");
}
