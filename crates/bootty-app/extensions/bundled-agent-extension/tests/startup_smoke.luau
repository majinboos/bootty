-- Generic package startup contract. This fixture intentionally uses the same generic commands/events
-- registration surface as the runtime loader, then exercises list and authoritative ingest.
return function()
	local root = "crates/bootty-app/extensions/bundled-agent-extension/"
	local old_bootty = bootty
	local old_path = package.path
	local calls = {}
	local events = {}
	local host = {
		commands = {},
		events = {},
	}
	host.storage = { values = {} }
	host.storage.get = function(key)
		return host.storage.values[key]
	end
	host.storage.put = function(key, value)
		host.storage.values[key] = value
		return true
	end
	host.events.publish = function() end
	host.commands.register = function(descriptor)
		calls[descriptor.id] = descriptor
	end
	host.events.register = function(topic)
		events[#events + 1] = topic
	end
	bootty = host
	package.path = root .. "?.luau;" .. root .. "?/init.luau;" .. root .. "?/?.luau;" .. old_path
	for _, module_name in ipairs({ "core", "model", "ui", "install" }) do
		package.loaded[module_name] = nil
	end
	local ok, extension_or_error = pcall(dofile, root .. "agent_extension.luau")
	bootty = old_bootty
	package.path = old_path
	assert(ok, tostring(extension_or_error))
	local extension = extension_or_error
	assert(type(extension) == "table", "package entrypoint must return its generic service table")
	assert(#events >= 1, "package must register its event topic")
	assert(events[1] == "agents.changed", "package event topic must be agents.changed")
	local command_count = 0
	for _ in pairs(calls) do command_count = command_count + 1 end
	assert(command_count == 29, "package must register all 29 agents commands")
	local ingest_descriptor = calls["agents.ingest"]
	assert(ingest_descriptor and ingest_descriptor.arguments[1].name == "event" and ingest_descriptor.arguments[1].required, "ingest descriptor must require object event")
	assert(ingest_descriptor.arguments[2].name == "adapter" and ingest_descriptor.arguments[3].name == "event_name", "ingest descriptor must expose adapter and event_name options")
	local listed = extension.handle("agents.list", {})
	assert(type(listed) == "table" and #listed == 0, "agents.list must return an array")
	local ingested = extension.handle("agents.ingest", {
		event = {
			event_id = "startup-event-1",
			source = "pi",
			generation = 1,
			sequence = 1,
			agent_id = "opaque/startup/pi/1",
			record = {
				agent_id = "opaque/startup/pi/1",
				provider = "pi",
				lifecycle = "running",
			},
		},
	})
	assert(ingested.accepted and ingested.changed, "agents.ingest must accept an authoritative event")
	local record = extension.handle("agents.get", { agent_id = "opaque/startup/pi/1" })
	assert(record.record and record.record.agent_id == "opaque/startup/pi/1", "ingest must preserve opaque IDs")
	local wrapped = extension.handle("agents.ingest", {
		event = {
			event_id = "cli-envelope-1",
			record = { agent_id = "opaque/claude/1", provider = "claude", lifecycle = "running" },
		},
		adapter = "claude",
		event_name = "SessionStart",
	})
	assert(wrapped.accepted and wrapped.changed, "CLI adapter/event envelope must ingest")
	assert(wrapped.event.adapter == "claude" and wrapped.event.event_name == "SessionStart", "CLI envelope must preserve explicit adapter and event kind")
	assert(wrapped.event.raw and wrapped.event.raw.event_id == "cli-envelope-1", "CLI envelope must retain raw hook payload")
	local rejected = extension.handle("agents.ingest", { event = {}, adapter = "unknown" })
	local hook_source = extension.handle("agents.ingest", {
		event = {
			event_id = "hook-source-1",
			source = "hook",
			event_name = "SessionStart",
			record = { agent_id = "opaque/claude/hook-source", provider = "claude", lifecycle = "running" },
		},
		adapter = "claude",
	})
	assert(hook_source.accepted and hook_source.changed, "generic hook source must not conflict with explicit adapter")
	local malformed = extension.handle("agents.ingest", { event = { event = 7 }, adapter = "pi" })
	assert(malformed.ok == false and malformed.code == "invalid_argument", "malformed event kind must be rejected")
	assert(rejected.ok == false and rejected.code == "unsupported_adapter", "unknown adapter must be rejected")
	return true
end
