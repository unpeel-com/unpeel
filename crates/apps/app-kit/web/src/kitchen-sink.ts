import {
  CanvasPageRenderer,
  MarkdownEditorRenderer,
  MediaRenderer,
  MenuRenderer,
  PageRenderer,
  SurfaceRenderer,
  TextBoxRenderer,
  TreeRenderer,
  type SurfaceNode,
  type SurfacePresenterAdapter,
  type UiAction,
  type UiSnapshot,
} from "./index";

interface ComponentRenderer {
  render(snapshot: UiSnapshot): void;
  destroy(): void;
}

declare global {
  interface Window {
    unpeelRenderSnapshot?: (snapshot: UiSnapshot) => void;
    webkit?: {
      messageHandlers?: {
        unpeelAction?: {
          postMessage(action: UiAction): void;
        };
        unpeelDiagnostic?: {
          postMessage(message: string): void;
        };
      };
    };
  }
}

const container = requiredElement("app");
const revision = requiredElement("revision");
let renderer: ComponentRenderer | undefined;
let renderedType: string | undefined;

interface ConnectedSurfaceModule {
  default(input: string): Promise<unknown>;
  startRemote(terminalOutput: (columns: number, rows: number, bytes: Uint8Array) => void): Promise<void>;
  sendRemoteKey(kind: number): void;
  setRemoteBackground(red: number, green: number, blue: number, alpha: number): void;
}

class KitchenSinkSurfaceAdapter implements SurfacePresenterAdapter {
  private readonly root: HTMLDivElement;
  private readonly presenter: HTMLDivElement;
  private readonly keyHandler: (event: KeyboardEvent) => void;
  private destroyed = false;
  private surfaceModule: ConnectedSurfaceModule | undefined;
  private surface: SurfaceNode;

  constructor(container: HTMLElement, surface: SurfaceNode) {
    this.surface = surface;
    this.root = document.createElement("div");
    this.root.id = "stage";
    this.root.className = "surface-stage";

    this.presenter = document.createElement("div");
    this.presenter.id = "presenter";
    this.presenter.className = "surface-presenter";
    const canvas = document.createElement("canvas");
    canvas.id = "surface";
    const status = document.createElement("div");
    status.id = "runtime-status";
    status.className = "surface-status";
    status.textContent = "Starting local WebGPU presenter…";
    const error = document.createElement("div");
    error.id = "error";
    error.className = "surface-error";
    this.presenter.append(canvas);
    this.root.append(this.presenter, status, error);
    container.replaceChildren(this.root);

    this.keyHandler = (event): void => {
      const kind = surfaceKeyKind(event);
      if (kind === undefined) return;
      event.preventDefault();
      try {
        this.surfaceModule?.sendRemoteKey(kind);
      } catch (cause) {
        error.textContent = cause instanceof Error ? cause.message : String(cause);
      }
    };
    this.presenter.addEventListener("keydown", this.keyHandler);
    this.update(surface);
    void this.start(error);
  }

  update(surface: SurfaceNode): void {
    this.surface = surface;
    const interactive = (surface.inputPolicy ?? "none") !== "none";
    this.presenter.style.pointerEvents = interactive ? "auto" : "none";
    if (surface.inputPolicy === "pointerAndKeyboard") {
      this.presenter.tabIndex = 0;
      this.presenter.setAttribute("aria-label", "Interactive Surface");
    } else {
      this.presenter.removeAttribute("tabindex");
    }
    this.applyBackground();
  }

  viewportSize(): { w: number; h: number } {
    return { w: 960, h: 600 };
  }

  destroy(): void {
    this.destroyed = true;
    this.presenter.removeEventListener("keydown", this.keyHandler);
    this.root.remove();
  }

  private async start(errorElement: HTMLElement): Promise<void> {
    const query = new URLSearchParams(window.location.search);
    const moduleURL = query.get("surfaceModule");
    const wasmURL = query.get("surfaceWasm");
    if (!moduleURL || !wasmURL) {
      errorElement.textContent = "This Host did not supply a Surface WebGPU module.";
      return;
    }
    try {
      window.webkit?.messageHandlers?.unpeelDiagnostic?.postMessage(
        `loading Surface module; WebGPU=${"gpu" in navigator}`,
      );
      const module = await import(moduleURL) as ConnectedSurfaceModule;
      await module.default(wasmURL);
      if (!this.destroyed) {
        await module.startRemote(() => {});
        this.surfaceModule = module;
        this.applyBackground();
        window.webkit?.messageHandlers?.unpeelDiagnostic?.postMessage(
          "Surface WebGPU presenter connected",
        );
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      errorElement.textContent = message;
      window.webkit?.messageHandlers?.unpeelDiagnostic?.postMessage(`Surface failed: ${message}`);
    }
  }

  private applyBackground(): void {
    if (!this.surfaceModule) return;
    const [red, green, blue, alpha] = surfaceBackgroundBytes(this.surface);
    this.surfaceModule.setRemoteBackground(red, green, blue, alpha);
  }

}

function surfaceBackgroundBytes(surface: SurfaceNode): [number, number, number, number] {
  const background = surface.background ?? { kind: "transparent" };
  if (background.kind === "transparent") return [0, 0, 0, 0];
  const hex = background.color.slice(1);
  return [
    Number.parseInt(hex.slice(0, 2), 16),
    Number.parseInt(hex.slice(2, 4), 16),
    Number.parseInt(hex.slice(4, 6), 16),
    hex.length === 8 ? Number.parseInt(hex.slice(6, 8), 16) : 255,
  ];
}

function surfaceKeyKind(event: KeyboardEvent): number | undefined {
  switch (event.key) {
    case "ArrowUp":
    case "ArrowLeft":
      return 10;
    case "ArrowDown":
    case "ArrowRight":
      return 11;
    case "Home":
      return 14;
    case "End":
      return 15;
    case "Enter":
    case " ":
      return 2;
    default:
      return undefined;
  }
}

function requiredElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing #${id}`);
  return element;
}

function postAction(action: UiAction): void {
  window.webkit?.messageHandlers?.unpeelAction?.postMessage(action);
}

function makeRenderer(type: string): ComponentRenderer | undefined {
  switch (type) {
    case "canvasPage":
      return new CanvasPageRenderer(
        container,
        (surfaceContainer, surface) => new KitchenSinkSurfaceAdapter(
          surfaceContainer,
          surface,
        ),
        postAction,
      );
    case "markdownEditor":
      return new MarkdownEditorRenderer(container, postAction);
    case "media":
      return new MediaRenderer(container, postAction, {
        onError(error) {
          showError(error.message);
        },
      });
    case "menu":
      return new MenuRenderer(container, postAction);
    case "page":
      return new PageRenderer(container, postAction);
    case "surface":
      return new SurfaceRenderer(
        container,
        (surfaceContainer, surface) => new KitchenSinkSurfaceAdapter(
          surfaceContainer,
          surface,
        ),
      );
    case "textBox":
      return new TextBoxRenderer(container, postAction);
    case "tree":
      return new TreeRenderer(container, postAction);
    default:
      return undefined;
  }
}

function showError(message: string): void {
  const fallback = document.createElement("div");
  fallback.className = "unpeel-fallback";
  const title = document.createElement("strong");
  title.textContent = "Terminal fallback required";
  const detail = document.createElement("span");
  detail.textContent = message;
  fallback.append(title, detail);
  container.replaceChildren(fallback);
}

window.unpeelRenderSnapshot = (snapshot): void => {
  try {
    const type = snapshot.root.type;
    if (type !== renderedType) {
      renderer?.destroy();
      renderer = makeRenderer(type);
      renderedType = type;
    }
    if (!renderer) {
      showError(`This web renderer does not recognize “${type}”.`);
      return;
    }
    renderer.render(snapshot);
    revision.textContent = `r${snapshot.revision} · ${type}`;
  } catch (error) {
    showError(error instanceof Error ? error.message : String(error));
  }
};
