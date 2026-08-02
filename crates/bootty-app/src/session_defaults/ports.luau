--!strict
-- Adds listening ports reported by each session.
local ui = bootty.ui

return function()
	return ui.session_components({
		sessions = bootty.sessions(),
		render = function(ctx, _)
			if #ctx.session.ports == 0 then
				return nil
			end
			return {
				details = {
					{
						key = "ports",
						icon = "plug",
						label = table.concat(ctx.session.ports, ", "),
						color = bootty.theme.subtext,
					},
				},
			}
		end,
	})
end
