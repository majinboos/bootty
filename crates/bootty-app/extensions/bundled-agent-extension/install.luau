--!strict
-- Bundled adapter installation workflows. Transforms are pure until apply() is invoked.
-- The host supplies generic file primitives; this module never shells out or prompts.

local function load_module(name)
    local ok, value = pcall(require, name)
    if ok then return value end
    local leaf = name:match("[^/]+$") or name
    local candidates = {
        "./installers/" .. leaf .. ".luau",
        "crates/bootty-app/extensions/bundled-agent-extension/installers/" .. leaf .. ".luau",
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
    error("unable to load installer module " .. tostring(name) .. ": " .. tostring(value))
end

local common = load_module("./installers/common")
local pi = load_module("./installers/pi")
local claude = load_module("./installers/claude")
local codex = load_module("./installers/codex")
local opencode = load_module("./installers/opencode")

local M = {}
local adapters = { pi = pi, claude = claude, codex = codex, opencode = opencode }
local adapter_order = { "pi", "claude", "codex", "opencode" }
local action_order = { "install", "setup", "uninstall", "doctor", "list" }

local function runtime_for(options)
    options = options or {}
    if options.host ~= nil then return options.host end
    if options.runtime ~= nil then return options.runtime end
    if type(bootty) == "table" then return bootty end
    return {}
end

local function files_for(host)
    if type(host) == "table" and type(host.files) == "table" then return host.files end
    return {}
end

local function storage_for(host)
    if type(host) == "table" and type(host.storage) == "table" then return host.storage end
    return nil
end

local function call(object, method, ...)
    if type(object) ~= "table" then return false, "operation unavailable" end
    local fn = object[method]
    if type(fn) ~= "function" then return false, "operation unavailable" end
    local ok, first, second = pcall(fn, ...)
    if ok then return true, first, second end
    local retry_ok, retry_first, retry_second = pcall(fn, object, ...)
    if retry_ok then return true, retry_first, retry_second end
    return false, tostring(first)
end

local function normalize_read(value)
    if type(value) == "table" then
        if value.content ~= nil then return value.content end
        if value.text ~= nil then return value.text end
        if value.value ~= nil then return value.value end
        if value.data ~= nil then return value.data end
    end
    return value
end

-- Minimal JSON decoder for fixture runners. Hosts may provide a stricter decoder.
local function json_decode_local(text)
    if type(text) ~= "string" then return nil, "JSON input must be text" end
    local index, length = 1, #text
    local function skip_space()
        while index <= length do
            local character = text:sub(index, index)
            if character == " " or character == "\t" or character == "\r" or character == "\n" then index = index + 1 else break end
        end
    end
    local parse_value
    local function parse_string()
        if text:sub(index, index) ~= '"' then return nil, "expected JSON string" end
        index = index + 1
        local output = {}
        while index <= length do
            local character = text:sub(index, index)
            if character == '"' then index = index + 1; return table.concat(output) end
            if character == "\\" then
                index = index + 1
                local escape = text:sub(index, index)
                local escapes = { ['"'] = '"', ["\\"] = "\\", ["/"] = "/", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t" }
                if escapes[escape] ~= nil then
                    output[#output + 1] = escapes[escape]
                    index = index + 1
                elseif escape == "u" then
                    local hexadecimal = text:sub(index + 1, index + 4)
                    if #hexadecimal ~= 4 or not hexadecimal:match("^[0-9a-fA-F]+$") then return nil, "invalid JSON unicode escape" end
                    local code = tonumber(hexadecimal, 16)
                    if code < 128 then output[#output + 1] = string.char(code)
                    elseif code < 2048 then output[#output + 1] = string.char(192 + math.floor(code / 64), 128 + code % 64)
                    else output[#output + 1] = string.char(224 + math.floor(code / 4096), 128 + math.floor(code / 64) % 64, 128 + code % 64) end
                    index = index + 5
                else return nil, "invalid JSON escape" end
            else output[#output + 1] = character; index = index + 1 end
        end
        return nil, "unterminated JSON string"
    end
    local function parse_number()
        local start = index
        if text:sub(index, index) == "-" then index = index + 1 end
        while text:sub(index, index):match("%d") do index = index + 1 end
        if text:sub(index, index) == "." then
            index = index + 1
            while text:sub(index, index):match("%d") do index = index + 1 end
        end
        local exponent = text:sub(index, index)
        if exponent == "e" or exponent == "E" then
            index = index + 1
            local sign = text:sub(index, index)
            if sign == "+" or sign == "-" then index = index + 1 end
            while text:sub(index, index):match("%d") do index = index + 1 end
        end
        local number = tonumber(text:sub(start, index - 1))
        if number == nil then return nil, "invalid JSON number" end
        return number
    end
    local function parse_array()
        index = index + 1
        local result = {}
        skip_space()
        if text:sub(index, index) == "]" then index = index + 1; return result end
        while index <= length do
            local value, err = parse_value()
            if err ~= nil then return nil, err end
            result[#result + 1] = value
            skip_space()
            local separator = text:sub(index, index)
            if separator == "]" then index = index + 1; return result end
            if separator ~= "," then return nil, "expected JSON array separator" end
            index = index + 1; skip_space()
        end
        return nil, "unterminated JSON array"
    end
    local function parse_object()
        index = index + 1
        local result = {}
        skip_space()
        if text:sub(index, index) == "}" then index = index + 1; return result end
        while index <= length do
            local key, key_error = parse_string()
            if key_error ~= nil then return nil, key_error end
            skip_space()
            if text:sub(index, index) ~= ":" then return nil, "expected JSON object colon" end
            index = index + 1; skip_space()
            local value, value_error = parse_value()
            if value_error ~= nil then return nil, value_error end
            result[key] = value
            skip_space()
            local separator = text:sub(index, index)
            if separator == "}" then index = index + 1; return result end
            if separator ~= "," then return nil, "expected JSON object separator" end
            index = index + 1; skip_space()
        end
        return nil, "unterminated JSON object"
    end
    parse_value = function()
        skip_space()
        local character = text:sub(index, index)
        if character == '"' then return parse_string()
        elseif character == "{" then return parse_object()
        elseif character == "[" then return parse_array()
        elseif text:sub(index, index + 3) == "true" then index = index + 4; return true
        elseif text:sub(index, index + 4) == "false" then index = index + 5; return false
        elseif text:sub(index, index + 3) == "null" then index = index + 4; return nil
        else return parse_number() end
    end
    local result, error_message = parse_value()
    if error_message ~= nil then return nil, error_message end
    skip_space()
    if index <= length then return nil, "trailing JSON data" end
    return result
end

local function decode_value(host, raw, path)
    if raw == nil or raw == "" then return {}, nil end
    if type(raw) == "table" then return common.copy(raw), nil end
    if type(raw) ~= "string" then return nil, "configuration is neither text nor an object" end
    local candidates = {}
    if type(host) == "table" then
        if type(host.json_decode) == "function" then candidates[#candidates + 1] = host.json_decode end
        if type(host.decode_json) == "function" then candidates[#candidates + 1] = host.decode_json end
        if type(host.json) == "table" and type(host.json.decode) == "function" then candidates[#candidates + 1] = host.json.decode end
    end
    for index = 1, #candidates do
        local ok, result, err = pcall(candidates[index], raw)
        if ok and result ~= nil then return result, nil end
        if ok and type(err) == "string" then return nil, err end
    end
    local result, err = json_decode_local(raw)
    if result == nil then return nil, "malformed JSON at " .. tostring(path) .. ": " .. tostring(err) end
    if type(result) ~= "table" then return nil, "configuration at " .. tostring(path) .. " must be an object" end
    return result, nil
end

local function encode_value(host, value, format, path)
    if type(value) == "string" then return value end
    local candidates = {}
    if type(host) == "table" then
        if type(host.json_encode) == "function" then candidates[#candidates + 1] = host.json_encode end
        if type(host.encode_json) == "function" then candidates[#candidates + 1] = host.encode_json end
        if type(host.json) == "table" and type(host.json.encode) == "function" then candidates[#candidates + 1] = host.json.encode end
    end
    for index = 1, #candidates do
        local ok, result = pcall(candidates[index], value)
        if ok and type(result) == "string" then return result end
    end
    if format == "json" then
        local ok, result = pcall(common.json_encode, value)
        if ok then return result .. "\n" end
        return nil, "unable to encode JSON for " .. tostring(path) .. ": " .. tostring(result)
    end
    return tostring(value), nil
end

local function path_exists(files, path)
    local ok, value = call(files, "exists", path)
    if ok then return value == true end
    local ok_read, raw = call(files, "read", path)
    return ok_read and raw ~= nil
end

local function read_file(host, path)
    local files = files_for(host)
    local exists = path_exists(files, path)
    if not exists then return nil, false, nil end
    local ok, value, error_message = call(files, "read", path)
    if not ok then return nil, true, tostring(value) end
    value = normalize_read(value)
    if value == nil then return nil, true, tostring(error_message or "file read returned no content") end
    return value, true, nil
end

local function option_copy(options)
    local result = {}
    if type(options) == "table" then for key, value in pairs(options) do result[key] = common.copy(value) end end
    return result
end

local function context_for(options, host)
    local context = option_copy((options and options.context) or {})
    if context.home == nil and type(host) == "table" then context.home = host.home or host.home_dir end
    if context.config_dir == nil and type(host) == "table" then context.config_dir = host.config_dir end
    if context.now == nil and type(options) == "table" then context.now = options.now end
    return context
end

local function get_adapter(adapter_id)
    if type(adapter_id) == "table" then return adapter_id end
    return adapters[adapter_id]
end

local function config_option_for(adapter, options, host, path)
    local context = context_for(options, host)
    local configured_path = options and options.config_path or adapter.settings_path
    if type(options) == "table" and options.settings ~= nil and path == common.resolve_path(configured_path, context) then return options.settings, nil end
    local raw, exists, read_error = read_file(host, path)
    if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
    if not exists then return {}, nil end
    local value, decode_error = decode_value(host, raw, path)
    if decode_error ~= nil then return nil, { code = "malformed_config", path = path, message = decode_error } end
    return value, nil
end

local function marker_option(adapter, options, host, context)
    if type(options) == "table" and options.marker ~= nil then return options.marker, nil end
    local path = common.resolve_path(adapter.marker_path, context)
    local raw, exists, read_error = read_file(host, path)
    if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
    if not exists then return nil, { code = "ownership_marker_missing", adapter = adapter.id, path = path, message = "matching ownership marker is missing" } end
    local value, decode_error = decode_value(host, raw, path)
    if decode_error ~= nil then return nil, { code = "malformed_marker", adapter = adapter.id, path = path, message = decode_error } end
    return value, nil
end

local function desired_specs(adapter, action, options, host, context)
    local working = option_copy(options)
    if action == "install" or action == "setup" then
        local settings_path = options and options.config_path or adapter.settings_path
        if options == nil or options.config_path == nil then
            local files = files_for(host)
            for candidate_index = 1, #(adapter.config_paths or {}) do
                local candidate = common.resolve_path(adapter.config_paths[candidate_index], context)
                if path_exists(files, candidate) then settings_path = adapter.config_paths[candidate_index]; break end
            end
        end
        settings_path = common.resolve_path(settings_path, context)
        working.config_path = working.config_path or (options and options.config_path) or settings_path
        if common.ends_with(settings_path, ".toml") then
            local inline_raw, inline_exists, inline_error = read_file(host, settings_path)
            if inline_error ~= nil then return nil, { code = "read_failed", path = settings_path, message = inline_error } end
            working.inline_config = working.inline_config or (inline_exists and inline_raw or "")
            working.settings = {}
        elseif working.settings == nil then
            local settings, settings_error = config_option_for(adapter, working, host, settings_path)
            if settings_error ~= nil then return nil, settings_error end
            working.settings = settings
        end
        if adapter.id == "codex" and working.inline_config == nil then
            local inline_path = common.resolve_path(adapter.inline_config_path, context)
            local inline_raw, inline_exists, inline_error = read_file(host, inline_path)
            if inline_error ~= nil then return nil, { code = "read_failed", path = inline_path, message = inline_error } end
            if inline_exists then working.inline_config = inline_raw end
        end
        local specs, error_value = adapter.expected_files(working, context)
        if specs == nil then return nil, error_value end
        return specs, nil
    end
    return {}, nil
end

local function make_change_for_spec(host, spec, context)
    local path = common.resolve_path(spec.path, context)
    local raw, exists, read_error = read_file(host, path)
    if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
    local before, after = raw, nil
    if spec.operation == "remove" then after = nil
    else
        local encoded, encode_error = encode_value(host, spec.value, spec.format, path)
        if encode_error ~= nil then return nil, { code = "encode_failed", path = path, message = encode_error } end
        after = encoded
    end
    return common.make_change(path, spec.operation, before, after, { format = spec.format, value = spec.value, existed = exists }), nil
end

local function owned_path(marker, path)
    if type(marker) ~= "table" or type(marker.files) ~= "table" then return false end
    for index = 1, #marker.files do if marker.files[index] == path then return true end end
    return false
end

function M.confirmation_context(action, adapter, changes)
    local paths, changed = {}, false
    for index = 1, #(changes or {}) do
        if changes[index].changed then changed = true; paths[#paths + 1] = changes[index].path end
    end
    if not changed then return nil end
    return { kind = "shared_dispatcher", command = "agents.integration." .. action, action = action, adapter = adapter.id, destructive = action == "uninstall", paths = paths }
end

local function build_install_plan(adapter, action, options, host, context)
    local specs, spec_error = desired_specs(adapter, action, options, host, context)
    if specs == nil then return nil, spec_error end
    local changes = {}
    for index = 1, #specs do
        local change, change_error = make_change_for_spec(host, specs[index], context)
        if change == nil then return nil, change_error end
        changes[#changes + 1] = change
    end
    local order = {}
    for index = 1, #(adapter.config_order or {}) do order[index] = common.resolve_path(adapter.config_order[index], context) end
    common.sort_changes(changes, order)
    local changed = false
    for index = 1, #changes do if changes[index].changed then changed = true end end
    return { action = action, adapter = adapter.id, version = adapter.version, changes = changes, changed = changed, preview = common.format_preview(changes), exact = true, transactional = true, confirmation = M.confirmation_context(action, adapter, changes) }, nil
end

local function build_uninstall_plan(adapter, options, host, context)
    local marker, marker_error = marker_option(adapter, options, host, context)
    if marker == nil then
        return { action = "uninstall", adapter = adapter.id, version = adapter.version, changes = {}, changed = false, preview = "", exact = true, transactional = true, diagnostics = { marker_error }, confirmation = nil }, nil
    end
    if not common.marker_matches(marker, adapter) then
        return { action = "uninstall", adapter = adapter.id, version = adapter.version, changes = {}, changed = false, preview = "", exact = true, transactional = true, diagnostics = {{ code = "ownership_marker_mismatch", adapter = adapter.id, message = "ownership marker belongs to another adapter or generation" }}, confirmation = nil }, nil
    end
    local configured_path = options and options.config_path
    if configured_path == nil and type(marker.files) == "table" then
        for marker_index = 1, #marker.files do
            if marker.files[marker_index] ~= adapter.marker_path then configured_path = marker.files[marker_index]; break end
        end
    end
    local settings_path = common.resolve_path(configured_path or adapter.settings_path, context)
    local settings, settings_error
    if common.ends_with(settings_path, ".toml") then
        local raw, exists, read_error = read_file(host, settings_path)
        if read_error ~= nil then return nil, { code = "read_failed", path = settings_path, message = read_error } end
        settings = exists and raw or ""
    else
        settings, settings_error = config_option_for(adapter, options, host, settings_path)
    end
    local changes = {}
    if not common.ends_with(settings_path, ".toml") then
        local removed, remove_error = adapter.remove_config(settings, marker, options)
        if removed == nil then return nil, remove_error end
        local config_change, config_change_error = make_change_for_spec(host, { path = settings_path, operation = "write", value = removed, format = "json" }, context)
        if config_change == nil then return nil, config_change_error end
        changes[#changes + 1] = config_change
    else
        local cleaned, clean_error = adapter.remove_toml(settings, marker)
        if cleaned == nil then return nil, clean_error end
        changes[#changes + 1] = common.make_change(settings_path, "write", settings, cleaned, { owned = true, safe = true })
    end
    local owned_files = marker.files or {}
    for index = 1, #owned_files do
        local path = common.resolve_path(owned_files[index], context)
        if path ~= settings_path and path ~= common.resolve_path(adapter.marker_path, context) then
            local current, exists, read_error = read_file(host, path)
            if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
            if exists and owned_path(marker, owned_files[index]) then
                if adapter.id == "codex" and owned_files[index] == adapter.inline_config_path then
                    local cleaned, clean_error = adapter.remove_toml(current, marker)
                    if cleaned == nil then
                        changes[#changes + 1] = common.make_change(path, "keep", current, current, { diagnostic = clean_error })
                    else
                        changes[#changes + 1] = common.make_change(path, "write", current, cleaned, { owned = true, safe = true })
                    end
                else
                    local expected = options and options.generated_files and options.generated_files[path]
                    if expected == nil and type(marker.owned_contents) == "table" then expected = marker.owned_contents[owned_files[index]] or marker.owned_contents[path] end
                    if expected ~= nil and current == expected then
                        changes[#changes + 1] = common.make_change(path, "remove", current, nil, { owned = true, safe = true })
                    else
                        changes[#changes + 1] = common.make_change(path, "keep", current, current, { owned = true, safe = false, diagnostic = { code = "owned_file_changed", path = path, message = "owned generated file changed or cannot be verified; preserving it" } })
                    end
                end
            end
        end
    end
    local marker_path = common.resolve_path(adapter.marker_path, context)
    local marker_raw, marker_exists, marker_read_error = read_file(host, marker_path)
    if marker_read_error ~= nil then return nil, { code = "read_failed", path = marker_path, message = marker_read_error } end
    if marker_exists then changes[#changes + 1] = common.make_change(marker_path, "remove", marker_raw, nil, { owned = true, safe = true }) end
    local order = {}
    for index = 1, #(adapter.config_order or {}) do order[index] = common.resolve_path(adapter.config_order[index], context) end
    common.sort_changes(changes, order)
    local changed, diagnostics = false, {}
    for index = 1, #changes do
        if changes[index].changed then changed = true end
        if changes[index].diagnostic ~= nil then diagnostics[#diagnostics + 1] = changes[index].diagnostic end
    end
    return { action = "uninstall", adapter = adapter.id, version = adapter.version, changes = changes, changed = changed, preview = common.format_preview(changes), exact = true, transactional = true, diagnostics = diagnostics, confirmation = M.confirmation_context("uninstall", adapter, changes) }, nil
end
function M.plan(action, adapter_id, options)
    options = options or {}
    local adapter = get_adapter(adapter_id)
    if adapter == nil then return nil, { code = "unsupported_adapter", adapter = adapter_id, message = "adapter must be pi, claude, codex, or opencode" } end
    local host = runtime_for(options)
    local context = context_for(options, host)
    if action == "list" then return M.list(options), nil end
    if action == "doctor" then return M.doctor(adapter.id, options.observation or options, options), nil end
    if action ~= "install" and action ~= "setup" and action ~= "uninstall" then return nil, { code = "unsupported_action", action = action, message = "action must be list, install, setup, uninstall, or doctor" } end
    if action == "uninstall" then return build_uninstall_plan(adapter, options, host, context) end
    return build_install_plan(adapter, action, options, host, context)
end

local function confirmation_matches(expected, supplied)
    if supplied == true then return true end
    if type(supplied) ~= "table" or type(expected) ~= "table" then return false end
    return supplied.command == expected.command and supplied.action == expected.action and supplied.adapter == expected.adapter
end

local function invoke_write(files, path, content)
    local ok, result, error_message = call(files, "write", path, content)
    if not ok or result == false then return false, tostring(error_message or result or "write failed") end
    return true, nil
end

local function invoke_remove(files, path)
    local ok, result, error_message = call(files, "remove", path)
    if not ok or result == false then return false, tostring(error_message or result or "remove failed") end
    return true, nil
end

local function rollback(files, backups, applied)
    local errors = {}
    for index = #applied, 1, -1 do
        local path = applied[index]
        local backup = backups[path]
        local ok, error_message
        if backup.exists then ok, error_message = invoke_write(files, path, backup.content) else ok, error_message = invoke_remove(files, path) end
        if not ok then errors[#errors + 1] = { path = path, message = error_message } end
    end
    return errors
end

function M.apply(plan, options)
    options = options or {}
    if type(plan) ~= "table" then return nil, { code = "invalid_plan", message = "apply requires a plan" } end
    if not plan.changed then return { status = "noop", plan = plan, applied = false, rolled_back = false }, nil end
    local supplied = options.confirmation or options.confirmation_context
    if not confirmation_matches(plan.confirmation, supplied) and options.confirmed ~= true then return { status = "confirmation_required", plan = plan, applied = false, confirmation = plan.confirmation }, nil end
    local host, files = runtime_for(options), nil
    files = files_for(host)
    local backups, actions = {}, {}
    for index = 1, #plan.changes do
        local change = plan.changes[index]
        if change.changed and change.operation ~= "keep" then
            local raw, exists, read_error = read_file(host, change.path)
            if read_error ~= nil then return nil, { code = "read_failed", path = change.path, message = read_error } end
            backups[change.path] = { exists = exists, content = raw }
            actions[#actions + 1] = { path = change.path, operation = change.operation, content = change.after, before = raw }
        end
    end
    if #actions == 0 then return { status = "noop", plan = plan, applied = false, rolled_back = false }, nil end
    local preview_value
    local preview_ok, preview_result = call(files, "preview", actions)
    if preview_ok then preview_value = preview_result end
    local applied = {}
    local has_native_apply = type(files.apply) == "function"
    if has_native_apply then
        local apply_ok, apply_result = call(files, "apply", actions)
        if not apply_ok or apply_result == false then
            for index = 1, #actions do applied[#applied + 1] = actions[index].path end
            local rollback_errors = rollback(files, backups, applied)
            return {
                status = "failed",
                plan = plan,
                applied = false,
                rolled_back = #rollback_errors == 0,
                rollback_errors = rollback_errors,
                message = "atomic host apply failed",
            }, nil
        end
        for index = 1, #actions do applied[#applied + 1] = actions[index].path end
    else
        for index = 1, #actions do
            local action = actions[index]
            local ok, error_message
            if action.operation == "remove" then ok, error_message = invoke_remove(files, action.path) else ok, error_message = invoke_write(files, action.path, action.content) end
            if not ok then
                local rollback_errors = rollback(files, backups, applied)
                return { status = "failed", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, failed_path = action.path, message = error_message }, nil
            end
            applied[#applied + 1] = action.path
        end
    end
    local storage = storage_for(host)
    if storage ~= nil and plan.adapter ~= nil then
        local marker = { adapter = plan.adapter, version = plan.version, action = plan.action, paths = {} }
        for index = 1, #plan.changes do if plan.changes[index].changed then marker.paths[#marker.paths + 1] = plan.changes[index].path end end
        call(storage, "set", "agents.integration." .. plan.adapter, marker)
    end
    return { status = "applied", plan = plan, applied = true, rolled_back = false, paths = applied, host_preview = preview_value }, nil
end

function M.list(options)
    options = options or {}
    local host, context = runtime_for(options), nil
    context = context_for(options, host)
    local result = {}
    for index = 1, #adapter_order do
        local id, adapter = adapter_order[index], adapters[adapter_order[index]]
        local marker_path = common.resolve_path(adapter.marker_path, context)
        local _, exists = read_file(host, marker_path)
        result[#result + 1] = { id = id, adapter = id, name = adapter.name, version = adapter.version, expected_version = adapter.expected_version, protocol = common.copy(adapter.protocol), installed = exists, settings_path = common.resolve_path(adapter.settings_path, context), marker_path = marker_path, required_hooks = common.array_copy(adapter.required_hooks), required_plugins = common.array_copy(adapter.required_plugins), controls = common.array_copy(adapter.required_controls), host_limits = common.copy(adapter.host_limits) }
    end
    return result
end

local function observation_from_files(adapter, options, host, context)
    local observation = option_copy(options and options.observation or {})
    local settings_path = options and options.config_path or adapter.settings_path
    if options == nil or options.config_path == nil then
        local files = files_for(host)
        for candidate_index = 1, #(adapter.config_paths or {}) do
            local candidate = common.resolve_path(adapter.config_paths[candidate_index], context)
            if path_exists(files, candidate) then settings_path = adapter.config_paths[candidate_index]; break end
        end
    end
    settings_path = common.resolve_path(settings_path, context)
    local settings, settings_error
    if common.ends_with(settings_path, ".toml") then
        local raw, exists, read_error = read_file(host, settings_path)
        if read_error ~= nil then observation.malformed_config = read_error; observation.config_error = { code = "read_failed", path = settings_path, message = read_error }; return observation end
        settings = { __toml = exists and raw or "" }
    else
        settings, settings_error = config_option_for(adapter, options, host, settings_path)
        if settings_error ~= nil then observation.malformed_config = settings_error.message; observation.config_error = settings_error; return observation end
    end
    local marker, marker_error = marker_option(adapter, options, host, context)
    if marker ~= nil then observation.marker = marker else observation.marker_error = marker_error end
    observation.hooks, observation.plugins = observation.hooks or {}, observation.plugins or {}
    if adapter.id == "pi" then
        if type(settings.extensions) == "table" then for index = 1, #settings.extensions do if settings.extensions[index] == adapter.extension_path or common.ends_with(settings.extensions[index], "/bootty.ts") then observation.plugins["bootty.ts"] = true end end end
        for index = 1, #adapter.required_hooks do observation.hooks[adapter.required_hooks[index]] = observation.plugins["bootty.ts"] == true end
    elseif adapter.id == "opencode" then
        local plugins = settings.plugin
        if type(plugins) == "string" then plugins = { plugins } end
        if type(plugins) == "table" then for index = 1, #plugins do local value = plugins[index]; if value == adapter.plugin_spec or (type(value) == "table" and value[1] == adapter.plugin_spec) then observation.plugins["bootty.ts"] = true end end end
        observation.hooks.event = observation.plugins["bootty.ts"] == true; observation.hooks["permission.ask"] = observation.plugins["bootty.ts"] == true
    else
        local toml = settings.__toml
        for index = 1, #adapter.required_hooks do
            local hook = adapter.required_hooks[index]
            observation.hooks[hook] = settings[hook] ~= nil or (type(settings.hooks) == "table" and settings.hooks[hook] ~= nil) or (type(toml) == "string" and toml:find(hook, 1, true) ~= nil)
        end
    end
    local marker_path = common.resolve_path(adapter.marker_path, context)
    local _, marker_exists = read_file(host, marker_path)
    observation.marker_exists = marker_exists
    return observation
end

function M.doctor(adapter_id, observation, options)
    options = options or {}
    local adapter = get_adapter(adapter_id)
    if adapter == nil then return { { code = "unsupported_adapter", adapter = adapter_id, severity = "error", message = "unknown adapter" } } end
    local host, context = runtime_for(options), nil
    context = context_for(options, host)
    local combined = option_copy(observation or {})
    if options.read_files ~= false then
        local from_files = observation_from_files(adapter, options, host, context)
        for key, value in pairs(from_files) do if combined[key] == nil then combined[key] = value end end
    end
    local diagnostics = {}
    if combined.malformed_config ~= nil then diagnostics[#diagnostics + 1] = { code = "malformed_config", severity = "error", adapter = adapter.id, path = adapter.settings_path, message = combined.malformed_config } end
    if combined.marker_error ~= nil and combined.marker_error.code == "ownership_marker_missing" then diagnostics[#diagnostics + 1] = { code = "ownership_marker_missing", severity = "warning", adapter = adapter.id, path = adapter.marker_path, message = combined.marker_error.message } end
    local adapter_diagnostics = adapter.doctor(combined)
    for index = 1, #adapter_diagnostics do diagnostics[#diagnostics + 1] = adapter_diagnostics[index] end
    return diagnostics
end

function M.integration_list(options) return M.list(options) end
function M.integration_install(adapter_id, options) return M.plan("install", adapter_id, options) end
function M.integration_setup(adapter_id, options) return M.plan("setup", adapter_id, options) end
function M.integration_uninstall(adapter_id, options) return M.plan("uninstall", adapter_id, options) end
function M.integration_doctor(adapter_id, options) return M.doctor(adapter_id, options and options.observation, options or {}) end

M.adapters, M.adapter_order, M.action_order, M.manifests = adapters, adapter_order, action_order, adapters
M.merge = function(adapter_id, config, options)
    local adapter = get_adapter(adapter_id)
    if adapter == nil then return nil, { code = "unsupported_adapter", adapter = adapter_id } end
    return adapter.merge_config(config, options or {})
end
M.unmerge = function(adapter_id, config, marker, options)
    local adapter = get_adapter(adapter_id)
    if adapter == nil then return nil, { code = "unsupported_adapter", adapter = adapter_id } end
    return adapter.remove_config(config, marker, options or {})
end
M.plan_install = function(adapter_id, options) return M.plan("install", adapter_id, options) end
M.plan_setup = function(adapter_id, options) return M.plan("setup", adapter_id, options) end
M.plan_uninstall = function(adapter_id, options) return M.plan("uninstall", adapter_id, options) end
M.install = M.plan_install
M.setup = M.plan_setup
M.uninstall = M.plan_uninstall
M.list_integrations = M.integration_list
M.doctor_integration = M.integration_doctor
M.shared_confirmation = M.confirmation_context

return M
