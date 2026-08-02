--!strict
-- Adds each session's working directory.
local ui = bootty.ui

return function()
	return ui.session_components({
		sessions = bootty.sessions(),
		render = function(ctx, _)
			return {
				details = {
					{
						key = "cwd",
						icon = "folder",
						label = bootty.path.display(ctx.session.cwd or "unknown"),
						color = bootty.theme.subtext,
					},
				},
			}
		end,
	})
end
