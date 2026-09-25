//! RFC 3264 §6 / §8.2: a media section with port 0 is rejected and carries
//! no media, except a `bundle-only` section inside a BUNDLE group, which
//! uses port 0 while sharing the group's transport (RFC 8843 §6).
use anyhow::Result;
use bytes::Bytes;
use rustrtc::media::frame::{AudioFrame, MediaSample};
use rustrtc::media::track::{SampleStreamSource, sample_track};
use rustrtc::peer_connection::RtpTransceiver;
use rustrtc::sdp::{Attribute, SessionDescription};
use rustrtc::{PeerConnection, RtcConfiguration, RtpCodecParameters};
use std::sync::Arc;
use std::time::Duration;

fn opus() -> RtpCodecParameters {
    RtpCodecParameters {
        payload_type: 111,
        name: "opus".to_string(),
        clock_rate: 48000,
        channels: 2,
    }
}

fn reject(desc: &mut SessionDescription) {
    desc.media_sections[0].port = 0;
}

/// Section `index` becomes port 0 + `bundle-only`, and the BUNDLE group
/// lists every section (the first one is the tag).
fn make_bundle_only(desc: &mut SessionDescription, index: usize) {
    desc.media_sections[index].port = 0;
    desc.media_sections[index]
        .attributes
        .push(Attribute::new("bundle-only", None));
    let mids: Vec<String> = desc.media_sections.iter().map(|m| m.mid.clone()).collect();
    desc.session.attributes.retain(|a| a.key != "group");
    desc.session.attributes.push(Attribute::new(
        "group",
        Some(format!("BUNDLE {}", mids.join(" "))),
    ));
}

fn pump(source: SampleStreamSource) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let frame = AudioFrame {
                data: Bytes::from_static(&[0xAA; 20]),
                ..AudioFrame::default()
            };
            if source.send(MediaSample::Audio(frame)).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

async fn sends_within(transceiver: &Arc<RtpTransceiver>, window: Duration) -> bool {
    let sender = transceiver.sender().expect("sender");
    let before = sender.packets_sent();
    tokio::time::sleep(window).await;
    sender.packets_sent() > before
}

/// pc1 (sending audio) offers to pc2; `edit_offer` / `edit_answer` modify
/// the descriptions on the wire. Returns whether pc1 then sends RTP.
async fn offerer_sends(
    edit_offer: fn(&mut SessionDescription),
    edit_answer: fn(&mut SessionDescription),
) -> Result<bool> {
    Ok(offerer_sends_on(1, edit_offer, edit_answer).await?[0])
}

/// As [`offerer_sends`] with `tracks` bundled audio m= lines; returns, per
/// m= line, whether pc1 sends RTP on it.
async fn offerer_sends_on(
    tracks: usize,
    edit_offer: fn(&mut SessionDescription),
    edit_answer: fn(&mut SessionDescription),
) -> Result<Vec<bool>> {
    let pc1 = PeerConnection::new(RtcConfiguration::default());
    let pc2 = PeerConnection::new(RtcConfiguration::default());
    let mut sources = Vec::new();
    for _ in 0..tracks {
        let (source, track, _) = sample_track(rustrtc::media::MediaKind::Audio, 100);
        pc1.add_track(track, opus())?;
        sources.push(source);
    }

    let _ = pc1.create_offer().await?;
    pc1.wait_for_gathering_complete().await;
    let mut offer = pc1.create_offer().await?;
    edit_offer(&mut offer);
    pc1.set_local_description(offer.clone())?;
    pc2.set_remote_description(offer).await?;
    let _ = pc2.create_answer().await?;
    pc2.wait_for_gathering_complete().await;
    let mut answer = pc2.create_answer().await?;
    edit_answer(&mut answer);
    pc2.set_local_description(answer.clone())?;
    pc1.set_remote_description(answer).await?;
    tokio::try_join!(pc1.wait_for_connected(), pc2.wait_for_connected())?;

    let pumps: Vec<_> = sources.into_iter().map(pump).collect();
    let mut sends = Vec::new();
    for transceiver in pc1.get_transceivers() {
        sends.push(sends_within(&transceiver, Duration::from_millis(1000)).await);
    }
    pumps.iter().for_each(|p| p.abort());
    Ok(sends)
}

#[tokio::test]
async fn a_webrtc_offer_with_one_section_carries_a_bundle_group() -> Result<()> {
    let pc = PeerConnection::new(RtcConfiguration::default());
    let (_source, track, _) = sample_track(rustrtc::media::MediaKind::Audio, 100);
    pc.add_track(track, opus())?;
    let offer = pc.create_offer().await?;
    let mid = &offer.media_sections[0].mid;
    assert!(
        offer
            .session
            .attributes
            .iter()
            .any(|a| a.key == "group" && a.value.as_deref() == Some(&format!("BUNDLE {mid}"))),
        "{}",
        offer.to_sdp_string()
    );
    Ok(())
}

#[tokio::test]
async fn media_flows_on_an_accepted_section() -> Result<()> {
    assert!(offerer_sends(|_| {}, |_| {}).await?);
    Ok(())
}

#[tokio::test]
async fn no_rtp_after_the_answer_rejects_the_section() -> Result<()> {
    assert!(
        !offerer_sends(|_| {}, reject).await?,
        "the answer rejected the m= section (port 0), yet we sent RTP"
    );
    Ok(())
}

#[tokio::test]
async fn no_rtp_on_a_section_we_offered_with_port_zero() -> Result<()> {
    assert!(
        !offerer_sends(reject, |_| {}).await?,
        "we offered the m= section disabled (port 0), yet we sent RTP"
    );
    Ok(())
}

#[tokio::test]
async fn a_bundle_only_section_with_port_zero_is_not_rejected() -> Result<()> {
    assert_eq!(
        offerer_sends_on(2, |_| {}, |d| make_bundle_only(d, 1)).await?,
        vec![true, true],
        "a non-tag bundle-only section in a BUNDLE group keeps its media"
    );
    Ok(())
}

#[tokio::test]
async fn the_bundle_tag_with_port_zero_is_rejected() -> Result<()> {
    assert_eq!(
        offerer_sends_on(2, |_| {}, |d| make_bundle_only(d, 0)).await?,
        vec![false, true]
    );
    Ok(())
}

#[tokio::test]
async fn bundle_only_outside_a_bundle_group_is_rejected() -> Result<()> {
    fn bundle_only_without_group(desc: &mut SessionDescription) {
        make_bundle_only(desc, 1);
        desc.session.attributes.retain(|a| a.key != "group");
    }
    assert_eq!(
        offerer_sends_on(2, |_| {}, bundle_only_without_group).await?,
        vec![true, false]
    );
    Ok(())
}

/// RFC 3264 §6: a stream offered with port 0 is answered with port 0, also
/// once candidates have been gathered.
#[tokio::test]
async fn a_disabled_offered_stream_is_answered_with_port_zero() -> Result<()> {
    let pc = PeerConnection::new(RtcConfiguration {
        transport_mode: rustrtc::TransportMode::Rtp,
        ..RtcConfiguration::default()
    });
    let raw = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nc=IN IP4 127.0.0.1\r\n\
               m=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
               m=audio 0 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let offer = SessionDescription::parse(rustrtc::sdp::SdpType::Offer, raw)?;
    pc.set_remote_description(offer).await?;
    let answer = pc.create_answer().await?;
    assert_ne!(answer.media_sections[0].port, 0);
    assert_eq!(answer.media_sections[1].port, 0);
    pc.set_local_description(answer)?;
    pc.wait_for_gathering_complete().await;
    let applied = pc.local_description().expect("local description");
    assert_eq!(
        applied.media_sections[1].port,
        0,
        "{}",
        applied.to_sdp_string()
    );
    Ok(())
}

/// A rejected section is left out of the answer's BUNDLE group, so it can
/// never become the group's tag.
#[tokio::test]
async fn a_rejected_section_is_not_bundled_in_the_answer() -> Result<()> {
    let pc1 = PeerConnection::new(RtcConfiguration::default());
    let pc2 = PeerConnection::new(RtcConfiguration::default());
    for _ in 0..2 {
        let (_source, track, _) = sample_track(rustrtc::media::MediaKind::Audio, 100);
        pc1.add_track(track, opus())?;
    }
    let mut offer = pc1.create_offer().await?;
    reject(&mut offer);
    pc2.set_remote_description(offer).await?;
    let answer = pc2.create_answer().await?;
    assert_eq!(answer.media_sections[0].port, 0);
    let second = &answer.media_sections[1].mid;
    let group: Vec<_> = answer
        .session
        .attributes
        .iter()
        .filter(|a| a.key == "group")
        .filter_map(|a| a.value.clone())
        .collect();
    assert_eq!(group, vec![format!("BUNDLE {second}")]);
    Ok(())
}
