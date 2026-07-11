use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

                let name = request.url().trim_start_matches('/').to_string();
                if name.is_empty() || name.contains('/') || name.contains("..") {
                    let _ = request.respond(Response::empty(404));
                    continue;
                }

                match std::fs::read(dir.join(&name)) {
                    Ok(data) => {
                        debug!("GET /{name} -> {} bytes", data.len());
                        let mut response = Response::from_data(data);
                        for (key, value) in [
                            ("Content-Type", content_type(&name)),
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

fn content_type(name: &str) -> &'static str {
    if name.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if name.ends_with(".ts") {
        "video/mp2t"
    } else {
        "application/octet-stream"
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
    let socket = UdpSocket::bind("0.0.0.0:0").context("binding probe socket")?;
    socket
        .connect((target, 9))
        .context("probing route to device")?;
    Ok(socket.local_addr()?.ip())
}
