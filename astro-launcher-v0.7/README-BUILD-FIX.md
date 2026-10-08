# Astro Launcher 0.7 — Build Fix

This build fixes the errors from the TypeScript/Tauri build log supplied by the developer.

## Fixed

- Added `@types/react` and `@types/react-dom` so TypeScript can type-check JSX.
- Moved the real Tauri application/command implementation into `src-tauri/src/lib.rs`.
- Kept `src-tauri/src/main.rs` as the desktop bootstrapper.
- Removed the duplicated `install_loader` registration from the Tauri command list.
- Bumped the project version to 0.7.0.
- Kept the loader engine from v0.5 intact.

The `JSX.IntrinsicElements` errors in the supplied log are consistent with missing React JSX type definitions. TypeScript requires JSX to be enabled and React typings to provide the JSX namespace. See the official TypeScript JSX documentation.

Build on Windows:

```powershell
npm install
npm run build
npx tauri build
```

If the Rust build reports a new error after these fixes, use that new error as the next build target; do not revert to the previous JSX error set.
