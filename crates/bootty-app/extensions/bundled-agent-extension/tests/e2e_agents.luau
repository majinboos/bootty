-- Generic runner contract: load ui.luau and this file from the repository root, then call the returned
-- function as `(ui, fixtures, fake_host)`.  The shared fixture/spec are pure Lua tables/functions;
-- this entrypoint loads them from the fixed extension path and does not touch the host filesystem
-- beyond those fixture files.

local fixture = dofile("crates/bootty-app/extensions/bundled-agent-extension/fixtures/shared/agents_e2e_fixture.luau")
local spec = dofile("crates/bootty-app/extensions/bundled-agent-extension/fixtures/shared/agents_e2e_spec.luau")

return function(ui)
	return spec(ui, fixture)
end
