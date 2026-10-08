# Astro Launcher v0.2

Real Minecraft Java Edition foundation.

## This milestone
- Tauri 2 + React + TypeScript + Rust
- Persistent instance definitions
- Mojang version manifest discovery
- Vanilla version installation from Mojang's official metadata
- Minecraft client/library/asset downloads
- SHA-1 verification
- Java detection
- Real Java process launch
- Offline/local profile launch arguments
- Instance logs directory
- Architecture ready for loader providers

This launcher does not bypass Minecraft authentication or obtain paid game files from unofficial sources. Offline profiles are local identities and multiplayer access still depends on the server's authentication policy.

## Build
Install Node.js, Rust, and Tauri prerequisites for your platform.
Then:
npm install
npm run tauri dev

For a production build:
npm run tauri build

## Java Manager
Astro maps Minecraft versions to Java 8, 16, 17, or 21, downloads a Windows x64 Temurin JRE on demand, verifies its SHA-256 checksum when supplied by the provider, extracts it under `runtime/`, and reuses it across instances.\n
## Loader providers
Fabric and Quilt metadata/profile resolution are wired to their official metadata services. Forge and NeoForge use their official Maven installer artifacts and run the official client installer. Loader Play integration still requires provider-specific launch-profile/classpath handling before claiming every loader is fully launch-ready.\n
# v0.5 Loader Engine

Implemented provider architecture:
- Fabric: official Fabric Meta loader/profile endpoint.
- Quilt: official Quilt Meta loader/profile endpoint.
- Legacy Fabric: official Legacy Fabric Meta manifest/profile endpoint.
- Forge: official Forge Maven metadata + installer.
- NeoForge: official NeoForge Maven metadata + installer.
- Babric: isolated Babric profile provider.

The provider engine:
1. discovers versions for a Minecraft version;
2. validates an explicitly requested loader version;
3. selects the newest stable compatible provider when no version is requested;
4. downloads the provider's official profile;
5. resolves declared Maven libraries into the shared library cache;
6. persists an installation record;
7. reuses an already-installed exact loader profile.

Important: Forge/NeoForge installers intentionally remain isolated from Fabric-style profile parsing because their installer-generated launch structures differ. The next engine stage should consume the generated installer profile and feed all loader profiles through the unified launch-argument resolver.
