//! Host-side HTTP(S) proxy that is the only way out of a sandbox with `policy.network`.
//!
//! The workload sits in a network namespace with loopback only. A helper inside bridges
//! loopback TCP to this proxy's UNIX socket. Every destination is checked against the
//! domain allow list, resolved here, and refused if any resolved address is not public,
//! so allow-listed names cannot be rebound to internal services.
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const HEADER_LIMIT: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_LIMIT: usize = 256;

/// Consulted for destinations outside the allow list (for example to ask the user).
#[async_trait]
pub trait NetworkDecider: Send + Sync {
    async fn decide(&self, host: &str, port: u16) -> bool;
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkDecision {
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    pub reason: String,
}

pub struct NetworkProxy {
    dir: Option<PathBuf>,
    socket: Option<PathBuf>,
    port: Option<u16>,
    task: tokio::task::JoinHandle<()>,
    log: Arc<Mutex<Vec<NetworkDecision>>>,
}

struct Shared {
    allow: Vec<String>,
    decider: Option<Arc<dyn NetworkDecider>>,
    allow_private: bool,
    log: Arc<Mutex<Vec<NetworkDecision>>>,
}

pub fn host_matches(host: &str, pattern: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    if pattern == "*" {
        return true;
    }
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || o[0] >= 240)
}

fn v6_public(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_public(v4);
    }
    let s = ip.segments();
    // 64:ff9b::/96 (NAT64) embeds an IPv4 address.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return v4_public(v4);
    }
    // Accept ordinary globally allocated unicast only. Reject transition,
    // translation and documentation ranges that can encode local IPv4 targets.
    (s[0] & 0xe000) == 0x2000
        && !(s[0] == 0x2001 && (s[1] <= 0x01ff || s[1] == 0x0db8))
        && s[0] != 0x2002
        && !(s[0] == 0x3fff && s[1] < 0x1000)
}

pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => v6_public(v6),
    }
}

impl NetworkProxy {
    #[cfg(unix)]
    pub async fn start(
        allow: Vec<String>,
        decider: Option<Arc<dyn NetworkDecider>>,
    ) -> Result<Self> {
        Self::start_with(allow, decider, false).await
    }

    /// Loopback TCP flavour for hosts without network namespaces (macOS Seatbelt allows
    /// the workload to reach exactly this port and nothing else).
    pub async fn start_tcp(
        allow: Vec<String>,
        decider: Option<Arc<dyn NetworkDecider>>,
    ) -> Result<Self> {
        Self::start_tcp_with(allow, decider, false).await
    }

    #[doc(hidden)]
    pub async fn start_tcp_with(
        allow: Vec<String>,
        decider: Option<Arc<dyn NetworkDecider>>,
        allow_private: bool,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (shared, log) = Shared::new(allow, decider, allow_private);
        let listener = Arc::new(listener);
        let task = tokio::spawn(accept_loop(shared, move || {
            let listener = listener.clone();
            async move { listener.accept().await.map(|(s, _)| s) }
        }));
        Ok(Self {
            dir: None,
            socket: None,
            port: Some(port),
            task,
            log,
        })
    }

    #[cfg(unix)]
    #[doc(hidden)]
    pub async fn start_with(
        allow: Vec<String>,
        decider: Option<Arc<dyn NetworkDecider>>,
        allow_private: bool,
    ) -> Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("layeragent-net-{}", layeragent_protocol::new_id()));
        std::fs::create_dir(&dir)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let socket = dir.join("proxy.sock");
        let listener = tokio::net::UnixListener::bind(&socket).context("binding proxy socket")?;
        let (shared, log) = Shared::new(allow, decider, allow_private);
        let listener = Arc::new(listener);
        let task = tokio::spawn(accept_loop(shared, move || {
            let listener = listener.clone();
            async move { listener.accept().await.map(|(s, _)| s) }
        }));
        Ok(Self {
            dir: Some(dir),
            socket: Some(socket),
            port: None,
            task,
            log,
        })
    }

    /// UNIX socket path (namespace-based sandboxes bind this into the workload).
    pub fn socket(&self) -> &Path {
        self.socket.as_deref().unwrap_or_else(|| Path::new(""))
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Decisions made so far (bounded), for auditing and events.
    pub fn decisions(&self) -> Vec<NetworkDecision> {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Drop for NetworkProxy {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Shared {
    fn new(
        allow: Vec<String>,
        decider: Option<Arc<dyn NetworkDecider>>,
        allow_private: bool,
    ) -> (Arc<Self>, Arc<Mutex<Vec<NetworkDecision>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(Self {
            allow,
            decider,
            allow_private,
            log: log.clone(),
        });
        (shared, log)
    }
}

async fn accept_loop<S, F, Fut>(shared: Arc<Shared>, mut accept: F)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<S>>,
{
    // JoinSet owns every tunnel: dropping the proxy aborts established connections,
    // rather than merely stopping acceptance while detached tasks keep transmitting.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        if connections.len() >= MAX_CONNECTIONS {
            let _ = connections.join_next().await;
            continue;
        }
        tokio::select! {
            incoming = accept() => {
                let Ok(stream) = incoming else { break; };
                let shared = shared.clone();
                connections.spawn(async move {
                    if let Err(e) = tokio::time::timeout(Duration::from_secs(300), serve(stream, &shared)).await {
                        tracing::debug!(error = %e, "proxy connection expired");
                    }
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn authorize(shared: &Shared, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let record = |allowed: bool, reason: &str| {
        if let Ok(mut log) = shared.log.lock()
            && log.len() < LOG_LIMIT
        {
            log.push(NetworkDecision {
                host: host.into(),
                port,
                allowed,
                reason: reason.into(),
            });
        }
    };
    let listed = shared.allow.iter().any(|p| host_matches(host, p));
    let permitted = listed
        || match &shared.decider {
            Some(decider) => decider.decide(host, port).await,
            None => false,
        };
    if !permitted {
        record(false, "destination not allowed");
        bail!("destination {host} is not allowed");
    }
    // Literal addresses and resolved names are both held to the public-address rule.
    let candidates: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port)).await?.collect(),
    };
    let blocked = |a: &SocketAddr| !shared.allow_private && !is_public(a.ip());
    if candidates.is_empty() || candidates.iter().any(blocked) {
        record(false, "resolves to a non-public address");
        bail!("{host} resolves to a non-public address");
    }
    record(true, if listed { "allow list" } else { "approved" });
    Ok(candidates)
}

/// Tries each vetted address in turn, so an unroutable IPv6 record does not hide a working IPv4 one.
async fn connect_any(addresses: &[SocketAddr]) -> Result<TcpStream> {
    let mut last = None;
    for address in addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = Some(anyhow::Error::from(e)),
            Err(_) => last = Some(anyhow::anyhow!("connect timeout")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no addresses to connect to")))
}

fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    // [v6]:port, host:port or host
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').context("bad IPv6 authority")?;
        ensure!(
            tail.is_empty() || tail.starts_with(':'),
            "bad IPv6 authority suffix"
        );
        ensure!(host.parse::<Ipv6Addr>().is_ok(), "invalid IPv6 address");
        let port = tail
            .strip_prefix(':')
            .map_or(Ok(default_port), str::parse)?;
        ensure!(port > 0, "invalid port");
        return Ok((host.into(), port));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse()?),
        _ => (authority, default_port),
    };
    ensure!(
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
        "invalid proxy host"
    );
    ensure!(port > 0, "invalid port");
    Ok((host.into(), port))
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut client: S, shared: &Shared) -> Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        let n = tokio::time::timeout(CONNECT_TIMEOUT, client.read(&mut chunk))
            .await
            .context("proxy header timed out")??;
        ensure!(n > 0, "client closed before sending a request");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            ensure!(pos + 4 <= HEADER_LIMIT, "request header too large");
            break pos + 4;
        }
        ensure!(buf.len() <= HEADER_LIMIT, "request header too large");
    };
    let head = std::str::from_utf8(&buf[..end]).context("non-UTF-8 request header")?;
    let (line, headers) = head.split_once("\r\n").context("malformed request")?;
    let mut parts = line.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    ensure!(
        matches!(version, "HTTP/1.0" | "HTTP/1.1") && parts.next().is_none(),
        "unsupported protocol"
    );
    ensure!(
        !method.is_empty() && method.bytes().all(|b| b.is_ascii_uppercase()),
        "invalid method"
    );
    let mut parsed_headers = Vec::new();
    for line in headers.split("\r\n").filter(|h| !h.is_empty()) {
        let (name, value) = line.split_once(':').context("malformed proxy header")?;
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)),
            "invalid proxy header name"
        );
        ensure!(
            !value.bytes().any(|b| b < 32 && b != b'\t' || b == 127),
            "invalid proxy header value"
        );
        parsed_headers.push((name.to_ascii_lowercase(), value.trim()));
    }
    let length: Vec<_> = parsed_headers
        .iter()
        .filter(|(n, _)| n == "content-length")
        .collect();
    ensure!(length.len() <= 1, "duplicate content length");
    if let Some((_, value)) = length.first() {
        ensure!(value.parse::<u64>().is_ok(), "invalid content length");
    }
    ensure!(
        length.is_empty() || !parsed_headers.iter().any(|(n, _)| n == "transfer-encoding"),
        "ambiguous request framing"
    );
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target, 443)?;
        let addresses = authorize(shared, &host, port).await?;
        let mut upstream = connect_any(&addresses).await?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        // Bytes the client pipelined after the header belong to the tunnel.
        upstream.write_all(&buf[end..]).await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }
    // Plain HTTP in absolute form: METHOD http://host[:port]/path HTTP/1.1
    let url = target
        .strip_prefix("http://")
        .context("only absolute-form http:// or CONNECT requests are accepted")?;
    let (authority, path) = url
        .split_once('/')
        .map_or((url, "/".to_string()), |(a, p)| (a, format!("/{p}")));
    ensure!(!authority.contains('@'), "credentials in URLs are refused");
    let (host, port) = split_authority(authority, 80)?;
    let addresses = authorize(shared, &host, port).await?;
    let mut upstream = connect_any(&addresses).await?;
    let mut request = format!("{method} {path} {version}\r\n");
    // Never let a user-supplied Host select a different virtual destination.
    request.push_str(&format!("Host: {authority}\r\n"));
    let hop_headers = parsed_headers
        .iter()
        .filter(|(n, _)| n == "connection")
        .flat_map(|(_, value)| value.split(',').map(|v| v.trim().to_ascii_lowercase()))
        .collect::<Vec<_>>();
    for (name, value) in parsed_headers {
        if ![
            "host",
            "proxy-connection",
            "proxy-authorization",
            "connection",
        ]
        .contains(&name.as_str())
            && !hop_headers.contains(&name)
        {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    request.push_str("Connection: close\r\n\r\n");
    upstream.write_all(request.as_bytes()).await?;
    upstream.write_all(&buf[end..]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, UnixStream};

    #[test]
    fn domain_patterns() {
        assert!(host_matches("Example.COM.", "example.com"));
        assert!(host_matches("api.example.com", "*.example.com"));
        assert!(!host_matches("example.com", "*.example.com"));
        assert!(!host_matches("evilexample.com", "*.example.com"));
        assert!(host_matches("anything.test", "*"));
    }

    #[test]
    fn non_public_ranges_are_rejected() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001::1",
            "3fff::1",
            "100::1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    async fn through_proxy(proxy: &NetworkProxy, request: &str) -> String {
        let mut stream = UnixStream::connect(proxy.socket()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn unlisted_and_private_destinations_are_refused_without_connecting() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        let proxy = NetworkProxy::start(vec!["127.0.0.1".into(), "example.com".into()], None)
            .await
            .unwrap();
        // Listed, but loopback is never public.
        let r = through_proxy(
            &proxy,
            &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
        )
        .await;
        assert!(!r.contains("200"));
        // Not listed at all.
        let r = through_proxy(&proxy, "CONNECT other.test:443 HTTP/1.1\r\n\r\n").await;
        assert!(!r.contains("200"));
        let decisions = proxy.decisions();
        assert_eq!(decisions.len(), 2);
        assert!(decisions.iter().all(|d| !d.allowed));
    }

    #[tokio::test]
    async fn allowed_connect_and_plain_http_are_tunnelled() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = upstream.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 512];
                    let n = s.read(&mut b).await.unwrap_or(0);
                    let echoed = String::from_utf8_lossy(&b[..n]).into_owned();
                    let _ = s.write_all(format!("echo:{echoed}").as_bytes()).await;
                });
            }
        });
        let proxy = NetworkProxy::start_with(vec!["127.0.0.1".into()], None, true)
            .await
            .unwrap();
        let r = through_proxy(
            &proxy,
            &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\nping"),
        )
        .await;
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(r.contains("echo:ping"), "{r}");
        let r = through_proxy(
            &proxy,
            &format!("GET http://127.0.0.1:{port}/x HTTP/1.1\r\nHost: a\r\nProxy-Authorization: secret\r\n\r\n"),
        )
        .await;
        assert!(r.contains("echo:GET /x HTTP/1.1"), "{r}");
        assert!(!r.contains("secret"));
        assert!(proxy.decisions().iter().all(|d| d.allowed));
    }

    #[tokio::test]
    async fn tcp_flavour_serves_the_same_policy_on_a_loopback_port() {
        let proxy = NetworkProxy::start_tcp(vec!["example.com".into()], None)
            .await
            .unwrap();
        let port = proxy.port().expect("tcp proxy has a port");
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"CONNECT blocked.test:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut out)).await;
        assert!(!String::from_utf8_lossy(&out).contains("200"));
        assert_eq!(proxy.decisions().len(), 1);
        assert!(!proxy.decisions()[0].allowed);
    }

    #[tokio::test]
    async fn dropping_proxy_closes_established_tunnels() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        let proxy = NetworkProxy::start_with(vec!["127.0.0.1".into()], None, true)
            .await
            .unwrap();
        let mut client = UnixStream::connect(proxy.socket()).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let (_upstream, _) = upstream.accept().await.unwrap();
        let mut response = [0; 39];
        client.read_exact(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        drop(proxy);
        let mut byte = [0];
        let n = tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 0, "an established tunnel survived proxy revocation");
    }

    #[tokio::test]
    async fn ambiguous_headers_and_malformed_authorities_never_reach_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        let proxy = NetworkProxy::start_with(vec!["127.0.0.1".into()], None, true)
            .await
            .unwrap();
        for headers in [
            "Content-Length: 1\r\nContent-Length: 2\r\n",
            "Content-Length: 1\r\nTransfer-Encoding: chunked\r\n",
            " Proxy-Authorization: secret\r\n",
            "Bad Header: x\r\n",
        ] {
            let reply = through_proxy(
                &proxy,
                &format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n{headers}\r\n"),
            )
            .await;
            assert!(!reply.contains("200"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), upstream.accept())
                .await
                .is_err()
        );
        assert!(split_authority("[::1]evil", 443).is_err());
        assert!(split_authority("user@example.com", 443).is_err());
        assert!(split_authority("example.com:0", 443).is_err());
    }

    #[tokio::test]
    async fn decider_can_approve_unlisted_hosts_and_socket_is_removed_on_drop() {
        struct Yes;
        #[async_trait]
        impl NetworkDecider for Yes {
            async fn decide(&self, host: &str, _: u16) -> bool {
                host == "127.0.0.1"
            }
        }
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = upstream.accept().await {
                let _ = s.write_all(b"hi").await;
            }
        });
        let proxy = NetworkProxy::start_with(vec![], Some(Arc::new(Yes)), true)
            .await
            .unwrap();
        let r = through_proxy(
            &proxy,
            &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
        )
        .await;
        assert!(r.contains("200") && r.ends_with("hi"), "{r}");
        let dir = proxy.dir.clone().unwrap();
        drop(proxy);
        assert!(!dir.exists());
    }
}
