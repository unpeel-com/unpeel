import {
  type MenuSpec,
  type UiAction,
  type UiSnapshot,
  isMenuNode,
  uiAction,
} from "./protocol";

/** DOM interpretation of the closed Menu component. */
export class MenuRenderer {
  readonly element: HTMLElement;
  private readonly onAction: (action: UiAction) => void;

  constructor(container: HTMLElement, onAction: (action: UiAction) => void) {
    this.onAction = onAction;
    this.element = document.createElement("section");
    this.element.className = "unpeel-menu-host";
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isMenuNode(snapshot.root)) {
      throw new Error(`MenuRenderer cannot render ${snapshot.root.type}`);
    }
    renderSemanticMenu(this.element, snapshot.root, snapshot.root.id, this.onAction);
  }

  destroy(): void {
    this.element.remove();
  }
}

/** Reusable nested interpretation for Markdown caret/context menus. */
export function renderSemanticMenu(
  container: HTMLElement,
  menu: MenuSpec,
  ownerId: string,
  onAction: (action: UiAction) => void,
): void {
  container.replaceChildren();
  container.classList.add("unpeel-menu");
  container.dataset.presentation = menu.presentation ?? "popup";
  container.dataset.anchor = menu.anchor ?? "control";
  container.setAttribute("role", "menu");
  container.setAttribute("aria-label", menu.label);
  container.tabIndex = 0;

  const enabled = menu.items.filter((item) => item.disabled !== true);
  let selectedId = menu.selectedId ?? enabled[0]?.id;

  const select = (id: string): void => {
    selectedId = id;
    for (const row of container.querySelectorAll<HTMLButtonElement>("[role=menuitem]")) {
      row.setAttribute("aria-selected", String(row.dataset.itemId === id));
    }
  };
  const activate = (): void => {
    const item = menu.items.find((candidate) => candidate.id === selectedId);
    if (item === undefined || item.disabled === true) return;
    onAction(uiAction(item.id, item.action, "activate"));
  };

  for (const item of menu.items) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = `unpeel-menu__item unpeel-menu__item--${item.role ?? "default"}`;
    row.dataset.itemId = item.id;
    row.setAttribute("role", "menuitem");
    row.setAttribute("aria-selected", String(item.id === selectedId));
    row.disabled = item.disabled === true;
    if (item.hint !== undefined) {
      const hint = document.createElement("code");
      hint.className = "unpeel-menu__hint";
      hint.textContent = item.hint;
      row.append(hint);
    }
    const label = document.createElement("span");
    label.textContent = item.label;
    row.append(label);
    row.addEventListener("pointerenter", () => {
      if (!row.disabled) select(item.id);
    });
    row.addEventListener("mousedown", (event) => event.preventDefault());
    row.addEventListener("click", () => {
      select(item.id);
      activate();
    });
    container.append(row);
  }

  container.onkeydown = (event): void => {
    const current = Math.max(0, enabled.findIndex((item) => item.id === selectedId));
    let index: number | undefined;
    switch (event.key) {
      case "ArrowUp": index = (current - 1 + enabled.length) % enabled.length; break;
      case "ArrowDown": index = (current + 1) % enabled.length; break;
      case "Home": index = 0; break;
      case "End": index = enabled.length - 1; break;
      case "Enter":
      case " ": activate(); break;
      case "Escape":
        if (menu.dismiss !== undefined) onAction(uiAction(ownerId, menu.dismiss, "cancel"));
        break;
      default: return;
    }
    if (index !== undefined && enabled[index] !== undefined) select(enabled[index]!.id);
    event.preventDefault();
  };
}
