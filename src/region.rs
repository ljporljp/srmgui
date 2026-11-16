//! 出口 IP 地区识别（国家 + 省份）与本地镜像源推荐。
//!
//! - 启动时向多个轻量 IP 归属地接口查询国家代码与省份，任一成功即可；
//! - 按地区代码筛出可选源，再按最近一次测速结果（下载速度优先、同省优先）推荐；
//! - 尚无测速结果时退化为同省源、内置优先级；
//! - 识别结果持久化到设置文件，离线启动时仍能给出推荐。

use std::collections::HashMap;

use crate::manifest::ManifestSource;
use crate::model::{Level, Probe};
use crate::net;

/// 地区接口响应允许的最大字节数。
const MAX_BODY_BYTES: usize = 8 * 1024;

/// 按顺序尝试的归属地接口：优先返回国家 + 省份，最后回退到仅国家。
const ENDPOINTS: [&str; 3] = [
    "https://ipinfo.io/json",
    "https://myip.ipip.net/json",
    "https://api.country.is/",
];

/// 中国大陆地区代码。
pub(crate) const REGION_CN: &str = "CN";
/// 其余地区统一使用全球源。
pub(crate) const REGION_GLOBAL: &str = "GLOBAL";

/// 中国大陆的兜底优先级：无测速结果时按此顺序挑选。
const CN_PRIORITY: [&str; 14] = [
    "rsproxy", "aliyun", "ustc", "tuna", "sjtu", "huawei", "cernet", "nju", "bfsu", "sdu", "hust",
    "zju", "neusoft", "nyist",
];

/// 中国大陆以外地区的推荐优先级。
const GLOBAL_PRIORITY: [&str; 1] = ["official"];

/// 识别到的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Location {
    /// ISO 3166-1 alpha-2 国家代码，大写。
    pub(crate) country: String,
    /// 省份代码（ISO 3166-2:CN 的后两位），仅中国大陆有值。
    pub(crate) province: Option<String>,
    /// 接口返回的省份原始名称（如 `浙江` / `Zhejiang`）。
    pub(crate) province_name: Option<String>,
}

/// 中国省级行政区：代码、中文名、英文名与英文别名。
struct Province {
    code: &'static str,
    zh: &'static str,
    en: &'static str,
    aliases: &'static [&'static str],
}

macro_rules! province {
    ($code:literal, $zh:literal, $en:literal) => {
        Province {
            code: $code,
            zh: $zh,
            en: $en,
            aliases: &[],
        }
    };
    ($code:literal, $zh:literal, $en:literal, $aliases:expr) => {
        Province {
            code: $code,
            zh: $zh,
            en: $en,
            aliases: $aliases,
        }
    };
}

/// 34 个省级行政区，顺序与常见行政区划顺序一致。
const PROVINCES: [Province; 34] = [
    province!("BJ", "北京", "Beijing"),
    province!("TJ", "天津", "Tianjin"),
    province!("HE", "河北", "Hebei"),
    province!("SX", "山西", "Shanxi"),
    province!("NM", "内蒙古", "Inner Mongolia", &["Nei Mongol"]),
    province!("LN", "辽宁", "Liaoning"),
    province!("JL", "吉林", "Jilin"),
    province!("HL", "黑龙江", "Heilongjiang"),
    province!("SH", "上海", "Shanghai"),
    province!("JS", "江苏", "Jiangsu"),
    province!("ZJ", "浙江", "Zhejiang"),
    province!("AH", "安徽", "Anhui"),
    province!("FJ", "福建", "Fujian"),
    province!("JX", "江西", "Jiangxi"),
    province!("SD", "山东", "Shandong"),
    province!("HA", "河南", "Henan"),
    province!("HB", "湖北", "Hubei"),
    province!("HN", "湖南", "Hunan"),
    province!("GD", "广东", "Guangdong"),
    province!("GX", "广西", "Guangxi", &["Guangxi Zhuang"]),
    province!("HI", "海南", "Hainan"),
    province!("CQ", "重庆", "Chongqing"),
    province!("SC", "四川", "Sichuan"),
    province!("GZ", "贵州", "Guizhou"),
    province!("YN", "云南", "Yunnan"),
    province!("XZ", "西藏", "Tibet", &["Xizang"]),
    province!("SN", "陕西", "Shaanxi"),
    province!("GS", "甘肃", "Gansu"),
    province!("QH", "青海", "Qinghai"),
    province!("NX", "宁夏", "Ningxia", &["Ningxia Hui"]),
    province!("XJ", "新疆", "Xinjiang", &["Xinjiang Uyghur"]),
    province!("TW", "台湾", "Taiwan"),
    province!("HK", "香港", "Hong Kong", &["Hongkong"]),
    province!("MO", "澳门", "Macau", &["Macao"]),
];

/// 识别当前出口 IP 的国家与省份，全部接口失败时返回 `None`。
pub(crate) fn detect_location() -> Option<Location> {
    ENDPOINTS.iter().find_map(|url| fetch_location(url))
}

fn fetch_location(url: &str) -> Option<Location> {
    let (status, body) = net::fetch(url, "*/*", MAX_BODY_BYTES).ok()?;
    if !status.starts_with('2') {
        return None;
    }
    parse_location(&String::from_utf8_lossy(&body))
}

/// 解析归属地响应，兼容三种格式：
/// - `ipinfo.io/json`：`{"country":"CN","region":"Zhejiang",...}`
/// - `myip.ipip.net/json`：`{"data":{"location":["中国","浙江","杭州","","电信"]}}`
/// - `api.country.is/`：`{"ip":"...","country":"US"}` 或纯文本国家代码
fn parse_location(text: &str) -> Option<Location> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    if let Some(country) = json_field(text, "country").filter(|value| is_country_code(value.trim()))
    {
        let province_name = json_field(text, "region").filter(|value| !value.trim().is_empty());
        return Some(build_location(&country, province_name));
    }

    if let Some(items) = ipip_location(text) {
        if let Some(country) = items.first().and_then(|name| country_from_name(name)) {
            return Some(build_location(&country, items.get(1).cloned()));
        }
    }

    parse_country(text).map(|country| build_location(&country, None))
}

/// 组装位置信息：仅中国大陆把省份名归一为省份代码。
fn build_location(country: &str, province_name: Option<String>) -> Location {
    let country = country.trim().to_ascii_uppercase();
    let province_name = province_name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    let province = province_name
        .as_deref()
        .filter(|_| country == REGION_CN)
        .and_then(match_province)
        .map(|province| province.code.to_string());
    Location {
        country,
        province,
        province_name,
    }
}

/// 解析国家代码：兼容纯文本（`CN`）与 JSON（`{"country":"CN"}`）两种响应。
fn parse_country(text: &str) -> Option<String> {
    let text = text.trim();
    if is_country_code(text) {
        return Some(text.to_ascii_uppercase());
    }
    json_field(text, "country")
        .map(|value| value.trim().to_string())
        .filter(|value| is_country_code(value))
        .map(|value| value.to_ascii_uppercase())
}

fn is_country_code(text: &str) -> bool {
    text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// 从 JSON 文本中提取字符串字段（仅支持扁平字段，足够解析归属地接口）。
fn json_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let (value, _) = net::scan_json_string(rest)?;
    Some(value)
}

/// 解析 `myip.ipip.net` 的 `location` 字符串数组。
fn ipip_location(text: &str) -> Option<Vec<String>> {
    let key = "\"location\"";
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    let start = rest.find('[')? + 1;
    let mut rest = &rest[start..];
    let mut items = Vec::new();
    loop {
        rest = rest.trim_start().trim_start_matches(',').trim_start();
        let value = rest.strip_prefix('"')?;
        let (item, consumed) = net::scan_json_string(value)?;
        items.push(item);
        rest = &value[consumed..];
        if rest.trim_start().starts_with(']') {
            break;
        }
    }
    Some(items)
}

/// 中文国家名归一为国家代码，兼容 `中国香港` 等写法。
fn country_from_name(name: &str) -> Option<String> {
    let name = name.trim();
    if is_country_code(name) {
        return Some(name.to_ascii_uppercase());
    }
    const NAMES: [(&str, &str); 7] = [
        ("中国香港", "HK"),
        ("中国澳门", "MO"),
        ("中国台湾", "TW"),
        ("香港", "HK"),
        ("澳门", "MO"),
        ("台湾", "TW"),
        ("中国", "CN"),
    ];
    NAMES
        .iter()
        .find(|(key, _)| name.contains(key))
        .map(|(_, code)| code.to_string())
}

/// 把省份名称（中文全称或英文名）归一为省级行政区。
fn match_province(name: &str) -> Option<&'static Province> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    // 中文名直接包含匹配，兼容「浙江省」「广西壮族自治区」等全称。
    if let Some(found) = PROVINCES.iter().find(|province| name.contains(province.zh)) {
        return Some(found);
    }
    let key = normalize_province_en(name);
    if key.is_empty() {
        return None;
    }
    PROVINCES.iter().find(|province| {
        normalize_province_en(province.en) == key
            || province
                .aliases
                .iter()
                .any(|alias| normalize_province_en(alias) == key)
    })
}

/// 英文省名归一化：只保留字母数字，并去掉行政区划后缀。
fn normalize_province_en(name: &str) -> String {
    let mut key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    const SUFFIXES: [&str; 9] = [
        "zhuangautonomousregion",
        "huiautonomousregion",
        "uyghurautonomousregion",
        "autonomousregion",
        "specialadministrativeregion",
        "province",
        "municipality",
        "region",
        "zhuang",
    ];
    for suffix in SUFFIXES {
        if let Some(rest) = key.strip_suffix(suffix) {
            key = rest.to_string();
        }
    }
    key
}

/// 把国家代码归一到源清单使用的地区代码。
pub(crate) fn region_for_country(country: &str) -> &'static str {
    if country.eq_ignore_ascii_case(REGION_CN) {
        REGION_CN
    } else {
        REGION_GLOBAL
    }
}

/// 单个源最近一次测速的综合指标。
struct Metric {
    /// 下载速度（字节/秒）。
    speed: f64,
    /// 索引探测延迟（毫秒），缺失时视为最大值。
    latency: u64,
}

fn probe_metric(probe: &Probe) -> Option<Metric> {
    let Level::Passed(download) = &probe.download else {
        return None;
    };
    let speed = parse_speed(download)?;
    let latency = match &probe.http {
        Level::Passed(detail) => parse_ms(detail),
        _ => None,
    }
    .or_else(|| match &probe.tcp {
        Level::Passed(detail) => parse_ms(detail),
        _ => None,
    })
    .unwrap_or(u64::MAX);
    Some(Metric { speed, latency })
}

/// 解析下载详情中的速度，如 `203 ms · 1.5 MB/s` / `180 ms · 512 KB/s`。
fn parse_speed(detail: &str) -> Option<f64> {
    let (_, speed) = detail.split_once('·')?;
    let mut parts = speed.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?.to_ascii_uppercase();
    let factor = if unit.starts_with("GB") {
        1024.0 * 1024.0 * 1024.0
    } else if unit.starts_with("MB") {
        1024.0 * 1024.0
    } else if unit.starts_with("KB") {
        1024.0
    } else {
        1.0
    };
    Some(value * factor)
}

/// 解析详情中首次出现的毫秒数，如 `35 ms` / `1.2.0 · 245 ms`。
fn parse_ms(detail: &str) -> Option<u64> {
    let (value, _) = detail.split_once(" ms")?;
    value
        .trim()
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// 按地区与最近测速结果挑选推荐源，返回源标识。
///
/// 排序依据：下载速度最高优先；速度相同时延迟低者优先；再相同则同省源优先。
/// 尚无可用测速结果时，优先同省源，其次按内置优先级挑选。
pub(crate) fn recommend(
    country: &str,
    province: Option<&str>,
    sources: &[ManifestSource],
    probes: &HashMap<String, Probe>,
) -> Option<String> {
    let region = region_for_country(country);
    let mut pool: Vec<&ManifestSource> = sources
        .iter()
        .filter(|source| source.region.eq_ignore_ascii_case(region))
        .collect();
    if pool.is_empty() {
        pool = sources.iter().collect();
    }
    if pool.is_empty() {
        return None;
    }

    let province = province.unwrap_or("").trim();
    let mut measured: Vec<(f64, u64, bool, &ManifestSource)> = pool
        .iter()
        .filter_map(|source| {
            let metric = probe_metric(probes.get(&source.id)?)?;
            let same_province =
                !province.is_empty() && source.province.eq_ignore_ascii_case(province);
            Some((metric.speed, metric.latency, same_province, *source))
        })
        .collect();

    if !measured.is_empty() {
        measured.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
        return Some(measured[0].3.id.clone());
    }

    if !province.is_empty() {
        if let Some(source) = pool
            .iter()
            .find(|source| source.province.eq_ignore_ascii_case(province))
        {
            return Some(source.id.clone());
        }
    }

    let priority: &[&str] = if region == REGION_CN {
        &CN_PRIORITY
    } else {
        &GLOBAL_PRIORITY
    };
    priority
        .iter()
        .find_map(|id| pool.iter().find(|source| source.id == *id))
        .or_else(|| pool.first())
        .map(|source| source.id.clone())
}

/// 从测速结果提取推荐依据：下载速度与索引延迟的展示文本。
pub(crate) fn basis(probe: &Probe) -> Option<(String, String)> {
    let (_, speed) = match &probe.download {
        Level::Passed(detail) => detail.split_once('·')?,
        _ => return None,
    };
    let latency = match &probe.http {
        Level::Passed(detail) => detail.to_string(),
        _ => match &probe.tcp {
            Level::Passed(detail) => detail.to_string(),
            _ => return None,
        },
    };
    Some((speed.trim().to_string(), latency.trim().to_string()))
}

/// 地区代码的本地化名称，未知代码原样展示。
pub(crate) fn label(region: &str) -> String {
    if region.is_empty() {
        rust_i18n::t!("region.unknown").to_string()
    } else if region.eq_ignore_ascii_case(REGION_CN) {
        rust_i18n::t!("region.cn").to_string()
    } else if region.eq_ignore_ascii_case(REGION_GLOBAL) {
        rust_i18n::t!("region.global").to_string()
    } else {
        region.to_ascii_uppercase()
    }
}

/// 省份展示名：优先按当前语言查表，缺失时回退接口原始值。
pub(crate) fn province_label(province: Option<&str>, raw: Option<&str>) -> String {
    if let Some(province) = province.filter(|code| !code.is_empty()) {
        if let Some(found) = PROVINCES
            .iter()
            .find(|item| item.code.eq_ignore_ascii_case(province))
        {
            return if is_chinese_locale() {
                found.zh.to_string()
            } else {
                found.en.to_string()
            };
        }
    }
    raw.map(str::trim).unwrap_or("").to_string()
}

/// 源所属地区的展示文本，如 `中国 · 浙江` / `全球`。
pub(crate) fn source_region_label(region: &str, province: &str) -> String {
    if region.trim().is_empty() {
        return String::new();
    }
    let country = label(region);
    let province = province_label(Some(province), None);
    if province.is_empty() {
        country
    } else {
        format!("{country} · {province}")
    }
}

/// 识别结果的位置展示文本，如 `中国 · 浙江`。
pub(crate) fn location_label(
    country: &str,
    province: Option<&str>,
    province_name: Option<&str>,
) -> String {
    let country = label(country);
    let province = province_label(province, province_name);
    if province.is_empty() {
        country
    } else {
        format!("{country} · {province}")
    }
}

fn is_chinese_locale() -> bool {
    rust_i18n::locale().to_ascii_lowercase().starts_with("zh")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, region: &str, province: &str) -> ManifestSource {
        ManifestSource {
            id: id.to_string(),
            name: String::new(),
            description: String::new(),
            url: "sparse+https://example.com/".to_string(),
            region: region.to_string(),
            province: province.to_string(),
        }
    }

    fn probe(download: &str, http: &str) -> Probe {
        Probe {
            tcp: Level::Passed("20 ms".into()),
            http: Level::Passed(http.into()),
            parse: Level::Passed("1.0.0 · 40 ms".into()),
            download: Level::Passed(download.into()),
        }
    }

    #[test]
    fn parses_ipinfo_with_province() {
        let body = r#"{"ip":"1.2.3.4","city":"Hangzhou","region":"Zhejiang","country":"CN"}"#;
        let location = parse_location(body).unwrap();
        assert_eq!(location.country, "CN");
        assert_eq!(location.province.as_deref(), Some("ZJ"));
        assert_eq!(location.province_name.as_deref(), Some("Zhejiang"));
    }

    #[test]
    fn parses_ipip_location_array() {
        let body =
            r#"{"ret":"ok","data":{"ip":"1.2.3.4","location":["中国","浙江","杭州","","电信"]}}"#;
        let location = parse_location(body).unwrap();
        assert_eq!(location.country, "CN");
        assert_eq!(location.province.as_deref(), Some("ZJ"));
        assert_eq!(location.province_name.as_deref(), Some("浙江"));
    }

    #[test]
    fn parses_country_only_responses() {
        let location = parse_location(r#"{"ip":"1.1.1.1","country":"US"}"#).unwrap();
        assert_eq!(location.country, "US");
        assert_eq!(location.province, None);

        let location = parse_location("CN\n").unwrap();
        assert_eq!(location.country, "CN");
        assert_eq!(location.province, None);
    }

    #[test]
    fn matches_province_names() {
        assert_eq!(match_province("浙江省").map(|p| p.code), Some("ZJ"));
        assert_eq!(match_province("Zhejiang").map(|p| p.code), Some("ZJ"));
        assert_eq!(match_province("Inner Mongolia").map(|p| p.code), Some("NM"));
        assert_eq!(match_province("Guangxi Zhuang").map(|p| p.code), Some("GX"));
        assert_eq!(
            match_province("新疆维吾尔自治区").map(|p| p.code),
            Some("XJ")
        );
        assert!(match_province("Atlantis").is_none());
    }

    #[test]
    fn maps_country_to_region() {
        assert_eq!(region_for_country("CN"), REGION_CN);
        assert_eq!(region_for_country("cn"), REGION_CN);
        assert_eq!(region_for_country("US"), REGION_GLOBAL);
    }

    #[test]
    fn recommends_fastest_measured_source() {
        let sources = vec![
            source("aliyun", "CN", "ZJ"),
            source("rsproxy", "CN", "BJ"),
            source("official", "GLOBAL", ""),
        ];
        let mut probes = HashMap::new();
        probes.insert("aliyun".to_string(), probe("100 ms · 1.5 MB/s", "60 ms"));
        probes.insert("rsproxy".to_string(), probe("80 ms · 3.0 MB/s", "90 ms"));
        assert_eq!(
            recommend("CN", Some("ZJ"), &sources, &probes).as_deref(),
            Some("rsproxy")
        );
    }

    #[test]
    fn prefers_lower_latency_on_equal_speed() {
        let sources = vec![source("aliyun", "CN", "ZJ"), source("zju", "CN", "ZJ")];
        let mut probes = HashMap::new();
        probes.insert("aliyun".to_string(), probe("100 ms · 2.0 MB/s", "30 ms"));
        probes.insert("zju".to_string(), probe("100 ms · 2.0 MB/s", "80 ms"));
        assert_eq!(
            recommend("CN", Some("ZJ"), &sources, &probes).as_deref(),
            Some("aliyun")
        );
    }

    #[test]
    fn prefers_same_province_on_equal_metrics() {
        let sources = vec![source("aliyun", "CN", "ZJ"), source("rsproxy", "CN", "BJ")];
        let mut probes = HashMap::new();
        probes.insert("aliyun".to_string(), probe("100 ms · 2.0 MB/s", "30 ms"));
        probes.insert("rsproxy".to_string(), probe("100 ms · 2.0 MB/s", "30 ms"));
        assert_eq!(
            recommend("CN", Some("ZJ"), &sources, &probes).as_deref(),
            Some("aliyun")
        );
    }

    #[test]
    fn falls_back_to_province_then_priority_without_probes() {
        let sources = vec![
            source("official", "GLOBAL", ""),
            source("aliyun", "CN", "ZJ"),
            source("rsproxy", "CN", "BJ"),
        ];
        assert_eq!(
            recommend("CN", Some("ZJ"), &sources, &HashMap::new()).as_deref(),
            Some("aliyun")
        );
        assert_eq!(
            recommend("CN", None, &sources, &HashMap::new()).as_deref(),
            Some("rsproxy")
        );
        assert_eq!(
            recommend("US", None, &sources, &HashMap::new()).as_deref(),
            Some("official")
        );
    }

    #[test]
    fn parses_probe_metrics() {
        assert_eq!(
            parse_speed("203 ms · 1.5 MB/s"),
            Some(1.5 * 1024.0 * 1024.0)
        );
        assert_eq!(parse_speed("180 ms · 512 KB/s"), Some(512.0 * 1024.0));
        assert_eq!(parse_speed("oops"), None);
        assert_eq!(parse_ms("35 ms"), Some(35));
        assert_eq!(parse_ms("1.2.0 · 245 ms"), Some(245));
        assert_eq!(parse_ms("oops"), None);
    }

    #[test]
    fn labels_locations() {
        assert!(!label(REGION_CN).is_empty());
        assert!(!label(REGION_GLOBAL).is_empty());
        assert_eq!(label("JP"), "JP");
        assert!(!label("").is_empty());
        assert!(!province_label(Some("ZJ"), None).is_empty());
        assert_eq!(province_label(None, Some("  ")), "");
        assert!(!source_region_label("CN", "ZJ").is_empty());
        assert_eq!(source_region_label("", ""), "");
        assert!(!location_label("CN", Some("ZJ"), Some("浙江")).is_empty());
    }
}
