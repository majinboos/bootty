import { BrowserDoomEngine, type DoomAssetUrls } from "./doom-engine";
import { embeddedDoomAssets } from "./doom-embedded-assets";
import { mapBrowserKeyToDoom } from "./doom-keys";
import type { TerminalBackend, TerminalKey, TerminalResize } from "./terminal-api";
import type { WebCell, WebColor, WebImage, WebTerminalFrame } from "./terminal-types";

const DEFAULT_ASSETS: DoomAssetUrls = {
  glueUrl: "https://raw.githubusercontent.com/badlogic/pi-doom/main/doom/build/doom.js",
  wasmUrl: "https://raw.githubusercontent.com/badlogic/pi-doom/main/doom/build/doom.wasm",
  wadUrl: "https://raw.githubusercontent.com/badlogic/pi-doom/main/doom1.wad",
};

const CELL_WIDTH = 10;
const CELL_HEIGHT = 18;
const DOOM_TICK_MS = 1000 / 35;

type DoomRenderMode = "image" | "halfblock";

export function createDoomSiteBackend(search: URLSearchParams): TerminalBackend {
  const assets = assetUrls(search);
  const renderMode: DoomRenderMode = search.get("render") === "halfblock" ? "halfblock" : "image";
  let cols = 96;
  let rows = 36;
  let status = "loading doomgeneric wasm";
  const engine = new BrowserDoomEngine(assets, (nextStatus) => {
    status = nextStatus;
  });
  let initPromise: Promise<void> | null = null;
  let ready = false;
  let lastTickAt = performance.now();
  let tickRemainder = 0;
  let frameRevision = 0;

  const ensureInit = () => {
    initPromise ??= engine
      .init()
      .then(() => {
        ready = true;
        status = renderMode === "halfblock" ? "running in half-block terminal mode" : "running as Bootty image layer";
        lastTickAt = performance.now();
      })
      .catch((error: unknown) => {
        status = error instanceof Error ? error.message : String(error);
      });
    return initPromise;
  };

  const frame = () => {
    if (ready) {
      if (
        tickToNow(
          engine,
          () => lastTickAt,
          (value) => (lastTickAt = value),
          () => tickRemainder,
          (value) => (tickRemainder = value),
        ) > 0
      ) {
        frameRevision += 1;
      }
    }
    return renderFrame(cols, rows, engine, ready, status, renderMode, assets, frameRevision);
  };

  return {
    label: "doomgeneric wasm site",
    async start() {
      ensureInit();
      return frame();
    },
    async readFrame() {
      return frame();
    },
    async resize(request: TerminalResize) {
      cols = request.cols;
      rows = request.rows;
      return frame();
    },
    async write(input: string) {
      if (!ready) {
        return;
      }
      for (const char of input) {
        const code = char.charCodeAt(0);
        engine.pushKey(true, code);
        engine.pushKey(false, code);
      }
    },
    wantsKey() {
      return true;
    },
    async key(event: TerminalKey) {
      if (!ready || event.metaKey || event.repeat) {
        return frame();
      }
      for (const key of mapBrowserKeyToDoom(event)) {
        engine.pushKey(event.kind === "down", key);
      }
      return frame();
    },
  };
}

function assetUrls(search: URLSearchParams): DoomAssetUrls | ReturnType<typeof embeddedDoomAssets> {
  if (!search.has("doomJs") && !search.has("doomWasm") && !search.has("wadUrl")) {
    return embeddedDoomAssets();
  }

  return {
    glueUrl: search.get("doomJs") || DEFAULT_ASSETS.glueUrl,
    wasmUrl: search.get("doomWasm") || DEFAULT_ASSETS.wasmUrl,
    wadUrl: search.get("wadUrl") || DEFAULT_ASSETS.wadUrl,
  };
}

function tickToNow(
  engine: BrowserDoomEngine,
  getLastTickAt: () => number,
  setLastTickAt: (value: number) => void,
  getRemainder: () => number,
  setRemainder: (value: number) => void,
): number {
  const now = performance.now();
  const elapsed = now - getLastTickAt() + getRemainder();
  const ticks = Math.min(5, Math.floor(elapsed / DOOM_TICK_MS));
  for (let index = 0; index < ticks; index += 1) {
    engine.tick();
  }
  setRemainder(elapsed - ticks * DOOM_TICK_MS);
  setLastTickAt(now);
  return ticks;
}

function renderFrame(
  cols: number,
  rows: number,
  engine: BrowserDoomEngine,
  ready: boolean,
  status: string,
  renderMode: DoomRenderMode,
  assets: DoomAssetUrls | ReturnType<typeof embeddedDoomAssets>,
  frameRevision: number,
): WebTerminalFrame {
  document.title = `Bootty / DOOM - ${status}`;
  const cells: WebCell[] = [];
  addText(cells, 1, 0, "Bootty / DOOM", green(), null, true);
  addText(cells, 16, 0, status, cyan(), null, false);
  addText(cells, 1, rows - 2, "WASD/arrows move  F/Ctrl fire  Space use  Q pause  1-7 weapons", gray(), null, false);
  addText(cells, 1, rows - 1, `assets: ${assetLabel(assets)}`, darkGray(), null, false);

  const images: WebImage[] = [];
  if (ready) {
    if (renderMode === "halfblock") {
      const rgba = engine.getFrameRGBA();
      renderHalfBlocks(cells, rgba, engine.width, engine.height, cols, Math.max(1, rows - 4));
    } else {
      images.push(doomImage(engine.getFrameBGRX(), engine.width, engine.height, cols, rows, frameRevision));
    }
  } else {
    addText(cells, 1, 3, "Fetching doom.js, doom.wasm, and a WAD. Add ?doom to run this mode.", yellow(), null, false);
  }

  return {
    cols,
    rows,
    cellWidth: CELL_WIDTH,
    cellHeight: CELL_HEIGHT,
    colors: {
      background: { r: 15, g: 17, b: 26 },
      foreground: { r: 192, g: 202, b: 245 },
      cursor: null,
    },
    cursor: null,
    cells,
    images,
  };
}

function doomImage(
  rgba: Uint8Array<ArrayBufferLike>,
  width: number,
  height: number,
  cols: number,
  rows: number,
  revision: number,
): WebImage {
  const surfaceWidth = cols * CELL_WIDTH;
  const surfaceHeight = rows * CELL_HEIGHT;
  const headerHeight = CELL_HEIGHT * 2;
  const footerHeight = CELL_HEIGHT * 2;
  const availableWidth = surfaceWidth - CELL_WIDTH * 2;
  const availableHeight = Math.max(CELL_HEIGHT, surfaceHeight - headerHeight - footerHeight);
  const scale = Math.min(availableWidth / width, availableHeight / height);
  const gameWidth = Math.floor(width * scale);
  const gameHeight = Math.floor(height * scale);
  const left = Math.floor((surfaceWidth - gameWidth) / 2);
  const top = headerHeight + Math.floor((availableHeight - gameHeight) / 2);

  return {
    key: "doom-frame",
    layer: "belowText",
    imageWidth: width,
    imageHeight: height,
    source: { minX: 0, minY: 0, maxX: width, maxY: height },
    destination: { minX: left, minY: top, maxX: left + gameWidth, maxY: top + gameHeight },
    pixelFormat: "bgrx",
    revision,
    rgba,
  };
}

function renderHalfBlocks(cells: WebCell[], rgba: Uint8Array, width: number, height: number, cols: number, rows: number): void {
  const top = 2;
  const targetRows = Math.max(1, rows);
  const scaleX = width / cols;
  const scaleY = height / (targetRows * 2);
  for (let row = 0; row < targetRows; row += 1) {
    const srcY1 = Math.floor(row * 2 * scaleY);
    const srcY2 = Math.floor((row * 2 + 1) * scaleY);
    for (let col = 0; col < cols; col += 1) {
      const srcX = Math.floor(col * scaleX);
      const topColor = sample(rgba, width, srcX, srcY1);
      const bottomColor = sample(rgba, width, srcX, srcY2);
      cells.push(cell(col, top + row, "▀", topColor, bottomColor, false));
    }
  }
}

function sample(rgba: Uint8Array, width: number, x: number, y: number): WebColor {
  const offset = (y * width + x) * 4;
  return {
    r: rgba[offset] ?? 0,
    g: rgba[offset + 1] ?? 0,
    b: rgba[offset + 2] ?? 0,
  };
}

function addText(cells: WebCell[], x: number, y: number, text: string, fg: WebColor, bg: WebColor | null, bold: boolean): void {
  for (let index = 0; index < text.length; index += 1) {
    cells.push(cell(x + index, y, text[index] ?? " ", fg, bg, bold));
  }
}

function cell(x: number, y: number, text: string, fg: WebColor | null, bg: WebColor | null, bold: boolean): WebCell {
  return {
    x,
    y,
    text,
    fg,
    bg,
    osc8: null,
    style: {
      bold,
      italic: false,
      faint: false,
      blink: false,
      inverse: false,
      invisible: false,
      strikethrough: false,
      overline: false,
      underline: false,
    },
  };
}

function assetLabel(assets: DoomAssetUrls | ReturnType<typeof embeddedDoomAssets>): string {
  if ("glueCode" in assets) {
    return "embedded badlogic/pi-doom assets";
  }
  if (assets.wadUrl === DEFAULT_ASSETS.wadUrl) {
    return "badlogic/pi-doom default WAD";
  }
  return assets.wadUrl;
}

function green(): WebColor {
  return { r: 158, g: 206, b: 106 };
}

function cyan(): WebColor {
  return { r: 125, g: 207, b: 255 };
}

function yellow(): WebColor {
  return { r: 224, g: 175, b: 104 };
}

function gray(): WebColor {
  return { r: 169, g: 177, b: 214 };
}

function darkGray(): WebColor {
  return { r: 86, g: 95, b: 137 };
}
