-- Generic runner contract: return `(ui, fixtures, fake_host)` test function.
-- The fixture command list mirrors the bundled manifest's canonical 29 ids; no JSON/filesystem access
-- is needed, so the check stays deterministic in Lua 5.4 and Luau.

return function(ui, fixtures)
	assert(#ui.command_ids == 29, "bundled agent command catalog has exactly 29 ids")
	assert(#fixtures.command_ids == 29, "fixture catalog has exactly 29 ids")
	local seen = {}
	for index, command_id in ipairs(ui.command_ids) do
		assert(type(command_id) == "string" and command_id:match("^agents%.[%w_%.]+$"), "agent command id is namespaced")
		assert(not seen[command_id], "agent command ids are unique")
		seen[command_id] = true
		assert(command_id == fixtures.command_ids[index], "UI and manifest fixture preserve catalog order")
	end
	for _, command_id in ipairs(fixtures.command_ids) do
		assert(seen[command_id], "every manifest command is exposed by ui.command_ids")
	end
	assert(seen["agents.list"], "agents.list is present")
	assert(seen["agents.prompt"], "agents.prompt is present")
	assert(seen["agents.abort"], "agents.abort is present for cancellation")
	return true
end
