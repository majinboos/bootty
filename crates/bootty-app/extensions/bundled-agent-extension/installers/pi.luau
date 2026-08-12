--!strict
local function load_common()
    local ok, value = pcall(require, "installers/common")
    if ok then return value end
    local candidates = {
        "./installers/common.luau",
        "crates/bootty-app/extensions/bundled-agent-extension/installers/common.luau",
    }
    for index = 1, #candidates do
        if type(loadfile) == "function" then
            local chunk = loadfile(candidates[index])
            if chunk ~= nil then
                local loaded, result = pcall(chunk)
                if loaded then return result end
            end
        end
    end
    error("unable to load installer common module: " .. tostring(value))
end
local common = load_common()

local M = {}
M.extension_path = "~/.pi/agent/extensions/bootty.ts"
M.id = "pi"
M.name = "Pi"
M.version = "0.84.1"
M.expected_version = M.version
M.protocol = {
    kind = "jsonl",
    command = "pi --mode rpc",
    framing = "LF JSONL",
    reachable = "rpc",
}
M.marker = "bootty-agents-pi-v1"
M.settings_path = "~/.pi/agent/settings.json"
M.marker_path = "~/.config/bootty/agents/pi.json"
M.config_path = M.settings_path
M.extension_path = "~/.pi/agent/extensions/bootty.ts"
M.extension_source = [[import { spawn } from "node:child_process";

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
]]
M.extension_asset = "adapters/pi/extension.ts"
M.source = M.extension_asset
M.owned_paths = { M.settings_path, M.extension_path, M.marker_path }
M.required_hooks = {
    "session_start",
    "session_shutdown",
    "agent_start",
    "agent_settled",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
}
M.required_plugins = { "bootty.ts" }
M.required_controls = { "prompt", "steer", "follow_up", "abort", "get_state" }
M.optional_controls = { "compact", "fork", "clone", "set_model", "set_thinking_level" }
M.host_limits = {
    max_settings_bytes = 2 * 1024 * 1024,
    max_preview_bytes = 2 * 1024 * 1024,
}
M.config_order = { M.settings_path, M.extension_path, M.marker_path }

local function extension_value(manifest, options)
    if type(options) == "table" and type(options.extension_source) == "string" then
        return options.extension_source
    end
    if type(manifest.extension_source) == "string" then
        return manifest.extension_source
    end
    return nil
end

function M.generated_extension(options)
    return extension_value(M, options)
end

function M.generated_settings_entry()
    return M.extension_path
end

function M.merge_config(config, _options)
    if config == nil then
        config = {}
    end
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "Pi settings must be a JSON object",
        }
    end
    local result = common.copy(config)
    local extensions = result.extensions
    if extensions == nil then
        extensions = {}
    elseif type(extensions) ~= "table" or not common.is_array(extensions) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "extensions",
            message = "Pi settings extensions must be an array",
        }
    else
        extensions = common.array_copy(extensions)
    end
    if not common.contains(extensions, M.extension_path) then
        extensions[#extensions + 1] = M.extension_path
    end
    result.extensions = extensions
    return result
end

function M.remove_config(config, marker, _options)
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "Pi settings must be a JSON object",
        }
    end
    if not common.marker_matches(marker, M) then
        return common.copy(config), {
            code = "ownership_marker_missing",
            adapter = M.id,
            path = M.marker_path,
            message = "refusing to remove Pi settings without the matching Bootty ownership marker",
        }
    end
    local result = common.copy(config)
    local extensions = result.extensions
    if extensions ~= nil and (type(extensions) ~= "table" or not common.is_array(extensions)) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "extensions",
            message = "Pi settings extensions must be an array",
        }
    end
    if type(extensions) == "table" then
        local kept = {}
        for index = 1, #extensions do
            if extensions[index] ~= M.extension_path then
                kept[#kept + 1] = common.copy(extensions[index])
            end
        end
        if #kept == 0 then
            result.extensions = nil
        else
            result.extensions = kept
        end
    end
    return result
end

function M.expected_files(options, context)
    local settings = options and options.settings
    local merged, merge_error = M.merge_config(settings, options)
    if not merged then
        return nil, merge_error
    end
    local extension = extension_value(M, options)
    local marker = common.marker_payload(M, context)
    if extension ~= nil then
        marker.owned_contents = { [M.extension_path] = extension }
    end
    local files = {
        { path = common.resolve_path(M.settings_path, context), operation = "write", value = merged, format = "json" },
    }
    if extension ~= nil then
        files[#files + 1] = {
            path = common.resolve_path(M.extension_path, context),
            operation = "write",
            value = extension,
            format = "text",
        }
    end
    files[#files + 1] = {
        path = common.resolve_path(M.marker_path, context),
        operation = "write",
        value = marker,
        format = "json",
    }
    return files
end

function M.doctor(observation)
    observation = observation or {}
    local diagnostics = {}
    local function add(code, severity, message, details)
        local item = { code = code, severity = severity, adapter = M.id, message = message }
        if type(details) == "table" then
            for key, value in pairs(details) do item[key] = value end
        end
        diagnostics[#diagnostics + 1] = item
    end
    local installed = observation.installed_version or observation.version
    if installed == nil then
        add("installed_version_missing", "warning", "Pi version was not supplied")
    elseif installed ~= M.version then
        add("installed_version_mismatch", "error", "Pi version does not match the bundled adapter", { expected = M.version, actual = installed })
    end
    if observation.protocol_reachable == false or observation.reachable == false then
        add("protocol_unreachable", "error", "Pi RPC protocol is not reachable")
    elseif observation.protocol_reachable == nil and observation.reachable == nil then
        add("protocol_unchecked", "warning", "Pi RPC reachability was not supplied")
    end
    for index = 1, #M.required_hooks do
        local hook = M.required_hooks[index]
        if type(observation.hooks) ~= "table" or not observation.hooks[hook] then
            add("missing_hook", "error", "Pi extension hook is missing", { hook = hook })
        end
    end
    for index = 1, #M.required_plugins do
        local plugin = M.required_plugins[index]
        if type(observation.plugins) ~= "table" or not observation.plugins[plugin] then
            add("missing_plugin", "error", "Pi extension plugin is missing", { plugin = plugin })
        end
    end
    if type(observation.malformed_events) == "table" then
        for index = 1, #observation.malformed_events do
            add("malformed_event", "error", "Pi emitted a malformed event", { event = observation.malformed_events[index] })
        end
    end
    if type(observation.stale_sessions) == "table" then
        for index = 1, #observation.stale_sessions do
            add("stale_session", "warning", "Pi session is stale", { session = observation.stale_sessions[index] })
        end
    end
    if type(observation.controls) == "table" then
        for index = 1, #M.required_controls do
            local control = M.required_controls[index]
            if observation.controls[control] == false then
                add("unsupported_control", "error", "Pi required control is unavailable", { control = control })
            end
        end
        for index = 1, #M.optional_controls do
            local control = M.optional_controls[index]
            if observation.controls[control] == false then
                add("unsupported_control", "info", "Pi optional control is unavailable", { control = control, optional = true })
            end
        end
    end
    local size = observation.settings_bytes or observation.config_bytes
    if type(size) == "number" and size > M.host_limits.max_settings_bytes then
        add("host_limit_exceeded", "error", "Pi settings exceed the host file-size limit", { limit = M.host_limits.max_settings_bytes, actual = size })
    end
    return diagnostics
end

M.merge = M.merge_config
M.unmerge = M.remove_config
M.diagnose = M.doctor
return M
