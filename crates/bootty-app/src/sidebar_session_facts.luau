--!strict
-- Authoritative session enrichment for the generic sidebar.
--
-- This module deliberately does not inspect process names, terminal output, descendants,
-- transcripts, or repository diffs to decide whether an agent is active. Those observations are
-- display context only; a native adapter record supplied by bootty.agents authorizes agent facts.
local sidebar = {}
local shared_provider = nil

local function first_nonempty(...)
	local values = { ... }
	for _, value in ipairs(values) do
		if value ~= nil and value ~= "" then
			return value
		end
	end
	return nil
end

local function session_key(session)
	return first_nonempty(session.cache_key, session.id) or ""
end

local function record_terminal_id(record)
	if type(record.terminal) == "table" then
		return first_nonempty(record.terminal.terminal_id, record.terminal.id)
	end
	return first_nonempty(record.terminal_id)
end

local function record_agent_id(record)
	return first_nonempty(record.agent_id, record.id)
end

local function record_matches_session(record, session)
	if type(record) ~= "table" then
		return false
	end
	local exact_key = first_nonempty(record.session_key, record.session_id, record.binding_session_id)
	if exact_key ~= nil then
		return exact_key == session_key(session) or exact_key == session.id
	end
	local terminal_id = record_terminal_id(record)
	return terminal_id ~= nil and terminal_id == session.pane_id
end

local function host_records()
	if type(bootty.agents) ~= "table" or type(bootty.agents.records) ~= "function" then
		return nil
	end
	local ok, records = pcall(bootty.agents.records)
	if not ok or type(records) ~= "table" then
		return nil
	end
	return records
end

local function find_record(records, session)
	if type(records) ~= "table" then
		return nil
	end
	local key = session_key(session)
	local direct = records[key] or records[session.id]
	if type(direct) == "table" then
		return direct
	end
	for _, record in pairs(records) do
		if record_matches_session(record, session) then
			return record
		end
	end
	return nil
end

local function style_for_provider(provider)
	if provider == "codex" then
		return { icon = "openai", color = bootty.theme.accent }
	elseif provider == "claude" then
		return { icon = "claude", color = bootty.theme.warning }
	elseif provider == "opencode" then
		return { icon = "bot", color = bootty.theme.primary }
	elseif provider == "pi" then
		return { icon = "terminal", color = bootty.theme.success }
	end
	return nil
end

local function facts_for(session, record)
	local facts = {
		display_process = session.process,
		pane_id = session.pane_id,
		pane_pid = session.pane_pid,
		authoritative = false,
	}
	if type(record) ~= "table" then
		return facts
	end
	facts.authoritative = true
	facts.agent_id = record_agent_id(record)
	facts.agent_name = first_nonempty(record.display_name, record.name)
	facts.provider = record.provider
	facts.lifecycle = record.lifecycle
	facts.activity = record.activity
	facts.activity_color = record.activity_color
	facts.native_ids = record.native_ids
	facts.turn = record.turn
	facts.tool = record.tool
	facts.permission = record.permission
	facts.completion = record.completion
	facts.usage = record.usage
	facts.cost = record.cost
	facts.model = record.model
	facts.reasoning = record.reasoning
	facts.notification = record.notification
	facts.error = record.error
	facts.agent_style = style_for_provider(record.provider)
	return facts
end

function sidebar.session_facts()
	if shared_provider then
		return shared_provider
	end
	local cache = {}
	local authoritative_records = nil
	local provider = {}

	function provider.refresh(sessions)
		local records = host_records() or authoritative_records or {}
		for _, session in ipairs(sessions or {}) do
			cache[session_key(session)] = facts_for(session, find_record(records, session))
		end
	end

	function provider.set_records(records)
		-- Generic hosts can feed an authoritative snapshot; fixture tests use this same seam.
		if type(records) == "table" then
			authoritative_records = records
		else
			authoritative_records = {}
		end
	end

	function provider.get(session)
		local key = session_key(session)
		return cache[key] or facts_for(session, nil)
	end

	function provider.activity(facts)
		if not facts or not facts.authoritative then
			return nil, nil
		end
		return first_nonempty(facts.activity, facts.lifecycle), facts.activity_color
	end

	function provider.clear()
		cache = {}
	end

	shared_provider = provider
	return provider
end

return sidebar
