--!strict
-- Built-in `clock` module: date and time with lucide icons. Override by creating
-- ~/.config/bootty/extensions/clock.luau.

return {
	interval = 1,
	render = function()
		return {
			{ icon = "calendar", text = os.date("%a %b %d"), fg = bootty.theme.subtext },
			{ icon = "clock", text = os.date("%H:%M:%S"), fg = bootty.theme.text },
		}
	end,
}
