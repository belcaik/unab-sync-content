use reqwest::Url;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CookieEntry {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: Option<u64>,
    pub name: String,
    pub value: String,
}

#[allow(dead_code)]
/// Parse a Netscape cookie file into cookie entries.
pub fn parse_netscape_file(path: &str) -> std::io::Result<Vec<CookieEntry>> {
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Netscape format: domain, flag, path, secure, expiry, name, value
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 7 {
            continue;
        }
        let domain = parts[0].to_string();
        let include_subdomains = parts[1].eq_ignore_ascii_case("TRUE");
        let path = parts[2].to_string();
        let secure = parts[3].eq_ignore_ascii_case("TRUE");
        let expires = parts[4].parse::<u64>().ok();
        let name = parts[5].to_string();
        let value = parts[6].to_string();
        out.push(CookieEntry {
            domain,
            include_subdomains,
            path,
            secure,
            expires,
            name,
            value,
        });
    }
    Ok(out)
}

#[allow(dead_code)]
fn domain_matches(cookie_domain: &str, host: &str, include_subdomains: bool) -> bool {
    if cookie_domain.starts_with('.') {
        let d = &cookie_domain[1..];
        host == d || (include_subdomains && host.ends_with(&format!(".{}", d)))
    } else {
        host == cookie_domain
    }
}

/// Build a Cookie header string for the given URL from the provided Netscape cookie entries.
#[allow(dead_code)]
pub fn cookie_header_for(url: &Url, cookies: &[CookieEntry]) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let host = url.host_str().unwrap_or("");
    let path = url.path();
    let is_https = url.scheme() == "https";

    let mut pairs: Vec<(String, String)> = Vec::new();
    for c in cookies {
        if let Some(exp) = c.expires {
            if exp != 0 && exp < now {
                continue;
            }
        }
        if c.secure && !is_https {
            continue;
        }
        if !domain_matches(&c.domain, host, c.include_subdomains) {
            continue;
        }
        if !path.starts_with(&c.path) {
            continue;
        }
        if c.name.is_empty() {
            continue;
        }
        pairs.push((c.name.clone(), c.value.clone()));
    }
    if pairs.is_empty() {
        None
    } else {
        let s = pairs
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("; ");
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_and_match_cookie() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = "# Netscape HTTP Cookie File\n.example.com\tTRUE\t/\tTRUE\t4102444800\tsessionid\tabc123\n";
        fs::write(tmp.path(), content).unwrap();
        let cookies = parse_netscape_file(tmp.path().to_str().unwrap()).unwrap();
        let url = Url::parse("https://sub.example.com/path").unwrap();
        let hdr = cookie_header_for(&url, &cookies).unwrap();
        assert!(hdr.contains("sessionid=abc123"));
    }
}
