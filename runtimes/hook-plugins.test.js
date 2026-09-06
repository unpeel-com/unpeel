import { afterEach, expect, test } from "bun:test";
import { UnpeelNotifyPlugin } from "./opencode/assets/hooks/plugin.js";
import registerAmp from "./amp/assets/hooks/plugin.js";

const originalSession = process.env.UNPEEL_SESSION_ID;
const originalSpawn = Bun.spawn;
afterEach(() => {
  if (originalSession === undefined) delete process.env.UNPEEL_SESSION_ID;
  else process.env.UNPEEL_SESSION_ID = originalSession;
  Bun.spawn = originalSpawn;
  delete globalThis.__unpeelOpencodeNotifyPluginV1;
});

async function openCode() {
  process.env.UNPEEL_SESSION_ID = "isolated-plugin-test";
  const posts = [];
  const sessions = [{ id: "root" }, { id: "child", parentID: "root" }];
  const plugin = await UnpeelNotifyPlugin({
    $: async (_parts, _path, payload) => { posts.push(JSON.parse(payload)); },
    client: { session: { list: async () => {
      await new Promise(resolve => setTimeout(resolve, 2));
      return { data: sessions };
    } } },
  });
  const event = (type, properties = {}) => plugin.event({ event: {
    type, properties: { sessionID: "root", ...properties },
  } });
  return { event, posts, sessions };
}

test("OpenCode keeps child lifecycle and duplicate idle out of the parent", async () => {
  const { event, posts } = await openCode();
  await Promise.all([
    event("session.status", { status: { type: "busy" } }),
    event("session.status", { status: { type: "busy" }, sessionID: "child" }),
    event("session.error", { error: { name: "APIError" }, sessionID: "child" }),
    event("session.idle", { sessionID: "child" }),
    event("session.idle"),
    event("session.status", { status: { type: "idle" } }),
  ]);
  expect(posts.map(p => p.hook_event_name)).toEqual(["Start", "Stop"]);
  expect(posts.every(p => p.session_id === "root")).toBe(true);
});

for (const [name, expected] of [["MessageAbortedError", "StopCancelled"], ["APIError", "StopFailure"]]) {
  test(`OpenCode serializes ${name} before idle and accepts the next turn`, async () => {
    const { event, posts } = await openCode();
    await event("session.busy");
    await event("session.error", { error: { name } });
    if (expected === "StopCancelled") {
      await event("message.updated", { info: {
        sessionID: "root", role: "assistant", time: { completed: 123 },
      } });
    }
    expect(posts.map(p => p.hook_event_name)).toEqual(["Start"]);
    await Promise.all([event("session.idle"), event("session.busy"), event("session.idle")]);
    expect(posts.map(p => p.hook_event_name)).toEqual(["Start", expected, "Start", "Stop"]);
  });
}

test("OpenCode recovery succeeds after a recoverable error", async () => {
  const { event, posts } = await openCode();
  await event("session.busy");
  await event("session.error", { error: { name: "ContextOverflowError" } });
  await event("message.updated", { sessionID: undefined, info: {
    sessionID: "root", role: "assistant", time: { completed: 123 }, finish: "stop",
  } });
  await event("session.idle");
  expect(posts.map(p => p.hook_event_name)).toEqual(["Start", "Stop"]);
});

test("OpenCode retries unknown session identity instead of caching it as a root", async () => {
  const { event, posts, sessions } = await openCode();
  await event("session.busy", { sessionID: "unknown" });
  expect(posts).toEqual([]);
  sessions.push({ id: "unknown" });
  await event("session.busy", { sessionID: "unknown" });
  expect(posts[0]).toEqual({ hook_event_name: "Start", session_id: "unknown" });
});

for (const [status, expected] of [["done", "Stop"], ["cancelled", "StopCancelled"], ["error", "StopFailure"]]) {
  test(`Amp preserves the ${status} outcome and waits for delivery`, async () => {
    const posts = [];
    const callbacks = {};
    Bun.spawn = (args) => ({ exited: new Promise(resolve => setTimeout(() => {
      posts.push(JSON.parse(args[2]));
      resolve(0);
    }, 2)) });
    registerAmp({ on: (name, callback) => { callbacks[name] = callback; } });
    await callbacks["agent.start"]({ thread: { id: "root" }, message: "hello" });
    await callbacks["agent.end"]({ thread: { id: "root" }, status });
    expect(posts.map(p => p.hook_event_name)).toEqual(["Start", expected]);
    expect(posts.every(p => p.session_id === "root")).toBe(true);
  });
}
