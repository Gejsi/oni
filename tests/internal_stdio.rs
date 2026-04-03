use std::process::{Command, Stdio};

use oni::protocol::{
    current_implementation, Capability, CapabilitySet, Hello, Limits, Message, PeerRole,
    CURRENT_PROTOCOL_VERSION,
};
use oni::transport::stdio::Connection;

#[test]
fn internal_stdio_helper_completes_a_hello_handshake() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_oni"))
        .arg("internal")
        .arg("serve")
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut connection =
        Connection::new(stdout, stdin, Limits::default().max_frame_bytes() as usize);

    let coordinator_hello = Hello::for_current(
        PeerRole::Coordinator,
        current_implementation(),
        CapabilitySet::from([Capability::WholeFileTransfer]),
        Limits::default(),
    );

    connection
        .send_frame(&Message::Hello(coordinator_hello.clone()).encode().unwrap())
        .unwrap();

    let response = connection.receive_frame().unwrap().unwrap();
    let Message::Hello(helper_hello) = Message::decode(&response).unwrap();

    assert_eq!(helper_hello.role, PeerRole::Helper);
    assert_eq!(helper_hello.capabilities, CapabilitySet::default());

    let negotiated = Hello::negotiate(&coordinator_hello, &helper_hello).unwrap();
    assert_eq!(negotiated.version, CURRENT_PROTOCOL_VERSION);

    drop(connection);

    let status = child.wait().unwrap();
    assert!(status.success());
}
