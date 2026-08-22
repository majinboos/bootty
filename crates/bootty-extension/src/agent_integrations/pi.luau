--!strict
-- Pi owns the RPC schema. Bootty supplies only bounded JSONL process transport.

local ui = bootty.ui
local PROCESS = "pi-rpc"
local ICON = "bot"
local COLOR = bootty.theme.primary
-- An agent runs in a session, so state is kept per pane: every reported event names the pane it
-- came from, and the sidebar row for that session reads the pane's state. The RPC session bootty
-- starts itself belongs to no pane and keeps its own key.
local MANAGED_PANE = "managed"
local panes = {}
local running = false
local next_request = 0

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

local function apply_rpc_message(message)
	local state = state_for(MANAGED_PANE)
	if message.type == "response" then
		if message.success == false then
			state.status = "error"
			state.error = message.error or "Pi RPC request failed"
		elseif message.command == "get_state" and message.data then
			state.session_id = message.data.sessionId
			state.session_name = message.data.sessionName
			state.status = if message.data.isStreaming then "working" else "idle"
			state.error = nil
		end
	else
		apply_native_event(state, message)
	end
	publish(MANAGED_PANE, "rpc", message)
end

local function drain()
	if not running then
		return
	end
	local state = state_for(MANAGED_PANE)
	local ok, events = pcall(function()
		return bootty.process.poll(PROCESS, 64)
	end)
	if not ok then
		running = false
		state.status = "error"
		state.error = tostring(events)
		return
	end
	for _, event in ipairs(events) do
		if event.stream == "stdout" then
			local decoded, message = pcall(function()
				return bootty.json.decode(event.line)
			end)
			if decoded then
				apply_rpc_message(message)
			else
				state.status = "error"
				state.error = "Pi emitted invalid JSONL"
				publish(MANAGED_PANE, "protocol_error", { line = event.line })
			end
		elseif event.stream == "stderr" or event.stream == "error" then
			state.error = event.line
			publish(MANAGED_PANE, event.stream, { line = event.line })
		elseif event.stream == "exit" then
			running = false
			state.status = "stopped"
			publish(MANAGED_PANE, "exit", { code = event.code })
		elseif event.stream == "dropped" then
			publish(MANAGED_PANE, "dropped", { count = event.count })
		end
	end
end

local function send(command)
	if not running then
		error("Pi RPC session is not running")
	end
	next_request += 1
	command.id = "bootty-" .. tostring(next_request)
	bootty.process.write(PROCESS, bootty.json.encode(command))
	return command.id
end

bootty.commands.register({
	id = "agents.pi.start",
	title = "Start managed Pi",
	mutation = "write",
	arguments = {
		{ name = "cwd", type = "string" },
		{ name = "program", type = "string" },
	},
}, function(context)
	local cwd = context.arguments[1]
	local program = context.arguments[2] or "pi"
	bootty.process.start({ id = PROCESS, argv = { program, "--mode", "rpc" }, cwd = cwd })
	running = true
	local state = state_for(MANAGED_PANE)
	state.source = "managed"
	state.status = "starting"
	state.error = nil
	send({ type = "get_state" })
	return snapshot(MANAGED_PANE)
end)

bootty.commands.register({
	id = "agents.pi.prompt",
	title = "Prompt managed Pi",
	mutation = "write",
	arguments = { { name = "message", type = "string", required = true } },
}, function(context)
	drain()
	local id = send({ type = "prompt", message = context.arguments[1] })
	state_for(MANAGED_PANE).status = "working"
	return { request_id = id, state = snapshot(MANAGED_PANE) }
end)

for _, command_type in ipairs({ "steer", "follow_up" }) do
	local current = command_type
	bootty.commands.register({
		id = "agents.pi." .. current,
		title = if current == "steer" then "Steer managed Pi" else "Follow up with managed Pi",
		mutation = "write",
		arguments = { { name = "message", type = "string", required = true } },
	}, function(context)
		drain()
		return { request_id = send({ type = current, message = context.arguments[1] }) }
	end)
end

bootty.commands.register({
	id = "agents.pi.abort",
	title = "Abort managed Pi",
	mutation = "destructive",
}, function()
	drain()
	return { request_id = send({ type = "abort" }) }
end)

bootty.commands.register({
	id = "agents.pi.state",
	title = "Inspect Pi state",
	-- Without a pane this answers for the RPC session bootty manages itself.
	arguments = { { name = "pane", type = "string" } },
}, function(context)
	drain()
	local pane = context.arguments[1]
	return snapshot(if pane and pane ~= "" then pane else MANAGED_PANE)
end)

bootty.commands.register({
	id = "agents.pi.stop",
	title = "Stop managed Pi",
	mutation = "destructive",
}, function()
	bootty.process.stop(PROCESS)
	running = false
	state_for(MANAGED_PANE).status = "stopped"
	return snapshot(MANAGED_PANE)
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
	local pane = if reported and reported ~= "" then reported else MANAGED_PANE
	local state = state_for(pane)
	state.source = "existing"
	state.error = nil
	apply_native_event(state, event)
	bootty.storage.set("latest", event)
	publish(pane, "native", event)
	local result = snapshot(pane)
	-- A finished session has nothing left to show, and keeping it would grow one entry per pane
	-- bootty ever hosted.
	if pane ~= MANAGED_PANE and state.status == "stopped" then
		panes[pane] = nil
	end
	return result
end)

bootty.ui.register({ id = "agents.pi", placement = "session", order = 900, interval = 0.1 }, function()
	drain()
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
  options: { stdio: "ignore"; detached: false },
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
      ["--json", "command", "agents.pi.ingest", JSON.stringify(event), pane],
      { stdio: "ignore", detached: false },
    );
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
