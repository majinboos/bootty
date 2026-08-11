--!strict
local function load_common()
    local ok, value = pcall(require, "./common")
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

M.id = "codex"
M.name = "OpenAI Codex CLI"
M.version = "0.0.0"
M.expected_version = M.version
M.source_revision = "50ef7395faee1d0e2d01730f9636aa0601c7be3"
M.protocol = {
    kind = "jsonrpc-jsonl",
    command = "codex app-server",
    framing = "newline-delimited JSON",
    reachable = "initialize",
}
M.marker = "bootty-agents-codex-v1"
M.settings_path = "~/.codex/hooks.json"
M.config_paths = {
    "~/.codex/hooks.json",
    "~/.codex/config.toml",
    ".codex/hooks.json",
    ".codex/config.toml",
}
M.config_path = M.settings_path
M.inline_config_path = "~/.codex/config.toml"
M.marker_path = "~/.config/bootty/agents/codex.json"
M.owned_paths = { M.settings_path, M.inline_config_path, M.marker_path }
M.required_hooks = {
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
    "Stop",
}
M.required_plugins = {}
M.required_controls = { "thread_start", "turn_start", "turn_interrupt", "initialize" }
M.optional_controls = { "turn_steer", "thread_resume", "thread_fork", "thread_compact", "approval" }
M.host_limits = {
    max_hooks_output_bytes = 2 * 1024 * 1024,
    max_session_end_timeout_seconds = 3,
}
M.config_order = { M.settings_path, M.inline_config_path, M.marker_path }

local function command_for(event)
    return "bootty command agents.ingest --adapter codex --event-name " .. event .. " --stdin-json"
end

function M.generated_hook(event)
    return {
        type = "command",
        command = command_for(event),
        timeout = event == "SessionEnd" and 1 or 600,
    }
end

function M.merge_config(config, _options)
    if config == nil then config = {} end
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "Codex hooks.json must be a JSON object",
        }
    end
    local result = common.copy(config)
    if result.hooks == nil then result.hooks = {} end
    if type(result.hooks) ~= "table" or (common.is_array(result.hooks) and next(result.hooks) ~= nil) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "hooks",
            message = "Codex hooks must be an object keyed by event",
        }
    end
    for index = 1, #M.required_hooks do
        local event = M.required_hooks[index]
        local entries = result.hooks[event]
        if entries == nil then
            entries = {}
        elseif type(entries) ~= "table" or not common.is_array(entries) then
            return nil, {
                code = "malformed_config",
                adapter = M.id,
                path = M.settings_path,
                field = "hooks." .. event,
                message = "Codex hook events must be arrays",
            }
        else
            entries = common.array_copy(entries)
        end
        local hook = M.generated_hook(event)
        local found = false
        for entry_index = 1, #entries do
            local entry = entries[entry_index]
            if common.equal(entry, hook) then
                found = true
            elseif type(entry) == "table" and type(entry.hooks) == "table" and common.is_array(entry.hooks) then
                local hooks = common.array_copy(entry.hooks)
                if not common.contains(hooks, hook) then
                    local copy_entry = common.copy(entry)
                    hooks[#hooks + 1] = hook
                    copy_entry.hooks = hooks
                    entries[entry_index] = copy_entry
                end
                found = true
            end
        end
        if not found then entries[#entries + 1] = { matcher = "*", hooks = { hook } } end
        result.hooks[event] = entries
    end
    return result
end

local function is_owned_hook(value, event)
    return type(value) == "table" and common.equal(value, M.generated_hook(event))
end

function M.remove_config(config, marker, _options)
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "Codex hooks.json must be a JSON object",
        }
    end
    if not common.marker_matches(marker, M) then
        return common.copy(config), {
            code = "ownership_marker_missing",
            adapter = M.id,
            path = M.marker_path,
            message = "refusing to remove Codex hooks without the matching Bootty ownership marker",
        }
    end
    local result = common.copy(config)
    if result.hooks == nil then return result end
    if type(result.hooks) ~= "table" or (common.is_array(result.hooks) and next(result.hooks) ~= nil) then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            field = "hooks",
            message = "Codex hooks must be an object keyed by event",
        }
    end
    for index = 1, #M.required_hooks do
        local event = M.required_hooks[index]
        local entries = result.hooks[event]
        if entries ~= nil and (type(entries) ~= "table" or not common.is_array(entries)) then
            return nil, {
                code = "malformed_config",
                adapter = M.id,
                path = M.settings_path,
                field = "hooks." .. event,
                message = "Codex hook events must be arrays",
            }
        end
        if type(entries) == "table" then
            local kept_entries = {}
            for entry_index = 1, #entries do
                local entry = entries[entry_index]
                if type(entry) == "table" and type(entry.hooks) == "table" and common.is_array(entry.hooks) then
                    local kept_hooks = {}
                    for hook_index = 1, #entry.hooks do
                        if not is_owned_hook(entry.hooks[hook_index], event) then kept_hooks[#kept_hooks + 1] = common.copy(entry.hooks[hook_index]) end
                    end
                    local copy_entry = common.copy(entry)
                    copy_entry.hooks = kept_hooks
                    if #kept_hooks > 0 then
                        kept_entries[#kept_entries + 1] = copy_entry
                    else
                        local has_other_fields = false
                        for key, _ in pairs(copy_entry) do if key ~= "hooks" then has_other_fields = true end end
                        if has_other_fields then kept_entries[#kept_entries + 1] = copy_entry end
                    end
                elseif not is_owned_hook(entry, event) then
                    kept_entries[#kept_entries + 1] = common.copy(entry)
                end
            end
            if #kept_entries == 0 then result.hooks[event] = nil else result.hooks[event] = kept_entries end
        end
    end
    if next(result.hooks) == nil then result.hooks = nil end
    return result
end

local toml_marker_begin = "# bootty.agents codex hooks begin"
local toml_marker_end = "# bootty.agents codex hooks end"

function M.merge_toml(text)
    if text == nil then text = "" end
    if type(text) ~= "string" then
        return nil, { code = "malformed_config", adapter = M.id, path = M.inline_config_path, message = "Codex config.toml must be text" }
    end
    if text:find(toml_marker_begin, 1, true) then return text end
    local lines = { toml_marker_begin }
    if text == "" then
        lines[#lines + 1] = "[features]"
        lines[#lines + 1] = "hooks = true"
    end
    for index = 1, #M.required_hooks do
        local event = M.required_hooks[index]
        lines[#lines + 1] = "[[hooks." .. event .. "]]"
        lines[#lines + 1] = "matcher = \"*\""
        lines[#lines + 1] = "[[hooks." .. event .. ".hooks]]"
        lines[#lines + 1] = "type = \"command\""
        lines[#lines + 1] = "command = \"bootty command agents.ingest --adapter codex --event-name " .. event .. " --stdin-json\""
        lines[#lines + 1] = "timeout = " .. (event == "SessionEnd" and "1" or "600")
    end
    lines[#lines + 1] = toml_marker_end
    local block = table.concat(lines, "\n")
    if text ~= "" and text:sub(-1) ~= "\n" then text = text .. "\n" end
    return text .. block .. "\n"
end

function M.remove_toml(text, marker)
    if type(text) ~= "string" then return nil, { code = "malformed_config", adapter = M.id, path = M.inline_config_path, message = "Codex config.toml must be text" } end
    if not common.marker_matches(marker, M) then return text, { code = "ownership_marker_missing", adapter = M.id, path = M.marker_path, message = "matching ownership marker is missing" } end
    local pattern = toml_marker_begin .. ".-" .. toml_marker_end .. "\n?"
    return (text:gsub(pattern, "", 1))
end

function M.expected_files(options, context)
    local config_path = options and options.config_path or M.settings_path
    local merged, merge_error = M.merge_config(options and options.settings, options)
    if not merged then return nil, merge_error end
    local inline, inline_error = M.merge_toml(options and options.inline_config or "")
    if inline == nil then return nil, inline_error end
    local marker = common.marker_payload(M, context)
    marker.files = { config_path, M.inline_config_path, M.marker_path }
    marker.owned_contents = { [M.inline_config_path] = inline }
    local files = {
        { path = common.resolve_path(config_path, context), operation = "write", value = merged, format = "json" },
    }
    if common.resolve_path(config_path, context) == common.resolve_path(M.inline_config_path, context) then
        files[1] = { path = common.resolve_path(M.inline_config_path, context), operation = "write", value = inline, format = "text" }
    else
        files[#files + 1] = { path = common.resolve_path(M.inline_config_path, context), operation = "write", value = inline, format = "text" }
    end
    files[#files + 1] = { path = common.resolve_path(M.marker_path, context), operation = "write", value = marker, format = "json" }
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
        add("installed_version_missing", "warning", "Codex version was not supplied")
    elseif type(installed) == "string" and installed ~= M.version then
        add("installed_version_mismatch", "warning", "Codex hook contract revision differs from the installed binary", { expected = M.version, actual = installed })
    end
    if observation.protocol_reachable == false or observation.reachable == false then
        add("protocol_unreachable", "error", "Codex app-server protocol is not reachable")
    elseif observation.protocol_reachable == nil and observation.reachable == nil then
        add("protocol_unchecked", "warning", "Codex app-server reachability was not supplied")
    end
    for index = 1, #M.required_hooks do
        local hook = M.required_hooks[index]
        if type(observation.hooks) ~= "table" or not observation.hooks[hook] then
            add("missing_hook", "error", "Codex hook is missing", { hook = hook })
        end
    end
    if type(observation.malformed_events) == "table" then
        for index = 1, #observation.malformed_events do add("malformed_event", "error", "Codex emitted a malformed event", { event = observation.malformed_events[index] }) end
    end
    if type(observation.stale_sessions) == "table" then
        for index = 1, #observation.stale_sessions do add("stale_session", "warning", "Codex session is stale", { session = observation.stale_sessions[index] }) end
    end
    if type(observation.controls) == "table" then
        for index = 1, #M.required_controls do
            local control = M.required_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "error", "Codex required control is unavailable", { control = control }) end
        end
        for index = 1, #M.optional_controls do
            local control = M.optional_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "info", "Codex optional control is unavailable", { control = control, optional = true }) end
        end
    end
    local size = observation.hooks_output_bytes or observation.config_bytes
    if type(size) == "number" and size > M.host_limits.max_hooks_output_bytes then
        add("host_limit_exceeded", "error", "Codex hook output exceeds the host limit", { limit = M.host_limits.max_hooks_output_bytes, actual = size })
    end
    return diagnostics
end

M.merge = M.merge_config
M.unmerge = M.remove_config
M.diagnose = M.doctor
return M
