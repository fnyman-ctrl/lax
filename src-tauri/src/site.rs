//! Only the site of a browser address is kept (GUARDRAILS §4).
//!
//! Paths, queries and fragments can contain searches, document names, tokens
//! and personal data, so Lax keeps only the host (`figma.com`). The window
//! title usually names the document anyway.

use url::{Host, Url};

/// The host of an `http` or `https` address, in lower case, without a port or
/// a trailing dot. `www.` is kept: the host is stored as the browser had it,
/// and exclusions match subdomains anyway. Anything else (other schemes, addresses that
/// don't parse) gives `None`, and nothing is kept.
///
/// International domain names are returned in their ASCII (punycode) form,
/// which is also how exclusions are matched.
pub fn site_from_url(address: &str) -> Option<String> {
    let url = Url::parse(address.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = match url.host()? {
        Host::Domain(domain) => domain.trim_end_matches('.').to_ascii_lowercase(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(address: &str) -> Option<String> {
        site_from_url(address)
    }

    #[test]
    fn keeps_only_the_host() {
        assert_eq!(
            site("https://www.figma.com/file/AbC123/Q3-proposal?node-id=1#frame"),
            Some("www.figma.com".to_string())
        );
        assert_eq!(
            site("https://docs.example.com/d/1x2y3z/edit"),
            Some("docs.example.com".to_string())
        );
    }

    #[test]
    fn drops_searches_and_tokens() {
        let kept =
            site("https://www.example.com/search?q=anna+example+diagnosis&token=abc123").unwrap();
        assert_eq!(kept, "www.example.com");
    }

    #[test]
    fn drops_credentials_and_ports() {
        assert_eq!(
            site("https://anna:hunter2@intranet.example.com:8443/admin"),
            Some("intranet.example.com".to_string())
        );
    }

    #[test]
    fn normalises_case_and_trailing_dot() {
        assert_eq!(
            site("HTTPS://Example.COM./"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn keeps_local_and_ip_hosts() {
        assert_eq!(
            site("http://localhost:1420/"),
            Some("localhost".to_string())
        );
        assert_eq!(
            site("http://192.168.1.10/router"),
            Some("192.168.1.10".to_string())
        );
        assert_eq!(site("http://[::1]:8080/"), Some("::1".to_string()));
    }

    #[test]
    fn international_domains_become_ascii() {
        assert_eq!(
            site("https://räksmörgås.se/meny"),
            Some("xn--rksmrgs-5wao1o.se".to_string())
        );
    }

    #[test]
    fn other_schemes_keep_nothing() {
        for address in [
            "file:///Users/anna/Documents/secret.pdf",
            "about:blank",
            "chrome://settings/passwords",
            "data:text/html,<h1>hi</h1>",
            "javascript:alert(1)",
            "mailto:anna@example.com",
            "ftp://files.example.com/",
        ] {
            assert_eq!(site(address), None, "{address}");
        }
    }

    #[test]
    fn unparseable_addresses_keep_nothing() {
        assert_eq!(site(""), None);
        assert_eq!(site("not a url"), None);
        assert_eq!(site("example.com/no-scheme"), None);
    }
}
