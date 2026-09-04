// Unpeel OpenCode plugin
export const UnpeelNotifyPlugin = async ({ $, client }) => {
  if (globalThis.__unpeelOpencodeNotifyPluginV1) return {};
  globalThis.__unpeelOpencodeNotifyPluginV1 = true;

  if (!process?.env?.UNPEEL_SESSION_ID) return {};

  const notifyPath = "{{NOTIFY_PATH}}";
  let currentState = 'idle';
  let rootSessionID = null;
  let stopSent = false;
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
      await notify('Start', sessionID);
    }
  };

  const handleStop = async (sessionID) => {
    if (rootSessionID && sessionID !== rootSessionID) return;
    if (currentState === 'busy' && !stopSent) {
      currentState = 'idle';
      stopSent = true;
      await notify('Stop', sessionID);
      rootSessionID = null;
    }
  };

  return {
    event: async ({ event }) => {
      const sessionID = event.properties?.sessionID;
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
      if (event.type === 'session.idle' || event.type === 'session.error') {
        await handleStop(sessionID);
      }
    },
    'permission.ask': async (_permission, output) => {
      if (output.status === 'ask') {
        await notify('PermissionRequest', rootSessionID);
      }
    },
  };
};
