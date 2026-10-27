/**
 * Global declaration for the Tauri WebView bridge.
 *
 * The desktop build injects `window.__TAURI__` (Tauri v2 "withGlobalTauri"
 * option). The frontend only uses `core.invoke` for commands and `event.listen`
 * for the server-sent task events.
 */
export {};

declare global {
  interface Window {
    __TAURI__?: {
      core?: {
        invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
      event?: {
        listen: (
          event: string,
          handler: (e: { payload: unknown }) => void,
        ) => Promise<() => void>;
      };
    };
  }
}
