/**
 * Ghost Console: Electron main process.
 *
 * Architecture (see SPEC.md section 3): the Rust backend
 * (ghost-console-api) serves HTTP on loopback. This shell spawns the
 * backend as a sidecar binary, waits for it to become healthy, then
 * loads the console UI from it. The shell never implements product
 * logic; it is a window + lifecycle wrapper.
 *
 * Backend resolution order:
 *   1. GHOST_BACKEND_URL env var: skip spawning, use the given base URL
 *      directly (dev / headless-adjacent workflows).
 *   2. A healthy backend already listening on the port (e.g. the user
 *      ran `ghost console` by hand): attach to it, do not spawn or kill.
 *   3. Packaged sidecar at <resources>/bin/ghost[.exe].
 *   4. Workspace build at ../target/{debug,release}/ghost[.exe] (dev).
 * If none exists, a dialog names the missing binary instead of a blank
 * screen.
 *
 * Security posture: the sidecar is always bound to 127.0.0.1
 * (explicit --host flag). Loopback-only is the network boundary; no
 * token is injected by the shell. The console UI keeps its own
 * token flow for users who configure one.
 */

"use strict";

const { app, BrowserWindow, dialog, Menu, shell } = require("electron");
const path = require("path");
const fs = require("fs");
const { spawn } = require("child_process");

// electron-updater is a runtime dependency, but the shell must still
// start (without auto-update) if it is somehow missing.
let autoUpdater = null;
try {
  ({ autoUpdater } = require("electron-updater"));
} catch (err) {
  process.stderr.write(`[shell] electron-updater unavailable, auto-update disabled: ${err.message}\n`);
}

const DEFAULT_PORT = 8766;
const HEALTH_PATH = "/api/console/status";
const STARTUP_TIMEOUT_MS = 30000;
const POLL_INTERVAL_MS = 250;
const MAX_BACKEND_RESTARTS = 3;
const MAX_LOG_BYTES = 5 * 1024 * 1024;

let backendProc = null; // child_process handle when WE spawned the sidecar
let backendExternal = false; // true when attaching to an existing backend
let backendBaseUrl = null;
let mainWindow = null;
let appQuitting = false;
let backendRestarts = 0;
let intentionalRestart = false; // manual "Restart Backend" menu action
let logStream = null;

function userDataFile(...parts) {
  return path.join(app.getPath("userData"), ...parts);
}

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
}

/** Append-only backend log with one-generation rotation. */
function openLogFile() {
  const logDir = userDataFile("logs");
  ensureDir(logDir);
  const logPath = path.join(logDir, "ghost-backend.log");
  try {
    const stat = fs.statSync(logPath);
    if (stat.size > MAX_LOG_BYTES) {
      fs.renameSync(logPath, logPath + ".old");
    }
  } catch {
    // no existing log; nothing to rotate
  }
  logStream = fs.createWriteStream(logPath, { flags: "a" });
  return logPath;
}

function logLine(prefix, message) {
  const line = `[${new Date().toISOString()}] ${prefix} ${message}\n`;
  process.stdout.write(line);
  if (logStream) logStream.write(line);
}

function exeName() {
  return process.platform === "win32" ? "ghost.exe" : "ghost";
}

function findSidecar() {
  const candidates = [
    // Packaged app: electron-builder extraResources -> <resources>/bin/
    path.join(process.resourcesPath, "bin", exeName()),
    // Dev: workspace builds (only when running unpacked).
    path.join(__dirname, "..", "target", "debug", exeName()),
    path.join(__dirname, "..", "target", "release", exeName()),
  ];
  for (const p of candidates) {
    try {
      if (fs.existsSync(p) && fs.statSync(p).isFile()) return p;
    } catch {
      // ignore and try the next candidate
    }
  }
  return null;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function isHealthy(baseUrl) {
  try {
    const res = await fetch(baseUrl + HEALTH_PATH, { signal: AbortSignal.timeout(2000) });
    return res.ok;
  } catch {
    return false;
  }
}

async function waitForHealth(baseUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let interval = POLL_INTERVAL_MS;
  while (Date.now() < deadline) {
    if (await isHealthy(baseUrl)) return true;
    await sleep(interval);
    interval = Math.min(interval * 1.5, 2000); // backoff, max 2s
  }
  return false;
}

function showFatal(message) {
  dialog.showErrorBox("Ghost Console", message);
  appQuitting = true;
  app.quit();
}

function port() {
  const raw = parseInt(process.env.GHOST_PORT || String(DEFAULT_PORT), 10);
  return Number.isFinite(raw) && raw > 0 && raw < 65536 ? raw : DEFAULT_PORT;
}

function spawnSidecar(sidecarPath, backendPort) {
  const child = spawn(
    sidecarPath,
    ["console", "--host", "127.0.0.1", "--port", String(backendPort)],
    { stdio: ["ignore", "pipe", "pipe"] }
  );
  child.stdout.on("data", (d) => logLine("[ghost]", d.toString().trimEnd()));
  child.stderr.on("data", (d) => logLine("[ghost:err]", d.toString().trimEnd()));
  child.on("exit", (code, signal) => onBackendExit(code, signal));
  child.on("error", (err) => {
    logLine("[shell]", `failed to spawn backend: ${err.message}`);
    showFatal(`Could not start the Ghost backend:\n\n${err.message}`);
  });
  return child;
}

async function onBackendExit(code, signal) {
  if (appQuitting || backendExternal) return; // expected shutdown, or not ours
  backendProc = null;
  logLine("[shell]", `backend exited (code=${code}, signal=${signal}, intentional=${intentionalRestart})`);
  if (intentionalRestart) {
    // Manual "Restart Backend": no crash dialog, just come back up.
    intentionalRestart = false;
    backendRestarts = 0;
    const ok = await startSpawnedBackend();
    if (ok && mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.loadURL(backendBaseUrl + "/");
    }
    return;
  }
  if (backendRestarts >= MAX_BACKEND_RESTARTS) {
    showFatal(
      `The Ghost backend keeps crashing (tried ${MAX_BACKEND_RESTARTS + 1} times).\n\n` +
        `See the log at:\n${userDataFile("logs", "ghost-backend.log")}`
    );
    return;
  }
  const { response } = await dialog.showMessageBox(mainWindow, {
    type: "warning",
    title: "Ghost Console",
    message: "The Ghost backend stopped unexpectedly.",
    detail: `Exit code ${code}. You can restart it or quit the app.\nLog: ${userDataFile("logs", "ghost-backend.log")}`,
    buttons: ["Restart backend", "Quit Ghost Console"],
    defaultId: 0,
    cancelId: 1,
  });
  if (response === 1) {
    appQuitting = true;
    app.quit();
    return;
  }
  backendRestarts += 1;
  await startSpawnedBackend();
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.loadURL(backendBaseUrl + "/");
  }
}

/** Spawn our own sidecar and wait for health. Sets backendProc/baseUrl. */
async function startSpawnedBackend() {
  const backendPort = port();
  const baseUrl = `http://127.0.0.1:${backendPort}`;
  const sidecar = findSidecar();
  if (!sidecar) {
    showFatal(
      "Ghost backend binary not found.\n\n" +
        "Looked in the packaged resources and the workspace target dir.\n" +
        "Set GHOST_BACKEND_URL to point at a running backend instead."
    );
    return false;
  }
  logLine("[shell]", `spawning backend: ${sidecar} (port ${backendPort})`);
  backendProc = spawnSidecar(sidecar, backendPort);
  backendExternal = false;
  const healthy = await waitForHealth(baseUrl, STARTUP_TIMEOUT_MS);
  if (!healthy) {
    try {
      backendProc.kill();
    } catch {
      // already gone
    }
    backendProc = null;
    showFatal(`Ghost backend did not become healthy at ${baseUrl} within ${STARTUP_TIMEOUT_MS / 1000}s.`);
    return false;
  }
  backendBaseUrl = baseUrl;
  logLine("[shell]", `backend healthy at ${baseUrl}`);
  return true;
}

async function resolveBackend() {
  // 1. Explicit env override.
  if (process.env.GHOST_BACKEND_URL) {
    backendBaseUrl = process.env.GHOST_BACKEND_URL.replace(/\/$/, "");
    backendExternal = true;
    logLine("[shell]", `using external backend from GHOST_BACKEND_URL: ${backendBaseUrl}`);
    return true;
  }
  // 2. Already-running backend on our port (e.g. `ghost console` by hand).
  const baseUrl = `http://127.0.0.1:${port()}`;
  if (await isHealthy(baseUrl)) {
    backendBaseUrl = baseUrl;
    backendExternal = true;
    logLine("[shell]", `attaching to already-running backend at ${baseUrl}`);
    return true;
  }
  // 3. Spawn our own.
  return startSpawnedBackend();
}

// ---------------------------------------------------------------------------
// Window state (persisted bounds)
// ---------------------------------------------------------------------------

function windowStatePath() {
  return userDataFile("window-state.json");
}

function loadWindowState() {
  try {
    const raw = fs.readFileSync(windowStatePath(), "utf8");
    const state = JSON.parse(raw);
    if (
      Number.isFinite(state.x) &&
      Number.isFinite(state.y) &&
      Number.isFinite(state.width) &&
      Number.isFinite(state.height)
    ) {
      return state;
    }
  } catch {
    // no saved state or corrupt; use defaults
  }
  return null;
}

function saveWindowState() {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  try {
    const bounds = mainWindow.getBounds();
    fs.writeFileSync(windowStatePath(), JSON.stringify(bounds));
  } catch {
    // best effort; a failed write must not break quit
  }
}

// ---------------------------------------------------------------------------
// Window + menu
// ---------------------------------------------------------------------------

function createWindow() {
  const saved = loadWindowState();
  mainWindow = new BrowserWindow({
    x: saved ? saved.x : undefined,
    y: saved ? saved.y : undefined,
    width: saved ? saved.width : 1280,
    height: saved ? saved.height : 860,
    minWidth: 960,
    minHeight: 640,
    title: "Ghost Console",
    backgroundColor: "#0d1117",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  mainWindow.loadURL(backendBaseUrl + "/");
  const saveSoon = (() => {
    let t = null;
    return () => {
      clearTimeout(t);
      t = setTimeout(saveWindowState, 500);
    };
  })();
  mainWindow.on("resize", saveSoon);
  mainWindow.on("move", saveSoon);
  mainWindow.on("closed", () => {
    mainWindow = null;
  });
  buildMenu();
}

function buildMenu() {
  const isMac = process.platform === "darwin";
  const template = [
    {
      label: "Console",
      submenu: [
        {
          label: "Reload Console",
          accelerator: "CmdOrCtrl+R",
          click: () => mainWindow && mainWindow.reload(),
        },
        {
          label: "Force Reload",
          accelerator: "CmdOrCtrl+Shift+R",
          click: () => mainWindow && mainWindow.webContents.reloadIgnoringCache(),
        },
        { type: "separator" },
        {
          label: "Restart Backend",
          click: async () => {
            if (backendExternal || !backendProc) {
              dialog.showMessageBox(mainWindow, {
                type: "info",
                title: "Ghost Console",
                message: "No managed backend to restart.",
                detail: "The app is attached to an external backend (GHOST_BACKEND_URL or a manually started `ghost console`). Restart it where it was started.",
              });
              return;
            }
            intentionalRestart = true;
            try {
              backendProc.kill();
            } catch {
              // already gone; onBackendExit will handle the restart
            }
          },
        },
        { type: "separator" },
        isMac ? { role: "close" } : { role: "quit", label: "Quit Ghost Console" },
      ],
    },
    {
      label: "View",
      submenu: [
        { label: "Zoom In", accelerator: "CmdOrCtrl+Plus", click: () => zoomBy(0.1) },
        { label: "Zoom Out", accelerator: "CmdOrCtrl+-", click: () => zoomBy(-0.1) },
        { label: "Reset Zoom", accelerator: "CmdOrCtrl+0", click: () => setZoom(1) },
        { type: "separator" },
        { role: "togglefullscreen" },
        { role: "toggleDevTools", label: "Toggle Developer Tools" },
      ],
    },
    {
      label: "Help",
      submenu: [
        {
          label: "Check for Updates…",
          click: () => checkForUpdates(true),
        },
        {
          label: "Open Backend Log",
          click: () => shell.openPath(userDataFile("logs", "ghost-backend.log")),
        },
        { type: "separator" },
        {
          label: "About Ghost Console",
          click: () => {
            dialog.showMessageBox(mainWindow, {
              type: "info",
              title: "About Ghost Console",
              message: "Ghost Console",
              detail: `Version ${app.getVersion()}\nElectron ${process.versions.electron}\nChromium ${process.versions.chrome}\n\nLocal-first console for GHOST-Chimera.`,
            });
          },
        },
      ],
    },
  ];
  // macOS puts the first menu under the app name; keep it consistent.
  if (isMac) template[0].label = app.name;
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

function zoomBy(delta) {
  if (!mainWindow) return;
  setZoom(mainWindow.webContents.getZoomFactor() + delta);
}

function setZoom(factor) {
  if (!mainWindow) return;
  mainWindow.webContents.setZoomFactor(Math.min(3, Math.max(0.5, factor)));
}

// ---------------------------------------------------------------------------
// Auto-update (electron-updater, GitHub Releases)
// ---------------------------------------------------------------------------

function setupAutoUpdater() {
  if (!app.isPackaged || !autoUpdater) {
    logLine("[updater]", `skipped: packaged=${app.isPackaged}, available=${!!autoUpdater}`);
    return;
  }
  autoUpdater.autoDownload = false; // prompt before downloading
  autoUpdater.autoInstallOnAppQuit = false; // prompt before installing

  autoUpdater.on("error", (err) => {
    logLine("[updater]", `error: ${err && err.message ? err.message : err}`);
  });
  autoUpdater.on("update-available", async (info) => {
    logLine("[updater]", `update available: ${info.version}`);
    const { response } = await dialog.showMessageBox(mainWindow, {
      type: "info",
      title: "Ghost Console",
      message: `Ghost Console ${info.version} is available.`,
      detail: "Download it now? The update installs on restart.",
      buttons: ["Download update", "Later"],
      defaultId: 0,
      cancelId: 1,
    });
    if (response === 0) autoUpdater.downloadUpdate();
  });
  autoUpdater.on("update-not-available", (info) => {
    logLine("[updater]", `up to date (${info.version})`);
  });
  autoUpdater.on("update-downloaded", async (info) => {
    const { response } = await dialog.showMessageBox(mainWindow, {
      type: "info",
      title: "Ghost Console",
      message: `Ghost Console ${info.version} downloaded.`,
      detail: "Restart now to install the update?",
      buttons: ["Restart and install", "Install on next quit"],
      defaultId: 0,
      cancelId: 1,
    });
    if (response === 0) {
      appQuitting = true;
      autoUpdater.quitAndInstall();
    } else {
      autoUpdater.autoInstallOnAppQuit = true;
    }
  });

  // Delayed check so it never blocks launch.
  setTimeout(() => checkForUpdates(false), 8000);
}

async function checkForUpdates(manual) {
  if (!app.isPackaged || !autoUpdater) {
    if (manual) {
      dialog.showMessageBox(mainWindow, {
        type: "info",
        title: "Ghost Console",
        message: "Updates are only checked in packaged builds.",
      });
    }
    return;
  }
  try {
    await autoUpdater.checkForUpdates();
  } catch (err) {
    logLine("[updater]", `check failed: ${err && err.message ? err.message : err}`);
    if (manual) {
      dialog.showMessageBox(mainWindow, {
        type: "warning",
        title: "Ghost Console",
        message: "Could not check for updates.",
        detail: String((err && err.message) || err),
      });
    }
  }
}

// ---------------------------------------------------------------------------
// App lifecycle
// ---------------------------------------------------------------------------

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on("second-instance", () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.focus();
    }
  });

  app.whenReady().then(async () => {
    const { ipcMain } = require("electron");
    ipcMain.handle("ghost:get-version", () => app.getVersion());

    openLogFile();
    logLine("[shell]", `Ghost Console ${app.getVersion()} starting (packaged=${app.isPackaged})`);

    const ok = await resolveBackend();
    if (ok) {
      createWindow();
      setupAutoUpdater();
    }
    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0 && backendBaseUrl) createWindow();
    });
  });
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});

app.on("before-quit", (event) => {
  appQuitting = true;
  saveWindowState();
  if (backendProc && !backendExternal) {
    // Give the sidecar a moment to exit cleanly, then force it.
    event.preventDefault();
    try {
      const proc = backendProc;
      backendProc = null;
      proc.kill();
      const forceTimer = setTimeout(() => {
        try {
          proc.kill("SIGKILL");
        } catch {
          // already gone
        }
        app.exit(0);
      }, 3000);
      proc.on("exit", () => {
        clearTimeout(forceTimer);
        app.exit(0);
      });
    } catch {
      app.exit(0);
    }
  }
  if (logStream) {
    logLine("[shell]", "shutting down");
    logStream.end();
  }
});
