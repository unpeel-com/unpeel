import {
  type FooterActionSpec,
  type FooterActionsSpec,
  type UiAction,
  uiAction,
} from "./protocol";

/** Renders the ordered Rust-owned action slot as native web buttons. */
export function renderFooterActions(
  container: HTMLElement,
  footer: FooterActionsSpec | undefined,
  onAction: (action: UiAction) => void,
): void {
  container.replaceChildren();
  container.hidden = (footer?.actions.length ?? 0) === 0;
  if (footer === undefined) return;
  for (const action of footer.actions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "unpeel-footer-actions__button";
    button.dataset.role = action.role ?? "default";
    button.disabled = action.disabled === true;
    if (action.accelerator !== undefined) {
      const accelerator = document.createElement("kbd");
      accelerator.textContent = acceleratorLabel(action.accelerator);
      button.append(accelerator);
    }
    const label = document.createElement("span");
    label.textContent = action.label;
    button.append(label);
    button.addEventListener("click", () => dispatchFooterAction(action, onAction));
    container.append(button);
  }
}

/** Resolves the same closed accelerator grammar used by the Ratatui view. */
export function handleFooterAccelerator(
  event: KeyboardEvent,
  footer: FooterActionsSpec | undefined,
  onAction: (action: UiAction) => void,
): boolean {
  if (event.defaultPrevented || footer === undefined || event.repeat) return false;
  const action = footer.actions.find((candidate) => (
    candidate.disabled !== true
      && candidate.accelerator !== undefined
      && matchesAccelerator(event, candidate.accelerator)
  ));
  if (action === undefined) return false;
  event.preventDefault();
  dispatchFooterAction(action, onAction);
  return true;
}

function dispatchFooterAction(
  action: FooterActionSpec,
  onAction: (action: UiAction) => void,
): void {
  onAction(uiAction(action.id, action.action, "activate"));
}

function matchesAccelerator(event: KeyboardEvent, accelerator: string): boolean {
  const ctrlKey = accelerator.startsWith("ctrl+");
  if (ctrlKey) {
    return event.ctrlKey && !event.altKey && !event.metaKey
      && event.key.toLocaleLowerCase() === accelerator.slice(5).toLocaleLowerCase();
  }
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  if (isEditableTarget(event.target) && accelerator.length === 1) return false;
  switch (accelerator) {
    case "escape": return event.key === "Escape";
    case "enter": return event.key === "Enter";
    case "space": return event.key === " ";
    default: return event.key === accelerator;
  }
}

function isEditableTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLInputElement
    || target instanceof HTMLTextAreaElement
    || target instanceof HTMLSelectElement
    || (target instanceof HTMLElement && target.isContentEditable);
}

function acceleratorLabel(accelerator: string): string {
  if (accelerator.startsWith("ctrl+")) return `Ctrl+${accelerator.slice(5).toUpperCase()}`;
  if (accelerator === "escape") return "Esc";
  if (accelerator === "enter") return "Enter";
  if (accelerator === "space") return "Space";
  return accelerator;
}
