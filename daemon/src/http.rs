use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use parking_lot::Mutex;
use tiny_http::{Header, Response, Server, StatusCode};

/// Minimal HTTP server serving the HLS playlist and segments to the
/// Chromecast. Every URL is under an unguessable per-session token
/// (`/<token>/<file>`); anything else 404s, so other hosts on the LAN can't
/// pull the stream even though the socket is bound to all interfaces. Serves
/// only plain file names inside `dir` - no subdirectories.
pub struct HlsServer {
    pub port: u16,
    pub token: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

trait ServerHandler: Send + Sync + 'static {
    fn handle(&self, url: &str, name: &str) -> Option<ServerResponse>;
}

enum ServerResponse {
    File(Vec<u8>, Vec<(&'static str, &'static str)>),
    Stream(Box<dyn Read + Send>, &'static str),
}

struct FileServerHandler {
    dir: PathBuf,
}

impl ServerHandler for FileServerHandler {
    fn handle(&self, _url: &str, name: &str) -> Option<ServerResponse> {
        let data = std::fs::read(self.dir.join(name)).ok()?;
        let mut processed_data = data;
        if has_extension(name, "m3u8") {
            processed_data = inject_start_tag(processed_data);
        }
        debug!("GET /{name} -> {} bytes", processed_data.len());
        Some(ServerResponse::File(
            processed_data,
            vec![
                ("Content-Type", content_type(name)),
                ("Access-Control-Allow-Origin", "*"),
                ("Cache-Control", "no-cache, no-store"),
            ],
        ))
    }
}

struct AudioServerHandler {
    broadcaster: AudioBroadcaster,
    content_type: &'static str,
}

impl ServerHandler for AudioServerHandler {
    fn handle(&self, url: &str, _name: &str) -> Option<ServerResponse> {
        debug!("audio client connected: {url}");
        let reader = ChannelReader {
            rx: self.broadcaster.subscribe(),
            cur: Arc::from(Vec::new()),
            pos: 0,
        };
        Some(ServerResponse::Stream(Box::new(reader), self.content_type))
    }
}

fn start_http_server<H: ServerHandler>(
    handler: H,
    thread_name: &str,
    log_msg: &str,
) -> Result<HlsServer> {
    let server =
        Server::http("0.0.0.0:0").map_err(|e| anyhow::anyhow!("starting HTTP server: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .context("HTTP server has no IP address")?
        .port();
    let token = random_token();
    info!("{log_msg} on port {port}");

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let route_token = token.clone();
    let handler = Arc::new(handler);

    let handle = thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(200)) else {
                    continue;
                };

                let Some(name) = file_after_token(request.url(), &route_token) else {
                    let _ = request.respond(Response::empty(404));
                    continue;
                };

                match handler.handle(request.url(), name) {
                    Some(ServerResponse::File(data, headers)) => {
                        let mut resp = Response::from_data(data);
                        for (key, value) in headers {
                            if let Ok(h) = Header::from_bytes(key.as_bytes(), value.as_bytes()) {
                                resp.add_header(h);
                            }
                        }
                        let _ = request.respond(resp);
                    }
                    Some(ServerResponse::Stream(reader, content_type)) => {
                        let headers = vec![
                            ("Content-Type", content_type),
                            ("Cache-Control", "no-cache, no-store"),
                            ("Access-Control-Allow-Origin", "*"),
                        ]
                        .into_iter()
                        .filter_map(|(k, v)| Header::from_bytes(k.as_bytes(), v.as_bytes()).ok())
                        .collect();
                        let resp = Response::new(StatusCode(200), headers, reader, None, None);
                        let _ = request.respond(resp);
                    }
                    None => {
                        warn!("GET /{name} failed");
                        let _ = request.respond(Response::empty(404));
                    }
                }
            }
        })?;

    Ok(HlsServer {
        port,
        token,
        stop,
        handle: Some(handle),
    })
}

pub fn serve(dir: &Path) -> Result<HlsServer> {
    start_http_server(
        FileServerHandler {
            dir: dir.to_path_buf(),
        },
        "hls-http",
        &format!("serving {} on port", dir.display()),
    )
}

/// A random URL-path token guarding the stream so only the Chromecast (which we
/// hand the full URL) can fetch it, even on a shared LAN.
fn random_token() -> String {
    use std::fmt::Write as _;
    let bytes: [u8; 16] = rand::random();
    bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The file name in a `/<token>/<file>` request URL, or `None` if the token is
/// wrong or the file name is empty or unsafe (contains `/` or `..`).
fn file_after_token<'a>(url: &'a str, token: &str) -> Option<&'a str> {
    let (tok, name) = url.trim_start_matches('/').split_once('/')?;
    if tok != token || name.is_empty() || name.contains('/') || name.contains("..") {
        return None;
    }
    Some(name)
}

/// A live audio chunk shared with every connected client without copying.
type Chunk = Arc<[u8]>;

/// Fans a live encoded-audio byte stream out to the HTTP clients currently
/// connected (normally just the one Cast device). Cloneable: the encoder side
/// holds one handle to `push` chunks, the server thread holds another to
/// `subscribe` new clients.
#[derive(Clone)]
pub struct AudioBroadcaster {
    clients: Arc<Mutex<Vec<SyncSender<Chunk>>>>,
}

impl AudioBroadcaster {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Sends a chunk to every client. A full queue drops the chunk (a brief
    /// glitch) rather than stalling the encoder; a gone client is removed.
    pub fn push(&self, chunk: &Chunk) {
        self.clients.lock().retain(|client| {
            !matches!(
                client.try_send(Arc::clone(chunk)),
                Err(TrySendError::Disconnected(_))
            )
        });
    }

    fn subscribe(&self) -> Receiver<Chunk> {
        // Bounded so a stalled client bounds its memory; full sends are dropped
        // in `push` instead of blocking the encoder.
        let (tx, rx) = sync_channel(256);
        self.clients.lock().push(tx);
        rx
    }
}

/// A blocking `Read` over the chunks a client is subscribed to, so `tiny_http`
/// can stream an unbounded live response body. Reads block until the next chunk
/// arrives and report EOF once the broadcaster (and all its senders) is gone.
struct ChannelReader {
    rx: Receiver<Chunk>,
    cur: Chunk,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.cur.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.cur = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = (self.cur.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Serves the live progressive audio stream from `broadcaster` to whichever
/// client (the Cast device) connects, with the given `content_type`. Every
/// request gets its own streaming thread so one long-lived response doesn't
/// block the accept loop.
pub fn serve_audio(broadcaster: AudioBroadcaster, content_type: &'static str) -> Result<HlsServer> {
    start_http_server(
        AudioServerHandler {
            broadcaster,
            content_type,
        },
        "audio-http",
        &format!("serving live audio ({content_type})"),
    )
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

/// The local address the OS would use to reach `target` - i.e. the right
/// interface IP to put in the URL handed to the Chromecast.
pub fn local_ip_towards(target: IpAddr) -> Result<IpAddr> {
    let socket = crate::net::connected_udp(target, 9).context("probing route to device")?;
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

    #[test]
    fn token_gates_and_extracts_the_file_name() {
        // Right token: yields the file name.
        assert_eq!(
            file_after_token("/abc123/stream.m3u8", "abc123"),
            Some("stream.m3u8")
        );
        assert_eq!(
            file_after_token("/abc123/segment00007.ts", "abc123"),
            Some("segment00007.ts")
        );
        // Wrong or missing token, path traversal, or no file: rejected.
        assert_eq!(file_after_token("/wrong/stream.m3u8", "abc123"), None);
        assert_eq!(file_after_token("/stream.m3u8", "abc123"), None);
        assert_eq!(file_after_token("/abc123/", "abc123"), None);
        assert_eq!(file_after_token("/abc123/../secret", "abc123"), None);
        assert_eq!(file_after_token("/abc123/sub/dir.ts", "abc123"), None);
    }

    #[test]
    fn tokens_are_random_and_hex() {
        let (a, b) = (random_token(), random_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
