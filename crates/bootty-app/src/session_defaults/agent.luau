--!strict
-- Adds detected coding-agent activity below each session.
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
				local activity, _ = facts.activity(enriched)
				return {
					details = {
						{
							key = "agent",
							icon = if enriched.agent_style then enriched.agent_style.icon else nil,
							label = enriched.agent_name,
							color = if enriched.agent_style then enriched.agent_style.color else nil,
							right = activity,
							right_color = bootty.theme.subtext,
						},
					},
				}
			end,
		})
	end,
}
