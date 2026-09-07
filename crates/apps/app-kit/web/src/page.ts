import {
  type BarChartSpec,
  type BadgeSpec,
  type CheckmarkSpec,
  type ContentLine,
  type ContentSelection,
  type ContentSpec,
  type GaugeSpec,
  type FooterActionsSpec,
  type InputSpec,
  type LineChartSpec,
  type ListItemSlot,
  type ListItemSpec,
  type ListItemTextRun,
  type ListSpec,
  type PageNode,
  type SparklineSpec,
  type StatusSymbolSpec,
  type ToggleSpec,
  type UiAction,
  type UiSnapshot,
  isBadgeSlot,
  isBarChartSpec,
  isCheckmarkSlot,
  isContentSpec,
  isGaugeSpec,
  isGaugeSlot,
  isLineChartSpec,
  isListSpec,
  isDisclosureSlot,
  isRenderablePageNode,
  isRenderableContentPageNode,
  isRenderableChartPageNode,
  isSparklineBodySpec,
  isSparklineSlot,
  isStatusSlot,
  isToggleSlot,
  gaugeValueLabel,
  listItemPrimaryRole,
  normalizedBarChartValues,
  normalizedSparklineSeries,
  resolvedLineChartBounds,
  uiAction,
} from "./protocol";
import { listNavigationDecision } from "./list_navigation";
import { renderSemanticMenu } from "./menu";
import { handleFooterAccelerator, renderFooterActions } from "./footer";

/** Native DOM interpretation of Page, List, ListItem, Toggle, and Input. */
export class PageRenderer {
  readonly element: HTMLElement;

  private readonly onAction: (action: UiAction) => void;
  private readonly drafts = new Map<string, string>();
  private readonly serverInputValues = new Map<string, string>();
  private readonly selections = new Map<string, string | undefined>();
  private readonly serverSelections = new Map<string, string | undefined>();
  private readonly resizeObservers: ResizeObserver[] = [];
  private contentSelection: ContentSelection | undefined;
  private contentAnchor: string | undefined;
  private footer: FooterActionsSpec | undefined;

  constructor(container: HTMLElement, onAction: (action: UiAction) => void) {
    this.onAction = onAction;
    this.element = document.createElement("section");
    this.element.className = "unpeel-page";
    this.element.addEventListener("keydown", (event) => {
      handleFooterAccelerator(event, this.footer, this.onAction);
    });
    container.replaceChildren(this.element);
  }

  render(snapshot: UiSnapshot): void {
    if (!isRenderablePageNode(snapshot.root) && !isRenderableContentPageNode(snapshot.root)
      && !isRenderableChartPageNode(snapshot.root)) {
      throw new Error(`PageRenderer cannot render ${snapshot.root.type}`);
    }
    this.renderPage(snapshot.root);
  }

  destroy(): void {
    this.disconnectResizeObservers();
    this.drafts.clear();
    this.serverInputValues.clear();
    this.selections.clear();
    this.serverSelections.clear();
    this.footer = undefined;
    this.element.remove();
  }

  private renderPage(page: PageNode & {
    header?: InputSpec;
    body: ListSpec | ContentSpec | SparklineSpec | BarChartSpec | LineChartSpec | GaugeSpec;
  }): void {
    const focusedID = document.activeElement instanceof HTMLElement
      ? document.activeElement.id
      : "";
    this.disconnectResizeObservers();
    this.element.replaceChildren();
    this.footer = page.footer;
    const pageHeader = document.createElement("header");
    pageHeader.className = "unpeel-page__header";
    if (page.back !== undefined) {
      const back = document.createElement("button");
      back.type = "button";
      back.className = "unpeel-page__back";
      back.textContent = "Back";
      back.addEventListener("click", () => {
        this.onAction(uiAction(page.id, page.back!, "cancel"));
      });
      pageHeader.append(back);
    }
    const heading = document.createElement("h1");
    heading.textContent = page.title;
    pageHeader.append(heading);
    this.element.append(pageHeader);

    if (page.header !== undefined) this.element.append(this.input(page.header));

    if (isContentSpec(page.body)) {
      this.element.append(this.content(page.body));
      this.finishRender(page, focusedID);
      return;
    }

    if (isSparklineBodySpec(page.body)) {
      this.element.append(this.sparkline(page.body, "accent", false));
      this.finishRender(page, focusedID);
      return;
    }
    if (isBarChartSpec(page.body)) {
      this.element.append(this.barChart(page.body));
      this.finishRender(page, focusedID);
      return;
    }
    if (isLineChartSpec(page.body)) {
      this.element.append(this.lineChart(page.body));
      this.finishRender(page, focusedID);
      return;
    }
    if (isGaugeSpec(page.body)) {
      this.element.append(this.gauge(page.body));
      this.finishRender(page, focusedID);
      return;
    }

    const body = page.body;
    if (!isListSpec(body)) {
      this.finishRender(page, focusedID);
      return;
    }
    const list = document.createElement("ul");
    list.className = "unpeel-list";
    list.id = `unpeel-list-${body.id}`;
    list.tabIndex = 0;
    list.setAttribute("role", "listbox");
    list.setAttribute("aria-label", page.title);
    this.reconcileSelection(body);
    list.addEventListener("keydown", (event) => this.handleListKey(event, page, body, list));
    if (body.items.length === 0) {
      const empty = document.createElement("li");
      empty.className = "unpeel-list__empty";
      empty.textContent = body.emptyMessage ?? "";
      list.append(empty);
    } else {
      for (const item of body.items) list.append(this.item(item, body));
    }
    this.element.append(list);
    this.configureValueVisibility(list, body);
    this.finishRender(page, focusedID);
  }

  private finishRender(page: PageNode, focusedID: string): void {
    const footer = document.createElement("footer");
    footer.className = "unpeel-footer-actions";
    renderFooterActions(footer, page.footer, this.onAction);
    if (!footer.hidden) this.element.append(footer);
    this.restoreFocus(focusedID);
  }

  private content(content: ContentSpec): HTMLElement {
    const viewport = document.createElement("div");
    viewport.className = "unpeel-content";
    viewport.dataset.wrap = String(content.wrap ?? true);
    viewport.dataset.font = content.font ?? "body";
    viewport.tabIndex = 0;
    viewport.setAttribute("role", "document");
    viewport.setAttribute("aria-label", content.label);
    this.contentSelection = content.selection;
    if (content.lines.length === 0) {
      const empty = document.createElement("p");
      empty.className = "unpeel-content__empty";
      empty.textContent = content.emptyMessage ?? "";
      viewport.append(empty);
      return viewport;
    }
    for (const line of content.lines) viewport.append(this.contentLine(content, line));
    return viewport;
  }

  private contentLine(content: ContentSpec, line: ContentLine): HTMLElement {
    const row = document.createElement("div");
    row.className = "unpeel-content__line";
    row.dataset.id = line.id;
    row.dataset.tone = line.tone ?? "default";
    row.dataset.selected = String(this.contentLineSelected(content, line.id));
    for (const run of line.runs) {
      const span = document.createElement("span");
      span.textContent = run.text;
      span.dataset.tone = run.tone ?? "default";
      span.dataset.emphasis = run.emphasis ?? "regular";
      row.append(span);
    }
    row.addEventListener("pointerdown", () => {
      this.contentAnchor = line.id;
      this.setContentSelection(content, line.id, line.id, false);
    });
    row.addEventListener("pointerenter", (event) => {
      if ((event.buttons & 1) === 0 || this.contentAnchor === undefined) return;
      this.setContentSelection(content, this.contentAnchor, line.id, false);
    });
    row.addEventListener("pointerup", () => {
      const selection = this.contentSelection;
      if (selection !== undefined) {
        this.setContentSelection(content, selection.anchorId, selection.headId, true);
      }
      this.contentAnchor = undefined;
    });
    if (content.contextMenu !== undefined) {
      row.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        this.setContentSelection(content, line.id, line.id, false);
        this.showContextMenu(event, content.contextMenu!, line.id);
      });
    }
    return row;
  }

  private contentLineSelected(content: ContentSpec, id: string): boolean {
    const selection = this.contentSelection;
    if (selection === undefined) return false;
    const anchor = content.lines.findIndex((line) => line.id === selection.anchorId);
    const head = content.lines.findIndex((line) => line.id === selection.headId);
    const index = content.lines.findIndex((line) => line.id === id);
    return index >= Math.min(anchor, head) && index <= Math.max(anchor, head);
  }

  private setContentSelection(
    content: ContentSpec,
    anchorId: string,
    headId: string,
    publish: boolean,
  ): void {
    const next = { anchorId, headId };
    this.contentSelection = next;
    for (const row of this.element.querySelectorAll<HTMLElement>(".unpeel-content__line")) {
      row.dataset.selected = String(this.contentLineSelected(content, row.dataset.id ?? ""));
    }
    if (publish && content.select !== undefined) {
      this.onAction(uiAction(
        content.id,
        content.select,
        "select",
        { type: "textList", value: [anchorId, headId] },
      ));
    }
  }

  private showContextMenu(event: MouseEvent, menu: NonNullable<ContentSpec["contextMenu"]>, target: string): void {
    const host = document.createElement("div");
    host.className = "unpeel-context-menu";
    host.style.position = "fixed";
    host.style.left = `${event.clientX}px`;
    host.style.top = `${event.clientY}px`;
    host.style.zIndex = "50";
    renderSemanticMenu(host, menu, target, (action) => {
      this.onAction({ ...action, value: { type: "text", value: target } });
      host.remove();
    });
    document.body.append(host);
    host.focus();
    const dismiss = (pointer: PointerEvent): void => {
      if (!host.contains(pointer.target as Node)) host.remove();
      document.removeEventListener("pointerdown", dismiss, true);
    };
    queueMicrotask(() => document.addEventListener("pointerdown", dismiss, true));
  }

  private restoreFocus(focusedID: string): void {
    if (focusedID === "") return;
    const escaped = typeof CSS !== "undefined" && typeof CSS.escape === "function"
      ? CSS.escape(focusedID)
      : focusedID.replace(/[^A-Za-z0-9_-]/g, "\\$&");
    this.element.querySelector<HTMLElement>(`#${escaped}`)?.focus();
  }

  private input(input: InputSpec): HTMLFormElement {
    const form = document.createElement("form");
    form.className = "unpeel-input";
    const label = document.createElement("label");
    label.htmlFor = input.id;
    label.textContent = input.label;
    const field = document.createElement("input");
    field.id = input.id;
    field.type = "text";
    const serverValue = input.value ?? "";
    if (this.serverInputValues.get(input.id) !== serverValue || !this.drafts.has(input.id)) {
      this.drafts.set(input.id, serverValue);
    }
    this.serverInputValues.set(input.id, serverValue);
    field.value = this.drafts.get(input.id) ?? serverValue;
    field.placeholder = input.placeholder ?? "";
    field.addEventListener("input", () => this.drafts.set(input.id, field.value));
    if (input.setValue !== undefined) {
      field.addEventListener("change", () => {
        this.onAction(uiAction(
          input.id,
          input.setValue!,
          "change",
          { type: "text", value: field.value },
        ));
      });
    }
    form.append(label, field);
    if (input.submit !== undefined) {
      const add = document.createElement("button");
      add.type = "submit";
      add.textContent = "Add";
      form.append(add);
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        this.onAction(uiAction(
          input.id,
          input.submit!,
          "submit",
          { type: "text", value: field.value },
        ));
        field.value = "";
        this.drafts.set(input.id, "");
        field.focus();
      });
    }
    return form;
  }

  private item(item: ListItemSpec, list: ListSpec): HTMLLIElement {
    const row = document.createElement("li");
    row.className = "unpeel-list-item";
    row.dataset.id = item.id;
    row.dataset.done = String(item.done ?? false);
    row.dataset.role = listItemPrimaryRole(item);
    row.dataset.actionRole = item.actionRole ?? "default";
    row.dataset.selected = String(this.selections.get(list.id) === item.id);
    row.setAttribute("role", "option");
    row.setAttribute("aria-selected", row.dataset.selected);
    row.addEventListener("click", (event) => {
      const control = event.target instanceof Element
        ? event.target.closest("button, input, label")
        : null;
      if (control !== null) {
        this.selectLocally(list, item.id);
        return;
      }
      row.parentElement?.focus();
      if (listItemPrimaryRole(item) === "static") {
        this.select(list, item.id);
      } else {
        this.selectLocally(list, item.id);
        this.invokePrimary(item);
      }
    });
    if (list.contextMenu !== undefined) {
      row.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        this.selectLocally(list, item.id);
        this.showContextMenu(event, list.contextMenu!, item.id);
      });
    }
    if (item.busy === true) {
      const busy = document.createElement("span");
      busy.className = "unpeel-list-item__busy";
      busy.textContent = "◌";
      busy.setAttribute("role", "progressbar");
      busy.setAttribute("aria-label", "Loading");
      row.append(busy);
    }
    this.appendSlot(row, item.leading, item.valueTone ?? "muted");
    const labelContent = document.createElement("span");
    labelContent.className = "unpeel-list-item__content";
    const label = document.createElement("span");
    label.className = "unpeel-list-item__label";
    this.appendListItemText(
      label,
      item.label,
      item.labelRuns,
      item.labelTone ?? "default",
      item.emphasis ?? "regular",
    );
    labelContent.append(label);
    if (item.detail !== undefined) {
      const detail = document.createElement("span");
      detail.className = "unpeel-list-item__detail";
      this.appendListItemText(detail, item.detail, item.detailRuns, "muted", "regular");
      labelContent.append(detail);
    }
    row.append(labelContent);
    if (item.value !== undefined) {
      const value = document.createElement("span");
      value.className = "unpeel-list-item__value";
      this.appendListItemText(
        value,
        item.value,
        item.valueRuns,
        item.valueTone ?? "muted",
        "regular",
      );
      const minimum = item.valueMinWidth
        ?? Math.min(item.value.length + 11, 65_535);
      value.dataset.minColumns = String(minimum);
      row.append(value);
    }
    this.appendSlot(
      row,
      item.trailing,
      item.valueTone ?? "muted",
      item.value === undefined,
    );
    this.appendSlot(row, item.accessory, item.valueTone ?? "muted");
    if (item.delete !== undefined) {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "unpeel-list-item__delete";
      remove.textContent = "Delete";
      remove.setAttribute("aria-label", `Delete ${item.label}`);
      remove.addEventListener("click", () => {
        this.onAction(uiAction(item.id, item.delete!, "change"));
      });
      row.append(remove);
    }
    return row;
  }

  private appendSlot(
    row: HTMLElement,
    slot: ListItemSlot | undefined,
    valueTone: ListItemSpec["valueTone"],
    showGaugeCaption = true,
  ): void {
    if (slot === undefined) return;
    if (isToggleSlot(slot)) row.append(this.toggle(slot));
    else if (isStatusSlot(slot)) row.append(this.status(slot));
    else if (isBadgeSlot(slot)) row.append(this.badge(slot));
    else if (isSparklineSlot(slot)) row.append(this.sparkline(slot, valueTone ?? "muted"));
    else if (isGaugeSlot(slot)) {
      row.append(this.compactGauge(slot, valueTone ?? "muted", showGaugeCaption));
    }
    else if (isDisclosureSlot(slot)) row.append(this.disclosure());
    else if (isCheckmarkSlot(slot)) row.append(this.checkmark(slot));
  }

  private appendListItemText(
    element: HTMLElement,
    fallback: string,
    runs: ListItemTextRun[] | undefined,
    tone: NonNullable<ListItemSpec["labelTone"]>,
    emphasis: NonNullable<ListItemSpec["emphasis"]>,
  ): void {
    element.dataset.tone = tone;
    element.dataset.emphasis = emphasis;
    if (runs === undefined || runs.length === 0) {
      element.textContent = fallback;
      return;
    }
    for (const run of runs) {
      const span = document.createElement("span");
      span.className = "unpeel-list-item__text-run";
      span.textContent = run.text;
      if (run.tone !== undefined) span.dataset.tone = run.tone;
      if (run.emphasis !== undefined) span.dataset.emphasis = run.emphasis;
      element.append(span);
    }
  }

  private sparkline(
    sparkline: SparklineSpec,
    tone: NonNullable<ListItemSpec["valueTone"]>,
    compact = true,
  ): SVGSVGElement {
    const namespace = "http://www.w3.org/2000/svg";
    const width = compact ? Math.min(Math.max(sparkline.series.length * 4, 64), 180) : 640;
    const height = compact ? 24 : 260;
    const top = compact ? 2 : 34;
    const bottom = compact ? height - 2 : height - 20;
    const element = document.createElementNS(namespace, "svg");
    element.classList.add("unpeel-sparkline");
    if (!compact) element.classList.add("unpeel-chart", "unpeel-chart--sparkline");
    element.dataset.tone = tone;
    element.setAttribute("viewBox", `0 0 ${width} ${height}`);
    element.setAttribute("width", compact ? String(width) : "100%");
    element.setAttribute("height", String(height));
    element.setAttribute("role", "img");
    element.setAttribute("aria-label", sparkline.accessibilityText);
    element.style.display = "block";
    if (compact) element.style.flex = "0 0 auto";

    const title = document.createElementNS(namespace, "title");
    title.textContent = [sparkline.caption, sparkline.unit, sparkline.accessibilityText]
      .filter((value): value is string => value !== undefined && value.length > 0)
      .join(" · ");
    const polyline = document.createElementNS(namespace, "polyline");
    const normalized = normalizedSparklineSeries(sparkline);
    polyline.setAttribute("points", normalized.map((value, index) => {
      const x = normalized.length === 1 ? width / 2 : (index / (normalized.length - 1)) * width;
      const y = bottom - value * (bottom - top);
      return `${x.toFixed(3)},${y.toFixed(3)}`;
    }).join(" "));
    polyline.setAttribute("fill", "none");
    polyline.setAttribute("stroke", "currentColor");
    polyline.setAttribute("stroke-width", "1.5");
    polyline.setAttribute("stroke-linecap", "round");
    polyline.setAttribute("stroke-linejoin", "round");
    element.append(title, polyline);
    if (!compact && (sparkline.caption !== undefined || sparkline.unit !== undefined)) {
      const caption = document.createElementNS(namespace, "text");
      caption.setAttribute("x", "0");
      caption.setAttribute("y", "18");
      caption.setAttribute("fill", "currentColor");
      caption.setAttribute("font-size", "14");
      caption.textContent = [sparkline.caption, sparkline.unit]
        .filter((value): value is string => value !== undefined)
        .join(" · ");
      element.append(caption);
    }
    if (normalized.length === 1) {
      const point = document.createElementNS(namespace, "circle");
      point.setAttribute("cx", String(width / 2));
      point.setAttribute("cy", String(bottom - normalized[0]! * (bottom - top)));
      point.setAttribute("r", "2");
      point.setAttribute("fill", "currentColor");
      element.append(point);
    }
    this.configureChartActivation(element, sparkline);
    return element;
  }

  private compactGauge(
    gauge: GaugeSpec,
    tone: NonNullable<ListItemSpec["valueTone"]>,
    showCaption = true,
  ): HTMLDivElement {
    const element = document.createElement("div");
    element.className = "unpeel-list-item__gauge";
    element.dataset.tone = tone;
    const caption = document.createElement("span");
    caption.className = "unpeel-list-item__gauge-caption";
    caption.textContent = gaugeValueLabel(gauge);
    caption.hidden = !showCaption;
    const progress = document.createElement("progress");
    progress.max = 1;
    progress.value = gauge.ratio;
    progress.setAttribute("aria-label", gauge.accessibilityText);
    progress.setAttribute("aria-valuemin", "0");
    progress.setAttribute("aria-valuemax", "1");
    progress.setAttribute("aria-valuenow", String(gauge.ratio));
    element.append(caption, progress);
    this.configureChartActivation(element, gauge);
    return element;
  }

  private barChart(chart: BarChartSpec): SVGSVGElement {
    const namespace = "http://www.w3.org/2000/svg";
    const width = Math.max(640, chart.bars.length * 54);
    const height = 320;
    const left = 40;
    const right = 20;
    const top = 28;
    const bottom = 258;
    const plotWidth = width - left - right;
    const plotHeight = bottom - top;
    const normalized = normalizedBarChartValues(chart);
    const slotWidth = plotWidth / chart.bars.length;
    const barWidth = Math.max(Math.min(slotWidth * 0.68, 48), 1);
    const element = this.chartSVG(chart, width, height, "bar-chart");

    const baseline = document.createElementNS(namespace, "line");
    baseline.setAttribute("x1", String(left));
    baseline.setAttribute("x2", String(width - right));
    baseline.setAttribute("y1", String(bottom));
    baseline.setAttribute("y2", String(bottom));
    baseline.setAttribute("stroke", "currentColor");
    baseline.setAttribute("stroke-opacity", "0.35");
    element.append(baseline);

    chart.bars.forEach((bar, index) => {
      const group = document.createElementNS(namespace, "g");
      group.dataset.label = bar.label;
      group.dataset.value = String(bar.value);
      group.dataset.emphasis = bar.emphasis ?? "default";
      const center = left + slotWidth * (index + 0.5);
      const barHeight = normalized[index]! * plotHeight;
      const rectangle = document.createElementNS(namespace, "rect");
      rectangle.setAttribute("x", String(center - barWidth / 2));
      rectangle.setAttribute("y", String(bottom - barHeight));
      rectangle.setAttribute("width", String(barWidth));
      rectangle.setAttribute("height", String(barHeight));
      rectangle.setAttribute("rx", "2");
      rectangle.setAttribute("fill", this.barColor(bar.emphasis ?? "default"));

      const label = document.createElementNS(namespace, "text");
      label.setAttribute("x", String(center));
      label.setAttribute("y", "282");
      label.setAttribute("text-anchor", "middle");
      label.setAttribute("fill", "currentColor");
      label.setAttribute("font-size", "12");
      label.textContent = bar.label;
      group.append(rectangle);
      if (bar.valueCaption !== undefined) {
        const value = document.createElementNS(namespace, "text");
        value.setAttribute("x", String(center));
        value.setAttribute("y", String(Math.max(bottom - barHeight - 7, 14)));
        value.setAttribute("text-anchor", "middle");
        value.setAttribute("fill", "currentColor");
        value.setAttribute("font-size", "11");
        value.textContent = bar.valueCaption;
        group.append(value);
      }
      group.append(label);
      element.append(group);
    });
    this.configureChartActivation(element, chart);
    return element;
  }

  private lineChart(chart: LineChartSpec): SVGSVGElement {
    const namespace = "http://www.w3.org/2000/svg";
    const width = 640;
    const height = 320;
    const left = 60;
    const right = 24;
    const top = 42;
    const bottom = 264;
    const [xMinimum, xMaximum] = resolvedLineChartBounds(chart, "x");
    const [yMinimum, yMaximum] = resolvedLineChartBounds(chart, "y");
    const x = (value: number): number => left
      + ((value - xMinimum) / (xMaximum - xMinimum)) * (width - left - right);
    const y = (value: number): number => bottom
      - ((value - yMinimum) / (yMaximum - yMinimum)) * (bottom - top);
    const element = this.chartSVG(chart, width, height, "line-chart");

    for (const [x1, y1, x2, y2] of [
      [left, bottom, width - right, bottom],
      [left, top, left, bottom],
    ]) {
      const axis = document.createElementNS(namespace, "line");
      axis.setAttribute("x1", String(x1));
      axis.setAttribute("y1", String(y1));
      axis.setAttribute("x2", String(x2));
      axis.setAttribute("y2", String(y2));
      axis.setAttribute("stroke", "currentColor");
      axis.setAttribute("stroke-opacity", "0.4");
      element.append(axis);
    }

    chart.series.forEach((series, index) => {
      const color = this.lineColor(index);
      const polyline = document.createElementNS(namespace, "polyline");
      polyline.dataset.series = series.name;
      polyline.setAttribute("points", series.points
        .map((point) => `${x(point.x).toFixed(3)},${y(point.y).toFixed(3)}`)
        .join(" "));
      polyline.setAttribute("fill", "none");
      polyline.setAttribute("stroke", color);
      polyline.setAttribute("stroke-width", "2");
      polyline.setAttribute("stroke-linecap", "round");
      polyline.setAttribute("stroke-linejoin", "round");
      element.append(polyline);
      if (series.points.length === 1) {
        const point = series.points[0]!;
        const circle = document.createElementNS(namespace, "circle");
        circle.setAttribute("cx", String(x(point.x)));
        circle.setAttribute("cy", String(y(point.y)));
        circle.setAttribute("r", "3");
        circle.setAttribute("fill", color);
        element.append(circle);
      }
      const legend = document.createElementNS(namespace, "text");
      legend.setAttribute("x", String(left + (index % 8) * ((width - left - right) / 8)));
      legend.setAttribute("y", String(16 + Math.floor(index / 8) * 14));
      legend.setAttribute("fill", color);
      legend.setAttribute("font-size", "12");
      legend.textContent = series.name;
      element.append(legend);
    });

    const labels: Array<[string, number, number, string]> = [
      [String(xMinimum), left, 284, "start"],
      [String(xMaximum), width - right, 284, "end"],
      [String(yMinimum), left - 8, bottom + 4, "end"],
      [String(yMaximum), left - 8, top + 4, "end"],
    ];
    for (const [text, xPosition, yPosition, anchor] of labels) {
      const label = document.createElementNS(namespace, "text");
      label.setAttribute("x", String(xPosition));
      label.setAttribute("y", String(yPosition));
      label.setAttribute("text-anchor", anchor);
      label.setAttribute("fill", "currentColor");
      label.setAttribute("fill-opacity", "0.7");
      label.setAttribute("font-size", "10");
      label.textContent = text;
      element.append(label);
    }
    if (chart.xAxis?.label !== undefined) {
      const label = document.createElementNS(namespace, "text");
      label.setAttribute("x", String((left + width - right) / 2));
      label.setAttribute("y", "308");
      label.setAttribute("text-anchor", "middle");
      label.setAttribute("fill", "currentColor");
      label.setAttribute("font-size", "12");
      label.textContent = chart.xAxis.label;
      element.append(label);
    }
    if (chart.yAxis?.label !== undefined) {
      const label = document.createElementNS(namespace, "text");
      label.setAttribute("x", "14");
      label.setAttribute("y", String((top + bottom) / 2));
      label.setAttribute("text-anchor", "middle");
      label.setAttribute("fill", "currentColor");
      label.setAttribute("font-size", "12");
      label.setAttribute("transform", `rotate(-90 14 ${(top + bottom) / 2})`);
      label.textContent = chart.yAxis.label;
      element.append(label);
    }
    this.configureChartActivation(element, chart);
    return element;
  }

  private gauge(gauge: GaugeSpec): SVGSVGElement {
    const namespace = "http://www.w3.org/2000/svg";
    const width = 640;
    const height = 140;
    const left = 40;
    const trackWidth = width - left * 2;
    const element = this.chartSVG(gauge, width, height, "gauge");
    element.dataset.ratio = String(gauge.ratio);

    const label = document.createElementNS(namespace, "text");
    label.setAttribute("x", String(left));
    label.setAttribute("y", "35");
    label.setAttribute("fill", "currentColor");
    label.setAttribute("font-size", "16");
    label.textContent = gauge.label;
    const percentage = document.createElementNS(namespace, "text");
    percentage.setAttribute("x", String(width - left));
    percentage.setAttribute("y", "35");
    percentage.setAttribute("text-anchor", "end");
    percentage.setAttribute("fill", "currentColor");
    percentage.setAttribute("font-size", "16");
    percentage.textContent = gaugeValueLabel(gauge);

    const track = document.createElementNS(namespace, "rect");
    track.setAttribute("x", String(left));
    track.setAttribute("y", "60");
    track.setAttribute("width", String(trackWidth));
    track.setAttribute("height", "28");
    track.setAttribute("rx", "14");
    track.setAttribute("fill", "currentColor");
    track.setAttribute("fill-opacity", "0.16");
    const fill = document.createElementNS(namespace, "rect");
    fill.setAttribute("x", String(left));
    fill.setAttribute("y", "60");
    fill.setAttribute("width", String(trackWidth * gauge.ratio));
    fill.setAttribute("height", "28");
    fill.setAttribute("rx", "14");
    fill.setAttribute("fill", "var(--accent, #0ea5e9)");
    element.append(label, percentage, track, fill);
    this.configureChartActivation(element, gauge);
    return element;
  }

  private chartSVG(
    chart: { id: string; accessibilityText: string },
    width: number,
    height: number,
    kind: string,
  ): SVGSVGElement {
    const element = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    element.id = chart.id;
    element.classList.add("unpeel-chart", `unpeel-chart--${kind}`);
    element.setAttribute("viewBox", `0 0 ${width} ${height}`);
    element.setAttribute("width", "100%");
    element.setAttribute("height", String(height));
    element.setAttribute("role", "img");
    element.setAttribute("aria-label", chart.accessibilityText);
    const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
    title.textContent = chart.accessibilityText;
    element.append(title);
    return element;
  }

  private configureChartActivation(
    element: HTMLElement | SVGSVGElement,
    chart: { id: string; activate?: string; accessibilityText: string },
  ): void {
    element.id = chart.id;
    if (chart.activate === undefined) return;
    element.setAttribute("role", "button");
    element.setAttribute("tabindex", "0");
    element.style.cursor = "pointer";
    const activate = (): void => {
      this.onAction(uiAction(chart.id, chart.activate!, "activate"));
    };
    element.addEventListener("click", (event) => {
      event.stopPropagation();
      activate();
    });
    element.addEventListener("keydown", (event) => {
      const key = (event as KeyboardEvent).key;
      if (key !== "Enter" && key !== " ") return;
      event.preventDefault();
      event.stopPropagation();
      activate();
    });
  }

  private barColor(emphasis: NonNullable<BarChartSpec["bars"][number]["emphasis"]>): string {
    switch (emphasis) {
      case "accent": return "var(--accent, #0ea5e9)";
      case "danger": return "var(--danger, #ef4444)";
      case "default": return "currentColor";
    }
  }

  private lineColor(index: number): string {
    const colors = [
      "var(--accent, #0ea5e9)",
      "#a855f7",
      "#22c55e",
      "#eab308",
      "#3b82f6",
      "var(--danger, #ef4444)",
    ];
    return colors[index % colors.length]!;
  }

  private status(status: StatusSymbolSpec): HTMLSpanElement {
    const element = document.createElement("span");
    element.className = "unpeel-status-symbol";
    element.textContent = status.symbol;
    element.dataset.tone = status.tone ?? "default";
    element.dataset.emphasis = status.emphasis ?? "regular";
    element.setAttribute("aria-label", status.label);
    return element;
  }

  private badge(badge: BadgeSpec): HTMLSpanElement {
    const element = document.createElement("span");
    element.className = "unpeel-badge";
    element.textContent = badge.text;
    element.dataset.tone = badge.tone ?? "default";
    return element;
  }

  private toggle(toggle: ToggleSpec): HTMLLabelElement {
    const label = document.createElement("label");
    label.className = "unpeel-toggle";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = toggle.value;
    input.setAttribute("aria-label", toggle.label);
    input.addEventListener("change", () => {
      this.onAction(uiAction(
        toggle.id,
        toggle.setValue,
        "change",
        { type: "bool", value: input.checked },
      ));
    });
    const text = document.createElement("span");
    text.textContent = toggle.label;
    text.className = "unpeel-toggle__label";
    label.append(input, text);
    return label;
  }

  private disclosure(): HTMLSpanElement {
    const element = document.createElement("span");
    element.className = "unpeel-list-item__disclosure";
    element.textContent = "›";
    element.setAttribute("aria-hidden", "true");
    return element;
  }

  private checkmark(checkmark: CheckmarkSpec): HTMLSpanElement {
    const element = document.createElement("span");
    element.className = "unpeel-list-item__checkmark";
    element.textContent = checkmark.value ? "✓" : "";
    element.setAttribute(
      "aria-label",
      `${checkmark.label}: ${checkmark.value ? "selected" : "not selected"}`,
    );
    return element;
  }

  private invokePrimary(item: ListItemSpec): boolean {
    const role = listItemPrimaryRole(item);
    if (role === "toggle") {
      const toggle = [item.leading, item.trailing, item.accessory]
        .find((slot): slot is ToggleSpec => slot !== undefined && isToggleSlot(slot));
      if (toggle === undefined) return false;
      this.onAction(uiAction(
        toggle.id,
        toggle.setValue,
        "change",
        { type: "bool", value: !toggle.value },
      ));
      return true;
    }
    if (role === "checkmark") {
      const checkmark = [item.leading, item.trailing, item.accessory]
        .find((slot): slot is CheckmarkSpec => slot !== undefined && isCheckmarkSlot(slot));
      if (checkmark === undefined) return false;
      this.onAction(uiAction(
        checkmark.id,
        checkmark.setValue,
        "change",
        { type: "bool", value: !checkmark.value },
      ));
      return true;
    }
    if ((role === "disclosure" || role === "command" || role === "destructive")
      && item.activate !== undefined) {
      this.onAction(uiAction(item.id, item.activate, "activate"));
      return true;
    }
    if (role === "command") {
      const sparkline = [item.leading, item.trailing, item.accessory]
        .find((slot): slot is SparklineSpec => slot !== undefined
          && isSparklineSlot(slot) && slot.activate !== undefined);
      if (sparkline?.activate !== undefined) {
        this.onAction(uiAction(sparkline.id, sparkline.activate, "activate"));
        return true;
      }
      const gauge = [item.leading, item.trailing, item.accessory]
        .find((slot): slot is GaugeSpec => slot !== undefined
          && isGaugeSlot(slot) && slot.activate !== undefined);
      if (gauge?.activate !== undefined) {
        this.onAction(uiAction(gauge.id, gauge.activate, "activate"));
        return true;
      }
      return false;
    }
    return false;
  }

  private reconcileSelection(list: ListSpec): void {
    const previousServer = this.serverSelections.get(list.id);
    if (!this.selections.has(list.id) || previousServer !== list.selectedId) {
      this.selections.set(list.id, list.selectedId);
    }
    this.serverSelections.set(list.id, list.selectedId);
  }

  private select(list: ListSpec, itemID: string): void {
    if (!list.items.some((item) => item.id === itemID)) return;
    const changed = this.selections.get(list.id) !== itemID;
    this.selectLocally(list, itemID);
    if (changed && list.select !== undefined) {
      this.onAction(uiAction(
        list.id,
        list.select,
        "change",
        { type: "text", value: itemID },
      ));
    }
  }

  private selectLocally(list: ListSpec, itemID: string): void {
    if (!list.items.some((item) => item.id === itemID)) return;
    this.selections.set(list.id, itemID);
    for (const row of this.element.querySelectorAll<HTMLElement>(".unpeel-list-item")) {
      const selected = row.dataset.id === itemID;
      row.dataset.selected = String(selected);
      row.setAttribute("aria-selected", String(selected));
    }
  }

  private handleListKey(
    event: KeyboardEvent,
    page: PageNode,
    list: ListSpec,
    element: HTMLElement,
  ): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (list.items.length === 0) return;
    const selectedID = this.selections.get(list.id);
    const selectedIndex = list.items.findIndex((item) => item.id === selectedID);
    const current = selectedIndex >= 0 ? selectedIndex : 0;
    const item = list.items[current];
    const decision = listNavigationDecision(event.key, listItemPrimaryRole(item));
    if (decision === "back") {
      if (page.back === undefined) return;
      event.preventDefault();
      this.onAction(uiAction(page.id, page.back, "cancel"));
      return;
    }
    if (decision === "invokePrimary") {
      event.preventDefault();
      this.invokePrimary(item);
      return;
    }
    if ((decision === "pageDown" || decision === "pageUp")
      && (list.pageBehavior ?? "selection") === "scroll") {
      return;
    }
    const firstRow = element.querySelector<HTMLElement>(".unpeel-list-item");
    const rowHeight = Math.max(firstRow?.getBoundingClientRect().height ?? 28, 1);
    const visibleRows = Math.max(Math.floor(element.clientHeight / rowHeight), 1);
    const pageRows = Math.max(visibleRows - (list.pageOverlap ?? 1), 1);
    const last = list.items.length - 1;
    let next: number | undefined;
    switch (decision) {
      case "down": next = Math.min(current + 1, last); break;
      case "up": next = Math.max(current - 1, 0); break;
      case "first": next = 0; break;
      case "last": next = last; break;
      case "pageDown": next = Math.min(current + pageRows, last); break;
      case "pageUp": next = Math.max(current - pageRows, 0); break;
      default: break;
    }
    if (next === undefined) return;
    event.preventDefault();
    const itemID = list.items[next].id;
    this.select(list, itemID);
    element.querySelector<HTMLElement>(`.unpeel-list-item[data-id="${this.escapeAttribute(itemID)}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }

  private configureValueVisibility(element: HTMLElement, list: ListSpec): void {
    if (typeof ResizeObserver === "undefined") return;
    for (const row of element.querySelectorAll<HTMLElement>(".unpeel-list-item")) {
      const item = list.items.find((candidate) => candidate.id === row.dataset.id);
      const value = row.querySelector<HTMLElement>(".unpeel-list-item__value");
      const gaugeCaption = row.querySelector<HTMLElement>(".unpeel-list-item__gauge-caption");
      if (item === undefined || value === null) continue;
      const minimum = item.valueMinWidth ?? Math.min((item.value?.length ?? 0) + 11, 65_535);
      const update = (): void => {
        const width = row.getBoundingClientRect().width;
        if (width > 0) value.hidden = width < minimum * 8;
        if (gaugeCaption !== null) gaugeCaption.hidden = !value.hidden;
      };
      const observer = new ResizeObserver(update);
      observer.observe(row);
      this.resizeObservers.push(observer);
      update();
    }
  }

  private disconnectResizeObservers(): void {
    for (const observer of this.resizeObservers) observer.disconnect();
    this.resizeObservers.length = 0;
  }

  private escapeAttribute(value: string): string {
    return value.replace(/(["\\])/g, "\\$1");
  }
}
