import React, { useCallback, useEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { createTerminalBackend } from "./terminal-api";
import type { TerminalBackend, TerminalKey } from "./terminal-api";
import type { WebTerminalFrame } from "./terminal-types";
import { WebGlTerminalRenderer } from "./webgl-terminal";
import "./style.css";

type GridSize = { cols: number; rows: number };

function App() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<WebGlTerminalRenderer | null>(null);
  const frameRef = useRef<WebTerminalFrame | null>(null);
  const gridRef = useRef<GridSize | null>(null);
  const backendRef = useRef<TerminalBackend | null>(null);
  const resizeInFlightRef = useRef(false);
  const fpsRef = useRef({ frames: 0, startedAt: performance.now() });

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
    frameRef.current = frame;
    renderer.render(frame);
  }, [resizeToCanvas]);

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
          frameRef.current = nextFrame;
          rendererRef.current?.render(nextFrame);
        })
        .catch(reportError);
      return true;
    },
    [],
  );

  const sendMouse = useCallback(
    (kind: "move" | "down" | "up" | "leave", event: React.PointerEvent<HTMLCanvasElement>) => {
      const backend = backendRef.current;
      const renderer = rendererRef.current;
      const frame = frameRef.current;
      const canvas = canvasRef.current;
      if (!frame || !canvas) {
        return;
      }
      if (kind === "leave") {
        canvas.style.cursor = "default";
      }
      const rect = canvas.getBoundingClientRect();
      const x = Math.max(0, Math.min(frame.cols - 1, Math.floor((event.clientX - rect.left) / frame.cellWidth)));
      const y = Math.max(0, Math.min(frame.rows - 1, Math.floor((event.clientY - rect.top) / frame.cellHeight)));
      const osc8 = osc8At(frame, x, y);
      if (kind !== "leave") {
        canvas.style.cursor = osc8 ? "pointer" : "default";
      }
      if (kind === "down" && osc8) {
        window.open(osc8, "_blank", "noopener,noreferrer");
        return;
      }
      if (!backend?.mouse || !renderer) {
        return;
      }
      backend
        .mouse({ kind, x, y, button: event.button })
        .then((nextFrame) => {
          frameRef.current = nextFrame;
          renderer.render(nextFrame);
        })
        .catch(reportError);
    },
    [],
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
      frameRef.current = frame;
      renderer.render(frame);
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
  }, [draw, resizeToCanvas]);

  useEffect(() => {
    const onResize = () => {
      gridRef.current = null;
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

    window.addEventListener("resize", onResize);
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("paste", onPaste);
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("paste", onPaste);
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
      />
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

createRoot(required(document.getElementById("root"), "find root element")).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

function required<T>(value: T | null, action: string): T {
  if (value == null) {
    throw new Error(`Failed to ${action}`);
  }
  return value;
}
