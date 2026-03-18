use std::future::Future;
use std::pin::Pin;

use crate::error::TransportError;
use crate::transport::Transport;

#[derive(Debug, Clone, Copy)]
pub struct StdioTransport {
    max_frame_size: usize,
}

impl StdioTransport {
    pub const DEFAULT_MAX_FRAME_SIZE: usize = 1024 * 1024;

    pub const fn new(max_frame_size: usize) -> Self {
        Self { max_frame_size }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_FRAME_SIZE)
    }
}

impl Transport for StdioTransport {
    fn name(&self) -> &'static str {
        "stdio"
    }

    fn supports_remote(&self) -> bool {
        true
    }

    fn codec_name(&self) -> &'static str {
        "length-delimited-binary"
    }

    fn max_frame_size(&self) -> usize {
        self.max_frame_size
    }

    fn handshake(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async {
            Err(TransportError::NotImplemented {
            feature: "stdio handshake",
            })
        })
    }
}
