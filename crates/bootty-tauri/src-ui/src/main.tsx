import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { createTerminalBackend } from "./terminal-api";
import type { TerminalBackend } from "./terminal-api";
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
  const [status, setStatus] = useState("starting terminal");
  const fpsRef = useRef({ frames: 0, startedAt: performance.now() });
  const [fps, setFps] = useState(0);
  const [inputCount, setInputCount] = useState(0);

  const focusTerminal = useCallback(() => {
    window.requestAnimationFrame(() => canvasRef.current?.focus());
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
      });
      gridRef.current = nextGrid;
      frameRef.current = resized;
      return resized;
    } finally {
      resizeInFlightRef.current = false;
    }
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }

    const renderer = new WebGlTerminalRenderer(canvas);
    rendererRef.current = renderer;
    let stop = false;
    let timeout = 0;

    async function drawNextFrame() {
      try {
        const backend = backendRef.current;
        if (!backend) {
          return;
        }
        let frame = await backend.readFrame();
        if (stop) {
          return;
        }
        frame = await resizeToCanvas(backend, frame);
        if (stop) {
          return;
        }
        frameRef.current = frame;
        renderer.render(frame);
        recordFrame(fpsRef, setFps);
      } catch (error) {
        if (!stop) {
          setStatus(String(error));
        }
      }

      if (!stop) {
        timeout = window.setTimeout(drawNextFrame, 33);
      }
    }

    async function start() {
      const backend = await createTerminalBackend();
      backendRef.current = backend;
      let frame = await backend.start();
      if (stop) {
        return;
      }
      frame = await resizeToCanvas(backend, frame);
      if (stop) {
        return;
      }
      frameRef.current = frame;
      renderer.render(frame);
      recordFrame(fpsRef, setFps);
      setStatus(backend.label);
      focusTerminal();
      timeout = window.setTimeout(drawNextFrame, 33);
    }

    start().catch((error) => setStatus(String(error)));

    return () => {
      stop = true;
      window.clearTimeout(timeout);
      renderer.dispose();
      backendRef.current = null;
      rendererRef.current = null;
    };
  }, [focusTerminal, resizeToCanvas]);

  useEffect(() => {
    const onResize = async () => {
      const frame = frameRef.current;
      const renderer = rendererRef.current;
      const backend = backendRef.current;
      if (!frame || !renderer || !backend) {
        return;
      }
      try {
        const resized = await resizeToCanvas(backend, frame);
        frameRef.current = resized;
        renderer.render(resized);
      } catch (error) {
        setStatus(String(error));
      }
    };

    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [resizeToCanvas]);

  const sendInput = useCallback((input: string) => {
    if (input.length === 0) {
      return;
    }
    const backend = backendRef.current;
    if (!backend) {
      return;
    }
    backend
      .write(input)
      .then(() => setInputCount((count) => count + input.length))
      .catch((error) => setStatus(String(error)));
  }, []);

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLCanvasElement>) => {
      const encoded = encodeKey(event);
      if (encoded == null) {
        return;
      }
      event.preventDefault();
      sendInput(encoded);
    },
    [sendInput],
  );

  return (
    <main>
      <header>
        <strong>Bootty Web Terminal</strong>
        <span>{status}</span>
        <span>fps: {fps.toFixed(1)}</span>
        <span>input: {inputCount}</span>
      </header>
      <canvas
        ref={canvasRef}
        className="terminal"
        tabIndex={0}
        onMouseDown={focusTerminal}
        onKeyDown={onKeyDown}
        onPaste={(event) => {
          event.preventDefault();
          sendInput(event.clipboardData.getData("text"));
        }}
      />
    </main>
  );
}

function recordFrame(
  fpsRef: React.MutableRefObject<{ frames: number; startedAt: number }>,
  setFps: React.Dispatch<React.SetStateAction<number>>,
): void {
  const now = performance.now();
  const sample = fpsRef.current;
  sample.frames += 1;
  const elapsed = now - sample.startedAt;
  if (elapsed >= 1000) {
    setFps((sample.frames * 1000) / elapsed);
    fpsRef.current = { frames: 0, startedAt: now };
  }
}
function gridForCanvas(canvas: HTMLCanvasElement, frame: WebTerminalFrame): GridSize {
  return {
    cols: Math.max(1, Math.floor(canvas.clientWidth / frame.cellWidth)),
    rows: Math.max(1, Math.floor(canvas.clientHeight / frame.cellHeight)),
  };
}

function encodeKey(event: React.KeyboardEvent): string | null {
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