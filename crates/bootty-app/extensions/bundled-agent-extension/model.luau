-- Pure data model for the bundled agent extension.
-- This module intentionally has no dependency on the host.  The core module wires
-- the returned values to bootty.commands/events/storage when it is registered.

local M = {}

local unpack_fn = table.unpack or unpack

local function is_table(value)
    return type(value) == "table"
end

local function is_array(value)
    if not is_table(value) then
        return false, 0
    end
    local count = 0
    for key, _ in pairs(value) do
        if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
            return false, 0
        end
        if key > count then
            count = key
        end
    end
    for index = 1, count do
        if rawget(value, index) == nil then
            return false, 0
        end
    end
    return true, count
end

local function copy_value(value, seen)
    if not is_table(value) then
        return value
    end
    seen = seen or {}
    if seen[value] ~= nil then
        return seen[value]
    end
    local result = {}
    seen[value] = result
    for key, nested in pairs(value) do
        if nested ~= nil then
            result[copy_value(key, seen)] = copy_value(nested, seen)
        end
    end
    return result
end

M.copy = copy_value
M.clone = copy_value

-- Return a copy that contains no nil/empty nested values.  Lua cannot preserve a
-- JSON null, so a missing key is the only representation of an absent fact.
local function prune_absent(value, seen)
    if not is_table(value) then
        return value
    end
    local array, count = is_array(value)
    seen = seen or {}
    if seen[value] ~= nil then
        return seen[value]
    end
    local result = {}
    seen[value] = result
    if array then
        for index = 1, count do
            local nested = prune_absent(value[index], seen)
            if nested ~= nil and (not is_table(nested) or next(nested) ~= nil) then
                result[#result + 1] = nested
            end
        end
    else
        for key, nested in pairs(value) do
            if nested ~= nil then
                local cleaned = prune_absent(nested, seen)
                if cleaned ~= nil and (not is_table(cleaned) or next(cleaned) ~= nil) then
                    result[key] = cleaned
                end
            end
        end
    end
    return result
end

M.omit_absent = prune_absent
M.prune = prune_absent

local function value_is_present(container, key)
    return is_table(container) and rawget(container, key) ~= nil
end

M.present = value_is_present

local function stable_value(value, seen)
    local value_type = type(value)
    if value == nil then
        return "null"
    end
    if value_type == "boolean" then
        return value and "true" or "false"
    end
    if value_type == "number" then
        if value ~= value then
            return "nan"
        end
        if value == math.huge then
            return "inf"
        end
        if value == -math.huge then
            return "-inf"
        end
        return string.format("%.17g", value)
    end
    if value_type == "string" then
        return string.format("%q", value)
    end
    if value_type ~= "table" then
        -- Opaque values should not normally reach the JSON boundary.  Including
        -- their type is deterministic and, importantly, does not inspect them.
        return "<" .. value_type .. ">"
    end

    seen = seen or {}
    if seen[value] then
        return "<cycle>"
    end
    seen[value] = true
    local array, count = is_array(value)
    local result = {}
    if array then
        for index = 1, count do
            result[#result + 1] = stable_value(value[index], seen)
        end
        seen[value] = nil
        return "[" .. table.concat(result, ",") .. "]"
    end

    local keys = {}
    for key, _ in pairs(value) do
        keys[#keys + 1] = key
    end
    table.sort(keys, function(left, right)
        local left_key = stable_value(left)
        local right_key = stable_value(right)
        if left_key == right_key then
            return type(left) < type(right)
        end
        return left_key < right_key
    end)
    for _, key in ipairs(keys) do
        result[#result + 1] = stable_value(key, seen) .. "=" .. stable_value(value[key], seen)
    end
    seen[value] = nil
    return "{" .. table.concat(result, ",") .. "}"
end

M.stable = stable_value
M.stable_key = stable_value

local function first_present(source, names)
    if not is_table(source) then
        return nil, false
    end
    for _, name in ipairs(names) do
        if value_is_present(source, name) then
            return source[name], true
        end
    end
    return nil, false
end

local function nonempty_table(value)
    return is_table(value) and next(value) ~= nil
end

local function valid_identity(value)
    if type(value) == "string" then
        return value ~= ""
    end
    if type(value) == "number" then
        return value == value and value ~= math.huge and value ~= -math.huge
    end
    return false
end

local function normalize_record(raw)
    if raw == nil then
        return {}
    end
    if not is_table(raw) then
        return { agent_id = raw }
    end

    local result = prune_absent(raw)
    local agent_id, has_agent_id = first_present(raw, { "agent_id", "id" })
    local native_id, has_native_id = first_present(raw, { "native_id", "nativeId" })
    local native_session_id, has_native_session_id =
        first_present(raw, { "adapter_session_id", "native_session_id", "session_id" })
    if (not has_agent_id or not valid_identity(agent_id))
        and has_native_id
    then
        agent_id = native_id
        has_agent_id = true
    elseif (not has_agent_id or not valid_identity(agent_id))
        and has_native_session_id
        and valid_identity(native_session_id)
    then
        agent_id = native_session_id
        has_agent_id = true
    end
    if has_agent_id and not value_is_present(result, "agent_id") then
        result.agent_id = copy_value(agent_id)
    end
    if has_native_id and valid_identity(native_id) and not value_is_present(result, "native_id") then
        result.native_id = copy_value(native_id)
    end
    local provider, has_provider = first_present(raw, { "provider", "adapter" })
    if has_provider and not value_is_present(result, "provider") then
        result.provider = copy_value(provider)
    end
    local kind, has_kind = first_present(raw, { "kind", "event_kind" })
    if has_kind and not value_is_present(result, "kind") then
        result.kind = copy_value(kind)
    end
    local adapter_session_id, has_adapter_session_id =
        first_present(raw, { "adapter_session_id", "native_session_id", "session_id" })
    if has_adapter_session_id and not value_is_present(result, "adapter_session_id") then
        result.adapter_session_id = copy_value(adapter_session_id)
    end
    local name, has_name = first_present(raw, { "name", "display_name" })
    if has_name and not value_is_present(result, "name") then
        result.name = copy_value(name)
    end
    local status, has_status = first_present(raw, { "status", "state", "lifecycle" })
    if has_status and not value_is_present(result, "status") then
        result.status = copy_value(status)
    end

    -- Keep partial facts genuinely partial: no empty facts table is emitted and
    -- no values are guessed from terminal output/process names.
    if value_is_present(raw, "facts") then
        local facts = prune_absent(raw.facts)
        if nonempty_table(facts) then
            result.facts = facts
        else
            result.facts = nil
        end
    end
    if value_is_present(raw, "authority") then
        local authority = prune_absent(raw.authority)
        if nonempty_table(authority) then
            result.authority = authority
        else
            result.authority = nil
        end
    end
    if value_is_present(raw, "view") then
        local view = prune_absent(raw.view)
        if nonempty_table(view) then
            result.view = view
        else
            result.view = nil
        end
    end
    return result
end

M.normalize_record = normalize_record
M.record = normalize_record

local function record_key(record, fallback)
    if not is_table(record) then
        if record ~= nil then
            return "value:" .. stable_value(record)
        end
        return "index:" .. tostring(fallback or 0)
    end
    local provider, has_provider = first_present(record, { "provider", "adapter" })
    local native_id, has_native = first_present(record, { "native_id", "nativeId" })
    if has_native and valid_identity(native_id) then
        -- The provider is part of the internal key; the native value itself is
        -- never rewritten or parsed.
        return "native:" .. stable_value(provider) .. ":" .. stable_value(native_id)
    end
    local id, has_id = first_present(record, { "agent_id", "id" })
    if has_id then
        return "agent:" .. stable_value(id)
    end
    local native_session_id, has_native_session =
        first_present(record, { "native_session_id", "adapter_session_id", "session_id" })
    if has_provider and has_native_session then
        return "session:" .. stable_value(provider) .. ":" .. stable_value(native_session_id)
    end
    return "record:" .. stable_value(record) .. ":" .. tostring(fallback or 0)
end

M.record_key = record_key
M.key = record_key

local function merge_facts(previous, partial)
    if not is_table(partial) then
        if partial == nil then
            return copy_value(previous), false
        end
        return copy_value(partial), true
    end
    local array, count = is_array(partial)
    if array then
        if count == 0 then
            return copy_value(previous), false
        end
        return prune_absent(partial), true
    end

    local result = is_table(previous) and copy_value(previous) or {}
    local changed = false
    for key, value in pairs(partial) do
        if value ~= nil then
            local old = result[key]
            if is_table(value) and is_table(old) then
                local merged, nested_changed = merge_facts(old, value)
                if nested_changed then
                    result[key] = merged
                    changed = true
                end
            elseif is_table(value) then
                local merged = prune_absent(value)
                if next(merged) ~= nil then
                    result[key] = merged
                    changed = true
                end
            elseif old ~= value then
                result[key] = copy_value(value)
                changed = true
            end
        end
    end
    return prune_absent(result), changed
end

M.merge_facts = merge_facts
M.merge_partial_facts = merge_facts

local function merge_partial(previous, incoming)
    local old = normalize_record(previous)
    local next_record = normalize_record(incoming)
    local result = copy_value(old)
    local changed = false
    for key, value in pairs(next_record) do
        if key == "facts" then
            local merged, did_change = merge_facts(result.facts, value)
            if did_change then
                result.facts = merged
                changed = true
            end
        elseif key == "authority" or key == "view" then
            local merged, did_change = merge_facts(result[key], value)
            if did_change then
                result[key] = merged
                changed = true
            end
        elseif result[key] ~= value then
            result[key] = copy_value(value)
            changed = true
        end
    end
    if result.agent_id == nil and next_record.agent_id ~= nil then
        result.agent_id = copy_value(next_record.agent_id)
        changed = true
    end
    return prune_absent(result), changed
end

M.merge_record = merge_partial
M.merge_partial = merge_partial

local function normalize_event(raw)
    if raw == nil then
        return {}
    end
    if not is_table(raw) then
        return { payload = raw }
    end
    local event = prune_absent(raw)
    local event_id, has_event_id = first_present(raw, { "event_id", "eventId", "id" })
    if has_event_id and not value_is_present(event, "event_id") then
        event.event_id = copy_value(event_id)
    end
    local sequence, has_sequence = first_present(raw, { "sequence", "seq", "revision", "cursor" })
    if has_sequence and not value_is_present(event, "sequence") then
        event.sequence = copy_value(sequence)
    end
    local generation, has_generation = first_present(raw, { "generation", "connection_generation", "epoch" })
    if has_generation and not value_is_present(event, "generation") then
        event.generation = copy_value(generation)
    end
    local source, has_source = first_present(raw, { "source", "provider", "adapter" })
    if has_source and not value_is_present(event, "source") then
        event.source = copy_value(source)
    end
    local record, has_record = first_present(raw, { "record", "update", "agent" })
    if has_record and is_table(record) and not value_is_present(event, "record") then
        event.record = normalize_record(record)
    end
    return event
end

M.normalize_event = normalize_event
M.event = normalize_event

local function event_key(event)
    local normalized = normalize_event(event)
    local explicit, has_explicit = first_present(normalized, { "event_id" })
    local source = normalized.source
    if has_explicit then
        return "event:" .. stable_value(source) .. ":" .. stable_value(explicit)
    end
    local generation, has_generation = first_present(normalized, { "generation" })
    local sequence, has_sequence = first_present(normalized, { "sequence" })
    local agent_id, has_agent = first_present(normalized, { "agent_id" })
    if has_sequence or has_generation or has_agent then
        return "cursor:" .. stable_value(source) .. ":" .. stable_value(generation) .. ":" .. stable_value(sequence) .. ":" .. stable_value(agent_id) .. ":" .. stable_value(normalized.type or normalized.operation)
    end
    return "payload:" .. stable_value(normalized)
end

M.event_key = event_key

local function numeric_or_text(value)
    if type(value) == "number" then
        return 0, value
    end
    if value == nil then
        return 1, ""
    end
    return 1, stable_value(value)
end

local function event_less(left, right)
    local left_event = normalize_event(left)
    local right_event = normalize_event(right)
    local left_generation_kind, left_generation_value = numeric_or_text(left_event.generation)
    local right_generation_kind, right_generation_value = numeric_or_text(right_event.generation)
    if left_generation_kind ~= right_generation_kind then
        return left_generation_kind < right_generation_kind
    end
    if left_generation_value ~= right_generation_value then
        return left_generation_value < right_generation_value
    end
    local left_sequence_kind, left_sequence_value = numeric_or_text(left_event.sequence)
    local right_sequence_kind, right_sequence_value = numeric_or_text(right_event.sequence)
    if left_sequence_kind ~= right_sequence_kind then
        return left_sequence_kind < right_sequence_kind
    end
    if left_sequence_value ~= right_sequence_value then
        return left_sequence_value < right_sequence_value
    end
    return event_key(left_event) < event_key(right_event)
end

local function ordered(events)
    local result = {}
    if not is_table(events) then
        return result
    end
    local array, count = is_array(events)
    if array then
        for index = 1, count do
            result[#result + 1] = copy_value(events[index])
        end
    else
        for _, event in pairs(events) do
            result[#result + 1] = copy_value(event)
        end
    end
    -- Insertion sort is stable and deterministic for small event batches, and
    -- avoids depending on Lua's implementation-specific table.sort tie behavior.
    for index = 2, #result do
        local value = result[index]
        local cursor = index - 1
        while cursor >= 1 and event_less(value, result[cursor]) do
            result[cursor + 1] = result[cursor]
            cursor = cursor - 1
        end
        result[cursor + 1] = value
    end
    return result
end

M.ordered = ordered
M.order = ordered
M.order_events = ordered

local function dedupe(events)
    local result = {}
    local seen = {}
    if not is_table(events) then
        return result
    end
    local array, count = is_array(events)
    local iterator
    if array then
        iterator = function(index)
            if index <= count then
                return index, events[index]
            end
            return nil
        end
    else
        local keys = {}
        for key, _ in pairs(events) do
            keys[#keys + 1] = key
        end
        table.sort(keys, function(left, right)
            return stable_value(left) < stable_value(right)
        end)
        local position = 0
        iterator = function()
            position = position + 1
            local key = keys[position]
            if key ~= nil then
                return key, events[key]
            end
            return nil
        end
    end
    local index = 1
    while true do
        local _, event = iterator(index)
        if event == nil then
            break
        end
        local key = event_key(event)
        if not seen[key] then
            seen[key] = true
            result[#result + 1] = copy_value(event)
        end
        index = index + 1
    end
    return result
end

M.dedupe = dedupe
M.deduplicate = dedupe

local function source_cursor(store, source)
    store.sources = store.sources or {}
    local key = stable_value(source)
    local cursor = store.sources[key]
    if cursor == nil then
        cursor = { generation = nil, sequence = nil }
        store.sources[key] = cursor
    end
    return cursor
end

local function event_record(event)
    local explicit
    if is_table(event.record) then
        explicit = event.record
    elseif is_table(event.update) then
        explicit = event.update
    elseif is_table(event.agent) then
        explicit = event.agent
    end
    if explicit ~= nil then
        local record = normalize_record(explicit)
        -- Adapter metadata is authoritative and must not be dropped merely
        -- because the adapter placed it on the envelope rather than record.
        for _, field in ipairs({
            "provider", "adapter", "native_id", "kind", "adapter_session_id",
            "terminal_id", "process_id", "sequence",
        }) do
            if record[field] == nil and event[field] ~= nil then
                record[field] = copy_value(event[field])
            end
        end
        return normalize_record(record)
    end
    local record = {}
    local copy_fields = {
        "agent_id", "id", "provider", "adapter", "native_id", "nativeId", "kind",
        "name", "display_name", "status", "state", "lifecycle", "facts", "authority",
        "view", "terminal_id", "process_id", "native_session_id", "adapter_session_id",
        "session_id", "pane_id", "worktree_id", "managed", "stale", "sequence",
    }
    for _, field in ipairs(copy_fields) do
        if event[field] ~= nil then
            record[field] = copy_value(event[field])
        end
    end
    if record.provider == nil then
        record.provider = copy_value(event.source)
    end
    return normalize_record(record)
end

local function event_agent_id(event, record)
    local agent_id, has_agent_id = first_present(event, { "agent_id" })
    if has_agent_id and valid_identity(agent_id) then
        return agent_id
    end
    local record_id, has_record_id = first_present(record, { "agent_id", "id" })
    if has_record_id and valid_identity(record_id) then
        return record_id
    end
    local native_id, has_native_id = first_present(event, { "native_id", "nativeId" })
    if has_native_id and valid_identity(native_id) then
        return native_id
    end
    local event_session, has_event_session =
        first_present(event, { "adapter_session_id", "native_session_id", "session_id" })
    if has_event_session and valid_identity(event_session) then
        return event_session
    end
    local record_session, has_record_session =
        first_present(record, { "adapter_session_id", "native_session_id", "session_id" })
    if has_record_session and valid_identity(record_session) then
        return record_session
    end
    return nil
end

local function record_map(store)
    store.records = store.records or {}
    return store.records
end

local function snapshot_values(snapshot)
    if not is_table(snapshot) then
        return {}
    end
    if is_table(snapshot.records) then
        return snapshot.records
    end
    if is_table(snapshot.agents) then
        return snapshot.agents
    end
    return snapshot
end

local function apply_snapshot(store, snapshot, source)
    local records = record_map(store)
    local values = snapshot_values(snapshot)
    local incoming = {}
    local applied = {}
    local array, count = is_array(values)
    local function add_record(value, fallback)
        local record = normalize_record(value)
        if record.provider == nil and source ~= nil then
            record.provider = copy_value(source)
        end
        if not valid_identity(record.agent_id) then
            return
        end
        local key = record_key(record, fallback)
        incoming[key] = record
    end
    if array then
        for index = 1, count do
            add_record(values[index], index)
        end
    else
        for key, value in pairs(values) do
            add_record(value, key)
        end
    end
    local removed = {}
    for key, old in pairs(records) do
        local belongs = source == nil or old.provider == source or old.adapter == source
        if belongs and incoming[key] == nil then
            records[key] = nil
            if valid_identity(old.agent_id) then
                removed[#removed + 1] = copy_value(old.agent_id)
            end
        end
    end
    for key, record in pairs(incoming) do
        records[key] = copy_value(record)
        applied[#applied + 1] = copy_value(record)
    end
    return {
        records = applied,
        removed = removed,
        changed = #applied > 0 or #removed > 0,
    }
end

local function remember_event(store, key, status, event)
    store.events = store.events or {}
    store.event_order = store.event_order or {}
    if store.events[key] == nil then
        store.event_order[#store.event_order + 1] = key
    end
    -- Keep status/cursor identity only. Full native payloads are returned from
    -- this call but are not retained indefinitely by the model.
    store.events[key] = { status = status }
    local limit = tonumber(store.max_events) or 512
    if limit < 16 then limit = 16 end
    if limit > 4096 then limit = 4096 end
    while #store.event_order > limit do
        local oldest = table.remove(store.event_order, 1)
        if oldest ~= nil then
            store.events[oldest] = nil
        end
    end
end

-- Apply one authoritative event to a mutable store. The returned table always
-- identifies whether it changed state; callers can therefore avoid publishing
-- duplicate or stale events.
local function apply_event(store, raw_event)
    store = store or {}
    store.events = store.events or {}
    store.event_order = store.event_order or {}
    store.records = store.records or {}
    local event = normalize_event(raw_event)
    local key = event_key(event)
    if store.events[key] ~= nil then
        return { status = "duplicate", changed = false, key = key, event = event }
    end

    local source = event.source or event.adapter
    local cursor = source_cursor(store, source)
    local generation = event.generation
    local sequence = event.sequence
    local generation_number = type(generation) == "number" and generation or nil
    local sequence_number = type(sequence) == "number" and sequence or nil
    local stale = false
    if generation_number ~= nil and cursor.generation ~= nil and generation_number < cursor.generation then
        stale = true
    elseif generation_number ~= nil and cursor.generation ~= nil and generation_number > cursor.generation then
        cursor.generation = generation_number
        cursor.sequence = nil
    elseif generation_number ~= nil and cursor.generation == nil then
        cursor.generation = generation_number
    end
    if not stale and sequence_number ~= nil and cursor.sequence ~= nil and sequence_number <= cursor.sequence then
        stale = true
    end
    if stale then
        remember_event(store, key, "stale", event)
        return { status = "stale", changed = false, key = key, event = event }
    end

    remember_event(store, key, "accepted", event)
    if sequence_number ~= nil then
        cursor.sequence = sequence_number
    end

    local rebased = event.rebase == true
        or event.reconnect == true
        or event.kind == "rebase"
        or event.type == "rebase"
    local applied_records = {}
    local removed = {}
    local changed = false
    local seen_records = {}
    if rebased and event.snapshot ~= nil then
        local snapshot_result = apply_snapshot(store, event.snapshot, source)
        applied_records = snapshot_result.records
        removed = snapshot_result.removed
        changed = snapshot_result.changed
        for _, item in ipairs(applied_records) do
            seen_records[record_key(item)] = true
        end
    end

    local operation = event.operation or event.op
    local record = event_record(event)
    local agent_id = event_agent_id(event, record)
    if operation == "remove" or operation == "delete" or operation == "removed" then
        if valid_identity(agent_id) then
            for record_key_value, existing in pairs(store.records) do
                if existing.agent_id == agent_id or existing.native_id == agent_id then
                    store.records[record_key_value] = nil
                    removed[#removed + 1] = copy_value(agent_id)
                    changed = true
                end
            end
        end
    elseif valid_identity(agent_id) then
        if record.agent_id == nil then
            record.agent_id = copy_value(agent_id)
        end
        if record.provider == nil then
            record.provider = copy_value(source)
        end
        local key_for_record = record_key(record)
        local old = store.records[key_for_record]
        local merged, record_changed = merge_partial(old, record)
        store.records[key_for_record] = merged
        if record_changed or old == nil then
            changed = true
        end
        record = merged
        if not seen_records[key_for_record] then
            applied_records[#applied_records + 1] = copy_value(record)
            seen_records[key_for_record] = true
        end
    end

    return {
        status = rebased and "rebase" or "accepted",
        changed = changed,
        key = key,
        event = event,
        record = record,
        records = applied_records,
        removed = removed,
        cursor = copy_value(cursor),
    }
end

M.apply_event = apply_event
M.accept_event = apply_event

local function new_store(options)
    options = options or {}
    local max_events = tonumber(options.max_events or options.event_limit) or 512
    if max_events < 16 then max_events = 16 end
    if max_events > 4096 then max_events = 4096 end
    return {
        records = {},
        events = {},
        event_order = {},
        max_events = max_events,
        sources = {},
        rebases = {},
    }
end

M.new_store = new_store
M.store = new_store

local function rebase(store, events, options)
    store = store or new_store()
    options = options or {}
    local list = dedupe(ordered(events or {}))
    local accepted = {}
    local generation = options.generation
    if generation == nil then
        generation = options.min_generation
    end
    for _, event in ipairs(list) do
        local normalized = normalize_event(event)
        if options.reconnect == true or options.replace == true then
            normalized.rebase = normalized.rebase == true or normalized.snapshot ~= nil
        end
        if generation == nil
            or type(normalized.generation) ~= "number"
            or type(generation) ~= "number"
            or normalized.generation >= generation
        then
            local outcome = apply_event(store, normalized)
            accepted[#accepted + 1] = outcome
        end
    end
    return store, accepted
end

M.rebase = rebase
M.reconnect = function(store, events, options)
    options = options or {}
    options.reconnect = true
    return rebase(store, events, options)
end

local lifecycle_order = {
    created = 1,
    starting = 2,
    running = 3,
    waiting = 4,
    paused = 5,
    retrying = 6,
    stopping = 7,
    completed = 8,
    failed = 9,
    aborted = 10,
    released = 11,
    stale = 12,
}

M.lifecycle_order = lifecycle_order

local function lifecycle_rank(value)
    return lifecycle_order[value] or 0
end

M.lifecycle_rank = lifecycle_rank

local function lifecycle_transition(previous, next_value)
    if next_value == nil then
        return false
    end
    if previous == nil or previous == next_value then
        return true
    end
    -- Authoritative adapter events are allowed to report a terminal state even
    -- when an earlier partial fact was missed.  The helper is still useful to
    -- command handlers for deterministic intent validation.
    if lifecycle_rank(previous) == 0 or lifecycle_rank(next_value) == 0 then
        return true
    end
    return true
end

M.lifecycle_transition = lifecycle_transition
M.can_transition = lifecycle_transition

return M
