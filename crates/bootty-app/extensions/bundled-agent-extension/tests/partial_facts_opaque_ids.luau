-- Generic runner contract: return `(ui, fixtures, fake_host)` test function; no terminal/process inspection.

return function(ui)
	local opaque_id = "provider://session/with:slashes#opaque?"
	local partial = {
		agent_id = opaque_id,
		provider = "pi",
		process_id = "must-not-be-promoted",
		native_session_id = "must-not-be-promoted",
		fidelity = { level = "partial" },
	}
	local normalized = ui.normalize(partial)
	assert(normalized ~= nil, "partial records with an opaque agent id remain renderable")
	assert(normalized.agent_id == opaque_id, "opaque id is preserved byte-for-byte")
	assert(normalized.lifecycle == "unknown", "missing lifecycle remains explicit unknown")
	assert(normalized.terminal == nil, "top-level process facts are not inferred as terminal facts")
	assert(normalized.native_ids == nil, "native ids are not inferred from native_session_id")
	assert(normalized.fidelity.level == "partial", "explicit fidelity is retained")

	local rows = ui.render({ partial })
	assert(#rows == 1, "one partial record creates one generic row")
	assert(rows[1].agent_id == opaque_id, "row key carries the opaque agent id")
	assert(rows[1].key == "agent:" .. opaque_id, "row key does not normalize or split the id")
	assert(rows[1].lifecycle == "unknown", "row lifecycle comes from normalized record")
	assert(#rows[1].actions > 0, "row exposes command actions without a widget dependency")
	for _, action in ipairs(rows[1].actions) do
		assert(action.args.agent_id == opaque_id, "every action uses the opaque bundle id")
	end

	local no_identity = ui.normalize({ provider = "codex", terminal = { terminal_id = "terminal-only" } })
	assert(no_identity == nil, "terminal identity cannot create a bundle agent record")
	return true
end
