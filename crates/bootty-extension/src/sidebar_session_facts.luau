--!strict
-- Session facts shared by the built-in session modules: the mux snapshot, the worktree, and
-- whichever coding agent is running in a session's pane.
local sidebar = {}
local shared_provider = nil

function sidebar.session_facts(options)
	if shared_provider then
		return shared_provider
	end
	options = options or {}

	local ui = bootty.ui

	local MUTED_COLOR = bootty.theme.muted
	local CODEX_COLOR = bootty.theme.accent
	local CLAUDE_COLOR = bootty.theme.warning
	local OPENCODE_COLOR = bootty.theme.primary
	local WAITING_COLOR = bootty.theme.warning
	local PERC_BASE = bootty.theme.destructive
	local PERC_SHINE = bootty.theme.warning

	local PULSE_PERIOD = 2.0
	-- How stale a session's git and agent facts may get. Each refresh runs a `git diff` for that
	-- session's repo, so the session being watched is worth checking often and the rest are not.
	local FOCUSED_FACT_INTERVAL = options.fact_refresh_interval or 5
	local BACKGROUND_FACT_INTERVAL = options.background_fact_refresh_interval or 30
	-- Floor between refreshes of sessions that already have facts, so a batch coming due together
	-- spreads out instead of shelling out all at once.
	local REFRESH_SPACING = 0.5
	-- Start one cold session per frame, spreading a fresh sidebar's subprocesses instead of firing the
	-- whole list together.
	local COLD_REFRESH_BURST = 1
	local COLD_REFRESH_SPACING = 1 / 20
	-- Cadence for the facts that start no process at all — the pane's command and the repo's `HEAD`.
	local LIVE_FACT_INTERVAL = 0.25
	-- However quiet a worktree looks, count its diff again this often. A watcher can miss events, and
	-- an unwatched tree reports no revision at all.
	local FORCED_DIFF_INTERVAL = 60
	-- How long a session that has left the sidebar keeps its facts, so switching spaces and coming
	-- back shows them immediately.
	local FACT_CACHE_TTL = 300
	local CODEX_VERBS = { "Codexing…", "Working…", "Thingamabobbing…" }
	local OPENCODE_VERBS = { "Opencodding…", "Opendoing…", "Shming Shmopenig…" }

	local AGENT_STYLE = {
		codex = { icon = "openai", color = CODEX_COLOR },
		claude = { icon = "claude", color = CLAUDE_COLOR },
		opencode = { icon = "bot", color = OPENCODE_COLOR },
	}

	local FACTS_CACHE = {}
	local last_fact_refresh = 0
	local last_cold_refresh = nil

	local function tmux_session(session)
		return string.sub(session.id, 1, 1) == "$"
	end

	-- The active pane's command, id, and pid, straight from the mux snapshot bootty already polls.
	-- Asking tmux again here cost a shell and a `tmux list-panes` per session per refresh.
	local function active_pane(session)
		if not tmux_session(session) then
			return nil, nil, nil
		end
		local command = session.process
		if command == "" then
			command = nil
		end
		return command, session.pane_id, session.pane_pid
	end

	local function capture_pane_argv(pane_id)
		return { "tmux", "capture-pane", "-t", pane_id, "-p", "-S", "-30" }
	end

	local function git_numstat_argv(cwd)
		return { "git", "-C", cwd, "diff", "HEAD", "--numstat" }
	end

	local function diff_counts_from(numstat)
		local added = 0
		local removed = 0
		for line in string.gmatch(numstat, "[^\n]+") do
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

	local function agent_command(command)
		return command_agent_name(command)
	end

	-- `bootty.descendants` walks the process tree natively, breadth-first, so the shallowest agent
	-- below the pane wins.
	local function descendant_agent(root_pid)
		local pid = tonumber(root_pid)
		if not pid then
			return nil
		end
		for _, descendant in ipairs(bootty.descendants(pid)) do
			local agent = command_agent_name(descendant.command)
				or wrapper_script_agent(descendant.command, descendant.args)
			if agent then
				return agent
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

	-- A session's facts, filled in by the two refreshes below: `display_process`, `branch` and
	-- `branch_status` by the live one, `diff_added`/`diff_removed`, `agent_name`, `agent_detail`,
	-- `agent_style` and `tree_agent_name` by the deep one. `tree_agent_name` is the agent found by
	-- walking the pane's process tree, kept apart so a live refresh can re-derive the effective agent
	-- without losing what only the deep refresh can see.
	local function session_cache_key(session)
		return session.cache_key or session.id
	end

	local function session_identity(session)
		return session_cache_key(session) .. "\0" .. (session.cwd or "") .. "\0" .. (session.pane_id or "")
	end

	local function new_facts(session, now)
		return {
			identity = session_identity(session),
			branch_status = "unknown",
			refreshed_at = 0,
			live_at = 0,
			seen_at = now,
			-- Worktree revision the diff counts were taken at, when, and whether any count has landed.
			diff_revision = 0,
			diff_counted_at = 0,
			diff_counted = false,
		}
	end

	local function consume_completed_facts(session, facts)
		local _, pane_id = active_pane(session)
		if facts.agent_name and pane_id and pane_id ~= "" then
			local text = bootty.read(capture_pane_argv(pane_id))
			if text ~= "" then
				facts.agent_detail = agent_status(facts.agent_name, text)
			end
		end
		if session.cwd and session.cwd ~= "" then
			local numstat, counted = bootty.read(git_numstat_argv(session.cwd))
			if counted then
				facts.diff_added, facts.diff_removed = diff_counts_from(numstat)
				facts.diff_counted = true
			end
		end
	end

	-- Everything a row can know without starting a process: the pane's command from the mux snapshot,
	-- the branch from the repo's `HEAD`, and whatever the commands below last printed. Reading their
	-- output here is what makes an answer appear the moment it lands, rather than when its next turn
	-- comes round — the read costs nothing and never starts anything.
	local function apply_live_facts(session, facts, now)
		local process, pane_id = active_pane(session)
		local branch = if session.cwd and session.cwd ~= "" then bootty.git.branch(session.cwd) else nil
		if branch then
			facts.branch = branch
			facts.branch_status = "current"
		elseif facts.branch then
			facts.branch_status = "stale"
		end
		-- The pane's own command is what changes when a command starts or exits; an agent found deeper in
		-- the tree keeps its name until the next deep pass looks again.
		facts.agent_name = agent_command(process) or facts.tree_agent_name
		facts.agent_style = AGENT_STYLE[facts.agent_name]
		facts.display_process = facts.agent_name or process
		if not facts.agent_name or not pane_id or pane_id == "" then
			facts.agent_detail = nil
		end
		consume_completed_facts(session, facts)
		facts.live_at = now
	end

	-- Starts the commands whose answers the live pass reads, and does the one piece of work that has to
	-- happen here: walking the pane's process tree to find an agent running under a wrapper.
	local function start_deep_refresh(session, facts, now)
		local process, pane_id, pane_pid = active_pane(session)
		if not agent_command(process) and pane_pid then
			facts.tree_agent_name = descendant_agent(pane_pid)
		else
			facts.tree_agent_name = nil
		end
		if (agent_command(process) or facts.tree_agent_name) and pane_id and pane_id ~= "" then
			bootty.exec(capture_pane_argv(pane_id))
		end
		if session.cwd and session.cwd ~= "" then
			-- Counting a diff runs git over the whole worktree. The filesystem already says when something
			-- under that tree changed, so a tree that has not moved since the last count is not worth
			-- asking about again — what an editor's git integration does instead of polling. A tree nobody
			-- is watching reports 0, and then this always asks.
			local revision = bootty.git.worktree_revision(session.cwd)
			local settled = facts.diff_counted and revision ~= 0 and revision == facts.diff_revision
			if not settled or now - facts.diff_counted_at >= FORCED_DIFF_INTERVAL then
				bootty.exec(git_numstat_argv(session.cwd))
				facts.diff_revision = revision
				facts.diff_counted_at = now
			end
		end
		facts.refreshed_at = now
	end

	local function cached_session_facts(session)
		return FACTS_CACHE[session_cache_key(session)] or minimal_facts()
	end

	-- Sessions leave the list whenever the sidebar shows a different space, and their facts are still
	-- true when they come back. Entries are kept until nothing has referred to them for a while, so
	-- switching spaces shows what it showed before instead of refilling a row at a time.
	local function prune_fact_cache(sessions, now)
		for _, session in ipairs(sessions) do
			local cached = FACTS_CACHE[session_cache_key(session)]
			if cached then
				cached.seen_at = now
			end
		end
		for id, cached in pairs(FACTS_CACHE) do
			if now - (cached.seen_at or 0) > FACT_CACHE_TTL then
				FACTS_CACHE[id] = nil
			end
		end
	end

	local function deep_interval(session)
		return if session.selected then FOCUSED_FACT_INTERVAL else BACKGROUND_FACT_INTERVAL
	end

	-- Cheap facts for every visible session, on their own short cadence. Nothing here starts a process,
	-- so this is the part that keeps a row's branch and process honest between deep refreshes.
	local function refresh_live_facts(sessions, now)
		for _, session in ipairs(sessions) do
			local key = session_cache_key(session)
			local facts = FACTS_CACHE[key]
			if not facts or facts.identity ~= session_identity(session) then
				facts = new_facts(session, now)
				FACTS_CACHE[key] = facts
			end
			-- Completed subprocess output is consumed on every module frame, so an answer is visible on the
			-- first frame after it lands rather than waiting for the cheap live-fact cadence.
			consume_completed_facts(session, facts)
			if now - facts.live_at >= LIVE_FACT_INTERVAL then
				apply_live_facts(session, facts, now)
			end
		end
	end

	-- A session nobody has deep facts for yet is worth several at once: the subprocesses behind them run
	-- in the background, so a fresh sidebar fills in one pass instead of one row per tick.
	local function refresh_unknown_sessions(sessions, now)
		local refreshed = 0
		for _, session in ipairs(sessions) do
			if refreshed >= COLD_REFRESH_BURST then
				break
			end
			local facts = FACTS_CACHE[session_cache_key(session)]
			if facts.refreshed_at == 0 then
				start_deep_refresh(session, facts, now)
				refreshed += 1
			end
		end
		return refreshed
	end

	-- The session being watched earns the short interval; the rest are checked rarely, and a set of
	-- sessions that are all current starts no processes at all.
	local function refresh_most_overdue_session(sessions, now)
		local due_session, due_by = nil, 0
		for _, session in ipairs(sessions) do
			local facts = FACTS_CACHE[session_cache_key(session)]
			local overdue = now - facts.refreshed_at - deep_interval(session)
			if overdue > 0 and overdue >= due_by then
				due_session, due_by = session, overdue
			end
		end
		if due_session then
			start_deep_refresh(due_session, FACTS_CACHE[session_cache_key(due_session)], now)
		end
		return due_session ~= nil
	end

	local provider = {}

	function provider.refresh(sessions)
		local now = bootty.time()
		prune_fact_cache(sessions, now)
		refresh_live_facts(sessions, now)
		if (not last_cold_refresh or now - last_cold_refresh >= COLD_REFRESH_SPACING)
			and refresh_unknown_sessions(sessions, now) > 0
		then
			last_cold_refresh = now
			last_fact_refresh = now
			return
		end
		-- Anything already answered waits for its interval, and never runs two refreshes back to back.
		if now - last_fact_refresh < REFRESH_SPACING then
			return
		end
		if refresh_most_overdue_session(sessions, now) then
			last_fact_refresh = now
		end
	end

	function provider.get(session)
		return cached_session_facts(session)
	end

	function provider.activity(facts)
		return cycled_activity(facts), activity_color(facts)
	end

	shared_provider = provider
	return provider
end

return sidebar
