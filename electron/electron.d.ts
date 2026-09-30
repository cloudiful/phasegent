// Minimal declaration of the Electron runtime API used by this app.
//
// The `electron` devDependency is installed by the packaging workflow, where
// the module ships its own complete typings. Until then `tsc --noEmit` has no
// `electron` types to resolve, so this file declares exactly the surface the
// main process and preload script touch, and nothing else.

declare module 'electron' {
  export interface Event {
    preventDefault(): void
  }

  export interface WebPreferences {
    preload?: string
    contextIsolation?: boolean
    nodeIntegration?: boolean
    sandbox?: boolean
    webviewTag?: boolean
    allowRunningInsecureContent?: boolean
  }

  export interface BrowserWindowConstructorOptions {
    width?: number
    height?: number
    minWidth?: number
    minHeight?: number
    show?: boolean
    title?: string
    autoHideMenuBar?: boolean
    backgroundColor?: string
    webPreferences?: WebPreferences
  }

  export interface HandlerDetails {
    url: string
  }

  export interface WebContents {
    on(event: 'will-navigate', listener: (event: Event, url: string) => void): void
    on(event: 'will-attach-webview', listener: (event: Event) => void): void
    setWindowOpenHandler(handler: (details: HandlerDetails) => { action: 'deny' }): void
  }

  export class BrowserWindow {
    constructor(options?: BrowserWindowConstructorOptions)
    readonly webContents: WebContents
    once(event: 'ready-to-show', listener: () => void): void
    on(event: 'closed', listener: () => void): void
    show(): void
    loadURL(url: string): Promise<void>
    isMinimized(): boolean
    restore(): void
    focus(): void
    static getAllWindows(): BrowserWindow[]
  }

  export interface OnHeadersReceivedDetails {
    url: string
    responseHeaders?: Record<string, string[] | undefined>
  }

  export interface WebRequest {
    onHeadersReceived(
      filter: { urls: string[] },
      listener: (
        details: OnHeadersReceivedDetails,
        callback: (response: { responseHeaders: Record<string, string[]> }) => void,
      ) => void,
    ): void
  }

  export interface Session {
    webRequest: WebRequest
    setPermissionRequestHandler(
      handler: (contents: unknown, permission: string, callback: (granted: boolean) => void) => void,
    ): void
    setPermissionCheckHandler(
      handler: (contents: unknown, permission: string, requestingOrigin: string) => boolean,
    ): void
  }

  export interface IpcMainInvokeEvent {
    readonly senderId: number
  }

  export interface IpcMain {
    handle(
      channel: string,
      listener: (event: IpcMainInvokeEvent, ...args: unknown[]) => unknown,
    ): void
  }

  export interface SchemePrivileges {
    standard?: boolean
    secure?: boolean
    supportFetchAPI?: boolean
    codeCache?: boolean
  }

  export interface Protocol {
    handle(scheme: string, handler: (request: { url: string }) => Promise<unknown>): void
    registerSchemesAsPrivileged(schemes: Array<{ scheme: string, privileges?: SchemePrivileges }>): void
  }

  export interface Net {
    fetch(input: string): Promise<unknown>
  }

  export interface App {
    readonly isPackaged: boolean
    getAppPath(): string
    requestSingleInstanceLock(): boolean
    quit(): void
    whenReady(): Promise<void>
    on(event: 'second-instance', listener: () => void): void
    on(event: 'window-all-closed', listener: () => void): void
    on(event: 'activate', listener: () => void): void
    on(event: 'will-quit', listener: () => void): void
    on(
      event: 'web-contents-created',
      listener: (event: Event, contents: WebContents) => void,
    ): void
  }

  export const app: App
  export const ipcMain: IpcMain
  export const session: Session
  export const protocol: Protocol
  export const net: Net

  export const contextBridge: {
    exposeInMainWorld(key: string, api: unknown): void
  }

  export const ipcRenderer: {
    invoke(channel: string, ...args: unknown[]): Promise<unknown>
  }
}
