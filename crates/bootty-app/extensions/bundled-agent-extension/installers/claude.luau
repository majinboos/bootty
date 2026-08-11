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

M.id = "claude"
M.name = "Claude Code"
M.version = 1
M.expected_version = M.version
M.protocol = {
    kind = "stream-json",
    command = "claude -p --output-format stream-json",
    framing = "JSONL",
    reachable = "streaming_input",
}
M.marker = "bootty-agents-claude-v1"
M.settings_path = "~/.claude/settings.json"
M.config_path = M.settings_path
M.hook_script_path = "~/.config/bootty/agents/claude-hook.sh"
M.hook_script_source = [[#!/bin/sh
set -eu

# Claude supplies one authoritative JSON object on stdin. Keep stdin attached
# to the ingest command; never inspect transcripts, terminal output, or
# process metadata.
if [ "$#" -ne 2 ] || [ "$1" != "--event" ] || [ -z "$2" ]; then
    printf '%s\n' "usage: claude-hook.sh --event EVENT" >&2
    exit 2
fi

exec bootty command agents.ingest --adapter claude --event-name "$2" --stdin-json
]]
M.marker_path = "~/.config/bootty/agents/claude.json"
M.owned_paths = { M.settings_path, M.hook_script_path, M.marker_path }
M.required_hooks = {
    "SessionStart",
    "SessionEnd",
    "Setup",
    "UserPromptSubmit",
    "UserPromptExpansion",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "PermissionRequest",
    "PermissionDenied",
    "Elicitation",
    "ElicitationResult",
    "Notification",
    "MessageDisplay",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "PreCompact",
    "PostCompact",
    "TaskCreated",
    "TaskCompleted",
    "TeammateIdle",
    "WorktreeCreate",
    "WorktreeRemove",
    "ConfigChange",
    "InstructionsLoaded",
    "CwdChanged",
    "DirectoryAdded",
    "FileChanged",
}
M.required_plugins = {}
M.required_controls = { "prompt", "interrupt", "close" }
M.optional_controls = { "steer", "follow_up", "stop_task", "set_permission_mode", "set_model" }
M.host_limits = {
    max_settings_bytes = 2 * 1024 * 1024,
    max_hook_timeout_seconds = 3,
}
M.config_order = { M.settings_path, M.hook_script_path, M.marker_path }

local function command_for(event)
    return M.hook_script_path .. " --event " .. event
end

function M.generated_hook(event)
    return {
        type = "command",
        command = command_for(event),
        timeout = event == "SessionEnd" and 1 or 3,
    }
end
function M.merge_config(config, _options)
    if config == nil then config = {} end
    if type(config) ~= "table" then
        return nil, {
            code = "malformed_config",
            adapter = M.id,
            path = M.settings_path,
            message = "Claude settings must be a JSON object",
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
            message = "Claude settings hooks must be an object keyed by event",
        }
    end
    for index = 1, #M.required_hooks do
        local event = M.required_hooks[index]
        local groups = result.hooks[event]
        if groups == nil then
            groups = {}
        elseif type(groups) ~= "table" or not common.is_array(groups) then
            return nil, {
                code = "malformed_config",
                adapter = M.id,
                path = M.settings_path,
                field = "hooks." .. event,
                message = "Claude hook groups must be arrays",
            }
        else
            groups = common.array_copy(groups)
        end
        local hook = M.generated_hook(event)
        local found = false
        for group_index = 1, #groups do
            local group = groups[group_index]
            if type(group) == "table" and type(group.hooks) == "table" and common.is_array(group.hooks) then
                for hook_index = 1, #group.hooks do
                    if common.equal(group.hooks[hook_index], hook) then
                        found = true
                    end
                end
                if not found and #group.hooks > 0 then
                    local copy_group = common.copy(group)
                    copy_group.hooks = common.append_unique(copy_group.hooks, hook)
                    groups[group_index] = copy_group
                    found = true
                end
            end
        end
        if not found then
            groups[#groups + 1] = { matcher = "*", hooks = { hook } }
        end
        result.hooks[event] = groups
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
            message = "Claude settings must be a JSON object",
        }
    end
    if not common.marker_matches(marker, M) then
        return common.copy(config), {
            code = "ownership_marker_missing",
            adapter = M.id,
            path = M.marker_path,
            message = "refusing to remove Claude hooks without the matching Bootty ownership marker",
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
            message = "Claude settings hooks must be an object keyed by event",
        }
    end
    for index = 1, #M.required_hooks do
        local event = M.required_hooks[index]
        local groups = result.hooks[event]
        if groups ~= nil and (type(groups) ~= "table" or not common.is_array(groups)) then
            return nil, {
                code = "malformed_config",
                adapter = M.id,
                path = M.settings_path,
                field = "hooks." .. event,
                message = "Claude hook groups must be arrays",
            }
        end
        if type(groups) == "table" then
            local kept_groups = {}
            for group_index = 1, #groups do
                local group = groups[group_index]
                if type(group) == "table" and type(group.hooks) == "table" and common.is_array(group.hooks) then
                    local kept_hooks = {}
                    for hook_index = 1, #group.hooks do
                        if not is_owned_hook(group.hooks[hook_index], event) then
                            kept_hooks[#kept_hooks + 1] = common.copy(group.hooks[hook_index])
                        end
                    end
                    local copy_group = common.copy(group)
                    if #kept_hooks == 0 then
                        copy_group.hooks = nil
                    else
                        copy_group.hooks = kept_hooks
                    end
                    if copy_group.hooks ~= nil or next(copy_group) ~= nil then
                        kept_groups[#kept_groups + 1] = copy_group
                    end
                else
                    kept_groups[#kept_groups + 1] = common.copy(group)
                end
            end
            if #kept_groups == 0 then
                result.hooks[event] = nil
            else
                result.hooks[event] = kept_groups
            end
        end
    end
    if next(result.hooks) == nil then result.hooks = nil end
    return result
end

function M.expected_files(options, context)
    local merged, merge_error = M.merge_config(options and options.settings, options)
    if not merged then return nil, merge_error end
    local marker = common.marker_payload(M, context)
    local script = (options and options.hook_script) or M.hook_script_source
    if type(script) ~= "string" or script == "" then
        return nil, { code = "asset_missing", adapter = M.id, path = M.hook_script_path, message = "Claude hook script asset is unavailable" }
    end
    marker.owned_contents = { [M.hook_script_path] = script }
    return {
        { path = common.resolve_path(M.settings_path, context), operation = "write", value = merged, format = "json" },
        { path = common.resolve_path(M.hook_script_path, context), operation = "write", value = script, format = "text", executable = true },
        { path = common.resolve_path(M.marker_path, context), operation = "write", value = marker, format = "json" },
    }
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
        add("installed_version_missing", "warning", "Claude Code version was not supplied")
    elseif type(installed) == "string" and installed ~= M.version and installed:sub(1, #M.version) ~= M.version .. "." then
        add("installed_version_mismatch", "warning", "Claude Code version differs from the researched hook contract", { expected = M.version, actual = installed })
    end
    if observation.protocol_reachable == false or observation.reachable == false then
        add("protocol_unreachable", "error", "Claude stream-json protocol is not reachable")
    elseif observation.protocol_reachable == nil and observation.reachable == nil then
        add("protocol_unchecked", "warning", "Claude stream-json reachability was not supplied")
    end
    for index = 1, #M.required_hooks do
        local hook = M.required_hooks[index]
        if type(observation.hooks) ~= "table" or not observation.hooks[hook] then
            add("missing_hook", "error", "Claude hook is missing", { hook = hook })
        end
    end
    if type(observation.malformed_events) == "table" then
        for index = 1, #observation.malformed_events do add("malformed_event", "error", "Claude emitted a malformed event", { event = observation.malformed_events[index] }) end
    end
    if type(observation.stale_sessions) == "table" then
        for index = 1, #observation.stale_sessions do add("stale_session", "warning", "Claude session is stale", { session = observation.stale_sessions[index] }) end
    end
    if type(observation.controls) == "table" then
        for index = 1, #M.required_controls do
            local control = M.required_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "error", "Claude required control is unavailable", { control = control }) end
        end
        for index = 1, #M.optional_controls do
            local control = M.optional_controls[index]
            if observation.controls[control] == false then add("unsupported_control", "info", "Claude optional control is unavailable", { control = control, optional = true }) end
        end
    end
    local size = observation.settings_bytes or observation.config_bytes
    if type(size) == "number" and size > M.host_limits.max_settings_bytes then
        add("host_limit_exceeded", "error", "Claude settings exceed the 2 MiB host limit", { limit = M.host_limits.max_settings_bytes, actual = size })
    end
    return diagnostics
end

M.merge = M.merge_config
M.unmerge = M.remove_config
M.diagnose = M.doctor
return M
