import {
  type FooterActionsSpec,
  type MarkdownEditorNode,
  type MarkdownPresentation,
  type MenuSpec,
  type TextEdit,
  type TextPosition,
  type TextSelection,
  type UiAction,
  type UiSnapshot,
  isMarkdownCommandHintVisible,
  isMarkdownEditorNode,
  markdownMenuTriggerForTextInput,
  uiAction,
} from "./protocol";
import { handleFooterAccelerator, renderFooterActions } from "./footer";
import { renderSemanticMenu } from "./menu";

export interface MarkdownEditorRendererOptions {
  renderMarkdown?: (source: string) => Node | string;
}

export interface MarkdownOffsetEdit {
  start: number;
  end: number;
  replacement: string;
}

/** Returns the checkbox-character edit when a caret lands on a task marker. */
export function markdownTaskToggleAtOffset(
  text: string,
  offset: number,
): MarkdownOffsetEdit | undefined {
  if (offset < 0 || offset > text.length) return undefined;
  const lineStart = text.lastIndexOf("\n", Math.max(offset - 1, 0)) + 1;
  const nextBreak = text.indexOf("\n", offset);
  const lineEnd = nextBreak < 0 ? text.length : nextBreak;
  const line = text.slice(lineStart, lineEnd);
  const match = /^(\s*(?:(?:[-+*])|(?:\d+\.))\s+)\[([ xX])\]/u.exec(line);
  if (!match) return undefined;
  const markerStart = lineStart + match[1]!.length;
  const markerEnd = markerStart + 2;
  if (offset < markerStart || offset > markerEnd) return undefined;
  return {
    start: markerStart + 1,
    end: markerStart + 2,
    replacement: match[2] === " " ? "x" : " ",
  };
}

function droppedMarkdownText(transfer: DataTransfer): string {
  const uri = transfer.getData("text/uri-list")
    .split(/\r?\n/u)
    .find((line) => line !== "" && !line.startsWith("#"));
  let text = "";
  if (uri) {
    try {
      const url = new URL(uri);
      text = url.protocol === "file:" ? decodeURIComponent(url.pathname) : uri;
    } catch {
      text = uri;
    }
  }
  if (text === "") text = transfer.getData("text/plain");
  if (text === "" && transfer.files.length > 0) {
    // Browsers intentionally hide local absolute paths. A filename remains a
    // useful Markdown insertion while native trusted renderers can insert the
    // complete filesystem path.
    text = Array.from(transfer.files, (file) => file.name).join("\n");
  }
  return text.replaceAll("\0", "").replaceAll("\r\n", "\n").replaceAll("\r", "\n");
}

/// DOM renderer for the same terminal-backed MarkdownEditor component.
export class MarkdownEditorRenderer {
  readonly element: HTMLElement;

  private readonly toolbar: HTMLElement;
  private readonly body: HTMLElement;
  private readonly footerElement: HTMLElement;
  private readonly textarea: HTMLTextAreaElement;
  private readonly commandHint: HTMLElement;
  private readonly preview: HTMLElement;
  private readonly insertMenu: HTMLElement;
  private readonly contextMenu: HTMLElement;
  private readonly onAction: (action: UiAction) => void;
  private readonly options: MarkdownEditorRendererOptions;
  private readonly dismissContextMenuOnPointerDown: (event: PointerEvent) => void;
  private snapshot?: UiSnapshot;
  private editor?: MarkdownEditorNode;
  private applyingSnapshot = false;
  private keyEdited = false;
  private hasRendered = false;
  private authoritativeText = "";
  private authoritativeSelection: TextSelection | undefined;
  private authoritativeRevision = -1;
  private inFlightText: string | undefined;
  private flushTimer: number | undefined;
  private semanticSelectedId: string | undefined;
  private footer: FooterActionsSpec | undefined;

  constructor(
    container: HTMLElement,
    onAction: (action: UiAction) => void,
    options: MarkdownEditorRendererOptions = {},
  ) {
    this.onAction = onAction;
    this.options = options;
    this.element = document.createElement("section");
    this.element.className = "unpeel-markdown-editor";
    this.toolbar = document.createElement("header");
    this.toolbar.className = "unpeel-markdown-editor__toolbar";
    this.body = document.createElement("div");
    this.body.className = "unpeel-markdown-editor__body";
    this.footerElement = document.createElement("footer");
    this.footerElement.className = "unpeel-footer-actions";
    this.textarea = document.createElement("textarea");
    this.textarea.className = "unpeel-markdown-editor__source";
    this.textarea.spellcheck = false;
    this.commandHint = document.createElement("span");
    this.commandHint.className = "unpeel-markdown-editor__command-hint";
    this.commandHint.setAttribute("role", "note");
    this.commandHint.hidden = true;
    this.commandHint.style.position = "absolute";
    this.commandHint.style.pointerEvents = "none";
    this.commandHint.style.whiteSpace = "pre";
    this.commandHint.style.opacity = "0.5";
    this.commandHint.style.zIndex = "1";
    this.preview = document.createElement("article");
    this.preview.className = "unpeel-markdown-editor__preview";
    this.insertMenu = document.createElement("div");
    this.insertMenu.className = "unpeel-markdown-editor__insert-menu";
    this.insertMenu.setAttribute("role", "listbox");
    this.insertMenu.hidden = true;
    this.insertMenu.style.display = "none";
    this.insertMenu.style.position = "absolute";
    this.insertMenu.style.zIndex = "20";
    this.insertMenu.style.insetInlineStart = "12px";
    this.insertMenu.style.insetBlockStart = "12px";
    this.insertMenu.style.minWidth = "250px";
    this.insertMenu.style.padding = "6px";
    this.insertMenu.style.border = "1px solid color-mix(in srgb, CanvasText 14%, transparent)";
    this.insertMenu.style.borderRadius = "8px";
    this.insertMenu.style.background = "Canvas";
    this.insertMenu.style.boxShadow = "0 10px 30px rgb(0 0 0 / 20%)";
    this.contextMenu = document.createElement("div");
    this.contextMenu.className = "unpeel-markdown-editor__context-menu";
    this.contextMenu.hidden = true;
    this.contextMenu.style.position = "absolute";
    this.contextMenu.style.zIndex = "30";
    this.dismissContextMenuOnPointerDown = (event) => {
      const target = event.target;
      if (this.contextMenu.hidden
        || !(target instanceof Node)
        || this.contextMenu.contains(target)) return;
      this.contextMenu.hidden = true;
    };
    document.addEventListener("pointerdown", this.dismissContextMenuOnPointerDown);
    this.contextMenu.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      this.contextMenu.hidden = true;
      this.textarea.focus();
    }, { capture: true });
    this.body.style.position = "relative";
    this.body.append(this.textarea, this.commandHint, this.preview);
    this.body.append(this.insertMenu, this.contextMenu);
    this.element.append(this.toolbar, this.body, this.footerElement);
    this.element.addEventListener("keydown", (event) => {
      handleFooterAccelerator(event, this.footer, this.onAction);
    });
    container.replaceChildren(this.element);

    this.textarea.addEventListener("keydown", (event) => {
      this.keyEdited = false;
      this.handleEditorKey(event);
    });
    this.textarea.addEventListener("input", () => this.textChanged());
    this.textarea.addEventListener("keyup", () => {
      if (!this.keyEdited) this.selectionChanged();
      this.keyEdited = false;
    });
    this.textarea.addEventListener("mouseup", (event) => {
      if (event.button === 0 && this.toggleTaskAtCaret()) return;
      this.selectionChanged();
    });
    this.textarea.addEventListener("blur", () => this.selectionChanged());
    this.textarea.addEventListener("scroll", () => this.renderCommandHint());
    this.textarea.addEventListener("dragover", (event) => {
      if (!event.dataTransfer) return;
      if (event.dataTransfer.files.length > 0
        || event.dataTransfer.types.includes("text/plain")
        || event.dataTransfer.types.includes("text/uri-list")) {
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
      }
    });
    this.textarea.addEventListener("drop", (event) => this.insertDrop(event));
    this.textarea.addEventListener("contextmenu", (event) => {
      if (this.editor?.contextMenu === undefined) return;
      event.preventDefault();
      this.showContextMenu(event, this.editor.contextMenu);
    });
  }

  render(snapshot: UiSnapshot): void {
    if (!isMarkdownEditorNode(snapshot.root)) {
      throw new Error(`MarkdownEditorRenderer cannot render ${snapshot.root.type}`);
    }
    const editor = snapshot.root;
    this.applyingSnapshot = true;
    try {
      const previousSelection = this.authoritativeSelection;
      let shouldApplyAuthoritativeSelection = !this.hasRendered;
      if (!this.hasRendered) {
        this.hasRendered = true;
        this.authoritativeText = editor.text;
        this.authoritativeRevision = snapshot.revision;
        this.textarea.value = editor.text;
      } else {
        const previousText = this.authoritativeText;
        const previousRevision = this.authoritativeRevision;
        const localText = this.textarea.value;
        const hadLocalChanges = localText !== previousText;
        const incomingChanged = snapshot.revision !== previousRevision
          || editor.text !== previousText;
        if (incomingChanged) {
          this.authoritativeText = editor.text;
          this.authoritativeRevision = snapshot.revision;
          if (
            this.inFlightText !== undefined
            && (snapshot.revision > previousRevision || editor.text === this.inFlightText)
          ) {
            this.inFlightText = undefined;
          }
          if (!hadLocalChanges || localText === editor.text) {
            this.textarea.value = editor.text;
            shouldApplyAuthoritativeSelection = !hadLocalChanges
              && editor.text !== previousText;
          }
        }
      }
      if (
        !shouldApplyAuthoritativeSelection
        && previousSelection !== undefined
        && !textSelectionsEqual(previousSelection, editor.selection)
        && this.textarea.value === editor.text
        && this.inFlightText === undefined
      ) {
        const previousOffsets = selectionOffsets(editor.text, previousSelection);
        const incomingOffsets = selectionOffsets(editor.text, editor.selection);
        if (incomingOffsets !== undefined) {
          const ownsFocus = document.activeElement === this.textarea;
          shouldApplyAuthoritativeSelection = !ownsFocus
            || (previousOffsets !== undefined
              && textareaSelectionMatches(this.textarea, previousOffsets))
            || textareaSelectionMatches(this.textarea, incomingOffsets);
        }
      }
      this.authoritativeSelection = editor.selection;
      this.snapshot = snapshot;
      this.editor = editor;
      this.footer = editor.footer;
      if (editor.contextMenu === undefined) this.contextMenu.hidden = true;
      this.textarea.readOnly = editor.readOnly ?? false;
      this.textarea.placeholder = editor.placeholder ?? "";
      const selection = shouldApplyAuthoritativeSelection
        && this.textarea.value === this.authoritativeText
        && this.inFlightText === undefined
        ? selectionOffsets(editor.text, editor.selection)
        : undefined;
      if (selection !== undefined) {
        this.textarea.setSelectionRange(
          selection.start,
          selection.end,
          selection.direction,
        );
      }
      this.renderToolbar(editor);
      renderFooterActions(this.footerElement, editor.footer, this.onAction);
      this.renderPreview(editor.text);
      this.applyPresentation(editor.presentation ?? "source");
      this.renderCommandHint();
      this.refreshInsertMenu();
    } finally {
      this.applyingSnapshot = false;
    }
    if (this.textarea.value !== this.authoritativeText && this.inFlightText === undefined) {
      this.scheduleFlush();
    }
  }

  destroy(): void {
    if (this.flushTimer !== undefined) window.clearTimeout(this.flushTimer);
    document.removeEventListener("pointerdown", this.dismissContextMenuOnPointerDown);
    this.element.remove();
  }

  private textChanged(): void {
    if (this.applyingSnapshot || !this.snapshot || !this.editor) return;
    const action = componentAction(
      this.editor.actions,
      "replaceRange",
      "replace-range",
    );
    if (this.editor.readOnly || !action) return;
    this.keyEdited = true;
    this.refreshInsertMenu();
    this.scheduleFlush();
  }

  private toggleTaskAtCaret(): boolean {
    if (!this.editor || this.editor.readOnly || this.textarea.selectionStart !== this.textarea.selectionEnd) {
      return false;
    }
    const caret = this.textarea.selectionStart;
    const edit = markdownTaskToggleAtOffset(this.textarea.value, caret);
    if (!edit) return false;
    this.textarea.setRangeText(edit.replacement, edit.start, edit.end, "preserve");
    this.textarea.setSelectionRange(caret, caret);
    this.textChanged();
    return true;
  }

  private insertDrop(event: DragEvent): void {
    if (!event.dataTransfer || !this.editor || this.editor.readOnly) return;
    const insertion = droppedMarkdownText(event.dataTransfer);
    if (insertion === "") return;
    event.preventDefault();
    this.textarea.setRangeText(
      insertion,
      this.textarea.selectionStart,
      this.textarea.selectionEnd,
      "end",
    );
    this.textChanged();
    this.textarea.focus();
  }

  private selectionChanged(): void {
    if (
      this.applyingSnapshot || !this.snapshot || !this.editor
      || this.textarea.value !== this.authoritativeText
      || this.inFlightText !== undefined
    ) return;
    const action = componentAction(
      this.editor.actions,
      "setSelection",
      "set-selection",
    );
    if (!action) return;
    const start = positionAtUtf16Offset(
      this.textarea.value,
      this.textarea.selectionStart,
    );
    const end = positionAtUtf16Offset(
      this.textarea.value,
      this.textarea.selectionEnd,
    );
    const backwards = this.textarea.selectionDirection === "backward";
    const selection: TextSelection = {
      anchor: backwards ? end : start,
      head: backwards ? start : end,
    };
    this.onAction(uiAction(
      this.editor.id,
      action,
      "select",
      { type: "textSelection", value: selection },
    ));
  }

  private scheduleFlush(): void {
    if (this.flushTimer !== undefined) window.clearTimeout(this.flushTimer);
    this.flushTimer = undefined;
    if (this.inFlightText !== undefined || this.textarea.value === this.authoritativeText) {
      return;
    }
    this.flushTimer = window.setTimeout(() => {
      this.flushTimer = undefined;
      this.flushLocalEdit();
    }, 90);
  }

  private flushLocalEdit(): void {
    if (!this.editor || this.inFlightText !== undefined || this.editor.readOnly) return;
    const action = componentAction(
      this.editor.actions,
      "replaceRange",
      "replace-range",
    );
    const edit = diffText(this.authoritativeText, this.textarea.value);
    if (!action || !edit) return;
    this.inFlightText = this.textarea.value;
    this.onAction(uiAction(
      this.editor.id,
      action,
      "change",
      { type: "textEdit", value: edit },
    ));
  }

  private handleEditorKey(event: KeyboardEvent): void {
    const openMenu = this.editor?.actions?.openMenu;
    const trigger = this.editor === undefined
      ? undefined
      : markdownMenuTriggerForTextInput(this.editor, event.key);
    if (openMenu !== undefined
      && trigger !== undefined
      && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault();
      this.onAction(uiAction(
        this.editor!.id,
        openMenu,
        "command",
        { type: "text", value: trigger },
      ));
      return;
    }
    if (this.editor?.insertMenu !== undefined) {
      if (event.key === "ArrowUp" || event.key === "ArrowDown") {
        event.preventDefault();
        this.moveSemanticSelection(event.key === "ArrowUp" ? -1 : 1);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        this.activateSemanticItem(this.editor.insertMenu);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        if (this.editor.insertMenu.dismiss !== undefined) {
          this.onAction(uiAction(this.editor.id, this.editor.insertMenu.dismiss, "cancel"));
        }
        return;
      }
    }
    if (event.key === "Backspace") {
      const edit = markdownBackspaceEdit(
        this.textarea.value,
        this.textarea.selectionStart,
        this.textarea.selectionEnd,
      );
      if (edit) {
        event.preventDefault();
        this.replaceLocalRange(
          edit.lineStart,
          edit.lineEnd,
          edit.replacement,
          edit.caretOffset,
        );
      }
    }
  }

  private refreshInsertMenu(): void {
    if (this.textarea.hidden || this.textarea.readOnly) {
      this.closeInsertMenu();
      return;
    }
    const menu = this.editor?.insertMenu;
    if (menu === undefined || this.editor === undefined) {
      this.closeInsertMenu();
      return;
    }
    const enabled = menu.items.filter((item) => item.disabled !== true);
    if (this.semanticSelectedId === undefined
      || !enabled.some((item) => item.id === this.semanticSelectedId)) {
      this.semanticSelectedId = menu.selectedId ?? enabled[0]?.id;
    }
    const projection = { ...menu, selectedId: this.semanticSelectedId };
    this.insertMenu.hidden = false;
    this.insertMenu.style.removeProperty("display");
    renderSemanticMenu(this.insertMenu, projection, this.editor.id, (action) => {
      this.onAction(action);
      this.textarea.focus();
    });
  }

  private replaceLocalRange(
    start: number,
    end: number,
    replacement: string,
    caretOffset: number,
  ): void {
    this.textarea.setRangeText(replacement, start, end, "end");
    const caret = start + caretOffset;
    this.textarea.setSelectionRange(caret, caret);
    this.keyEdited = true;
    this.textChanged();
  }

  private closeInsertMenu(): void {
    this.insertMenu.hidden = true;
    this.insertMenu.style.display = "none";
    this.insertMenu.replaceChildren();
    this.semanticSelectedId = undefined;
  }

  private moveSemanticSelection(delta: number): void {
    const menu = this.editor?.insertMenu;
    if (menu === undefined) return;
    const enabled = menu.items.filter((item) => item.disabled !== true);
    if (enabled.length === 0) return;
    const current = Math.max(0, enabled.findIndex((item) => item.id === this.semanticSelectedId));
    this.semanticSelectedId = enabled[(current + delta + enabled.length) % enabled.length]!.id;
    this.refreshInsertMenu();
  }

  private activateSemanticItem(menu: MenuSpec): void {
    const item = menu.items.find((candidate) => candidate.id === this.semanticSelectedId);
    if (item === undefined || item.disabled === true) return;
    this.onAction(uiAction(item.id, item.action, "activate"));
  }

  private showContextMenu(event: MouseEvent, menu: MenuSpec): void {
    this.contextMenu.hidden = false;
    this.contextMenu.style.insetInlineStart = `${event.offsetX}px`;
    this.contextMenu.style.insetBlockStart = `${event.offsetY}px`;
    renderSemanticMenu(this.contextMenu, menu, this.editor?.id ?? "markdown-editor", (action) => {
      this.onAction(action);
      this.contextMenu.hidden = true;
      this.textarea.focus();
    });
    this.contextMenu.focus();
  }

  private renderToolbar(editor: MarkdownEditorNode): void {
    this.toolbar.replaceChildren();
    const title = document.createElement("strong");
    title.textContent = `${editor.title ?? "Markdown"}${editor.dirty ? " •" : ""}`;
    this.toolbar.append(title);

    const presentationAction = componentAction(
      editor.actions,
      "setPresentation",
      "set-presentation",
    );
    if (presentationAction) {
      const picker = document.createElement("select");
      picker.setAttribute("aria-label", "Presentation");
      for (const presentation of ["source", "preview", "split"] as const) {
        const option = document.createElement("option");
        option.value = presentation;
        option.textContent = presentation[0]!.toUpperCase() + presentation.slice(1);
        option.selected = presentation === (editor.presentation ?? "source");
        picker.append(option);
      }
      picker.addEventListener("change", () => {
        if (!this.snapshot) return;
        this.onAction(uiAction(
          editor.id,
          presentationAction,
          "change",
          { type: "text", value: picker.value },
        ));
      });
      this.toolbar.append(picker);
    }

    const openMenuAction = editor.actions?.openMenu;
    if (openMenuAction) {
      const commands = document.createElement("button");
      commands.type = "button";
      commands.textContent = "Commands";
      commands.addEventListener("click", () => {
        this.onAction(uiAction(
          editor.id,
          openMenuAction,
          "command",
          { type: "text", value: "palette" },
        ));
      });
      this.toolbar.append(commands);
    }

    const saveAction = componentAction(editor.actions, "save", "save");
    if (saveAction && !editor.readOnly) {
      const save = document.createElement("button");
      save.type = "button";
      save.textContent = "Save";
      save.addEventListener("click", () => {
        if (!this.snapshot) return;
        this.onAction(uiAction(
          editor.id,
          saveAction,
          "command",
        ));
      });
      this.toolbar.append(save);
    }
  }

  private renderPreview(source: string): void {
    this.preview.replaceChildren();
    const rendered = this.options.renderMarkdown?.(source) ?? source;
    if (typeof rendered === "string") {
      this.preview.textContent = rendered;
    } else {
      this.preview.append(rendered);
    }
  }

  private renderCommandHint(): void {
    const editor = this.editor;
    if (editor === undefined
      || !isMarkdownCommandHintVisible(editor)
      || editor.commandHint === undefined
      || this.textarea.hidden) {
      this.commandHint.hidden = true;
      this.commandHint.replaceChildren();
      return;
    }
    const offset = utf16OffsetAtPosition(editor.text, editor.selection.head);
    if (offset === undefined) {
      this.commandHint.hidden = true;
      return;
    }
    const position = textareaCaretPosition(this.textarea, offset);
    this.commandHint.textContent = editor.commandHint.text;
    this.commandHint.style.insetInlineStart = `${position.x}px`;
    this.commandHint.style.insetBlockStart = `${position.y}px`;
    this.commandHint.style.font = getComputedStyle(this.textarea).font;
    this.commandHint.hidden = false;
  }

  private applyPresentation(presentation: MarkdownPresentation): void {
    this.element.dataset.presentation = presentation;
    this.textarea.hidden = presentation === "preview";
    this.preview.hidden = presentation === "source";
    this.body.style.display = presentation === "split" ? "grid" : "block";
    this.body.style.gridTemplateColumns = presentation === "split" ? "1fr 1fr" : "";
  }
}

function textareaCaretPosition(
  textarea: HTMLTextAreaElement,
  utf16Offset: number,
): { x: number; y: number } {
  const style = getComputedStyle(textarea);
  const mirror = document.createElement("div");
  mirror.style.position = "fixed";
  mirror.style.visibility = "hidden";
  mirror.style.pointerEvents = "none";
  mirror.style.insetInlineStart = "-100000px";
  mirror.style.insetBlockStart = "0";
  mirror.style.boxSizing = style.boxSizing;
  mirror.style.width = `${textarea.clientWidth}px`;
  mirror.style.padding = style.padding;
  mirror.style.border = style.border;
  mirror.style.font = style.font;
  mirror.style.letterSpacing = style.letterSpacing;
  mirror.style.lineHeight = style.lineHeight;
  mirror.style.tabSize = style.tabSize;
  mirror.style.whiteSpace = "pre-wrap";
  mirror.style.overflowWrap = "break-word";
  mirror.textContent = textarea.value.slice(0, utf16Offset);
  const marker = document.createElement("span");
  marker.textContent = "\u200b";
  mirror.append(marker);
  document.body.append(mirror);
  const mirrorRect = mirror.getBoundingClientRect();
  const markerRect = marker.getBoundingClientRect();
  const position = {
    x: textarea.offsetLeft + markerRect.left - mirrorRect.left - textarea.scrollLeft,
    y: textarea.offsetTop + markerRect.top - mirrorRect.top - textarea.scrollTop,
  };
  mirror.remove();
  return position;
}

interface MarkdownBackspaceEdit {
  lineStart: number;
  lineEnd: number;
  replacement: string;
  caretOffset: number;
}

function markdownBackspaceEdit(
  text: string,
  selectionStart: number,
  selectionEnd: number,
): MarkdownBackspaceEdit | undefined {
  if (selectionStart !== selectionEnd) return undefined;
  const lineStart = text.lastIndexOf("\n", Math.max(0, selectionStart - 1)) + 1;
  const newline = text.indexOf("\n", selectionStart);
  const lineEnd = newline === -1 ? text.length : newline;
  const line = text.slice(lineStart, lineEnd);
  const indent = line.match(/^[\t ]*/u)?.[0] ?? "";
  const rest = line.slice(indent.length);
  const marker = ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ ", "> "]
    .find((candidate) => rest.startsWith(candidate))
    ?? rest.match(/^#{1,6}\s/u)?.[0]
    ?? rest.match(/^\d+\. /u)?.[0];
  if (!marker) return undefined;
  const column = selectionStart - lineStart;
  const prefixLength = indent.length + marker.length;
  if (column <= indent.length || column > prefixLength) return undefined;
  return {
    lineStart,
    lineEnd,
    replacement: indent + rest.slice(marker.length),
    caretOffset: indent.length,
  };
}

export function diffText(before: string, after: string): TextEdit | undefined {
  if (before === after) return undefined;
  const beforePoints = Array.from(before);
  const afterPoints = Array.from(after);
  let prefix = 0;
  while (
    prefix < beforePoints.length
    && prefix < afterPoints.length
    && beforePoints[prefix] === afterPoints[prefix]
  ) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < beforePoints.length - prefix
    && suffix < afterPoints.length - prefix
    && beforePoints[beforePoints.length - suffix - 1]
      === afterPoints[afterPoints.length - suffix - 1]
  ) {
    suffix += 1;
  }
  const startOffset = beforePoints.slice(0, prefix).join("").length;
  const removedEnd = beforePoints.slice(0, beforePoints.length - suffix).join("").length;
  const replacement = afterPoints.slice(prefix, afterPoints.length - suffix).join("");
  return {
    range: {
      start: positionAtUtf16Offset(before, startOffset),
      end: positionAtUtf16Offset(before, removedEnd),
    },
    text: replacement,
  };
}

export function positionAtUtf16Offset(text: string, target: number): TextPosition {
  const clamped = Math.max(0, Math.min(target, text.length));
  const before = text.slice(0, clamped);
  const lines = before.split("\n");
  return {
    line: lines.length - 1,
    utf16Column: lines.at(-1)?.length ?? 0,
  };
}

export function utf16OffsetAtPosition(
  text: string,
  position: TextPosition,
): number | undefined {
  if (position.line < 0 || position.utf16Column < 0) return undefined;
  const lines = text.split("\n");
  const line = lines[position.line];
  if (line === undefined || position.utf16Column > line.length) return undefined;
  let offset = position.utf16Column;
  for (let index = 0; index < position.line; index += 1) {
    offset += lines[index]!.length + 1;
  }
  return offset;
}

function selectionOffsets(
  text: string,
  selection: TextSelection,
): {
  start: number;
  end: number;
  direction: "forward" | "backward";
} | undefined {
  const anchor = utf16OffsetAtPosition(text, selection.anchor);
  const head = utf16OffsetAtPosition(text, selection.head);
  if (anchor === undefined || head === undefined) return undefined;
  return {
    start: Math.min(anchor, head),
    end: Math.max(anchor, head),
    direction: anchor > head ? "backward" : "forward",
  };
}

function textSelectionsEqual(left: TextSelection, right: TextSelection): boolean {
  return left.anchor.line === right.anchor.line
    && left.anchor.utf16Column === right.anchor.utf16Column
    && left.head.line === right.head.line
    && left.head.utf16Column === right.head.utf16Column;
}

function textareaSelectionMatches(
  textarea: HTMLTextAreaElement,
  expected: NonNullable<ReturnType<typeof selectionOffsets>>,
): boolean {
  return textarea.selectionStart === expected.start
    && textarea.selectionEnd === expected.end
    && (
      expected.start === expected.end
      || textarea.selectionDirection === expected.direction
    );
}

function componentAction<K extends keyof NonNullable<MarkdownEditorNode["actions"]>>(
  actions: MarkdownEditorNode["actions"],
  key: K,
  defaultAction: string,
): string | undefined {
  return actions === undefined ? defaultAction : actions[key];
}
