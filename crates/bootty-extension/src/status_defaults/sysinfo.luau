--!strict
-- Built-in `sysinfo` module: keep-awake toggle, load/CPU, memory, and power
-- status from native cross-platform metrics. Override by creating
-- ~/.config/bootty/extensions/sysinfo.luau.

return {
	interval = 2,
	render = function()
		local m = bootty.metrics()
		local function battery_time()
			local secs = m.battery_time_to_full or m.battery_time_to_empty
			if secs == nil then
				return nil
			end
			local minutes = math.floor((secs / 60) + 0.5)
			local hours = math.floor(minutes / 60)
			minutes = minutes % 60
			return string.format("%d:%02d", hours, minutes)
		end

		local items = {}
		local function cell(item)
			if item.bg == nil then
				item.bg = if #items % 2 == 0 then bootty.theme.surface else bootty.theme.hover
			end
			items[#items + 1] = item
		end

		local awake = bootty.awake()
		cell({
			icon = if awake then "coffee-cup-filled" else "coffee-cup",
			fg = if awake then bootty.theme.base else bootty.theme.subtext,
			bg = if awake then bootty.theme.success else nil,
			action = "toggle-caffeinate",
		})

		if m.load1 > 0 then
			cell({
				icon = "cpu",
				text = string.format("%.2f", m.load1),
				fg = if m.load1 >= 4 then bootty.theme.warning else bootty.theme.subtext,
			})
		else
			cell({
				icon = "cpu",
				text = string.format("%.0f%%", m.cpu),
				fg = if m.cpu >= 80 then bootty.theme.warning else bootty.theme.subtext,
			})
		end

		cell({ icon = "memory-stick", text = string.format("%.0f%%", m.mem_pct), fg = bootty.theme.subtext })

		if m.battery ~= nil then
			local remaining = battery_time()
			local low = (not m.on_ac) and m.battery <= 20
			local battery_icon = nil
			if m.on_ac then
				battery_icon = if m.battery >= 99.5 then "battery-full" else "plug"
			end
			cell({
				gauge = m.battery / 100,
				icon = battery_icon,
				text = if remaining then string.format("%.0f%% %s", m.battery, remaining) else string.format("%.0f%%", m.battery),
				fg = if low then bootty.theme.warning elseif m.on_ac then bootty.theme.success else bootty.theme.text,
			})
		else
			cell({ icon = "plug-zap", fg = bootty.theme.success })
		end

		return items
	end,
}
