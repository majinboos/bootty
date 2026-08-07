--!strict
-- Adds git diff counts to each session summary row.
local ui = bootty.ui
local facts = bootty.sidebar.session_facts()

return {
	interval = ui.animation_interval,
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
					summary = ui.diff_summary(enriched.diff_added, enriched.diff_removed, {
						success = bootty.theme.success,
						destructive = bootty.theme.destructive,
					}),
				}
			end,
		})
	end,
}
