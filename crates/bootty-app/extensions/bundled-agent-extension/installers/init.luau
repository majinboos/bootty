--!strict
local M = {}

local function load_adapter(name)
    local ok, value = pcall(require, "installers/" .. name)
    if ok then return value end
    local candidates = {
        "./installers/" .. name .. ".luau",
        "crates/bootty-app/extensions/bundled-agent-extension/installers/" .. name .. ".luau",
    }
    for index = 1, #candidates do
        if type(loadfile) == "function" then
            local chunk = loadfile(candidates[index])
            if chunk ~= nil then
                local loaded, result = pcall(chunk)
                if loaded then return result end
            end
        end
    end
    error("unable to load adapter installer " .. name .. ": " .. tostring(value))
end

local modules = {
    pi = load_adapter("pi"),
    claude = load_adapter("claude"),
    codex = load_adapter("codex"),
    opencode = load_adapter("opencode"),
}

local order = { "pi", "claude", "codex", "opencode" }

function M.ids()
    local result = {}
    for index = 1, #order do result[index] = order[index] end
    return result
end

function M.get(adapter_id)
    return modules[adapter_id]
end

function M.all()
    local result = {}
    for index = 1, #order do result[index] = modules[order[index]] end
    return result
end

M.order = order
M.adapters = modules

return M
