import type { WebTerminalFrame } from "./terminal-types";

export type TerminalResize = {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
};

export type TerminalMouse = {
  kind: "move" | "down" | "up" | "leave";
  x: number;
  y: number;
  button: number;
};

export type TerminalKey = {
  kind: "down" | "up";
  key: string;
  code: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  repeat: boolean;
};

export type TerminalBackend = {
  label: string;
  start(): Promise<WebTerminalFrame>;
  readFrame(): Promise<WebTerminalFrame>;
  resize(request: TerminalResize): Promise<WebTerminalFrame>;
  write(input: string): Promise<void>;
  wantsKey?(event: TerminalKey, frame: WebTerminalFrame): boolean;
  key?(event: TerminalKey): Promise<WebTerminalFrame | null>;
  mouse?(event: TerminalMouse): Promise<WebTerminalFrame>;
  fps?(value: number): Promise<WebTerminalFrame>;
};

export async function createTerminalBackend(): Promise<TerminalBackend> {
  const search = new URLSearchParams(window.location.search);
  const backendMode = String(import.meta.env.VITE_TERMINAL_BACKEND ?? "");
  if (backendMode === "doom") {
    const { createDoomSiteBackend } = await import("./doom-site-backend");
    return createDoomSiteBackend(search);
  }

  if (import.meta.env.MODE === "github-pages" || backendMode === "site") {
    const { createRustSiteBackend } = await import("./rust-site-backend");
    return createRustSiteBackend(search);
  }

  const { createTauriTerminalBackend } = await import("./tauri-terminal-backend");
  return createTauriTerminalBackend();
}
