import type { ListItemPrimaryRole } from "./protocol";

export type ListNavigationDecision = "down" | "up" | "first" | "last"
  | "pageDown" | "pageUp" | "invokePrimary" | "back";

/** One focused-row keyboard table shared by every DOM Page/List renderer. */
export function listNavigationDecision(
  key: string,
  primaryRole: ListItemPrimaryRole,
): ListNavigationDecision | undefined {
  switch (key) {
    case "Enter": return primaryRole === "static" ? undefined : "invokePrimary";
    case " ": return primaryRole === "toggle" ? "invokePrimary" : "pageDown";
    case "ArrowDown":
    case "j": return "down";
    case "ArrowUp":
    case "k": return "up";
    case "Home":
    case "g": return "first";
    case "End":
    case "G": return "last";
    case "PageDown": return "pageDown";
    case "PageUp": return "pageUp";
    case "Escape":
    case "q": return "back";
    default: return undefined;
  }
}
