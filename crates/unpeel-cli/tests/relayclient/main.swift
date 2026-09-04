import Foundation
setbuf(stdout, nil)
// Phone-crypto oracle: line-JSON on stdin/stdout, SHIPPED RelayProtocol.
var e2e = Data(); var dev = ""; var eph: RelayHandshake.EphemeralKeyPair?
var salt = Data(); var session: RelayCryptoSession?
while let line = readLine() {
    guard let d = line.data(using: .utf8),
          let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
          let op = o["op"] as? String else { continue }
    var out: [String: Any] = [:]
    switch op {
    case "hello":
        e2e = Data(base64Encoded: o["e2eKeyB64"] as! String)!
        dev = o["deviceID"] as! String
        let k = RelayHandshake.EphemeralKeyPair(); eph = k
        salt = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let hello = RelayClientHello(deviceID: dev, salt: salt, ephemeralPublicKey: k.publicKey)
        out["payloadB64"] = (try! JSONEncoder().encode(hello)).base64EncodedString()
    case "finish":
        let hh = try! JSONDecoder().decode(RelayHostHello.self,
            from: Data(base64Encoded: o["hostHelloB64"] as! String)!)
        let shared = try! RelayHandshake.sharedSecret(
            privateKey: eph!.privateKey, peerPublicKey: hh.ephemeralPublicKey!)
        let mac = RelayHandshake.transcriptMAC(e2eKey: e2e, deviceID: dev,
            clientSalt: salt, hostSalt: hh.salt!,
            clientEphemeralPublicKey: eph!.publicKey, hostEphemeralPublicKey: hh.ephemeralPublicKey!)
        guard RelayHandshake.constantTimeEqual(mac, hh.mac!) else { out["error"] = "MAC MISMATCH"; break }
        session = try! RelayCryptoSession(e2eKey: e2e, sharedSecret: shared,
            clientSalt: salt, hostSalt: hh.salt!, isHost: false)
        let body = (o["bodyB64"] as? String).flatMap { Data(base64Encoded: $0) }
        let req = RelayTunnelRequest(
            id: 1,
            method: "GET",
            path: "/mobile/bootstrap",
            auth: o["auth"] as? String,
            contentType: o["contentType"] as? String,
            body: body
        )
        let requestJSON = try! JSONEncoder().encode(req)
        out["requestJSONB64"] = requestJSON.base64EncodedString()
        out["frameB64"] = (try! session!.seal(requestJSON)).base64EncodedString()
    case "seal":
        guard session != nil else { out["error"] = "NO SESSION"; break }
        let body = (o["bodyB64"] as? String).flatMap { Data(base64Encoded: $0) }
        let req = RelayTunnelRequest(
            id: (o["id"] as? NSNumber)?.uint64Value ?? 0,
            method: o["method"] as! String,
            path: o["path"] as! String,
            query: o["query"] as? [String: String] ?? [:],
            auth: o["auth"] as? String,
            contentType: o["contentType"] as? String,
            body: body
        )
        let requestJSON = try! JSONEncoder().encode(req)
        out["requestJSONB64"] = requestJSON.base64EncodedString()
        out["frameB64"] = (try! session!.seal(requestJSON)).base64EncodedString()
    case "open":
        let pt = try! session!.open(Data(base64Encoded: o["frameB64"] as! String)!)
        out["plaintext"] = String(data: pt, encoding: .utf8) ?? ""
    default: break
    }
    let r = try! JSONSerialization.data(withJSONObject: out)
    print(String(data: r, encoding: .utf8)!)
}
