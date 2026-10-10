//! Where a model server is, and whether it counts as local (GUARDRAILS §3).
//!
//! A server is **local** only if its address is a loopback address
//! (`127.0.0.0/8`, `::1`) or the name `localhost`. Everything else is
//! **remote**, including servers on the local network and names that merely
//! look local. Remote servers must use HTTPS.

use std::fmt;

use url::{Host, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Local,
    Remote,
}

/// The base address of an OpenAI-compatible server, such as
/// `http://localhost:1234/v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    base: Url,
    locality: Locality,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    /// Not an address at all.
    Invalid,
    /// Only `http` and `https`.
    Scheme,
    /// User names or passwords don't belong in the address; keys live in the
    /// Keychain (GUARDRAILS §3).
    Credentials,
    /// A query or fragment, which a base address doesn't have.
    Extra,
    /// A remote server without HTTPS.
    RemoteNeedsHttps,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EndpointError::Invalid => "That isn't a valid address.",
            EndpointError::Scheme => "The address must start with http:// or https://.",
            EndpointError::Credentials => "Leave the user name and password out of the address.",
            EndpointError::Extra => "The address can't have a ? or # part.",
            EndpointError::RemoteNeedsHttps => "A server that isn't on this Mac must use https://.",
        })
    }
}

impl std::error::Error for EndpointError {}

impl Endpoint {
    pub fn parse(address: &str) -> Result<Self, EndpointError> {
        let mut base = Url::parse(address.trim()).map_err(|_| EndpointError::Invalid)?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(EndpointError::Scheme);
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(EndpointError::Credentials);
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(EndpointError::Extra);
        }
        let locality = match base.host() {
            Some(host) if is_loopback(&host) => Locality::Local,
            Some(_) => Locality::Remote,
            None => return Err(EndpointError::Invalid),
        };
        if locality == Locality::Remote && base.scheme() != "https" {
            return Err(EndpointError::RemoteNeedsHttps);
        }
        // Paths are joined onto the base, so it must end in a slash:
        // ".../v1" + "chat/completions" = ".../v1/chat/completions".
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Endpoint { base, locality })
    }

    pub fn locality(&self) -> Locality {
        self.locality
    }

    /// `path` (such as `chat/completions`) under the base address.
    pub fn url(&self, path: &str) -> Url {
        self.base
            .join(path.trim_start_matches('/'))
            .unwrap_or_else(|_| self.base.clone())
    }

    pub fn as_str(&self) -> &str {
        self.base.as_str()
    }
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
        // Only the name itself; "localhost.example.com" is a real host
        // somewhere else.
        Host::Domain(name) => name.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locality(address: &str) -> Locality {
        Endpoint::parse(address).unwrap().locality()
    }

    #[test]
    fn only_loopback_is_local() {
        for address in [
            "http://localhost:11434/v1",
            "http://LOCALHOST.:1234/v1",
            "http://127.0.0.1:1234/v1",
            "http://127.1.2.3:8080",
            "http://[::1]:8080/v1",
        ] {
            assert_eq!(locality(address), Locality::Local, "{address}");
        }
    }

    #[test]
    fn everything_else_is_remote_and_needs_https() {
        for address in [
            "http://192.168.1.10:11434/v1",
            "http://10.0.0.2/v1",
            "http://my-mac.local:1234/v1",
            "http://localhost.example.com/v1",
            "http://127.0.0.1.nip.io/v1",
            "http://api.example.com/v1",
        ] {
            assert_eq!(
                Endpoint::parse(address),
                Err(EndpointError::RemoteNeedsHttps),
                "{address}"
            );
        }
        assert_eq!(locality("https://api.example.com/v1"), Locality::Remote);
        assert_eq!(locality("https://192.168.1.10/v1"), Locality::Remote);
    }

    #[test]
    fn odd_addresses_are_refused() {
        assert_eq!(Endpoint::parse("not a url"), Err(EndpointError::Invalid));
        assert_eq!(
            Endpoint::parse("ftp://localhost/v1"),
            Err(EndpointError::Scheme)
        );
        assert_eq!(
            Endpoint::parse("file:///Users/anna/model"),
            Err(EndpointError::Scheme)
        );
        assert_eq!(
            Endpoint::parse("https://anna:hunter2@api.example.com/v1"),
            Err(EndpointError::Credentials)
        );
        assert_eq!(
            Endpoint::parse("http://localhost:1234/v1?key=abc"),
            Err(EndpointError::Extra)
        );
    }

    #[test]
    fn paths_are_joined_under_the_base() {
        let endpoint = Endpoint::parse("http://localhost:1234/v1").unwrap();
        assert_eq!(
            endpoint.url("chat/completions").as_str(),
            "http://localhost:1234/v1/chat/completions"
        );
        let endpoint = Endpoint::parse("http://localhost:1234/v1/").unwrap();
        assert_eq!(
            endpoint.url("/models").as_str(),
            "http://localhost:1234/v1/models"
        );
    }
}
