//! Chrome-style Cast Streaming ("mirroring"): sub-second screen casting via
//! the 0F5096E8 receiver app, AES-encrypted RTP over UDP, and RTCP feedback.
//! Protocol ported from Chromium's openscreen `cast/streaming`.

mod channel;
mod crypto;
mod encoder;
mod messages;
mod rtcp;
mod rtp;
mod sender;

use std::net::UdpSocket;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};

use crate::SharedState;
use crate::capture::Capture;
use crate::discovery::Device;
use crate::pipeline::{self, StreamSettings};
use channel::{ChannelControl, ChannelEvent, MirrorChannel};
use sender::{ChunkSender, EncodedChunk, MediaSender, StreamConfig, StreamKind, chunk_channel};

const AUDIO_INDEX: u32 = 0;
/// First video stream index; each offered codec gets the next one up.
const VIDEO_INDEX_BASE: u32 = 1;
const AUDIO_BIT_RATE: u32 = 128_000;

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
        ssrc: ssrc_base + rand::random::<u32>() % 1000,
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
    // 1. Stream parameters. Mirroring caps at 1080p; VP8 software encoding
    // above that is not realtime, and receivers scale anyway.
    let (width, height) = settings.size.unwrap_or((1920, 1080));
    let fps = settings.fps;
    let video_bps = (settings.bitrate_kbps as u32) * 1000;

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
        bit_rate: AUDIO_BIT_RATE,
    });

    // One video variant per codec we can encode locally, best first; the
    // receiver picks one in its ANSWER. Empty for an audio-only cast.
    let codecs = if capture.is_some() {
        encoder::available_video_codecs()
    } else {
        Vec::new()
    };
    if capture.is_some() && codecs.is_empty() {
        return Outcome::Unavailable(anyhow!("no video encoder is installed"));
    }
    let video_params: Vec<messages::VideoParams> = codecs
        .iter()
        .enumerate()
        .map(|(i, codec)| {
            let keys = generate_keys(50_000 + (i as u32) * 1_000);
            messages::VideoParams {
                index: VIDEO_INDEX_BASE + i as u32,
                ssrc: keys.ssrc,
                aes_key: keys.aes_key,
                aes_iv_mask: keys.aes_iv_mask,
                codec_name: codec.codec_name(),
                max_bit_rate: video_bps,
                max_fps: fps as u32,
                width: width as u32,
                height: height as u32,
            }
        })
        .collect();
    let offer = messages::offer(1, audio_params.as_ref(), &video_params);

    // 2. Launch the mirroring app and negotiate (blocking I/O on a worker).
    let addr = device.addr;
    let port = device.port;
    info!(
        "negotiating mirroring session with {} ({addr})",
        device.name
    );
    let negotiation =
        tokio::task::spawn_blocking(move || MirrorChannel::negotiate(addr, port, offer)).await;
    let (mirror_channel, answer) = match negotiation {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return Outcome::Unavailable(e),
        Err(e) => return Outcome::Unavailable(anyhow!("negotiation task failed: {e}")),
    };
    // The receiver accepts one video variant; take our highest-priority one
    // (video_params is already in preference order). Keep only the scalar
    // stream data so nothing borrows video_params past here.
    let chosen_video = video_params
        .iter()
        .position(|p| answer.send_indexes.contains(&p.index))
        .map(|i| {
            (
                codecs[i],
                video_params[i].ssrc,
                video_params[i].aes_key,
                video_params[i].aes_iv_mask,
            )
        });
    if capture.is_some() && chosen_video.is_none() {
        return Outcome::Unavailable(anyhow!(
            "receiver accepted none of the offered video codecs"
        ));
    }
    let audio_accepted = audio_params.is_some() && answer.send_indexes.contains(&AUDIO_INDEX);
    if capture.is_none() && !audio_accepted {
        return Outcome::Unavailable(anyhow!("receiver did not accept the audio stream"));
    }

    // The encoder branch for the negotiated codec (None for an audio-only cast).
    let video_encoder_desc = match chosen_video {
        Some((codec, ..)) => match encoder::video_encoder(codec, video_bps, fps as u32) {
            Some(desc) => Some(desc),
            None => {
                return Outcome::Unavailable(anyhow!("no encoder for the negotiated video codec"));
            }
        },
        None => None,
    };

    // From here on the app is running; ChannelControl's Drop stops it again.
    let (channel_events_tx, mut channel_events) = mpsc::unbounded_channel();
    let channel_control = ChannelControl::spawn(mirror_channel, channel_events_tx);

    // 3. Media transport socket.
    let socket = match udp_socket_towards(addr, answer.udp_port) {
        Ok(s) => s,
        Err(e) => return Outcome::Unavailable(e),
    };

    // 4. Encoder pipeline feeding the sender thread through a channel.
    let (chunks_tx, chunks_rx) = chunk_channel();
    let pipeline_result = build_pipeline(
        capture,
        settings,
        (width, height),
        video_encoder_desc.as_deref(),
        audio_monitor.as_deref().filter(|_| audio_accepted),
        &chunks_tx,
    );
    drop(chunks_tx); // the appsink callbacks hold their own clones
    let pipeline = match pipeline_result {
        Ok(p) => p,
        Err(e) => return Outcome::Unavailable(e.context("building mirroring pipeline")),
    };

    let mut stream_configs = Vec::with_capacity(2);
    if let Some((_, ssrc, aes_key, aes_iv_mask)) = chosen_video {
        stream_configs.push(StreamConfig {
            kind: StreamKind::Video,
            ssrc,
            payload_type: messages::VIDEO_PAYLOAD_TYPE,
            aes_key,
            aes_iv_mask,
        });
    }
    if audio_accepted {
        stream_configs.push(StreamConfig {
            kind: StreamKind::Audio,
            ssrc: audio_keys.ssrc,
            payload_type: messages::AUDIO_PAYLOAD_TYPE,
            aes_key: audio_keys.aes_key,
            aes_iv_mask: audio_keys.aes_iv_mask,
        });
    }

    let encoder = pipeline.by_name("venc");
    let request_keyframe: Box<dyn Fn() + Send> = Box::new(move || {
        if let Some(enc) = &encoder {
            let s = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            enc.send_event(gst::event::CustomUpstream::new(s));
        }
    });
    let media_sender = MediaSender::spawn(socket, stream_configs, chunks_rx, request_keyframe);

    // 5. Run.
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        return Outcome::Finished(Err(anyhow!("starting mirroring pipeline: {e}")));
    }
    let _pipeline_stop = PipelineStop(pipeline.clone());
    // Codecs the receiver accepted from our OFFER, for the "show details" line.
    let receiver_codecs: Vec<String> = video_params
        .iter()
        .enumerate()
        .filter(|(_, p)| answer.send_indexes.contains(&p.index))
        .map(|(i, _)| codecs[i].codec_name().to_string())
        .collect();
    if let Some((codec, ..)) = chosen_video {
        info!(
            "mirroring started ({} {width}x{height} @{fps}fps, {video_bps} bps)",
            codec.codec_name()
        );
        state.set_details("mirror", codec.codec_name(), receiver_codecs);
    } else {
        info!("audio-only mirroring started ({AUDIO_BIT_RATE} bps)");
        state.set_details("mirror", "opus", Vec::new());
    }
    state.set_status("casting", &device.id);

    let bus = pipeline.bus();
    let result = loop {
        tokio::select! {
            _ = &mut *stop_rx => {
                info!("stop requested");
                break Ok(());
            }
            event = channel_events.recv() => match event {
                Some(ChannelEvent::Ended(reason)) => {
                    info!("device ended the mirroring session: {reason}");
                    break Ok(());
                }
                None => break Ok(()),
            },
            () = tokio::time::sleep(Duration::from_millis(500)) => {
                if let Some(error) = bus.as_ref().and_then(pop_bus_error) {
                    break Err(error);
                }
            }
        }
    };

    // Stop the encoder first so no more frames are produced, then the sender
    // (explicit stop flag; it can't wait on the appsink channel closing), then
    // the control channel, which stops the receiver app.
    let _ = pipeline.set_state(gst::State::Null);
    drop(media_sender);
    drop(channel_control);
    Outcome::Finished(result)
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

/// Binds a UDP socket and connects it to the receiver's negotiated port, so
/// plain `send()` works and the receiver learns our address from the traffic.
fn udp_socket_towards(addr: std::net::IpAddr, port: u16) -> Result<UdpSocket> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("binding RTP socket")?;
    socket
        .connect((addr, port))
        .context("connecting RTP socket")?;
    Ok(socket)
}

fn build_pipeline(
    capture: Option<&Capture>,
    settings: &StreamSettings,
    (width, height): (i32, i32),
    video_encoder: Option<&str>,
    audio_monitor: Option<&str>,
    chunks_tx: &ChunkSender,
) -> Result<gst::Pipeline> {
    use std::fmt::Write as _;

    let mut desc = String::new();
    // The video branch exists only when we have both a capture and a chosen
    // encoder (audio-only casts have neither). `video_encoder` already carries
    // its codec, bitrate and low-latency settings and names the element `venc`.
    if let (Some(capture), Some(venc)) = (capture, video_encoder) {
        let fps = settings.fps;
        let fd = capture.fd.as_raw_fd();
        let node = capture.node_id;
        let _ = write!(
            desc,
            "pipewiresrc fd={fd} path={node} do-timestamp=true keepalive-time=1000 resend-last=true \
             ! queue leaky=downstream max-size-buffers=3 max-size-bytes=0 max-size-time=0 \
             ! videoconvert ! videoscale ! videorate \
             ! video/x-raw,format=I420,framerate={fps}/1,width={width},height={height},pixel-aspect-ratio=1/1 \
             ! {venc} ! appsink name=vsink sync=false max-buffers=32 "
        );
    }
    if let Some(monitor) = audio_monitor {
        let _ = write!(
            desc,
            "pulsesrc device={monitor} provide-clock=false \
             ! queue ! audioconvert ! audioresample \
             ! audio/x-raw,rate=48000,channels=2 \
             ! opusenc bitrate={AUDIO_BIT_RATE} \
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
                let rtp_timestamp =
                    ((u128::from(pts_ns) * u128::from(timebase)) / 1_000_000_000) as u32;
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

struct PipelineStop(gst::Pipeline);

impl Drop for PipelineStop {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}
