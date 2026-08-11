-- Generic runner contract: return `(ui, fixtures, fake_host)` test function.
-- Adapters differ only in explicit facts; the normalized record/UI contract is shared.

local function action_ids(row)
	local ids = {}
	for _, action in ipairs(row.actions or {}) do
		ids[#ids + 1] = action.command_id
	end
	return ids
end

return function(ui, fixtures)
	local records = {}
	for provider, adapter in pairs(fixtures.adapters) do
		local agent_id = "bundle/opaque/" .. provider
		records[#records + 1] = {
			agent_id = agent_id,
			provider = adapter.provider,
			native_ids = { session = adapter.native_session_id },
			authority = { adapter_session_id = adapter.native_session_id, sequence = 1 },
			lifecycle = adapter.facts.lifecycle,
			model = adapter.facts.model,
			permission = adapter.facts.permission,
			usage = adapter.facts.usage,
			tool = adapter.facts.tool,
		}
	end
	local rows = ui.render(records)
	assert(#rows == 4, "all four adapters render through one generic surface")
	local expected_actions = action_ids(rows[1])
	assert(#expected_actions > 0, "generic rows expose actions")
	for _, row in ipairs(rows) do
		assert(row.agent_id:match("^bundle/opaque/"), "adapter ids remain opaque bundle ids")
		assert(row.provider ~= nil, "provider is an explicit normalized fact")
		assert(row.lifecycle == "running", "lifecycle is consumed, not inferred")
		local current = action_ids(row)
		assert(#current == #expected_actions, "action count is adapter-independent")
		for index, command_id in ipairs(expected_actions) do
			assert(current[index] == command_id, "action command IDs are adapter-independent")
			assert(current[index]:match("^agents%.[%w_%.]+$"), "actions call canonical agent commands")
		end
		assert(row.facts.native_ids.session ~= nil, "explicit native id remains a nested fact")
	end
	return true
end
