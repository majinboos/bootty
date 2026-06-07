import { BrowserDoomEngine } from "./doom-engine";
import { embeddedDoomAssets } from "./doom-embedded-assets";
import { mapBrowserKeyToDoom } from "./doom-keys";
import type { TerminalKey } from "./terminal-api";
import type { WebCell, WebColor, WebImage, WebRect, WebTerminalFrame } from "./terminal-types";

const DOOM_TICK_MS = 1000 / 35;

export class DoomTabOverlay {
  private readonly engine: BrowserDoomEngine;
  private initPromise: Promise<void> | null = null;
  private ready = false;
  private status = "loading doomgeneric wasm";
  private lastTickAt = performance.now();
  private tickRemainder = 0;

  constructor() {
    this.engine = new BrowserDoomEngine(embeddedDoomAssets(), (status) => {
      this.status = status;
    });
  }

  render(frame: WebTerminalFrame): WebTerminalFrame {
    this.ensureInit();
    if (this.ready) {
      this.tickToNow();
    }
    document.documentElement.dataset.boottyDoomStatus = this.status;

    const panel = detailPanel(frame);
    const inner = inset(panel, frame.cellWidth, frame.cellHeight);
    const cells = frame.cells.filter((cell) => !insideCellRect(cell, panel.contentCells));
    addText(cells, panel.contentCells.minX, panel.contentCells.minY, `DOOM: ${this.status}`, red(), null, true);
    addText(
      cells,
      panel.contentCells.minX,
      panel.contentCells.maxY - 1,
      "WASD/arrows move  F/Ctrl fire  Space use  1-7 weapons",
      gray(),
      null,
      false,
    );

    const images = this.ready ? [...frame.images, this.doomImage(inner)] : frame.images;
    return { ...frame, cells, images };
  }

  async key(event: TerminalKey, frame: WebTerminalFrame): Promise<WebTerminalFrame | null> {
    if (!isDoomTab(frame) || !isDetailFocused(frame) || event.metaKey || event.repeat) {
      return null;
    }
    if (!this.ready) {
      return this.render(frame);
    }
    for (const key of mapBrowserKeyToDoom(event)) {
      this.engine.pushKey(event.kind === "down", key);
    }
    return this.render(frame);
  }

  private ensureInit(): void {
    this.initPromise ??= this.engine
      .init()
      .then(() => {
        this.ready = true;
        this.status = "running as Bootty image layer";
        this.lastTickAt = performance.now();
      })
      .catch((error: unknown) => {
        this.status = error instanceof Error ? error.message : String(error);
      });
  }

  private tickToNow(): void {
    const now = performance.now();
    const elapsed = now - this.lastTickAt + this.tickRemainder;
    const ticks = Math.min(5, Math.floor(elapsed / DOOM_TICK_MS));
    for (let index = 0; index < ticks; index += 1) {
      this.engine.tick();
    }
    this.tickRemainder = elapsed - ticks * DOOM_TICK_MS;
    this.lastTickAt = now;
  }

  private doomImage(destination: WebRect): WebImage {
    const rgba = this.engine.getFrameRGBA();
    const width = this.engine.width;
    const height = this.engine.height;
    const availableWidth = destination.maxX - destination.minX;
    const availableHeight = destination.maxY - destination.minY;
    const scale = Math.min(availableWidth / width, availableHeight / height);
    const gameWidth = Math.floor(width * scale);
    const gameHeight = Math.floor(height * scale);
    const left = destination.minX + Math.floor((availableWidth - gameWidth) / 2);
    const top = destination.minY + Math.floor((availableHeight - gameHeight) / 2);

    return {
      key: "doom-tab-frame",
      layer: "belowText",
      imageWidth: width,
      imageHeight: height,
      source: { minX: 0, minY: 0, maxX: width, maxY: height },
      destination: { minX: left, minY: top, maxX: left + gameWidth, maxY: top + gameHeight },
      rgba,
    };
  }
}

export function isDoomTab(frame: WebTerminalFrame): boolean {
  return frameText(frame).includes("Doom runs inside this terminal frame.");
}

function isDetailFocused(frame: WebTerminalFrame): boolean {
  return frameText(frame).includes("Detail:");
}

function detailPanel(frame: WebTerminalFrame): { pixels: WebRect; contentCells: WebRect } {
  const bodyRows = Math.max(8, frame.rows - 7);
  const vertical = frame.cols < 78;
  const detailX = vertical ? 1 : 29;
  const detailY = vertical ? 13 : 4;
  const detailCols = vertical ? frame.cols - 2 : frame.cols - 30;
  const detailRows = vertical ? bodyRows - 9 : bodyRows;
  const contentCells = {
    minX: detailX + 1,
    minY: detailY + 2,
    maxX: detailX + Math.max(1, detailCols - 1),
    maxY: detailY + Math.max(3, detailRows - 1),
  };
  return {
    pixels: {
      minX: detailX * frame.cellWidth,
      minY: detailY * frame.cellHeight,
      maxX: (detailX + detailCols) * frame.cellWidth,
      maxY: (detailY + detailRows) * frame.cellHeight,
    },
    contentCells,
  };
}

function inset(rect: { pixels: WebRect }, x: number, y: number): WebRect {
  return {
    minX: rect.pixels.minX + x,
    minY: rect.pixels.minY + y * 2,
    maxX: rect.pixels.maxX - x,
    maxY: rect.pixels.maxY - y * 2,
  };
}

function insideCellRect(cell: WebCell, rect: WebRect): boolean {
  return cell.x >= rect.minX && cell.x < rect.maxX && cell.y >= rect.minY && cell.y < rect.maxY;
}

function frameText(frame: WebTerminalFrame): string {
  return frame.cells.map((cell) => cell.text).join("");
}

function addText(cells: WebCell[], x: number, y: number, text: string, fg: WebColor, bg: WebColor | null, bold: boolean): void {
  for (let index = 0; index < text.length; index += 1) {
    cells.push({
      x: x + index,
      y,
      text: text[index] ?? " ",
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
    });
  }
}

function red(): WebColor {
  return { r: 247, g: 118, b: 142 };
}

function gray(): WebColor {
  return { r: 169, g: 177, b: 214 };
}
