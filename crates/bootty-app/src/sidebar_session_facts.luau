--!strict
-- Optional session enrichment used by the built-in module.
local sidebar = {}

function sidebar.session_facts(options)
options = options or {}

local ui = bootty.ui
local shell = bootty.shell
local quote = shell.quote
local stderr_null = shell.stderr_null

local MUTED_COLOR = bootty.theme.muted
local DESTRUCTIVE_COLOR = bootty.theme.destructive
local CODEX_COLOR = bootty.theme.accent
local CLAUDE_COLOR = bootty.theme.warning
local OPENCODE_COLOR = bootty.theme.primary
local WAITING_COLOR = bootty.theme.warning
local PERC_BASE = bootty.theme.destructive
local PERC_SHINE = bootty.theme.warning

local PULSE_PERIOD = 2.0
local FACT_REFRESH_INTERVAL = options.fact_refresh_interval or 0.5
local CODEX_VERBS = { "Codexing…", "Working…", "Thingamabobbing…" }
local OPENCODE_VERBS = { "Opencodding…", "Opendoing…", "Shming Shmopenig…" }

local AGENT_STYLE = {
	codex = { icon = "openai", color = CODEX_COLOR },
	claude = { icon = "claude", color = CLAUDE_COLOR },
	opencode = { icon = "bot", color = OPENCODE_COLOR },
}

local FACTS_CACHE = {}
local first_render = true
local refresh_index = 1
local last_fact_refresh = 0

local function tmux_session(session)
	return string.sub(session.id, 1, 1) == "$"
end

local function tmux_active_pane(session)
	if not tmux_session(session) then
		return nil, nil, nil
	end
	local out = shell.run(
		"tmux list-panes -t "
			.. quote(session.id)
			.. " -F '#{pane_active}\t#{pane_id}\t#{pane_current_command}\t#{pane_pid}' "
			.. stderr_null
	)
	for line in string.gmatch(out, "[^\n]+") do
		local active, pane_id, command, pane_pid = string.match(line, "^([^\t]*)\t([^\t]*)\t([^\t]*)\t(.+)$")
		if active == "1" and command ~= "" then
			return command, pane_id, pane_pid
		end
	end
	return nil, nil, nil
end

local function captured_pane(pane_id)
	return if pane_id and pane_id ~= ""
		then shell.run("tmux capture-pane -t " .. quote(pane_id) .. " -p -S -30 " .. stderr_null)
		else ""
end

local function git_branch(cwd)
	if not cwd or cwd == "" then
		return nil
	end
	local branch = shell.run("git -C " .. quote(cwd) .. " rev-parse --abbrev-ref HEAD " .. stderr_null)
	local function unusable(value)
		return value == "" or ui.starts_with(value, "bootty.run:") or string.find(value, "\n", 1, true) ~= nil
	end
	if unusable(branch) then
		return nil
	end
	-- Detached HEAD reports "HEAD" here. Return the short commit (not nil) so the
	-- caller does not preserve the last real branch name, which would read as a stale
	-- branch with no sign the session has detached. nil stays reserved for the
	-- error/empty case, where preserving the previous branch is correct.
	if branch == "HEAD" then
		local short = shell.run("git -C " .. quote(cwd) .. " rev-parse --short HEAD " .. stderr_null)
		return if unusable(short) then "detached" else "detached " .. short
	end
	return branch
end


local function git_diff(cwd)
	if not cwd or cwd == "" then
		return nil, nil
	end
	local added = 0
	local removed = 0
	for line in string.gmatch(shell.run("git -C " .. quote(cwd) .. " diff HEAD --numstat " .. stderr_null), "[^\n]+") do
		local add, remove = string.match(line, "^(%d+)\t(%d+)\t")
		added += tonumber(add) or 0
		removed += tonumber(remove) or 0
	end
	if added == 0 and removed == 0 then
		return nil, nil
	end
	return added, removed
end

local function command_basename(command)
	if not command or command == "" then
		return nil
	end
	local name = string.match(command, "([^/]+)$") or command
	name = string.gsub(name, "^%-+", "")
	return string.lower(name)
end

local function command_agent_name(command)
	local lower = command_basename(command)
	if lower == "codex.app" then
		return "codex"
	elseif lower == "claude.app" then
		return "claude"
	end
	return if lower == "claude" or lower == "codex" or lower == "opencode" then lower else nil
end

local INTERPRETER_WRAPPERS = {
	bun = true,
	deno = true,
	node = true,
	python = true,
	python3 = true,
	ruby = true,
}

local function args_tokens(args)
	local tokens = {}
	for token in string.gmatch(args or "", "%S+") do
		tokens[#tokens + 1] = token
	end
	return tokens
end

local function wrapper_script_agent(command, args)
	local wrapper = command_basename(command)
	if not INTERPRETER_WRAPPERS[wrapper] then
		return nil
	end
	local tokens = args_tokens(args)
	for index = 2, #tokens do
		local token = tokens[index]
		if not ui.starts_with(token, "-") then
			return command_agent_name(token)
		end
	end
	return nil
end

local function process_snapshot()
	local children = {}
	for line in string.gmatch(shell.run("ps -axo pid=,ppid=,comm=,args= " .. stderr_null), "[^\n]+") do
		local pid, ppid, command, args = string.match(line, "^%s*(%d+)%s+(%d+)%s+(%S+)%s*(.*)$")
		if pid and ppid and command then
			if not children[ppid] then
				children[ppid] = {}
			end
			local siblings = children[ppid]
			siblings[#siblings + 1] = {
				pid = pid,
				command = command_basename(command),
				agent = command_agent_name(command) or wrapper_script_agent(command, args),
			}
		end
	end
	return children
end

local function agent_command(command)
	return command_agent_name(command)
end

local function descendant_agent(children, root_pid)
	if not root_pid or root_pid == "" then
		return nil
	end
	local queue = { root_pid }
	local seen = {}
	local head = 1
	while head <= #queue and head < 256 do
		local pid = queue[head]
		head += 1
		if not seen[pid] then
			seen[pid] = true
			for _, child in ipairs(children[pid] or {}) do
				local agent = child.agent or agent_command(child.command)
				if agent then
					return agent
				end
				queue[#queue + 1] = child.pid
			end
		end
	end
	return nil
end

local function is_input_separator(line)
	local count = 0
	for char in string.gmatch(line, utf8.charpattern) do
		if char ~= "─" then
			return false
		end
		count += 1
	end
	return count >= 20
end

local function gerund_from_line(line)
	local marks = { "·", "•", "✻", "⋆", "✦", "✧", "✶", "✢", "✽", "✳" }
	for _, mark in ipairs(marks) do
		local prefix = mark .. " "
		if ui.starts_with(line, prefix) then
			local rest = string.sub(line, #prefix + 1)
			if string.find(rest, "…", 1, true) then
				local word = string.match(rest, "^(%S+)") or ""
				word = string.gsub(word, "…$", "")
				return if #word >= 2 then word .. "…" else nil
			end
		end
	end
	return nil
end

local function parse_star_activity(text)
	local lines = {}
	for line in string.gmatch(text, "[^\n]+") do
		lines[#lines + 1] = line
	end
	local scan_until = #lines
	for index = #lines, 1, -1 do
		if is_input_separator(ui.trim(lines[index])) then
			scan_until = index - 1
			for top = index - 1, 1, -1 do
				if is_input_separator(ui.trim(lines[top])) then
					scan_until = top - 1
					break
				end
			end
			break
		end
	end
	for index = scan_until, 1, -1 do
		local line = lines[index]
		local trimmed = ui.trim(line)
		if trimmed ~= "" and not string.find(trimmed, "٪", 1, true) then
			local first = string.sub(line, 1, 1)
			if first ~= " " and first ~= "\t" then
				return gerund_from_line(trimmed)
			end
		end
	end
	return nil
end

local function parse_codex_activity(text)
	for line in string.gmatch(text, "[^\n]+") do
		local trimmed = ui.trim(line)
		if ui.starts_with(trimmed, "• ") then
			local verb = string.match(trimmed, "^•%s*(%S+)")
			if verb == "Working" or verb == "Thinking" or verb == "Running" then
				return verb .. "…"
			end
		end
	end
	return nil
end

local function parse_opencode_activity(text)
	local lines = {}
	for line in string.gmatch(text, "[^\n]+") do
		lines[#lines + 1] = line
	end
	local tail = {}
	for index = math.max(1, #lines - 9), #lines do
		tail[#tail + 1] = lines[index]
	end
	local bottom = table.concat(tail, " ")
	return if string.find(bottom, "esc", 1, true) and string.find(bottom, "interrupt", 1, true)
		then "Working…"
		else nil
end

local function agent_status(agent, text)
	local activity = nil
	if agent == "claude" then
		activity = parse_star_activity(text)
	elseif agent == "codex" then
		activity = parse_codex_activity(text)
	else
		activity = parse_opencode_activity(text)
	end
	if activity then
		return activity
	end
	local lower = string.lower(text or "")
	local asking = false
	if agent == "claude" then
		asking = string.find(lower, "enter to select", 1, true) ~= nil
	elseif agent == "codex" then
		asking = string.find(lower, "to submit answer", 1, true) ~= nil
	else
		asking = string.find(lower, "select", 1, true) ~= nil
			and string.find(lower, "submit", 1, true) ~= nil
			and string.find(lower, "dismiss", 1, true) ~= nil
	end
	return if asking then "Waiting…" else nil
end

local function hex_to_rgb(color)
	local r, g, b = string.match(color or "", "^#(%x%x)(%x%x)(%x%x)$")
	if not r then
		return 255, 255, 255
	end
	return tonumber(r, 16), tonumber(g, 16), tonumber(b, 16)
end

local function rgb_to_hex(r, g, b)
	return string.format(
		"#%02x%02x%02x",
		ui.clamp(math.floor(r + 0.5), 0, 255),
		ui.clamp(math.floor(g + 0.5), 0, 255),
		ui.clamp(math.floor(b + 0.5), 0, 255)
	)
end

local function scale_color(color, factor)
	local r, g, b = hex_to_rgb(color)
	return rgb_to_hex(r * factor, g * factor, b * factor)
end

local function mix_color(from_color, to_color, amount)
	local ar, ag, ab = hex_to_rgb(from_color)
	local br, bg, bb = hex_to_rgb(to_color)
	return rgb_to_hex(ar + (br - ar) * amount, ag + (bg - ag) * amount, ab + (bb - ab) * amount)
end

local function pulse_amount()
	local t = (bootty.time() % PULSE_PERIOD) / PULSE_PERIOD
	return 1 - math.abs(2 * t - 1)
end

local function activity_color(facts)
	local amount = pulse_amount()
	if facts.agent_detail == "Waiting…" then
		return mix_color(scale_color(WAITING_COLOR, 0.7), scale_color(WAITING_COLOR, 1.25), amount)
	elseif facts.agent_name == "claude" then
		return mix_color(PERC_BASE, PERC_SHINE, amount)
	elseif facts.agent_style then
		return mix_color(scale_color(facts.agent_style.color, 0.6), scale_color(facts.agent_style.color, 1.5), amount)
	end
	return MUTED_COLOR
end

local function cycled_activity(facts)
	if facts.agent_detail == "Waiting…" then
		return facts.agent_detail
	end
	if facts.agent_name == "codex" then
		return CODEX_VERBS[math.floor(bootty.time() / 8) % #CODEX_VERBS + 1]
	elseif facts.agent_name == "opencode" then
		return OPENCODE_VERBS[math.floor(bootty.time() / 8) % #OPENCODE_VERBS + 1]
	end
	return facts.agent_detail
end

local function minimal_facts()
	return {}
end

local function refreshed_session_facts(session)
	local process, pane_id, pane_pid = tmux_active_pane(session)
	local agent_name = agent_command(process)
	if not agent_name and pane_pid then
		agent_name = descendant_agent(process_snapshot(), pane_pid)
	end
	local agent_detail = if agent_name then agent_status(agent_name, captured_pane(pane_id)) else nil
	local diff_added, diff_removed = git_diff(session.cwd)
	return {
		display_process = agent_name or process,
		branch = git_branch(session.cwd),
		branch_status = "current",
		diff_added = diff_added,
		diff_removed = diff_removed,
		agent_name = agent_name,
		agent_detail = agent_detail,
		agent_style = AGENT_STYLE[agent_name],
	}
end

local function cached_session_facts(session)
	return FACTS_CACHE[session.id] or minimal_facts()
end

local function prune_fact_cache(sessions)
	local live = {}
	for _, session in ipairs(sessions) do
		live[session.id] = true
	end
	for id, _ in pairs(FACTS_CACHE) do
		if not live[id] then
			FACTS_CACHE[id] = nil
		end
	end
end

local function next_refresh_session(sessions)
	if #sessions == 0 then
		return nil
	end
	for _, session in ipairs(sessions) do
		if session.selected and not FACTS_CACHE[session.id] then
			return session
		end
	end
	if refresh_index > #sessions then
		refresh_index = 1
	end
	local session = sessions[refresh_index]
	refresh_index = refresh_index % #sessions + 1
	return session
end

local function refresh_one_session(sessions)
	local session = next_refresh_session(sessions)
	if not session then
		return
	end
	local previous = FACTS_CACHE[session.id]
	local refreshed = refreshed_session_facts(session)
	if refreshed.branch == nil then
		refreshed.branch_status = if previous and previous.branch then "stale" else "unknown"
		refreshed.branch = if previous then previous.branch else nil
	end
	FACTS_CACHE[session.id] = refreshed
end

local provider = {}

function provider.refresh(sessions)
	local now = bootty.time()
	prune_fact_cache(sessions)
	if first_render then
		first_render = false
		last_fact_refresh = now
		refresh_one_session(sessions)
	elseif now - last_fact_refresh >= FACT_REFRESH_INTERVAL then
		last_fact_refresh = now
		refresh_one_session(sessions)
	end
end

function provider.get(session)
	return cached_session_facts(session)
end

function provider.activity(facts)
	return cycled_activity(facts), activity_color(facts)
end

return provider
end

return sidebar
