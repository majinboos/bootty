-- Generic runner contract: return one function `(ui, fixtures, fake_host)`; see the runner contract comment
-- in ../ui.luau.  This test uses only deterministic event fixtures.

local function assert_equal(left, right, message)
	assert(left == right, message or ("expected " .. tostring(right) .. ", got " .. tostring(left)))
end

return function(ui, fixtures)
	local ordered = ui.lifecycle_order(fixtures.lifecycle_events)
	assert_equal(#ordered, 5, "all lifecycle events remain ordered before dedup")
	assert_equal(ordered[1].sequence, 1, "sequence one first")
	assert_equal(ordered[2].sequence, 1, "same sequence remains stable")
	assert_equal(ordered[3].sequence, 2, "sequence two follows sequence one")
	assert_equal(ordered[4].sequence, 3, "sequence three follows sequence two")
	assert_equal(ordered[5].sequence, 4, "remove follows updates")

	local deduped = ui.dedupe_events(fixtures.lifecycle_events)
	assert_equal(#deduped, 4, "duplicate event id is removed")
	assert_equal(deduped[1].event_id, "evt-1", "dedup keeps the first ordered event")
	assert_equal(deduped[4].operation, "remove", "remove event is retained")

	local records, state = ui.reduce(fixtures.lifecycle_events, fixtures.records)
	assert_equal(state.cursor, 4, "cursor follows latest explicit event sequence")
	local by_id = {}
	for _, record in ipairs(records) do
		by_id[record.agent_id] = record
	end
	assert_equal(by_id["opaque/pi/session/01"].lifecycle, "completed", "latest lifecycle wins")
	assert(by_id["opaque/opencode#gamma"] == nil, "explicit remove removes only its opaque id")

	local rebased, rebased_state = ui.reduce({ fixtures.rebase_event }, records)
	assert_equal(#rebased, 2, "rebase replaces stale records")
	assert_equal(rebased_state.cursor, 8, "rebase cursor is authoritative")
	assert_equal(rebased[1].agent_id, "opaque/pi/session/01", "snapshot ordering is preserved")
	assert_equal(rebased[1].lifecycle, "idle", "snapshot lifecycle replaces stale state")
	assert_equal(rebased[2].agent_id, "opaque/new-agent/9", "snapshot adds new record")
	local direct_rebase = ui.rebase(records, fixtures.rebase_event)
	assert_equal(#direct_rebase, 2, "rebase helper accepts an explicit rebase event")
	return true
end
