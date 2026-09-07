import {
  isTextBoxNode,
  uiAction,
  type TextBoxNode,
  type TextBoxTitlePosition,
  type UiAction,
  type UiSnapshot,
} from "./protocol";

const TITLE_CLASSES: Record<TextBoxTitlePosition, string> = {
  topLeft: "unpeel-text-box__title--top-left",
  topRight: "unpeel-text-box__title--top-right",
  bottomLeft: "unpeel-text-box__title--bottom-left",
  bottomRight: "unpeel-text-box__title--bottom-right",
};

/**
 * Web interpretation of the closed `textBox` root component.
 *
 * The textarea keeps a local draft across unrelated redraws; a changed
 * server text replaces it. `set-text` is sent on `change` (blur) and
 * `submit` on Enter (submitMode `enter`) or the submit button.
 */
export class TextBoxRenderer {
  readonly element: HTMLElement;
  private readonly onAction: (action: UiAction) => void;
  private readonly textarea: HTMLTextAreaElement;
  private serverText: string | undefined;

  constructor(container: HTMLElement, onAction: (action: UiAction) => void) {
    this.onAction = onAction;
    this.element = document.createElement("section");
    this.element.className = "unpeel-text-box-host";
    this.textarea = document.createElement("textarea");
    this.textarea.className = "unpeel-text-box__field";
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isTextBoxNode(snapshot.root)) {
      throw new Error(`TextBoxRenderer cannot render ${snapshot.root.type}`);
    }
    this.renderTextBox(snapshot.root);
  }

  destroy(): void {
    this.element.remove();
  }

  private renderTextBox(node: TextBoxNode): void {
    const hadFocus = document.activeElement === this.textarea;
    this.element.replaceChildren();
    const serverText = node.text ?? "";
    if (this.serverText !== serverText) {
      this.serverText = serverText;
      this.textarea.value = serverText;
    }
    this.textarea.id = node.id;
    this.textarea.placeholder = node.placeholder ?? "";
    this.textarea.setAttribute("aria-label", node.placeholder ?? "Text");
    const minRows = node.minRows ?? 3;
    const maxRows = node.maxRows ?? Math.max(minRows, 10);
    this.textarea.rows = minRows;
    this.textarea.style.setProperty("--unpeel-text-box-max-rows", String(maxRows));
    this.textarea.onchange = () => {
      if (node.actions?.setText === undefined || this.textarea.value === this.serverText) return;
      this.onAction(uiAction(node.id, node.actions.setText, "change", {
        type: "text",
        value: this.textarea.value,
      }));
    };
    this.textarea.onkeydown = (event) => {
      if (event.key !== "Enter" || event.isComposing) return;
      const newline = event.shiftKey || event.altKey;
      if ((node.submitMode ?? "enter") === "enter" && !newline) {
        event.preventDefault();
        this.submit(node);
      }
    };

    if (node.busy !== undefined) {
      const status = document.createElement("div");
      status.className = "unpeel-text-box__status";
      status.setAttribute("role", "status");
      const spinner = document.createElement("span");
      spinner.className = "unpeel-text-box__spinner";
      spinner.setAttribute("aria-hidden", "true");
      const label = document.createElement("span");
      label.className = "unpeel-text-box__status-label";
      const elapsed = ((node.busy.elapsedMs ?? 0) / 1000).toFixed(1);
      label.textContent = `${node.busy.label} ${elapsed}s`;
      status.append(spinner, label);
      if (node.busy.rightMeta !== undefined && node.busy.rightMeta !== "") {
        const meta = document.createElement("span");
        meta.className = "unpeel-text-box__status-meta";
        meta.textContent = node.busy.rightMeta;
        status.append(meta);
      }
      this.element.append(status);
    }

    const box = document.createElement("div");
    box.className = "unpeel-text-box";
    if (node.prompt !== undefined && node.prompt !== "") {
      const prompt = document.createElement("span");
      prompt.className = "unpeel-text-box__prompt";
      prompt.textContent = node.prompt;
      prompt.setAttribute("aria-hidden", "true");
      box.append(prompt);
    }
    box.append(this.textarea);
    for (const title of node.titles ?? []) {
      const element = document.createElement("span");
      element.className = `unpeel-text-box__title ${TITLE_CLASSES[title.position]}`;
      element.textContent = title.text;
      box.append(element);
    }
    this.element.append(box);

    if (node.hints !== undefined && node.hints.length > 0) {
      const footer = document.createElement("div");
      footer.className = "unpeel-text-box__hints";
      node.hints.forEach((hint, index) => {
        if (index > 0) {
          const separator = document.createElement("span");
          separator.className = "unpeel-text-box__hint-separator";
          separator.textContent = "│";
          footer.append(separator);
        }
        const key = document.createElement("kbd");
        key.textContent = hint.key;
        const label = document.createElement("span");
        label.className = "unpeel-text-box__hint-label";
        label.textContent = `:${hint.label}`;
        footer.append(key, label);
      });
      this.element.append(footer);
    }
    if (hadFocus) this.textarea.focus();
  }

  private submit(node: TextBoxNode): void {
    if (node.actions?.submit === undefined || this.textarea.value.trim() === "") return;
    this.onAction(uiAction(node.id, node.actions.submit, "submit", {
      type: "text",
      value: this.textarea.value,
    }));
    this.textarea.value = "";
  }
}
