--!strict
-- Adds the active process to a session summary when no diff counts occupy that space.
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
				if not enriched.display_process or enriched.display_process == "" then
					return nil
				end
				if enriched.diff_added and enriched.diff_removed then
					return {
						details = {
							{
								key = "process",
								icon = "terminal",
								label = enriched.display_process,
								color = bootty.theme.subtext,
							},
						},
					}
				end
				return {
					summary = {
						ui.text(enriched.display_process, {
							x = 1,
							x_px = -12,
							color = bootty.theme.subtext,
							align = "right_center",
							size = 12,
						}),
					},
				}
			end,
		})
	end,
}
