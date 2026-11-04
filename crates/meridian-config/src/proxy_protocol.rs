//! The `[proxy-protocol]` table: whether the proxy reads the PROXY protocol header that a
//! layer 4 load balancer in front of it sends, and which addresses may send it
//! (docs/OPERATING.md).
//!
//! The header says who the player is. Anyone who may send it can claim any address, so the
//! proxy reads it only from the addresses listed in `trusted`, and a mode other than `off`
//! is refused without such a list.
use super::*;
use std::net::IpAddr;

/// Whether the PROXY protocol header is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocolMode {
    /// No header is read. The address of the player is the one of the connection.
    #[default]
    Off,
    /// A connection from a trusted address may begin with a header or not. A connection
    /// from any other address is a player, whatever it sends.
    Optional,
    /// A connection from a trusted address must begin with a header. A connection from
    /// any other address is closed.
    Required,
}

/// An address, or a range of addresses (`192.0.2.0/24`), that may send the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedNet {
    pub addr: IpAddr,
    /// How many leading bits of `addr` an address shares with the range: 32 or 128 for
    /// one address.
    pub prefix: u8,
}

impl TrustedNet {
    /// `text` is an IP address, or an address and a prefix length, as `10.0.0.0/8` or
    /// `2001:db8::/32`. The range of every address (`/0`) is refused: it would let any
    /// client claim an address.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = address
            .trim()
            .parse()
            .map_err(|_| format!("{address:?} is not an IP address"))?;
        let addr = addr.to_canonical();
        let width: u8 = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => width,
            Some(p) => {
                let p: u8 = p
                    .trim()
                    .parse()
                    .map_err(|_| format!("{p:?} is not a prefix length"))?;
                if p > width {
                    return Err(format!("the prefix length {p} is more than {width}"));
                }
                p
            }
        };
        if prefix == 0 {
            return Err(
                "a prefix length of 0 is every address, so any client could claim any address"
                    .to_owned(),
            );
        }
        Ok(Self { addr, prefix })
    }

    /// Whether `ip` is in the range. An IPv4 address written as IPv6 (`::ffff:a.b.c.d`)
    /// is the IPv4 address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                prefix_matches(&net.octets(), &ip.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_matches(&net.octets(), &ip.octets(), self.prefix)
            }
            _ => false,
        }
    }
}

/// Whether the first `bits` bits of `a` and `b` are the same.
fn prefix_matches(a: &[u8], b: &[u8], bits: u8) -> bool {
    let whole = usize::from(bits / 8);
    if a[..whole] != b[..whole] {
        return false;
    }
    match bits % 8 {
        0 => true,
        rest => {
            let mask = 0xffu8 << (8 - rest);
            a[whole] & mask == b[whole] & mask
        }
    }
}

/// The `[proxy-protocol]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyProtocol {
    pub mode: ProxyProtocolMode,
    /// The addresses that may send the header. Required, and not empty, unless `mode`
    /// is `off`.
    pub trusted: Vec<TrustedNet>,
    /// Milliseconds a trusted connection has to send the whole header.
    pub header_timeout_ms: u64,
}

impl Default for ProxyProtocol {
    fn default() -> Self {
        Self {
            mode: ProxyProtocolMode::Off,
            trusted: Vec::new(),
            header_timeout_ms: 3_000,
        }
    }
}

impl ProxyProtocol {
    /// Whether the header is read from `ip`: the mode is not `off` and `ip` is trusted.
    pub fn trusts(&self, ip: IpAddr) -> bool {
        self.mode != ProxyProtocolMode::Off && self.trusted.iter().any(|t| t.contains(ip))
    }
}

/// The `[proxy-protocol]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileProxyProtocol {
    mode: Option<ProxyProtocolMode>,
    trusted: Option<Vec<String>>,
    header_timeout_ms: Option<u64>,
}

impl FileProxyProtocol {
    pub(crate) fn into_settings(self) -> Result<ProxyProtocol, ConfigError> {
        let defaults = ProxyProtocol::default();
        let trusted = self
            .trusted
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                TrustedNet::parse(entry).map_err(|reason| ConfigError::InvalidTrustedAddress {
                    entry: entry.clone(),
                    reason,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ProxyProtocol {
            mode: self.mode.unwrap_or(defaults.mode),
            trusted,
            header_timeout_ms: self.header_timeout_ms.unwrap_or(defaults.header_timeout_ms),
        })
    }
}

impl ProxyProtocol {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.header_timeout_ms == 0 {
            return Err(ConfigError::ZeroProxyProtocolTimeout);
        }
        if self.mode != ProxyProtocolMode::Off && self.trusted.is_empty() {
            return Err(ConfigError::ProxyProtocolWithoutTrusted);
        }
        Ok(())
    }
}
