use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use tiny_http::{Header, Response, Server};

/// Minimal HTTP server serving the HLS playlist and segments to the
/// Chromecast. Serves only plain file names inside `dir` — no subdirectories.
pub struct HlsServer {
    pub port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

pub fn serve(dir: PathBuf) -> Result<HlsServer> {
    let server =
        Server::http("0.0.0.0:0").map_err(|e| anyhow::anyhow!("starting HTTP server: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .context("HTTP server has no IP address")?
        .port();
    info!("serving {} on port {port}", dir.display());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let handle = thread::Builder::new()
        .name("hls-http".into())
        .spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(200)) else {
                    continue;
                };

                let name = request.url().trim_start_matches('/');
                if name.is_empty() || name.contains('/') || name.contains("..") {
                    let _ = request.respond(Response::empty(404));
                    continue;
                }

                match std::fs::read(dir.join(name)) {
                    Ok(mut data) => {
                        if has_extension(name, "m3u8") {
                            data = inject_start_tag(data);
                        }
                        debug!("GET /{name} -> {} bytes", data.len());
                        let mut response = Response::from_data(data);
                        for (key, value) in [
                            ("Content-Type", content_type(name)),
                            // CAF/HLS playback on the Chromecast requires CORS.
                            ("Access-Control-Allow-Origin", "*"),
                            ("Cache-Control", "no-cache, no-store"),
                        ] {
                            if let Ok(h) = Header::from_bytes(key.as_bytes(), value.as_bytes()) {
                                response.add_header(h);
                            }
                        }
                        let _ = request.respond(response);
                    }
                    Err(e) => {
                        warn!("GET /{name} failed: {e}");
                        let _ = request.respond(Response::empty(404));
                    }
                }
            }
        })?;

    Ok(HlsServer {
        port,
        stop,
        handle: Some(handle),
    })
}

/// Tells the player to start 2s from the live edge. Without this, HLS players
/// pick their own live offset (Shaka and `ExoPlayer` default to 3 target
/// durations or more, measured from when they *parse* the playlist), which is
/// where most of the glass-to-glass lag comes from. Both honor EXT-X-START.
fn inject_start_tag(data: Vec<u8>) -> Vec<u8> {
    let text = match String::from_utf8(data) {
        Ok(text) => text,
        Err(e) => return e.into_bytes(),
    };
    if text.contains("#EXT-X-START") {
        return text.into_bytes();
    }
    text.replacen(
        "#EXTM3U",
        "#EXTM3U\n#EXT-X-START:TIME-OFFSET=-2.0,PRECISE=NO",
        1,
    )
    .into_bytes()
}

fn has_extension(name: &str, ext: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e == ext)
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("ts") => "video/mp2t",
        _ => "application/octet-stream",
    }
}

impl Drop for HlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The local address the OS would use to reach `target` — i.e. the right
/// interface IP to put in the URL handed to the Chromecast.
pub fn local_ip_towards(target: IpAddr) -> Result<IpAddr> {
    // The probe socket's family must match the target's, or connect() fails
    // with EAFNOSUPPORT (e.g. an IPv4-bound socket probing an IPv6 device).
    let bind_addr = match target {
        IpAddr::V4(_) => "0.0.0.0:0",
        IpAddr::V6(_) => "[::]:0",
    };
    let socket = UdpSocket::bind(bind_addr).context("binding probe socket")?;
    socket
        .connect((target, 9))
        .context("probing route to device")?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_tag_is_injected_after_header() {
        let playlist = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:1.0,\nsegment00000.ts\n".to_vec();
        let out = String::from_utf8(inject_start_tag(playlist)).unwrap();
        assert!(out.starts_with("#EXTM3U\n#EXT-X-START:TIME-OFFSET=-2.0,PRECISE=NO\n"));
        assert!(out.contains("segment00000.ts"));
    }

    #[test]
    fn existing_start_tag_is_kept() {
        let playlist = b"#EXTM3U\n#EXT-X-START:TIME-OFFSET=-5.0\n".to_vec();
        let out = String::from_utf8(inject_start_tag(playlist)).unwrap();
        assert_eq!(out.matches("#EXT-X-START").count(), 1);
    }
}
