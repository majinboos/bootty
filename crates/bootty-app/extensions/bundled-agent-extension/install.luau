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
    if type(value) ~= "table" then return value, {} end
    local metadata = {}
    for _, key in ipairs({ "identity", "id", "etag", "inode", "version", "revision", "generation", "mtime", "bytes", "exists" }) do
        if value[key] ~= nil then metadata[key] = value[key] end
    end
    if value.content ~= nil then return value.content, metadata end
    if value.text ~= nil then return value.text, metadata end
    if value.value ~= nil then return value.value, metadata end
    if value.data ~= nil then return value.data, metadata end
    if value.exists == false then return nil, metadata end
    return value, metadata
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

local function read_metadata(files, path)
    local metadata = {}
    for _, method in ipairs({ "stat", "metadata" }) do
        local ok, value = call(files, method, path)
        if ok and type(value) == "table" then
            for key, item in pairs(value) do metadata[key] = item end
        end
    end
    return metadata
end

local function read_file(host, path)
    local files = files_for(host)
    local exists_known, exists = false, false
    local exists_ok, exists_value = call(files, "exists", path)
    if exists_ok then
        exists_known, exists = true, exists_value == true
        if not exists then return nil, false, nil, {} end
    end
    local ok, value, error_message = call(files, "read", path)
    if not ok then
        if exists_known and not exists then return nil, false, nil, {} end
        return nil, exists_known and exists or true, tostring(value), {}
    end
    local content, metadata = normalize_read(value)
    if content == nil then
        if (type(metadata) == "table" and metadata.exists == false) or (exists_known and not exists) then
            return nil, false, nil, metadata
        end
        return nil, true, tostring(error_message or "file read returned no content"), metadata
    end
    local stat = read_metadata(files, path)
    for key, item in pairs(stat) do if metadata[key] == nil then metadata[key] = item end end
    return content, true, nil, metadata
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
    local raw, exists, read_error, metadata = read_file(host, path)
    if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
    local before, after = raw, nil
    if spec.operation == "remove" then after = nil
    else
        local encoded, encode_error = encode_value(host, spec.value, spec.format, path)
        if encode_error ~= nil then return nil, { code = "encode_failed", path = path, message = encode_error } end
        after = encoded
    end
    local before_identity, identity_source = common.identity(before, exists, metadata)
    local before_version = common.version(metadata)
    return common.make_change(path, spec.operation, before, after, {
        format = spec.format,
        value = spec.value,
        existed = exists,
        before_identity = before_identity,
        before_identity_source = identity_source,
        before_version = before_version,
        file_identity = before_identity,
        file_version = before_version,
    }), nil
end

local function snapshot_metadata(content, exists, metadata)
    local identity, source = common.identity(content, exists, metadata)
    return {
        existed = exists,
        before_identity = identity,
        before_identity_source = source,
        before_version = common.version(metadata),
        file_identity = identity,
        file_version = common.version(metadata),
    }
end
local function owned_path(marker, path)
    if type(marker) ~= "table" or type(marker.files) ~= "table" then return false end
    for index = 1, #marker.files do if marker.files[index] == path then return true end end
    return false
end
local function validate_marker_ownership(adapter, marker, context, configured_path)
    if not common.marker_matches(marker, adapter) then
        return false, { code = "ownership_marker_mismatch", adapter = adapter.id, message = "ownership marker belongs to another adapter or generation" }
    end
    if type(marker.files) ~= "table" or #marker.files == 0 then
        return false, { code = "ownership_marker_invalid", adapter = adapter.id, message = "ownership marker does not enumerate owned files" }
    end
    local marker_path = common.resolve_path(adapter.marker_path, context)
    local marker_seen = false
    for index = 1, #marker.files do
        local raw_path = marker.files[index]
        if type(raw_path) ~= "string" or not common.owned_path(adapter, raw_path, context, configured_path) then
            return false, { code = "ownership_marker_mismatch", adapter = adapter.id, path = raw_path, message = "ownership marker lists a path outside the adapter-owned set" }
        end
        if common.resolve_path(raw_path, context) == marker_path then marker_seen = true end
    end
    if not marker_seen then
        return false, { code = "ownership_marker_invalid", adapter = adapter.id, path = marker_path, message = "ownership marker does not own its marker file" }
    end
    if marker.version ~= nil and tostring(marker.version) ~= tostring(adapter.version) then
        return false, { code = "ownership_marker_mismatch", adapter = adapter.id, message = "ownership marker belongs to a different adapter version" }
    end
    return true, nil
end

function M.confirmation_context(action, adapter, changes, digest)
    local paths, changed = {}, false
    for index = 1, #(changes or {}) do
        if changes[index].changed then changed = true; paths[#paths + 1] = changes[index].path end
    end
    if not changed then return nil end
    return {
        kind = "shared_dispatcher",
        command = "agents.integration." .. action,
        action = action,
        adapter = adapter.id,
        version = adapter.version,
        destructive = action == "uninstall",
        paths = paths,
        digest = digest,
        plan_id = digest,
    }
end

local function finalize_plan(plan)
    local digest = common.change_fingerprint(plan.changes)
    local operations, identities = {}, {}
    for index = 1, #(plan.changes or {}) do
        local change = plan.changes[index]
        operations[#operations + 1] = {
            path = change.path,
            operation = change.operation,
            before = change.before,
            after = change.after,
            before_identity = change.before_identity,
            before_version = change.before_version,
        }
        identities[change.path] = {
            identity = change.before_identity,
            version = change.before_version,
            source = change.before_identity_source,
        }
    end
    plan.digest = digest
    plan.plan_id = digest
    plan.operations = operations
    plan.file_identities = identities
    plan.preview_bytes = #(plan.preview or "")
    plan.transaction = {
        transaction_id = plan.transaction_id,
        digest = digest,
        preview = plan.preview,
    }
    if plan.confirmation ~= nil then
        plan.confirmation.digest = digest
        plan.confirmation.plan_id = digest
        plan.confirmation.preview = plan.preview
        plan.confirmation.transaction_id = plan.transaction_id
    end
    return plan
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
    local plan = {
        action = action,
        adapter = adapter.id,
        transaction_id = options and options.transaction_id,
        version = adapter.version,
        changes = changes,
        changed = changed,
        preview = common.format_preview(changes),
        exact = true,
        bounded = true,
        transactional = true,
    }
    plan.confirmation = M.confirmation_context(action, adapter, changes)
    return finalize_plan(plan), nil
end

local function build_uninstall_plan(adapter, options, host, context)
    local marker, marker_error = marker_option(adapter, options, host, context)
    if marker == nil then
        return { action = "uninstall", adapter = adapter.id, version = adapter.version, changes = {}, changed = false, preview = "", exact = true, bounded = true, transactional = true, diagnostics = { marker_error }, confirmation = nil }, nil
    end
    local configured_path = options and options.config_path
    local owned, ownership_error = validate_marker_ownership(adapter, marker, context, configured_path)
    if not owned then
        return { action = "uninstall", adapter = adapter.id, version = adapter.version, changes = {}, changed = false, preview = "", exact = true, bounded = true, transactional = true, diagnostics = { ownership_error }, confirmation = nil }, nil
    end
    if configured_path == nil and type(marker.files) == "table" then
        for marker_index = 1, #marker.files do
            if marker.files[marker_index] ~= adapter.marker_path then configured_path = marker.files[marker_index]; break end
        end
    end
    local settings_path = common.resolve_path(configured_path or adapter.settings_path, context)
    local settings, settings_error, settings_metadata, settings_exists
    if common.ends_with(settings_path, ".toml") then
        local raw, exists, read_error, metadata = read_file(host, settings_path)
        if read_error ~= nil then return nil, { code = "read_failed", path = settings_path, message = read_error } end
        settings, settings_metadata, settings_exists = exists and raw or "", metadata, exists
    else
        settings, settings_error = config_option_for(adapter, options, host, settings_path)
        if settings_error ~= nil then return nil, settings_error end
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
        changes[#changes + 1] = common.make_change(settings_path, "write", settings, cleaned, common.merge({ owned = true, safe = true }, snapshot_metadata(settings, settings_exists == true, settings_metadata)))
    end
    local owned_files = marker.files or {}
    for index = 1, #owned_files do
        local path = common.resolve_path(owned_files[index], context)
        if path ~= settings_path and path ~= common.resolve_path(adapter.marker_path, context) then
            local current, exists, read_error, current_metadata = read_file(host, path)
            if read_error ~= nil then return nil, { code = "read_failed", path = path, message = read_error } end
            if exists and owned_path(marker, owned_files[index]) then
                local identity_metadata = snapshot_metadata(current, exists, current_metadata)
                if adapter.id == "codex" and owned_files[index] == adapter.inline_config_path then
                    local cleaned, clean_error = adapter.remove_toml(current, marker)
                    if cleaned == nil then
                        changes[#changes + 1] = common.make_change(path, "keep", current, current, common.merge({ diagnostic = clean_error }, identity_metadata))
                    else
                        changes[#changes + 1] = common.make_change(path, "write", current, cleaned, common.merge({ owned = true, safe = true }, identity_metadata))
                    end
                else
                    local expected = options and options.generated_files and options.generated_files[path]
                    if expected == nil and type(marker.owned_contents) == "table" then expected = marker.owned_contents[owned_files[index]] or marker.owned_contents[path] end
                    if expected ~= nil and current == expected then
                        changes[#changes + 1] = common.make_change(path, "remove", current, nil, common.merge({ owned = true, safe = true }, identity_metadata))
                    else
                        changes[#changes + 1] = common.make_change(path, "keep", current, current, common.merge({ owned = true, safe = false, diagnostic = { code = "owned_file_changed", path = path, message = "owned generated file changed or cannot be verified; preserving it" } }, identity_metadata))
                    end
                end
            end
        end
    end
    local marker_path = common.resolve_path(adapter.marker_path, context)
    local marker_raw, marker_exists, marker_read_error, marker_metadata = read_file(host, marker_path)
    if marker_read_error ~= nil then return nil, { code = "read_failed", path = marker_path, message = marker_read_error } end
    if marker_exists then changes[#changes + 1] = common.make_change(marker_path, "remove", marker_raw, nil, common.merge({ owned = true, safe = true }, snapshot_metadata(marker_raw, marker_exists, marker_metadata))) end
    local order = {}
    for index = 1, #(adapter.config_order or {}) do order[index] = common.resolve_path(adapter.config_order[index], context) end
    common.sort_changes(changes, order)
    local changed, diagnostics = false, {}
    for index = 1, #changes do
        if changes[index].changed then changed = true end
        if changes[index].diagnostic ~= nil then diagnostics[#diagnostics + 1] = changes[index].diagnostic end
    end
    local plan = { action = "uninstall", adapter = adapter.id, transaction_id = options and options.transaction_id, version = adapter.version, changes = changes, changed = changed, preview = common.format_preview(changes), exact = true, bounded = true, transactional = true, diagnostics = diagnostics, confirmation = M.confirmation_context("uninstall", adapter, changes) }
    return finalize_plan(plan), nil
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

local function confirmation_matches(files, expected, supplied)
    if type(expected) ~= "table" or type(supplied) ~= "table" or type(supplied.token) ~= "string" or supplied.token == "" then return false end
    local ok, accepted = call(files, "validate_confirmation", expected, supplied.token)
    return ok and accepted == true
end

local function invoke_write(files, path, content, precondition)
    local method = type(files.write_if_unchanged) == "function" and "write_if_unchanged" or nil
    if method == nil and type(files.compare_and_swap) == "function" then method = "compare_and_swap" end
    if method ~= nil then
        local ok, result, error_message
        if method == "compare_and_swap" then
            ok, result, error_message = call(files, method, path, precondition, content)
        else
            ok, result, error_message = call(files, method, path, content, precondition)
        end
        if not ok or result == false then return false, tostring(error_message or result or "compare-and-swap write failed") end
        return true, nil
    end
    local ok, result, error_message = call(files, "write", path, content, precondition)
    if not ok or result == false then return false, tostring(error_message or result or "write failed") end
    return true, nil
end

local function invoke_remove(files, path, precondition)
    local method = type(files.remove_if_unchanged) == "function" and "remove_if_unchanged" or nil
    if method == nil and type(files.compare_and_swap) == "function" then method = "compare_and_swap" end
    if method ~= nil then
        local ok, result, error_message
        if method == "compare_and_swap" then
            ok, result, error_message = call(files, method, path, precondition, nil)
        else
            ok, result, error_message = call(files, method, path, precondition)
        end
        if not ok or result == false then return false, tostring(error_message or result or "compare-and-swap remove failed") end
        return true, nil
    end
    local ok, result, error_message = call(files, "remove", path, precondition)
    if not ok or result == false then return false, tostring(error_message or result or "remove failed") end
    return true, nil
end

local function snapshot_matches(change, content, exists, metadata, after)
    local expected, expected_exists, expected_identity, expected_source, expected_version
    if after then
        expected, expected_exists = change.after, change.after ~= nil
        expected_identity = change.after_identity
        expected_source = change.after_identity_source
        expected_version = change.after_version
    else
        expected, expected_exists = change.before, change.existed == true
        expected_identity = change.before_identity
        expected_source = change.before_identity_source
        expected_version = change.before_version
    end
    if not common.same_content(expected, content) or expected_exists ~= exists then return false end
    if expected_identity ~= nil and expected_source == "host" then
        local identity, source = common.identity(content, exists, metadata)
        if source ~= "host" or identity ~= expected_identity then return false end
    end
    if expected_version ~= nil and common.version(metadata) ~= expected_version then return false end
    return true
end

local function rollback(files, records)
    local errors = {}
    for index = #records, 1, -1 do
        local record = records[index]
        local backup = record.backup
        local current, exists, read_error, metadata = read_file({ files = files }, record.action.path)
        if read_error ~= nil then
            errors[#errors + 1] = { path = record.action.path, code = "rollback_read_failed", message = read_error }
        elseif snapshot_matches(record.action, current, exists, metadata, false) then
            -- The failed action did not change this path; there is nothing to restore.
        elseif not snapshot_matches(record.action, current, exists, metadata, true) then
            errors[#errors + 1] = { path = record.action.path, code = "rollback_conflict", conflict = true, message = "concurrent edit detected; preserving current content" }
        else
            local precondition = {
                before = record.action.after,
                existed = record.action.after ~= nil,
                before_identity = record.action.after_identity,
                before_identity_source = record.action.after_identity_source,
                before_version = record.action.after_version,
            }
            local ok, error_message
            if backup.exists then
                ok, error_message = invoke_write(files, record.action.path, backup.content, precondition)
            else
                ok, error_message = invoke_remove(files, record.action.path, precondition)
            end
            if not ok then
                errors[#errors + 1] = { path = record.action.path, code = "rollback_failed", message = error_message }
            end
        end
    end
    return errors
end

function M.apply(plan, options)
    options = options or {}
    if type(plan) ~= "table" or plan.exact ~= true or type(plan.plan_id) ~= "string" or type(plan.changes) ~= "table" then
        return nil, { code = "invalid_plan", message = "apply requires an exact preview plan" }
    end
    if not plan.changed then return { status = "noop", plan = plan, applied = false, rolled_back = false }, nil end
    local host, files = runtime_for(options), nil
    files = files_for(host)
    local supplied = options.confirmation
    local transaction = plan.transaction
    if type(transaction) ~= "table" or not confirmation_matches(files, transaction, supplied) then
        return { status = "confirmation_required", plan = plan, applied = false, confirmation = plan.confirmation }, nil
    end
    local token = supplied.token
    local backups, actions, records = {}, {}, {}
    for index = 1, #plan.changes do
        local change = plan.changes[index]
        if change.changed and change.operation ~= "keep" then
            local raw, exists, read_error, metadata = read_file(host, change.path)
            if read_error ~= nil then return nil, { code = "read_failed", path = change.path, message = read_error } end
            if not snapshot_matches(change, raw, exists, metadata, false) then
                return {
                    status = "stale",
                    plan = plan,
                    applied = false,
                    stale_paths = { { path = change.path, expected = change.before, actual = raw, expected_identity = change.before_identity, actual_identity = common.identity(raw, exists, metadata) } },
                }, nil
            end
            backups[change.path] = {
                exists = exists,
                content = raw,
                identity = change.before_identity,
                version = change.before_version,
            }
            local action = {
                path = change.path,
                operation = change.operation,
                content = change.after,
                before = change.before,
                existed = change.existed,
                before_identity = change.before_identity,
                before_version = change.before_version,
                after = change.after,
                after_identity = change.after_identity,
                after_identity_source = change.after_identity_source,
            }
            actions[#actions + 1] = action
            records[#records + 1] = { action = action, backup = backups[change.path] }
        end
    end
    if #actions == 0 then return { status = "noop", plan = plan, applied = false, rolled_back = false }, nil end
    local preview_value = nil
    local applied = {}
    local function verify_action(action)
        local current, exists, read_error, metadata = read_file(host, action.path)
        if read_error ~= nil then return false, read_error end
        if not snapshot_matches(action, current, exists, metadata, true) then return false, "file changed while applying" end
        action.after_identity, action.after_identity_source = common.identity(current, exists, metadata)
        action.after_version = common.version(metadata)
        return true, nil
    end
    if type(files.apply) == "function" then
        local apply_ok, apply_result = call(files, "apply", actions, token, { transaction_id = transaction.transaction_id, digest = transaction.digest })
        if not apply_ok or apply_result == false then
            local rollback_errors = rollback(files, records)
            return { status = "failed", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, message = "atomic host apply failed" }, nil
        end
        for index = 1, #actions do
            local verified, verify_error = verify_action(actions[index])
            if not verified then
                local rollback_errors = rollback(files, records)
                return { status = "failed", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, failed_path = actions[index].path, message = verify_error }, nil
            end
            applied[#applied + 1] = actions[index].path
        end
    else
        for index = 1, #actions do
            local action = actions[index]
            local current, exists, read_error, metadata = read_file(host, action.path)
            if read_error ~= nil or not snapshot_matches(action, current, exists, metadata, false) then
                local rollback_errors = rollback(files, records)
                return { status = "stale", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, stale_paths = { { path = action.path } } }, nil
            end
            local precondition = { before = action.before, existed = action.before ~= nil, before_identity = action.before_identity, before_identity_source = action.before_identity_source, before_version = action.before_version }
            local ok, error_message
            if action.operation == "remove" then ok, error_message = invoke_remove(files, action.path, precondition) else ok, error_message = invoke_write(files, action.path, action.content, precondition) end
            if not ok then
                local rollback_errors = rollback(files, records)
                return { status = "failed", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, failed_path = action.path, message = error_message }, nil
            end
            local verified, verify_error = verify_action(action)
            if not verified then
                local rollback_errors = rollback(files, records)
                return { status = "failed", plan = plan, applied = false, rolled_back = #rollback_errors == 0, rollback_errors = rollback_errors, failed_path = action.path, message = verify_error }, nil
            end
            applied[#applied + 1] = action.path
        end
    end
    local storage = storage_for(host)
    if storage ~= nil and plan.adapter ~= nil then
        local key = "agents.integration." .. plan.adapter
        if plan.action == "uninstall" and type(storage.remove) == "function" then
            call(storage, "remove", key)
        else
            local marker = { adapter = plan.adapter, version = plan.version, action = plan.action, paths = applied, plan_id = plan.plan_id }
            call(storage, "set", key, marker)
        end
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

local function integration_action(action, adapter_id, options)
    options = options or {}
    if options.plan ~= nil then return M.apply(options.plan, options) end
    local plan, plan_error = M.plan(action, adapter_id, options)
    if plan == nil or plan_error ~= nil then return plan, plan_error end
    if options.confirmation ~= nil then
        return M.apply(plan, options)
    end
    return plan, nil
end

function M.integration_list(options) return M.list(options) end
function M.integration_install(adapter_id, options) return integration_action("install", adapter_id, options) end
function M.integration_setup(adapter_id, options) return integration_action("setup", adapter_id, options) end
function M.integration_uninstall(adapter_id, options) return integration_action("uninstall", adapter_id, options) end
function M.integration_apply(adapter_id, plan, options)
    options = options or {}
    options.plan = plan
    return M.apply(plan, options)
end
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
