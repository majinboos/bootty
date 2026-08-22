--!strict
-- Session facts shared by the built-in session modules: the pane's command from the mux snapshot,
-- and the branch and diff counts of the session's worktree.
local sidebar = {}
local shared_provider = nil

function sidebar.session_facts(options)
	if shared_provider then
		return shared_provider
	end
	options = options or {}

	local ui = bootty.ui

	-- How stale a session's git facts may get. Each refresh runs a `git diff` for that
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

	local FACTS_CACHE = {}
	local last_fact_refresh = 0
	local last_cold_refresh = nil

	local function tmux_session(session)
		return string.sub(session.id, 1, 1) == "$"
	end

	-- The active pane's command, straight from the mux snapshot bootty already polls. Asking tmux
	-- again here cost a shell and a `tmux list-panes` per session per refresh.
	local function active_pane(session)
		if not tmux_session(session) then
			return nil
		end
		local command = session.process
		return if command == "" then nil else command
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


	local function minimal_facts()
		return {}
	end

	-- A session's facts, filled in by the two refreshes below: `display_process`, `branch` and
	-- `branch_status` by the live one, `diff_added`/`diff_removed` by the deep one.
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
		local process = active_pane(session)
		local branch = if session.cwd and session.cwd ~= "" then bootty.git.branch(session.cwd) else nil
		if branch then
			facts.branch = branch
			facts.branch_status = "current"
		elseif facts.branch then
			facts.branch_status = "stale"
		end
		facts.display_process = process
		consume_completed_facts(session, facts)
		facts.live_at = now
	end

	-- Starts the commands whose answers the live pass reads.
	local function start_deep_refresh(session, facts, now)
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

	shared_provider = provider
	return provider
end

return sidebar
