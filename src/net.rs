use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const TIMEOUT: Duration = Duration::from_secs(8);

/// Probe a registry using the same protocol cargo would use.
///
/// `https://` sources get a full TLS handshake plus one HTTPS request for the
/// sparse `config.json`, so the measured time is the real end-to-end latency
/// (DNS + TCP + TLS + HTTP). `http://` mirrors get the plain HTTP request.
pub fn measure_latency(registry_url: &str) -> std::io::Result<Duration> {
    let target = parse(registry_url)?;
    // 证书链构建不计入延迟。
    let config = target.https.then(tls_config);
    let start = Instant::now();

    let tcp = connect(&target.host, target.port, TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let path = probe_path(&target.path);

    let status = match config {
        Some(config) => {
            let server_name = ServerName::try_from(target.host.as_str())
                .map_err(std::io::Error::other)?
                .to_owned();
            let conn = ClientConnection::new(config, server_name).map_err(std::io::Error::other)?;
            let mut stream = StreamOwned::new(conn, tcp);
            request_status(&mut stream, &target.host, &path)?
        }
        None => {
            let mut stream = tcp;
            request_status(&mut stream, &target.host, &path)?
        }
    };

    let elapsed = start.elapsed();
    match status.chars().next() {
        Some('2') | Some('3') => Ok(elapsed),
        _ => Err(std::io::Error::other(format!("HTTP {status}"))),
    }
}

fn tls_config() -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Send one `GET` and return the HTTP status token (e.g. `200`).
fn request_status(
    stream: &mut (impl Read + Write),
    host: &str,
    path: &str,
) -> std::io::Result<String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: cargo-mirror-switcher/0.1\r\n\
         Connection: close\r\n\
         Accept: */*\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;

    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf)?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("000");
    Ok(status.to_string())
}

fn connect(host: &str, port: u16, timeout: Duration) -> std::io::Result<std::net::TcpStream> {
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))?;
    let mut last_err = None;
    for addr in addrs {
        match std::net::TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err
        .unwrap_or_else(|| std::io::Error::other(format!("cannot connect to {host}:{port}"))))
}

/// The sparse registry lives at `{base}/config.json`; request that exact file.
fn probe_path(base: &str) -> String {
    if base.ends_with('/') {
        format!("{base}config.json")
    } else {
        format!("{base}/config.json")
    }
}

struct Target {
    host: String,
    port: u16,
    path: String,
    https: bool,
}

/// Minimal URL parser — only needs to handle the registry URLs we generate.
fn parse(raw: &str) -> std::io::Result<Target> {
    let stripped = raw.strip_prefix("sparse+").unwrap_or(raw);
    let (https, after_scheme) = if let Some(rest) = stripped.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = stripped.strip_prefix("http://") {
        (false, rest)
    } else {
        (true, stripped)
    };

    let (hostpart, path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, "/"),
    };

    let (host, port) = if let Some((h, p)) = hostpart.rsplit_once(':') {
        match p.parse::<u16>() {
            Ok(port) => (
                h.trim_start_matches('[').trim_end_matches(']').to_string(),
                port,
            ),
            Err(_) => (hostpart.to_string(), 443),
        }
    } else {
        (hostpart.to_string(), 443)
    };

    let path = if path.is_empty() { "/" } else { path }.to_string();
    Ok(Target {
        host,
        port,
        path,
        https,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sparse_https() {
        let t = parse("sparse+https://rsproxy.cn/index/").unwrap();
        assert_eq!(t.host, "rsproxy.cn");
        assert_eq!(t.port, 443);
        assert_eq!(t.path, "/index/");
    }

    #[test]
    fn parses_sparse_http_with_port() {
        let t = parse("sparse+http://example.com:8080/foo").unwrap();
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 8080);
        assert_eq!(t.path, "/foo");
    }

    #[test]
    fn defaults_path_to_root() {
        let t = parse("sparse+https://example.com").unwrap();
        assert_eq!(t.path, "/");
        assert_eq!(probe_path(&t.path), "/config.json");
    }

    #[test]
    fn probe_path_appends_config_json() {
        assert_eq!(probe_path("/"), "/config.json");
        assert_eq!(probe_path("/index/"), "/index/config.json");
        assert_eq!(probe_path("/foo"), "/foo/config.json");
    }

    #[test]
    fn detects_https_scheme() {
        assert!(parse("sparse+https://rsproxy.cn/index/").unwrap().https);
        assert!(!parse("sparse+http://example.com:8080/foo").unwrap().https);
    }

    #[test]
    #[ignore = "需要网络访问"]
    fn measures_latency_over_real_https() {
        let latency = measure_latency("sparse+https://rsproxy.cn/index/").unwrap();
        assert!(latency > Duration::ZERO);
    }
}
