-- Shared e2e spec function. The generic runner executes it with `(ui, fixture)` and a fresh fake host.
-- It exercises command/event boundaries only; no process, terminal, clock, or network access.

return function(ui, fixture)
	local host = fixture.new_host()
	local list_result = ui.workflow.list({}, host)
	assert(type(list_result) == "table" and #list_result.records == 1, "agents.list returns fixture records")
	assert(list_result.records[1].agent_id == fixture.agent_id, "list preserves opaque agent identity")

	local rows = ui.render(list_result.records, nil, host)
	assert(#rows == 1 and rows[1].agent_id == fixture.agent_id, "listed agent renders as a generic row")

	local state = ui.new_state(list_result.records)
	local subscription = ui.subscribe(host, function(event)
		ui.apply_event(state, event)
	end)
	assert(subscription.topic == "agents.changed", "UI subscribes to the normalized changed topic")

	local prompt_result = ui.workflow.prompt(fixture.agent_id, "run the fixture", "normal", host)
	assert(prompt_result.accepted == true, "agents.prompt crosses the command boundary")
	assert(host.calls[2].id == "agents.prompt", "prompt invokes the canonical command id")
	assert(host.calls[2].args.agent_id == fixture.agent_id, "prompt uses the opaque agent id")

	host.publish("agents.changed", {
		event_id = "e2e-change-02",
		sequence = 2,
		record = { agent_id = fixture.agent_id, lifecycle = "running", turn = "turn-opaque-02" },
	})
	assert(state.records[1].lifecycle == "running", "agents.changed updates only explicit normalized lifecycle")
	assert(state.records[1].turn == "turn-opaque-02", "agents.changed carries explicit partial facts")

	local steer_result = ui.workflow.steer(fixture.agent_id, "nudge the fixture", host)
	assert(steer_result.accepted == true, "steer uses the prompt command boundary")
	assert(host.calls[3].id == "agents.prompt", "steer invokes the canonical prompt command")
	assert(host.calls[3].args.mode == "steer", "steer mode is explicit")

	local wait_result = ui.workflow.wait(fixture.agent_id, 0, host)
	assert(wait_result.accepted == true, "wait uses the canonical command boundary")
	assert(host.calls[4].id == "agents.wait", "wait invokes agents.wait")
	assert(host.calls[4].args.timeout_ms == 0, "wait timeout is preserved")

	local cancel_result = ui.workflow.cancel(fixture.agent_id, host)
	assert(cancel_result.cancelled == true, "cancellation invokes agents.abort")
	assert(host.calls[5].id == "agents.abort", "cancellation command is canonical")

	local cleanup_results = ui.workflow.cleanup(fixture.agent_id, host)
	assert(#cleanup_results == 3, "cleanup has a deterministic command sequence")
	assert(host.calls[6].id == "agents.view.clear", "cleanup clears view metadata first")
	assert(host.calls[7].id == "agents.clear_authority", "cleanup clears adapter authority second")
	assert(host.calls[8].id == "agents.release", "cleanup releases bundle ownership last")
	for index = 6, 8 do
		assert(host.calls[index].args.agent_id == fixture.agent_id, "cleanup preserves opaque identity")
	end
	return true
end
