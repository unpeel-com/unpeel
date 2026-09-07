import SwiftUI
import UnpeelAppKitUI
@preconcurrency import WebKit

/// An embedded browser interpretation of the same projection rendered by the
/// SwiftUI card. Participant credentials remain in the mini-host process; only
/// renderer-local UIAction values cross the script-message boundary.
struct WebComponentPane: NSViewRepresentable {
    let snapshot: UISnapshot
    let surfaceEndpoint: SurfaceWebEndpoint?
    let onAction: (UIAction) -> Void

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        var onAction: (UIAction) -> Void
        private var isReady = false
        private var latestSnapshot: UISnapshot?
        private var lastSentSnapshot: UISnapshot?

        init(onAction: @escaping (UIAction) -> Void) {
            self.onAction = onAction
        }

        func render(_ snapshot: UISnapshot, in webView: WKWebView) {
            latestSnapshot = snapshot
            guard isReady else { return }
            sendLatestSnapshot(to: webView)
        }

        func webView(_ webView: WKWebView, didFinish _: WKNavigation!) {
            isReady = true
            lastSentSnapshot = nil
            sendLatestSnapshot(to: webView)
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            isReady = false
            lastSentSnapshot = nil
            webView.reload()
        }

        func userContentController(
            _: WKUserContentController,
            didReceive message: WKScriptMessage
        ) {
            if message.name == "unpeelDiagnostic" {
                NSLog("Kitchen Sink web: %@", String(describing: message.body))
                return
            }
            guard message.name == "unpeelAction" else { return }
            do {
                let data = try JSONSerialization.data(withJSONObject: message.body)
                let action = try JSONDecoder().decode(UIAction.self, from: data)
                onAction(action)
            } catch {
                NSLog("Kitchen Sink rejected web UI action: %@", String(describing: error))
            }
        }

        private func sendLatestSnapshot(to webView: WKWebView) {
            guard let snapshot = latestSnapshot, snapshot != lastSentSnapshot else { return }
            do {
                let data = try JSONEncoder().encode(snapshot)
                let object = try JSONSerialization.jsonObject(with: data)
                lastSentSnapshot = snapshot
                webView.callAsyncJavaScript(
                    "window.unpeelRenderSnapshot(snapshot)",
                    arguments: ["snapshot": object],
                    in: nil,
                    in: .page
                ) { result in
                    if case let .failure(error) = result {
                        self.lastSentSnapshot = nil
                        NSLog(
                            "Kitchen Sink web renderer failed: %@",
                            String(describing: error)
                        )
                    }
                }
            } catch {
                NSLog("Kitchen Sink could not encode web snapshot: %@", String(describing: error))
            }
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onAction: onAction)
    }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        configuration.userContentController.add(
            context.coordinator,
            name: "unpeelAction"
        )
        configuration.userContentController.add(
            context.coordinator,
            name: "unpeelDiagnostic"
        )

        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.underPageBackgroundColor = .clear
        let page = Bundle.module.url(
            forResource: "index",
            withExtension: "html",
            subdirectory: "Web"
        ) ?? Bundle.module.url(forResource: "index", withExtension: "html")
        guard let page else {
            webView.loadHTMLString(
                "<p>Missing Kitchen Sink web renderer resources.</p>",
                baseURL: nil
            )
            return webView
        }
        var components = URLComponents(url: page, resolvingAgainstBaseURL: false)
        if let surfaceEndpoint {
            components?.queryItems = [
                URLQueryItem(name: "host", value: surfaceEndpoint.host),
                URLQueryItem(name: "port", value: String(surfaceEndpoint.port)),
                URLQueryItem(name: "token", value: surfaceEndpoint.token),
                URLQueryItem(name: "surfaceModule", value: surfaceEndpoint.moduleURL),
                URLQueryItem(name: "surfaceWasm", value: surfaceEndpoint.wasmURL),
            ]
        }
        webView.loadFileURL(
            components?.url ?? page,
            allowingReadAccessTo: page.deletingLastPathComponent()
        )
        context.coordinator.render(snapshot, in: webView)
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        context.coordinator.onAction = onAction
        context.coordinator.render(snapshot, in: webView)
    }

    static func dismantleNSView(_ webView: WKWebView, coordinator _: Coordinator) {
        webView.configuration.userContentController.removeScriptMessageHandler(
            forName: "unpeelAction"
        )
        webView.configuration.userContentController.removeScriptMessageHandler(
            forName: "unpeelDiagnostic"
        )
        webView.navigationDelegate = nil
        webView.stopLoading()
    }
}
