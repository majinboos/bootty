import initSiteWasm, { SiteBackend } from "./bootty-site-wasm/bootty_site";
import type { TerminalBackend, TerminalKey, TerminalMouse, TerminalResize } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";

let wasmReady: Promise<unknown> | null = null;

export async function createRustSiteBackend(search = new URLSearchParams()): Promise<TerminalBackend> {
  if (!wasmReady) {
    wasmReady = initSiteWasm();
  }
  await wasmReady;

  const site = SiteBackend.new();
  if (search.has("doom")) {
    site.input("\x1b[B\x1b[B\r");
  }
  let lastFrame = site.frame() as WebTerminalFrame;
  let doomOverlay: import("./doom-tab-overlay").DoomTabOverlay | null = null;
  let doomActive = false;
  let detailFocused = false;

  const render = async (frame: WebTerminalFrame): Promise<WebTerminalFrame> => {
    const { isDoomTab, DoomTabOverlay } = await import("./doom-tab-overlay");
    doomActive = isDoomTab(frame);
    detailFocused = isDetailFocused(frame);
    if (!doomActive) {
      return frame;
    }
    doomOverlay ??= new DoomTabOverlay();
    return doomOverlay.render(frame);
  };

  return {
    label: "bootty ratatui/tuirealm site",
    async start() {
      lastFrame = site.frame() as WebTerminalFrame;
      lastFrame = await render(lastFrame);
      return lastFrame;
    },
    async readFrame() {
      if (doomActive && doomOverlay) {
        lastFrame = doomOverlay.render(lastFrame);
        return lastFrame;
      }
      lastFrame = site.frame() as WebTerminalFrame;
      lastFrame = await render(lastFrame);
      return lastFrame;
    },
    async resize(request: TerminalResize) {
      lastFrame = site.resize(request.cols, request.rows) as WebTerminalFrame;
      lastFrame = await render(lastFrame);
      return lastFrame;
    },
    async write(input: string) {
      lastFrame = site.input(input) as WebTerminalFrame;
      lastFrame = await render(lastFrame);
    },
    wantsKey(_event: TerminalKey, frame: WebTerminalFrame) {
      return (doomActive || isDoomFrame(frame)) && (detailFocused || isDetailFocused(frame));
    },
    async key(event: TerminalKey) {
      if (!doomOverlay) {
        return null;
      }
      const doomFrame = await doomOverlay.key(event, lastFrame);
      if (doomFrame) {
        lastFrame = doomFrame;
      }
      return doomFrame;
    },
    async mouse(event: TerminalMouse) {
      lastFrame = site.mouse(event.kind, event.x, event.y, event.button) as WebTerminalFrame;
      lastFrame = await render(lastFrame);
      return lastFrame;
    },
    async fps(value: number) {
      lastFrame = site.set_fps(value) as WebTerminalFrame;
      lastFrame = await render(lastFrame);
      return lastFrame;
    },
  };
}

function isDoomFrame(frame: WebTerminalFrame): boolean {
  return frameText(frame).includes("Doom runs inside this terminal frame.");
}

function isDetailFocused(frame: WebTerminalFrame): boolean {
  return frameText(frame).includes("Detail:");
}

function frameText(frame: WebTerminalFrame): string {
  return frame.cells.map((cell) => cell.text).join("");
}
