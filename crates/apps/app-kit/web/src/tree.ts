import {
  type FooterActionsSpec,
  type TreeItem,
  type TreeNode,
  type UiAction,
  type UiSnapshot,
  isTreeNode,
  uiAction,
} from "./protocol";
import { renderSemanticMenu } from "./menu";
import { handleFooterAccelerator, renderFooterActions } from "./footer";

interface VisibleItem {
  item: TreeItem;
  depth: number;
}

/** Accessible DOM interpretation of the closed Tree/Explorer component. */
export class TreeRenderer {
  readonly element: HTMLElement;

  private readonly onAction: (action: UiAction) => void;
  private selectedId: string | undefined;
  private filterDraft = "";
  private clickTimer: ReturnType<typeof setTimeout> | undefined;
  private footer: FooterActionsSpec | undefined;

  constructor(container: HTMLElement, onAction: (action: UiAction) => void) {
    this.onAction = onAction;
    this.element = document.createElement("section");
    this.element.className = "unpeel-tree";
    this.element.addEventListener("keydown", (event) => {
      handleFooterAccelerator(event, this.footer, this.onAction);
    });
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isTreeNode(snapshot.root)) {
      throw new Error(`TreeRenderer cannot render ${snapshot.root.type}`);
    }
    this.renderTree(snapshot.root);
  }

  destroy(): void {
    if (this.clickTimer !== undefined) clearTimeout(this.clickTimer);
    this.footer = undefined;
    this.element.remove();
  }

  private renderTree(tree: TreeNode): void {
    const focused = document.activeElement instanceof HTMLElement
      ? document.activeElement.id
      : "";
    this.element.replaceChildren();
    this.footer = tree.footer;
    this.selectedId = tree.selectedId ?? this.selectedId;

    let filterInput: HTMLInputElement | undefined;
    if (tree.filter !== undefined) {
      const label = document.createElement("label");
      label.className = "unpeel-tree__filter";
      label.textContent = tree.filter.label;
      filterInput = document.createElement("input");
      filterInput.id = tree.filter.id;
      filterInput.type = "search";
      filterInput.placeholder = tree.filter.placeholder ?? "";
      if (this.filterDraft === "" || tree.filter.value !== undefined) {
        this.filterDraft = tree.filter.value ?? "";
      }
      filterInput.value = this.filterDraft;
      filterInput.addEventListener("input", () => {
        this.filterDraft = filterInput!.value;
        this.onAction(uiAction(
          tree.filter!.id,
          tree.filter!.setValue,
          "change",
          { type: "text", value: filterInput!.value },
        ));
      });
      label.append(filterInput);
      this.element.append(label);
    }

    const location = document.createElement("div");
    location.className = "unpeel-tree__location";
    location.textContent = tree.location;
    this.element.append(location);

    if (tree.primaryAction !== undefined) {
      const action = document.createElement("button");
      action.type = "button";
      action.className = `unpeel-tree__primary-action unpeel-tree__primary-action--${tree.primaryAction.role ?? "default"}`;
      action.textContent = tree.primaryAction.label;
      action.addEventListener("click", () => {
        this.onAction(uiAction(tree.primaryAction!.id, tree.primaryAction!.action, "activate"));
      });
      this.element.append(action);
    }

    const rows = this.visibleItems(tree.items, tree.presentation ?? "drillDown");
    if (rows.length === 0) {
      const empty = document.createElement("p");
      empty.className = "unpeel-tree__empty";
      empty.textContent = tree.emptyMessage ?? "No items";
      this.element.append(empty);
      this.finishRender(tree, focused);
      return;
    }

    const list = document.createElement("ul");
    list.id = `unpeel-tree-${tree.id}`;
    list.className = "unpeel-tree__items";
    list.setAttribute("role", "tree");
    list.setAttribute("aria-label", tree.label);
    list.tabIndex = 0;
    for (const row of rows) list.append(this.item(tree, row));
    list.addEventListener("keydown", (event) => {
      this.handleKey(event, tree, rows, filterInput);
    });
    this.element.append(list);

    filterInput?.addEventListener("keydown", (event) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key === "ArrowDown" || event.key === "Tab") {
        list.focus();
        event.preventDefault();
      } else if (event.key === "Escape") {
        this.onAction(uiAction(tree.id, tree.actions.parent, "cancel"));
        event.preventDefault();
      }
    });

    this.finishRender(tree, focused);
  }

  private finishRender(tree: TreeNode, focused: string): void {
    const footer = document.createElement("footer");
    footer.className = "unpeel-footer-actions";
    renderFooterActions(footer, tree.footer, this.onAction);
    if (!footer.hidden) this.element.append(footer);
    if (focused !== "") document.getElementById(focused)?.focus();
  }

  private item(tree: TreeNode, row: VisibleItem): HTMLLIElement {
    const item = document.createElement("li");
    item.id = `unpeel-tree-item-${row.item.id}`;
    item.className = `unpeel-tree__item unpeel-tree__item--${row.item.kind}`;
    item.dataset.itemId = row.item.id;
    item.setAttribute("role", "treeitem");
    item.setAttribute("aria-level", String(row.depth + 1));
    item.setAttribute("aria-selected", String(this.selectedId === row.item.id));
    item.style.paddingInlineStart = `${12 + row.depth * 18}px`;
    if (row.item.kind === "directory" && (tree.presentation ?? "drillDown") === "outline") {
      item.setAttribute("aria-expanded", String(row.item.expanded === true));
      const disclosure = document.createElement("button");
      disclosure.type = "button";
      disclosure.className = "unpeel-tree__disclosure";
      disclosure.textContent = row.item.expanded === true ? "▾" : "▸";
      disclosure.setAttribute("aria-label", row.item.expanded === true ? "Collapse" : "Expand");
      disclosure.addEventListener("click", (event) => {
        event.stopPropagation();
        this.setExpanded(tree, row.item, row.item.expanded !== true);
      });
      item.append(disclosure);
    }
    const icon = document.createElement("span");
    icon.className = "unpeel-tree__icon";
    icon.setAttribute("aria-hidden", "true");
    icon.textContent = row.item.kind === "parent" ? "↰" : row.item.kind === "directory" ? "▸" : "";
    const label = document.createElement("span");
    label.textContent = row.item.kind === "parent" ? ".." : row.item.label;
    item.append(icon, label);
    if (row.item.childState === "loading") {
      item.setAttribute("aria-busy", "true");
      const busy = document.createElement("span");
      busy.className = "unpeel-tree__busy";
      busy.textContent = "…";
      item.append(busy);
    }
    item.addEventListener("click", () => {
      if (this.clickTimer !== undefined) clearTimeout(this.clickTimer);
      this.clickTimer = setTimeout(() => {
        this.select(tree, row.item.id);
        this.clickTimer = undefined;
      }, 180);
    });
    item.addEventListener("dblclick", () => {
      if (this.clickTimer !== undefined) clearTimeout(this.clickTimer);
      this.clickTimer = undefined;
      this.activate(tree, row.item);
    });
    if (tree.contextMenu !== undefined) {
      item.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        this.selectLocally(row.item.id);
        const host = document.createElement("div");
        host.className = "unpeel-context-menu";
        host.style.position = "fixed";
        host.style.left = `${event.clientX}px`;
        host.style.top = `${event.clientY}px`;
        host.style.zIndex = "50";
        renderSemanticMenu(host, tree.contextMenu!, row.item.id, (action) => {
          this.onAction({ ...action, value: { type: "text", value: row.item.id } });
          host.remove();
        });
        document.body.append(host);
        host.focus();
      });
    }
    return item;
  }

  private visibleItems(
    items: readonly TreeItem[],
    presentation: "drillDown" | "outline",
    depth = 0,
  ): VisibleItem[] {
    return items.flatMap((item) => {
      const rows: VisibleItem[] = [{ item, depth }];
      if (presentation === "outline" && item.expanded === true) {
        rows.push(...this.visibleItems(item.children ?? [], presentation, depth + 1));
      }
      return rows;
    });
  }

  private select(tree: TreeNode, id: string): void {
    if (this.selectedId === id) return;
    this.selectLocally(id);
    this.onAction(uiAction(tree.id, tree.actions.select, "select", { type: "text", value: id }));
  }

  private selectLocally(id: string): void {
    this.selectedId = id;
    for (const row of this.element.querySelectorAll<HTMLElement>("[role=treeitem]")) {
      row.setAttribute("aria-selected", String(row.dataset.itemId === id));
    }
  }

  private activate(tree: TreeNode, item: TreeItem): void {
    this.selectLocally(item.id);
    if (item.kind === "parent") {
      this.onAction(uiAction(tree.id, tree.actions.parent, "cancel"));
    } else {
      this.onAction(uiAction(
        tree.id,
        tree.actions.open,
        "activate",
        { type: "text", value: item.id },
      ));
    }
  }

  private setExpanded(tree: TreeNode, item: TreeItem, expanded: boolean): void {
    if (tree.actions.setExpanded === undefined) {
      this.activate(tree, item);
      return;
    }
    this.onAction(uiAction(
      tree.id,
      tree.actions.setExpanded,
      "change",
      { type: "textList", value: [item.id, String(expanded)] },
    ));
  }

  private handleKey(
    event: KeyboardEvent,
    tree: TreeNode,
    rows: readonly VisibleItem[],
    filter: HTMLInputElement | undefined,
  ): void {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const current = Math.max(0, rows.findIndex((row) => row.item.id === this.selectedId));
    let target: number | undefined;
    switch (event.key) {
      case "ArrowDown": target = (current + 1) % rows.length; break;
      case "ArrowUp":
        if (current === 0 && filter !== undefined) {
          filter.focus();
          event.preventDefault();
          return;
        }
        target = (current - 1 + rows.length) % rows.length;
        break;
      case "Home": target = 0; break;
      case "End": target = rows.length - 1; break;
      case "PageDown": target = Math.min(current + 10, rows.length - 1); break;
      case "PageUp": target = Math.max(current - 10, 0); break;
      case " ": target = Math.min(current + 10, rows.length - 1); break;
      case "Enter":
        this.activate(tree, rows[current]!.item);
        event.preventDefault();
        return;
      case "ArrowRight": {
        const item = rows[current]!.item;
        if (item.kind === "directory" && (tree.presentation ?? "drillDown") === "outline"
          && item.expanded !== true) this.setExpanded(tree, item, true);
        else this.activate(tree, item);
        event.preventDefault();
        return;
      }
      case "ArrowLeft":
      case "Escape":
      case "Backspace":
        this.onAction(uiAction(tree.id, tree.actions.parent, "cancel"));
        event.preventDefault();
        return;
      case "Tab":
      case "/":
        if (filter !== undefined) {
          filter.focus();
          event.preventDefault();
        }
        return;
      default:
        if (event.key.length === 1 && filter !== undefined) {
          filter.focus();
          filter.value += event.key;
          filter.dispatchEvent(new Event("input", { bubbles: true }));
          event.preventDefault();
        }
        return;
    }
    if (target !== undefined) {
      this.select(tree, rows[target]!.item.id);
      document.getElementById(`unpeel-tree-item-${rows[target]!.item.id}`)
        ?.scrollIntoView({ block: "nearest" });
      event.preventDefault();
    }
  }
}
