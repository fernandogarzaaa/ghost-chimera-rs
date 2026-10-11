/**
 * Ghost Console: preload script.
 *
 * Runs in an isolated world (contextIsolation: true, sandbox: true).
 * Exposes only the minimal, read-only surface the renderer needs.
 * No Node APIs leak into the page.
 */

"use strict";

const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("ghostDesktop", {
  platform: process.platform,
  arch: process.arch,
  versions: {
    electron: process.versions.electron,
    chrome: process.versions.chrome,
  },
  /** App version from package.json (async; resolved in the main process). */
  getAppVersion: () => ipcRenderer.invoke("ghost:get-version"),
});
