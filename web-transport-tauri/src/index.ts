import { isTauri } from "@tauri-apps/api/core";
import WebTransportTauri from "./polyfill.ts";

// Install polyfill if WebTransport is not available, returning true if installed
export function install(): boolean {
    if ("WebTransport" in globalThis) return false;
    if (!isTauri()) return false;

    // biome-ignore lint/suspicious/noExplicitAny: polyfill
    (globalThis as any).WebTransport = WebTransportTauri;
    return true;
}

export default WebTransportTauri;
