//! Detection of local service URLs a session's processes expose.
//!
//! Agents routinely start local servers (dev servers, notebooks, preview
//! tools) whose only announcement is a printed `http://localhost:<port>`
//! line. The host scans the rendered viewport for such URLs (same cadence
//! and screen text as the menu-prompt scan), remembers them as candidates
//! for the session's lifetime, and probes the port before publishing:
//! only URLs whose port currently accepts a loopback TCP connection land
//! in the manifest's `detected_local_urls`, and they drop out again when
//! the server goes away. Printed-but-dead URLs (scrollback ghosts, URLs
//! that were merely discussed) never surface.
//!
//! Rules learned from scanning real sessions:
//! - explicit port required — a bare `http://127.0.0.1` matches whatever
//!   unrelated service owns port 80;
//! - probe both loopbacks — vite and friends bind `[::1]` only, so an
//!   IPv4-only probe reports a live server dead;
//! - viewport text (VT-parsed) is scanned, not raw output bytes, so ANSI
//!   sequences and wrap artifacts cannot mangle a URL mid-match;
//! - "live" means *browsable*: the probe speaks HTTP and requires a
//!   page-like answer (HTML or a redirect), so a TCP service that merely
//!   accepts connections (postgres, redis, a CDP debug port) is never
//!   published as something to open in a browser.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
const HTTP_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_HEAD: usize = 16 * 1024;

/// Candidate URLs seen on a session's screen, keyed by port so the list
/// stays stable (first URL seen for a port wins — later mentions of the
/// same server don't churn the manifest). Candidates persist for the
/// session's lifetime; liveness decides publication, so a dev server
/// restarting on the same port reappears without being reprinted.
#[derive(Debug, Default)]
pub struct LocalUrlTracker {
    candidates: BTreeMap<u16, Candidate>,
    published: Vec<String>,
}

#[derive(Debug)]
struct Candidate {
    url: String,
    port: u16,
    https: bool,
}

impl LocalUrlTracker {
    /// Absorb URLs visible on the current screen. Returns true if a new
    /// candidate port appeared (callers may use this to probe eagerly).
    /// One entry per port: a deep link (`/link`) is kept while it is all
    /// we know, but upgrades to the parent URL the moment a shorter path
    /// for the same server is printed — one server, one entry.
    pub fn observe_screen(&mut self, screen_text: &str) -> bool {
        let mut added = false;
        for (port, url) in extract_local_urls(screen_text) {
            match self.candidates.entry(port) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if url_path_len(&url) < url_path_len(&entry.get().url) {
                        entry.get_mut().url = url;
                    }
                }
                std::collections::btree_map::Entry::Vacant(vacant) => {
                    added = true;
                    vacant.insert(Candidate {
                        https: url.starts_with("https://"),
                        url,
                        port,
                    });
                }
            }
        }
        added
    }

    /// Probe every candidate and return the list of URLs that currently
    /// serve a browsable page, or `None` if it is unchanged since the last
    /// probe. The caller edge-writes the manifest only on `Some`,
    /// mirroring `menu_prompt_active`.
    pub fn probe(&mut self) -> Option<Vec<String>> {
        let live: Vec<String> = self
            .candidates
            .values()
            .filter(|c| url_serves_website(c))
            .map(|c| c.url.clone())
            .collect();
        if live == self.published {
            return None;
        }
        self.published = live.clone();
        Some(live)
    }

    pub fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }
}

/// One-shot re-verification of a single manifest URL against the CURRENT
/// rules. Display layers (app titlebar chip, TUI preview chip) call this
/// before showing a URL: a session host is a long-lived process running
/// whatever detection code it started with, so its manifest may contain
/// URLs an older filter accepted — the reader, which is always current,
/// gets the final say.
pub fn url_is_openable_site(url: &str) -> bool {
    let Some((port, normalized)) = extract_local_urls(url).into_iter().next() else {
        return false;
    };
    let candidate = Candidate {
        https: normalized.starts_with("https://"),
        url: normalized,
        port,
    };
    url_serves_website(&candidate)
}

/// Reduce a URL to its origin with a trailing slash
/// ("http://localhost:5173/whatever" → "http://localhost:5173/"). Display
/// layers group by this so one server never shows twice — a deep link
/// survives only while no parent URL exists for the same origin.
pub fn origin_only(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority_len = rest.find('/').unwrap_or(rest.len());
    let scheme_len = url.len() - rest.len();
    Some(format!("{}{}/", &url[..scheme_len], &rest[..authority_len]))
}

/// Length of the path/query portion, for "which URL is closer to the
/// parent" comparisons (bare origin and origin-with-slash both count 0).
fn url_path_len(url: &str) -> usize {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return usize::MAX;
    };
    match rest.find('/') {
        None => 0,
        Some(at) => rest.len() - at - 1, // "/" alone is the parent: 0
    }
}

/// Collapse a URL list to one entry per origin, preferring the URL closest
/// to the parent. Display layers call this over the aggregated manifest
/// lists so servers announced through several deep links show once.
pub fn dedupe_by_origin(urls: &[String]) -> Vec<String> {
    let mut best: Vec<(String, String)> = Vec::new(); // (origin, url) in first-seen order
    for url in urls {
        let Some(origin) = origin_only(url) else {
            continue;
        };
        match best.iter_mut().find(|(o, _)| *o == origin) {
            Some((_, kept)) => {
                if url_path_len(url) < url_path_len(kept) {
                    *kept = url.clone();
                }
            }
            None => best.push((origin, url.clone())),
        }
    }
    best.into_iter().map(|(_, url)| url).collect()
}

/// The process serving a detected local-site URL, resolved on demand
/// (dropdown open / stop click), never on a timer.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSiteServer {
    pub pid: u32,
    /// Short command name, for display ("node", "python3").
    pub command: String,
    /// The hosted session whose process tree contains the server — the only
    /// case a Stop action is offered. `None` means the server was started
    /// outside Unpeel (the user's own infra): show, never touch.
    pub session_id: Option<String>,
}

/// Resolve which process listens on the URL's port and whether it belongs
/// to a hosted session (an ancestor pid matches a running session manifest).
pub fn server_for_url(url: &str) -> Option<LocalSiteServer> {
    let (port, _) = extract_local_urls(url).into_iter().next()?;
    let pid = listening_pid(port)?;
    let command = process_command(pid).unwrap_or_default();
    Some(LocalSiteServer {
        pid,
        command,
        session_id: owning_session(pid),
    })
}

/// Stop the server behind `url`, but only when it still resolves to a
/// session-owned process at this very moment — resolve-and-kill in one
/// step, so a stale pid can never be signaled (kill paths here follow the
/// same identity discipline as the session-host kill paths).
pub fn stop_server_for_url(url: &str) -> Result<LocalSiteServer, String> {
    let server = server_for_url(url).ok_or("no server is listening on that port")?;
    if server.session_id.is_none() {
        return Err(format!(
            "{} (pid {}) was not started by an Unpeel session",
            server.command, server.pid
        ));
    }
    let rc = unsafe { libc::kill(server.pid as i32, libc::SIGTERM) };
    if rc == 0 {
        Ok(server)
    } else {
        Err(format!("failed to signal pid {}", server.pid))
    }
}

fn listening_pid(port: u16) -> Option<u32> {
    let output = std::process::Command::new("lsof")
        .args(["-t", "-n", "-P", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn process_command(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if comm.is_empty() {
        return None;
    }
    // Full paths read as noise in a menu item; the basename is the name.
    Some(comm.rsplit('/').next().unwrap_or(&comm).to_string())
}

fn parent_pid(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Walk the server's ancestry against the running session manifests: any
/// ancestor matching a manifest's recorded host pid ties the server to
/// that session.
fn owning_session(pid: u32) -> Option<String> {
    let mut session_by_pid = std::collections::HashMap::new();
    let root = crate::app_paths::app_sessions_root();
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let bytes = match std::fs::read(entry.path().join("manifest.json")) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(manifest) =
            serde_json::from_slice::<crate::session_host::HostedSessionManifest>(&bytes)
        else {
            continue;
        };
        if manifest.state == crate::session_host::HostedSessionState::Running {
            if let Some(host_pid) = manifest.pid {
                session_by_pid.insert(host_pid, manifest.session.id.clone());
            }
        }
    }
    let mut current = pid;
    for _ in 0..12 {
        if let Some(session) = session_by_pid.get(&current) {
            return Some(session.clone());
        }
        match parent_pid(current) {
            Some(parent) if parent > 1 => current = parent,
            _ => return None,
        }
    }
    None
}

/// True if the candidate currently answers like a website a browser can
/// open. Plain HTTP gets a real GET whose response head must look like a
/// page (HTML content or a redirect). HTTPS candidates fall back to a TCP
/// liveness check — local dev TLS is almost always self-signed, so a
/// certificate-validating client would reject exactly the servers we are
/// trying to detect.
fn url_serves_website(candidate: &Candidate) -> bool {
    let addrs = [
        SocketAddr::from((Ipv4Addr::LOCALHOST, candidate.port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, candidate.port)),
    ];
    for addr in addrs {
        let Ok(stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
            continue;
        };
        if candidate.https {
            return true;
        }
        if http_head_looks_like_website(stream, &candidate.url) {
            return true;
        }
        // Connected but the HTTP answer wasn't page-like: the port owner is
        // not a website; the other loopback would reach the same process.
        return false;
    }
    false
}

/// Issue a minimal GET for the candidate's path and decide whether a real
/// page came back. "Page" is judged from evidence, not status: an erroring
/// dev server (vite's 500 with its error overlay — and no Content-Type at
/// all) is a site that exists and should be openable, while Chrome's CDP
/// port answering `404` + `text/html` + zero bytes of body is not a site.
/// So: any redirect counts, otherwise there must be actual HTML — sniffed
/// from the body start, or an HTML content type with a non-empty body.
fn http_head_looks_like_website(mut stream: TcpStream, url: &str) -> bool {
    let _ = stream.set_read_timeout(Some(HTTP_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HTTP_TIMEOUT));
    let (host_port, path) = match url.strip_prefix("http://").map(|rest| {
        rest.split_once('/')
            .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")))
    }) {
        Some((host, path)) => (host, path),
        None => return false,
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nAccept: text/html\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    // Read headers plus enough body to sniff for HTML. Stop early once the
    // sniff window is full (or the declared body length has arrived) so a
    // keep-alive server that ignores `Connection: close` doesn't make every
    // probe ride out the read timeout.
    const SNIFF_BODY: usize = 2048;
    let mut data = Vec::new();
    let mut buf = [0u8; 2048];
    while data.len() < MAX_HEAD {
        let header_end = data.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(end) = header_end {
            let body_len = data.len() - (end + 4);
            if body_len >= SNIFF_BODY {
                break;
            }
            let head_text = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
            let declared = head_text.lines().find_map(|l| {
                l.strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            });
            if declared.is_some_and(|len| body_len >= len) {
                break;
            }
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&data);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let mut lines = head.lines();
    let Some(status_line) = lines.next() else {
        return false;
    };
    let mut parts = status_line.split_whitespace();
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) {
        return false;
    }
    let Some(status) = parts.next().and_then(|s| s.parse::<u16>().ok()) else {
        return false;
    };
    if (300..400).contains(&status) {
        return true;
    }
    let sniff = body[..body.len().min(SNIFF_BODY)].to_ascii_lowercase();
    if sniff.contains("<html") || sniff.contains("<!doctype") {
        return true;
    }
    // Header fallback for pages whose markup starts beyond the sniff window:
    // an HTML content type with a body that actually exists.
    let mut html = false;
    let mut empty_body = body.is_empty();
    for line in lines.take_while(|l| !l.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("content-type:") && lower.contains("text/html") {
            html = true;
        }
        if lower.starts_with("content-length:") && lower["content-length:".len()..].trim() == "0" {
            empty_body = true;
        }
    }
    html && !empty_body
}

/// Extract `http(s)` URLs with a loopback authority and an explicit port
/// from rendered screen text. Returns `(port, normalized_url)` pairs.
pub fn extract_local_urls(text: &str) -> Vec<(u16, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_scheme(bytes, i) {
        let (scheme_len, _https) = scheme_at(bytes, pos);
        let rest = &text[pos + scheme_len..];
        i = pos + scheme_len;
        let Some((host_len, port, port_len)) = parse_loopback_authority(rest) else {
            continue;
        };
        let after_port = &rest[host_len + 1 + port_len..];
        let path_len = path_length(after_port);
        let url = &text[pos..pos + scheme_len + host_len + 1 + port_len + path_len];
        let url = url.trim_end_matches(['.', ',', ';', ':', ')', ']', '\'', '"']);
        out.push((port, url.to_string()));
        i = pos + scheme_len + host_len + 1 + port_len + path_len;
    }
    out
}

fn find_scheme(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 7 <= bytes.len() {
        if bytes[i] == b'h'
            && (bytes[i..].starts_with(b"http://") || bytes[i..].starts_with(b"https://"))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn scheme_at(bytes: &[u8], pos: usize) -> (usize, bool) {
    if bytes[pos..].starts_with(b"https://") {
        (8, true)
    } else {
        (7, false)
    }
}

/// Match a loopback host followed by `:<port>`. Returns
/// `(host_len, port, port_digits_len)`.
fn parse_loopback_authority(rest: &str) -> Option<(usize, u16, usize)> {
    const HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "0.0.0.0"];
    let host = HOSTS.iter().find(|h| rest.starts_with(**h))?;
    let after = &rest[host.len()..];
    let after = after.strip_prefix(':')?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    let port: u16 = digits.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.len(), port, digits.len()))
}

/// Length of the URL path/query/fragment run after the port: stop at
/// whitespace or characters that in practice terminate a printed URL.
fn path_length(after_port: &str) -> usize {
    if !after_port.starts_with('/') {
        return 0;
    }
    after_port
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | ')' | ']' | '>' | '`' | '|' | '&')
        })
        .unwrap_or(after_port.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn extracts_plain_localhost_url() {
        let urls = extract_local_urls("  ➜  Local:   http://localhost:5173/\n");
        assert_eq!(urls, vec![(5173, "http://localhost:5173/".to_string())]);
    }

    #[test]
    fn requires_explicit_port() {
        assert!(extract_local_urls("see http://127.0.0.1 for details").is_empty());
        assert!(extract_local_urls("http://localhost/path").is_empty());
    }

    #[test]
    fn ignores_non_loopback_hosts() {
        assert!(extract_local_urls("https://unpeel.com:443/x http://example.com:3000").is_empty());
    }

    #[test]
    fn keeps_path_and_strips_trailing_punctuation() {
        let urls = extract_local_urls("Open (http://localhost:8080/app/index.html).");
        assert_eq!(
            urls,
            vec![(8080, "http://localhost:8080/app/index.html".to_string())]
        );
    }

    #[test]
    fn stops_path_at_shell_operators() {
        let urls = extract_local_urls("curl -s http://localhost:4823/apps/skill.md&&curl -s x");
        assert_eq!(
            urls,
            vec![(4823, "http://localhost:4823/apps/skill.md".to_string())]
        );
    }

    #[test]
    fn finds_multiple_and_ipv6_and_https() {
        let text = "a https://127.0.0.1:8443/ok b http://[::1]:9000 c";
        let urls = extract_local_urls(text);
        assert_eq!(
            urls,
            vec![
                (8443, "https://127.0.0.1:8443/ok".to_string()),
                (9000, "http://[::1]:9000".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_port_zero_and_overlong_ports() {
        assert!(extract_local_urls("http://localhost:0/x").is_empty());
        assert!(extract_local_urls("http://localhost:123456/x").is_empty());
    }

    /// Loopback server answering every connection with a canned response
    /// until dropped.
    struct FakeServer {
        port: u16,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeServer {
        fn spawn(response: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_flag = std::sync::Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut buf = [0u8; 2048];
                            let _ = stream.read(&mut buf);
                            let _ = stream.write_all(response.as_bytes());
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            });
            FakeServer {
                port,
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    const HTML_RESPONSE: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<html></html>";
    const JSON_RESPONSE: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}";
    const REDIRECT_RESPONSE: &str =
        "HTTP/1.1 302 Found\r\nLocation: /login\r\nConnection: close\r\n\r\n";

    #[test]
    fn tracker_publishes_only_live_websites_and_edge_reports() {
        let server = FakeServer::spawn(HTML_RESPONSE);
        let port = server.port;
        let mut tracker = LocalUrlTracker::default();
        let screen = format!("server at http://127.0.0.1:{port}/ and old http://localhost:1/dead");
        assert!(tracker.observe_screen(&screen));
        let published = tracker.probe().expect("first probe reports a change");
        assert_eq!(published, vec![format!("http://127.0.0.1:{port}/")]);
        // Unchanged state → no edge.
        assert!(tracker.probe().is_none());
        // Server goes away → list empties exactly once.
        drop(server);
        assert_eq!(tracker.probe(), Some(Vec::new()));
        assert!(tracker.probe().is_none());
    }

    #[test]
    fn non_html_http_service_is_not_a_website() {
        let server = FakeServer::spawn(JSON_RESPONSE);
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen(&format!("api at http://127.0.0.1:{}/", server.port));
        assert!(tracker.probe().is_none(), "JSON API must not publish");
    }

    #[test]
    fn redirect_counts_as_website() {
        let server = FakeServer::spawn(REDIRECT_RESPONSE);
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen(&format!("app at http://localhost:{}/", server.port));
        assert_eq!(
            tracker.probe(),
            Some(vec![format!("http://localhost:{}/", server.port)])
        );
    }

    const CDP_RESPONSE: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length:0\r\nContent-Type:text/html\r\n\r\n";
    // vite's SSR-error shape: 500, no Content-Type at all, HTML in the body.
    const VITE_500_RESPONSE: &str =
        "HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n<!DOCTYPE html>\n<html><body>error overlay</body></html>";

    #[test]
    fn empty_html_404_is_not_a_website() {
        // Chrome's CDP debug port answers GET / exactly like this.
        let server = FakeServer::spawn(CDP_RESPONSE);
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen(&format!("cdp at http://127.0.0.1:{}/", server.port));
        assert!(tracker.probe().is_none(), "CDP shape must not publish");
    }

    #[test]
    fn erroring_dev_server_with_real_page_is_a_website() {
        // A site that exists but is erroring (vite's 500 + error overlay,
        // no Content-Type header) is exactly what a developer wants to
        // open — the page IS the error report.
        let server = FakeServer::spawn(VITE_500_RESPONSE);
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen(&format!("app at http://localhost:{}/", server.port));
        assert_eq!(
            tracker.probe(),
            Some(vec![format!("http://localhost:{}/", server.port)])
        );
    }

    #[test]
    fn raw_tcp_listener_is_not_a_website() {
        // Accepts connections but never speaks HTTP (postgres/redis/CDP
        // shape): must not publish.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen(&format!("http://127.0.0.1:{port}/"));
        assert!(tracker.probe().is_none());
    }

    #[test]
    fn tracker_keeps_deep_link_until_a_parent_appears() {
        let mut tracker = LocalUrlTracker::default();
        tracker.observe_screen("http://localhost:5173/link");
        assert_eq!(
            tracker.candidates.get(&5173).map(|c| c.url.as_str()),
            Some("http://localhost:5173/link")
        );
        // A later, longer path never displaces the shorter one…
        tracker.observe_screen("http://localhost:5173/link/deeper");
        assert_eq!(
            tracker.candidates.get(&5173).map(|c| c.url.as_str()),
            Some("http://localhost:5173/link")
        );
        // …but the parent URL upgrades the entry the moment it is printed.
        tracker.observe_screen("http://localhost:5173/");
        assert_eq!(
            tracker.candidates.get(&5173).map(|c| c.url.as_str()),
            Some("http://localhost:5173/")
        );
    }

    #[test]
    fn dedupe_by_origin_prefers_the_parent_url() {
        let urls = vec![
            "http://localhost:5173/link".to_string(),
            "http://localhost:5173/".to_string(),
            "http://localhost:3000/only-deep".to_string(),
        ];
        assert_eq!(
            dedupe_by_origin(&urls),
            vec![
                "http://localhost:5173/".to_string(),
                "http://localhost:3000/only-deep".to_string(),
            ]
        );
    }
}
