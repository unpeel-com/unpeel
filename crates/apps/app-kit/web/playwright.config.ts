import { defineConfig } from "@playwright/test";
import { tmpdir } from "node:os";
import { join } from "node:path";

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.pw.ts",
  outputDir: join(tmpdir(), "unpeel-app-kit-playwright"),
  fullyParallel: true,
  forbidOnly: true,
  timeout: 15_000,
  use: {
    headless: true,
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
});
