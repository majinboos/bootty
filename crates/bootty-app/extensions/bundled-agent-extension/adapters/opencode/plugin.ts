// Native OpenCode plugin ingress. The plugin forwards only OpenCode hook/event
// payloads; adapter.luau owns normalization and schema inspection.

type JsonObject = Record<string, unknown>;
type BoottyCommand = (command: string[], input: string) => unknown | Promise<unknown>;
type Sink = (event: JsonObject) => void | Promise<void>;

type OpenCodeEvent = {
  type?: unknown;
  id?: unknown;
  data?: unknown;
  properties?: unknown;
  durable?: unknown;
  [key: string]: unknown;
};

export const BOOTTY_INGEST_COMMAND = [
  "bootty",
  "command",
  "agents.ingest",
  "--stdin-json",
] as const;

type SpawnSpec = {
  cmd: string[];
  stdin: Uint8Array;
  stdout: "ignore";
  stderr: "ignore";
};

type BunRuntime = {
  spawn(spec: SpawnSpec): { exited?: Promise<unknown> };
};

function isJsonObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function bunRuntime(): BunRuntime | undefined {
  const runtime = globalThis;
  if (!("Bun" in runtime)) return undefined;
  const candidate = runtime.Bun;
  if (!isJsonObject(candidate) || typeof candidate.spawn !== "function") return undefined;
  const spawn = candidate.spawn;
  return {
    spawn(spec) {
      return spawn(spec);
    },
  };
}

function runBoottyCommand(command: string[], input: string) {
  const bun = bunRuntime();
  return bun?.spawn({
    cmd: command,
    stdin: new TextEncoder().encode(input),
    stdout: "ignore",
    stderr: "ignore",
  })?.exited;
}

export function createBoottyCommandSink(command: BoottyCommand = runBoottyCommand): Sink {
  return async (event) => {
    await Promise.resolve(command([...BOOTTY_INGEST_COMMAND], JSON.stringify(event))).catch(() => undefined);
  };
}

function object(value: unknown): JsonObject | undefined {
  if (!isJsonObject(value)) return undefined;
  return value;
}

function dataOf(event: OpenCodeEvent): JsonObject {
  return object(event.data) ?? object(event.properties) ?? event;
}

function idOf(value: unknown): string | number | undefined {
  if (typeof value === "string" && value.length > 0) return value;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return undefined;
}

function firstId(source: JsonObject | undefined, fields: string[]): string | number | undefined {
  if (!source) return undefined;
  for (const field of fields) {
    const candidate = idOf(source[field]);
    if (candidate !== undefined) return candidate;
  }
  return undefined;
}

function sessionId(data: JsonObject): string | number | undefined {
  return firstId(data, ["sessionID", "session_id"]);
}

function nativeId(event: OpenCodeEvent, data: JsonObject): string | number | undefined {
  const type = typeof event.type === "string" ? event.type : "";
  let candidate: string | number | undefined;
  if (type === "permission.asked" || type === "permission.v2.asked") {
    candidate = firstId(data, ["id"]);
  } else if (type === "permission.replied" || type === "permission.v2.replied") {
    candidate = firstId(data, ["requestID", "request_id", "id"]);
  } else if (type.startsWith("message.part.")) {
    candidate = firstId(data, ["partID", "part_id"]);
    candidate ??= firstId(object(data.part), ["id", "partID", "part_id"]);
    candidate ??= firstId(data, ["messageID", "message_id"]);
  } else if (type === "message.updated") {
    candidate = firstId(data, ["messageID", "message_id"]);
    candidate ??= firstId(object(data.info), ["id", "messageID", "message_id"]);
  } else if (type === "message.removed") {
    candidate = firstId(data, ["messageID", "message_id"]);
  } else if (type.startsWith("session.next.tool.")) {
    candidate = firstId(data, ["callID", "call_id"]);
  } else if (type.startsWith("session.next.")) {
    candidate = firstId(data, ["messageID", "message_id", "assistantMessageID", "assistant_message_id"]);
  }
  candidate ??= sessionId(data);
  candidate ??= firstId(event, ["id", "event_id", "eventID"]);
  return candidate;
}

function kindOf(eventType: string): string {
  const fixed: Record<string, string> = {
    "server.connected": "server_connected",
    "session.created": "session_started",
    "session.updated": "session_updated",
    "session.deleted": "session_deleted",
    "session.status": "session_status",
    "session.idle": "session_idle",
    "session.error": "error",
    "session.diff": "diff",
    "session.compacted": "compaction",
    "message.updated": "message_updated",
    "message.removed": "message_removed",
    "message.part.updated": "message_part_updated",
    "message.part.removed": "message_part_removed",
    "message.part.delta": "message_part_delta",
    "permission.asked": "permission_requested",
    "permission.v2.asked": "permission_requested",
    "permission.replied": "permission_resolved",
    "permission.v2.replied": "permission_resolved",
    "session.next.prompt.admitted": "prompt_admitted",
    "session.next.prompted": "prompt_submitted",
    "session.next.tool.called": "tool_called",
    "session.next.tool.progress": "tool_progress",
    "session.next.tool.success": "tool_succeeded",
    "session.next.tool.failed": "tool_failed",
    "session.next.compaction.started": "compaction_started",
    "session.next.compaction.ended": "compaction_ended",
  };
  if (fixed[eventType]) return fixed[eventType];
  if (eventType.startsWith("tui.")) return "tui";
  if (eventType.startsWith("session.next.tool.")) return "tool";
  if (eventType.startsWith("session.next.compaction.")) return "compaction";
  if (eventType.startsWith("permission.")) return "permission";
  return "unknown";
}

function eventEnvelope(event: OpenCodeEvent, extraFacts?: JsonObject): JsonObject {
  const eventType = typeof event.type === "string" ? event.type : "";
  const data = dataOf(event);
  const facts: JsonObject = {
    event_type: eventType,
    payload: data,
    ...(extraFacts ?? {}),
  };
  const normalized: JsonObject = {
    adapter: "opencode",
    kind: kindOf(eventType),
    facts,
    raw: event,
  };
  const native = nativeId(event, data);
  if (native !== undefined) normalized.native_id = native;
  const session = sessionId(data);
  if (session !== undefined) normalized.adapter_session_id = session;
  const durable = object(event.durable);
  if (durable) {
    if (durable.seq !== undefined) normalized.sequence = durable.seq;
    facts.durable = durable;
  } else if (event.sequence !== undefined) {
    normalized.sequence = event.sequence;
  }
  return normalized;
}

export function createBoottyAgentForwarder(sink: Sink) {
  return async (event: OpenCodeEvent) => {
    const forwarded = eventEnvelope(event);
    await sink(forwarded);
    return forwarded;
  };
}

export async function BoottyAgentPlugin(): Promise<JsonObject> {
  const forward = createBoottyAgentForwarder(createBoottyCommandSink());
  return {
    event: async ({ event }: { event: OpenCodeEvent }) => forward(event),
    "permission.ask": async ({
      permission,
      status,
    }: {
      permission: JsonObject;
      status: JsonObject;
    }) => {
      const forwarded = eventEnvelope(
        {
          type: "permission.ask",
          data: permission,
        },
        { permission_status: status },
      );
      await createBoottyCommandSink()(forwarded);
    },
  };
}

export default BoottyAgentPlugin;
