import assert from "node:assert/strict";
import test from "node:test";

import { BoundedEventPublisher, MAX_PENDING_EVENTS } from "./bootty.ts";

test("the Pi bridge bounds updates and delivers the final lifecycle event", async () => {
  const delivered: unknown[] = [];
  const releases: (() => void)[] = [];
  const publisher = new BoundedEventPublisher(
    (event) =>
      new Promise<void>((resolve) => {
        delivered.push(event);
        releases.push(resolve);
      }),
  );

  publisher.enqueue({ type: "session_start", sessionId: "opaque" });
  for (let index = 0; index < 10_000; index += 1) {
    publisher.enqueue({
      type: "tool_execution_update",
      toolCallId: "tool-1",
      index,
    });
  }
  publisher.enqueue({ type: "agent_settled", sessionId: "opaque" });

  assert.ok(publisher.pendingCount <= MAX_PENDING_EVENTS + 1);
  while (
    !delivered.some(
      (event) =>
        typeof event === "object" && event !== null && Reflect.get(event, "type") === "agent_settled",
    )
  ) {
    const release = releases.shift();
    assert.ok(release, "one publication must be active");
    release();
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  while (releases.length > 0) {
    releases.shift()?.();
    await new Promise<void>((resolve) => setImmediate(resolve));
  }

  const updates = delivered.filter(
    (event) =>
      typeof event === "object" &&
      event !== null &&
      Reflect.get(event, "type") === "tool_execution_update",
  );
  assert.equal(updates.length, 1);
  assert.equal(Reflect.get(updates[0] as object, "index"), 9_999);
  assert.equal(Reflect.get(delivered.at(-1) as object, "type"), "agent_settled");
});
