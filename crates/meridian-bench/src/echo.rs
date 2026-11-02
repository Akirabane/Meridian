//! The measuring packet: a custom payload on the channel `meridian:bench`, whose
//! data starts with a sequence number. The proxy relays custom payloads without
//! reading them, so it carries this one like any other play packet.
use horizon_protocol::buf::{Reader, write_varint};

/// The channel of the measuring packets.
pub const CHANNEL: &str = "meridian:bench";

/// The payload of a measuring packet: the channel, then `size` bytes of data
/// that start with `seq`.
pub fn payload(seq: u64, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHANNEL.len() + 1 + size.max(8));
    write_varint(&mut out, CHANNEL.len() as i32);
    out.extend_from_slice(CHANNEL.as_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.resize(out.len() + size.saturating_sub(8), 0x5a);
    out
}

/// The sequence number of a measuring packet, or `None` for any other payload.
pub fn seq(payload: &[u8]) -> Option<u64> {
    let mut r = Reader::new(payload);
    let channel = r.string(32767).ok()?;
    if channel != CHANNEL {
        return None;
    }
    let data = r.rest();
    Some(u64::from_be_bytes(data.get(..8)?.try_into().ok()?))
}

/// Whether a payload is a measuring packet.
pub fn is_echo(payload: &[u8]) -> bool {
    seq(payload).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_carries_its_number_and_size() {
        let p = payload(42, 64);
        // A one-byte length, the 14 bytes of the channel, then 64 bytes of data.
        assert_eq!(p.len(), 1 + 14 + 64);
        assert_eq!(seq(&p), Some(42));
        assert!(is_echo(&p));
        // A smaller size still carries the whole number.
        assert_eq!(seq(&payload(7, 0)), Some(7));
    }

    #[test]
    fn other_payloads_are_not_measuring_packets() {
        let mut brand = Vec::new();
        write_varint(&mut brand, "minecraft:brand".len() as i32);
        brand.extend_from_slice(b"minecraft:brand");
        brand.extend_from_slice(b"\x07vanilla");
        assert_eq!(seq(&brand), None);
        assert_eq!(seq(&[]), None);
    }

    /// The measuring packets cross the proxy's relay as they are, both ways, in
    /// the play phase: the relay neither reads nor answers them, so a run measures
    /// the plain copying of packets. The relay here is the one the proxy runs, with
    /// a command hook that would answer every chat command, compressing on both
    /// sides as a run does, with one packet under the threshold and one over it.
    #[test]
    fn the_proxy_relays_measuring_packets_untouched() {
        use horizon_protocol::ids::{configuration as cfg, play};
        use meridian_net::backend::BackendLink;
        use meridian_net::client::{AuthKind, ClientSession};
        use meridian_net::relay::{Flow, Relay};
        use meridian_testkit::backend::{acknowledge_configuration, finish_configuration};
        use meridian_testkit::peer::{self, Step};
        use meridian_testkit::trace::{Dir, Recorder};
        use meridian_testkit::{body, pipe};
        use std::sync::Arc;

        let (mut proxy_client, mut player) = pipe();
        let (mut proxy_backend, mut server) = pipe();
        for side in [
            &mut proxy_client,
            &mut player,
            &mut proxy_backend,
            &mut server,
        ] {
            side.set_compression(crate::backend::COMPRESSION);
        }
        let client = ClientSession {
            profile: meridian_net::Profile {
                name: "Bench0".into(),
                uuid: [1; 16],
                properties: Vec::new(),
            },
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        };
        let backend = BackendLink {
            connection: proxy_backend,
        };
        let (small, large) = (payload(1, 64), payload(2, 1000));
        let player_script = vec![
            Step::Expect {
                id: cfg::clientbound::FINISH_CONFIGURATION,
            },
            acknowledge_configuration(),
            Step::send(play::serverbound::CUSTOM_PAYLOAD, small.clone()),
            Step::expect_exactly(play::clientbound::CUSTOM_PAYLOAD, small.clone()),
            Step::send(play::serverbound::CUSTOM_PAYLOAD, large.clone()),
            Step::expect_exactly(play::clientbound::CUSTOM_PAYLOAD, large.clone()),
            Step::Close,
        ];
        let backend_script = vec![
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
            Step::expect_exactly(play::serverbound::CUSTOM_PAYLOAD, small.clone()),
            Step::send(play::clientbound::CUSTOM_PAYLOAD, small.clone()),
            Step::expect_exactly(play::serverbound::CUSTOM_PAYLOAD, large.clone()),
            Step::send(play::clientbound::CUSTOM_PAYLOAD, large.clone()),
            Step::ExpectClose,
        ];
        let recorder = Recorder::new();
        let server = peer::spawn(server, backend_script, Dir::ToServer, recorder.clone());
        let player = peer::spawn(player, player_script, Dir::ToClient, recorder.clone());
        let (relay, _handle) = Relay::new(client, backend).unwrap();
        let report = relay
            .with_commands(Arc::new(|_: &str| Some("answered by the proxy".into())))
            .run()
            .unwrap();
        server.join().unwrap().unwrap();
        player.join().unwrap().unwrap();

        // Each side got the other's measuring packets, byte for byte, and nothing
        // else in the play phase.
        let play_packets = |dir: Dir, id: i32| -> Vec<Vec<u8>> {
            recorder
                .trace()
                .packets(dir)
                .filter(|p| p.id == id)
                .map(|p| p.payload.clone())
                .collect()
        };
        assert_eq!(
            play_packets(Dir::ToServer, play::serverbound::CUSTOM_PAYLOAD),
            [small.clone(), large.clone()]
        );
        assert_eq!(
            play_packets(Dir::ToClient, play::clientbound::CUSTOM_PAYLOAD),
            [small.clone(), large.clone()]
        );
        let bytes = |id: i32| (body(id, &small).len() + body(id, &large).len()) as u64;
        // The end of the configuration phase, then the two echoes, each way.
        assert_eq!(
            report.to_backend,
            Flow {
                packets: 3,
                bytes: body(cfg::serverbound::FINISH_CONFIGURATION, &[]).len() as u64
                    + bytes(play::serverbound::CUSTOM_PAYLOAD),
            }
        );
        assert_eq!(report.to_client.packets, 3);
    }
}
