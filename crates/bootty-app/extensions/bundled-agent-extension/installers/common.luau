--!strict
-- Pure installer primitives shared by the bundled agent adapters.
-- The module intentionally uses ordinary Lua 5.4 syntax so fixture runners can load it.

local M = {}

local function is_table(value)
    return type(value) == "table"
end

function M.copy(value)
    if not is_table(value) then
        return value
    end
    local result = {}
    for key, item in pairs(value) do
        result[M.copy(key)] = M.copy(item)
    end
    return result
end

function M.array_copy(values)
    local result = {}
    if not is_table(values) then
        return result
    end
    for index = 1, #values do
        result[index] = M.copy(values[index])
    end
    return result
end

function M.is_array(value)
    if not is_table(value) then
        return false
    end
    local count, total = 0, 0
    for key, _ in pairs(value) do
        total = total + 1
        if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
            return false
        end
        count = count + 1
    end
    return total == 0 or (count == total and #value == count)
end

function M.equal(left, right)
    if type(left) ~= type(right) then
        return false
    end
    if not is_table(left) then
        return left == right
    end
    for key, value in pairs(left) do
        if not M.equal(value, right[key]) then
            return false
        end
    end
    for key, value in pairs(right) do
        if not M.equal(value, left[key]) then
            return false
        end
    end
    return true
end

local function key_order(keys)
    table.sort(keys, function(a, b)
        local ta, tb = type(a), type(b)
        if ta == tb then
            return tostring(a) < tostring(b)
        end
        return ta < tb
    end)
    return keys
end

-- A deterministic compact JSON encoder used for generated marker/config values.
-- It is deliberately small but handles the complete JSON value domain.
function M.json_encode(value)
    local value_type = type(value)
    if value == nil then
        return "null"
    elseif value_type == "boolean" then
        return value and "true" or "false"
    elseif value_type == "number" then
        if value ~= value or value == math.huge or value == -math.huge then
            error("cannot encode non-finite JSON number")
        end
        return string.format("%.17g", value)
    elseif value_type == "string" then
        local escaped = value:gsub("[%z\1-\31\\\"]", function(character)
            local byte = string.byte(character)
            if character == "\\" then
                return "\\\\"
            elseif character == '"' then
                return '\\"'
            elseif character == "\b" then
                return "\\b"
            elseif character == "\f" then
                return "\\f"
            elseif character == "\n" then
                return "\\n"
            elseif character == "\r" then
                return "\\r"
            elseif character == "\t" then
                return "\\t"
            end
            return string.format("\\u%04x", byte)
        end)
        return '"' .. escaped .. '"'
    elseif value_type == "table" then
        if M.is_array(value) then
            local items = {}
            for index = 1, #value do
                items[index] = M.json_encode(value[index])
            end
            return "[" .. table.concat(items, ",") .. "]"
        end
        local keys = {}
        for key, _ in pairs(value) do
            if type(key) ~= "string" then
                error("JSON object keys must be strings")
            end
            keys[#keys + 1] = key
        end
        key_order(keys)
        local fields = {}
        for index = 1, #keys do
            local key = keys[index]
            fields[index] = M.json_encode(key) .. ":" .. M.json_encode(value[key])
        end
        return "{" .. table.concat(fields, ",") .. "}"
    end
    error("cannot encode value of type " .. value_type)
end

-- Deep merge for configuration objects. Existing keys and array items are retained;
-- generated array entries are appended only when they are not already present.
function M.merge(base, overlay)
    local result = M.copy(base or {})
    if not is_table(overlay) then
        return result
    end
    for key, value in pairs(overlay) do
        if is_table(value) and is_table(result[key]) then
            if M.is_array(value) or M.is_array(result[key]) then
                local target = M.array_copy(result[key])
                for index = 1, #value do
                    local item = value[index]
                    local found = false
                    for target_index = 1, #target do
                        if M.equal(target[target_index], item) then
                            found = true
                            break
                        end
                    end
                    if not found then
                        target[#target + 1] = M.copy(item)
                    end
                end
                result[key] = target
            else
                result[key] = M.merge(result[key], value)
            end
        else
            result[key] = M.copy(value)
        end
    end
    return result
end

function M.contains(values, expected)
    if not is_table(values) then
        return false
    end
    for index = 1, #values do
        if M.equal(values[index], expected) then
            return true
        end
    end
    return false
end

function M.append_unique(values, value)
    local result = M.array_copy(values)
    if not M.contains(result, value) then
        result[#result + 1] = M.copy(value)
    end
    return result
end

function M.remove_exact(values, value)
    local result = {}
    local removed = false
    if not is_table(values) then
        return result, removed
    end
    for index = 1, #values do
        if M.equal(values[index], value) then
            removed = true
        else
            result[#result + 1] = M.copy(values[index])
        end
    end
    return result, removed
end

function M.starts_with(value, prefix)
    return type(value) == "string" and type(prefix) == "string" and value:sub(1, #prefix) == prefix
end

function M.ends_with(value, suffix)
    return type(value) == "string" and type(suffix) == "string" and suffix ~= "" and value:sub(-#suffix) == suffix
end

function M.path_join(left, right)
    if left == nil or left == "" then
        return right
    end
    if right == nil or right == "" then
        return left
    end
    if left:sub(-1) == "/" then
        return left .. right
    end
    return left .. "/" .. right
end

function M.resolve_path(path, context)
    if type(path) ~= "string" then
        return path
    end
    local home = context and (context.home or context.home_dir)
    local config_dir = context and (context.config_dir or context.config)
    if M.starts_with(path, "~/") and type(home) == "string" then
        return M.path_join(home, path:sub(3))
    end
    if M.starts_with(path, "$HOME/") and type(home) == "string" then
        return M.path_join(home, path:sub(7))
    end
    if M.starts_with(path, "$CONFIG/") and type(config_dir) == "string" then
        return M.path_join(config_dir, path:sub(9))
    end
    return path
end

-- A compact deterministic identity for hosts that do not expose inode/etag
-- metadata. The content itself remains the authoritative compare-and-swap value.
function M.content_identity(content, exists)
    local text = exists == false and "<missing>" or tostring(content or "")
    local hash = 2166136261
    for index = 1, #text do
        hash = (hash * 16777619 + string.byte(text, index)) % 4294967296
    end
    return string.format("content:%08x:%d", hash, #text)
end

function M.identity(content, exists, metadata)
    if type(metadata) == "table" then
        local identity = metadata.identity or metadata.id or metadata.etag or metadata.inode
        if identity ~= nil then return tostring(identity), "host" end
    end
    return M.content_identity(content, exists), "content"
end

function M.version(metadata)
    if type(metadata) ~= "table" then return nil end
    local value = metadata.version or metadata.revision or metadata.generation or metadata.mtime
    if value == nil then return nil end
    return tostring(value)
end

function M.same_content(left, right)
    if left == nil or right == nil then return left == right end
    return left == right
end

function M.change_fingerprint(changes)
    local rows = {}
    for index = 1, #(changes or {}) do
        local change = changes[index]
        rows[#rows + 1] = {
            path = change.path,
            operation = change.operation,
            before = change.before,
            after = change.after,
            before_identity = change.before_identity,
            before_identity_source = change.before_identity_source,
            before_version = change.before_version,
        }
    end
    return M.content_identity(M.json_encode(rows), true)
end

function M.quote(value)
    return M.json_encode(tostring(value))
end

local function line_diff(before, after)
    if before == after then
        return {}
    end
    local before_lines, after_lines = {}, {}
    if type(before) == "string" and before ~= "" then
        for line in (before .. "\n"):gmatch("(.-)\n") do
            before_lines[#before_lines + 1] = line
        end
    end
    if type(after) == "string" and after ~= "" then
        for line in (after .. "\n"):gmatch("(.-)\n") do
            after_lines[#after_lines + 1] = line
        end
    end
    local lines = { "--- " .. (before == nil and "/dev/null" or "before"), "+++ " .. (after == nil and "/dev/null" or "after") }
    local common = math.max(#before_lines, #after_lines)
    for index = 1, common do
        local old_line, new_line = before_lines[index], after_lines[index]
        if old_line == new_line then
            if old_line ~= nil then
                lines[#lines + 1] = " " .. old_line
            end
        else
            if old_line ~= nil then
                lines[#lines + 1] = "-" .. old_line
            end
            if new_line ~= nil then
                lines[#lines + 1] = "+" .. new_line
            end
        end
    end
    return lines
end

function M.make_change(path, operation, before, after, metadata)
    local change = {
        path = path,
        operation = operation,
        before = before,
        after = after,
        changed = not M.same_content(before, after),
        diff = line_diff(before, after),
        bounded = true,
        before_bytes = type(before) == "string" and #before or 0,
        after_bytes = type(after) == "string" and #after or 0,
    }
    if type(metadata) == "table" then
        for key, value in pairs(metadata) do
            change[key] = value
        end
    end
    local before_exists = change.existed
    if before_exists == nil then before_exists = before ~= nil end
    if change.before_identity == nil then
        local identity, source = M.identity(before, before_exists, change)
        change.before_identity, change.before_identity_source = identity, source
    end
    if change.before_version == nil then
        change.before_version = M.version(change)
    end
    if change.after_identity == nil then
        change.after_identity = M.content_identity(after, after ~= nil)
        change.after_identity_source = "content"
    end
    return change
end
function M.owned_path(manifest, path, context, configured_path)
    if type(path) ~= "string" then return false end
    local expected = {}
    local function add(candidate)
        if type(candidate) == "string" then expected[M.resolve_path(candidate, context)] = true end
    end
    add(configured_path)
    add(manifest.settings_path)
    add(manifest.marker_path)
    for _, candidate in ipairs(manifest.config_paths or {}) do add(candidate) end
    for _, candidate in ipairs(manifest.owned_paths or {}) do add(candidate) end
    return expected[M.resolve_path(path, context)] == true
end

function M.sort_changes(changes, order)
    local rank = {}
    if type(order) == "table" then
        for index = 1, #order do
            rank[order[index]] = index
        end
    end
    table.sort(changes, function(left, right)
        local left_rank = rank[left.path] or 1000000
        local right_rank = rank[right.path] or 1000000
        if left_rank ~= right_rank then
            return left_rank < right_rank
        end
        return tostring(left.path) < tostring(right.path)
    end)
    return changes
end

function M.format_preview(changes)
    local chunks = {}
    for index = 1, #changes do
        local change = changes[index]
        if change.changed then
            chunks[#chunks + 1] = table.concat(change.diff or {}, "\n")
        end
    end
    return table.concat(chunks, "\n")
end

function M.marker_payload(manifest, context)
    local marker = {
        schema = 1,
        owner = "bootty.agents",
        adapter = manifest.id,
        version = manifest.version,
        marker = manifest.marker,
        files = M.array_copy(manifest.owned_paths or {}),
    }
    return marker
end

function M.marker_matches(marker, manifest)
    return type(marker) == "table"
        and marker.owner == "bootty.agents"
        and marker.adapter == manifest.id
        and marker.marker == manifest.marker
end

function M.value_or(value, fallback)
    if value == nil then
        return fallback
    end
    return value
end

function M.safe_call(fn, ...)
    if type(fn) ~= "function" then
        return false, "operation unavailable"
    end
    local ok, first, second = pcall(fn, ...)
    if not ok then
        return false, tostring(first)
    end
    return true, first, second
end

return M
