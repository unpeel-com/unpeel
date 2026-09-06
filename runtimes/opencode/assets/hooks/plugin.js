// Unpeel OpenCode plugin
export const UnpeelNotifyPlugin = async ({ $, client }) => {
  if (globalThis.__unpeelOpencodeNotifyPluginV1) return {};
  globalThis.__unpeelOpencodeNotifyPluginV1 = true;

  if (!process?.env?.UNPEEL_SESSION_ID) return {};

  const notifyPath = "{{NOTIFY_PATH}}";
  let currentState = 'idle';
  let rootSessionID = null;
  let stopSent = false;
  let stopEvent = 'Stop';
  let eventQueue = Promise.resolve();
  const childSessionCache = new Map();

  const notify = async (hookEventName, sessionID = rootSessionID) => {
    const payload = JSON.stringify({
      hook_event_name: hookEventName,
      ...(sessionID ? { session_id: sessionID } : {}),
    });
    try {
      await $`bash ${notifyPath} ${payload}`;
    } catch {
      // Best effort.
    }
  };

  const isChildSession = async (sessionID) => {
    if (!sessionID || !client?.session?.list) return true;
    if (childSessionCache.has(sessionID)) {
      return childSessionCache.get(sessionID);
    }
    try {
      const sessions = await client.session.list();
      const session = sessions.data?.find((value) => value.id === sessionID);
      if (!session) return true;
      const isChild = !!session?.parentID;
      childSessionCache.set(sessionID, isChild);
      return isChild;
    } catch {
      return true;
    }
  };

  const handleBusy = async (sessionID) => {
    if (!rootSessionID) rootSessionID = sessionID;
    if (sessionID !== rootSessionID) return;
    if (currentState === 'idle') {
      currentState = 'busy';
      stopSent = false;
      stopEvent = 'Stop';
      await notify('Start', sessionID);
    }
  };

  const handleStop = async (sessionID) => {
    if (rootSessionID && sessionID !== rootSessionID) return;
    if (currentState === 'busy' && !stopSent) {
      currentState = 'idle';
      stopSent = true;
      await notify(stopEvent, sessionID);
      rootSessionID = null;
    }
  };

  const handleEvent = async ({ event }) => {
      const sessionID = event.properties?.sessionID ?? event.properties?.info?.sessionID;
      if (await isChildSession(sessionID)) return;

      if (event.type === 'session.status') {
        const status = event.properties?.status;
        if (status?.type === 'busy') {
          await handleBusy(sessionID);
        } else if (status?.type === 'idle') {
          await handleStop(sessionID);
        }
      }

      if (event.type === 'session.busy') {
        await handleBusy(sessionID);
      }
      if (event.type === 'session.error' && sessionID === rootSessionID) {
        // Errors can be followed by retries/compaction. Remember the outcome
        // and wait for the provider's idle event before settling the turn.
        stopEvent = event.properties?.error?.name === 'MessageAbortedError'
          ? 'StopCancelled' : 'StopFailure';
      }
      if (event.type === 'message.updated' && sessionID === rootSessionID) {
        const info = event.properties?.info;
        // A successful response after retry/compaction replaces its earlier
        // recoverable error. Tool messages and incomplete chunks do not.
        if (stopEvent === 'StopFailure' && info?.role === 'assistant' && info?.time?.completed && !info.error) {
          stopEvent = 'Stop';
        }
      }
      if (event.type === 'session.idle') {
        await handleStop(sessionID);
      }
  };

  return {
    event: (input) => {
      // Provider event callbacks can overlap while session lookup/notify waits.
      // Preserve error -> idle ordering and never let a child end the root turn.
      eventQueue = eventQueue.then(() => handleEvent(input), () => handleEvent(input));
      return eventQueue;
    },
    'permission.ask': async (_permission, output) => {
      if (output.status === 'ask') {
        await notify('PermissionRequest', rootSessionID);
      }
    },
  };
};
