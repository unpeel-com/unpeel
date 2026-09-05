//
//  NearbyHostBrowser.swift
//  UnpeelNative
//
//  Local-network discovery for the Add Host sheet. Bonjour is only a hint:
//  choosing a row never grants access, and the sealed one-time pairing code
//  still authenticates the Host identity and endpoint.
//

import Foundation
import Network

struct NearbyHostCandidate: Equatable, Identifiable, Sendable {
    let hostID: String
    let name: String

    var id: String { hostID }
}

enum NearbyHostCatalog {
    static func candidate(
        serviceName: String,
        txt: [String: String]
    ) -> NearbyHostCandidate? {
        guard let hostID = txt["macid"]?.trimmingCharacters(in: .whitespacesAndNewlines),
              !hostID.isEmpty
        else { return nil }
        let trimmedName = serviceName.trimmingCharacters(in: .whitespacesAndNewlines)
        return NearbyHostCandidate(
            hostID: hostID,
            name: trimmedName.isEmpty ? "Unpeel Host" : trimmedName
        )
    }

    static func merging(
        _ candidates: [NearbyHostCandidate],
        excludingHostID: String? = nil
    ) -> [NearbyHostCandidate] {
        let excluded = excludingHostID?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        var byID: [String: NearbyHostCandidate] = [:]
        for candidate in candidates {
            let key = candidate.hostID.lowercased()
            guard key != excluded, byID[key] == nil else { continue }
            byID[key] = candidate
        }
        return byID.values.sorted {
            let order = $0.name.localizedCaseInsensitiveCompare($1.name)
            return order == .orderedSame ? $0.hostID < $1.hostID : order == .orderedAscending
        }
    }
}

@MainActor
final class NearbyHostBrowser: ObservableObject {
    enum State: Equatable {
        case idle
        case searching
        case unavailable(String)
    }

    static let serviceType = "_unpeel-remote._tcp"

    @Published private(set) var candidates: [NearbyHostCandidate] = []
    @Published private(set) var state: State = .idle

    private var browser: NWBrowser?
    private let excludedHostID: String?

    init(excludingHostID: String? = nil) {
        self.excludedHostID = excludingHostID
    }

    func start() {
        guard browser == nil else { return }
        state = .searching
        let browser = NWBrowser(
            for: .bonjourWithTXTRecord(type: Self.serviceType, domain: nil),
            using: NWParameters()
        )
        let excludedHostID = excludedHostID
        self.browser = browser
        browser.browseResultsChangedHandler = { results, _ in
            let discovered = results.compactMap { result -> NearbyHostCandidate? in
                guard case .bonjour(let txt) = result.metadata else { return nil }
                let name: String
                if case .service(let serviceName, _, _, _) = result.endpoint {
                    name = serviceName
                } else {
                    name = "Unpeel Host"
                }
                guard let hostID = txt["macid"] else { return nil }
                return NearbyHostCatalog.candidate(
                    serviceName: name,
                    txt: ["macid": hostID]
                )
            }
            let merged = NearbyHostCatalog.merging(
                discovered,
                excludingHostID: excludedHostID
            )
            Task { @MainActor [weak self] in
                self?.candidates = merged
            }
        }
        browser.stateUpdateHandler = { newState in
            Task { @MainActor [weak self] in
                guard let self, self.browser === browser else { return }
                switch newState {
                case .ready:
                    self.state = .searching
                case .failed(let error):
                    self.candidates = []
                    self.state = .unavailable(error.localizedDescription)
                    self.browser = nil
                case .cancelled:
                    self.candidates = []
                    self.state = .idle
                    self.browser = nil
                default:
                    break
                }
            }
        }
        browser.start(queue: .global(qos: .userInitiated))
    }

    func stop() {
        let active = browser
        browser = nil
        active?.cancel()
        candidates = []
        state = .idle
    }

    deinit {
        browser?.cancel()
    }
}
