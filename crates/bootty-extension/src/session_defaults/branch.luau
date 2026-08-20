--!strict
-- Adds each session's git branch.
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
				return {
					details = {
						{
							key = "branch",
							icon = "git-branch",
							label = enriched.branch or "unknown",
							right = if enriched.branch_status ~= "current" then enriched.branch_status else nil,
							color = bootty.theme.subtext,
						},
					},
				}
			end,
		})
	end,
}
