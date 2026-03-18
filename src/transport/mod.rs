pub mod stdio;

use std::future::Future;
use std::pin::Pin;

use crate::error::TransportError;

pub trait Transport: Send + Sync {
    fn name(&self) -> &'static str;
    fn supports_remote(&self) -> bool;
    fn codec_name(&self) -> &'static str;
    fn max_frame_size(&self) -> usize;
    fn handshake(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;
}
