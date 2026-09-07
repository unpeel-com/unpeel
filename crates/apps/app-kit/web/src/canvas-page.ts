import {
  canvasSurfaceNode,
  isButtonControl,
  isCanvasPageNode,
  type ButtonSpec,
  type UiAction,
  type UiSnapshot,
} from "./protocol";
import { SurfaceRenderer, type SurfacePresenterFactory } from "./surface";

/** Browser interpretation of the closed CanvasPage composition. */
export class CanvasPageRenderer {
  readonly element: HTMLElement;

  private readonly onAction: (action: UiAction) => void;
  private readonly surfaceHost: HTMLDivElement;
  private readonly toolbar: HTMLElement;
  private readonly surfaceRenderer: SurfaceRenderer;

  constructor(
    container: HTMLElement,
    createPresenter: SurfacePresenterFactory,
    onAction: (action: UiAction) => void,
  ) {
    this.onAction = onAction;
    this.element = document.createElement("section");
    this.element.className = "unpeel-canvas-page";
    this.surfaceHost = document.createElement("div");
    this.surfaceHost.className = "unpeel-canvas-page__surface";
    this.toolbar = document.createElement("header");
    this.toolbar.className = "unpeel-canvas-page__toolbar";
    this.toolbar.setAttribute("role", "toolbar");
    this.element.append(this.surfaceHost, this.toolbar);
    container.replaceChildren(this.element);
    this.surfaceRenderer = new SurfaceRenderer(this.surfaceHost, createPresenter);
  }

  render(snapshot: UiSnapshot): void {
    if (!isCanvasPageNode(snapshot.root)) {
      throw new Error(`CanvasPageRenderer cannot render ${snapshot.root.type}`);
    }
    const page = snapshot.root;
    if (!page.controls.every(isButtonControl)) {
      throw new Error("CanvasPage contains an unsupported control");
    }
    this.surfaceRenderer.renderSurface(canvasSurfaceNode(page.surface));
    this.toolbar.replaceChildren();
    const title = document.createElement("strong");
    title.className = "unpeel-canvas-page__title";
    title.textContent = page.title;
    this.toolbar.setAttribute("aria-label", page.title);
    this.toolbar.append(title);
    const spacer = document.createElement("span");
    spacer.className = "unpeel-canvas-page__spacer";
    this.toolbar.append(spacer);
    for (const control of page.controls) this.toolbar.append(this.button(control));
  }

  destroy(): void {
    this.surfaceRenderer.destroy();
    this.element.remove();
  }

  private button(spec: ButtonSpec): HTMLButtonElement {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "unpeel-canvas-page__button";
    button.dataset.role = spec.role ?? "default";
    button.textContent = spec.label;
    button.addEventListener("click", () => {
      this.onAction({
        nodeId: spec.id,
        action: spec.action,
        kind: "activate",
        value: { type: "none" },
      });
    });
    return button;
  }
}
