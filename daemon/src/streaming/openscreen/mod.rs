//! Cast Streaming protocol code ported from Chromium's Open Screen Library
//! (openscreen) `cast/streaming`: OFFER/ANSWER messages, RTP packetization,
//! RTCP feedback, and AES-128-CTR frame encryption.
//!
//! These modules are derivative works of openscreen and are used under its
//! BSD-3-Clause licence, not the project's MIT licence. See the `NOTICE` file
//! in this directory for the required copyright notice and disclaimer.

pub(crate) mod crypto;
pub(crate) mod messages;
pub(crate) mod rtcp;
pub(crate) mod rtp;
pub(crate) mod sender;
