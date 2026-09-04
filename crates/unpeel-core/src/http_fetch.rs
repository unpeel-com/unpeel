//! Minimal blocking HTTP(S) GET for small manifests — the CLI update check
//! fetching `cli/latest.json` from unpeel.com. Same TLS stack as the relay
//! uplink (rustls + webpki roots), `http://` allowed for tests and local
//! dev servers. Not a general client: no redirects, no keep-alive, response
//! capped at 2 MB.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

/// GET `url` and return the response body. Errors on any non-200 status.
pub fn get(url: &str) -> Result<Vec<u8>, String> {
    get_with_headers(url, &[])
}

/// `get` with extra request headers — the CLI update check uses this to carry
/// its anonymous install id (see `unpeel-cli`'s `update` module).
pub fn get_with_headers(url: &str, extra_headers: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let response = fetch_once(url, extra_headers, MAX_RESPONSE)?;
    if response.status != 200 {
        return Err(format!("http status {}", response.status));
    }
    Ok(response.body)
}

/// Stream a larger artifact (a pinned engine binary) straight to `path`,
/// following up to five redirects (GitHub release assets 302 to a CDN),
/// capping the body at `max_bytes`, and hashing as it goes. The body never
/// exists in memory as a whole: reads go through one 64 KiB buffer into the
/// file, so a worker that installs the engine at start keeps its footprint
/// (the memory benchmark measured a 25 MB `Vec<u8>` held by the install
/// thread before this). Returns `(bytes written, sha256 hex)`. On any error
/// the partial file is removed.
pub fn get_to_file(
    url: &str,
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<(u64, String), String> {
    let result = get_to_file_inner(url, path, max_bytes);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn get_to_file_inner(
    url: &str,
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<(u64, String), String> {
    let mut current = url.to_string();
    for _ in 0..6 {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
        let mut written = 0u64;
        let head = fetch_streaming(&current, &[("Accept", "*/*")], max_bytes, &mut |chunk| {
            file.write_all(chunk)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
            hasher.update(chunk);
            written += chunk.len() as u64;
            Ok(())
        })?;
        match head.status {
            200 => {
                file.sync_all()
                    .map_err(|e| format!("sync {}: {e}", path.display()))?;
                let digest = hasher.finish();
                let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
                return Ok((written, hex));
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = head
                    .location
                    .ok_or_else(|| format!("http status {} without Location", head.status))?;
                current = if location.starts_with("http://") || location.starts_with("https://") {
                    location
                } else {
                    return Err(format!("unsupported relative redirect: {location}"));
                };
            }
            status => return Err(format!("http status {status}")),
        }
    }
    Err("too many redirects".into())
}

struct Head {
    status: u16,
    location: Option<String>,
    chunked: bool,
}

/// Open the connection, send the request, parse the header block, then hand
/// every body byte to `sink` (dechunked when the server chunks) without ever
/// buffering more than one read at a time. Redirect bodies are drained the
/// same way (they are tiny) so the caller can inspect the status.
fn fetch_streaming(
    url: &str,
    extra_headers: &[(&str, &str)],
    max_bytes: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<Head, String> {
    let (secure, host, request, tcp) = open_request(url, extra_headers)?;
    if secure {
        let mut stream = tls_stream(&host, tcp)?;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        stream_response(&mut stream, max_bytes, sink)
    } else {
        let mut stream = tcp;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        stream_response(&mut stream, max_bytes, sink)
    }
}

fn stream_response(
    stream: &mut impl Read,
    max_bytes: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<Head, String> {
    // Header block: read until the terminator; whatever follows it in the
    // same read is the first body bytes.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 65536];
    let split = loop {
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buffer.len() > 64 * 1024 {
            return Err("malformed response: header block too large".into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err("malformed response: no header terminator".into()),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err("malformed response: no header terminator".into())
            }
            Err(e) => return Err(format!("read: {e}")),
        }
    };
    let head_text = String::from_utf8_lossy(&buffer[..split]).into_owned();
    let head = parse_head(&head_text)?;
    let mut body = Vec::new();
    body.extend_from_slice(&buffer[split + 4..]);
    drop(buffer);

    let mut total = 0usize;
    let mut deliver = |bytes: &[u8], total: &mut usize| -> Result<(), String> {
        *total += bytes.len();
        if *total > max_bytes {
            return Err("response too large".into());
        }
        sink(bytes)
    };

    if !head.chunked {
        if !body.is_empty() {
            deliver(&body, &mut total)?;
        }
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => deliver(&chunk[..n], &mut total)?,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(format!("read: {e}")),
            }
        }
        return Ok(head);
    }

    // Chunked: `body` is the undecoded carry-over; decode chunk by chunk,
    // pulling more bytes from the stream only when a size line or a chunk
    // payload is incomplete.
    let mut remaining_in_chunk: Option<usize> = None;
    loop {
        if let Some(remaining) = remaining_in_chunk {
            if remaining == 0 {
                // Consume the CRLF after the payload.
                if body.len() < 2 {
                    if !fill(stream, &mut body, &mut chunk)? {
                        return Err("truncated chunk".into());
                    }
                    continue;
                }
                body.drain(..2);
                remaining_in_chunk = None;
                continue;
            }
            if body.is_empty() && !fill(stream, &mut body, &mut chunk)? {
                return Err("truncated chunk".into());
            }
            let take = remaining.min(body.len());
            deliver(&body[..take], &mut total)?;
            body.drain(..take);
            remaining_in_chunk = Some(remaining - take);
            continue;
        }
        let Some(line_end) = body.windows(2).position(|w| w == b"\r\n") else {
            if body.len() > 1024 {
                return Err("malformed chunk: no size line".into());
            }
            if !fill(stream, &mut body, &mut chunk)? {
                return Err("malformed chunk: no size line".into());
            }
            continue;
        };
        let size_text = String::from_utf8_lossy(&body[..line_end]).into_owned();
        let size = usize::from_str_radix(size_text.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|e| format!("malformed chunk size: {e}"))?;
        body.drain(..line_end + 2);
        if size == 0 {
            return Ok(head);
        }
        remaining_in_chunk = Some(size);
    }
}

/// One more read into `carry`; `Ok(false)` at end of stream.
fn fill(
    stream: &mut impl Read,
    carry: &mut Vec<u8>,
    chunk: &mut [u8; 65536],
) -> Result<bool, String> {
    match stream.read(chunk) {
        Ok(0) => Ok(false),
        Ok(n) => {
            carry.extend_from_slice(&chunk[..n]);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(format!("read: {e}")),
    }
}

fn parse_head(head: &str) -> Result<Head, String> {
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("malformed response: no status code")?;
    let location = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_string())
    });
    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    Ok(Head {
        status,
        location,
        chunked,
    })
}

struct Response {
    status: u16,
    body: Vec<u8>,
}

fn fetch_once(
    url: &str,
    extra_headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<Response, String> {
    let (secure, host, request, tcp) = open_request(url, extra_headers)?;
    let mut raw = Vec::new();
    if secure {
        let mut stream = tls_stream(&host, tcp)?;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        read_to_end_capped(&mut stream, &mut raw, max_bytes)?;
    } else {
        let mut stream = tcp;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        read_to_end_capped(&mut stream, &mut raw, max_bytes)?;
    }
    parse_response(&raw)
}

fn tls_stream(
    host: &str,
    tcp: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("server name: {e}"))?;
    let connection =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| format!("tls: {e}"))?;
    Ok(rustls::StreamOwned::new(connection, tcp))
}

/// Resolve, connect, and build the request line + headers (no send).
fn open_request(
    url: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(bool, String, String, TcpStream), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("unsupported url: {url}"));
    };
    let (host_port, path) = match rest.split_once('/') {
        Some((host_port, path)) => (host_port, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>().map_err(|e| format!("port: {e}"))?,
        ),
        None => (host_port.to_string(), if secure { 443 } else { 80 }),
    };

    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}: no addresses"))?;
    let tcp = TcpStream::connect_timeout(&address, TIMEOUT).map_err(|e| format!("connect: {e}"))?;
    tcp.set_read_timeout(Some(TIMEOUT)).ok();
    tcp.set_write_timeout(Some(TIMEOUT)).ok();

    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: unpeel/{}\r\nConnection: close\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if !extra_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("accept"))
    {
        request.push_str("Accept: application/json\r\n");
    }
    for (name, value) in extra_headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    Ok((secure, host, request, tcp))
}

fn read_to_end_capped(
    stream: &mut impl Read,
    out: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<(), String> {
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > max_bytes {
                    return Err("response too large".into());
                }
            }
            // rustls surfaces a close without close_notify as an error;
            // Connection: close servers routinely skip it — the response is
            // complete (and further validated by the header/body parse).
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(format!("read: {e}")),
        }
    }
}

fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed response: no header terminator")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status_line = head.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("malformed response: no status code")?;
    let body = &raw[split + 4..];
    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok(Response { status, body })
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("malformed chunk: no size line")?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size = usize::from_str_radix(size_text.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|e| format!("malformed chunk size: {e}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_response_parses() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(raw).unwrap().body, b"{\"a\":1}");
    }

    #[test]
    fn chunked_response_reassembles() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap().body, b"{\"a\":1}");
    }

    #[test]
    fn non_200_is_an_error() {
        let raw = b"HTTP/1.1 404 Not Found\r\n\r\nnope";
        assert_eq!(parse_response(raw).unwrap().status, 404);
    }

    fn serve_once(response: Vec<u8>) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 2048];
            let _ = socket.read(&mut buffer);
            socket.write_all(&response).unwrap();
        });
        port
    }

    fn temp_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("unpeel-http-fetch-{tag}-{}", std::process::id()))
    }

    #[test]
    fn get_to_file_streams_a_plain_body_and_hashes_it() {
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        )
        .into_bytes();
        response.extend_from_slice(&payload);
        let port = serve_once(response);
        let path = temp_file("plain");
        let (written, sha) =
            get_to_file(&format!("http://127.0.0.1:{port}/x"), &path, 1 << 20).unwrap();
        assert_eq!(written, payload.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        let expected = ring::digest::digest(&ring::digest::SHA256, &payload);
        let expected_hex: String = expected
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(sha, expected_hex);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_to_file_dechunks_across_reads() {
        // Chunk sizes straddle the 64 KiB read buffer and the last chunk's
        // CRLF lands in its own read.
        let a: Vec<u8> = vec![b'a'; 70_000];
        let b: Vec<u8> = vec![b'b'; 5];
        let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        response.extend_from_slice(format!("{:x}\r\n", a.len()).as_bytes());
        response.extend_from_slice(&a);
        response.extend_from_slice(b"\r\n5\r\n");
        response.extend_from_slice(&b);
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let port = serve_once(response);
        let path = temp_file("chunked");
        let (written, _) =
            get_to_file(&format!("http://127.0.0.1:{port}/x"), &path, 1 << 20).unwrap();
        assert_eq!(written, 70_005);
        let got = std::fs::read(&path).unwrap();
        assert_eq!(&got[..70_000], &a[..]);
        assert_eq!(&got[70_000..], &b[..]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_to_file_enforces_the_cap_and_removes_the_partial_file() {
        let payload = vec![7u8; 10_000];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        )
        .into_bytes();
        response.extend_from_slice(&payload);
        let port = serve_once(response);
        let path = temp_file("cap");
        let err = get_to_file(&format!("http://127.0.0.1:{port}/x"), &path, 4096).unwrap_err();
        assert!(err.contains("too large"), "{err}");
        assert!(!path.exists(), "partial file must be removed");
    }

    #[test]
    fn get_to_file_follows_a_redirect() {
        let final_port = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec());
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{final_port}/asset\r\nContent-Length: 0\r\n\r\n"
        );
        let first_port = serve_once(redirect.into_bytes());
        let path = temp_file("redirect");
        let (written, _) =
            get_to_file(&format!("http://127.0.0.1:{first_port}/x"), &path, 1 << 20).unwrap();
        assert_eq!(written, 3);
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_over_local_http_works() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 2048];
            let _ = socket.read(&mut buffer);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"v\":2}")
                .unwrap();
        });
        let body = get(&format!(
            "http://127.0.0.1:{port}/releases/beta/cli/latest.json"
        ))
        .unwrap();
        assert_eq!(body, b"{\"v\":2}");
    }
}
