//! Chrome-style Cast Streaming ("mirroring"): sub-second screen casting via
//! the 0F5096E8 receiver app, AES-encrypted RTP over UDP, and RTCP feedback.
//!
//! The wire protocol itself (OFFER/ANSWER, RTP, RTCP, crypto) is a port of
//! Chromium's openscreen and lives in the [`openscreen`] submodule under its
//! own BSD-3-Clause licence; this module and [`channel`]/[`encoder`] are the
//! original glue that drives it.

mod channel;
pub mod encoder;
pub mod ladder;
mod openscreen;
pub mod quality;

use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};

use crate::capture::Capture;
use crate::discovery::Device;
use crate::pipeline::{self, PipelineStop, StreamSettings};
use crate::{CastDetails, SharedState};
use channel::{ChannelControl, ChannelEvent, MirrorChannel};
use openscreen::sender::{
    ChunkSender, EncodedChunk, MediaSender, RateControl, StreamConfig, StreamKind, chunk_channel,
};
use openscreen::{messages, rtcp};

const AUDIO_INDEX: u32 = 0;
/// First video stream index; each offered codec gets the next one up.
const VIDEO_INDEX_BASE: u32 = 1;

pub enum Outcome {
    /// Mirroring ran (successfully or not); do not fall back.
    Finished(Result<()>),
    /// Mirroring could not be established; the caller should fall back to HLS.
    Unavailable(anyhow::Error),
}

struct StreamKeys {
    ssrc: u32,
    aes_key: [u8; 16],
    aes_iv_mask: [u8; 16],
}

fn generate_keys(ssrc_base: u32) -> StreamKeys {
    StreamKeys {
        ssrc: ssrc_base.wrapping_add(rand::random::<u32>() % 1000),
        aes_key: rand::random(),
        aes_iv_mask: rand::random(),
    }
}

/// Runs a mirroring session end to end. `capture` is `None` for audio-only
/// casts (speakers): the OFFER then carries only the Opus stream. Returns
/// `Unavailable` only for failures before any media flowed (negotiation), so
/// the caller can fall back to HLS.
pub async fn run(
    state: &Arc<SharedState>,
    device: &Device,
    capture: Option<&Capture>,
    settings: &StreamSettings,
    stop_rx: &mut oneshot::Receiver<()>,
) -> Outcome {
    // 1. Stream parameters. "Native" (no explicit size) is advertised to the
    // receiver as 1080p; it scales anyway.
    // Offered before the receiver has told us anything, so only our own
    // limits apply; the ANSWER may narrow every one of these.
    let offered = settings.resolve_local();
    let video_bps = u32::try_from(offered.video_bitrate_kbps)
        .unwrap_or(0)
        .saturating_mul(1000);

    let audio_monitor = pipeline::default_audio_monitor().await;
    match (capture, &audio_monitor) {
        (None, None) => {
            return Outcome::Unavailable(anyhow!(
                "audio-only cast but no system audio monitor was found"
            ));
        }
        (Some(_), None) => warn!("no audio monitor found, mirroring video only"),
        _ => {}
    }

    let audio_keys = generate_keys(1_000);
    let audio_params = audio_monitor.as_ref().map(|_| messages::AudioParams {
        index: AUDIO_INDEX,
        ssrc: audio_keys.ssrc,
        aes_key: audio_keys.aes_key,
        aes_iv_mask: audio_keys.aes_iv_mask,
        bit_rate: u32::try_from(offered.audio_bitrate_bps).unwrap_or(128_000),
    });

    let codecs = match offer_codecs(capture, settings) {
        Ok(codecs) => codecs,
        Err(outcome) => return outcome,
    };
    let video_params = video_params(&codecs, offered.size, offered.fps, video_bps);
    let offer = messages::offer(1, audio_params.as_ref(), &video_params);

    // 2. Launch the mirroring app and negotiate (blocking I/O on a worker).
    let addr = device.addr;
    let port = device.port;
    info!(
        "negotiating mirroring session with {} ({addr})",
        device.name
    );
    let negotiation =
        tokio::task::spawn_blocking(move || MirrorChannel::negotiate(addr, port, &offer)).await;
    let (mirror_channel, answer) = match negotiation {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return Outcome::Unavailable(e),
        Err(e) => return Outcome::Unavailable(anyhow!("negotiation task failed: {e}")),
    };
    // The receiver accepts one video variant; take our highest-priority one
    // (video_params is already in preference order). Keep only the scalar
    // stream data so nothing borrows video_params past here.
    // The receiver's constraints outrank the request; automatic settings are
    // filled in from them entirely.
    let resolved = settings.resolve(&answer.constraints);
    let selected = match select_streams(
        &answer,
        &video_params,
        &codecs,
        (audio_params.is_some(), capture.is_some()),
        settings,
        &resolved,
    ) {
        Ok(selected) => selected,
        Err(e) => return Outcome::Unavailable(e),
    };
    let (chosen_video, audio_accepted, video_encoder) =
        (selected.video, selected.audio, selected.encoder);

    // From here on the app is running; ChannelControl's Drop stops it again.
    let (channel_events_tx, mut channel_events) = mpsc::unbounded_channel();
    let channel_control = ChannelControl::spawn(mirror_channel, channel_events_tx);

    // 3+4. RTP socket, then the encoder pipeline feeding the sender thread.
    let media = start_media(
        (addr, answer.udp_port),
        capture,
        settings,
        &resolved,
        &MediaStreams {
            video: chosen_video,
            audio: audio_accepted.then_some(&audio_keys),
            encoder: video_encoder.as_ref().map(|(desc, _)| desc.as_str()),
            audio_monitor: audio_monitor.as_deref().filter(|_| audio_accepted),
        },
    );
    let (pipeline, media_sender) = match media {
        Ok(started) => started,
        Err(e) => return Outcome::Unavailable(e),
    };

    // 5. Run.
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        return Outcome::Finished(Err(anyhow!("starting mirroring pipeline: {e}")));
    }
    let _pipeline_stop = PipelineStop(pipeline.clone());
    announce(
        state,
        device,
        chosen_video.map(|(codec, ..)| codec),
        &pipeline,
        video_encoder.as_ref().is_some_and(|(_, hw)| *hw),
        accepted_codec_names(&answer, &video_params, &codecs),
        &resolved,
    );

    let result = run_until_stopped(state, capture, &pipeline, stop_rx, &mut channel_events).await;

    // Stop the encoder first so no more frames are produced, then the sender
    // (explicit stop flag; it can't wait on the appsink channel closing), then
    // the control channel, which stops the receiver app.
    let _ = pipeline.set_state(gst::State::Null);
    drop(media_sender);
    drop(channel_control);
    Outcome::Finished(result)
}

/// One video variant per codec we can encode locally, best first; the receiver
/// picks one in its ANSWER. Empty for an audio-only cast; `Err` carries the
/// outcome `run` returns when nothing can be offered.
fn offer_codecs(
    capture: Option<&Capture>,
    settings: &StreamSettings,
) -> Result<Vec<encoder::VideoCodec>, Outcome> {
    if capture.is_none() {
        return Ok(Vec::new());
    }
    let codecs = encoder::available_video_codecs(settings.encoding, settings.video_codec);
    if !codecs.is_empty() {
        return Ok(codecs);
    }
    let error = match settings.video_codec {
        Some(codec) => anyhow!(
            "no {} encoder matches the selected encoder and pixel format",
            codec.codec_name()
        ),
        None => anyhow!(encoder::policy_failure_message(settings.encoding)),
    };
    // A forced setting that rules everything out is reported, not worked
    // around: falling back to HLS would quietly defeat the user's choice.
    // With no forced setting this is just "nothing installed" - still worth
    // trying HLS, which needs only H.264.
    Err(
        if settings.encoding == encoder::EncodingPolicy::default() && settings.video_codec.is_none()
        {
            Outcome::Unavailable(error)
        } else {
            Outcome::Finished(Err(error))
        },
    )
}

/// Codecs the receiver accepted from our OFFER, for "show details".
fn accepted_codec_names(
    answer: &messages::Answer,
    video_params: &[messages::VideoParams],
    codecs: &[encoder::VideoCodec],
) -> Vec<String> {
    video_params
        .iter()
        .zip(codecs.iter())
        .filter(|(p, _)| answer.send_indexes.contains(&p.index))
        .map(|(_, codec)| codec.codec_name().to_owned())
        .collect()
}

/// What `start_media` needs to know about the negotiated streams.
struct MediaStreams<'a> {
    video: Option<(encoder::VideoCodec, u32, [u8; 16], [u8; 16])>,
    audio: Option<&'a StreamKeys>,
    encoder: Option<&'a str>,
    audio_monitor: Option<&'a str>,
}

/// Opens the RTP socket and builds the encoder pipeline that feeds it.
fn start_media(
    peer: (std::net::IpAddr, u16),
    capture: Option<&Capture>,
    settings: &StreamSettings,
    resolved: &quality::Resolved,
    streams: &MediaStreams<'_>,
) -> Result<(gst::Pipeline, MediaSender)> {
    // Probe the route first (a connect only consults the routing table), then
    // send from an unconnected socket: connecting would pin our local address,
    // and a receiver may answer RTCP to a different address of ours than the
    // one the route picked, which the kernel would then discard.
    drop(crate::net::connected_udp(peer.0, peer.1).context("connecting the RTP socket")?);
    let socket = crate::net::bound_udp(peer.0).context("opening the RTP socket")?;

    let (chunks_tx, chunks_rx) = chunk_channel();
    let pipeline = build_pipeline(
        capture,
        settings,
        resolved,
        streams.encoder,
        streams.audio_monitor,
        &chunks_tx,
    );
    drop(chunks_tx); // the appsink callbacks hold their own clones
    let pipeline = pipeline.context("building mirroring pipeline")?;

    let configs = stream_configs(streams.video, streams.audio);
    let sender = MediaSender::spawn(
        socket,
        std::net::SocketAddr::new(peer.0, peer.1),
        configs,
        chunks_rx,
        keyframe_forcer(&pipeline),
        rate_control(&pipeline, settings, resolved),
    );
    Ok((pipeline, sender))
}

/// The bitrate window the control loop may move in. An explicitly chosen
/// bitrate is left alone; only automatic follows the link.
fn rate_control(
    pipeline: &gst::Pipeline,
    settings: &StreamSettings,
    resolved: &quality::Resolved,
) -> RateControl {
    let start_bps = u32::try_from(resolved.video_bitrate_kbps)
        .unwrap_or(0)
        .saturating_mul(1000);
    let adaptive = settings.bitrate_kbps.is_none();
    // A pinned resolution is honoured; only automatic climbs the ladder.
    let adaptive_size = settings.size.is_none();
    let weak = pipeline.downgrade();
    let weak_size = pipeline.downgrade();
    RateControl {
        start_bps,
        min_bps: u32::try_from(resolved.video_min_bitrate_bps).unwrap_or(300_000),
        max_bps: u32::try_from(resolved.video_max_bitrate_bps).unwrap_or(start_bps),
        set_bitrate: adaptive.then(|| -> openscreen::sender::SetBitrate {
            Box::new(move |bps: u32| {
                if let Some(pipeline) = weak.upgrade() {
                    pipeline::set_encoder_bitrate(&pipeline, bps);
                }
            })
        }),
        set_size: adaptive_size.then(|| -> openscreen::sender::SetSize {
            Box::new(move |size: (i32, i32)| {
                if let Some(pipeline) = weak_size.upgrade() {
                    pipeline::set_capture_size(&pipeline, size);
                }
            })
        }),
        sizes: ladder::snapped_sizes(resolved.size, (320, 180), resolved.size),
        start_size: resolved.size,
    }
}

/// What the receiver accepted from the OFFER, and the encoder to feed it.
struct Selected {
    video: Option<(encoder::VideoCodec, u32, [u8; 16], [u8; 16])>,
    audio: bool,
    encoder: Option<(String, bool)>,
}

/// Matches the ANSWER against what was offered. `offered` is
/// (audio was offered, this is a screen cast).
fn select_streams(
    answer: &messages::Answer,
    video_params: &[messages::VideoParams],
    codecs: &[encoder::VideoCodec],
    offered: (bool, bool),
    settings: &StreamSettings,
    resolved: &quality::Resolved,
) -> Result<Selected> {
    let (audio_offered, has_capture) = offered;
    // The receiver accepts one video variant; take our highest-priority one
    // (video_params is already in preference order).
    let video = video_params
        .iter()
        .zip(codecs.iter())
        .find(|(p, _)| answer.send_indexes.contains(&p.index))
        .map(|(p, codec)| (*codec, p.ssrc, p.aes_key, p.aes_iv_mask));
    if has_capture && video.is_none() {
        bail!("receiver accepted none of the offered video codecs");
    }
    let audio = audio_offered && answer.send_indexes.contains(&AUDIO_INDEX);
    if !has_capture && !audio {
        bail!("receiver did not accept the audio stream");
    }

    let video_bps = u32::try_from(resolved.video_bitrate_kbps)
        .unwrap_or(0)
        .saturating_mul(1000);
    let fps = u32::try_from(resolved.fps).unwrap_or(30);
    let encoder = match video {
        // (launch fragment, is-hardware)
        // Same policy as the OFFER above, or we would advertise a codec we then
        // refuse to encode and the receiver would already have committed to it.
        Some((codec, ..)) => match encoder::video_encoder(codec, video_bps, fps, settings.encoding)
        {
            Some(picked) => Some(picked),
            None => bail!("no encoder for the negotiated video codec"),
        },
        None => None,
    };
    Ok(Selected {
        video,
        audio,
        encoder,
    })
}

/// Logs what was negotiated and moves the session to "casting".
fn announce(
    state: &Arc<SharedState>,
    device: &Device,
    codec: Option<encoder::VideoCodec>,
    pipeline: &gst::Pipeline,
    hardware: bool,
    receiver_codecs: Vec<String>,
    resolved: &quality::Resolved,
) {
    if let Some(codec) = codec {
        let (width, height) = resolved.size;
        let element = pipeline::encoder_element(pipeline).unwrap_or_default();
        info!(
            "mirroring started ({} via {element} {}, {width}x{height} @{}fps, {} kbit/s)",
            codec.codec_name(),
            if hardware { "hardware" } else { "software" },
            resolved.fps,
            resolved.video_bitrate_kbps,
        );
        state.set_details(CastDetails {
            transport: "mirror".to_owned(),
            codec: codec.codec_name().to_owned(),
            encoder: element,
            format: String::new(),
            receiver_codecs,
        });
    } else {
        info!(
            "audio-only mirroring started ({} bps)",
            resolved.audio_bitrate_bps
        );
        state.set_details(CastDetails {
            transport: "mirror".to_owned(),
            codec: "opus".to_owned(),
            ..CastDetails::default()
        });
    }
    state.set_status("casting", &device.id);
}

/// One OFFER variant per codec we can encode, each with its own SSRC and key.
fn video_params(
    codecs: &[encoder::VideoCodec],
    (width, height): (i32, i32),
    fps: i32,
    video_bps: u32,
) -> Vec<messages::VideoParams> {
    codecs
        .iter()
        .enumerate()
        .map(|(i, codec)| {
            let index = u32::try_from(i).unwrap_or(0);
            let keys = generate_keys(50_000_u32.saturating_add(index.saturating_mul(1_000)));
            messages::VideoParams {
                index: VIDEO_INDEX_BASE.saturating_add(index),
                ssrc: keys.ssrc,
                aes_key: keys.aes_key,
                aes_iv_mask: keys.aes_iv_mask,
                codec_name: codec.codec_name(),
                max_bit_rate: video_bps,
                max_fps: u32::try_from(fps).unwrap_or(0),
                width: u32::try_from(width).unwrap_or(0),
                height: u32::try_from(height).unwrap_or(0),
            }
        })
        .collect()
}

/// The RTP streams the receiver accepted, in the order the sender expects.
fn stream_configs(
    video: Option<(encoder::VideoCodec, u32, [u8; 16], [u8; 16])>,
    audio: Option<&StreamKeys>,
) -> Vec<StreamConfig> {
    let mut configs = Vec::with_capacity(2);
    if let Some((_, ssrc, aes_key, aes_iv_mask)) = video {
        configs.push(StreamConfig {
            kind: StreamKind::Video,
            ssrc,
            payload_type: messages::VIDEO_PAYLOAD_TYPE,
            aes_key,
            aes_iv_mask,
        });
    }
    if let Some(keys) = audio {
        configs.push(StreamConfig {
            kind: StreamKind::Audio,
            ssrc: keys.ssrc,
            payload_type: messages::AUDIO_PAYLOAD_TYPE,
            aes_key: keys.aes_key,
            aes_iv_mask: keys.aes_iv_mask,
        });
    }
    configs
}

/// Asks the encoder for a key frame; the sender calls this on picture loss.
fn keyframe_forcer(pipeline: &gst::Pipeline) -> Box<dyn Fn() + Send> {
    let encoder = pipeline.by_name("venc");
    Box::new(move || {
        if let Some(enc) = &encoder {
            let forced = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            enc.send_event(gst::event::CustomUpstream::new(forced));
        }
    })
}

/// Runs until the user stops the cast, the compositor ends the screen share,
/// the device drops the session, or the pipeline errors.
async fn run_until_stopped(
    state: &Arc<SharedState>,
    capture: Option<&Capture>,
    pipeline: &gst::Pipeline,
    stop_rx: &mut oneshot::Receiver<()>,
    channel_events: &mut mpsc::UnboundedReceiver<ChannelEvent>,
) -> Result<()> {
    let bus = pipeline.bus();
    // Subscribed once, outside the loop: re-creating it each iteration would
    // resubscribe every tick and could miss the signal in between.
    let capture_closed = capture_closed(capture);
    tokio::pin!(capture_closed);
    loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                info!("stop requested");
                return Ok(());
            }
            () = &mut capture_closed => {
                info!("screen sharing was stopped from the system menu");
                return Ok(());
            }
            event = channel_events.recv() => match event {
                Some(ChannelEvent::Ended(reason)) => {
                    info!("device ended the mirroring session: {reason}");
                    state.set_last_event("ended", &reason);
                    return Ok(());
                }
                None => return Ok(()),
            },
            () = tokio::time::sleep(Duration::from_millis(500)) => {
                if let Some(error) = bus.as_ref().and_then(pop_bus_error) {
                    return Err(error);
                }
                // Caps are only fixed once the pipeline has prerolled, so this
                // is read here rather than at announce time. Once is enough.
                if state.details().format.is_empty()
                    && let Some(format) = pipeline::negotiated_format(pipeline)
                {
                    info!("encoder negotiated {format}");
                    state.set_detail_format(&format);
                }
            }
        }
    }
}

/// Pends forever for an audio-only cast, which has no portal session to lose.
async fn capture_closed(capture: Option<&Capture>) {
    match capture {
        Some(capture) => capture.closed().await,
        None => std::future::pending().await,
    }
}

fn pop_bus_error(bus: &gst::Bus) -> Option<anyhow::Error> {
    use gst::MessageView;
    while let Some(message) = bus.pop() {
        if let MessageView::Error(e) = message.view() {
            return Some(anyhow!("mirroring pipeline error: {}", e.error()));
        }
    }
    None
}

fn build_pipeline(
    capture: Option<&Capture>,
    settings: &StreamSettings,
    resolved: &quality::Resolved,
    video_encoder: Option<&str>,
    audio_monitor: Option<&str>,
    chunks_tx: &ChunkSender,
) -> Result<gst::Pipeline> {
    use std::fmt::Write as _;

    let mut desc = String::new();
    // The video branch exists only when we have both a capture and a chosen
    // encoder (audio-only casts have neither). `video_encoder` already carries
    // its codec, bitrate and low-latency settings and names the element `venc`.
    // The format must stay a set unless forced: VA-API encoders take only NV12,
    // the VPX/AV1 ones only I420, and leaving it open lets videoconvert pick
    // 4:4:4. A forced value already filtered the encoder list, so it can link.
    if let (Some(capture), Some(venc)) = (capture, video_encoder) {
        let (width, height) = resolved.size;
        let fps = resolved.fps;
        let format = settings.encoding.format.caps_format();
        let fd = capture.fd.as_raw_fd();
        let node = capture.node_id;
        let _ = write!(
            desc,
            "pipewiresrc fd={fd} path={node} do-timestamp=true keepalive-time=1000 resend-last=true \
             ! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0 \
             ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,format={format},framerate={fps}/1,width={width},height={height},pixel-aspect-ratio=1/1 \
             ! {venc} ! appsink name=vsink sync=false max-buffers=32 "
        );
    }
    if let Some(monitor) = audio_monitor {
        let audio_bps = resolved.audio_bitrate_bps;
        let (rate, channels) = (resolved.sample_rate, resolved.channels);
        let _ = write!(
            desc,
            "pulsesrc device={monitor} provide-clock=false \
             ! queue ! audioconvert ! audioresample \
             ! audio/x-raw,rate={rate},channels={channels} \
             ! opusenc bitrate={audio_bps} \
             ! appsink name=asink sync=false max-buffers=32"
        );
    }
    info!("mirror pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)
        .context(
            "parsing the mirroring pipeline (are the encoder plugins installed for the negotiated codec?)",
        )?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("parsed element is not a pipeline"))?;

    if video_encoder.is_some() {
        attach_appsink(
            &pipeline,
            "vsink",
            StreamKind::Video,
            messages::VIDEO_RTP_TIMEBASE,
            chunks_tx.clone(),
        )?;
    }
    if audio_monitor.is_some() {
        attach_appsink(
            &pipeline,
            "asink",
            StreamKind::Audio,
            messages::AUDIO_RTP_TIMEBASE,
            chunks_tx.clone(),
        )?;
    }
    Ok(pipeline)
}

fn attach_appsink(
    pipeline: &gst::Pipeline,
    name: &str,
    kind: StreamKind,
    timebase: u32,
    chunks: ChunkSender,
) -> Result<()> {
    let sink = pipeline
        .by_name(name)
        .and_then(|e| e.downcast::<AppSink>().ok())
        .ok_or_else(|| anyhow!("pipeline has no appsink named {name}"))?;

    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buffer) = sample.buffer_owned() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let pts_ns = buffer.pts().map_or(0, gstreamer::ClockTime::nseconds);
                let rtp_timestamp = u128::from(pts_ns)
                    .saturating_mul(u128::from(timebase))
                    .checked_div(1_000_000_000)
                    .and_then(|ticks| u32::try_from(ticks & u128::from(u32::MAX)).ok())
                    .unwrap_or(0);
                let is_key_frame = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
                // Ship the mapped buffer itself; the sender thread reads the
                // encoded bytes in place instead of from a copy.
                let Ok(data) = buffer.into_mapped_buffer_readable() else {
                    return Err(gst::FlowError::Error);
                };
                let chunk = EncodedChunk {
                    kind,
                    is_key_frame,
                    rtp_timestamp,
                    ntp_timestamp: rtcp::ntp_now(),
                    data,
                };
                chunks.send(chunk);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(())
}
