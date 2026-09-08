import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { decodeUiMessage, type UiAction, type UiSnapshot } from "../src/protocol";

const kit = fileURLToPath(new URL("../..", import.meta.url));
const fixture = readFileSync(resolve(kit, "protocol/unpeel-ui-v1.ndjson"), "utf8")
  .trimEnd().split("\n").map(decodeUiMessage)
  .find((message) => message.type === "snapshot" && message.root.id === "git-page") as UiSnapshot;

test("Page tabs stay at the top and emit shared actions by mouse and keyboard", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 560 });
  await page.addInitScript(() => {
    const actions: unknown[] = [];
    Object.assign(window, {
      __tabActions: actions,
      webkit: { messageHandlers: {
        unpeelAction: { postMessage: (action: unknown) => actions.push(action) },
        unpeelDiagnostic: { postMessage: () => {} },
      } },
    });
  });
  await page.goto(pathToFileURL(resolve(kit, "swift/Examples/KitchenSink/Sources/KitchenSink/Resources/Web/index.html")).href);
  await page.evaluate((snapshot) => {
    (window as unknown as { unpeelRenderSnapshot(value: UiSnapshot): void }).unpeelRenderSnapshot(snapshot);
  }, fixture);
  const changes = page.getByRole("tab", { name: "Changes" });
  const history = page.getByRole("tab", { name: "History" });
  await expect(changes).toHaveAttribute("aria-selected", "true");
  await expect(history).toHaveAttribute("aria-selected", "false");
  const tabsBox = await page.getByRole("tablist").boundingBox();
  const titleBox = await page.getByRole("heading", { name: "main · 1 changed" }).boundingBox();
  expect(tabsBox!.y + tabsBox!.height).toBeLessThanOrEqual(titleBox!.y);
  await history.click();
  expect(await page.evaluate(() => (window as unknown as { __tabActions: UiAction[] }).__tabActions.at(-1)))
    .toMatchObject({ nodeId: "history-tab", action: "show-history", kind: "activate", value: { type: "none" } });
  await changes.focus();
  await changes.press("ArrowRight");
  await expect(history).toBeFocused();
  const fetch = page.getByRole("button", { name: "Fetch", exact: true });
  await fetch.click();
  expect(await page.evaluate(() => (window as unknown as { __tabActions: UiAction[] }).__tabActions.at(-1)))
    .toMatchObject({ nodeId: "git-fetch", action: "fetch", kind: "activate", value: { type: "none" } });
  const menu = page.getByRole("button", { name: "Remote actions" });
  await menu.click();
  await expect(page.getByRole("menuitem", { name: "Push" })).toBeDisabled();
  await page.getByRole("menuitem", { name: "Pull (fast-forward)" }).click();
  expect(await page.evaluate(() => (window as unknown as { __tabActions: UiAction[] }).__tabActions.at(-1)))
    .toMatchObject({ nodeId: "pull-menu", action: "pull", kind: "activate", value: { type: "none" } });
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await menu.press("ArrowDown");
  await expect(page.getByRole("menu")).toBeVisible();
  await page.getByRole("menu").press("Escape");
  await expect(menu).toBeFocused();
  await expect(menu).toHaveAttribute("aria-expanded", "false");
  await page.screenshot({ path: "/tmp/unpeel-git-tabs-web.png" });
});
