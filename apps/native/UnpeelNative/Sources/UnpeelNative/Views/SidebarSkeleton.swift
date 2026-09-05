//
//  SidebarSkeleton.swift
//  UnpeelNative
//
//  Loading placeholder for sidebar content that is not knowable yet: the
//  workspace-swipe peek panel of a never-reached host (no pooled snapshot
//  exists before first contact) and the remote-scope connecting/reconnecting
//  state. A deliberately BLANK sidebar with one small, muted, centered
//  spinner — skeleton placeholder rows were tried and removed (2026-08-18):
//  fake rows promised structure that first contact often contradicted.
//

import SwiftUI

/// A blank sidebar area with a small centered spinner: muted, unobtrusive,
/// and shared by the carousel's never-reached-host page and the remote
/// scope's connecting/reconnecting empty state, so a swipe commit into a
/// still-connecting Host reads as one continuous quiet load.
struct SidebarLoadingPlaceholder: View {
    var body: some View {
        ProgressView()
            .controlSize(.small)
            .tint(Theme.mutedForeground)
            .opacity(0.55)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
