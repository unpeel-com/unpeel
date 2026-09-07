// Browser-level interpretation of the shared Rust/Swift/web conformance fixture.
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  applyUiDelta,
  decodeUiMessage,
  type UiDelta,
  type UiEvent,
  type UiSnapshot,
} from "../src/protocol";

const here = dirname(fileURLToPath(import.meta.url));
const repository = resolve(here, "../..");
const fixture = readFileSync(resolve(repository, "protocol/unpeel-ui-v1.ndjson"), "utf8")
  .trimEnd()
  .split("\n")
  .slice(-5)
  .map(decodeUiMessage);

test("slash keystroke presents the authoritative Menu and returns its action", async ({ page }) => {
  const [initialMessage, slashMessage, openMessage, selectionMessage, selectedMessage] = fixture;
  expect(initialMessage?.type).toBe("snapshot");
  expect(slashMessage?.type).toBe("event");
  expect(openMessage?.type).toBe("delta");
  expect(selectionMessage?.type).toBe("event");
  expect(selectedMessage?.type).toBe("delta");
  const initial = initialMessage as UiSnapshot;
  const slashEvent = slashMessage as UiEvent;
  const menuSnapshot = applyUiDelta(initial, openMessage as UiDelta);
  const selectionEvent = selectionMessage as UiEvent;
  const selectedSnapshot = applyUiDelta(menuSnapshot, selectedMessage as UiDelta);

  await page.addInitScript(() => {
    const actions: unknown[] = [];
    Object.assign(window, {
      __unpeelActions: actions,
      webkit: {
        messageHandlers: {
          unpeelAction: { postMessage: (action: unknown) => actions.push(action) },
          unpeelDiagnostic: { postMessage: () => {} },
        },
      },
    });
  });
  const kitchenPage = pathToFileURL(resolve(
    repository,
    "swift/Examples/KitchenSink/Sources/KitchenSink/Resources/Web/index.html",
  )).href;
  await page.goto(kitchenPage);
  await page.evaluate((snapshot) => {
    (window as unknown as { unpeelRenderSnapshot(value: UiSnapshot): void })
      .unpeelRenderSnapshot(snapshot);
  }, initial);

  const editor = page.locator("textarea.unpeel-markdown-editor__source");
  await editor.focus();
  await editor.press("/");
  const slashAction = await page.evaluate(() => (
    window as unknown as { __unpeelActions: UiEvent[] }
  ).__unpeelActions.find((action) => action.action === "open-menu"));
  expect(slashAction).toMatchObject({
    nodeId: slashEvent.nodeId,
    action: slashEvent.action,
    kind: slashEvent.kind,
    value: slashEvent.value,
  });

  await page.evaluate((snapshot) => {
    (window as unknown as { unpeelRenderSnapshot(value: UiSnapshot): void })
      .unpeelRenderSnapshot(snapshot);
  }, menuSnapshot);
  const menu = page.getByRole("menu", { name: "Insert block" });
  await expect(menu).toBeVisible();
  await page.getByRole("menuitem", { name: /Heading 1/u }).click();
  const selectionAction = await page.evaluate(() => (
    window as unknown as { __unpeelActions: UiEvent[] }
  ).__unpeelActions.find((action) => action.action === "markdown-menu-select"));
  expect(selectionAction).toMatchObject({
    nodeId: selectionEvent.nodeId,
    action: selectionEvent.action,
    kind: selectionEvent.kind,
    value: selectionEvent.value,
  });

  await page.evaluate((snapshot) => {
    (window as unknown as { unpeelRenderSnapshot(value: UiSnapshot): void })
      .unpeelRenderSnapshot(snapshot);
  }, selectedSnapshot);
  await expect(menu).toBeHidden();
  await expect(editor).toHaveValue("# ");
});
