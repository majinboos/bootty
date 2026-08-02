--!strict
-- Shared declarative helpers for Bootty Luau UI extensions.
-- The helpers only build the generic item/primitive tables that Rust already renders.

local ui = {}

local function opt(options)
	return options or {}
end

function ui.clamp(value, min_value, max_value)
	return math.max(min_value, math.min(max_value, value))
end

function ui.trim(value)
	return (string.gsub(value or "", "^%s*(.-)%s*$", "%1"))
end

function ui.starts_with(value, prefix)
	return string.sub(value or "", 1, #prefix) == prefix
end

function ui.shell_quote(value)
	return "'" .. string.gsub(value or "", "'", "'\\''") .. "'"
end

local function days_from_civil(year, month, day)
	if month <= 2 then
		year -= 1
	end
	local era = math.floor(year / 400)
	local yoe = year - era * 400
	local mp = month + if month > 2 then -3 else 9
	local doy = math.floor((153 * mp + 2) / 5) + day - 1
	local doe = yoe * 365 + math.floor(yoe / 4) - math.floor(yoe / 100) + doy
	return era * 146097 + doe - 719468
end

function ui.parse_iso_epoch(value)
	if not value then
		return nil
	end
	local year, month, day, hour, minute, second =
		string.match(value, "^(%d%d%d%d)%-(%d%d)%-(%d%d)T(%d%d):(%d%d):(%d%d)")
	if not year then
		return nil
	end
	return days_from_civil(tonumber(year), tonumber(month), tonumber(day)) * 86400
		+ tonumber(hour) * 3600
		+ tonumber(minute) * 60
		+ tonumber(second)
end

function ui.format_duration(secs)
	secs = math.max(0, math.floor(secs or 0))
	if secs >= 86400 then
		return string.format(
			"%dd%02d:%02d",
			math.floor(secs / 86400),
			math.floor((secs % 86400) / 3600),
			math.floor((secs % 3600) / 60)
		)
	elseif secs >= 3600 then
		return string.format("%dh%02d", math.floor(secs / 3600), math.floor((secs % 3600) / 60))
	end
	return string.format("%dm", math.floor(secs / 60))
end

function ui.push(items, item)
	if item then
		items[#items + 1] = item
	end
	return item
end

function ui.text(text, options)
	local opts = opt(options)
	return {
		type = "text",
		text = text or "",
		x = opts.x or 0,
		x_px = opts.x_px or 0,
		y = opts.y or 0.5,
		y_px = opts.y_px or 0,
		size = opts.size or 11,
		align = opts.align or "left_center",
		color = opts.color,
		min_width = opts.min_width,
	}
end
function ui.diff_summary(added, removed, options)
	local opts = opt(options)
	local size = opts.size or 12
	local right_px = opts.right_px or -12
	local removed_text = "-" .. tostring(removed)
	local removed_width = #removed_text * size * 0.6
	return {
		ui.text("+" .. tostring(added), {
			x = 1,
			x_px = right_px - (opts.gap or 10) - removed_width,
			color = opts.success,
			align = "right_center",
			size = size,
		}),
		ui.text(removed_text, {
			x = 1,
			x_px = right_px,
			color = opts.destructive,
			align = "right_center",
			size = size,
		}),
	}
end


function ui.icon(icon, options)
	local opts = opt(options)
	return {
		type = "icon",
		icon = icon,
		x = opts.x or 0,
		x_px = opts.x_px or 0,
		y = opts.y or 0.5,
		y_px = opts.y_px or 0,
		size = opts.size or 12,
		color = opts.color,
		min_width = opts.min_width,
	}
end

function ui.rect(options)
	local opts = opt(options)
	return {
		type = "rect",
		x = opts.x or 0,
		x_px = opts.x_px or 0,
		y = opts.y or 0,
		y_px = opts.y_px or 0,
		w = if opts.w ~= nil then opts.w else 1,
		w_px = opts.w_px,
		h = if opts.h ~= nil then opts.h else 1,
		h_px = opts.h_px,
		radius = opts.radius or 0,
		fill = opts.fill,
		stroke = opts.stroke,
	}
end

function ui.row_x(indent)
	return 12 + (indent or 0) * 7
end

function ui.row(spec)
	local session = spec.session
	local key = spec.key
	if key == nil and session then
		key = table.concat({
			session.id,
			spec.kind or "row",
			tostring(spec.text or ""),
			tostring(spec.indent or 0),
			tostring(spec.tree or "none"),
		}, ":")
	end
	return {
		key = key,
		kind = spec.kind or "row",
		text = spec.text or "",
		number = spec.number,
		indent = spec.indent or 0,
		tree = spec.tree or "none",
		selectable = if spec.selectable ~= nil then spec.selectable else session ~= nil,
		session_id = spec.session_id or if session then session.id else nil,
		reorder_anchor = spec.anchor or spec.reorder_anchor,
		fg = spec.fg or if session then session.color else nil,
		dim_fg = spec.dim_fg or if session then session.dim_color else nil,
		current = if spec.current ~= nil then spec.current else if session then session.selected else nil,
		active = spec.active,
		primitives = spec.primitives or {},
	}
end

function ui.group_row(spec)
	spec.kind = "group"
	if spec.selectable == nil then
		spec.selectable = false
	end
	return ui.row(spec)
end

function ui.session_row(spec)
	spec.kind = "session"
	if spec.selectable == nil then
		spec.selectable = true
	end
	if spec.active == nil and spec.session then
		spec.active = spec.session.selected
	end
	return ui.row(spec)
end

function ui.detail_row(ctx, spec)
	if not spec.label or spec.label == "" then
		return nil
	end
	local x = ui.row_x(ctx.detail_indent)
	local primitives = {
		ui.icon(spec.icon, { x_px = x + 6, color = spec.color }),
		ui.text(spec.label, { x_px = x + 16, x = 0, color = spec.color, align = "left_center", size = 11 }),
	}
	if spec.right and spec.right ~= "" then
		primitives[#primitives + 1] = ui.text(spec.right, {
			x = 1,
			x_px = -12,
			color = spec.right_color or spec.muted_color,
			align = "right_center",
			size = 11,
		})
	end
	return ui.row({
		key = if spec.key then ctx.session.id .. ":" .. spec.key else nil,
		text = spec.label,
		indent = ctx.detail_indent,
		tree = ctx.detail_tree,
		session = ctx.session,
		anchor = ctx.anchor,
		fg = ctx.session.color,
		dim_fg = ctx.session.dim_color,
		current = ctx.session.selected,
		primitives = primitives,
	})
end

function ui.bar_row(ctx, spec)
	if spec.value == nil then
		return nil
	end
	local value = ui.clamp(spec.value, 0, 100)
	local fill = if spec.indeterminate then 25 else value
	local offset = 0
	if spec.indeterminate then
		local phase = (bootty.time() % 1.5) / 1.5
		offset = (1 - fill / 100) * (1 - math.abs(2 * phase - 1))
	end
	local value_label = if spec.indeterminate then "..." else string.format("%d%%", value)
	local x = ui.row_x(ctx.detail_indent)
	local label_width = if spec.label and spec.label ~= "" then spec.label_width or 72 else 0
	local bar_x = x + label_width
	local width_px = (spec.right_px or -54) - bar_x
	local primitives = {
		ui.rect({
			x = 0,
			x_px = bar_x,
			y = 0.5,
			y_px = -1,
			w = 1,
			w_px = width_px,
			h = 0,
			h_px = 2,
			radius = 1,
			fill = spec.track,
		}),
		ui.rect({
			x = offset,
			x_px = bar_x + width_px * offset,
			y = 0.5,
			y_px = -1,
			w = fill / 100,
			w_px = width_px * fill / 100,
			h = 0,
			h_px = 2,
			radius = 1,
			fill = spec.fill,
		}),
	}
	if spec.label and spec.label ~= "" then
		primitives[#primitives + 1] = ui.text(spec.label, {
			x = 0,
			x_px = x,
			color = spec.label_color,
			align = "left_center",
			size = 11,
			min_width = 160,
		})
	end
	primitives[#primitives + 1] = ui.text(value_label, {
		x = 1,
		x_px = -12,
		color = spec.label_color,
		align = "right_center",
		size = 11,
	})
	return ui.row({
		key = if spec.key then ctx.session.id .. ":" .. spec.key else nil,
		text = "",
		indent = ctx.detail_indent,
		tree = ctx.detail_tree,
		session = ctx.session,
		anchor = ctx.anchor,
		fg = ctx.session.color,
		dim_fg = ctx.session.dim_color,
		current = ctx.session.selected,
		primitives = primitives,
	})
end

local function session_group(name)
	local slash = string.find(name, "/", 1, true)
	if slash then
		return string.sub(name, 1, slash - 1)
	end
	return name
end

local function session_suffix(name)
	local slash = string.find(name, "/", 1, true)
	if slash then
		return string.sub(name, slash + 1)
	end
	return ""
end

local function count_groups(sessions, group_of)
	local groups = {}
	for _, session in ipairs(sessions) do
		local group = group_of(session)
		if not groups[group] then
			groups[group] = { count = 0, emitted = 0, first = session.name }
		end
		groups[group].count += 1
	end
	return groups
end

local function session_context(session, groups, last_group, options)
	local group_of = options.group_of or function(value)
		return session_group(value.name)
	end
	local label_of = options.label_of
		or function(value, group)
			local suffix = session_suffix(value.name)
			return if suffix == "" then group else suffix
		end
	local group = group_of(session)
	local summary = groups[group]
	local grouped = group ~= "" and summary.count > 1
	local last_in_group = grouped and summary.emitted + 1 == summary.count
	-- A lone session shows just its group (project) name; the "/leaf" suffix only appears
	-- once a project holds more than one session. `group` == `session.name` for plain
	-- directories with no "/", so those are unaffected.
	local label = if grouped then label_of(session, group) else group
	return {
		session = session,
		group = group,
		summary = summary,
		emit_group = grouped and group ~= last_group,
		label = label,
		-- Per-session anchor so a drag reorders within the group; the group header shares the
		-- first session's anchor, so dragging it past another group still moves the whole block.
		anchor = session.name,
		indent = if grouped then 2 else 0,
		tree = if not grouped then "none" elseif last_in_group then "last" else "middle",
		detail_tree = if not grouped then "none" elseif last_in_group then "blank" else "pipe",
		detail_indent = if grouped then 4 else 2,
	}
end

function ui.session_items(options)
	local opts = opt(options)
	local sessions = opts.sessions or {}
	local group_of = opts.group_of or function(value)
		return session_group(value.name)
	end
	local groups = count_groups(sessions, group_of)
	local items = {}
	local ordinal = 0
	local last_group = nil
	for _, session in ipairs(sessions) do
		local ctx = session_context(session, groups, last_group, opts)
		local facts = if opts.facts then opts.facts(session, ctx) else nil
		ordinal += 1
		if ctx.emit_group then
			ui.push(
				items,
				ui.group_row({
					text = ctx.group,
					anchor = ctx.anchor,
					fg = session.color,
					dim_fg = session.dim_color,
				})
			)
		end
		ui.push(
			items,
			ui.session_row({
				text = ctx.label,
				number = ordinal,
				indent = ctx.indent,
				tree = ctx.tree,
				session = session,
				anchor = ctx.anchor,
				fg = session.color,
				dim_fg = session.dim_color,
				current = session.selected,
				active = session.selected,
				primitives = if opts.right then opts.right(ctx, facts) else {},
			})
		)
		if opts.details then
			for _, spec in ipairs(opts.details(ctx, facts) or {}) do
				ui.push(items, ui.detail_row(ctx, spec))
			end
		end
		if opts.progress then
			local progress = opts.progress(ctx, facts)
			if type(progress) == "number" then
				progress = { value = progress }
			end
			if progress then
				local rows = if progress.value ~= nil then { progress } else progress
				for _, row in ipairs(rows) do
					ui.push(items, ui.bar_row(ctx, row))
				end
			end
		end
		ctx.summary.emitted += 1
		last_group = ctx.group
	end
	return items
end

function ui.footer_meter(spec)
	local fill = ui.clamp(spec.fill or 0, 0, 1)
	local primitives = {
		ui.icon(
			spec.icon,
			{ x_px = 6, y = 0, y_px = 7, size = spec.icon_size or 13, color = spec.icon_color or spec.color }
		),
		ui.text(spec.label or "", {
			x_px = 18,
			y = 0,
			y_px = 7,
			color = spec.text_color,
			align = "left_center",
		}),
	}
	if spec.track then
		primitives[#primitives + 1] =
			ui.rect({ x = 0, x_px = 0, y = 0, y_px = 17, w = 1, h = 0, h_px = 4, radius = 2, fill = spec.track })
	end
	primitives[#primitives + 1] =
		ui.rect({ x = 0, x_px = 0, y = 0, y_px = 17, w = fill, h = 0, h_px = 4, radius = 2, fill = spec.color })
	if spec.pace and spec.pace ~= "" then
		primitives[#primitives + 1] = ui.text(spec.pace, {
			x = 1,
			x_px = -102,
			y = 0,
			y_px = 7,
			color = spec.pace_color or spec.muted_color,
			align = "right_center",
			min_width = 260,
		})
	end
	if spec.reset and spec.reset ~= "" then
		primitives[#primitives + 1] = ui.icon("rotate-ccw", {
			x = 1,
			x_px = -52,
			y = 0,
			y_px = 7,
			size = 10,
			color = spec.muted_color,
			min_width = 150,
		})
		primitives[#primitives + 1] = ui.text(spec.reset, {
			x = 1,
			x_px = 0,
			y = 0,
			y_px = 7,
			color = spec.muted_color,
			align = "right_center",
			min_width = 150,
		})
	end
	if spec.marker then
		primitives[#primitives + 1] = ui.rect({
			x = ui.clamp(spec.marker, 0, 1),
			x_px = -1,
			y = 0,
			y_px = 14,
			w = 0,
			w_px = 2,
			h = 0,
			h_px = 10,
			radius = 1,
			fill = spec.marker_color,
		})
	end
	return {
		kind = "footer",
		text = spec.text or spec.label or "",
		fg = spec.color,
		primitives = primitives,
	}
end

ui.append = ui.push

return ui
