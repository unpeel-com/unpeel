import {
  type SurfaceNode,
  type SurfaceViewportSize,
  type UiSnapshot,
  isSurfaceNode,
} from "./protocol";

/**
 * Adapter supplied by unpeel-surface's connected WebGPU presenter.
 *
 * It owns the USRF decoder, canvas, local GPU renderer, resources, and input
 * packets. Implementations must never accept composed frames as a fallback.
 */
export interface SurfacePresenterAdapter {
  update(surface: SurfaceNode): void;
  viewportSize?(): SurfaceViewportSize | undefined;
  destroy(): void;
}

export type SurfacePresenterFactory = (
  container: HTMLElement,
  surface: SurfaceNode,
) => SurfacePresenterAdapter;

/** Browser allocation/delegation wrapper for one reference-only Surface. */
export class SurfaceRenderer {
  readonly element: HTMLDivElement;

  private readonly createPresenter: SurfacePresenterFactory;
  private presenter: SurfacePresenterAdapter | undefined;
  private referenceKey: string | undefined;

  constructor(container: HTMLElement, createPresenter: SurfacePresenterFactory) {
    this.createPresenter = createPresenter;
    this.element = document.createElement("div");
    this.element.className = "unpeel-surface";
    this.element.style.overflow = "hidden";
    this.element.style.position = "relative";
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isSurfaceNode(snapshot.root)) {
      throw new Error(`SurfaceRenderer cannot render ${snapshot.root.type}`);
    }
    this.renderSurface(snapshot.root);
  }

  /** Render a Surface from an explicitly named, closed parent slot. */
  renderSurface(surface: SurfaceNode): void {
    const key = `${surface.reference.sessionId}\0${surface.reference.streamId}`;
    if (key !== this.referenceKey) {
      this.presenter?.destroy();
      this.element.replaceChildren();
      this.presenter = this.createPresenter(this.element, surface);
      this.referenceKey = key;
    } else {
      this.presenter?.update(surface);
    }
    this.applyBox(surface);
  }

  /** Re-resolves a one-axis point request after a USRF resize arrives. */
  updateLayout(surface: SurfaceNode): void {
    this.applyBox(surface);
  }

  destroy(): void {
    this.presenter?.destroy();
    this.presenter = undefined;
    this.referenceKey = undefined;
    this.element.remove();
  }

  private applyBox(surface: SurfaceNode): void {
    const background = surface.background ?? { kind: "transparent" as const };
    this.element.style.backgroundColor = background.kind === "solid"
      ? background.color
      : "transparent";
    const size = resolveSurfacePointSize(surface, this.presenter?.viewportSize?.());
    setAxis(this.element.style, "width", size?.w ?? surface.points?.w);
    setAxis(this.element.style, "height", size?.h ?? surface.points?.h);
    this.element.style.pointerEvents = (surface.inputPolicy ?? "none") === "none"
      ? "none"
      : "auto";
    if ((surface.inputPolicy ?? "none") === "pointerAndKeyboard") {
      this.element.tabIndex = 0;
    } else {
      this.element.removeAttribute("tabindex");
    }
  }
}

/** Derives a missing requested axis from the presenter-owned live viewport. */
export function resolveSurfacePointSize(
  surface: SurfaceNode,
  viewport: SurfaceViewportSize | undefined,
): { w: number; h: number } | undefined {
  if (!surface.points) return undefined;
  const { w, h } = surface.points;
  if (w !== undefined && h !== undefined) return { w, h };
  if (!viewport) return undefined;
  if (w !== undefined) return { w, h: ratioCeil(w, viewport.h, viewport.w) };
  if (h !== undefined) return { w: ratioCeil(h, viewport.w, viewport.h), h };
  return undefined;
}

function ratioCeil(value: number, numerator: number, denominator: number): number {
  const scaled = BigInt(value) * BigInt(Math.max(numerator, 1));
  const divisor = BigInt(Math.max(denominator, 1));
  const resolved = (scaled + divisor - 1n) / divisor;
  return Number(resolved > 4_294_967_295n ? 4_294_967_295n : resolved);
}

function setAxis(style: CSSStyleDeclaration, axis: "width" | "height", value?: number): void {
  if (value === undefined) {
    style.removeProperty(axis);
  } else {
    style[axis] = `${value}px`;
  }
}
