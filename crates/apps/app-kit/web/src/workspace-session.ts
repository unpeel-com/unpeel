import {
  type UiAck,
  type UiAction,
  type UiAttached,
  type UiDelta,
  type UiErrorMessage,
  type UiMessage,
  type UiPresence,
  type UiRendererMetadata,
  type UiRendererState,
  type UiSnapshot,
  UI_COMPONENT_CAPABILITIES,
  UI_DELTA_CAPABILITY,
  applyUiDelta,
  decodeUiMessage,
  isBrowserSafeUiNode,
  negotiateUiProtocolVersion,
  newEventId,
  uiNodeCapabilities,
} from "./protocol";

export const WORKSPACE_UI_PROTOCOL_NAME = "unpeel.workspace.ui" as const;
export const WORKSPACE_UI_PROTOCOL_VERSION = 1 as const;

/**
 * Browser-to-workspace attach/resume request.
 *
 * Authentication comes from the workspace WebSocket session. There is
 * deliberately no auth token, participant identity, or grant list here.
 */
export interface WorkspaceUiResume {
  type: "resume";
  protocol: typeof WORKSPACE_UI_PROTOCOL_NAME;
  protocolVersion: typeof WORKSPACE_UI_PROTOCOL_VERSION;
  appSessionId: string;
  clientId: string;
  renderer: UiRendererMetadata;
  viewId: string;
  expectedAppInstanceId?: string;
  lastSeenRevision?: number;
  state: UiRendererState;
}

/** Renderer action before the existing Host applies authenticated identity. */
export interface WorkspaceUiAction extends UiAction {
  type: "action";
  protocol: typeof WORKSPACE_UI_PROTOCOL_NAME;
  protocolVersion: typeof WORKSPACE_UI_PROTOCOL_VERSION;
  appSessionId: string;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  eventId: string;
  baseRevision: number;
}

export interface WorkspaceUiLifecycle {
  type: "lifecycle";
  protocol: typeof WORKSPACE_UI_PROTOCOL_NAME;
  protocolVersion: typeof WORKSPACE_UI_PROTOCOL_VERSION;
  appSessionId: string;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  state: UiRendererState;
}

export interface WorkspaceUiRequestSnapshot {
  type: "requestSnapshot";
  protocol: typeof WORKSPACE_UI_PROTOCOL_NAME;
  protocolVersion: typeof WORKSPACE_UI_PROTOCOL_VERSION;
  appSessionId: string;
  appInstanceId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
}

export type WorkspaceUiClientMessage =
  | WorkspaceUiResume
  | WorkspaceUiAction
  | WorkspaceUiLifecycle
  | WorkspaceUiRequestSnapshot;

export type WorkspaceUiServerMessage =
  | UiAttached
  | UiSnapshot
  | UiDelta
  | UiAck
  | UiPresence
  | UiErrorMessage;

export type WorkspaceUiConnectionState =
  | { status: "stopped" }
  | { status: "connecting" }
  | { status: "attached"; appInstanceId: string; resumed: boolean }
  | { status: "waitingToReconnect"; delayMs: number };

/** Minimal socket shape, kept injectable so the session can be unit tested. */
export interface WorkspaceWebSocket {
  readonly readyState: number;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
  send(data: string): void;
  close(code?: number, reason?: string): void;
}

export interface WorkspaceUiSessionOptions {
  url: string | URL;
  appSessionId: string;
  clientId: string;
  rendererId: string;
  viewId: string;
  capabilities?: string[];
  supportedComponentCapabilities?: string[];
  initialState?: UiRendererState;
  onAttached?: (attached: UiAttached) => void;
  onSnapshot: (snapshot: UiSnapshot) => void;
  onDelta?: (delta: UiDelta) => void;
  onAck?: (ack: UiAck) => void;
  onPresence?: (presence: UiPresence) => void;
  onError?: (error: Error | UiErrorMessage) => void;
  onTerminalFallback?: (componentKind: string) => void;
  onConnectionState?: (state: WorkspaceUiConnectionState) => void;
  webSocketFactory?: (url: string) => WorkspaceWebSocket;
}

interface PendingWorkspaceAction {
  action: UiAction;
  event?: WorkspaceUiAction;
}

/**
 * Reconnecting browser transport for one terminal-backed App view.
 *
 * The existing Unpeel Host authenticates this extension of `/mobile`, derives
 * identity and grants from its ControllerPrincipal, and translates to the
 * local `unpeel.ui/1` socket. It is not a standalone workspace server.
 */
export class WorkspaceUiSession {
  private readonly options: WorkspaceUiSessionOptions;
  private readonly socketFactory: (url: string) => WorkspaceWebSocket;
  private socket: WorkspaceWebSocket | undefined;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private reconnectAttempt = 0;
  private running = false;
  private attached = false;
  private negotiatedProtocolVersion: number | undefined;
  private appInstanceId: string | undefined;
  private latestSnapshot: UiSnapshot | undefined;
  private rendererState: UiRendererState;
  private readonly supportedComponentCapabilities: Set<string>;
  private semanticProjectionAvailable = false;
  private usingTerminalFallback = false;
  private rendererStateBeforeFallback: UiRendererState | undefined;
  private readonly pendingActions = new Map<string, PendingWorkspaceAction>();
  private readonly pendingActionOrder: string[] = [];
  private inFlightEventId: string | undefined;

  constructor(options: WorkspaceUiSessionOptions) {
    this.options = options;
    this.supportedComponentCapabilities = new Set(
      options.supportedComponentCapabilities ?? UI_COMPONENT_CAPABILITIES,
    );
    this.rendererState = options.initialState ?? {
      rendererVisible: true,
      terminalVisible: false,
    };
    this.socketFactory = options.webSocketFactory
      ?? ((url) => new WebSocket(url));
  }

  get currentSnapshot(): UiSnapshot | undefined {
    return this.latestSnapshot;
  }

  get currentAppInstanceId(): string | undefined {
    return this.appInstanceId;
  }

  get pendingEventCount(): number {
    return this.pendingActions.size;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.connect();
  }

  stop(): void {
    this.running = false;
    this.attached = false;
    this.semanticProjectionAvailable = false;
    this.negotiatedProtocolVersion = undefined;
    if (this.reconnectTimer !== undefined) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = undefined;
    }
    const socket = this.socket;
    this.socket = undefined;
    socket?.close(1000, "workspace UI session stopped");
    this.options.onConnectionState?.({ status: "stopped" });
  }

  /**
   * Adds trusted routing context to a renderer-local action.
   *
   * Returns the stable event ID, or `undefined` until a snapshot is available.
   */
  send(action: UiAction, eventId = newEventId()): string | undefined {
    const existing = this.pendingActions.get(eventId);
    if (existing) {
      if (this.attached && existing.event && this.inFlightEventId === eventId) {
        this.sendJson(existing.event);
      }
      return eventId;
    }
    const snapshot = this.latestSnapshot;
    const appInstanceId = this.appInstanceId;
    if (!snapshot || !appInstanceId || !this.semanticProjectionAvailable) return undefined;

    if (canCoalesce(action)) {
      let queuedId: string | undefined;
      for (let index = this.pendingActionOrder.length - 1; index >= 0; index -= 1) {
        const candidate = this.pendingActionOrder[index]!;
        const pending = this.pendingActions.get(candidate);
        if (pending?.event === undefined && actionsCoalesce(pending?.action, action)) {
          queuedId = candidate;
          break;
        }
      }
      if (queuedId) {
        this.pendingActions.set(queuedId, { action });
        return queuedId;
      }
    }
    this.pendingActions.set(eventId, { action });
    this.pendingActionOrder.push(eventId);
    this.dispatchNextAction();
    return eventId;
  }

  setRendererState(state: UiRendererState): void {
    this.rendererState = state;
    if (!this.attached || !this.appInstanceId) return;
    const lifecycle: WorkspaceUiLifecycle = {
      type: "lifecycle",
      protocol: WORKSPACE_UI_PROTOCOL_NAME,
      protocolVersion: WORKSPACE_UI_PROTOCOL_VERSION,
      appSessionId: this.options.appSessionId,
      appInstanceId: this.appInstanceId,
      clientId: this.options.clientId,
      rendererId: this.options.rendererId,
      viewId: this.options.viewId,
      state,
    };
    this.sendJson(lifecycle);
  }

  requestSnapshot(): void {
    if (!this.attached || !this.appInstanceId) return;
    const request: WorkspaceUiRequestSnapshot = {
      type: "requestSnapshot",
      protocol: WORKSPACE_UI_PROTOCOL_NAME,
      protocolVersion: WORKSPACE_UI_PROTOCOL_VERSION,
      appSessionId: this.options.appSessionId,
      appInstanceId: this.appInstanceId,
      clientId: this.options.clientId,
      rendererId: this.options.rendererId,
      viewId: this.options.viewId,
    };
    this.sendJson(request);
  }

  private connect(): void {
    if (!this.running || this.socket) return;
    this.attached = false;
    this.negotiatedProtocolVersion = undefined;
    this.options.onConnectionState?.({ status: "connecting" });
    let socket: WorkspaceWebSocket;
    try {
      socket = this.socketFactory(String(this.options.url));
    } catch (error) {
      this.options.onError?.(asError(error));
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;
    socket.addEventListener("open", () => {
      if (this.socket !== socket || !this.running) return;
      this.reconnectAttempt = 0;
      this.sendResume();
    });
    socket.addEventListener("message", (event) => {
      if (this.socket !== socket || !this.running) return;
      this.receive((event as MessageEvent<unknown>).data, socket);
    });
    socket.addEventListener("error", () => {
      if (this.socket === socket) {
        this.options.onError?.(new Error("workspace UI WebSocket failed"));
      }
    });
    socket.addEventListener("close", () => {
      if (this.socket !== socket) return;
      this.socket = undefined;
      this.attached = false;
      this.scheduleReconnect();
    });
  }

  private sendResume(): void {
    const capabilities = Array.from(new Set([
      ...(this.options.capabilities ?? []),
      ...this.supportedComponentCapabilities,
      UI_DELTA_CAPABILITY,
    ]));
    const resume: WorkspaceUiResume = {
      type: "resume",
      protocol: WORKSPACE_UI_PROTOCOL_NAME,
      protocolVersion: WORKSPACE_UI_PROTOCOL_VERSION,
      appSessionId: this.options.appSessionId,
      clientId: this.options.clientId,
      renderer: {
        id: this.options.rendererId,
        kind: "web",
        capabilities,
      },
      viewId: this.options.viewId,
      ...(this.appInstanceId === undefined
        ? {}
        : { expectedAppInstanceId: this.appInstanceId }),
      ...(this.latestSnapshot === undefined
        ? {}
        : { lastSeenRevision: this.latestSnapshot.revision }),
      state: this.rendererState,
    };
    this.sendJson(resume);
  }

  private receive(data: unknown, source: WorkspaceWebSocket): void {
    if (typeof data === "string") {
      this.decode(data);
      return;
    }
    if (data instanceof ArrayBuffer) {
      this.decode(new TextDecoder().decode(data));
      return;
    }
    if (typeof Blob !== "undefined" && data instanceof Blob) {
      void data.text().then(
        (text) => {
          if (this.socket === source && this.running) this.decode(text);
        },
        (error: unknown) => this.options.onError?.(asError(error)),
      );
      return;
    }
    this.options.onError?.(new Error("workspace UI WebSocket sent a non-text frame"));
  }

  private decode(frame: string): void {
    let message: UiMessage;
    try {
      message = decodeUiMessage(frame.trim());
    } catch (error) {
      this.options.onError?.(asError(error));
      return;
    }
    if (message.type !== "attach"
      && message.type !== "attached"
      && message.type !== "error"
      && message.protocolVersion !== this.negotiatedProtocolVersion) {
      this.options.onError?.(new Error("Unpeel Host changed the negotiated UI version"));
      this.socket?.close(1008, "UI protocol version mismatch");
      return;
    }
    switch (message.type) {
      case "attached":
        this.handleAttached(message);
        break;
      case "snapshot":
        if (this.matchesView(message)) {
          if (this.accept(message)) this.options.onSnapshot(message);
        }
        break;
      case "delta":
        if (this.matchesView(message)) {
          if (this.latestSnapshot === undefined) {
            this.requestSnapshot();
            break;
          }
          try {
            const next = applyUiDelta(this.latestSnapshot, message);
            if (this.accept(next)) {
              this.options.onDelta?.(message);
              this.options.onSnapshot(next);
            }
          } catch (error) {
            this.options.onError?.(asError(error));
            this.requestSnapshot();
          }
        }
        break;
      case "ack":
        if (!this.matchesRenderer(message)) return;
        if (message.status !== "pending") {
          this.pendingActions.delete(message.eventId);
          const index = this.pendingActionOrder.indexOf(message.eventId);
          if (index >= 0) this.pendingActionOrder.splice(index, 1);
          if (this.inFlightEventId === message.eventId) {
            this.inFlightEventId = undefined;
          }
          if (message.status !== "stale") this.dispatchNextAction();
        }
        if (message.status === "stale") this.requestSnapshot();
        this.options.onAck?.(message);
        break;
      case "presence":
        if (message.appInstanceId === this.appInstanceId
          && message.viewId === this.options.viewId) {
          this.options.onPresence?.(publicPresence(message));
        }
        break;
      case "error":
        this.options.onError?.(message);
        break;
      case "attach":
      case "event":
      case "lifecycle":
      case "requestSnapshot":
        this.options.onError?.(
          new Error(`Unpeel Host sent forbidden ${message.type} frame`),
        );
        this.socket?.close(1008, "invalid workspace UI frame");
        break;
    }
  }

  private handleAttached(attached: UiAttached): void {
    if (negotiateUiProtocolVersion(
      attached.minProtocolVersion,
      attached.maxProtocolVersion,
    ) !== attached.protocolVersion
      || attached.clientId !== this.options.clientId
      || attached.rendererId !== this.options.rendererId
      || attached.viewId !== this.options.viewId) {
      this.options.onError?.(new Error("workspace UI attachment route mismatch"));
      this.socket?.close(1008, "attachment route mismatch");
      return;
    }

    const sameInstance = this.appInstanceId === undefined
      || this.appInstanceId === attached.appInstanceId;
    if (!sameInstance) {
      this.pendingActions.clear();
      this.pendingActionOrder.length = 0;
      this.inFlightEventId = undefined;
      this.latestSnapshot = undefined;
      this.semanticProjectionAvailable = false;
    }
    this.appInstanceId = attached.appInstanceId;
    this.negotiatedProtocolVersion = attached.protocolVersion;
    this.attached = true;
    this.options.onConnectionState?.({
      status: "attached",
      appInstanceId: attached.appInstanceId,
      resumed: attached.resumed,
    });
    this.options.onAttached?.(attached);

    if (sameInstance && this.inFlightEventId) {
      const event = this.pendingActions.get(this.inFlightEventId)?.event;
      if (event) this.sendJson(event);
    }
  }

  private matchesView(message: UiSnapshot | UiDelta): boolean {
    return message.appInstanceId === this.appInstanceId
      && message.clientId === this.options.clientId
      && message.viewId === this.options.viewId;
  }

  /** Keeps transport identity alive while the Host exposes the complete PTY. */
  private accept(snapshot: UiSnapshot): boolean {
    this.latestSnapshot = snapshot;
    const capabilities = uiNodeCapabilities(snapshot.root);
    if (capabilities !== undefined
      && capabilities.every((capability) => this.supportedComponentCapabilities.has(capability))
      && isBrowserSafeUiNode(snapshot.root)) {
      this.semanticProjectionAvailable = true;
      this.dispatchNextAction();
      if (this.usingTerminalFallback) {
        const restoredState = this.rendererStateBeforeFallback ?? {
          rendererVisible: true,
          terminalVisible: false,
        };
        this.usingTerminalFallback = false;
        this.rendererStateBeforeFallback = undefined;
        if (!rendererStatesEqual(this.rendererState, restoredState)) {
          this.setRendererState(restoredState);
        }
      }
      return true;
    }
    this.semanticProjectionAvailable = false;
    if (!this.usingTerminalFallback) {
      this.rendererStateBeforeFallback = this.rendererState;
      this.usingTerminalFallback = true;
    }
    const terminalState = { rendererVisible: false, terminalVisible: true };
    if (!rendererStatesEqual(this.rendererState, terminalState)) {
      this.setRendererState(terminalState);
    }
    this.options.onTerminalFallback?.(snapshot.root.type);
    return false;
  }

  private matchesRenderer(message: UiAck): boolean {
    return message.appInstanceId === this.appInstanceId
      && message.clientId === this.options.clientId
      && message.rendererId === this.options.rendererId
      && message.viewId === this.options.viewId;
  }

  private dispatchNextAction(): void {
    if (!this.attached || !this.semanticProjectionAvailable || this.inFlightEventId) return;
    const eventId = this.pendingActionOrder[0];
    const snapshot = this.latestSnapshot;
    const appInstanceId = this.appInstanceId;
    if (!eventId || !snapshot || !appInstanceId) return;
    const pending = this.pendingActions.get(eventId);
    if (!pending) return;
    const event: WorkspaceUiAction = {
      type: "action",
      protocol: WORKSPACE_UI_PROTOCOL_NAME,
      protocolVersion: WORKSPACE_UI_PROTOCOL_VERSION,
      appSessionId: this.options.appSessionId,
      appInstanceId,
      clientId: this.options.clientId,
      rendererId: this.options.rendererId,
      viewId: this.options.viewId,
      eventId,
      baseRevision: snapshot.revision,
      ...pending.action,
    };
    pending.event = event;
    this.inFlightEventId = eventId;
    this.sendJson(event);
  }

  private sendJson(message: WorkspaceUiClientMessage): void {
    const socket = this.socket;
    if (!socket || socket.readyState !== 1) return;
    try {
      socket.send(JSON.stringify(message));
    } catch (error) {
      this.options.onError?.(asError(error));
      socket.close(1011, "workspace UI send failed");
    }
  }

  private scheduleReconnect(): void {
    if (!this.running || this.reconnectTimer !== undefined) {
      if (!this.running) {
        this.options.onConnectionState?.({ status: "stopped" });
      }
      return;
    }
    this.reconnectAttempt = Math.min(this.reconnectAttempt + 1, 8);
    const delayMs = Math.min(100 * 2 ** (this.reconnectAttempt - 1), 5_000);
    this.options.onConnectionState?.({ status: "waitingToReconnect", delayMs });
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined;
      this.connect();
    }, delayMs);
  }
}

function rendererStatesEqual(left: UiRendererState, right: UiRendererState): boolean {
  return left.rendererVisible === right.rendererVisible
    && left.terminalVisible === right.terminalVisible;
}

function canCoalesce(action: UiAction): boolean {
  return action.kind === "change" || action.kind === "select";
}

function actionsCoalesce(left: UiAction | undefined, right: UiAction): boolean {
  return left !== undefined
    && left.nodeId === right.nodeId
    && left.action === right.action
    && left.kind === right.kind;
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}

function publicPresence(presence: UiPresence): UiPresence {
  return {
    ...presence,
    members: presence.members.map((member) => {
      const {
        grants: _grants,
        sourceSessionId: _sourceSessionId,
        ...participant
      } = member.participant;
      return { ...member, participant };
    }),
  };
}
