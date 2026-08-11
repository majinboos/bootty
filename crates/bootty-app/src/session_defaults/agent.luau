--!strict
-- Render only adapter-authoritative agent records published by the bundled extension.
-- Process names, terminal text, descendants, and transcript output are never agent state.
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
				if not enriched or not enriched.authoritative or not enriched.agent_name then
					return nil
				end
				local activity, activity_color = facts.activity(enriched)
				local style = enriched.agent_style or {}
				return {
					details = {
						{
							key = "agent",
							icon = style.icon,
							label = enriched.agent_name,
							color = style.color,
							right = activity,
							right_color = activity_color,
						},
					},
				}
			end,
		})
	end,
}
