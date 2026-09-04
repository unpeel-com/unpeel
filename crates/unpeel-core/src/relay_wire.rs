//! Crypto-independent request/response wire contract used by every
//! Controller transport.
//!
//! Direct, SSH stdio, and Link Relay all carry the same HTTP-shaped tunnel
//! envelope. Keeping its DTOs, codecs, and frame budget separate from the
//! native `ring` implementation lets browser Controllers compile the exact
//! shipped protocol instead of growing a JavaScript dialect.

use base64::Engine;

/// Maximum opaque payload accepted by the Relay Worker. The outer
/// `[type][conn id]` data envelope is not part of this budget: the Worker
/// explicitly permits those additional five bytes.
pub const MAX_FRAME_BYTES: usize = 512 * 1024;
/// Every sealed payload carries an eight-byte counter and a 16-byte GCM tag.
pub const AEAD_OVERHEAD_BYTES: usize = 8 + 16;
pub const MAX_SEALED_BYTES: usize = MAX_FRAME_BYTES;
pub const MAX_PLAINTEXT_BYTES: usize = MAX_SEALED_BYTES - AEAD_OVERHEAD_BYTES;

pub const fn sealed_frame_fits(byte_count: usize) -> bool {
    byte_count <= MAX_SEALED_BYTES
}

pub const fn plaintext_frame_fits(byte_count: usize) -> bool {
    byte_count <= MAX_PLAINTEXT_BYTES
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelRequest {
    pub id: u64,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub auth: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Encode the transport-neutral request envelope used inside Relay AEAD,
/// Direct, and SSH stdio framing. Authentication is optional because paired
/// transports carry a device credential while SSH derives owner authority
/// from the remote Unix account and ignores this field.
pub fn encode_tunnel_request(request: &TunnelRequest) -> Vec<u8> {
    let query: serde_json::Map<String, serde_json::Value> = request
        .query
        .iter()
        .map(|(key, value)| (key.clone(), value.clone().into()))
        .collect();
    serde_json::json!({
        "id": request.id,
        "method": request.method,
        "path": request.path,
        "query": query,
        "auth": request.auth,
        "contentType": request.content_type,
        "bodyB64": if request.body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(b64(&request.body))
        },
    })
    .to_string()
    .into_bytes()
}

/// Strict request decoding for owner transports. The legacy Relay helper
/// remains an `Option`, but SSH needs a useful protocol error and must never
/// turn malformed base64 into an empty mutation body.
pub fn parse_tunnel_request_strict(plaintext: &[u8]) -> Result<TunnelRequest, String> {
    let value: serde_json::Value =
        serde_json::from_slice(plaintext).map_err(|_| "request is not valid JSON")?;
    let object = value
        .as_object()
        .ok_or("request envelope must be an object")?;
    let query = match object.get("query") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Object(query)) => query
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or_else(|| "query values must be strings".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("query must be an object".into()),
    };
    let optional_string = |key: &str| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(format!("{key} must be a string")),
        }
    };
    let body = match optional_string("bodyB64")? {
        Some(value) => unb64(&value).ok_or("bodyB64 is not valid base64")?,
        None => Vec::new(),
    };
    Ok(TunnelRequest {
        id: object
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("id must be an unsigned integer")?,
        method: object
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or("method must be a string")?
            .to_owned(),
        path: object
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or("path must be a string")?
            .to_owned(),
        query,
        auth: optional_string("auth")?,
        content_type: optional_string("contentType")?,
        body,
    })
}

/// Decode the permissive v1 Relay dialect used by shipped clients.
pub fn parse_tunnel_request(plaintext: &[u8]) -> Option<TunnelRequest> {
    let value: serde_json::Value = serde_json::from_slice(plaintext).ok()?;
    let query = value
        .get("query")
        .and_then(|q| q.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(TunnelRequest {
        id: value.get("id")?.as_u64()?,
        method: value.get("method")?.as_str()?.to_string(),
        path: value.get("path")?.as_str()?.to_string(),
        query,
        auth: value
            .get("auth")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        content_type: value
            .get("contentType")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        body: value
            .get("bodyB64")
            .and_then(|value| value.as_str())
            .and_then(unb64)
            .unwrap_or_default(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelResponse {
    pub id: u64,
    pub status: u16,
    pub body: Vec<u8>,
}

pub fn parse_tunnel_response(plaintext: &[u8]) -> Result<TunnelResponse, String> {
    let value: serde_json::Value =
        serde_json::from_slice(plaintext).map_err(|_| "response is not valid JSON")?;
    let object = value
        .as_object()
        .ok_or("response envelope must be an object")?;
    let body = match object.get("bodyB64") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::String(value)) => {
            unb64(value).ok_or("bodyB64 is not valid base64")?
        }
        Some(_) => return Err("bodyB64 must be a string".into()),
    };
    let status = object
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or("status must be an unsigned 16-bit integer")?;
    Ok(TunnelResponse {
        id: object
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("id must be an unsigned integer")?,
        status,
        body,
    })
}

pub fn encode_tunnel_response(id: u64, status: u16, body: &[u8]) -> Vec<u8> {
    serde_json::json!({
        "id": id,
        "status": status,
        "bodyB64": if body.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(b64(body))
        },
    })
    .to_string()
    .into_bytes()
}

/// Encode a tunneled response without ever handing an oversized plaintext to
/// the crypto/session/socket pipeline. A large route response becomes a small
/// correlated 413, keeping the transport alive for this and other clients.
pub fn encode_bounded_tunnel_response(id: u64, status: u16, body: &[u8]) -> Vec<u8> {
    let response = encode_tunnel_response(id, status, body);
    if plaintext_frame_fits(response.len()) {
        return response;
    }

    let replacement = encode_tunnel_response(id, 413, br#"{"error":"response too large"}"#);
    assert!(
        plaintext_frame_fits(replacement.len()),
        "413 relay response must fit the plaintext frame budget"
    );
    replacement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_frame_boundaries_include_the_exact_aead_overhead() {
        assert_eq!(AEAD_OVERHEAD_BYTES, 24);
        assert_eq!(MAX_PLAINTEXT_BYTES + AEAD_OVERHEAD_BYTES, MAX_SEALED_BYTES);
        assert!(plaintext_frame_fits(MAX_PLAINTEXT_BYTES));
        assert!(!plaintext_frame_fits(MAX_PLAINTEXT_BYTES + 1));
        assert!(sealed_frame_fits(MAX_SEALED_BYTES));
        assert!(!sealed_frame_fits(MAX_SEALED_BYTES + 1));
    }

    #[test]
    fn tunnel_shapes_match_the_swift_dialect() {
        let request = parse_tunnel_request(
            br#"{"id":4,"method":"GET","path":"/mobile/bootstrap","query":{"a":"1"},"auth":"tok","bodyB64":null}"#,
        )
        .unwrap();
        assert_eq!(request.id, 4);
        assert_eq!(request.path, "/mobile/bootstrap");
        assert_eq!(request.auth.as_deref(), Some("tok"));
        assert_eq!(request.content_type, None);
        let response = encode_tunnel_response(4, 200, b"{}");
        let value: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(value["status"], 200);
        assert_eq!(value["bodyB64"], "e30=");
        assert_eq!(
            parse_tunnel_response(&response).unwrap(),
            TunnelResponse {
                id: 4,
                status: 200,
                body: b"{}".to_vec(),
            }
        );
    }

    #[test]
    fn tunnel_request_encoder_round_trips_binary_and_strictly_rejects_bad_base64() {
        let request = TunnelRequest {
            id: 19,
            method: "POST".into(),
            path: "/mobile/upload-chunk".into(),
            query: vec![("name".into(), "hei 🙂".into())],
            auth: Some("ignored by SSH".into()),
            content_type: Some("image/png".into()),
            body: vec![0, 1, 2, 0xff],
        };
        let decoded = parse_tunnel_request_strict(&encode_tunnel_request(&request)).unwrap();
        assert_eq!(decoded, request);

        assert!(parse_tunnel_request_strict(
            br#"{"id":19,"method":"POST","path":"/mobile/write","bodyB64":"%%%"}"#,
        )
        .is_err());
        assert!(parse_tunnel_request_strict(
            br#"{"id":19,"method":"GET","path":"/mobile/bootstrap","query":{"bad":2}}"#,
        )
        .is_err());
    }

    #[test]
    fn tunnel_request_preserves_shipped_swift_fields() {
        let request = parse_tunnel_request(
            br#"{"id":42,"method":"POST","path":"/mobile/image","query":{"session_id":"s1","kind":"attachment"},"auth":"Bearer full-token","contentType":"image/png","bodyB64":"iVBORw0KGgo="}"#,
        )
        .unwrap();

        assert_eq!(request.id, 42);
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/mobile/image");
        assert!(request.query.contains(&("session_id".into(), "s1".into())));
        assert!(request
            .query
            .contains(&("kind".into(), "attachment".into())));
        assert_eq!(request.auth.as_deref(), Some("Bearer full-token"));
        assert_eq!(request.content_type.as_deref(), Some("image/png"));
        assert_eq!(
            request.body,
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn bounded_tunnel_response_keeps_the_largest_encoding_and_replaces_the_next() {
        let fits = |body_len| {
            plaintext_frame_fits(encode_tunnel_response(91, 200, &vec![b'x'; body_len]).len())
        };
        let mut low = 0;
        let mut high = MAX_PLAINTEXT_BYTES;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if fits(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let largest_body = low;
        assert!(fits(largest_body));
        assert!(!fits(largest_body + 1));

        let boundary = encode_bounded_tunnel_response(91, 200, &vec![b'x'; largest_body]);
        assert_eq!(
            boundary,
            encode_tunnel_response(91, 200, &vec![b'x'; largest_body])
        );

        let replacement = encode_bounded_tunnel_response(91, 200, &vec![b'x'; largest_body + 1]);
        assert!(plaintext_frame_fits(replacement.len()));
        let value: serde_json::Value = serde_json::from_slice(&replacement).unwrap();
        assert_eq!(value["id"], 91);
        assert_eq!(value["status"], 413);
        assert_eq!(
            unb64(value["bodyB64"].as_str().unwrap()).unwrap(),
            br#"{"error":"response too large"}"#
        );
    }
}
