//! Internal stdio helper entrypoint.

use std::io;

use crate::error::OniError;
use crate::protocol::{current_implementation, CapabilitySet, Hello, Limits, Message, PeerRole};
use crate::transport::stdio::Connection;

pub fn serve() -> Result<(), OniError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut connection = Connection::new(
        stdin.lock(),
        stdout.lock(),
        Limits::default().max_frame_bytes() as usize,
    );

    let Some(frame) = connection.receive_frame()? else {
        return Ok(());
    };
    let Message::Hello(remote_hello) = Message::decode(&frame)?;

    let local_hello = Hello::for_current(
        PeerRole::Helper,
        current_implementation(),
        CapabilitySet::default(),
        Limits::default(),
    );

    Hello::negotiate(&local_hello, &remote_hello)?;
    connection.send_frame(&Message::Hello(local_hello).encode()?)?;

    Ok(())
}
