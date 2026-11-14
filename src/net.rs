use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const TIMEOUT: Duration = Duration::from_secs(8);
/// 第 4 层下载测速的字节上限，避免定时测速产生过多流量。
const DOWNLOAD_MAX_BYTES: usize = 512 * 1024;
/// 用于索引解析与下载测速的示例 crate。
const SAMPLE_CRATE: &str = "serde";

/// 单层探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    /// 前序层失败，本层未执行。
    Skipped,
    /// 通过，附带展示详情（耗时、版本、速度等）。
    Passed(String),
    /// 失败，附带错误信息。
    Failed(String),
}

/// 四层探测的完整结果。
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub tcp: Layer,
    pub http: Layer,
    pub parse: Layer,
    pub download: Layer,
}

impl ProbeReport {
    fn skipped() -> Self {
        Self {
            tcp: Layer::Skipped,
            http: Layer::Skipped,
            parse: Layer::Skipped,
            download: Layer::Skipped,
        }
    }
}

/// 按层级探测一个 registry：
/// 1. TCP 连通
/// 2. HTTP 索引探测（GET config.json）
/// 3. 索引文件解析（拉取 crate 索引项并提取版本，同时从 config.json 提取下载端点）
/// 4. 下载端点速度（从 dl 端点下载示例 crate，计算吞吐）
///
/// 任一层失败时，后续层标记为 `Skipped`。
pub fn probe(registry_url: &str) -> ProbeReport {
    let mut report = ProbeReport::skipped();

    let target = match parse(registry_url) {
        Ok(t) => t,
        Err(e) => {
            report.tcp = Layer::Failed(format!("URL 无效：{e}"));
            return report;
        }
    };

    // 第 1 层：TCP 连通
    let started = Instant::now();
    if let Err(e) = probe_tcp(&target) {
        report.tcp = Layer::Failed(e.to_string());
        return report;
    }
    report.tcp = Layer::Passed(format!("{} ms", started.elapsed().as_millis()));

    // 第 2 层：HTTP 索引探测
    let started = Instant::now();
    let config_body = match probe_config(&target) {
        Ok(body) => body,
        Err(e) => {
            report.http = Layer::Failed(e.to_string());
            return report;
        }
    };
    report.http = Layer::Passed(format!("{} ms", started.elapsed().as_millis()));

    // 第 3 层：索引文件解析
    let started = Instant::now();
    let dl = match json_string_field(&config_body, "dl") {
        Some(dl) => dl,
        None => {
            report.parse = Layer::Failed("config.json 缺少 dl 字段".into());
            return report;
        }
    };
    let version = match probe_index_version(&target, SAMPLE_CRATE) {
        Ok(v) => v,
        Err(e) => {
            report.parse = Layer::Failed(e.to_string());
            return report;
        }
    };
    report.parse = Layer::Passed(format!("{version} · {} ms", started.elapsed().as_millis()));

    // 第 4 层：下载端点速度
    match download_speed(&dl, SAMPLE_CRATE, &version) {
        Ok((elapsed, speed)) => {
            report.download =
                Layer::Passed(format!("{} ms · {}", elapsed.as_millis(), format_speed(speed)));
        }
        Err(e) => report.download = Layer::Failed(e.to_string()),
    }

    report
}

fn probe_tcp(target: &Target) -> std::io::Result<()> {
    let _ = connect(&target.host, target.port, TIMEOUT)?;
    Ok(())
}

/// 请求 sparse registry 的 `config.json`，返回响应正文。
fn probe_config(target: &Target) -> std::io::Result<String> {
    let resp = http_get(target, &join_path(&target.path, "config.json"), 256 * 1024)?;
    ensure_success(&resp.status)?;
    Ok(String::from_utf8_lossy(&resp.body).to_string())
}

/// 拉取 crate 的 sparse 索引项并解析出最新版本号。
fn probe_index_version(target: &Target, krate: &str) -> std::io::Result<String> {
    let resp = http_get(
        target,
        &join_path(&target.path, &index_path(krate)),
        512 * 1024,
    )?;
    ensure_success(&resp.status)?;
    let body = String::from_utf8_lossy(&resp.body);
    parse_latest_version(&body).ok_or_else(|| std::io::Error::other("索引文件解析失败"))
}

/// 从下载端点拉取示例 crate，返回（耗时, 吞吐 字节/秒）。
fn download_speed(dl: &str, krate: &str, version: &str) -> std::io::Result<(Duration, f64)> {
    let url = download_url(dl, krate, version);
    let target = parse(&url)?;
    let started = Instant::now();
    let resp = http_get(&target, &target.path, DOWNLOAD_MAX_BYTES)?;
    let elapsed = started.elapsed();
    ensure_success(&resp.status)?;
    if resp.body.is_empty() {
        return Err(std::io::Error::other("下载内容为空"));
    }
    let secs = elapsed.as_secs_f64().max(0.001);
    Ok((elapsed, resp.body.len() as f64 / secs))
}

fn ensure_success(status: &str) -> std::io::Result<()> {
    match status.chars().next() {
        Some('2') | Some('3') => Ok(()),
        _ => Err(std::io::Error::other(format!("HTTP {status}"))),
    }
}

fn format_speed(bytes_per_sec: f64) -> String {
    let kb = bytes_per_sec / 1024.0;
    if kb >= 1024.0 {
        format!("{:.1} MB/s", kb / 1024.0)
    } else {
        format!("{kb:.0} KB/s")
    }
}

/// sparse 索引项路径：serde -> se/rd/serde。
fn index_path(krate: &str) -> String {
    match krate.len() {
        0 => krate.to_string(),
        1 => format!("1/{krate}"),
        2 => format!("2/{krate}"),
        3 => format!("3/{}/{}", &krate[..1], krate),
        _ => format!("{}/{}/{}", &krate[..2], &krate[2..4], krate),
    }
}

/// 从 index 文件（每行一个 JSON）中取最后一行的 `vers`。
fn parse_latest_version(body: &str) -> Option<String> {
    body.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| json_string_field(line, "vers"))
}

/// 极简 JSON 字符串字段提取（仅用于我们已知结构的响应）。
fn json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 根据 config.json 的 `dl` 端点拼出某个 crate 版本的下载 URL。
fn download_url(dl: &str, krate: &str, version: &str) -> String {
    if dl.contains("{crate}") || dl.contains("{version}") {
        dl.replace("{crate}", krate).replace("{version}", version)
    } else {
        format!("{}/{krate}/{version}/download", dl.trim_end_matches('/'))
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

struct Response {
    status: String,
    body: Vec<u8>,
}

/// 发送一个 `GET` 请求并读取响应（最多 `max_bytes` 字节）。
fn http_get(target: &Target, path: &str, max_bytes: usize) -> std::io::Result<Response> {
    let tcp = connect(&target.host, target.port, TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: cargo-mirror-switcher/0.1\r\n\
         Accept: */*\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\r\n",
        target.host
    );

    let raw = if target.https {
        let config = tls_config();
        let server_name = ServerName::try_from(target.host.as_str())
            .map_err(std::io::Error::other)?
            .to_owned();
        let conn = ClientConnection::new(config, server_name).map_err(std::io::Error::other)?;
        let mut stream = StreamOwned::new(conn, tcp);
        stream.write_all(request.as_bytes())?;
        read_all(&mut stream, max_bytes)?
    } else {
        let mut stream = tcp;
        stream.write_all(request.as_bytes())?;
        read_all(&mut stream, max_bytes)?
    };

    parse_response(&raw)
}

fn read_all(stream: &mut impl Read, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() >= max_bytes {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

fn parse_response(raw: &[u8]) -> std::io::Result<Response> {
    let split = find_subsequence(raw, b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("HTTP 响应格式错误"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("000")
        .to_string();

    let lower = head.to_ascii_lowercase();
    let chunked = lower
        .lines()
        .any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
    if chunked {
        body = decode_chunked(&body);
    }

    Ok(Response { status, body })
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn decode_chunked(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let line_end = match find_subsequence(&body[i..], b"\r\n") {
            Some(p) => i + p,
            None => break,
        };
        let size_line = String::from_utf8_lossy(&body[i..line_end]);
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = match usize::from_str_radix(size_hex, 16) {
            Ok(s) => s,
            Err(_) => break,
        };
        if size == 0 {
            break;
        }
        let data_start = line_end + 2;
        let data_end = (data_start + size).min(body.len());
        out.extend_from_slice(&body[data_start..data_end]);
        i = data_end + 2;
    }
    out
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

/// 拼接 registry 基础路径与相对资源路径。
fn join_path(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        format!("/{rel}")
    } else {
        format!("{base}/{rel}")
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
    fn detects_https_scheme() {
        assert!(parse("sparse+https://rsproxy.cn/index/").unwrap().https);
        assert!(!parse("sparse+http://example.com:8080/foo").unwrap().https);
    }

    #[test]
    fn joins_base_and_relative_path() {
        assert_eq!(join_path("/", "config.json"), "/config.json");
        assert_eq!(join_path("/index/", "config.json"), "/index/config.json");
        assert_eq!(join_path("", "config.json"), "/config.json");
    }

    #[test]
    fn builds_sparse_index_paths() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("serde"), "se/rd/serde");
    }

    #[test]
    fn extracts_json_string_field() {
        let json = r#"{"dl":"https://static.crates.io/crates","api":"https://crates.io"}"#;
        assert_eq!(
            json_string_field(json, "dl").as_deref(),
            Some("https://static.crates.io/crates")
        );
        assert_eq!(json_string_field(json, "missing"), None);
    }

    #[test]
    fn parses_latest_version_from_last_line() {
        let body = "{\"vers\":\"1.0.0\"}\n{\"vers\":\"1.0.1\"}\n";
        assert_eq!(parse_latest_version(body).as_deref(), Some("1.0.1"));
    }

    #[test]
    fn builds_download_url_without_markers() {
        assert_eq!(
            download_url("https://static.crates.io/crates", "serde", "1.0.0"),
            "https://static.crates.io/crates/serde/1.0.0/download"
        );
    }

    #[test]
    fn builds_download_url_with_markers() {
        assert_eq!(
            download_url(
                "https://example.com/{crate}/{version}/download",
                "serde",
                "1.0.0"
            ),
            "https://example.com/serde/1.0.0/download"
        );
    }

    #[test]
    fn formats_speed_in_kb_and_mb() {
        assert_eq!(format_speed(500.0 * 1024.0), "500 KB/s");
        assert_eq!(format_speed(2.5 * 1024.0 * 1024.0), "2.5 MB/s");
    }

    #[test]
    fn decodes_chunked_body() {
        let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(raw), b"hello world");
    }

    #[test]
    #[ignore = "需要网络访问"]
    fn probes_real_registry() {
        let report = probe("sparse+https://rsproxy.cn/index/");
        matches!(report.tcp, Layer::Passed(_));
    }
}
