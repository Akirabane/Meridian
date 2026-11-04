//! The `[metrics]` table.
use super::*;

fn parse(tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{tables}"#
    ))
}

/// Off unless asked for, and then on the loopback.
#[test]
fn metrics_are_off_by_default_and_bound_to_the_loopback() {
    let metrics = parse("").unwrap().metrics;
    assert_eq!(metrics, Metrics::default());
    assert!(!metrics.enabled);
    assert_eq!(metrics.bind, "127.0.0.1:9100".parse().unwrap());
    assert_eq!(parse("[metrics]").unwrap().metrics, Metrics::default());
}

#[test]
fn every_key_can_be_set() {
    let metrics = parse("[metrics]\nenabled = true\nbind = \"127.0.0.1:9200\"")
        .unwrap()
        .metrics;
    assert_eq!(
        metrics,
        Metrics {
            enabled: true,
            bind: "127.0.0.1:9200".parse().unwrap(),
        }
    );
}

#[test]
fn a_bad_address_is_refused_even_when_off() {
    for bad in ["nowhere", "127.0.0.1", "127.0.0.1:99999", ":9100"] {
        let e = parse(&format!("[metrics]\nbind = \"{bad}\"")).unwrap_err();
        assert!(
            matches!(e, ConfigError::InvalidMetricsBind(_)),
            "{bad}: {e}"
        );
    }
}

#[test]
fn port_zero_is_refused_when_on() {
    let e = parse("[metrics]\nenabled = true\nbind = \"127.0.0.1:0\"").unwrap_err();
    assert!(matches!(e, ConfigError::InvalidMetricsBind(_)), "{e}");
}

#[test]
fn metrics_cannot_take_the_port_players_use() {
    let e = Config::from_toml_str(
        r#"forwarding-secret = "test-secret"
listen = "0.0.0.0:25565"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
[metrics]
enabled = true
bind = "127.0.0.1:25565"
"#,
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::MetricsOnListen(_)), "{e}");
    // Off, the same address does no harm; and another address is no clash.
    let ok = parse("[metrics]\nbind = \"127.0.0.1:25565\"").unwrap();
    assert!(!ok.metrics.enabled);
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(parse("[metrics]\nport = 9100").is_err());
}
