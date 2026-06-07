import type { TerminalBackend, TerminalResize } from "./terminal-api";
import type { WebCell, WebColor, WebTerminalFrame } from "./terminal-types";

const CELL_WIDTH = 9;
const CELL_HEIGHT = 18;
const PROMPT = "guest@bootty:~$ ";
const BACKGROUND: WebColor = { r: 17, g: 18, b: 26 };
const FOREGROUND: WebColor = { r: 192, g: 202, b: 245 };
const MUTED: WebColor = { r: 169, g: 177, b: 214 };
const GREEN: WebColor = { r: 158, g: 206, b: 106 };
const BLUE: WebColor = { r: 122, g: 162, b: 247 };
const RED: WebColor = { r: 247, g: 118, b: 142 };
const YELLOW: WebColor = { r: 224, g: 175, b: 104 };

type Line = { text: string; fg?: WebColor; bg?: WebColor; inverse?: boolean };
type Cursor = { x: number; y: number; color: WebColor | null };
type InputKey = string;

type EditorState = {
  file: string;
  lines: string[];
  x: number;
  y: number;
  mode: "normal" | "insert" | "command";
  command: string;
  dirty: boolean;
  message: string;
};

export function createFakeShellBackend(): TerminalBackend {
  const shell = new FakeShell();
  return {
    label: "static demo shell: try `vim demo.txt`",
    start: () => shell.start(),
    readFrame: () => shell.frame(),
    resize: (request) => shell.resize(request),
    write: (input) => shell.write(input),
  };
}

class FakeShell {
  private cols = 80;
  private rows = 24;
  private input = "";
  private editor: EditorState | null = null;
  private readonly scrollback: Line[] = [];
  private readonly files = new Map<string, string>([
    [
      "README.md",
      [
        "# Bootty static demo",
        "",
        "This GitHub Pages build uses the same WebGL renderer as the Tauri app.",
        "The backend is a small in-browser shell, so it works on static hosting.",
        "",
        "Try:",
        "  ls",
        "  cat README.md",
        "  vim demo.txt",
      ].join("\n"),
    ],
    [
      "demo.txt",
      [
        "Bootty can render a terminal frame without a native host.",
        "",
        "This fake vim supports:",
        "- i / a to enter insert mode",
        "- Esc to return to normal mode",
        "- arrow keys or hjkl movement",
        "- x to delete a character",
        "- :w, :q, :wq, :q!",
      ].join("\n"),
    ],
    ["package.json", '{ "scripts": { "build:pages": "vite build --mode github-pages" } }'],
  ]);

  async start(): Promise<WebTerminalFrame> {
    if (this.scrollback.length === 0) {
      this.scrollback.push(
        { text: "Bootty static web demo", fg: BLUE },
        { text: "A browser-only fake shell is driving the terminal backend.", fg: MUTED },
        { text: "Try: help, ls, cat README.md, vim demo.txt, clear", fg: MUTED },
        { text: "" },
      );
    }
    return this.frame();
  }

  async resize(request: TerminalResize): Promise<WebTerminalFrame> {
    this.cols = Math.max(1, request.cols);
    this.rows = Math.max(1, request.rows);
    return this.frame();
  }

  async write(input: string): Promise<void> {
    for (const key of parseInput(input)) {
      if (this.editor) {
        this.acceptEditor(key);
      } else {
        this.acceptShell(key);
      }
    }
  }

  async frame(): Promise<WebTerminalFrame> {
    const lines = this.editor ? this.editorLines() : this.shellLines();
    const cells: WebCell[] = [];

    for (let y = 0; y < lines.length; y += 1) {
      const line = lines[y];
      for (let x = 0; x < line.text.length && x < this.cols; x += 1) {
        cells.push(cell(x, y, line.text[x], line));
      }
    }

    return {
      cols: this.cols,
      rows: this.rows,
      cellWidth: CELL_WIDTH,
      cellHeight: CELL_HEIGHT,
      colors: { background: BACKGROUND, foreground: FOREGROUND, cursor: FOREGROUND },
      cursor: this.editor ? this.editorCursor() : cursorFor(lines, this.cols),
      cells,
      images: [],
    };
  }

  private acceptShell(key: InputKey): void {
    switch (key) {
      case "Enter":
        this.submit();
        return;
      case "Backspace":
        this.input = this.input.slice(0, -1);
        return;
      case "Tab":
        this.input += "  ";
        return;
      case "Escape":
      case "ArrowUp":
      case "ArrowDown":
      case "ArrowLeft":
      case "ArrowRight":
        return;
      default:
        if (key >= " ") {
          this.input += key;
        }
    }
  }

  private acceptEditor(key: InputKey): void {
    const editor = this.editor;
    if (!editor) {
      return;
    }

    if (editor.mode === "command") {
      this.acceptEditorCommand(editor, key);
      return;
    }

    if (editor.mode === "insert") {
      this.acceptEditorInsert(editor, key);
      return;
    }

    this.acceptEditorNormal(editor, key);
  }

  private acceptEditorCommand(editor: EditorState, key: InputKey): void {
    if (key === "Escape") {
      editor.mode = "normal";
      editor.command = "";
      editor.message = "";
      return;
    }
    if (key === "Backspace") {
      editor.command = editor.command.slice(0, -1);
      return;
    }
    if (key === "Enter") {
      this.runEditorCommand(editor.command.trim());
      return;
    }
    if (key.length === 1 && key >= " ") {
      editor.command += key;
    }
  }

  private acceptEditorInsert(editor: EditorState, key: InputKey): void {
    if (key === "Escape") {
      editor.mode = "normal";
      editor.x = Math.max(0, editor.x - 1);
      return;
    }
    if (key === "Enter") {
      const line = editor.lines[editor.y] ?? "";
      editor.lines.splice(editor.y, 1, line.slice(0, editor.x), line.slice(editor.x));
      editor.y += 1;
      editor.x = 0;
      editor.dirty = true;
      return;
    }
    if (key === "Backspace") {
      this.backspaceEditor(editor);
      return;
    }
    if (moveEditor(editor, key)) {
      return;
    }
    if (key.length === 1 && key >= " ") {
      const line = editor.lines[editor.y] ?? "";
      editor.lines[editor.y] = line.slice(0, editor.x) + key + line.slice(editor.x);
      editor.x += 1;
      editor.dirty = true;
    }
  }

  private acceptEditorNormal(editor: EditorState, key: InputKey): void {
    if (moveEditor(editor, key)) {
      return;
    }
    switch (key) {
      case "i":
        editor.mode = "insert";
        editor.message = "-- INSERT --";
        return;
      case "a":
        editor.mode = "insert";
        editor.x = Math.min((editor.lines[editor.y] ?? "").length, editor.x + 1);
        editor.message = "-- INSERT --";
        return;
      case ":":
        editor.mode = "command";
        editor.command = "";
        editor.message = "";
        return;
      case "x":
        this.deleteEditorChar(editor);
        return;
      default:
        return;
    }
  }

  private backspaceEditor(editor: EditorState): void {
    if (editor.x > 0) {
      const line = editor.lines[editor.y] ?? "";
      editor.lines[editor.y] = line.slice(0, editor.x - 1) + line.slice(editor.x);
      editor.x -= 1;
      editor.dirty = true;
      return;
    }
    if (editor.y > 0) {
      const previous = editor.lines[editor.y - 1] ?? "";
      const current = editor.lines[editor.y] ?? "";
      editor.lines.splice(editor.y - 1, 2, previous + current);
      editor.y -= 1;
      editor.x = previous.length;
      editor.dirty = true;
    }
  }

  private deleteEditorChar(editor: EditorState): void {
    const line = editor.lines[editor.y] ?? "";
    if (line.length === 0) {
      return;
    }
    editor.lines[editor.y] = line.slice(0, editor.x) + line.slice(editor.x + 1);
    editor.x = Math.min(editor.x, Math.max(0, editor.lines[editor.y].length - 1));
    editor.dirty = true;
  }

  private runEditorCommand(command: string): void {
    const editor = this.editor;
    if (!editor) {
      return;
    }
    switch (command) {
      case "w":
        this.files.set(editor.file, editor.lines.join("\n"));
        editor.dirty = false;
        editor.mode = "normal";
        editor.command = "";
        editor.message = `"${editor.file}" written`;
        return;
      case "q":
        if (editor.dirty) {
          editor.mode = "normal";
          editor.command = "";
          editor.message = "No write since last change (:q! overrides)";
          return;
        }
        this.exitEditor();
        return;
      case "q!":
        this.exitEditor();
        return;
      case "wq":
      case "x":
        this.files.set(editor.file, editor.lines.join("\n"));
        this.exitEditor();
        return;
      default:
        editor.mode = "normal";
        editor.command = "";
        editor.message = `Not an editor command: ${command}`;
    }
  }

  private exitEditor(): void {
    const file = this.editor?.file ?? "";
    this.editor = null;
    this.scrollback.push({ text: `closed ${file}`, fg: MUTED });
  }

  private submit(): void {
    const command = this.input.trim();
    this.scrollback.push({ text: `${PROMPT}${this.input}` });
    this.input = "";

    if (command.length === 0) {
      return;
    }
    if (command === "clear") {
      this.scrollback.length = 0;
      return;
    }

    this.scrollback.push(...this.runCommand(command));
  }

  private runCommand(command: string): Line[] {
    const [name, ...args] = splitWords(command);
    switch (name) {
      case "help":
        return [
          { text: "available commands:", fg: GREEN },
          { text: "  help                 show this help" },
          { text: "  ls                   list demo files" },
          { text: "  cat <file>           print a file" },
          { text: "  vim <file>           open the fake full-screen editor" },
          { text: "  echo <text>          print text" },
          { text: "  date                 print browser time" },
          { text: "  pwd                  print current directory" },
          { text: "  clear                clear the screen" },
        ];
      case "echo":
        return [{ text: args.join(" ") }];
      case "date":
        return [{ text: new Date().toString() }];
      case "pwd":
        return [{ text: "/home/guest" }];
      case "ls":
        return [...this.files.keys()].sort().map((file) => ({ text: file, fg: BLUE }));
      case "cat":
        return this.catFile(args[0]);
      case "vim":
      case "vi":
        return this.openEditor(args[0] ?? "untitled.txt");
      default:
        return [{ text: `${name}: command not found`, fg: RED }];
    }
  }

  private catFile(file: string | undefined): Line[] {
    if (!file) {
      return [{ text: "cat: missing file operand", fg: RED }];
    }
    const content = this.files.get(file);
    if (content == null) {
      return [{ text: `cat: ${file}: No such file`, fg: RED }];
    }
    return content.split("\n").map((text) => ({ text }));
  }

  private openEditor(file: string): Line[] {
    this.editor = {
      file,
      lines: (this.files.get(file) ?? "").split("\n"),
      x: 0,
      y: 0,
      mode: "normal",
      command: "",
      dirty: false,
      message: "",
    };
    if (this.editor.lines.length === 0) {
      this.editor.lines.push("");
    }
    return [];
  }

  private shellLines(): Line[] {
    return wrapLines([...this.scrollback, { text: `${PROMPT}${this.input}`, fg: FOREGROUND }], this.cols).slice(
      -this.rows,
    );
  }

  private editorLines(): Line[] {
    const editor = this.editor;
    if (!editor) {
      return [];
    }
    const bodyRows = Math.max(1, this.rows - 2);
    const lines: Line[] = [];
    for (let y = 0; y < bodyRows; y += 1) {
      lines.push({ text: fit(editor.lines[y] ?? "~", this.cols), fg: editor.lines[y] == null ? MUTED : FOREGROUND });
    }
    const status = `"${editor.file}" ${editor.lines.length}L ${editor.dirty ? "[modified]" : ""}`;
    lines.push({ text: fit(status, this.cols), fg: BACKGROUND, bg: MUTED, inverse: true });
    const commandLine = editor.mode === "command" ? `:${editor.command}` : editor.message || "NORMAL  i insert  :wq save+quit  :q! quit";
    lines.push({ text: fit(commandLine, this.cols), fg: editor.mode === "insert" ? GREEN : YELLOW });
    return lines;
  }

  private editorCursor(): Cursor {
    const editor = this.editor;
    if (!editor) {
      return { x: 0, y: 0, color: null };
    }
    if (editor.mode === "command") {
      return { x: Math.min(editor.command.length + 1, this.cols - 1), y: this.rows - 1, color: null };
    }
    return {
      x: Math.min(editor.x, this.cols - 1),
      y: Math.min(editor.y, Math.max(0, this.rows - 3)),
      color: null,
    };
  }
}

function parseInput(input: string): InputKey[] {
  const keys: InputKey[] = [];
  for (let i = 0; i < input.length; i += 1) {
    if (input.startsWith("\x1b[A", i)) {
      keys.push("ArrowUp");
      i += 2;
    } else if (input.startsWith("\x1b[B", i)) {
      keys.push("ArrowDown");
      i += 2;
    } else if (input.startsWith("\x1b[C", i)) {
      keys.push("ArrowRight");
      i += 2;
    } else if (input.startsWith("\x1b[D", i)) {
      keys.push("ArrowLeft");
      i += 2;
    } else if (input[i] === "\x1b") {
      keys.push("Escape");
    } else if (input[i] === "\r" || input[i] === "\n") {
      keys.push("Enter");
    } else if (input[i] === "\x7f") {
      keys.push("Backspace");
    } else if (input[i] === "\t") {
      keys.push("Tab");
    } else {
      keys.push(input[i]);
    }
  }
  return keys;
}

function moveEditor(editor: EditorState, key: InputKey): boolean {
  switch (key) {
    case "ArrowUp":
    case "k":
      editor.y = Math.max(0, editor.y - 1);
      break;
    case "ArrowDown":
    case "j":
      editor.y = Math.min(editor.lines.length - 1, editor.y + 1);
      break;
    case "ArrowLeft":
    case "h":
      editor.x = Math.max(0, editor.x - 1);
      return true;
    case "ArrowRight":
    case "l":
      editor.x = Math.min((editor.lines[editor.y] ?? "").length, editor.x + 1);
      return true;
    default:
      return false;
  }
  editor.x = Math.min(editor.x, Math.max(0, (editor.lines[editor.y] ?? "").length - 1));
  return true;
}

function splitWords(command: string): string[] {
  return command.split(/\s+/).filter((word) => word.length > 0);
}

function wrapLines(lines: Line[], cols: number): Line[] {
  const wrapped: Line[] = [];
  for (const line of lines) {
    if (line.text.length === 0) {
      wrapped.push(line);
      continue;
    }
    for (let start = 0; start < line.text.length; start += cols) {
      wrapped.push({ ...line, text: line.text.slice(start, start + cols) });
    }
  }
  return wrapped;
}

function cursorFor(lines: Line[], cols: number): Cursor {
  const lastLine = lines.at(-1);
  if (!lastLine) {
    return { x: 0, y: 0, color: null };
  }
  return { x: Math.min(lastLine.text.length, cols - 1), y: Math.max(0, lines.length - 1), color: null };
}

function fit(text: string, cols: number): string {
  return text.length > cols ? text.slice(0, cols) : text.padEnd(cols, " ");
}

function cell(x: number, y: number, text: string, line: Line): WebCell {
  return {
    x,
    y,
    text,
    fg: line.fg ?? FOREGROUND,
    bg: line.bg ?? null,
    style: {
      bold: false,
      italic: false,
      faint: false,
      blink: false,
      inverse: line.inverse ?? false,
      invisible: false,
      strikethrough: false,
      overline: false,
      underline: false,
    },
  };
}