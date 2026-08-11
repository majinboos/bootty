import { spawn } from "node:child_process";

export type JsonObject = Record<string, unknown>;
export type PiEvent = JsonObject;
export type PiContext = {
  sessionManager?: { getSessionId?: () => string | undefined };
  cwd?: string;
  mode?: "tui" | "rpc" | "json" | "print";
  sessionFile?: string;
  terminal_id?: string | number;
  process_id?: string | number;
};
export type Sink = (event: JsonObject) => void | Promise<void>;
export type PiExtension = {
  on: (name: string, handler: (event: PiEvent, context: PiContext) => void | Promise<void>) => void;
};

const KIND_BY_EVENT: Record<string, string> = {
  session_start: "session_started", session_info_changed: "state",
  session_before_switch: "session_switched", session_before_fork: "session_forked",
  session_before_compact: "compaction", session_compact: "compaction",
  session_shutdown: "session_ended", before_agent_start: "prompt_submitted",
  input: "prompt_submitted", agent_start: "turn_started", agent_end: "turn_completed",
  agent_settled: "agent_settled", turn_start: "turn_started", turn_end: "turn_completed",
  message_start: "message_started", message_update: "message_updated", message_end: "message_completed",
  tool_execution_start: "tool_started", tool_execution_update: "tool_updated",
  tool_execution_end: "tool_completed", tool_call: "tool_call", tool_result: "tool_result",
  model_select: "model_changed", thinking_level_select: "reasoning_changed",
};
export const PI_HOOK_EVENTS = Object.keys(KIND_BY_EVENT);

function isObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function copy(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(copy);
  if (!isObject(value)) return value;
  const result: JsonObject = {};
  for (const [key, item] of Object.entries(value)) result[key] = copy(item);
  return result;
}
function present(target: JsonObject, key: string, value: unknown): void {
  if (value !== undefined) target[key] = copy(value);
}
function sessionId(event: PiEvent, context: PiContext): string | undefined {
  const fromContext = context.sessionManager?.getSessionId?.();
  if (typeof fromContext === "string" && fromContext.length > 0) return fromContext;
  const explicit = event.adapter_session_id ?? event.adapterSessionId ?? event.session_id ?? event.sessionId;
  return typeof explicit === "string" && explicit.length > 0 ? explicit : undefined;
}
function nativeId(event: PiEvent, session: string): string | number {
  const explicit = event.event_id ?? event.eventId ?? event.native_id ?? event.nativeId;
  if (typeof explicit === "string" || typeof explicit === "number") return explicit;
  const tool = event.toolCallId ?? event.tool_call_id;
  if (typeof tool === "string" || typeof tool === "number") return tool;
  const entry = event.entryId ?? event.entry_id;
  if (typeof entry === "string" || typeof entry === "number") return entry;
  const message = isObject(event.message) ? event.message.id ?? event.message.messageId ?? event.message.message_id : undefined;
  if (typeof message === "string" || typeof message === "number") return message;
  return session;
}

function eventFacts(name: string, event: PiEvent, context: PiContext): JsonObject {
  const facts: JsonObject = { native_event: name, source: "hook" };
  const fields: Array<[string, string]> = [
    ["reason", "reason"], ["previousSessionFile", "previous_session_file"],
    ["targetSessionFile", "target_session_file"], ["entryId", "entry_id"], ["position", "position"],
    ["sessionFile", "session_file"], ["sessionName", "session_name"], ["name", "name"],
    ["cwd", "cwd"], ["mode", "mode"], ["prompt", "prompt"], ["text", "text"],
    ["images", "images"], ["streamingBehavior", "streaming_behavior"], ["turnIndex", "turn_index"],
    ["timestamp", "timestamp"], ["timestampMs", "timestamp_ms"], ["level", "level"],
    ["previousLevel", "previous_level"], ["previousModel", "previous_model"], ["model", "model"],
    ["args", "args"], ["partialResult", "partial_result"], ["result", "result"],
    ["isError", "is_error"], ["input", "input"], ["content", "content"], ["details", "details"],
    ["usage", "usage"], ["cost", "cost"], ["contextUsage", "context_usage"],
    ["preparation", "preparation"], ["branchEntries", "branch_entries"],
    ["customInstructions", "custom_instructions"], ["willRetry", "will_retry"],
    ["compactionEntry", "compaction_entry"], ["fromExtension", "from_extension"],
    ["messages", "messages"], ["toolResults", "tool_results"],
    ["assistantMessageEvent", "assistant_message_event"], ["steering", "steering"],
    ["followUp", "follow_up"], ["isStreaming", "is_streaming"], ["isCompacting", "is_compacting"],
    ["steeringMode", "steering_mode"], ["followUpMode", "follow_up_mode"],
    ["autoCompactionEnabled", "auto_compaction_enabled"], ["messageCount", "message_count"],
    ["pendingMessageCount", "pending_message_count"], ["tokens", "tokens"], ["error", "error"],
    ["extensionPath", "extension_path"],
  ];
  for (const [source, target] of fields) present(facts, target, event[source]);
  if (name === "input") present(facts, "input_source", event.inputSource ?? event.input_source ?? event.source);
  if (name === "model_select") present(facts, "selection_source", event.selectionSource ?? event.selection_source ?? event.source);
  present(facts, "event_id", event.event_id ?? event.eventId);
  present(facts, "tool_call_id", event.toolCallId ?? event.tool_call_id);
  if (isObject(event.message)) {
    present(facts, "message", event.message);
    present(facts, "message_id", event.message.id ?? event.message.messageId ?? event.message.message_id);
    if (facts.usage === undefined) present(facts, "usage", event.message.usage);
    if (facts.cost === undefined && isObject(event.message.usage)) present(facts, "cost", event.message.usage.cost);
    if (facts.cost === undefined) present(facts, "cost", event.message.cost);
  }
  present(facts, "cwd", context.cwd);
  present(facts, "mode", context.mode);
  present(facts, "session_file", context.sessionFile);
  return facts;
}

export function normalizePiHook(name: string, event: PiEvent, context: PiContext = {}): JsonObject | undefined {
  const kind = KIND_BY_EVENT[name];
  if (kind === undefined) return undefined;
  const session = sessionId(event, context);
  if (session === undefined) return undefined;
  const sequence = event.sequence ?? event.seq;
  if (sequence !== undefined && (typeof sequence !== "number" || !Number.isFinite(sequence))) return undefined;
  const normalized: JsonObject = {
    adapter: "pi", kind, native_id: nativeId(event, session), adapter_session_id: session,
    facts: eventFacts(name, event, context),
    raw: { source: "hook", event: name, payload: copy(event) },
  };
  if (sequence !== undefined) normalized.sequence = sequence;
  const terminal = event.terminal_id ?? event.terminalId ?? context.terminal_id;
  const process = event.process_id ?? event.processId ?? context.process_id;
  if (terminal !== undefined) normalized.terminal_id = terminal;
  if (process !== undefined) normalized.process_id = process;
  return normalized;
}

export function createBoottyPiExtension(sink: Sink): Record<string, (event: PiEvent, context: PiContext) => Promise<void>> {
  const handlers: Record<string, (event: PiEvent, context: PiContext) => Promise<void>> = {};
  for (const name of PI_HOOK_EVENTS) {
    handlers[name] = async (event, context) => {
      const normalized = normalizePiHook(name, event, context);
      if (normalized !== undefined) await sink(normalized);
    };
  }
  return handlers;
}

export const BOOTTY_INGEST_COMMAND = ["bootty", "command", "agents.ingest", "--stdin-json"] as const;
type BoottyCommand = (argv: string[], input: string) => void | Promise<void>;
function runBoottyCommand(argv: string[], input: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn(argv[0], argv.slice(1), { stdio: ["pipe", "ignore", "ignore"] });
    child.once("error", reject); child.once("close", () => resolve()); child.stdin.end(input);
  });
}
export function createBoottyCommandSink(command: BoottyCommand = runBoottyCommand): Sink {
  return async (event) => { await Promise.resolve(command([...BOOTTY_INGEST_COMMAND], JSON.stringify(event))).catch(() => undefined); };
}
export const createBoottyLocalIpcSink = createBoottyCommandSink;

export default function boottyPiExtension(pi: PiExtension): void {
  const handlers = createBoottyPiExtension(createBoottyCommandSink());
  for (const [name, handler] of Object.entries(handlers)) pi.on(name, handler);
}
