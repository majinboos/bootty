--!strict
-- Session facts come only from mux state and the worktree.
-- Agent state comes from native agent integrations under extensions/agents.

local sidebar = {}
local shared_provider = nil

function sidebar.session_facts(options)
	if shared_provider then
		return shared_provider
	end
	options = options or {}
	local focused_interval = options.fact_refresh_interval or 5
	local background_interval = options.background_fact_refresh_interval or 30
	local forced_diff_interval = 60
	local cache_ttl = 300
	local cache = {}
	local last_refresh = 0

	local function key(session)
		return session.cache_key or session.id
	end

	local function identity(session)
		return key(session) .. "\0" .. (session.cwd or "")
	end

	local function git_numstat(cwd)
		return { "git", "-C", cwd, "diff", "HEAD", "--numstat" }
	end

	local function diff_counts(value)
		local added = 0
		local removed = 0
		for line in string.gmatch(value, "[^\n]+") do
			local add, remove = string.match(line, "^(%d+)\t(%d+)\t")
			added += tonumber(add) or 0
			removed += tonumber(remove) or 0
		end
		if added == 0 and removed == 0 then
			return nil, nil
		end
		return added, removed
	end

	local function ensure(session, now)
		local current = cache[key(session)]
		if not current or current.identity ~= identity(session) then
			current = {
				identity = identity(session),
				branch_status = "unknown",
				refreshed_at = 0,
				seen_at = now,
				diff_revision = 0,
				diff_counted_at = 0,
				diff_counted = false,
			}
			cache[key(session)] = current
		end
		current.seen_at = now
		return current
	end

	local function read_answers(session, facts)
		facts.display_process = session.process
		if session.cwd and session.cwd ~= "" then
			local branch = bootty.git.branch(session.cwd)
			if branch then
				facts.branch = branch
				facts.branch_status = "current"
			elseif facts.branch then
				facts.branch_status = "stale"
			end
			local numstat, counted = bootty.read(git_numstat(session.cwd))
			if counted then
				facts.diff_added, facts.diff_removed = diff_counts(numstat)
				facts.diff_counted = true
			end
		end
	end

	local function start_diff(session, facts, now)
		if not session.cwd or session.cwd == "" then
			return
		end
		local revision = bootty.git.worktree_revision(session.cwd)
		local current = facts.diff_counted and revision ~= 0 and revision == facts.diff_revision
		if not current or now - facts.diff_counted_at >= forced_diff_interval then
			bootty.exec(git_numstat(session.cwd))
			facts.diff_revision = revision
			facts.diff_counted_at = now
		end
		facts.refreshed_at = now
	end

	local provider = {}

	function provider.refresh(sessions)
		local now = bootty.time()
		local selected = nil
		local most_overdue = 0
		for _, session in ipairs(sessions) do
			local facts = ensure(session, now)
			read_answers(session, facts)
			local interval = if session.selected then focused_interval else background_interval
			local overdue = now - facts.refreshed_at - interval
			if facts.refreshed_at == 0 or overdue > most_overdue then
				selected = session
				most_overdue = math.max(overdue, 0.001)
			end
		end
		if selected and now - last_refresh >= 0.5 then
			start_diff(selected, cache[key(selected)], now)
			last_refresh = now
		end
		for id, facts in pairs(cache) do
			if now - facts.seen_at > cache_ttl then
				cache[id] = nil
			end
		end
	end

	function provider.get(session)
		return cache[key(session)] or {}
	end

	shared_provider = provider
	return provider
end

return sidebar
