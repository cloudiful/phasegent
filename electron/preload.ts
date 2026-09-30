// Preload script: the renderer's only backend surface.
//
// Exposes the typed API object built by `preload/api.ts` under
// `window.phasegent`. The raw `ipcRenderer`, channel names, and Node APIs stay
// in this isolated context and are never handed to the renderer.

import { contextBridge, ipcRenderer } from 'electron'
import { createDesktopApi } from './preload/api'
import { channelFor } from './shared/methods'

const api = createDesktopApi((method, payload) => ipcRenderer.invoke(channelFor(method), payload))

contextBridge.exposeInMainWorld('phasegent', api)
