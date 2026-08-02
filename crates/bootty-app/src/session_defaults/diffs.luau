--!strict
-- Adds git diff counts to each session summary row.
local ui = bootty.ui
local facts = bootty.sidebar.session_facts()

return {
	interval = 1 / 120,
	render = function()
		local sessions = bootty.sessions()
		facts.refresh(sessions)
		return ui.session_components({
			sessions = sessions,
			facts = facts.get,
			render = function(_, enriched)
				if not enriched.diff_added or not enriched.diff_removed then
					return nil
				end
				return {
					summary = {
						ui.text(
							"+" .. tostring(enriched.diff_added),
							{ x = 1, x_px = -62, color = bootty.theme.success, align = "right_center", size = 12 }
						),
						ui.text(
							"-" .. tostring(enriched.diff_removed),
							{ x = 1, x_px = -12, color = bootty.theme.destructive, align = "right_center", size = 12 }
						),
					},
				}
			end,
		})
	end,
}
