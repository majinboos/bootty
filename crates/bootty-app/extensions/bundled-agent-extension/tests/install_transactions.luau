-- Installer transaction contracts: exact previews, host confirmation, CAS, rollback, and ownership.
local install = dofile("crates/bootty-app/extensions/bundled-agent-extension/install.luau")

local function new_host()
    local host = { data = {}, fail_path = nil, concurrent_path = nil, writes = 0 }
    host.files = {}
    host.files.exists = function(path) return host.data[path] ~= nil end
    host.files.read = function(path) return host.data[path] end
    host.files.stat = function(path)
        if host.data[path] == nil then return { exists = false } end
        return { identity = "fixture:" .. path, version = tostring(#host.data[path]) }
    end
    host.files.write = function(path, content)
        host.writes = host.writes + 1
        if host.fail_path == path then return false, "fixture write failure" end
        host.data[path] = content
        if host.concurrent_path == path then host.data[path] = "concurrent-user-edit" end
        return true
    end
    host.files.remove = function(path)
        if host.fail_path == path then return false, "fixture remove failure" end
        host.data[path] = nil
        return true
    end
    host.files.validate_confirmation = function(expected, token)
        return token == "fixture-host-token"
            and expected.digest ~= nil
            and expected.preview ~= nil
    end
    return host
end

local function confirmation(_plan)
    return { token = "fixture-host-token" }
end

local function apply(plan, host)
    return assert(install.apply(plan, { host = host, confirmation = confirmation(plan) }))
end

return function()
    local host = new_host()
    local context = { home = "/fixture/home" }
    local install_plan = assert(install.plan("install", "pi", {
        host = host,
        context = context,
        settings = { extensions = { "user-extension" } },
        transaction_id = "fixture-install-1",
    }))
    assert(install_plan.changed and install_plan.exact and install_plan.transactional)
    assert(install_plan.plan_id and install_plan.transaction.digest == install_plan.digest)
    assert(#install_plan.operations == #install_plan.changes)
    assert(apply(install_plan, host).status == "applied")

    local setup_plan = assert(install.plan("setup", "pi", {
        host = host,
        context = context,
        settings = { extensions = { "user-extension", "second-user-extension" } },
        transaction_id = "fixture-setup-1",
    }))
    assert(setup_plan.changed and apply(setup_plan, host).status == "applied")

    local stale_plan = assert(install.plan("setup", "pi", {
        host = host,
        context = context,
        settings = { extensions = { "user-extension", "stale-preview" } },
        transaction_id = "fixture-stale-1",
    }))
    host.data[stale_plan.changes[1].path] = "changed-after-preview"
    local stale = assert(install.apply(stale_plan, { host = host, confirmation = confirmation(stale_plan) }))
    assert(stale.status == "stale")
    assert(host.data[stale_plan.changes[1].path] == "changed-after-preview")
    host.data[stale_plan.changes[1].path] = stale_plan.changes[1].before

    local uninstall_plan = assert(install.plan("uninstall", "pi", {
        host = host,
        context = context,
        transaction_id = "fixture-uninstall-1",
    }))
    assert(uninstall_plan.changed)
    assert(apply(uninstall_plan, host).status == "applied")
    local settings = host.data["/fixture/home/.pi/agent/settings.json"]
    assert(settings and settings:find("user%-extension") and settings:find("second%-user%-extension"))
    assert(host.data["/fixture/home/.config/bootty/agents/pi.json"] == nil)

    local failing_host = new_host()
    local failing_plan = assert(install.plan("install", "pi", {
        host = failing_host,
        context = context,
        settings = {},
        transaction_id = "fixture-failure-1",
    }))
    failing_host.fail_path = failing_plan.changes[3].path
    failing_host.concurrent_path = failing_plan.changes[1].path
    local failed = assert(install.apply(failing_plan, { host = failing_host, confirmation = confirmation(failing_plan) }))
    assert(failed.status == "failed")
    assert(failing_host.data[failing_plan.changes[1].path] == "concurrent-user-edit")
    assert(#failed.rollback_errors > 0)

    local foreign = assert(install.plan("uninstall", "pi", {
        host = new_host(),
        context = context,
        marker = {
            owner = "bootty.agents",
            adapter = "pi",
            marker = "bootty-agents-pi-v1",
            version = "0.84.1",
            files = { "~/user-owned-file", "~/.config/bootty/agents/pi.json" },
        },
    }))
    assert(not foreign.changed and foreign.diagnostics[1].code == "ownership_marker_mismatch")
    return true
end
