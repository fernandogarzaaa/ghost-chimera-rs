# Testing the Ghost Console desktop app

How to verify the packaged app on each OS. The app is an Electron
shell around the Rust `ghost` sidecar binary; it spawns
`ghost console --host 127.0.0.1 --port <port>` on launch and loads
the console UI from `http://127.0.0.1:<port>/`.

## Quick dev loop (no packaging)

Prereqs: Node 20+, a debug/release Rust build (`cargo build` in the
workspace root produces `target/debug/ghost`).

```bash
cd desktop
npm install
npm start
```

What to check:

1. The window opens and shows the Ghost Console UI (served from the
   sidecar, not from `desktop/`).
2. DevTools console has no errors from `preload.js`
   (`window.ghostDesktop.platform` is defined).
3. `ask` works in the console chat (deterministic backend = honest
   offline mode, no provider keys needed).
4. Quit the app; confirm no `ghost` process is left behind
   (`ps aux | grep ghost` / Task Manager).

Dev overrides:

- `GHOST_BACKEND_URL=http://127.0.0.1:9999 npm start` — attach to an
  already-running backend instead of spawning one.
- `GHOST_PORT=8777 npm start` — spawn the sidecar on another port.

## What the shell does (test each)

| Behavior | How to verify |
|---|---|
| Spawns sidecar | Launch; `logs/ghost-backend.log` appears under the userData dir (`%APPDATA%/Ghost Console/logs` on Windows, `~/Library/Application Support/Ghost Console/logs` on macOS, `~/.config/Ghost Console/logs` on Linux). |
| Waits for health | Temporarily break the sidecar (rename it); launch shows "backend binary not found" dialog, no blank window. |
| Crash restart | Kill the `ghost` process while the app runs; a dialog offers Restart/Quit. Restart reloads the UI. After 3 crashes, a fatal dialog names the log path. |
| Graceful shutdown | Quit the app; no `ghost` process remains. |
| Single instance | Launch the app twice; the second launch focuses the first window instead of opening a new one. |
| External backend | Start `ghost console` by hand on port 8766, then launch the app: it attaches (log says "attaching to already-running backend") and quitting the app does NOT kill your manual backend. |
| Window state | Resize/move, quit, relaunch: bounds are restored. |
| Menu | Every item works: Reload, Force Reload, Restart Backend, Zoom In/Out/Reset, DevTools, Fullscreen, Check for Updates, Open Backend Log, About, Quit. |
| Auto-update | Only in packaged builds. After install, Help > Check for Updates contacts GitHub Releases. When a newer release exists: prompt to download, then prompt to restart-and-install. Declining the restart installs on next quit. |

## Packaged builds (CI)

`npm run dist:win` / `dist:mac` / `dist:linux` (or the `Release`
workflow on a `v*` tag) produce:

- Windows: NSIS installer (`Ghost Console Setup 0.5.0.exe`), per-user,
  Start Menu + optional desktop shortcut, install dir changeable.
- macOS: `Ghost Console-0.5.0-x64.dmg` and `-arm64.dmg`.
- Linux: `Ghost Console-0.5.0.AppImage` and `ghost-console_0.5.0_amd64.deb`.

Per-OS verification:

1. **Windows (primary — Inan's machine):** run the NSIS installer,
   accept defaults, launch from Start Menu. Confirm the console opens
   with zero configuration. Check `%APPDATA%/Ghost Console/logs/`
   exists after first run. Uninstall via Add/Remove Programs; confirm
   no `ghost.exe` remains.
2. **macOS:** open the dmg, drag to Applications, launch. On first
   launch without notarization Gatekeeper will warn — right-click >
   Open to bypass for unsigned alphas. Verify both x64 and arm64 dmgs
   on matching hardware if available.
3. **Linux:** `chmod +x` the AppImage and run; install the deb with
   `dpkg -i` and launch from the app menu. Confirm the tray-less
   single window and clean quit.

## Must-verify items (not checkable in this sandbox)

- `publish.owner`/`publish.repo` in `desktop/package.json` must match
  the repo where GitHub Releases are published, or auto-update
  resolves the wrong feed. Currently set to
  `fernandogarzaaa/GHOST-Chimera`; update when the Rust workspace gets
  its own repo.
- Windows/macOS code signing and macOS notarization need the secrets
  in RELEASING.md; unsigned builds show SmartScreen/Gatekeeper
  warnings (expected for alphas).
- `electron-builder --dir` unpacked-layout check and real installer
  runs need their target OS runners (CI covers this on tag builds).
- The `${arch}` macro in the macOS `extraResources` path
  (`sidecar/mac-${arch}/`) is per electron-builder docs; confirm on
  the first real mac release that the sidecar lands in
  `<resources>/bin/ghost`.
