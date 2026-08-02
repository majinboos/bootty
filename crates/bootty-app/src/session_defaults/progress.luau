--!strict
-- Adds progress bars reported by processes in each session.
local ui = bootty.ui

return {
	interval = 1 / 120,
	render = function()
		return ui.session_components({
			sessions = bootty.sessions(),
			render = function(ctx, _)
				local rows = {}
				for index, progress in ipairs(ctx.session.progresses) do
					rows[#rows + 1] = {
						key = "progress:" .. tostring(index),
						value = progress.value,
						indeterminate = progress.indeterminate,
						track = bootty.theme.border,
						fill = ctx.session.color or bootty.theme.accent,
						label = progress.process,
						label_width = 72,
						label_color = bootty.theme.subtext,
					}
				end
				return { progress = rows }
			end,
		})
	end,
}
