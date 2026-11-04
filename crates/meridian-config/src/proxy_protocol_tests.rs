//! The `[proxy-protocol]` table.
use super::*;
use std::net::IpAddr;

fn parse(tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{tables}"#
    ))
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// Without the table the header is never read: the proxy behaves as it did before.
#[test]
fn the_default_reads_no_header() {
    let table = parse("").unwrap().proxy_protocol;
    assert_eq!(table, ProxyProtocol::default());
    assert_eq!(table.mode, ProxyProtocolMode::Off);
    assert!(table.trusted.is_empty());
    assert!(!table.trusts(ip("10.0.0.1")));
    // An empty table is the same.
    assert_eq!(
        parse("[proxy-protocol]").unwrap().proxy_protocol,
        ProxyProtocol::default()
    );
}

#[test]
fn a_mode_needs_the_addresses_that_may_send_the_header() {
    for mode in ["optional", "required"] {
        let e = parse(&format!("[proxy-protocol]\nmode = \"{mode}\"")).unwrap_err();
        assert!(matches!(e, ConfigError::ProxyProtocolWithoutTrusted), "{e}");
        assert!(e.to_string().contains("trusted"), "{e}");
        let e = parse(&format!(
            "[proxy-protocol]\nmode = \"{mode}\"\ntrusted = []"
        ))
        .unwrap_err();
        assert!(matches!(e, ConfigError::ProxyProtocolWithoutTrusted), "{e}");
    }
}

#[test]
fn every_key_can_be_set() {
    let table = parse(
        "[proxy-protocol]\nmode = \"required\"\ntrusted = [\"10.0.0.5\", \"192.168.0.0/16\", \
         \"2001:db8::/32\"]\nheader-timeout-ms = 500",
    )
    .unwrap()
    .proxy_protocol;
    assert_eq!(table.mode, ProxyProtocolMode::Required);
    assert_eq!(table.header_timeout_ms, 500);
    assert_eq!(table.trusted.len(), 3);
    assert!(table.trusts(ip("10.0.0.5")));
    assert!(table.trusts(ip("192.168.77.1")));
    assert!(table.trusts(ip("2001:db8:1::1")));
    assert!(!table.trusts(ip("10.0.0.6")));
    assert!(!table.trusts(ip("192.169.0.1")));
    assert!(!table.trusts(ip("2001:db9::1")));
}

/// A table that is off trusts nobody, whatever it lists.
#[test]
fn an_off_table_trusts_nobody() {
    let table = parse("[proxy-protocol]\nmode = \"off\"\ntrusted = [\"10.0.0.5\"]")
        .unwrap()
        .proxy_protocol;
    assert!(!table.trusts(ip("10.0.0.5")));
}

#[test]
fn ranges_are_checked_bit_by_bit() {
    let net = |t: &str| TrustedNet::parse(t).unwrap();
    assert!(net("10.0.0.0/8").contains(ip("10.255.255.255")));
    assert!(!net("10.0.0.0/8").contains(ip("11.0.0.0")));
    // A prefix that ends inside a byte.
    assert!(net("172.16.0.0/12").contains(ip("172.31.255.255")));
    assert!(!net("172.16.0.0/12").contains(ip("172.32.0.0")));
    assert!(net("192.0.2.7/32").contains(ip("192.0.2.7")));
    assert!(!net("192.0.2.7/32").contains(ip("192.0.2.8")));
    assert!(net("fd00::/8").contains(ip("fdff::1")));
    assert!(!net("fd00::/8").contains(ip("fe00::1")));
    // The families do not mix, except for IPv4 written as IPv6.
    assert!(!net("10.0.0.0/8").contains(ip("::1")));
    assert!(net("10.0.0.0/8").contains(ip("::ffff:10.1.2.3")));
    assert!(net("::ffff:10.0.0.1").contains(ip("10.0.0.1")));
}

/// Anyone who may send the header can claim any address, so a range of every address is
/// refused, as is anything that is not an address.
#[test]
fn a_range_of_every_address_and_nonsense_are_refused() {
    for bad in [
        "0.0.0.0/0",
        "::/0",
        "10.0.0.0/33",
        "::1/129",
        "10.0.0.0/",
        "10.0.0.0/x",
        "balancer.example.com",
        "",
        "10.0.0",
        "10.0.0.256",
    ] {
        assert!(TrustedNet::parse(bad).is_err(), "{bad:?}");
        let e = parse(&format!(
            "[proxy-protocol]\nmode = \"required\"\ntrusted = [{bad:?}]"
        ))
        .unwrap_err();
        assert!(
            matches!(e, ConfigError::InvalidTrustedAddress { .. }),
            "{e}"
        );
        assert!(e.to_string().contains("trusted"), "{e}");
    }
}

#[test]
fn a_zero_timeout_a_wrong_mode_and_a_wrong_key_are_refused() {
    let e = parse("[proxy-protocol]\nheader-timeout-ms = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroProxyProtocolTimeout), "{e}");
    for bad in [
        "[proxy-protocol]\nmode = \"always\"",
        "[proxy-protocol]\nmode = true",
        "[proxy-protocol]\nheader_timeout_ms = 5",
        "[proxy-protocol]\nunknown = 1",
        "[proxy-protocol]\ntrusted = \"10.0.0.1\"",
    ] {
        assert!(matches!(parse(bad), Err(ConfigError::Parse(_))), "{bad}");
    }
}
