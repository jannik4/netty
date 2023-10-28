use netty::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[test]
fn simple() {
    let mut server = {
        let mut channels = Channels::new();
        channels
            .add_send::<ServerToClient>()
            .add_recv::<ClientToServer>();
        Server::new(Arc::new(channels))
    };

    let mut client = {
        let mut channels = Channels::new();
        channels
            .add_send::<ClientToServer>()
            .add_recv::<ServerToClient>();
        Client::new(Arc::new(channels))
    };

    let (server_transport, client_transport) =
        transport::ChannelServerTransport::new_server_client_pair();

    server.start(server_transport);
    sleep(10);
    client.connect(client_transport);
    sleep(10);

    let client_handle = match server.process_events().next() {
        Some(ServerEvent::Connected(handle)) => handle,
        event => panic!("unexpected event: {:?}", event),
    };
    assert!(server.process_events().next().is_none());

    assert!(matches!(
        client.process_events().next(),
        Some(ClientEvent::Connected)
    ));
    assert!(client.process_events().next().is_none());

    server.send_to(
        ServerToClient {
            data: "Hello Client".to_string(),
        },
        client_handle,
    );
    client.send(ClientToServer {
        data: "Hello Server".to_string(),
    });
    sleep(10);

    let (msg, msg_handle) = server.recv::<ClientToServer>().next().unwrap();
    assert!(msg.data == "Hello Server");
    assert!(msg_handle == client_handle);

    let msg = client.recv::<ServerToClient>().next().unwrap();
    assert!(msg.data == "Hello Client");

    client.disconnect();
    sleep(10);

    assert!(
        matches!(server.process_events().next(), Some(ServerEvent::Disconnected(handle)) if handle == client_handle)
    );
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerToClient {
    pub data: String,
}

impl NetworkMessage for ServerToClient {
    const CHANNEL_ID: ChannelId = ChannelId::new(0);
    const CHANNEL_CONFIG: ChannelConfig = ChannelConfig {
        reliable: true,
        ordered: true,
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientToServer {
    pub data: String,
}

impl NetworkMessage for ClientToServer {
    const CHANNEL_ID: ChannelId = ChannelId::new(0);
    const CHANNEL_CONFIG: ChannelConfig = ChannelConfig {
        reliable: true,
        ordered: true,
    };
}

fn sleep(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}
