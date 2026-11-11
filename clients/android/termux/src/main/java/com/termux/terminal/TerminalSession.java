package com.termux.terminal;

/**
 * Unpeel does not start a local shell. Upstream {@code TerminalSession}
 * forks a PTY through {@code JNI} and {@code libtermux.so}; both are
 * omitted. This stub exists only so {@link TerminalSessionClient} compiles.
 * Nothing in the app constructs it.
 */
public final class TerminalSession {
    private TerminalSession() {}
}
