//! Video codec and encoder selection for the Cast Streaming (mirroring) path.
//!
//! The RTP/RTCP/crypto layer is codec-agnostic - it packetizes whole encrypted
//! frames - so the only codec-specific parts of mirroring are the `codecName`
//! advertised in the OFFER and the `GStreamer` encoder element. This module
//! owns both: which codecs we can encode locally, and the encoder for each,
//! **preferring hardware** (VA-API/NVENC) over software.
//!
//! Every candidate fragment is parse-checked before use, so a hardware encoder
//! that is present but mis-parametrised falls back to the next candidate (and
//! ultimately software) rather than failing the cast.

use gstreamer as gst;
use gstreamer::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoCodec {
    Vp8,
    Vp9,
    Av1,
    H264,
}

impl VideoCodec {
    /// The `codecName` string used in the Cast OFFER.
    pub fn codec_name(self) -> &'static str {
        match self {
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::H264 => "h264",
        }
    }
}

/// Which encoders the user will accept. `Auto` keeps the hardware-first order in
/// `factories`; the rest are escape hatches - `Software` for a driver that is
/// present but misbehaving, and the three API choices for a machine with more
/// than one hardware path, where the automatic order picks the wrong one (a
/// hybrid laptop always prefers its integrated VA-API encoder over NVENC).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EncoderPolicy {
    #[default]
    Auto,
    Hardware,
    Software,
    VaApi,
    Nvenc,
    V4l2,
}

/// The raw format fed to the encoder. Forcing one narrows the candidate list
/// rather than the caps, because an encoder that cannot take the format would
/// otherwise fail to link.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FormatPolicy {
    #[default]
    Auto,
    Nv12,
    I420,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EncodingPolicy {
    pub encoder: EncoderPolicy,
    pub format: FormatPolicy,
}

impl EncoderPolicy {
    /// Anything unrecognised means `Auto`: the extension sending the option can
    /// be a different version than the daemon reading it.
    pub fn parse(value: &str) -> Self {
        match value {
            "hardware" => Self::Hardware,
            "software" => Self::Software,
            "vaapi" => Self::VaApi,
            "nvenc" => Self::Nvenc,
            "v4l2" => Self::V4l2,
            _ => Self::Auto,
        }
    }
}

impl FormatPolicy {
    pub fn parse(value: &str) -> Self {
        match value {
            "nv12" => Self::Nv12,
            "i420" => Self::I420,
            _ => Self::Auto,
        }
    }

    /// The `format` field for the raw caps feeding the encoder. `Auto` offers
    /// both, letting negotiation pick NV12 for VA-API and I420 for the rest -
    /// leaving it out entirely would let videoconvert choose 4:4:4.
    pub fn caps_format(self) -> &'static str {
        match self {
            Self::Auto => "{NV12,I420}",
            Self::Nv12 => "NV12",
            Self::I420 => "I420",
        }
    }

    /// The format an encoder is required to accept, or `None` when unconstrained.
    fn required(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Nv12 => Some("NV12"),
            Self::I420 => Some("I420"),
        }
    }
}

/// Efficiency order, best first - used to break ties among codecs at the same
/// hardware tier. VP8 is last and mandatory (every Cast-V2 receiver decodes
/// it), so it is the guaranteed fallback.
const EFFICIENCY_ORDER: [VideoCodec; 4] = [
    VideoCodec::Av1,
    VideoCodec::Vp9,
    VideoCodec::H264,
    VideoCodec::Vp8,
];

fn efficiency_rank(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::Av1 => 0,
        VideoCodec::Vp9 => 1,
        VideoCodec::H264 => 2,
        VideoCodec::Vp8 => 3,
    }
}

/// `GStreamer` encoder factories to try for `codec`, best first: hardware
/// (VA-API, then NVENC) ahead of software.
fn factories(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        // The v4l2* elements exist only when a kernel device advertises that
        // codec, which is how Arm boards (Raspberry Pi, Rockchip, Amlogic)
        // expose their encoders. Absent everywhere else, and dropped by the
        // usability check when they are.
        VideoCodec::H264 => &[
            "vah264enc",
            "vah264lpenc",
            "nvh264enc",
            "v4l2h264enc",
            "x264enc",
        ],
        VideoCodec::Vp8 => &["vavp8enc", "vavp8lpenc", "v4l2vp8enc", "vp8enc"],
        // Intel encodes VP9 only in low power mode, so the `lp` element is the
        // only VA-API VP9 encoder that exists on most machines.
        VideoCodec::Vp9 => &["vavp9enc", "vavp9lpenc", "v4l2vp9enc", "vp9enc"],
        // SVT-AV1 is far faster than aom's av1enc, so prefer it in software.
        VideoCodec::Av1 => &[
            "vaav1enc",
            "vaav1lpenc",
            "nvav1enc",
            "v4l2av1enc",
            "svtav1enc",
            "av1enc",
        ],
    }
}

/// The hardware API an encoder element belongs to, or `None` for software. The
/// three prefixes are how `GStreamer` names these plugins' elements, and each
/// one reaches different silicon: VA-API for Intel and AMD, NVENC for NVIDIA,
/// V4L2 for the stateful encoders on Arm boards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Api {
    Va,
    Nvenc,
    V4l2,
}

fn api_of(factory: &str) -> Option<Api> {
    // v4l2 first: it is the only prefix that could be read as another's.
    if factory.starts_with("v4l2") {
        Some(Api::V4l2)
    } else if factory.starts_with("va") {
        Some(Api::Va)
    } else if factory.starts_with("nv") {
        Some(Api::Nvenc)
    } else {
        None
    }
}

/// VA-API / NVENC / V4L2 elements are hardware; everything else is software.
fn is_hardware(factory: &str) -> bool {
    api_of(factory).is_some()
}

/// Whether `factory` accepts raw video in `format` on its sink pad, read from
/// the element's own template caps so it stays right across plugin versions.
/// The probe carries no caps features, so it matches only the system-memory
/// variant - which is the one the pipeline actually feeds.
fn accepts_format(factory: &str, format: &str) -> bool {
    let Some(found) = gst::ElementFactory::find(factory) else {
        return false;
    };
    let probe = gst::Caps::builder("video/x-raw")
        .field("format", format)
        .build();
    found
        .static_pad_templates()
        .iter()
        .filter(|template| template.direction() == gst::PadDirection::Sink)
        .any(|template| template.caps().can_intersect(&probe))
}

/// Whether `factory` may be used under `policy`. Shared with the HLS path so
/// both pipelines honour the setting the same way.
pub fn allowed(factory: &str, policy: EncodingPolicy) -> bool {
    let encoder_ok = match policy.encoder {
        EncoderPolicy::Auto => true,
        EncoderPolicy::Hardware => is_hardware(factory),
        EncoderPolicy::Software => !is_hardware(factory),
        EncoderPolicy::VaApi => api_of(factory) == Some(Api::Va),
        EncoderPolicy::Nvenc => api_of(factory) == Some(Api::Nvenc),
        EncoderPolicy::V4l2 => api_of(factory) == Some(Api::V4l2),
    };
    encoder_ok
        && policy
            .format
            .required()
            .is_none_or(|format| accepts_format(factory, format))
}

/// The launch fragment configuring `factory` for low-latency CBR at
/// `bitrate_bps`, producing an element named `venc` (so keyframe forcing can
/// find it). `fps` sizes the keyframe interval. Hardware params are kept
/// minimal - just bitrate and CBR - to maximise the chance they parse across
/// driver/plugin versions; the parse-check drops any that don't.
fn launch_for(factory: &str, bitrate_bps: u32, fps: u32) -> String {
    // svtav1/av1/VA/NVENC want kbit/s
    let kbps = bitrate_bps.checked_div(1000).unwrap_or(1).max(1);
    let key_int = fps.saturating_mul(2).max(1);
    match factory {
        // vp8enc and vp9enc share the VPX base and its properties (bit/s).
        "vp8enc" | "vp9enc" => format!(
            "{factory} name=venc deadline=1 cpu-used=8 end-usage=cbr \
             target-bitrate={bitrate_bps} keyframe-max-dist={key_int} lag-in-frames=0 \
             error-resilient=default threads=4"
        ),
        "svtav1enc" => {
            format!(
                "svtav1enc name=venc preset=12 target-bitrate={kbps} intra-period-length={key_int}"
            )
        }
        "av1enc" => format!(
            "av1enc name=venc usage-profile=realtime end-usage=cbr \
             target-bitrate={kbps} cpu-used=9 lag-in-frames=0 keyframe-max-dist={key_int} \
             threads=4"
        ),
        "x264enc" => format!(
            "x264enc name=venc tune=zerolatency speed-preset=veryfast bitrate={kbps} \
             key-int-max={key_int} bframes=0"
        ),
        // VA-API (GStreamer 'va' plugin): bitrate in kbit/s, CBR rate control.
        f if f.starts_with("va") => {
            format!("{factory} name=venc bitrate={kbps} rate-control=cbr")
        }
        // NVENC (GStreamer 'nvcodec' plugin).
        f if f.starts_with("nv") => {
            format!("{factory} name=venc bitrate={kbps} rc-mode=cbr")
        }
        // V4L2 stateful encoders take no bitrate property: everything goes
        // through V4L2 controls, in bit/s. The structure name is arbitrary, and
        // controls a device does not implement are ignored rather than fatal.
        f if f.starts_with("v4l2") => format!(
            "{factory} name=venc extra-controls=\"controls,video_bitrate={bitrate_bps},video_gop_size={key_int}\""
        ),
        other => format!("{other} name=venc"),
    }
}

/// A parse-only check that `fragment` names a real element with valid
/// properties/enum values, without disturbing the real pipeline.
fn fragment_parses(fragment: &str) -> bool {
    gst::parse::launch(fragment).is_ok()
}

/// Whether the element in `fragment` can also *open* its device, not merely be
/// created. A hardware encoder is registered from what the plugin believed about
/// the driver when the registry was built, which is regularly a lie: a discrete
/// GPU asleep under runtime power management, a `GStreamer` registry cached from
/// before the driver was installed, a VA-API driver advertising a profile it
/// cannot open, a V4L2 node another process holds. `READY` is where
/// `GstVideoEncoder` opens the device, so it is the cheapest question that gets
/// a truthful answer, and it is asked before the encoder is ever put in a
/// pipeline - a mirroring session has no way back to software once it starts.
fn element_opens(fragment: &str) -> bool {
    let Ok(element) = gst::parse::launch(fragment) else {
        return false;
    };
    let opened = element.set_state(gst::State::Ready).is_ok();
    // Back to NULL either way, so the probe never holds the device.
    let _ = element.set_state(gst::State::Null);
    opened
}

/// Whether `factory`'s `fragment` is usable here. Hardware candidates have to
/// open their device; software ones only have to parse, since they cannot fail
/// that way and the probe is not free.
pub fn fragment_usable(factory: &str, fragment: &str) -> bool {
    if is_hardware(factory) {
        element_opens(fragment)
    } else {
        fragment_parses(fragment)
    }
}

/// The encoder fragment for `codec` and whether it is hardware, or `None` when
/// no encoder for it is installed **and** permitted by `policy`. Returns the
/// first candidate that actually parses.
pub fn video_encoder(
    codec: VideoCodec,
    bitrate_bps: u32,
    fps: u32,
    policy: EncodingPolicy,
) -> Option<(String, bool)> {
    factories(codec)
        .iter()
        .filter(|&&factory| allowed(factory, policy))
        .find_map(|&factory| {
            let fragment = launch_for(factory, bitrate_bps, fps);
            fragment_usable(factory, &fragment).then(|| (fragment, is_hardware(factory)))
        })
}

/// Whether any VA-API or NVENC encoder element is installed. A registry lookup
/// rather than the parse-check `video_encoder` uses: instantiating a VA element
/// initialises the libva driver, which costs hundreds of milliseconds, and this
/// only answers a hint in preferences.
fn hardware_encoder_available() -> bool {
    EFFICIENCY_ORDER
        .into_iter()
        .flat_map(|codec| factories(codec).iter().copied())
        .any(|factory| is_hardware(factory) && gst::ElementFactory::find(factory).is_some())
}

/// Whether the `GStreamer` va plugin is loaded at all, regardless of which
/// elements it managed to register. It registers one element per capability the
/// libva driver reports, so "plugin loaded but no encoder" means the driver is
/// missing or cannot encode, which needs different advice than a missing plugin.
fn va_plugin_registered() -> bool {
    gst::Registry::get().find_plugin("va").is_some()
}

/// What a graphics card means for hardware encoding. Only these three groups
/// lead anywhere different: silicon reached through VA-API, silicon reached
/// through NVENC, and silicon we cannot send the user shopping for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gpu {
    VaApi,
    Nvenc,
    Neither,
}

/// Classifies a DRM driver name into the encoder API it leads to.
///
/// `Neither` is the honest answer for a lot of real hardware, and it produces no
/// advice at all: a system on chip (`v3d`/`vc4` on a Raspberry Pi,
/// `panfrost`/`panthor` on
/// Mali, `msm`, `rockchip`, `sun4i`, `mediatek`, `meson`) encodes through V4L2,
/// which this daemon does not drive yet; a VM (`virtio_gpu`, `vmwgfx`, `qxl`,
/// `vkms`, `simpledrm`) has no encoder to reach; a server display chip (`ast`,
/// `mgag200`) never had one; and Apple Silicon under `asahi` has no open encoder
/// driver. Telling any of those users to install a VA-API driver would send them
/// after something that does not exist for their machine.
fn gpu_class(driver: &str) -> Gpu {
    match driver {
        // Intel: i915 through Alder Lake and friends, xe from Lunar Lake on.
        // AMD: amdgpu for GCN and later, radeon for the older cards - whose
        // encode support is thin, but the packages to install are the same.
        "i915" | "xe" | "amdgpu" | "radeon" => Gpu::VaApi,
        // The proprietary driver and its open kernel module both expose NVENC.
        // Nouveau does not expose it at all, so the fix there is the same swap.
        "nvidia" | "nvidia-drm" | "nouveau" => Gpu::Nvenc,
        _ => Gpu::Neither,
    }
}

/// The DRM driver behind each render node, e.g. `["nvidia", "i915"]` on a hybrid
/// laptop. Read from sysfs rather than probed, so it costs nothing and answers
/// the same with a discrete GPU powered down.
fn render_node_drivers() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .filter_map(|entry| node_driver(&entry.path()))
        .collect()
}

/// The driver for one `/sys/class/drm/renderD*`: the `DRIVER=` line of its
/// device uevent, or the name its `driver` symlink points at when the uevent
/// carries no such line.
fn node_driver(node: &std::path::Path) -> Option<String> {
    let uevent = std::fs::read_to_string(node.join("device/uevent")).unwrap_or_default();
    let named = uevent
        .lines()
        .find_map(|line| line.strip_prefix("DRIVER="))
        .map(str::trim)
        .filter(|name| !name.is_empty());
    if let Some(name) = named {
        return Some(name.to_owned());
    }
    std::fs::read_link(node.join("device/driver"))
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
}

/// The `ID` and `ID_LIKE` values from an os-release file, in that order.
fn os_release_ids(text: &str) -> Vec<&str> {
    let value = |key: &str| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| name.trim() == key)
            .map_or("", |(_, value)| value.trim().trim_matches('"'))
    };
    ["ID", "ID_LIKE"]
        .into_iter()
        .flat_map(|key| value(key).split_whitespace())
        .collect()
}

/// The package carrying the `GStreamer` `va` plugin (`vah264enc` and friends) on
/// the distributions we know, matched on `ID` then `ID_LIKE`. Empty for the
/// rest: the extension then phrases the hint without naming a package.
fn va_package_for(ids: &[&str]) -> &'static str {
    ids.iter()
        .find_map(|id| match *id {
            "arch" => Some("gst-plugin-va"),
            "ubuntu" | "debian" => Some("gstreamer1.0-plugins-bad"),
            "fedora" | "rhel" | "centos" => Some("gstreamer1-plugins-bad-free"),
            "opensuse" | "suse" => Some("gstreamer-plugins-bad"),
            _ => None,
        })
        .unwrap_or_default()
}

/// The package to install for hardware encoding on this host, or empty when the
/// distribution is unknown.
fn va_plugin_package() -> &'static str {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    va_package_for(&os_release_ids(&text))
}

/// Why hardware encoding is unavailable, as a token the extension turns into a
/// translated sentence: `plugin` for VA-API silicon with no va plugin, `driver`
/// for a va plugin that registered no encoder, `nvidia` for a machine whose only
/// hardware path is NVENC. Empty when hardware encoding already works, and empty
/// for hardware no install would help - saying nothing beats sending someone
/// after a package that does not exist for their GPU.
///
/// VA-API wins on a hybrid machine: its packages are the ones a user can
/// realistically install, and the integrated GPU is the one always present.
fn hardware_gap(hardware: bool, gpus: &[Gpu], plugin: bool) -> &'static str {
    if hardware {
        return "";
    }
    if gpus.contains(&Gpu::VaApi) {
        return if plugin { "driver" } else { "plugin" };
    }
    if gpus.contains(&Gpu::Nvenc) {
        return "nvidia";
    }
    ""
}

/// Why hardware encoding is unavailable on this host, and the package that would
/// fix it when we know its name.
pub fn hardware_encoding_gap() -> (&'static str, &'static str) {
    let gpus: Vec<Gpu> = render_node_drivers()
        .iter()
        .map(|driver| gpu_class(driver))
        .collect();
    let gap = hardware_gap(hardware_encoder_available(), &gpus, va_plugin_registered());
    // Only the missing-plugin advice names a package, so only it reads os-release.
    let package = if gap == "plugin" {
        va_plugin_package()
    } else {
        ""
    };
    (gap, package)
}

/// Why no encoder was usable, phrased for the user - this reaches them verbatim
/// through `user_message()`, so it names the setting to change rather than the
/// `GStreamer` element that was missing.
pub fn policy_failure_message(policy: EncodingPolicy) -> String {
    match (policy.encoder, policy.format) {
        (EncoderPolicy::Auto, FormatPolicy::Auto) => {
            "No video encoder is installed. Install the GStreamer encoder plugins for your system."
                .to_owned()
        }
        (EncoderPolicy::Hardware, _) => {
            "No hardware video encoder can be used for this device. Set the video encoder \
             preference back to automatic."
                .to_owned()
        }
        (EncoderPolicy::Software, _) => {
            "No software video encoder can be used for this device. Set the video encoder \
             preference back to automatic."
                .to_owned()
        }
        // Named per API, because "hardware" is not what the user chose here.
        (EncoderPolicy::VaApi, _) => {
            "No VA-API encoder can be used for this device. Set the video encoder preference \
             back to automatic."
                .to_owned()
        }
        (EncoderPolicy::Nvenc, _) => {
            "No NVENC encoder can be used for this device. Set the video encoder preference \
             back to automatic."
                .to_owned()
        }
        (EncoderPolicy::V4l2, _) => {
            "No V4L2 encoder can be used for this device. Set the video encoder preference \
             back to automatic."
                .to_owned()
        }
        (EncoderPolicy::Auto, _) => {
            "No video encoder accepts the selected pixel format. Set the pixel format \
             preference back to automatic."
                .to_owned()
        }
    }
}

/// The codecs we can encode on this host, **hardware-encodable ones first**,
/// then by efficiency. Used to build the OFFER - we advertise only codecs we
/// can produce, in the order we prefer to use them.
pub fn available_video_codecs(policy: EncodingPolicy) -> Vec<VideoCodec> {
    let mut avail: Vec<(VideoCodec, bool)> = EFFICIENCY_ORDER
        .into_iter()
        .filter_map(|codec| video_encoder(codec, 4_000_000, 30, policy).map(|(_, hw)| (codec, hw)))
        .collect();
    avail.sort_by_key(|&(codec, hw)| (!hw, efficiency_rank(codec)));
    avail.into_iter().map(|(codec, _)| codec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names_match_the_cast_offer_strings() {
        assert_eq!(VideoCodec::Vp8.codec_name(), "vp8");
        assert_eq!(VideoCodec::Vp9.codec_name(), "vp9");
        assert_eq!(VideoCodec::Av1.codec_name(), "av1");
        assert_eq!(VideoCodec::H264.codec_name(), "h264");
    }

    #[test]
    fn vpx_bitrate_is_bits_per_second() {
        let f = launch_for("vp9enc", 4_000_000, 30);
        assert!(f.starts_with("vp9enc name=venc"));
        assert!(f.contains("end-usage=cbr"));
        assert!(f.contains("target-bitrate=4000000"));
    }

    /// A receiver that lost the first key frame shows black until the next.
    #[test]
    fn every_fragment_keys_about_every_two_seconds() {
        for codec in EFFICIENCY_ORDER {
            for factory in factories(codec) {
                let f = launch_for(factory, 2_000_000, 40);
                if let Some(rest) = f.split("keyframe-max-dist=").nth(1) {
                    assert_eq!(rest.split_whitespace().next(), Some("80"), "{factory}");
                }
            }
        }
        assert!(launch_for("x264enc", 2_000_000, 40).contains("key-int-max=80"));
        assert!(launch_for("svtav1enc", 2_000_000, 40).contains("intra-period-length=80"));
    }

    #[test]
    fn av1_bitrate_is_kilobits_per_second() {
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        assert!(launch_for("av1enc", 4_000_000, 30).contains("target-bitrate=4000"));
        assert!(launch_for("svtav1enc", 4_000_000, 30).contains("intra-period-length=60"));
    }

    #[test]
    fn hardware_encoders_are_detected_and_kilobit_rated() {
        assert!(is_hardware("vah264enc"));
        assert!(is_hardware("nvav1enc"));
        assert!(!is_hardware("x264enc"));
        assert!(!is_hardware("svtav1enc"));
        assert!(!is_hardware("vp9enc"));
        // 4 Mbit/s -> 4000 kbit/s for VA/NVENC.
        assert!(launch_for("vah264enc", 4_000_000, 30).contains("bitrate=4000"));
        assert!(launch_for("nvh264enc", 4_000_000, 30).contains("rc-mode=cbr"));
    }

    /// The format lookup has to agree with what the pipeline can actually link:
    /// VA-API H.264 takes only NV12, the VPX/AV1 encoders only I420. Each
    /// assertion is skipped when the plugin is not installed, so this passes on
    /// a CI box without gst-plugin-va.
    #[test]
    fn accepted_formats_match_the_encoders() {
        gst::init().unwrap();
        let installed = |factory| gst::ElementFactory::find(factory).is_some();

        if installed("vah264enc") {
            assert!(accepts_format("vah264enc", "NV12"));
            assert!(!accepts_format("vah264enc", "I420"));
        }
        if installed("vp8enc") {
            assert!(accepts_format("vp8enc", "I420"));
            assert!(!accepts_format("vp8enc", "NV12"));
        }
        if installed("x264enc") {
            assert!(accepts_format("x264enc", "I420"));
            assert!(accepts_format("x264enc", "NV12"));
        }
        assert!(!accepts_format("no-such-encoder", "I420"));
    }

    #[test]
    fn policy_filters_the_candidate_list() {
        gst::init().unwrap();
        let auto = EncodingPolicy::default();
        let software = EncodingPolicy {
            encoder: EncoderPolicy::Software,
            ..auto
        };
        let hardware = EncodingPolicy {
            encoder: EncoderPolicy::Hardware,
            ..auto
        };

        assert!(allowed("x264enc", auto));
        assert!(allowed("x264enc", software));
        assert!(!allowed("x264enc", hardware));
        assert!(allowed("vah264enc", hardware));
        assert!(!allowed("vah264enc", software));

        // A forced format rules out encoders that cannot take it, which is what
        // keeps a forced value from producing a pipeline that will not link.
        if gst::ElementFactory::find("vah264enc").is_some() {
            let i420 = EncodingPolicy {
                format: FormatPolicy::I420,
                ..auto
            };
            assert!(!allowed("vah264enc", i420));
            assert!(allowed("x264enc", i420));
        }
    }

    /// Whichever VA-API VP9 element a driver offers - full power or low power -
    /// the hardware policy has to find it, or a VP9 cast silently runs in
    /// software on hardware that can encode it. Skipped where neither exists.
    #[test]
    fn a_va_api_vp9_encoder_is_picked_when_one_is_installed() {
        gst::init().unwrap();
        let hardware = EncodingPolicy {
            encoder: EncoderPolicy::Hardware,
            format: FormatPolicy::Auto,
        };
        let installed = ["vavp9enc", "vavp9lpenc"]
            .into_iter()
            .any(|f| gst::ElementFactory::find(f).is_some());
        let picked = video_encoder(VideoCodec::Vp9, 4_000_000, 30, hardware);
        assert_eq!(installed, picked.is_some());
        if let Some((fragment, is_hw)) = picked {
            assert!(is_hw);
            assert!(fragment.starts_with("vavp9"), "{fragment}");
        }
    }

    /// A machine with two hardware paths needs each one pinnable, and the V4L2
    /// elements have to survive the same filters as VA-API and NVENC.
    #[test]
    fn each_hardware_api_can_be_pinned() {
        let pin = |encoder| EncodingPolicy {
            encoder,
            format: FormatPolicy::Auto,
        };
        let va = pin(EncoderPolicy::VaApi);
        let nvenc = pin(EncoderPolicy::Nvenc);
        let v4l2 = pin(EncoderPolicy::V4l2);

        assert!(allowed("vah264enc", va));
        assert!(!allowed("nvh264enc", va));
        assert!(!allowed("v4l2h264enc", va));
        assert!(allowed("nvh264enc", nvenc));
        assert!(!allowed("vah264lpenc", nvenc));
        assert!(allowed("v4l2h264enc", v4l2));
        assert!(!allowed("x264enc", v4l2));
        // Every API choice is a hardware choice, so software never qualifies.
        for policy in [va, nvenc, v4l2] {
            assert!(!allowed("x264enc", policy));
            assert!(!allowed("vp9enc", policy));
        }
        // "v4l2" must not be read as the "va" or "nv" prefix.
        assert_eq!(api_of("v4l2vp8enc"), Some(Api::V4l2));
        assert_eq!(api_of("vavp9lpenc"), Some(Api::Va));
        assert_eq!(api_of("nvav1enc"), Some(Api::Nvenc));
        assert_eq!(api_of("vp8enc"), None);
    }

    /// V4L2 encoders take no bitrate property, so the controls carry it - in
    /// bit/s, unlike the kbit/s properties everywhere else.
    #[test]
    fn v4l2_settings_travel_in_extra_controls() {
        let f = launch_for("v4l2h264enc", 4_000_000, 30);
        assert!(f.starts_with("v4l2h264enc name=venc"));
        assert!(f.contains("video_bitrate=4000000"));
        assert!(f.contains("video_gop_size=60"));
        assert!(!f.contains("bitrate=4000 "));
    }

    /// A registered element that cannot open its device must not be picked: the
    /// mirroring path has no way back to software once a session starts.
    #[test]
    fn an_encoder_that_cannot_open_its_device_is_not_usable() {
        gst::init().unwrap();
        assert!(!fragment_usable("vah264enc", "no-such-encoder name=venc"));
        assert!(!fragment_usable("x264enc", "no-such-encoder name=venc"));
        // Software still only needs to parse, so this holds with no GPU at all.
        assert_eq!(
            fragment_usable("x264enc", &launch_for("x264enc", 4_000_000, 30)),
            gst::ElementFactory::find("x264enc").is_some()
        );
        // Where a VA-API encoder is installed, opening it is what proves it.
        if gst::ElementFactory::find("vah264enc").is_some() {
            assert!(element_opens(&launch_for("vah264enc", 4_000_000, 30)));
        }
    }

    #[test]
    fn unknown_option_values_fall_back_to_auto() {
        assert_eq!(EncoderPolicy::parse("software"), EncoderPolicy::Software);
        assert_eq!(EncoderPolicy::parse("nonsense"), EncoderPolicy::Auto);
        assert_eq!(EncoderPolicy::parse(""), EncoderPolicy::Auto);
        assert_eq!(FormatPolicy::parse("nv12"), FormatPolicy::Nv12);
        assert_eq!(FormatPolicy::parse("yuv"), FormatPolicy::Auto);
        assert_eq!(FormatPolicy::Auto.caps_format(), "{NV12,I420}");
        assert_eq!(FormatPolicy::I420.caps_format(), "I420");
    }

    #[test]
    fn os_release_ids_take_id_before_id_like() {
        let text =
            "NAME=\"openSUSE Tumbleweed\"\nID=\"opensuse-tumbleweed\"\nID_LIKE=\"suse opensuse\"\n";
        assert_eq!(
            os_release_ids(text),
            ["opensuse-tumbleweed", "suse", "opensuse"]
        );
        assert!(os_release_ids("").is_empty());
    }

    /// Every DRM driver a user might boot this on, grouped by the encoder API it
    /// actually leads to - the table decides whose advice is right, so a wrong
    /// entry sends real people after the wrong package.
    #[test]
    fn drm_drivers_map_to_the_encoder_api_they_offer() {
        for driver in ["i915", "xe", "amdgpu", "radeon"] {
            assert_eq!(gpu_class(driver), Gpu::VaApi, "{driver}");
        }
        for driver in ["nvidia", "nvidia-drm", "nouveau"] {
            assert_eq!(gpu_class(driver), Gpu::Nvenc, "{driver}");
        }
        // SoCs encode through V4L2, VMs and server display chips not at all.
        for driver in [
            "v3d",
            "vc4",
            "panfrost",
            "panthor",
            "msm",
            "rockchip",
            "sun4i",
            "mediatek",
            "meson",
            "virtio_gpu",
            "vmwgfx",
            "qxl",
            "vkms",
            "vgem",
            "simpledrm",
            "ast",
            "mgag200",
            "asahi",
            "",
        ] {
            assert_eq!(gpu_class(driver), Gpu::Neither, "{driver}");
        }
    }

    /// What preferences ends up saying, for each shape of machine this runs on.
    #[test]
    fn the_hardware_gap_names_the_missing_piece() {
        let intel = [Gpu::VaApi];
        let nvidia_only = [Gpu::Nvenc];
        // A hybrid laptop: Intel plus NVIDIA, in either sysfs order.
        let hybrid = [Gpu::Nvenc, Gpu::VaApi];
        let pi = [Gpu::Neither];

        // Working hardware encoding says nothing, whatever the machine is.
        for gpus in [&intel[..], &nvidia_only[..], &hybrid[..], &pi[..], &[]] {
            assert_eq!(hardware_gap(true, gpus, true), "");
            assert_eq!(hardware_gap(true, gpus, false), "");
        }

        assert_eq!(hardware_gap(false, &intel, false), "plugin");
        assert_eq!(hardware_gap(false, &intel, true), "driver");
        // VA-API is the reachable path on a hybrid, so it wins over NVENC.
        assert_eq!(hardware_gap(false, &hybrid, false), "plugin");
        assert_eq!(hardware_gap(false, &hybrid, true), "driver");
        // Only NVIDIA silicon: a VA-API package would not help.
        assert_eq!(hardware_gap(false, &nvidia_only, false), "nvidia");
        assert_eq!(hardware_gap(false, &nvidia_only, true), "nvidia");
        // A Raspberry Pi, a VM, a headless box: nothing to suggest.
        assert_eq!(hardware_gap(false, &pi, true), "");
        assert_eq!(hardware_gap(false, &[], true), "");
    }

    /// Whatever this machine has, the sysfs walk has to classify it without
    /// panicking and agree with what the gap logic is told.
    #[test]
    fn this_machine_classifies_its_own_render_nodes() {
        for driver in render_node_drivers() {
            assert!(!driver.is_empty());
            let _ = gpu_class(&driver);
        }
    }

    #[test]
    fn va_package_follows_the_distribution() {
        assert_eq!(va_package_for(&["arch"]), "gst-plugin-va");
        assert_eq!(
            va_package_for(&["ubuntu", "debian"]),
            "gstreamer1.0-plugins-bad"
        );
        // Derivatives name themselves first and their base in ID_LIKE.
        assert_eq!(
            va_package_for(&["nobara", "fedora"]),
            "gstreamer1-plugins-bad-free"
        );
        assert_eq!(
            va_package_for(&["opensuse-leap", "suse"]),
            "gstreamer-plugins-bad"
        );
        assert_eq!(va_package_for(&["nixos"]), "");
        assert_eq!(va_package_for(&[]), "");
    }

    /// VA-API splits several codecs into a full-power and a low-power element,
    /// and on Intel the low-power one is often the only one that exists.
    #[test]
    fn every_va_encoder_is_listed_with_its_low_power_variant() {
        for codec in EFFICIENCY_ORDER {
            let list = factories(codec);
            for factory in list
                .iter()
                .filter(|f| f.starts_with("va") && !f.ends_with("lpenc"))
            {
                let lp = format!("{}lpenc", factory.trim_end_matches("enc"));
                assert!(list.contains(&lp.as_str()), "{codec:?} is missing {lp}");
            }
        }
    }

    #[test]
    fn every_fragment_names_the_encoder_venc() {
        for codec in EFFICIENCY_ORDER {
            for factory in factories(codec) {
                assert!(launch_for(factory, 2_000_000, 24).contains("name=venc"));
            }
        }
    }
}
