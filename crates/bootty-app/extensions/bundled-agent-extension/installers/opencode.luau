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

M.id = "opencode"
M.name = "OpenCode"
M.version = "1.18.16"
M.expected_version = M.version
M.protocol = {
    kind = "http-sse",
    command = "opencode serve",
    framing = "SSE",
    reachable = "v2-sdk",
}
M.marker = "bootty-agents-opencode-v1"
M.settings_path = "~/.config/opencode/opencode.json"
M.config_paths = {
    "~/.config/opencode/opencode.jsonc",
    "~/.config/opencode/opencode.json",
    "~/.config/opencode/config.json",
}
M.plugin_path = "~/.config/opencode/plugins/bootty.ts"
M.plugin_spec = "./plugins/bootty.ts"
M.plugin_source = [[
type JsonObject = Record<string, unknown>;
type OpenCodeEvent = { type?: unknown; data?: unknown; properties?: unknown; [key: string]: unknown };

const ingestCommand = ["bootty", "command", "agents.ingest", "--stdin-json"];
const object = (value: unknown): JsonObject =>
    value !== null && typeof value === "object" ? (value as JsonObject) : {};
const opaqueId = (value: unknown): string | number | undefined =>
    typeof value === "string" && value.length > 0 ? value :
    typeof value === "number" && Number.isFinite(value) ? value : undefined;

function nativeId(event: OpenCodeEvent, data: JsonObject): string | number | undefined {
    const type = typeof event.type === "string" ? event.type : "";
    const fields = type.includes("permission") ? ["requestID", "request_id", "id"] :
        type.includes("tool") ? ["callID", "call_id", "toolCallId"] :
        type.includes("message") ? ["messageID", "message_id", "partID", "part_id"] :
        ["sessionID", "session_id", "id"];
    for (const field of fields) {
        const id = opaqueId(data[field]);
        if (id !== undefined) return id;
    }
    return opaqueId(event.id);
}

async function forward(event: OpenCodeEvent, extra?: JsonObject) {
    const data = object(event.data ?? event.properties ?? event);
    const payload: JsonObject = {
        adapter: "opencode",
        kind: typeof event.type === "string" ? event.type : "unknown",
        facts: { event_type: event.type ?? "", payload: data, ...(extra ?? {}) },
        raw: event,
    };
    const id = nativeId(event, data);
    if (id !== undefined) payload.native_id = id;
    const bun = (globalThis as { Bun?: { spawn?: (spec: { cmd: string[]; stdin: Uint8Array; stdout: "ignore"; stderr: "ignore" }) => { exited?: Promise<unknown> } } }).Bun;
    await bun?.spawn?.({ cmd: ingestCommand, stdin: new TextEncoder().encode(JSON.stringify(payload)), stdout: "ignore", stderr: "ignore" })?.exited;
    return payload;
}

export default async function BoottyAgentPlugin() {
    return {
        event: ({ event }: { event: OpenCodeEvent }) => forward(event),
        "permission.ask": ({ permission, status }: { permission: JsonObject; status: JsonObject }) =>
            forward({ type: "permission.asked", data: permission }, { permission_status: status }),
        "session.created": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.updated": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.deleted": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "message.updated": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "message.part.updated": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.next.tool.called": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.next.tool.progress": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.next.tool.success": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "session.next.tool.failed": ({ event }: { event: OpenCodeEvent }) => forward(event),
        "tui.toast.show": ({ event }: { event: OpenCodeEvent }) => forward(event),
    };
end]]
M.marker_path = "~/.config/bootty/agents/opencode.json"
M.owned_paths = { M.settings_path, M.plugin_path, M.marker_path }
M.required_hooks = {
    "event",
    "permission.ask",
}
M.required_plugins = { "bootty.ts" }
M.required_controls = { "session.create", "session.prompt", "session.interrupt", "permission.reply" }
M.optional_controls = { "session.wait", "session.compact", "history.replay", "quota" }
M.host_limits = {
    max_config_bytes = 2 * 1024 * 1024,
    max_sse_event_bytes = 2 * 1024 * 1024,
}
M.config_order = { M.settings_path, M.plugin_path, M.marker_path }

function M.generated_plugin(options)
    if type(options) == "table" and type(options.plugin_source) == "string" then
        return options.plugin_source
    end
    return M.plugin_source
end

function M.generated_plugin_spec()
    return M.plugin_spec
end

function M.merge_config(config, _options)
    if config == nil then config = {} end
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "OpenCode config must be a JSON object",
        }
    end
    local result = common.copy(config)
    local plugins = result.plugin
    if plugins == nil then
        plugins = {}
    elseif type(plugins) == "string" then
        plugins = { plugins }
    elseif type(plugins) ~= "table" or not common.is_array(plugins) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "plugin",
            message = "OpenCode plugin must be a string or array",
        }
    else
        plugins = common.array_copy(plugins)
    end
    if not common.contains(plugins, M.plugin_spec) then
        plugins[#plugins + 1] = M.plugin_spec
    end
    result.plugin = plugins
    return result
end

local function spec_owned(value)
    if value == M.plugin_spec then return true end
    if type(value) == "table" and #value > 0 and value[1] == M.plugin_spec then return true end
    return false
end

function M.remove_config(config, marker, _options)
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "OpenCode config must be a JSON object",
        }
    end
    if not common.marker_matches(marker, M) then
        return common.copy(config), {
            code = "ownership_marker_missing",
            adapter = M.id,
            path = M.marker_path,
            message = "refusing to remove OpenCode plugin without the matching Bootty ownership marker",
        }
    end
    local result = common.copy(config)
    local plugins = result.plugin
    if plugins == nil then return result end
    if type(plugins) == "string" then
        if plugins == M.plugin_spec then result.plugin = nil end
        return result
    end
    if type(plugins) ~= "table" or not common.is_array(plugins) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "plugin",
            message = "OpenCode plugin must be a string or array",
        }
    end
    local kept = {}
    for index = 1, #plugins do
        if not spec_owned(plugins[index]) then kept[#kept + 1] = common.copy(plugins[index]) end
    end
    if #kept == 0 then result.plugin = nil else result.plugin = kept end
    return result
end

function M.expected_files(options, context)
    local merged, merge_error = M.merge_config(options and options.settings, options)
    if not merged then return nil, merge_error end
    local marker = common.marker_payload(M, context)
    local plugin = M.generated_plugin(options)
    if plugin ~= nil then marker.owned_contents = { [M.plugin_path] = plugin } end
    local settings_path = options and options.config_path or M.settings_path
    local files = {
        { path = common.resolve_path(settings_path, context), operation = "write", value = merged, format = "json" },
        { path = common.resolve_path(M.marker_path, context), operation = "write", value = marker, format = "json" },
    }
    if plugin ~= nil then
        files[2] = { path = common.resolve_path(M.plugin_path, context), operation = "write", value = plugin, format = "text" }
        files[3] = { path = common.resolve_path(M.marker_path, context), operation = "write", value = marker, format = "json" }
    end
    return files
end

function M.doctor(observation)
    observation = observation or {}
    local diagnostics = {}
    local function add(code, severity, message, details)
        local item = { code = code, severity = severity, adapter = M.id, message = message }
        if type(details) == "table" then for key, value in pairs(details) do item[key] = value end end
        diagnostics[#diagnostics + 1] = item
    end
    local installed = observation.installed_version or observation.version
    if installed == nil then
        add("installed_version_missing", "warning", "OpenCode version was not supplied")
    elseif installed ~= M.version then
        add("installed_version_mismatch", "warning", "OpenCode version differs from the bundled SDK contract", { expected = M.version, actual = installed })
    end
    if observation.protocol_reachable == false or observation.reachable == false then
        add("protocol_unreachable", "error", "OpenCode server/SSE protocol is not reachable")
    elseif observation.protocol_reachable == nil and observation.reachable == nil then
        add("protocol_unchecked", "warning", "OpenCode protocol reachability was not supplied")
    end
    for index = 1, #M.required_hooks do
        local hook = M.required_hooks[index]
        if type(observation.hooks) ~= "table" or not observation.hooks[hook] then
            add("missing_hook", "error", "OpenCode plugin hook is missing", { hook = hook })
        end
    end
    for index = 1, #M.required_plugins do
        local plugin = M.required_plugins[index]
        if type(observation.plugins) ~= "table" or not observation.plugins[plugin] then
            add("missing_plugin", "error", "OpenCode plugin is missing", { plugin = plugin })
        end
    end
    if type(observation.malformed_events) == "table" then
        for index = 1, #observation.malformed_events do add("malformed_event", "error", "OpenCode emitted a malformed event", { event = observation.malformed_events[index] }) end
    end
    if type(observation.stale_sessions) == "table" then
        for index = 1, #observation.stale_sessions do add("stale_session", "warning", "OpenCode session is stale", { session = observation.stale_sessions[index] }) end
    end
    if type(observation.controls) == "table" then
        for index = 1, #M.required_controls do
            local control = M.required_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "error", "OpenCode required control is unavailable", { control = control }) end
        end
        for index = 1, #M.optional_controls do
            local control = M.optional_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "info", "OpenCode control is unavailable in the current server", { control = control, optional = true }) end
        end
    end
    local size = observation.config_bytes or observation.sse_event_bytes
    if type(size) == "number" and size > M.host_limits.max_config_bytes then
        add("host_limit_exceeded", "error", "OpenCode configuration/event exceeds the host limit", { limit = M.host_limits.max_config_bytes, actual = size })
    end
    return diagnostics
end

M.merge = M.merge_config
M.unmerge = M.remove_config
M.diagnose = M.doctor
return M
