import React from "react";
import {createRoot} from "react-dom/client";
import {invoke} from "@tauri-apps/api/core";
import "./styles.css";

type Page="Home"|"Instances"|"Content Hub"|"Settings";
type Instance={name:string;version:string;loader:string;dir:string;java:string;ram:number};

const nav:Page[]=["Home","Instances","Content Hub","Settings"];

function App(){
 const [page,setPage]=React.useState<Page>("Home");
 const [instances,setInstances]=React.useState<Instance[]>([]);
 const [name,setName]=React.useState("My Minecraft");
 const [version,setVersion]=React.useState("1.21.1");
 const [loader,setLoader]=React.useState("Vanilla");
 const [ram,setRam]=React.useState(4);
 const [java,setJava]=React.useState("java");
 const [rootDir,setRootDir]=React.useState("");
 const [profile,setProfile]=React.useState("AstroPlayer");
 const [status,setStatus]=React.useState("Ready");

 async function installLoader(i:Instance){
   try{
     setStatus(`Resolving ${i.loader} for Minecraft ${i.version}…`);
     const r=await invoke<any>("install_loader",{selection:{loader:i.loader,minecraftVersion:i.version,loaderVersion:null},minecraftRoot:rootDir||undefined});
     setStatus(`${r.loader} ${r.loader_version} installed.`);
   }catch(e){setStatus(String(e))}
 }
 async function installAndLaunch(i:Instance){
   try{
     setStatus(`Installing ${i.version}…`);
     const result=await invoke<string>("install_and_launch_vanilla",{
       instance:{name:i.name,version:i.version,loader:i.loader,dir:i.dir,java:i.java,ram:i.ram},
       minecraftRoot:rootDir||undefined,
       username:profile
     });
     setStatus(result);
   }catch(e){setStatus(String(e))}
 }
 async function create(){
   if(!name.trim()||!version.trim())return;
   try{
     const created=await invoke<Instance>("create_instance",{
       name,version,loader,java,ram,minecraftRoot:rootDir||undefined
     });
     setInstances(x=>[...x,created]);
     setStatus(`Created ${created.name}`);
   }catch(e){setStatus(String(e))}
 }
 async function chooseRoot(){
   try{
     const p=await invoke<string>("choose_directory");
     if(p){setRootDir(p);setStatus(`Minecraft directory: ${p}`)}
   }catch(e){setStatus(String(e))}
 }
 async function ensureJava(){
   try{setStatus("Checking Java runtime…"); const p=await invoke<string>("ensure_java",{minecraftRoot:rootDir||undefined,minecraftVersion:version}); setJava(p); setStatus("Correct Java runtime is ready");}catch(e){setStatus(String(e))}
 }
 async function javaStatus(){
   try{const x=await invoke<string[]>("java_status",{minecraftRoot:rootDir||undefined}); setStatus(x.join(" · "));}catch(e){setStatus(String(e))}
 }
 async function detectJava(){
   try{setJava(await invoke<string>("detect_java"));setStatus("Java detected")}catch(e){setStatus(String(e))}
 }
 return <div className="app">
  <aside className="sidebar"><div className="brand"><b>A</b> ASTRO</div>
   <div className="nav">{nav.map(n=><button className={page===n?"active":""} onClick={()=>setPage(n)} key={n}>{n}</button>)}</div>
   <div className="sideStatus">{status}</div>
  </aside>
  <main className="main"><header><div><small>ASTRO LAUNCHER</small><h1>{page}</h1></div><span className="pill">{profile}</span></header>
   {page==="Home"&&<section className="hero"><small>REAL MINECRAFT ENGINE</small><h2>Play Minecraft through Astro.</h2><p>Astro now has a real Vanilla Java installation and launch pipeline. Loaders and content providers plug into the same engine.</p><div><button className="primary" onClick={()=>setPage("Instances")}>Open Instances</button><button className="secondary" onClick={ensureJava}>Install / Check Java</button></div></section>}
   {page==="Instances"&&<section><div className="top"><div><h2>Instances</h2><p>Create and launch real Minecraft Java instances.</p></div><button className="primary" onClick={create}>+ Create</button></div>
    <div className="form"><input value={name} onChange={e=>setName(e.target.value)} placeholder="Instance name"/><input value={version} onChange={e=>setVersion(e.target.value)} placeholder="Minecraft version"/><select value={loader} onChange={e=>setLoader(e.target.value)}><option>Vanilla</option><option>Fabric</option><option>Forge</option><option>NeoForge</option><option>Quilt</option><option>Legacy Fabric</option><option>Babric</option></select><input type="number" min="1" value={ram} onChange={e=>setRam(Number(e.target.value))}/></div>
    <div className="grid">{instances.map((i,k)=><div className="card" key={k}><h3>{i.name}</h3><p>{i.version} · {i.loader} · {i.ram} GB</p><button className="secondary small" onClick={()=>installLoader(i)} disabled={i.loader==="Vanilla"}>{i.loader==="Vanilla"?"No loader":"Install Loader"}</button><button className="primary small" onClick={()=>installAndLaunch(i)}>Install & Play</button></div>)}</div>
   </section>}
   {page==="Content Hub"&&<section><h2>Content Hub</h2><p>Provider architecture is ready for real integrations.</p><div className="grid"><div className="card"><h3>Modrinth</h3><p>API provider: search, versions, files, dependencies and hashes.</p></div><div className="card"><h3>CurseForge</h3><p>Provider adapter slot with API-key configuration.</p></div><div className="card"><h3>PacksMC</h3><p>Provider slot using the site's supported download flow.</p></div></div></section>}
   {page==="Settings"&&<section><div className="card"><h2>Minecraft Directory</h2><p>All Minecraft data lives below this root.</p><div className="path">{rootDir||"Not configured"}</div><button className="primary small" onClick={chooseRoot}>Choose Windows Folder</button></div><div className="card"><h2>Offline Profile</h2><input value={profile} onChange={e=>setProfile(e.target.value)}/><p>This is a local profile identity. It does not bypass Microsoft authentication or server authentication.</p></div><div className="card"><h2>Java</h2><div className="path">{java}</div><button className="secondary small" onClick={ensureJava}>Install / Check Java</button></div></section>}
  </main>
 </div>
}
createRoot(document.getElementById("root")!).render(<App/>);
