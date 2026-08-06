--!strict
-- Built-in sidebar `sessions` module. Override with ~/.config/bootty/sidebar/sessions.luau.
-- Owns session grouping and summary rows. Per-session content lives in ~/.config/bootty/session/.
local ui = bootty.ui

return {
	interval = ui.animation_interval,
	render = function()
		return ui.session_items({ sessions = bootty.sessions() })
	end,
}
