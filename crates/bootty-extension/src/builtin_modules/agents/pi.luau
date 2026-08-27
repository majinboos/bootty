--!strict
-- Pi owns its interactive session. Bootty targets the visible terminal that owns it.

local ui = bootty.ui
local ICON = "bot"
local COLOR = bootty.theme.primary
local panes = {}

bootty.events.register("agents.pi.event")

local function new_state()
	return {
		provider = "pi",
		source = "none",
		status = "stopped",
		session_id = nil,
		session_name = nil,
		last_event = nil,
		error = nil,
	}
end

local function state_for(pane)
	local state = panes[pane]
	if not state then
		state = new_state()
		panes[pane] = state
	end
	return state
end

local function snapshot(pane)
	return table.clone(panes[pane] or new_state())
end

local function publish(pane, kind, payload)
	pcall(function()
		bootty.events.publish("agents.pi.event", {
			kind = kind,
			pane = pane,
			state = snapshot(pane),
			payload = payload,
		})
	end)
end

local function apply_native_event(state, event)
	state.last_event = event.type or event.hook_event_name or "unknown"
	if event.sessionId then
		state.session_id = event.sessionId
	elseif event.session_id then
		state.session_id = event.session_id
	end
	if event.sessionName then
		state.session_name = event.sessionName
	end
	if event.type == "agent_start" or event.type == "turn_start" then
		state.status = "working"
	elseif event.type == "agent_settled" then
		state.status = "idle"
	elseif event.type == "tool_execution_start" then
		state.status = "tool:" .. (event.toolName or "unknown")
	elseif event.type == "extension_error" then
		state.status = "error"
		state.error = event.error or "Pi extension error"
	elseif event.type == "session_shutdown" then
		state.status = "stopped"
	end
end

local function invoke(invocation)
	local outcome = bootty.commands.invoke(invocation)
	if outcome.status ~= "success" then
		error(outcome.message or ("command failed: " .. tostring(outcome.status)))
	end
	return outcome.value
end

local function write(context, text)
	local invocation = {
		command = "terminal.write",
		arguments = { text },
	}
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	return { sent = true }
end

local function submit(context, text)
	local invocation = { command = "terminal.paste", arguments = { text } }
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	invocation = { command = "terminal.submit" }
	if context.target_supplied then
		invocation.target = context.target
	end
	invoke(invocation)
	return { sent = true }
end

bootty.commands.register({
	id = "agents.pi.start",
	title = "Start Pi",
	mutation = "write",
	target = "terminal",
	arguments = {
		{ name = "cwd", type = "string" },
		{ name = "program", type = "string" },
	},
}, function(context)
	if not context.target_supplied then
		local created = invoke({ command = "new_tab" }).created
		if not created then
			error("new tab did not return its terminal target")
		end
		context.target = created
		context.target_supplied = true
	end
	local cwd = context.arguments[1]
	local program = context.arguments[2] or "pi"
	local command = bootty.shell.quote(program)
	if cwd and cwd ~= "" then
		command = "cd " .. bootty.shell.quote(cwd) .. " && exec " .. command
	else
		command = "exec " .. command
	end
	return submit(context, command)
end)

bootty.commands.register({
	id = "agents.pi.prompt",
	title = "Prompt Pi",
	mutation = "write",
	target = "terminal",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	return submit(context, context.arguments[1])
end)

for _, command_type in ipairs({ "steer", "follow_up" }) do
	local current = command_type
	bootty.commands.register({
		id = "agents.pi." .. current,
		title = if current == "steer" then "Steer Pi" else "Follow up with Pi",
		mutation = "write",
		target = "terminal",
		arguments = { { name = "message", type = "string", required = true } },
	}, function(context)
		return submit(context, context.arguments[1])
	end)
end

bootty.commands.register({
	id = "agents.pi.abort",
	title = "Abort Pi",
	mutation = "destructive",
	target = "terminal",
}, function(context)
	return write(context, "\003")
end)

bootty.commands.register({
	id = "agents.pi.state",
	title = "Inspect Pi state",
	-- Without a pane this answers for the most recently reported visible session.
	arguments = { { name = "pane", type = "string" } },
}, function(context)
	local pane = context.arguments[1]
	return snapshot(if pane and pane ~= "" then pane else next(panes))
end)

bootty.commands.register({
	id = "agents.pi.stop",
	title = "Stop Pi pane",
	mutation = "destructive",
	target = "pane",
}, function(context)
	invoke({
		command = "kill_pane",
		target = context.target,
	})
	return { stopped = true }
end)

bootty.commands.register({
	id = "agents.pi.ingest",
	title = "Ingest a Pi native event",
	mutation = "write",
	arguments = {
		{ name = "event", type = "string", required = true },
		-- The pane Pi runs in, so the event lands on the session showing it.
		{ name = "pane", type = "string" },
	},
}, function(context)
	local event = bootty.json.decode(context.arguments[1])
	local reported = context.arguments[2]
	local pane = if reported and reported ~= "" then reported else "unknown"
	local state = state_for(pane)
	state.source = "existing"
	state.error = nil
	apply_native_event(state, event)
	bootty.storage.set("latest", event)
	publish(pane, "native", event)
	local result = snapshot(pane)
	-- A finished session has nothing left to show, and keeping it would grow one entry per pane
	-- bootty ever hosted.
	if state.status == "stopped" then
		panes[pane] = nil
	end
	return result
end)

bootty.ui.register({ id = "agents.pi", placement = "session", order = 900, interval = 0.1 }, function()
	return ui.session_components({
		sessions = bootty.sessions(),
		render = function(ctx)
			local state = if ctx.session.pane_id then panes[ctx.session.pane_id] else nil
			-- Nothing has reported Pi in this session's pane, so the row stays quiet.
			if not state or state.source == "none" then
				return nil
			end
			return {
				details = {
					{
						key = "agent",
						icon = ICON,
						label = "pi",
						color = COLOR,
						right = state.status,
						right_color = if state.status == "idle" then bootty.theme.muted else COLOR,
					},
				},
			}
		end,
	})
end)

-- The adapter Bootty installs for an existing Pi session, declared here so the extension source
-- stays next to the module that reads its events. Pi loads extensions from its own directory, so
-- the installed file has to be linked into `~/.pi/agent/extensions/`.
local ADAPTER = [==[
import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const MAX_PENDING_EVENTS = 64;

type PiEvent = unknown;
type PublishOne = (event: PiEvent) => Promise<void>;

function eventField(event: PiEvent, field: string): unknown {
  if (typeof event !== "object" || event === null) {
    return undefined;
  }
  return Reflect.get(event, field);
}

function replaceableUpdate(event: PiEvent): boolean {
  return eventField(event, "type") === "tool_execution_update";
}

function updateKey(event: PiEvent): unknown {
  return eventField(event, "toolCallId") ?? eventField(event, "tool_call_id");
}

export class BoundedEventPublisher {
  readonly #publishOne: PublishOne;
  readonly #queue: unknown[] = [];
  #active = false;
  #dropped = 0;

  constructor(publishOne: PublishOne) {
    this.#publishOne = publishOne;
  }

  get pendingCount(): number {
    return this.#queue.length + Number(this.#active);
  }

  enqueue(event: PiEvent): void {
    if (replaceableUpdate(event)) {
      const key = updateKey(event);
      const existing = this.#queue.findLastIndex(
        (candidate) => replaceableUpdate(candidate) && updateKey(candidate) === key,
      );
      if (existing >= 0) {
        this.#queue[existing] = event;
        return;
      }
    }

    if (this.#queue.length === MAX_PENDING_EVENTS) {
      const replaceable = this.#queue.findIndex(replaceableUpdate);
      if (replaceable >= 0) {
        this.#queue.splice(replaceable, 1);
      } else if (replaceableUpdate(event)) {
        this.#dropped += 1;
        return;
      } else {
        this.#queue.shift();
      }
      this.#dropped += 1;
    }
    this.#queue.push(event);
    void this.#drain();
  }

  async #drain(): Promise<void> {
    if (this.#active) {
      return;
    }
    this.#active = true;
    try {
      while (this.#queue.length > 0 || this.#dropped > 0) {
        if (this.#dropped > 0) {
          const dropped = this.#dropped;
          this.#dropped = 0;
          await this.#publish({
            type: "extension_error",
            error: `Bootty Pi bridge dropped ${dropped} queued event(s)`,
            dropped,
          });
          continue;
        }
        const event = this.#queue.shift();
        if (event) {
          await this.#publish(event);
        }
      }
    } finally {
      this.#active = false;
    }
  }

  async #publish(event: PiEvent): Promise<void> {
    try {
      await this.#publishOne(event);
    } catch (error) {
      console.error(`Bootty Pi integration failed: ${String(error)}`);
    }
  }
}

type SpawnBootty = (
  command: string,
    arguments_: string[],
  options: { stdio: ["pipe", "ignore", "ignore"]; detached: false },
) => ChildProcess;

export function publishToBootty(
  event: PiEvent,
  spawnBootty: SpawnBootty = spawn,
): Promise<void> {
  // The pane Pi runs in, so Bootty can show the event on the session hosting it. tmux names its
  // own panes; Bootty names the panes it spawns itself.
  const pane = process.env.TMUX_PANE ?? process.env.BOOTTY_PANE ?? "";
  return new Promise((resolve) => {
    let finished = false;
    const child = spawnBootty(
      "bootty",
      ["--json", "command", "agents.pi.ingest", "--stdin-json"],
      { stdio: ["pipe", "ignore", "ignore"], detached: false },
    );
    // The child is killed on a timeout, so a pending write can fail with EPIPE.
    // An unhandled stream error would take the Pi host process down with it.
    child.stdin?.once("error", () => {});
    // Keep event data out of argv: a Pi tool result can be much larger than macOS's execve limit.
    // Command arguments are strings, so the event travels as encoded JSON text.
    child.stdin?.end(JSON.stringify([JSON.stringify(event), pane]));
    const finish = (): void => {
      if (finished) {
        return;
      }
      finished = true;
      clearTimeout(terminate);
      clearTimeout(kill);
      clearTimeout(abandon);
      resolve();
    };
    const stop = (signal?: NodeJS.Signals): void => {
      try {
        child.kill(signal);
      } catch (error) {
        console.error(`Bootty Pi integration cleanup failed: ${String(error)}`);
      }
    };
    const terminate = setTimeout(stop, 1_750);
    const kill = setTimeout(() => stop("SIGKILL"), 2_000);
    // A broken child-process implementation must not stall the event bridge forever.
    const abandon = setTimeout(finish, 2_250);
    child.once("error", (error) => {
      console.error(`Bootty Pi integration failed: ${error.message}`);
      finish();
    });
    child.once("close", finish);
  });
}

export default function boottyPiIntegration(pi: ExtensionAPI): void {
  const publisher = new BoundedEventPublisher(publishToBootty);
  const publish = (event: PiEvent): void => publisher.enqueue(event);

  pi.on("session_start", publish);
  pi.on("session_info_changed", publish);
  pi.on("session_shutdown", publish);
  pi.on("agent_start", publish);
  pi.on("agent_end", publish);
  pi.on("agent_settled", publish);
  pi.on("turn_start", publish);
  pi.on("turn_end", publish);
  pi.on("tool_execution_start", publish);
  pi.on("tool_execution_update", publish);
  pi.on("tool_execution_end", publish);
}
]==]

bootty.integration.register({
	id = "extension",
	title = "Pi extension",
	summary = "Reports Pi activity to Bootty.",
	files = {
		{ path = "pi/bootty.ts", contents = ADAPTER },
	},
	-- Pi loads extensions from its own directory, so the adapter has to be in it. Nothing in Pi
	-- points at a file elsewhere the way a hooks entry does.
	place = {
		{ path = "~/.pi/agent/extensions/bootty.ts", file = "pi/bootty.ts" },
	},
})
