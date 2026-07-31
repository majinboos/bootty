--!strict
-- Built-in sidebar `codexbar` module. Override with ~/.config/bootty/sidebar/codexbar.luau.
-- Backed by a persistent CodexBar localhost server managed by Bootty. Provider fetches
-- are independent; a missing provider should not suppress rows from another provider.
local ui = bootty.ui

local PROVIDERS = {
	{
		id = "codex",
		label = "codex",
		icon = "openai",
		color = bootty.theme.accent,
		warning = bootty.theme.warning,
		critical = bootty.theme.destructive,
	},
	{
		id = "claude",
		label = "claude",
		icon = "claude",
		color = bootty.theme.warning,
		warning = bootty.theme.warning,
		critical = bootty.theme.destructive,
	},
}

local GREEN_COLOR = bootty.theme.success
local TEXT_COLOR = bootty.theme.text
local MUTED_COLOR = bootty.theme.muted
local empty_renders = 0

local function parse_window(data, key, fallback_window_secs)
	local window = data[key]
	if not window or not window.usedPercent then
		return nil
	end
	local reset_epoch = ui.parse_iso_epoch(window.resetsAt)
	return {
		used_percent = ui.clamp(window.usedPercent, 0, 100),
		window_secs = if window.windowMinutes and window.windowMinutes > 0
			then window.windowMinutes * 60
			else fallback_window_secs,
		reset_secs = if reset_epoch then reset_epoch - bootty.time() else nil,
	}
end

local function provider_color(provider, key)
	return provider[key] or provider.color
end

local function quota_color(window, provider)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	if remaining_pct <= 5 then
		return provider_color(provider, "critical")
	elseif remaining_pct <= 15 then
		return provider_color(provider, "warning")
	end
	return provider.color
end

local function deficit_color(provider, deficit)
	if deficit >= 25 then
		return provider_color(provider, "critical")
	end
	return provider_color(provider, "warning")
end

local function marker_color(provider, remaining_percent, expected_remaining_percent)
	local deficit = expected_remaining_percent - remaining_percent
	if deficit <= 0 then
		return GREEN_COLOR
	end
	return deficit_color(provider, deficit)
end

local function expected_remaining_percent(window)
	if not window.reset_secs or window.window_secs <= 0 or window.reset_secs <= 0 then
		return nil
	end
	return math.max(0, math.min(100, window.reset_secs / window.window_secs * 100))
end

local function pace_label(provider, remaining_pct, expected_remaining_pct)
	if not expected_remaining_pct then
		return "", nil
	end
	local deficit = math.floor(expected_remaining_pct - remaining_pct + 0.5)
	if deficit > 0 then
		return string.format("%d%% def", deficit), deficit_color(provider, deficit)
	end
	local ahead = math.floor(remaining_pct - expected_remaining_pct + 0.5)
	if ahead > 0 then
		return string.format("+%d%%", ahead), MUTED_COLOR
	end
	return "", nil
end

local function usage_item(provider, label, window)
	local remaining_pct = math.max(0, math.min(100, 100 - window.used_percent))
	local expected_remaining_pct = expected_remaining_percent(window)
	local color = quota_color(window, provider)
	local pace, pace_color = pace_label(provider, remaining_pct, expected_remaining_pct)
	local reset = if window.reset_secs and window.reset_secs > 0 then ui.format_duration(window.reset_secs) else ""
	return ui.footer_meter({
		text = provider.label .. " " .. label,
		icon = provider.icon,
		label = string.format("%s %d%% left", label, math.floor(remaining_pct + 0.5)),
		text_color = TEXT_COLOR,
		muted_color = MUTED_COLOR,
		color = color,
		track = bootty.theme.border,
		fill = remaining_pct / 100,
		pace = pace,
		pace_color = pace_color,
		reset = reset,
		marker = if expected_remaining_pct then expected_remaining_pct / 100 else nil,
		marker_color = if expected_remaining_pct then marker_color(provider, remaining_pct, expected_remaining_pct) else nil,
	})
end

local function status_item(label)
	return {
		kind = "footer",
		text = "codexbar " .. label,
		fg = MUTED_COLOR,
	}
end

local function short_status(value)
	local first = string.match(value, "^[^\n]*") or value
	if #first > 42 then
		return string.sub(first, 1, 39) .. "..."
	end
	return first
end

local function error_message(data)
	local err = data.error
	if not err and data[1] then
		err = data[1].error
	end
	if err and err.message then
		return short_status(err.message)
	end
	return nil
end


local function provider_items(provider)
	local raw = bootty.codexbar_usage(provider.id)
	if raw == "" then
		return {}, nil
	end
	local ok, data = pcall(function()
		return bootty.json.decode(raw)
	end)
	if not ok or not data then
		return {}, short_status(raw)
	end
	local err = error_message(data)
	if err then
		return {}, err
	end
	data = data[1] or data
	data = data.usage or data
	local primary = parse_window(data, "primary", 5 * 3600)
	local secondary = parse_window(data, "secondary", 7 * 24 * 3600)
	local items = {}
	if primary then
		items[#items + 1] = usage_item(provider, "5h", primary)
	end
	if secondary then
		items[#items + 1] = usage_item(provider, "7d", secondary)
	end
	return items, nil
end

local function usage_items()
	local items = {}
	local status = nil
	for _, provider in ipairs(PROVIDERS) do
		local provider_items_result, provider_status = provider_items(provider)
		status = status or provider_status
		for _, item in ipairs(provider_items_result) do
			items[#items + 1] = item
		end
	end
	if #items > 0 then
		return items
	end
	if status then
		return { status_item(status) }
	end
	return {}
end

return {
	-- CodexBar's server owns provider refresh/caching; this cadence only refreshes Bootty rows.
	-- Each provider is one localhost request, so keep PROVIDERS short to bound render work.
	interval = 60,
	render = usage_items,
}
