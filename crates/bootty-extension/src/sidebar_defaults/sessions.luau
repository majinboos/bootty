--!strict
-- Built-in sidebar `sessions` module. Override with ~/.config/bootty/extensions/sessions.luau.
-- Owns session grouping and summary rows. Other extension files add per-session content.
local ui = bootty.ui

return {
	interval = ui.animation_interval,
	render = function()
		return ui.session_items({ sessions = bootty.sessions() })
	end,
}
