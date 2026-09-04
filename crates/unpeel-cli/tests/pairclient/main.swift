import Foundation
// Acts as the iPhone: decodes the QR text, seals a pairing request with the
// SHIPPED crypto, posts it, and validates the sealed response.
let code = CommandLine.arguments[1]
guard let payload = RemotePairingCode.decode(code) else {
    print("DECODE FAILED"); exit(1)
}
let device = RemoteDeviceIdentity(id: "test-device-1", name: "Test iPhone", platform: "iOS", appVersion: "9.9.9")
let request = RemotePairingRequest(token: payload.token, device: device)
let plaintext = try! JSONEncoder().encode(request)
let envelope = try! RemotePairingCrypto.seal(
    plaintext, token: payload.token, macID: payload.macID,
    endpoint: payload.endpoint, direction: .request)
var http = URLRequest(url: payload.endpoint.appendingPathComponent("pair"))
http.httpMethod = "POST"
http.setValue("application/json", forHTTPHeaderField: "Content-Type")
http.httpBody = try! JSONEncoder().encode(envelope)
let sem = DispatchSemaphore(value: 0)
var result = "NO RESPONSE"
URLSession.shared.dataTask(with: http) { data, response, error in
    defer { sem.signal() }
    if let error { result = "HTTP ERROR: \(error)"; return }
    guard let data, let status = (response as? HTTPURLResponse)?.statusCode else { return }
    guard status == 200 else {
        result = "STATUS \(status): \(String(data: data, encoding: .utf8) ?? "")"; return
    }
    do {
        let sealed = try JSONDecoder().decode(RemotePairingEnvelope.self, from: data)
        let opened = try RemotePairingCrypto.open(
            sealed, token: payload.token, macID: payload.macID,
            endpoint: payload.endpoint, direction: .response)
        let paired = try JSONDecoder().decode(RemotePairingResponse.self, from: opened)
        guard paired.macID == payload.macID, paired.endpoint == payload.endpoint else {
            result = "IDENTITY MISMATCH"; return
        }
        result = "PAIRED ok deviceID=\(paired.deviceID) tokenLen=\(paired.authToken.count) relay=\(paired.relayCredentials.relayURL) e2eLen=\(Data(base64Encoded: paired.relayCredentials.e2eKeyB64)?.count ?? -1)"
    } catch {
        result = "RESPONSE DECODE FAILED: \(error)"
    }
}.resume()
_ = sem.wait(timeout: .now() + 20)
print(result)
