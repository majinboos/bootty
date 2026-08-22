--!strict
-- Built-in `windows` module: the active session's windows as tmux-style mux
-- cells. Override by creating ~/.config/bootty/extensions/windows.luau.

return {
	interval = 0.2,
	render = function()
		-- Tint the active window tab with the focused session's color so the status bar matches the
		-- sidebar; falls back to the theme accent when no session color is available.
		local accent = bootty.session_color() or bootty.theme.accent
		local windows = bootty.windows()
		local items = {}
		local radius = 6
		local wedge = 8
		local progress_height = 2

		local function left_radius()
			return { nw = radius, sw = radius }
		end

		local function right_radius(enabled)
			return { ne = if enabled then radius else 0, se = if enabled then radius else 0 }
		end

		local function rect(fill, corners)
			return { type = "rect", fill = fill, radius = corners }
		end

		local function underlay(fill)
			return { type = "rect", fill = fill, w = 0, w_px = radius }
		end

		local function previous_tab_fill(index)
			if index <= 1 then
				return nil
			end
			return if windows[index - 1].active then bootty.theme.surface else bootty.theme.base
		end


		local function left_chevron(fill)
			return {
				type = "polygon",
				fill = fill,
				points = {
					{ x = 0, dx = -1, y = 0 },
					{ x = 0, dx = wedge, y = 0.5 },
					{ x = 0, dx = -1, y = 1 },
				},
			}
		end

		local function push(window, item)
			item.action = "activate-window:" .. window.id
			-- Both cells of a window share the window id, so they form one draggable tab.
			item.reorder_anchor = window.id
			items[#items + 1] = item
		end

		for index, window in ipairs(windows) do
			local last = index == #windows
			local active = window.active
			local index_fill = if active then accent else bootty.theme.base
			local name_fill = if active then bootty.theme.surface else bootty.theme.base
			local index_fg = if active then bootty.theme.base else bootty.theme.subtext
			local name_fg = if active then bootty.theme.text else bootty.theme.subtext

			local index_primitives = {}
			local previous_fill = previous_tab_fill(index)
			if previous_fill then
				index_primitives[#index_primitives + 1] = underlay(previous_fill)
			end
			index_primitives[#index_primitives + 1] = rect(index_fill, left_radius())

			local name_primitives = { rect(name_fill, right_radius(last)) }
			if active then
				name_primitives[#name_primitives + 1] = left_chevron(accent)
			end
			if window.progress then
				local width = if window.progress_indeterminate then 0.25 else window.progress / 100
				name_primitives[#name_primitives + 1] = {
					type = "rect",
					fill = bootty.theme.border,
					x = 0,
					y = 1,
					y_px = -progress_height,
					w = 1,
					h = 0,
					h_px = progress_height,
				}
				name_primitives[#name_primitives + 1] = {
					type = "rect",
					fill = accent,
					x = 0,
					y = 1,
					y_px = -progress_height,
					w = width,
					h = 0,
					h_px = progress_height,
					-- The host sweeps an indeterminate bar off the frame clock, so it animates
					-- smoothly instead of stepping at this module's render interval.
					sweep = window.progress_indeterminate,
				}
			end

			push(window, {
				text = tostring(window.index),
				fg = index_fg,
				primitives = index_primitives,
				join = false,
				gap = false,
			})
			push(window, {
				text = window.name,
				fg = name_fg,
				primitives = name_primitives,
				pad_left = wedge,
				gap = false,
			})
		end
		return items
	end,
}
