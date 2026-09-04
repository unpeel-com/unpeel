// @i-know-the-amp-plugin-api-is-wip-and-very-experimental-right-now
const notifyPath = "{{NOTIFY_PATH}}";

async function notify(hookEventName, payload = {}) {
  const body = JSON.stringify({
    hook_event_name: hookEventName,
    tool_name: "amp",
    ...payload,
  });

  try {
    const proc = Bun.spawn(["bash", notifyPath, body], {
      stdin: "ignore",
      stdout: "ignore",
      stderr: "ignore",
    });
    await proc.exited;
  } catch {
    // Best effort.
  }
}

function threadIDFrom(event) {
  const candidates = [
    event?.threadID,
    event?.threadId,
    event?.thread?.id,
    event?.conversationID,
    event?.conversationId,
  ];
  return candidates.find((value) => typeof value === "string" && value.length > 0);
}

export default function registerUnpeelAmpPlugin(amp) {
  amp.on("agent.start", async (event) => {
    await notify("Start", {
      prompt_text: typeof event?.message === "string" ? event.message : undefined,
      session_id: threadIDFrom(event),
    });
  });

  amp.on("agent.end", async (event) => {
    await notify("Stop", {
      session_id: threadIDFrom(event),
    });
  });
}
