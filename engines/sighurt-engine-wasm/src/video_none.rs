//! The video decoder of `video` where this build has no FFmpeg: Windows, where FFmpeg would come
//! from vcpkg and is not set up (docs/rewrite/PORTING.md). Every open fails with [`NO_VIDEO`],
//! so `api_video_load` and `api_video_load_url` return their failure code and the console says
//! why; nothing is ever decoded.
//!
//! Owns: the refusal. Must not own: what may be opened, the playback clock or drawing (`video`'s).

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;

use crate::capabilities::DecodedImage;

/// Why no video opens here.
pub const NO_VIDEO: &str = "this build of sig decodes no video on Windows (no FFmpeg)";

/// A video stream: never made here.
pub struct VideoPlayer {
    pub duration_ms: u64,
    never: Infallible,
}

impl VideoPlayer {
    /// Fails: [`NO_VIDEO`].
    pub fn open_path(_path: &Path) -> Result<Self, String> {
        Err(NO_VIDEO.into())
    }

    /// Fails: [`NO_VIDEO`].
    pub(super) fn open_url(_url: &str) -> Result<Self, String> {
        Err(NO_VIDEO.into())
    }

    /// Unreachable: there is no player.
    pub fn decode_frame_at(&mut self, _target_ms: u64) -> Result<Arc<DecodedImage>, String> {
        match self.never {}
    }
}
