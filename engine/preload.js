'use strict';
// Minimal bridge from the page's main world to the engine. Exposes only an event
// sink, plus the configuration lookup on our own local page (devtoken mode).
const { contextBridge, ipcRenderer } = require('electron');

const mode = (process.argv.find((a) => a.startsWith('--am-mode=')) || '').split('=')[1];

const bridge = {
  emit: (type, data) => ipcRenderer.send('am:event', String(type), data),
};
if (mode === 'devtoken') bridge.config = () => ipcRenderer.invoke('am:config');

contextBridge.exposeInMainWorld('__amBridge', bridge);
