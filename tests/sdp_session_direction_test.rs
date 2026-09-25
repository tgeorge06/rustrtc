//! RFC 8866 §6.7 (RFC 4566 §6): a direction attribute at session level
//! applies to every media section that does not carry its own.
use rustrtc::sdp::{Direction, SdpType, SessionDescription};
use rustrtc::{PeerConnection, RtcConfiguration, TransportMode};

fn sdp(session_direction: Option<&str>, media_directions: &[Option<&str>]) -> String {
    let mut out = String::from("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n");
    if let Some(direction) = session_direction {
        out.push_str(&format!("a={direction}\r\n"));
    }
    for (port, direction) in media_directions.iter().enumerate() {
        out.push_str(&format!(
            "m=audio {} RTP/AVP 0\r\nc=IN IP4 127.0.0.1\r\na=rtpmap:0 PCMU/8000\r\n",
            40000 + 2 * port
        ));
        if let Some(direction) = direction {
            out.push_str(&format!("a={direction}\r\n"));
        }
    }
    out
}

fn directions(raw: &str) -> Vec<Direction> {
    SessionDescription::parse(SdpType::Offer, raw)
        .expect("parse")
        .media_sections
        .iter()
        .map(|section| section.direction)
        .collect()
}

#[test]
fn session_level_direction_applies_to_media_without_one() {
    for (attribute, direction) in [
        ("sendonly", Direction::SendOnly),
        ("recvonly", Direction::RecvOnly),
        ("inactive", Direction::Inactive),
        ("sendrecv", Direction::SendRecv),
    ] {
        assert_eq!(
            directions(&sdp(Some(attribute), &[None, None])),
            vec![direction, direction],
            "session-level a={attribute}"
        );
    }
}

#[test]
fn media_level_direction_overrides_session_level() {
    assert_eq!(
        directions(&sdp(Some("sendonly"), &[Some("sendrecv"), None])),
        vec![Direction::SendRecv, Direction::SendOnly]
    );
}

#[test]
fn data_channel_sections_do_not_inherit_the_session_direction() {
    let raw = format!(
        "{}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=sctp-port:5000\r\n",
        sdp(Some("sendonly"), &[None])
    );
    assert_eq!(
        directions(&raw),
        vec![Direction::SendOnly, Direction::SendRecv]
    );
}

#[test]
fn without_any_direction_media_is_sendrecv() {
    assert_eq!(
        directions(&sdp(None, &[None, Some("recvonly")])),
        vec![Direction::SendRecv, Direction::RecvOnly]
    );
}

#[test]
fn reparsing_our_serialization_keeps_the_direction() {
    let parsed =
        SessionDescription::parse(SdpType::Offer, &sdp(Some("inactive"), &[None])).expect("parse");
    assert_eq!(
        directions(&parsed.to_sdp_string()),
        vec![Direction::Inactive]
    );
}

/// A SIP peer holds the call with a session-level `a=sendonly`: we answer
/// `recvonly`, as for a media-level hold.
#[tokio::test]
async fn a_session_level_hold_is_answered_recvonly() {
    let pc = PeerConnection::new(RtcConfiguration {
        transport_mode: TransportMode::Rtp,
        ..RtcConfiguration::default()
    });
    let offer =
        SessionDescription::parse(SdpType::Offer, &sdp(Some("sendonly"), &[None])).expect("parse");
    pc.set_remote_description(offer)
        .await
        .expect("remote offer");
    let answer = pc.create_answer().await.expect("answer");
    assert_eq!(answer.media_sections[0].direction, Direction::RecvOnly);
}
