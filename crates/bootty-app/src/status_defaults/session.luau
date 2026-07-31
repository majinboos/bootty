--!strict
-- Built-in `session` module: the active session name when the sidebar is hidden.
-- Override by creating ~/.config/bootty/status/session.luau.

return {
	interval = 0.5,
	render = function()
		if bootty.sidebar.visible() then
			return {}
		end
		local name = bootty.session()
		if not name then
			return {}
		end
		return { icon = "folder", text = name, fg = bootty.theme.accent }
	end,
}
