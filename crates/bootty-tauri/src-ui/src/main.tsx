import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { createTerminalBackend } from "./terminal-api";
import type { TerminalBackend, TerminalKey } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";
import { type TerminalSelection, WebGlTerminalRenderer } from "./webgl-terminal";
import "./style.css";

type GridSize = { cols: number; rows: number };
type LinkOverlay = { key: string; href: string; left: number; top: number; width: number; height: number };

declare global {
  interface Window {
    __boottyRoot?: Root;
  }
}

function App() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<WebGlTerminalRenderer | null>(null);
  const frameRef = useRef<WebTerminalFrame | null>(null);
  const gridRef = useRef<GridSize | null>(null);
  const backendRef = useRef<TerminalBackend | null>(null);
  const selectionRef = useRef<TerminalSelection | null>(null);
  const selectingRef = useRef(false);
  const linkOverlaySignatureRef = useRef("");
  const resizeInFlightRef = useRef(false);
  const fpsRef = useRef({ frames: 0, startedAt: performance.now() });
  const [linkOverlays, setLinkOverlays] = useState<LinkOverlay[]>([]);

  const renderFrame = useCallback((frame: WebTerminalFrame) => {
    frameRef.current = frame;
    rendererRef.current?.render(frame, selectionRef.current);
    const canvas = canvasRef.current;
    const overlays = canvas ? linkOverlaysForFrame(frame, canvas) : [];
    const signature = JSON.stringify(overlays);
    if (signature !== linkOverlaySignatureRef.current) {
      linkOverlaySignatureRef.current = signature;
      setLinkOverlays(overlays);
    }
  }, []);

  const resizeToCanvas = useCallback(async (backend: TerminalBackend, frame: WebTerminalFrame) => {
    const canvas = canvasRef.current;
    if (!canvas || resizeInFlightRef.current) {
      return frame;
    }

    const nextGrid = gridForCanvas(canvas, frame);
    const currentGrid = gridRef.current;
    if (currentGrid?.cols === nextGrid.cols && currentGrid.rows === nextGrid.rows) {
      return frame;
    }

    resizeInFlightRef.current = true;
    try {
      const resized = await backend.resize({
        cols: nextGrid.cols,
        rows: nextGrid.rows,
        cellWidth: frame.cellWidth,
        cellHeight: frame.cellHeight,
        devicePixelRatio: window.devicePixelRatio || 1,
      });
      gridRef.current = nextGrid;
      frameRef.current = resized;
      return resized;
    } finally {
      resizeInFlightRef.current = false;
    }
  }, []);

  const draw = useCallback(async () => {
    const backend = backendRef.current;
    const renderer = rendererRef.current;
    if (!backend || !renderer) {
      return;
    }

    let frame = await resizeToCanvas(backend, await backend.readFrame());
    frame = await publishFps(backend, frame, fpsRef.current);
    renderFrame(frame);
  }, [renderFrame, resizeToCanvas]);

  const sendInput = useCallback(
    (input: string) => {
      const backend = backendRef.current;
      if (!backend || input.length === 0) {
        return;
      }
      backend.write(input).then(draw).catch(reportError);
    },
    [draw],
  );

  const sendKey = useCallback(
    (event: KeyboardEvent, kind: TerminalKey["kind"]) => {
      const backend = backendRef.current;
      const frame = frameRef.current;
      if (!backend?.key || !frame || !backend.wantsKey?.(keyboardEvent(event, kind), frame)) {
        return false;
      }
      event.preventDefault();
      backend
        .key(keyboardEvent(event, kind))
        .then((nextFrame) => {
          if (!nextFrame) {
            return;
          }
          renderFrame(nextFrame);
        })
        .catch(reportError);
      return true;
    },
    [renderFrame],
  );

  const sendMouse = useCallback(
    (kind: "move" | "down" | "up" | "leave", event: React.PointerEvent<HTMLCanvasElement>) => {
      const backend = backendRef.current;
      const frame = frameRef.current;
      const canvas = canvasRef.current;
      if (!frame || !canvas) {
        return;
      }
      if (kind === "leave") {
        canvas.style.cursor = "default";
      }
      const { x, y } = cellForPoint(frame, canvas, event.clientX, event.clientY);
      const eguiLink = eguiLinkAt(frame, canvas, event.clientX, event.clientY);
      const osc8 = osc8At(frame, x, y);
      if (kind !== "leave") {
        canvas.style.cursor = eguiLink || osc8 ? "pointer" : "default";
      }
      if (kind === "down" && eguiLink) {
        selectionRef.current = null;
        selectingRef.current = false;
        return;
      }
      if (kind === "down" && osc8) {
        selectionRef.current = null;
        selectingRef.current = false;
        return;
      }
      if (kind === "down") {
        selectionRef.current = { anchor: { x, y }, focus: { x, y } };
        selectingRef.current = true;
        renderFrame(frame);
      } else if (kind === "move" && selectingRef.current && selectionRef.current) {
        selectionRef.current = { ...selectionRef.current, focus: { x, y } };
        renderFrame(frame);
        return;
      } else if (kind === "up" && selectingRef.current) {
        selectingRef.current = false;
        if (selectionRef.current?.anchor.x === x && selectionRef.current.anchor.y === y) {
          selectionRef.current = null;
        } else if (selectionRef.current) {
          selectionRef.current = { ...selectionRef.current, focus: { x, y } };
        }
        renderFrame(frame);
      }
      if (!backend?.mouse) {
        return;
      }
      backend
        .mouse({ kind, x, y, button: event.button })
        .then((nextFrame) => {
          renderFrame(nextFrame);
        })
        .catch(reportError);
    },
    [renderFrame],
  );

  const sendWheel = useCallback(
    (event: React.WheelEvent<HTMLCanvasElement>) => {
      const backend = backendRef.current;
      const frame = frameRef.current;
      const canvas = canvasRef.current;
      if (!backend?.mouse || !frame || !canvas) {
        return;
      }
      event.preventDefault();
      const { x, y } = cellForPoint(frame, canvas, event.clientX, event.clientY);
      const direction = event.deltaY < 0 ? -1 : 1;
      const rows = Math.max(1, Math.min(8, Math.ceil(Math.abs(event.deltaY) / frame.cellHeight)));
      backend
        .mouse({ kind: "wheel", x, y, button: direction * rows })
        .then((nextFrame) => {
          renderFrame(nextFrame);
        })
        .catch(reportError);
    },
    [renderFrame],
  );

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }

    const renderer = new WebGlTerminalRenderer(canvas);
    rendererRef.current = renderer;
    let stop = false;
    let animationFrame = 0;

    async function drawLoop() {
      if (stop) {
        return;
      }
      try {
        await draw();
      } catch (error) {
        reportError(error);
      }
      animationFrame = window.requestAnimationFrame(drawLoop);
    }

    async function start() {
      await loadTerminalFont();
      const backend = await createTerminalBackend();
      backendRef.current = backend;
      const frame = await resizeToCanvas(backend, await backend.start());
      if (stop) {
        return;
      }
      renderFrame(frame);
      animationFrame = window.requestAnimationFrame(drawLoop);
    }

    start().catch(reportError);

    return () => {
      stop = true;
      window.cancelAnimationFrame(animationFrame);
      renderer.dispose();
      backendRef.current = null;
      rendererRef.current = null;
    };
  }, [draw, renderFrame, resizeToCanvas]);

  useEffect(() => {
    const onResize = () => {
      gridRef.current = null;
      selectionRef.current = null;
      selectingRef.current = false;
      draw().catch(reportError);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (sendKey(event, "down")) {
        return;
      }
      const encoded = encodeKey(event);
      if (encoded == null) {
        return;
      }
      event.preventDefault();
      sendInput(encoded);
    };
    const onKeyUp = (event: KeyboardEvent) => {
      sendKey(event, "up");
    };
    const onPaste = (event: ClipboardEvent) => {
      event.preventDefault();
      sendInput(event.clipboardData?.getData("text") ?? "");
    };
    const onCopy = (event: ClipboardEvent) => {
      const frame = frameRef.current;
      const selection = selectionRef.current;
      if (!frame || !selection) {
        return;
      }
      const text = selectedText(frame, selection);
      if (text.length === 0) {
        return;
      }
      event.preventDefault();
      event.clipboardData?.setData("text/plain", text);
    };

    window.addEventListener("resize", onResize);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("paste", onPaste);
    window.addEventListener("copy", onCopy);
    const observer = new ResizeObserver(onResize);
    if (canvasRef.current?.parentElement) {
      observer.observe(canvasRef.current.parentElement);
    }
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("paste", onPaste);
      window.removeEventListener("copy", onCopy);
      observer.disconnect();
    };
  }, [draw, sendInput, sendKey]);

  return (
    <main className="terminal-site" aria-label="Bootty terminal website">
      <canvas
        ref={canvasRef}
        className="terminal-site-canvas"
        tabIndex={0}
        aria-label="Interactive Bootty terminal website"
        onPointerDown={(event) => {
          event.currentTarget.focus();
          event.currentTarget.setPointerCapture(event.pointerId);
          sendMouse("down", event);
        }}
        onPointerMove={(event) => sendMouse("move", event)}
        onPointerUp={(event) => {
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
          sendMouse("up", event);
        }}
        onPointerLeave={(event) => sendMouse("leave", event)}
        onWheel={sendWheel}
      />
      {linkOverlays.map((link) => (
        <a
          key={link.key}
          className="terminal-site-link"
          href={link.href}
          target="_blank"
          rel="noopener noreferrer"
          style={{
            left: link.left,
            top: link.top,
            width: link.width,
            height: link.height,
          }}
          aria-label={link.href}
        />
      ))}
    </main>
  );
}

function keyboardEvent(event: KeyboardEvent, kind: TerminalKey["kind"]): TerminalKey {
  return {
    kind,
    key: event.key,
    code: event.code,
    ctrlKey: event.ctrlKey,
    shiftKey: event.shiftKey,
    altKey: event.altKey,
    metaKey: event.metaKey,
    repeat: event.repeat,
  };
}

function osc8At(frame: WebTerminalFrame, x: number, y: number): string | null {
  return frame.cells.find((cell) => cell.x === x && cell.y === y)?.osc8 ?? null;
}

function eguiLinkAt(
  frame: WebTerminalFrame,
  canvas: HTMLCanvasElement,
  clientX: number,
  clientY: number,
): string | null {
  const point = canvasPoint(canvas, clientX, clientY);
  return (
    frame.egui?.links.find(
      (link) =>
        point.x >= link.rect.minX &&
        point.x < link.rect.maxX &&
        point.y >= link.rect.minY &&
        point.y < link.rect.maxY,
    )?.url ?? null
  );
}

function cellForPoint(
  frame: WebTerminalFrame,
  canvas: HTMLCanvasElement,
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  const point = canvasPoint(canvas, clientX, clientY);
  return {
    x: Math.max(0, Math.min(frame.cols - 1, Math.floor(point.x / frame.cellWidth))),
    y: Math.max(0, Math.min(frame.rows - 1, Math.floor(point.y / frame.cellHeight))),
  };
}

function canvasPoint(canvas: HTMLCanvasElement, clientX: number, clientY: number): { x: number; y: number } {
  const rect = canvas.getBoundingClientRect();
  return { x: clientX - rect.left, y: clientY - rect.top };
}

function linkOverlaysForFrame(frame: WebTerminalFrame, canvas: HTMLCanvasElement): LinkOverlay[] {
  const { scaleX, scaleY } = frameScale(frame, canvas);
  const overlays: LinkOverlay[] = [];
  for (const [index, link] of (frame.egui?.links ?? []).entries()) {
    overlays.push({
      key: `egui-${index}-${link.url}`,
      href: link.url,
      left: link.rect.minX * scaleX,
      top: link.rect.minY * scaleY,
      width: (link.rect.maxX - link.rect.minX) * scaleX,
      height: (link.rect.maxY - link.rect.minY) * scaleY,
    });
  }
  for (const cell of frame.cells) {
    if (!cell.osc8) {
      continue;
    }
    overlays.push({
      key: `osc8-${cell.x}-${cell.y}-${cell.osc8}`,
      href: cell.osc8,
      left: cell.x * frame.cellWidth * scaleX,
      top: cell.y * frame.cellHeight * scaleY,
      width: frame.cellWidth * scaleX,
      height: frame.cellHeight * scaleY,
    });
  }
  return overlays;
}

function frameScale(frame: WebTerminalFrame, canvas: HTMLCanvasElement): { scaleX: number; scaleY: number } {
  const rect = canvas.getBoundingClientRect();
  return {
    scaleX: rect.width / (frame.cols * frame.cellWidth),
    scaleY: rect.height / (frame.rows * frame.cellHeight),
  };
}

function selectedText(frame: WebTerminalFrame, selection: TerminalSelection): string {
  const start = orderedSelectionStart(selection);
  const end = orderedSelectionEnd(selection);
  const cells = new Map(frame.cells.map((cell) => [`${cell.x}:${cell.y}`, cell.text]));
  const lines: string[] = [];
  for (let y = start.y; y <= end.y; y += 1) {
    const rowBounds = visibleRowBounds(frame, y);
    if (!rowBounds) {
      continue;
    }
    const startX = Math.max(y === start.y ? start.x : 0, rowBounds.startX);
    const endX = Math.min(y === end.y ? end.x : frame.cols - 1, rowBounds.endX);
    if (endX < startX) {
      continue;
    }
    let line = "";
    for (let x = startX; x <= endX; x += 1) {
      line += cells.get(`${x}:${y}`) ?? " ";
    }
    lines.push(line.replace(/\s+$/u, ""));
  }
  return lines.join("\n");
}

function visibleRowBounds(frame: WebTerminalFrame, y: number): { startX: number; endX: number } | null {
  let startX = Number.POSITIVE_INFINITY;
  let endX = Number.NEGATIVE_INFINITY;
  for (const cell of frame.cells) {
    if (cell.y !== y || cell.style.invisible || !isSelectableText(cell.text)) {
      continue;
    }
    startX = Math.min(startX, cell.x);
    endX = Math.max(endX, cell.x);
  }
  if (!Number.isFinite(startX) || !Number.isFinite(endX)) {
    return null;
  }
  return { startX, endX };
}

const BOX_DRAWING_TEXT = new Set(["─", "━", "│", "┃", "┌", "┐", "└", "┘", "╭", "╮", "╰", "╯"]);

function isSelectableText(text: string): boolean {
  return text.trim().length > 0 && !BOX_DRAWING_TEXT.has(text);
}

function orderedSelectionStart(selection: TerminalSelection): { x: number; y: number } {
  if (
    selection.anchor.y < selection.focus.y ||
    (selection.anchor.y === selection.focus.y && selection.anchor.x <= selection.focus.x)
  ) {
    return selection.anchor;
  }
  return selection.focus;
}

function orderedSelectionEnd(selection: TerminalSelection): { x: number; y: number } {
  if (
    selection.anchor.y < selection.focus.y ||
    (selection.anchor.y === selection.focus.y && selection.anchor.x <= selection.focus.x)
  ) {
    return selection.focus;
  }
  return selection.anchor;
}

function gridForCanvas(canvas: HTMLCanvasElement, frame: WebTerminalFrame): GridSize {
  const container = canvas.parentElement;
  const width = container?.clientWidth || window.innerWidth;
  const height = container?.clientHeight || window.innerHeight;
  return {
    cols: Math.max(40, Math.floor(width / frame.cellWidth)),
    rows: Math.max(18, Math.floor(height / frame.cellHeight)),
  };
}
async function publishFps(
  backend: TerminalBackend,
  frame: WebTerminalFrame,
  fps: { frames: number; startedAt: number },
): Promise<WebTerminalFrame> {
  fps.frames += 1;
  const now = performance.now();
  const elapsed = now - fps.startedAt;
  if (elapsed < 1000 || !backend.fps) {
    return frame;
  }
  const nextFrame = await backend.fps((fps.frames * 1000) / elapsed);
  fps.frames = 0;
  fps.startedAt = now;
  return nextFrame;
}


function encodeKey(event: KeyboardEvent): string | null {
  if (event.metaKey) {
    return null;
  }
  if (event.ctrlKey && event.key.length === 1) {
    return String.fromCharCode(event.key.toUpperCase().charCodeAt(0) - 64);
  }
  switch (event.key) {
    case "Enter":
      return "\r";
    case "Backspace":
      return "\x7f";
    case "Tab":
      return "\t";
    case "Escape":
      return "\x1b";
    case "ArrowUp":
      return "\x1b[A";
    case "ArrowDown":
      return "\x1b[B";
    case "ArrowRight":
      return "\x1b[C";
    case "ArrowLeft":
      return "\x1b[D";
    case "PageUp":
      return "\x1b[5~";
    case "PageDown":
      return "\x1b[6~";
    case "Home":
      return "\x1b[H";
    case "End":
      return "\x1b[F";
    default:
      return event.key.length === 1 ? event.key : null;
  }
}

async function loadTerminalFont(): Promise<void> {
  await document.fonts.load('18px "Maple Mono NF"');
  await document.fonts.ready;
}

function reportError(error: unknown): void {
  console.error(error);
}

const rootElement = required(document.getElementById("root"), "find root element");
window.__boottyRoot ??= createRoot(rootElement);
window.__boottyRoot.render(<App />);

function required<T>(value: T | null, action: string): T {
  if (value == null) {
    throw new Error(`Failed to ${action}`);
  }
  return value;
}
