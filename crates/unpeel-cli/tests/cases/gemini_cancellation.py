"""Gemini skips AfterAgent on abort; verify the Host's ESC fallback in a PTY."""

from hook_cancellation import body, run

if __name__ == "__main__":
    run("gemini_cancellation", lambda case: body(case, runtime="gemini"))
