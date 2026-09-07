import {
  MAX_INLINE_MEDIA_BYTES,
  type MediaBlobSource,
  type MediaNode,
  type UiAction,
  type UiSnapshot,
  isMediaNode,
  uiAction,
} from "./protocol";

/** Fetches one content-addressed blob through the existing authorized Host route. */
export type MediaBlobResolver = (
  source: MediaBlobSource,
) => Promise<Blob | ArrayBuffer>;

export interface MediaRendererOptions {
  resolveBlob?: MediaBlobResolver;
  onError?: (error: Error) => void;
}

/** Browser interpretation of App Kit's static Media component. */
export class MediaRenderer {
  readonly element: HTMLImageElement;

  private readonly onAction: (action: UiAction) => void;
  private readonly options: MediaRendererOptions;
  private media?: MediaNode;
  private objectUrl: string | undefined;
  private generation = 0;

  constructor(
    container: HTMLElement,
    onAction: (action: UiAction) => void,
    options: MediaRendererOptions = {},
  ) {
    this.onAction = onAction;
    this.options = options;
    this.element = document.createElement("img");
    this.element.className = "unpeel-media";
    this.element.draggable = false;
    this.element.style.display = "block";
    this.element.addEventListener("click", () => this.activate());
    this.element.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      this.activate();
    });
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isMediaNode(snapshot.root)) {
      throw new Error(`MediaRenderer cannot render ${snapshot.root.type}`);
    }
    const media = snapshot.root;
    this.media = media;
    this.generation += 1;
    this.element.alt = media.alt;
    this.element.style.objectFit = media.fit ?? "contain";
    const size = resolveMediaPointSize(media);
    this.element.style.width = `${size.w}px`;
    this.element.style.height = `${size.h}px`;
    if (media.activate) {
      this.element.role = "button";
      this.element.tabIndex = 0;
    } else {
      this.element.removeAttribute("role");
      this.element.removeAttribute("tabindex");
    }
    this.loadSource(media, this.generation);
  }

  destroy(): void {
    this.generation += 1;
    this.revokeObjectUrl();
    this.element.remove();
  }

  private activate(): void {
    const media = this.media;
    if (!media?.activate) return;
    this.onAction(uiAction(media.id, media.activate, "activate"));
  }

  private loadSource(media: MediaNode, generation: number): void {
    switch (media.source.kind) {
      case "inline": {
        this.revokeObjectUrl();
        try {
          const decodedLength = inlineBase64Length(media.source.base64);
          if (decodedLength > MAX_INLINE_MEDIA_BYTES
            || !/^image\/[A-Za-z0-9!#$&^_.+/-]+$/.test(media.source.mediaType)) {
            throw new Error("Inline Media reference is invalid or exceeds 256 KiB");
          }
        } catch (error) {
          this.element.removeAttribute("src");
          this.fail(asError(error));
          return;
        }
        this.element.src = `data:${media.source.mediaType};base64,${media.source.base64}`;
        return;
      }
      case "blob":
        this.revokeObjectUrl();
        this.element.removeAttribute("src");
        void this.loadBlob(media.source, generation);
        return;
      case "path":
        // A conforming broker translates paths to grant-checked blob refs.
        this.revokeObjectUrl();
        this.element.removeAttribute("src");
        this.fail(new Error("Browser Media cannot consume filesystem path references"));
        return;
    }
  }

  private async loadBlob(source: MediaBlobSource, generation: number): Promise<void> {
    const resolver = this.options.resolveBlob;
    if (!resolver) {
      this.fail(new Error("Media blob requires the existing Host's authorized resolver"));
      return;
    }
    try {
      const resolved = await resolver(source);
      const buffer = resolved instanceof Blob
        ? await resolved.arrayBuffer()
        : resolved;
      await verifyMediaBlobBytes(source, buffer);
      if (generation !== this.generation) return;
      const blob = new Blob([buffer], { type: source.mediaType });
      this.revokeObjectUrl();
      this.objectUrl = URL.createObjectURL(blob);
      this.element.src = this.objectUrl;
    } catch (error) {
      if (generation === this.generation) this.fail(asError(error));
    }
  }

  private revokeObjectUrl(): void {
    if (!this.objectUrl) return;
    URL.revokeObjectURL(this.objectUrl);
    this.objectUrl = undefined;
  }

  private fail(error: Error): void {
    this.options.onError?.(error);
  }
}

/** Resolves one omitted point axis from intrinsic pixel aspect. */
export function resolveMediaPointSize(media: MediaNode): { w: number; h: number } {
  const width = media.points?.w;
  const height = media.points?.h;
  if (width !== undefined && height !== undefined) return { w: width, h: height };
  if (width !== undefined) {
    return {
      w: width,
      h: ratioCeil(width, media.intrinsic.h, media.intrinsic.w),
    };
  }
  if (height !== undefined) {
    return {
      w: ratioCeil(height, media.intrinsic.w, media.intrinsic.h),
      h: height,
    };
  }
  return { w: media.intrinsic.w, h: media.intrinsic.h };
}

function ratioCeil(value: number, numerator: number, denominator: number): number {
  const scaled = BigInt(value) * BigInt(numerator);
  const divisor = BigInt(Math.max(denominator, 1));
  const resolved = (scaled + divisor - 1n) / divisor;
  return Number(resolved > 4_294_967_295n ? 4_294_967_295n : resolved);
}

/** Verifies bytes returned by an out-of-band Host blob route. */
export async function verifyMediaBlobBytes(
  source: MediaBlobSource,
  buffer: ArrayBuffer,
): Promise<void> {
  if (buffer.byteLength !== source.byteLength) {
    throw new Error("Media blob byte length mismatch");
  }
  const digest = await sha256Hex(buffer);
  if (digest !== source.sha256) {
    throw new Error("Media blob SHA-256 mismatch");
  }
}

function inlineBase64Length(value: string): number {
  if (value.length === 0 || value.length > 349_528 || value.length % 4 !== 0
    || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) {
    throw new Error("Inline Media base64 is invalid");
  }
  const decoded = atob(value);
  if (btoa(decoded) !== value) throw new Error("Inline Media base64 is non-canonical");
  return decoded.length;
}

async function sha256Hex(buffer: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", buffer);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}
