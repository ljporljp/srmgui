use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const TIMEOUT: Duration = Duration::from_secs(8);
/// 第 4 层下载测速的字节上限，避免定时测速产生过多流量。
const DOWNLOAD_MAX_BYTES: usize = 512 * 1024;
/// 最多的重定向跟随次数。
const MAX_REDIRECTS: usize = 5;
/// 更新安装包允许的最大下载字节数。
const MAX_INSTALLER_BYTES: usize = 256 * 1024 * 1024;
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
/// 3. 解析 config.json 的下载端点并拉取 crate 索引项提取版本
/// 4. 下载端点速度（从 dl 端点下载示例 crate，计算吞吐）
///
/// 任一层失败时，后续层标记为 `Skipped`。
pub fn probe(registry_url: &str) -> ProbeReport {
    let mut report = ProbeReport::skipped();

    let target = match parse(registry_url) {
        Ok(t) => t,
        Err(e) => {
            report.tcp = Layer::Failed(
                rust_i18n::t!("net.url_invalid", error = e.to_string()).to_string(),
            );
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

    // 第 3 层：解析 config.json 并读取索引
    let started = Instant::now();
    let dl = match download_base(&config_body) {
        Some(dl) => dl,
        None => {
            report.parse = Layer::Failed(rust_i18n::t!("net.missing_dl").to_string());
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
    let resp = http_get(
        target,
        &join_path(&target.path, "config.json"),
        "text/plain",
        256 * 1024,
    )?;
    ensure_success(&resp.status)?;
    Ok(String::from_utf8_lossy(&resp.body).to_string())
}

/// 拉取 crate 的 sparse 索引项并解析出最新版本号。
fn probe_index_version(target: &Target, krate: &str) -> std::io::Result<String> {
    let resp = http_get(
        target,
        &join_path(&target.path, &index_path(krate)),
        "text/plain",
        512 * 1024,
    )?;
    ensure_success(&resp.status)?;
    let body = String::from_utf8_lossy(&resp.body);
    parse_latest_version(&body).ok_or_else(|| {
        std::io::Error::other(rust_i18n::t!("net.index_parse_failed").to_string())
    })
}

/// 从下载端点拉取示例 crate，返回（耗时, 吞吐 字节/秒）。
fn download_speed(dl: &str, krate: &str, version: &str) -> std::io::Result<(Duration, f64)> {
    let url = download_url(dl, krate, version);
    let target = parse(&url)?;
    let started = Instant::now();
    let resp = http_get(&target, &target.path, "*/*", DOWNLOAD_MAX_BYTES)?;
    let elapsed = started.elapsed();
    ensure_success(&resp.status)?;
    if resp.body.is_empty() {
        return Err(std::io::Error::other(
            rust_i18n::t!("net.empty_download").to_string(),
        ));
    }
    let secs = elapsed.as_secs_f64().max(0.001);
    Ok((elapsed, resp.body.len() as f64 / secs))
}

fn ensure_success(status: &str) -> std::io::Result<()> {
    match status.chars().next() {
        Some('2') => Ok(()),
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

/// 从 index 文件（每行一个 JSON）中取最后一条未 yank 的 `vers`。
fn parse_latest_version(body: &str) -> Option<String> {
    body.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| json_bool_field(line, "yanked") != Some(true))
        .find_map(|line| json_string_field(line, "vers"))
}

/// 获取任意 URL 的响应（状态码 + 正文），供更新检查等通用场景使用。
pub fn fetch(url: &str, accept: &str, max_bytes: usize) -> std::io::Result<(String, Vec<u8>)> {
    let target = parse(url)?;
    let resp = http_get(&target, &target.path, accept, max_bytes)?;
    Ok((resp.status, resp.body))
}

/// 下载 URL 到本地文件，返回写入的字节数。
pub fn download(url: &str, path: &std::path::Path) -> std::io::Result<u64> {
    let (status, body) = fetch(url, "*/*", MAX_INSTALLER_BYTES)?;
    ensure_success(&status)?;
    if body.is_empty() {
        return Err(std::io::Error::other(
            rust_i18n::t!("net.empty_download").to_string(),
        ));
    }
    std::fs::write(path, &body)?;
    Ok(body.len() as u64)
}

/// 极简 JSON 字符串字段提取（仅用于我们已知结构的响应）。
///
/// registry 的 `config.json` 带缩进（官方源即为多行格式），因此这里按
/// JSON 语法容忍键、冒号、值之间的空白，而不是只匹配紧凑格式。
pub(crate) fn json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(pos) = json[from..].find(&needle) {
        let after_key = from + pos + needle.len();
        let rest = json[after_key..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            if let Some(rest) = rest.trim_start().strip_prefix('"') {
                return parse_json_string(rest);
            }
        }
        from = after_key;
    }
    None
}

/// 极简 JSON 布尔字段提取。
fn json_bool_field(json: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{key}\"");
    let pos = json.find(&needle)? + needle.len();
    let rest = json[pos..].trim_start().strip_prefix(':')?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// 读取 JSON 字符串内容（入参不含起始引号），并处理转义。
fn parse_json_string(raw: &str) -> Option<String> {
    scan_json_string(raw).map(|(value, _)| value)
}

/// 读取 JSON 字符串内容（入参不含起始引号），返回内容与消耗的字节数
/// （包含结束引号），便于调用方继续扫描后续字段。
pub(crate) fn scan_json_string(raw: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = raw.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((out, index + 1)),
            '\\' => match chars.next()?.1 {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// 解析上传/下载端点：优先 `dl`；按 sparse 协议 `dl` 可省略，
/// 此时由 `api` 派生 `{api}/api/v1/crates`。
fn download_base(config: &str) -> Option<String> {
    if let Some(dl) = json_string_field(config, "dl") {
        return Some(dl);
    }
    let api = json_string_field(config, "api")?;
    Some(format!("{}/api/v1/crates", api.trim_end_matches('/')))
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
    // 追加系统证书库，兼容企业代理/自建根证书（GitHub API 等可能被其拦截）。
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
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
    location: Option<String>,
    body: Vec<u8>,
}

/// 发送一个 `GET` 请求并读取响应（最多 `max_bytes` 字节），自动跟随重定向。
fn http_get(
    target: &Target,
    path: &str,
    accept: &str,
    max_bytes: usize,
) -> std::io::Result<Response> {
    let mut host = target.host.clone();
    let mut port = target.port;
    let mut https = target.https;
    let mut path = path.to_string();

    for _ in 0..=MAX_REDIRECTS {
        let resp = http_get_once(&host, port, https, &path, accept, max_bytes)?;
        if !is_redirect(&resp.status) {
            return Ok(resp);
        }
        let location = resp
            .location
            .as_deref()
            .ok_or_else(|| {
                std::io::Error::other(rust_i18n::t!("net.missing_location").to_string())
            })?;
        let (next_host, next_port, next_https, next_path) =
            resolve_location(&host, port, https, &path, location)?;
        host = next_host;
        port = next_port;
        https = next_https;
        path = next_path;
    }
    Err(std::io::Error::other(
        rust_i18n::t!("net.too_many_redirects", max = MAX_REDIRECTS).to_string(),
    ))
}

fn is_redirect(status: &str) -> bool {
    status.starts_with('3')
}

/// 解析 `Location`，支持绝对 URL 与相对当前请求路径的位置。
fn resolve_location(
    host: &str,
    port: u16,
    https: bool,
    current_path: &str,
    location: &str,
) -> std::io::Result<(String, u16, bool, String)> {
    if location.starts_with("http://") || location.starts_with("https://") {
        let t = parse(location)?;
        return Ok((t.host, t.port, t.https, t.path));
    }
    let path = if location.starts_with('/') {
        location.to_string()
    } else {
        let dir = match current_path.rfind('/') {
            Some(i) => &current_path[..=i],
            None => "/",
        };
        format!("{dir}{location}")
    };
    Ok((host.to_string(), port, https, path))
}

fn http_get_once(
    host: &str,
    port: u16,
    https: bool,
    path: &str,
    accept: &str,
    max_bytes: usize,
) -> std::io::Result<Response> {
    let host_header = if (https && port == 443) || (!https && port == 80) {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         User-Agent: cargo-mirror-switcher/0.1\r\n\
         Accept: {accept}\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\r\n"
    );

    let raw = if https {
        https_get(host, port, &request, max_bytes)?
    } else {
        let mut stream = tcp_stream(host, port)?;
        stream.write_all(request.as_bytes())?;
        read_all(&mut stream, max_bytes)?
    };

    parse_response(&raw)
}

fn tcp_stream(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let tcp = connect(host, port, TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    Ok(tcp)
}

fn https_get(
    host: &str,
    port: u16,
    request: &str,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let tcp = tcp_stream(host, port)?;
    let server_name = ServerName::try_from(host)
        .map_err(std::io::Error::other)?
        .to_owned();
    let conn = ClientConnection::new(tls_config(), server_name).map_err(std::io::Error::other)?;
    let mut stream = StreamOwned::new(conn, tcp);
    stream.write_all(request.as_bytes())?;
    read_all(&mut stream, max_bytes)
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
            // 部分服务端（如 GitHub API 前置代理）不发送 TLS close_notify 就断开，
            // 此时已收到的数据是完整的，按正常结束处理。
            Err(e)
                if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() =>
            {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

fn parse_response(raw: &[u8]) -> std::io::Result<Response> {
    let split = find_subsequence(raw, b"\r\n\r\n").ok_or_else(|| {
        std::io::Error::other(rust_i18n::t!("net.bad_response").to_string())
    })?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut body = raw[split + 4..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("000")
        .to_string();

    let location = head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_string())
    });

    let lower = head.to_ascii_lowercase();
    let chunked = lower
        .lines()
        .any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
    if chunked {
        body = decode_chunked(&body);
    }

    Ok(Response {
        status,
        location,
        body,
    })
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
    fn extracts_json_string_field_from_pretty_json() {
        let json = "{\n  \"dl\": \"https://static.crates.io/crates\",\n  \"api\": \"https://crates.io\"\n}";
        assert_eq!(
            json_string_field(json, "dl").as_deref(),
            Some("https://static.crates.io/crates")
        );
        assert_eq!(
            json_string_field(json, "api").as_deref(),
            Some("https://crates.io")
        );
    }

    #[test]
    fn derives_download_base_from_api_when_dl_missing() {
        let json = "{\n  \"api\": \"https://crates.io/\"\n}";
        assert_eq!(
            download_base(json).as_deref(),
            Some("https://crates.io/api/v1/crates")
        );
        assert_eq!(download_base("{}"), None);
    }

    #[test]
    fn parses_latest_version_from_last_line() {
        let body = "{\"vers\":\"1.0.0\",\"yanked\":false}\n{\"vers\":\"1.0.1\",\"yanked\":false}\n";
        assert_eq!(parse_latest_version(body).as_deref(), Some("1.0.1"));
    }

    #[test]
    fn skips_yanked_versions() {
        let body = "{\"vers\":\"1.0.0\",\"yanked\":false}\n{\"vers\":\"1.0.1\",\"yanked\":true}\n";
        assert_eq!(parse_latest_version(body).as_deref(), Some("1.0.0"));
    }

    #[test]
    fn resolves_relative_redirects() {
        let (host, port, https, path) =
            resolve_location("example.com", 443, true, "/index/config.json", "/cdn/a").unwrap();
        assert_eq!(
            (host.as_str(), port, https, path.as_str()),
            ("example.com", 443, true, "/cdn/a")
        );

        let (_, _, _, path) =
            resolve_location("example.com", 443, true, "/index/config.json", "a/b").unwrap();
        assert_eq!(path, "/index/a/b");
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
