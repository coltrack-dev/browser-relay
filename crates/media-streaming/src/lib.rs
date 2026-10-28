//! Phase 2 video transport primitives. Audio and input control are intentionally
//! out of scope. The default build contains signaling/state logic; the
//! `gstreamer` feature enables the real Xvfb -> H.264 -> webrtcbin pipeline.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSessionState {
    Idle,
    Connecting,
    Streaming,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalMessage {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_m_line_index: u32,
    },
    Ready,
    State {
        state: MediaSessionState,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MediaError {
    #[error("one viewer is already connected")]
    ViewerAlreadyConnected,
    #[error("no viewer is connected")]
    NoViewer,
    #[error("media backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub display: String,
}
impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 2500,
            display: std::env::var("DISPLAY").unwrap_or_else(|_| ":99".into()),
        }
    }
}
impl VideoConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            width: env_u32("BROWSER_RELAY_VIDEO_WIDTH", defaults.width),
            height: env_u32("BROWSER_RELAY_VIDEO_HEIGHT", defaults.height),
            fps: env_u32("BROWSER_RELAY_VIDEO_FPS", defaults.fps),
            bitrate_kbps: env_u32("BROWSER_RELAY_VIDEO_BITRATE_KBPS", defaults.bitrate_kbps),
            display: std::env::var("DISPLAY").unwrap_or(defaults.display),
        }
    }
}
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone)]
pub struct MediaSession {
    state: Arc<Mutex<MediaSessionState>>,
    viewer_connected: Arc<Mutex<bool>>,
}
impl Default for MediaSession {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(MediaSessionState::Idle)),
            viewer_connected: Arc::new(Mutex::new(false)),
        }
    }
}
impl MediaSession {
    pub async fn state(&self) -> MediaSessionState {
        *self.state.lock().await
    }
    pub async fn connect_viewer(&self) -> Result<(), MediaError> {
        let mut viewer = self.viewer_connected.lock().await;
        if *viewer {
            return Err(MediaError::ViewerAlreadyConnected);
        }
        *viewer = true;
        *self.state.lock().await = MediaSessionState::Connecting;
        Ok(())
    }
    pub async fn mark_streaming(&self) {
        *self.state.lock().await = MediaSessionState::Streaming;
    }
    pub async fn fail(&self) {
        *self.state.lock().await = MediaSessionState::Failed;
    }
    pub async fn disconnect_viewer(&self) -> Result<(), MediaError> {
        let mut viewer = self.viewer_connected.lock().await;
        if !*viewer {
            return Err(MediaError::NoViewer);
        }
        *viewer = false;
        *self.state.lock().await = MediaSessionState::Idle;
        Ok(())
    }
}

#[cfg(feature = "gstreamer")]
mod gst_backend {
    use super::{MediaError, SignalMessage, VideoConfig};
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_sdp as gst_sdp;
    use gstreamer_webrtc as gst_webrtc;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    #[derive(Clone, Default)]
    struct MediaCounters {
        h264_buffers: Arc<AtomicU64>,
        rtp_buffers: Arc<AtomicU64>,
    }

    pub struct GstreamerVideoSession {
        pipeline: gst::Pipeline,
        webrtc: gst::Element,
        payloader: gst::Element,
        counters: MediaCounters,
    }

    struct NegotiationGuard {
        pipeline: gst::Pipeline,
        webrtc: gst::Element,
        sink_pad: Option<gst::Pad>,
    }

    impl NegotiationGuard {
        fn new(pipeline: &gst::Pipeline, webrtc: &gst::Element, sink_pad: gst::Pad) -> Self {
            Self {
                pipeline: pipeline.clone(),
                webrtc: webrtc.clone(),
                sink_pad: Some(sink_pad),
            }
        }

        fn commit(&mut self) {
            self.sink_pad = None;
        }
    }

    impl Drop for NegotiationGuard {
        fn drop(&mut self) {
            if let Some(sink_pad) = self.sink_pad.take() {
                self.webrtc.release_request_pad(&sink_pad);
                let _ = self.pipeline.set_state(gst::State::Null);
            }
        }
    }

    impl GstreamerVideoSession {
        pub fn start(config: &VideoConfig) -> Result<Self, MediaError> {
            gst::init().map_err(|error| {
                eprintln!("GStreamer initialization failed: {error}");
                MediaError::Backend(error.to_string())
            })?;
            eprintln!(
                "Starting video pipeline: display={} size={}x{} fps={} bitrate_kbps={}",
                config.display, config.width, config.height, config.fps, config.bitrate_kbps
            );
            let (pipeline, webrtc, payloader, counters) = build_video_pipeline(config)?;
            Ok(Self {
                pipeline,
                webrtc,
                payloader,
                counters,
            })
        }

        pub fn stop(&self) -> Result<(), MediaError> {
            drain_pipeline_errors(&self.pipeline);
            log_media_counters("stopping", &self.counters);
            self.pipeline
                .set_state(gst::State::Null)
                .map(|_| ())
                .map_err(|error| MediaError::Backend(format!("pipeline stop: {error:?}")))
        }
        pub fn set_remote_offer(&self, sdp_text: &str) -> Result<SignalMessage, MediaError> {
            let sdp = gst_sdp::SDPMessage::parse_buffer(sdp_text.as_bytes())
                .map_err(|error| MediaError::Backend(format!("invalid offer SDP: {error:?}")))?;
            let (mline_index, payload_type, codec_caps) = h264_offer_caps(&sdp)?;
            eprintln!("WebRTC negotiation: selected video H264 payload type {payload_type}");
            self.payloader.set_property("pt", payload_type);

            let sink_pad = self
                .webrtc
                .request_pad_simple(&format!("sink_{mline_index}"))
                .ok_or_else(|| MediaError::Backend("webrtc sink request pad failed".into()))?;
            let transceiver = sink_pad
                .property::<Option<gst_webrtc::WebRTCRTPTransceiver>>("transceiver")
                .ok_or_else(|| {
                    MediaError::Backend("RTP sink pad has no associated transceiver".into())
                })?;
            if transceiver.mlineindex() != mline_index {
                let _ = self.webrtc.release_request_pad(&sink_pad);
                return Err(MediaError::Backend(
                    "RTP sink pad was allocated for a different media line".into(),
                ));
            }
            if transceiver.find_property("codec-preferences").is_none() {
                let _ = self.webrtc.release_request_pad(&sink_pad);
                return Err(MediaError::Backend(
                    "webrtc transceiver does not support codec preferences".into(),
                ));
            }
            transceiver.set_direction(gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
            transceiver.set_property("codec-preferences", &codec_caps);
            if transceiver.direction() != gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly {
                let _ = self.webrtc.release_request_pad(&sink_pad);
                return Err(MediaError::Backend(
                    "webrtc video transceiver is not sendonly".into(),
                ));
            }
            let src_pad = self
                .payloader
                .static_pad("src")
                .ok_or_else(|| MediaError::Backend("rtph264pay src pad is missing".into()))?;
            let src_caps = src_pad.query_caps(None);
            let sink_caps = sink_pad.query_caps(None);
            let intersection = src_caps.intersect(&sink_caps);
            eprintln!(
                "RTP caps before link: src={} sink={} intersection={}",
                src_caps, sink_caps, intersection
            );
            if let Some(current_caps) = src_pad.current_caps() {
                eprintln!("RTP src current caps: {current_caps}");
            }
            if let Some(current_caps) = sink_pad.current_caps() {
                eprintln!("WebRTC sink current caps: {current_caps}");
            }
            eprintln!(
                "RTP pad templates: src={} sink={}",
                src_pad.pad_template_caps(),
                sink_pad.pad_template_caps()
            );
            if intersection.is_empty() {
                let _ = self.webrtc.release_request_pad(&sink_pad);
                return Err(MediaError::Backend(
                    "rtph264pay and webrtc sink caps have no intersection".into(),
                ));
            }
            src_pad.link(&sink_pad).map_err(|error| {
                MediaError::Backend(format!("rtph264pay to webrtc link failed: {error:?}"))
            })?;
            if transceiver.sender().is_none() {
                let _ = self.webrtc.release_request_pad(&sink_pad);
                return Err(MediaError::Backend(
                    "sendonly transceiver has no RTP sender".into(),
                ));
            }
            let mut negotiation_guard =
                NegotiationGuard::new(&self.pipeline, &self.webrtc, sink_pad.clone());
            ensure_pipeline_playing(&self.pipeline)?;

            let offer =
                gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp);
            eprintln!(
                "WebRTC negotiation: RTP pad linked to sendonly transceiver; setting remote description"
            );
            let set_promise = gst::Promise::new();
            self.webrtc
                .emit_by_name::<()>("set-remote-description", &[&offer, &set_promise]);
            let _ = wait_promise_reply("set-remote-description", &set_promise)
                .map_err(MediaError::Backend)?;
            eprintln!("WebRTC negotiation: remote description accepted");
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let promise = gst::Promise::with_change_func(move |reply| {
                let result = promise_reply_to_structure("create-answer", reply)
                    .and_then(|reply| {
                        reply.ok_or_else(|| "create-answer reply has no GstStructure".into())
                    })
                    .and_then(|reply| {
                        reply
                            .get::<gst_webrtc::WebRTCSessionDescription>("answer")
                            .map_err(|error| {
                                format!("create-answer reply has no answer field: {error}")
                            })
                    });
                let _ = sender.send(result);
            });
            eprintln!("WebRTC negotiation: creating answer");
            self.webrtc
                .emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
            let answer = receiver
                .recv()
                .map_err(|_| MediaError::Backend("answer promise closed".into()))?
                .map_err(MediaError::Backend)?;
            eprintln!("WebRTC negotiation: answer structure contains a session description");
            let answer_sdp = answer
                .sdp()
                .as_text()
                .map_err(|error| MediaError::Backend(format!("answer SDP: {error:?}")))?
                .to_string();
            let answer_message = gst_sdp::SDPMessage::parse_buffer(answer_sdp.as_bytes())
                .map_err(|error| MediaError::Backend(format!("generated answer SDP: {error:?}")))?;
            validate_h264_answer(&answer_message, payload_type)?;
            let local_promise = gst::Promise::new();
            eprintln!("WebRTC negotiation: setting local description");
            self.webrtc
                .emit_by_name::<()>("set-local-description", &[&answer, &local_promise]);
            let _ = wait_promise_reply("set-local-description", &local_promise)
                .map_err(MediaError::Backend)?;
            eprintln!("WebRTC negotiation: local description accepted");
            drain_pipeline_errors(&self.pipeline);
            log_media_counters("after SDP negotiation", &self.counters);
            negotiation_guard.commit();
            Ok(SignalMessage::Answer { sdp: answer_sdp })
        }
        pub fn add_ice_candidate(&self, candidate: &SignalMessage) -> Result<(), MediaError> {
            if let SignalMessage::IceCandidate {
                candidate,
                sdp_m_line_index,
            } = candidate
            {
                self.webrtc
                    .emit_by_name::<()>("add-ice-candidate", &[sdp_m_line_index, candidate]);
            }
            Ok(())
        }
    }

    fn promise_reply_to_structure(
        stage: &str,
        result: Result<Option<&gst::StructureRef>, gst::PromiseError>,
    ) -> Result<Option<gst::Structure>, String> {
        let reply = match result {
            Ok(reply) => reply,
            Err(gst::PromiseError::Interrupted) => {
                return Err(format!("{stage} promise interrupted"));
            }
            Err(gst::PromiseError::Expired) => {
                return Err(format!("{stage} promise expired"));
            }
            Err(gst::PromiseError::Other(result)) => {
                return Err(format!("{stage} promise failed with {result:?}"));
            }
        };
        let Some(reply) = reply else {
            return Ok(None);
        };
        if let Ok(error) = reply.get::<&gst::glib::Error>("error") {
            eprintln!(
                "WebRTC negotiation: {stage} GError: message={} domain={:?} details={error:?}",
                error.message(),
                error.domain()
            );
            return Err(format!(
                "{stage} GError: message={} domain={:?} details={error:?}",
                error.message(),
                error.domain()
            ));
        }
        if reply.value("error").is_ok() {
            return Err(format!("{stage} reply error field is not a glib::Error"));
        }
        Ok(Some(reply.to_owned()))
    }

    fn wait_promise_reply(
        stage: &str,
        promise: &gst::Promise,
    ) -> Result<Option<gst::Structure>, String> {
        let result = match promise.wait() {
            gst::PromiseResult::Replied => Ok(promise.get_reply()),
            gst::PromiseResult::Interrupted => Err(gst::PromiseError::Interrupted),
            gst::PromiseResult::Expired => Err(gst::PromiseError::Expired),
            result => Err(gst::PromiseError::Other(result)),
        };
        promise_reply_to_structure(stage, result)
    }

    fn ensure_pipeline_playing(pipeline: &gst::Pipeline) -> Result<(), MediaError> {
        let (_, current_state, pending_state) = pipeline.state(Some(gst::ClockTime::ZERO));
        if current_state == gst::State::Playing && pending_state == gst::State::VoidPending {
            eprintln!("GStreamer pipeline is already Playing; reusing active state");
            return Ok(());
        }
        eprintln!(
            "GStreamer pipeline negotiation transition: current={current_state:?} pending={pending_state:?} -> Playing"
        );
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| MediaError::Backend(format!("pipeline state(Playing): {error:?}")))?;
        let (state_result, current_state, pending_state) =
            pipeline.state(Some(gst::ClockTime::from_seconds(5)));
        eprintln!(
            "GStreamer pipeline negotiation state: result={state_result:?} current={current_state:?} pending={pending_state:?}"
        );
        state_result.map_err(|error| MediaError::Backend(format!("pipeline state: {error:?}")))?;
        drain_pipeline_errors(pipeline);
        if current_state != gst::State::Playing {
            return Err(MediaError::Backend(format!(
                "pipeline did not reach Playing: current={current_state:?} pending={pending_state:?}"
            )));
        }
        Ok(())
    }

    fn build_video_pipeline(
        config: &VideoConfig,
    ) -> Result<(gst::Pipeline, gst::Element, gst::Element, MediaCounters), MediaError> {
        let source = gst::ElementFactory::make("ximagesrc")
            .name("ximagesrc")
            .property_from_str("display-name", &config.display)
            .property("use-damage", false)
            .build()
            .map_err(|error| MediaError::Backend(format!("ximagesrc: {error}")))?;
        let raw_caps = gst::Caps::builder("video/x-raw")
            .field("framerate", gst::Fraction::new(config.fps as i32, 1))
            .field("width", config.width as i32)
            .field("height", config.height as i32)
            .build();
        let raw_filter = gst::ElementFactory::make("capsfilter")
            .name("raw-caps")
            .property("caps", raw_caps)
            .build()
            .map_err(|error| MediaError::Backend(format!("raw caps: {error}")))?;
        let convert = gst::ElementFactory::make("videoconvert")
            .name("videoconvert")
            .build()
            .map_err(|error| MediaError::Backend(format!("videoconvert: {error}")))?;
        let encoder = gst::ElementFactory::make("x264enc")
            .name("x264enc")
            .property_from_str("tune", "zerolatency")
            .property_from_str("speed-preset", "ultrafast")
            .property("bitrate", config.bitrate_kbps)
            .property("key-int-max", config.fps)
            .build()
            .map_err(|error| MediaError::Backend(format!("x264enc: {error}")))?;
        let h264_caps = gst::Caps::builder("video/x-h264")
            .field("profile", "constrained-baseline")
            .field("stream-format", "avc")
            .field("alignment", "au")
            .build();
        let h264_filter = gst::ElementFactory::make("capsfilter")
            .name("h264-caps")
            .property("caps", h264_caps)
            .build()
            .map_err(|error| MediaError::Backend(format!("H264 caps: {error}")))?;
        let parser = gst::ElementFactory::make("h264parse")
            .name("h264parse")
            .build()
            .map_err(|error| MediaError::Backend(format!("h264parse: {error}")))?;
        let payloader = gst::ElementFactory::make("rtph264pay")
            .name("payloader")
            .property("config-interval", 1i32)
            .build()
            .map_err(|error| MediaError::Backend(format!("rtph264pay: {error}")))?;
        let webrtc = gst::ElementFactory::make("webrtcbin")
            .name("webrtc")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .map_err(|error| MediaError::Backend(format!("webrtcbin: {error}")))?;

        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([
                &source,
                &raw_filter,
                &convert,
                &encoder,
                &h264_filter,
                &parser,
                &payloader,
                &webrtc,
            ])
            .map_err(|error| MediaError::Backend(format!("add video elements: {error}")))?;
        gst::Element::link_many([
            &source,
            &raw_filter,
            &convert,
            &encoder,
            &h264_filter,
            &parser,
            &payloader,
        ])
        .map_err(|error| MediaError::Backend(format!("link video elements: {error}")))?;
        let counters = MediaCounters::default();
        let h264_counter = counters.h264_buffers.clone();
        pipeline
            .by_name("h264parse")
            .and_then(|parser| parser.static_pad("src"))
            .and_then(|pad| {
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    h264_counter.fetch_add(1, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                })
            })
            .ok_or_else(|| {
                MediaError::Backend("h264parse src probe could not be installed".into())
            })?;
        let rtp_counter = counters.rtp_buffers.clone();
        payloader
            .static_pad("src")
            .and_then(|pad| {
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    rtp_counter.fetch_add(1, Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                })
            })
            .ok_or_else(|| {
                MediaError::Backend("rtph264pay src probe could not be installed".into())
            })?;
        Ok((pipeline, webrtc, payloader, counters))
    }

    fn log_media_counters(stage: &str, counters: &MediaCounters) {
        eprintln!(
            "GStreamer media diagnostics ({stage}): h264_buffers={} rtp_buffers={}",
            counters.h264_buffers.load(Ordering::Relaxed),
            counters.rtp_buffers.load(Ordering::Relaxed)
        );
    }

    fn drain_pipeline_errors(pipeline: &gst::Pipeline) {
        let Some(bus) = pipeline.bus() else { return };
        while let Some(message) = bus.timed_pop(gst::ClockTime::ZERO) {
            if let gst::MessageView::Error(error) = message.view() {
                let source = message
                    .src()
                    .map(|object| object.name().to_string())
                    .unwrap_or_else(|| "unknown".into());
                let gst_error = error.error();
                eprintln!(
                    "GStreamer bus error: element={} message={} domain={:?} debug={:?}",
                    source,
                    gst_error.message(),
                    gst_error.domain(),
                    error.debug()
                );
                log_pipeline_caps(pipeline);
            }
        }
    }

    fn log_pipeline_caps(pipeline: &gst::Pipeline) {
        for (element_name, pad_name) in [
            ("x264enc", "src"),
            ("h264parse", "sink"),
            ("h264parse", "src"),
            ("payloader", "src"),
        ] {
            let Some(pad) = pipeline
                .by_name(element_name)
                .and_then(|element| element.static_pad(pad_name))
            else {
                continue;
            };
            eprintln!(
                "GStreamer caps diagnostics: {}.{} current={:?} query={}",
                element_name,
                pad_name,
                pad.current_caps(),
                pad.query_caps(None)
            );
        }
        let (_, current, pending) = pipeline.state(Some(gst::ClockTime::ZERO));
        eprintln!("GStreamer pipeline diagnostics: current={current:?} pending={pending:?}");
    }

    fn h264_offer_caps(sdp: &gst_sdp::SDPMessage) -> Result<(u32, u32, gst::Caps), MediaError> {
        for (mline_index, media) in sdp.medias().enumerate() {
            if media.media() != Some("video")
                || !media
                    .attributes()
                    .any(|attribute| attribute.key() == "recvonly")
            {
                continue;
            }
            let mid = media.attribute_val("mid").ok_or_else(|| {
                MediaError::Backend("video offer media section has no MID".into())
            })?;
            let bundle = sdp
                .attribute_val("group")
                .ok_or_else(|| MediaError::Backend("video offer has no BUNDLE group".into()))?;
            if !bundle
                .split_whitespace()
                .skip(1)
                .any(|bundle_mid| bundle_mid == mid)
            {
                return Err(MediaError::Backend(
                    "video offer MID is not present in BUNDLE group".into(),
                ));
            }
            for format in media.formats() {
                let Ok(payload_type) = format.parse::<u32>() else {
                    continue;
                };
                let Some(rtpmap) = media.attributes().find_map(|attribute| {
                    let value = attribute.value()?;
                    (attribute.key() == "rtpmap" && value.starts_with(&format!("{format} ")))
                        .then_some(value)
                }) else {
                    continue;
                };
                let Some(codec) = rtpmap.strip_prefix(&format!("{format} ")) else {
                    continue;
                };
                if !codec.to_ascii_lowercase().starts_with("h264/90000") {
                    continue;
                }
                let Some(fmtp) = media.attributes().find_map(|attribute| {
                    let value = attribute.value()?;
                    (attribute.key() == "fmtp" && value.starts_with(&format!("{format} ")))
                        .then_some(value)
                }) else {
                    continue;
                };
                let fmtp_parameters = fmtp.split_once(' ').map(|(_, value)| value).unwrap_or("");
                let packetization_mode_one = fmtp_parameters
                    .split(';')
                    .any(|parameter| parameter.trim() == "packetization-mode=1");
                let baseline_profile = fmtp_parameters.split(';').any(|parameter| {
                    parameter
                        .trim()
                        .strip_prefix("profile-level-id=")
                        .is_some_and(|profile| profile.to_ascii_lowercase().starts_with("42"))
                });
                if !packetization_mode_one || !baseline_profile {
                    continue;
                }
                let profile_level_id = fmtp_parameters.split(';').find_map(|parameter| {
                    parameter
                        .trim()
                        .strip_prefix("profile-level-id=")
                        .map(str::to_owned)
                });
                let mut caps = gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("encoding-name", "H264")
                    .field("clock-rate", 90000i32)
                    .field("payload", payload_type as i32)
                    .field("packetization-mode", "1");
                if let Some(profile_level_id) = profile_level_id {
                    caps = caps.field("profile-level-id", profile_level_id);
                }
                return Ok((mline_index as u32, payload_type, caps.build()));
            }
        }
        Err(MediaError::Backend(
            "offer has no recvonly H264/90000 video codec".into(),
        ))
    }

    fn validate_h264_answer(
        sdp: &gst_sdp::SDPMessage,
        payload_type: u32,
    ) -> Result<(), MediaError> {
        let valid = sdp.medias().any(|media| {
            media.media() == Some("video")
                && media
                    .attributes()
                    .any(|attribute| attribute.key() == "sendonly")
                && media
                    .formats()
                    .any(|format| format == payload_type.to_string())
        });
        if valid {
            Ok(())
        } else {
            Err(MediaError::Backend(
                "generated answer does not contain a sendonly H264 video m-line".into(),
            ))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const OFFER: &str = "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 111 127\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=rtpmap:127 H264/90000\r\n\
a=fmtp:127 packetization-mode=1;profile-level-id=42e01f;level-asymmetry-allowed=1\r\n";

        #[test]
        fn selects_recvonly_baseline_h264_and_offer_payload_type() {
            gst::init().unwrap();
            let sdp = gst_sdp::SDPMessage::parse_buffer(OFFER.as_bytes()).unwrap();
            let (mline_index, payload_type, caps) = h264_offer_caps(&sdp).unwrap();
            assert_eq!(mline_index, 0);
            assert_eq!(payload_type, 127);
            let caps_text = caps.to_string();
            assert!(caps_text.starts_with("application/x-rtp"));
            assert!(caps_text.contains("encoding-name=(string)H264"));
            assert!(caps_text.contains("payload=(int)127"));
            assert!(!caps_text.contains("application/x-unknown"));
        }

        #[test]
        fn rejects_offer_without_recvonly_baseline_h264() {
            gst::init().unwrap();
            let offer = OFFER
                .replace("a=recvonly", "a=sendonly")
                .replace("profile-level-id=42e01f", "profile-level-id=640c1f");
            let sdp = gst_sdp::SDPMessage::parse_buffer(offer.as_bytes()).unwrap();
            assert!(h264_offer_caps(&sdp).is_err());
        }

        #[test]
        fn accepts_only_sendonly_answer_with_selected_payload_type() {
            gst::init().unwrap();
            let answer = OFFER
                .replace("a=recvonly", "a=sendonly")
                .replace("127 H264", "127 H264");
            let sdp = gst_sdp::SDPMessage::parse_buffer(answer.as_bytes()).unwrap();
            assert!(validate_h264_answer(&sdp, 127).is_ok());
            assert!(validate_h264_answer(&sdp, 96).is_err());
        }

        #[test]
        fn gstreamer_builds_and_links_video_pipeline_when_plugins_are_available() {
            gst::init().unwrap();
            let required = [
                "ximagesrc",
                "videoconvert",
                "x264enc",
                "h264parse",
                "rtph264pay",
                "webrtcbin",
            ];
            if required
                .iter()
                .any(|factory| gst::ElementFactory::find(factory).is_none())
            {
                return;
            }
            let (pipeline, webrtc, payloader, _) = build_video_pipeline(&VideoConfig::default())
                .expect("GStreamer video pipeline must be created");
            assert!(pipeline.by_name("ximagesrc").is_some());
            assert!(pipeline.by_name("videoconvert").is_some());
            assert!(pipeline.by_name("x264enc").is_some());
            assert!(pipeline.by_name("h264parse").is_some());
            assert!(pipeline.by_name("payloader").is_some());
            assert!(pipeline.by_name("webrtc").is_some());
            assert_eq!(webrtc.name(), "webrtc");
            assert_eq!(payloader.name(), "payloader");
            assert!(
                payloader
                    .static_pad("sink")
                    .is_some_and(|pad| pad.is_linked())
            );
            let sink_pad = webrtc
                .request_pad_simple("sink_0")
                .expect("webrtc must provide one video sink pad");
            let transceiver = sink_pad
                .property::<Option<gst_webrtc::WebRTCRTPTransceiver>>("transceiver")
                .expect("sink pad must expose its transceiver")
                .expect("sink pad must have a transceiver");
            assert_eq!(transceiver.mlineindex(), 0);
            transceiver.set_direction(gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
            assert_eq!(
                transceiver.direction(),
                gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly
            );
            let codec_caps = gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("encoding-name", "H264")
                .field("clock-rate", 90000i32)
                .field("payload", 127i32)
                .field("packetization-mode", "1")
                .field("profile-level-id", "42e01f")
                .build();
            transceiver.set_property("codec-preferences", &codec_caps);
            payloader.set_property("pt", 127u32);
            let src_pad = payloader.static_pad("src").unwrap();
            assert!(
                !src_pad
                    .query_caps(None)
                    .intersect(&sink_pad.query_caps(None))
                    .is_empty()
            );
            src_pad
                .link(&sink_pad)
                .expect("RTP to WebRTC link must succeed");
            assert!(src_pad.is_linked());
            webrtc.release_request_pad(&sink_pad);
        }

        #[test]
        fn pipeline_reaches_playing_before_sdp_negotiation_when_runtime_is_available() {
            gst::init().unwrap();
            let required = [
                "ximagesrc",
                "videoconvert",
                "x264enc",
                "h264parse",
                "rtph264pay",
                "webrtcbin",
            ];
            if std::env::var_os("DISPLAY").is_none()
                || required
                    .iter()
                    .any(|factory| gst::ElementFactory::find(factory).is_none())
            {
                return;
            }
            let (pipeline, _, _, _) = build_video_pipeline(&VideoConfig::default())
                .expect("GStreamer video pipeline must be created");
            ensure_pipeline_playing(&pipeline).expect("pipeline must reach Playing");
            let (_, current_state, _) = pipeline.state(Some(gst::ClockTime::ZERO));
            assert_eq!(current_state, gst::State::Playing);
            pipeline.set_state(gst::State::Null).unwrap();
        }

        #[test]
        fn promise_reply_parser_preserves_success_and_error_states() {
            gst::init().unwrap();
            let success = gst::Structure::builder("reply").build();
            let parsed = promise_reply_to_structure("create-answer", Ok(Some(&success)))
                .expect("successful reply must be preserved")
                .expect("successful reply must contain a structure");
            assert_eq!(parsed.name(), "reply");

            let error = gst::glib::Error::new(gst::CoreError::Failed, "negotiation failed");
            let error_reply = gst::Structure::builder("reply")
                .field("error", &error)
                .build();
            let error_message = promise_reply_to_structure("create-answer", Ok(Some(&error_reply)))
                .expect_err("reply error must be returned");
            assert!(error_message.contains("message=negotiation failed"));
            assert!(error_message.contains("domain="));
            assert!(
                promise_reply_to_structure("create-answer", Err(gst::PromiseError::Interrupted))
                    .expect_err("interrupted promise must be returned")
                    .contains("interrupted")
            );
            assert!(
                promise_reply_to_structure("create-answer", Err(gst::PromiseError::Expired))
                    .expect_err("expired promise must be returned")
                    .contains("expired")
            );
        }
    }
    impl Drop for GstreamerVideoSession {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }
}
#[cfg(feature = "gstreamer")]
pub use gst_backend::GstreamerVideoSession;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn allows_only_one_viewer_and_recovers_after_disconnect() {
        let session = MediaSession::default();
        session.connect_viewer().await.unwrap();
        assert_eq!(
            session.connect_viewer().await,
            Err(MediaError::ViewerAlreadyConnected)
        );
        session.mark_streaming().await;
        assert_eq!(session.state().await, MediaSessionState::Streaming);
        session.disconnect_viewer().await.unwrap();
        assert_eq!(session.state().await, MediaSessionState::Idle);
    }
    #[test]
    fn signal_messages_are_json_compatible() {
        let message = SignalMessage::IceCandidate {
            candidate: "candidate:1".into(),
            sdp_m_line_index: 0,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            json,
            r#"{"type":"ice_candidate","candidate":"candidate:1","sdp_m_line_index":0}"#
        );
    }

    #[test]
    fn x264_profile_is_configured_by_downstream_caps() {
        let source = include_str!("lib.rs");
        let invalid_option = [
            ".property_from_str(\"",
            "option-string",
            "\", \"profile=constrained-baseline\")",
        ]
        .concat();
        assert!(!source.contains(&invalid_option));
        assert!(source.contains("field(\"profile\", \"constrained-baseline\")"));
        assert!(source.contains("field(\"stream-format\", \"avc\")"));
        assert!(source.contains("field(\"alignment\", \"au\")"));
    }
}
