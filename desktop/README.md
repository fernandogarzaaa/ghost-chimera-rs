# Ghost Console desktop shell (Electron)

Phase 4 component. This directory is the Electron wrapper around the
Rust console backend (`ghost-console-api`).

## How it works

`main.js` is the shell:

1. Resolves the backend: `GHOST_BACKEND_URL` env var wins (dev), else
   attaches to an already-healthy backend on the port (e.g. a manual
   `ghost console`), else spawns the packaged sidecar at
   `<resources>/bin/ghost[.exe]`, else the workspace `target/debug`
   (or `release`) build.
2. Spawns `ghost console --host 127.0.0.1 --port 8766` and polls
   `/api/console/status` until healthy (30s timeout, backoff).
3. Opens a `BrowserWindow` on the console URL with `contextIsolation`,
   no `nodeIntegration`, sandboxed renderer. Window bounds persist
   across launches.
4. Single-instance lock: a second launch focuses the existing window.
5. Backend crashes show a Restart/Quit dialog (3 restarts max, then a
   fatal dialog naming the log file). Quitting the app kills a
   sidecar it spawned; attached/external backends are left alone.
6. Auto-update via `electron-updater` against GitHub Releases
   (packaged builds only): checks shortly after launch, prompts
   before downloading, prompts before installing.

`preload.js` exposes a minimal read-only `window.ghostDesktop`
bridge (platform, arch, versions, async app version). The console UI
itself is served by the backend over loopback HTTP, so there is no
bundled renderer HTML to keep in sync.

Sidecar stdout/stderr go to
`<userData>/logs/ghost-backend.log` (rotated at 5 MB). "Open Backend
Log" in the Help menu opens it.

## Sidecar layout (what the release workflow stages)

```
desktop/sidecar/
  ghost.exe            # windows-latest job
  ghost                # ubuntu-latest job
  mac-x64/ghost        # macos job, x86_64
  mac-arm64/ghost      # macos job, aarch64
```

`package.json` `build` config maps these per OS via electron-builder
`extraResources` (macOS uses the `${arch}` macro). The packaged layout
is always `<resources>/bin/ghost[.exe]`.

## Local dev

```bash
cd desktop
npm install
npm start                 # spawns ../target/debug/ghost
GHOST_BACKEND_URL=http://127.0.0.1:8766 npm start   # attach instead
GHOST_PORT=8777 npm start # spawn on another port
```

`npm install` needs network for the Electron binary download.
`node --check` on both JS files runs without it; full packaging is
verified by the release workflow on tag builds.

## Release notes

- `publish.owner`/`publish.repo` in `package.json` must match the repo
  where GitHub Releases are published, or auto-update resolves the
  wrong feed. Currently `fernandogarzaaa/GHOST-Chimera`.
- Version is synced with the Cargo workspace by
  `.github/scripts/check-version.py` (tag, Cargo.toml,
  desktop/package.json must agree).
- See `TESTING.md` for the per-OS verification checklist and
  `../RELEASING.md` for signing and the release process.
