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
      if (this.#queue.length > 0 || this.#dropped > 0) {
        void this.#drain();
      }
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
  return new Promise((resolve) => {
    let finished = false;
    const child = spawnBootty(
      "bootty",
      ["--json", "command", "agents.pi.ingest", JSON.stringify(event)],
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
