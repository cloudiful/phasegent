// Ambient declaration of the Electron preload surface.
//
// The renderer only ever touches `window.phasegent`; the API type is the
// preload builder's own contract (`electron/preload/api.ts`), so the renderer
// cannot drift from the method set the preload script exposes. This file is
// type-only and is erased from the bundle.

import type { PhasegentDesktopApi } from '../../electron/preload/api'

declare global {
  interface Window {
    /** Typed backend surface exposed by the Electron preload script. */
    readonly phasegent?: PhasegentDesktopApi
  }
}

export {}
