import Combine
import Foundation
import UnpeelShared

/// Settings observe this small model directly. Device polling never invalidates
/// the terminal/sidebar store, and a repeated response is a publication no-op.
@MainActor
final class HostManagementState: ObservableObject {
    struct Value: Equatable {
        var devices: [RemotePairedDeviceSummary] = []
        var endpoint: URL?
        var error: String?
    }

    @Published private(set) var value = Value()

    func apply(_ next: Value) {
        guard next != value else { return }
        value = next
    }

    func update(endpoint: URL?) {
        var next = value
        next.endpoint = endpoint
        apply(next)
    }

    func update(error: String?) {
        var next = value
        next.error = error
        apply(next)
    }
}
