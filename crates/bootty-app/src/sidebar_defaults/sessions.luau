--!strict
-- Built-in sidebar `sessions` module. Override with ~/.config/bootty/sidebar/sessions.luau.
-- This file owns the visible structure; bootty.sidebar.session_facts only supplies optional enrichment.
local ui = bootty.ui
local facts = bootty.sidebar.session_facts()

local SUBTEXT_COLOR = bootty.theme.subtext
local SUCCESS_COLOR = bootty.theme.success
local DESTRUCTIVE_COLOR = bootty.theme.destructive

local function session_right(_, enriched)
	if enriched.diff_added and enriched.diff_removed then
		return {
			ui.text(
				"+" .. tostring(enriched.diff_added),
				{ x = 1, x_px = -62, color = SUCCESS_COLOR, align = "right_center", size = 12 }
			),
			ui.text(
				"-" .. tostring(enriched.diff_removed),
				{ x = 1, x_px = -12, color = DESTRUCTIVE_COLOR, align = "right_center", size = 12 }
			),
		}
	elseif enriched.display_process and enriched.display_process ~= "" then
		return {
			ui.text(
				enriched.display_process,
				{ x = 1, x_px = -12, color = SUBTEXT_COLOR, align = "right_center", size = 12 }
			),
		}
	end
	return {}
end

local function session_details(ctx, enriched)
	local activity = facts.activity(enriched)
	local rows = {
		{
			key = "agent",
			icon = if enriched.agent_style then enriched.agent_style.icon else nil,
			label = enriched.agent_name,
			color = if enriched.agent_style then enriched.agent_style.color else nil,
			right = activity,
			right_color = SUBTEXT_COLOR,
		},
		{
			key = "cwd",
			icon = "folder",
			label = bootty.path.display(ctx.session.cwd or "unknown"),
			color = SUBTEXT_COLOR,
		},
		{
			key = "branch",
			icon = "git-branch",
			label = enriched.branch or "unknown",
			right = if enriched.branch_status ~= "current" then enriched.branch_status else nil,
			color = SUBTEXT_COLOR,
		},
	}
	if #ctx.session.ports > 0 then
		rows[#rows + 1] = {
			key = "ports",
			icon = "plug",
			label = table.concat(ctx.session.ports, ", "),
			color = SUBTEXT_COLOR,
		}
	end
	return rows
end

local function session_progress(ctx, _)
	local rows = {}
	for index, progress in ipairs(ctx.session.progresses) do
		rows[#rows + 1] = {
			key = "progress:" .. tostring(index),
			value = progress.value,
			indeterminate = progress.indeterminate,
			track = bootty.theme.border,
			fill = ctx.session.color or bootty.theme.accent,
			label = progress.process,
			label_width = 72,
			label_color = SUBTEXT_COLOR,
		}
	end
	return rows
end

return {
	interval = 1 / 120,
	render = function()
		local sessions = bootty.sessions()
		facts.refresh(sessions)
		return ui.session_items({
			sessions = sessions,
			facts = facts.get,
			right = session_right,
			details = session_details,
			progress = session_progress,
		})
	end,
}
