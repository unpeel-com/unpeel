import { afterEach, expect, test } from "bun:test";
import { createServer } from "node:http";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { UnpeelNotifyPlugin } from "./opencode/assets/hooks/plugin.js";
import registerAmp from "./amp/assets/hooks/plugin.js";

const originalSession = process.env.UNPEEL_SESSION_ID;
const originalAppPort = process.env.UNPEEL_APP_PORT;
const originalSessionDir = process.env.UNPEEL_SESSION_DIR;
const originalGeneration = process.env.UNPEEL_RUNTIME_GENERATION;
const originalSpawn = Bun.spawn;
afterEach(() => {
  if (originalSession === undefined) delete process.env.UNPEEL_SESSION_ID;
  else process.env.UNPEEL_SESSION_ID = originalSession;
  if (originalAppPort === undefined) delete process.env.UNPEEL_APP_PORT;
  else process.env.UNPEEL_APP_PORT = originalAppPort;
  if (originalSessionDir === undefined) delete process.env.UNPEEL_SESSION_DIR;
  else process.env.UNPEEL_SESSION_DIR = originalSessionDir;
  if (originalGeneration === undefined) delete process.env.UNPEEL_RUNTIME_GENERATION;
  else process.env.UNPEEL_RUNTIME_GENERATION = originalGeneration;
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

/**
 * OMP's reporter is an extension module rather than a hook script: the
 * provider has no shell-hook configuration, so the extension event bus is the
 * lifecycle mechanism. These tests load the shipped asset, drive OMP's own
 * events, and assert the wire payload, the durable seed, and the inert
 * outside-session behavior.
 */
async function loadOmpReporter(env) {
  const directory = mkdtempSync(join(tmpdir(), "unpeel-omp-hook-"));
  Object.assign(process.env, {
    UNPEEL_SESSION_ID: "omp-extension-test",
    UNPEEL_SESSION_DIR: directory,
    UNPEEL_RUNTIME_GENERATION: "7",
    ...env,
  });
  const reporters = await import(`./omp/assets/extensions/unpeel-lifecycle.ts?${crypto.randomUUID()}`);
  const handlers = {};
  const registered = [];
  const pi = {
    on: (name, handler) => {
      registered.push(name);
      (handlers[name] ??= []).push(handler);
    },
  };
  reporters.default(pi);
  return {
    handlers,
    registered,
    directory,
    cleanup: () => rmSync(directory, { recursive: true, force: true }),
  };
}

function capturePort() {
  const posts = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", chunk => { body += chunk; });
    request.on("end", () => {
      posts.push({ path: request.url, payload: JSON.parse(body) });
      response.writeHead(200);
      response.end("ok");
    });
  });
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => resolve({
      posts,
      port: server.address().port,
      close: () => server.close(),
    }));
  });
}

const ompContext = {
  sessionManager: {
    getSessionId: () => "01a0b32e-ef83-72d6-9210-d5cd55a1523c",
    getSessionFile: () => "/home/me/.omp/agent/sessions/-tmp/2026-09-18T06-23-12-771Z_01a0b32e.jsonl",
  },
};

test("OMP reports identity, turn edges and attention to the session's hook port", async () => {
  const capture = await capturePort();
  const omp = await loadOmpReporter({ UNPEEL_APP_PORT: String(capture.port) });
  try {
    expect(omp.registered).toEqual([
      "session_start",
      "before_agent_start",
      "agent_end",
      "tool_approval_requested",
      "session_shutdown",
    ]);

    await omp.handlers.session_start[0]({ type: "session_start" }, ompContext);
    expect(capture.posts[0].path).toBe("/hook/omp-extension-test");
    expect(capture.posts[0].payload).toEqual({
      hook_event_name: "HookSeen",
      session_id: "01a0b32e-ef83-72d6-9210-d5cd55a1523c",
      transcript_path: ompContext.sessionManager.getSessionFile(),
      unpeel_runtime_generation: 7,
    });

    await omp.handlers.before_agent_start[0]({ type: "before_agent_start", prompt: "hi" }, ompContext);
    await omp.handlers.agent_end[0]({ type: "agent_end", messages: [] }, ompContext);
    expect(capture.posts.map(post => post.payload.hook_event_name)).toEqual([
      "HookSeen",
      "UserPromptSubmit",
      "Stop",
    ]);
  } finally {
    capture.close();
    omp.cleanup();
  }
});

test("OMP keeps a turn open while a continuation is scheduled and records attention", async () => {
  const capture = await capturePort();
  const omp = await loadOmpReporter({ UNPEEL_APP_PORT: String(capture.port) });
  try {
    await omp.handlers.agent_end[0]({ type: "agent_end", messages: [], willContinue: true }, ompContext);
    expect(capture.posts).toEqual([]);

    await omp.handlers.tool_approval_requested[0](
      { type: "tool_approval_requested", toolName: "bash" },
      ompContext,
    );
    expect(capture.posts.map(post => post.payload)).toEqual([
      {
        hook_event_name: "PermissionRequest",
        tool_name: "bash",
        unpeel_runtime_generation: 7,
      },
    ]);

    const seeded = JSON.parse(readFileSync(join(omp.directory, "last-hook-event.json"), "utf8"));
    expect(seeded.hook_event_name).toBe("PermissionRequest");
    expect(seeded.tool_name).toBe("bash");

    await omp.handlers.session_shutdown[0]({ type: "session_shutdown" }, ompContext);
    expect(capture.posts.at(-1).payload.hook_event_name).toBe("Stop");
  } finally {
    capture.close();
    omp.cleanup();
  }
});

test("OMP delivery also reaches every app port in the registry", async () => {
  const direct = await capturePort();
  const registered = await capturePort();
  const registryFile = join(mkdtempSync(join(tmpdir(), "unpeel-omp-ports-")), "app-ports");
  writeFileSync(registryFile, `${registered.port}\n${direct.port}\nnot-a-port\n`, "utf8");
  const omp = await loadOmpReporter({
    UNPEEL_APP_PORT: String(direct.port),
    UNPEEL_APP_PORT_REGISTRY_FILE: registryFile,
  });
  try {
    await omp.handlers.before_agent_start[0]({ type: "before_agent_start", prompt: "hi" }, ompContext);
    expect(direct.posts.map(post => post.payload.hook_event_name)).toEqual(["UserPromptSubmit"]);
    expect(registered.posts.map(post => post.payload.hook_event_name)).toEqual(["UserPromptSubmit"]);
  } finally {
    direct.close();
    registered.close();
    rmSync(registryFile, { force: true });
    omp.cleanup();
  }
});

test("OMP reporter is inert outside a hosted Session", async () => {
  delete process.env.UNPEEL_SESSION_ID;
  const reporters = await import(`./omp/assets/extensions/unpeel-lifecycle.ts?${crypto.randomUUID()}`);
  const registered = [];
  reporters.default({ on: name => registered.push(name) });
  expect(registered).toEqual([]);
});
