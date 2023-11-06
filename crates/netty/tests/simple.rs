use netty::{
    transport::{
        ChannelClientTransport, ChannelServerTransport, ClientTransport, ServerTransport,
        TcpClientTransport, TcpServerTransport,
    },
    *,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[allow(unused)]
fn channel_transport() -> (ChannelServerTransport, ChannelClientTransport, u64) {
    let (server, client) = ChannelServerTransport::new_server_client_pair();
    (server, client, 10)
}

#[allow(unused)]
fn tcp_transport() -> (TcpServerTransport, TcpClientTransport, u64) {
    let (server_addr, server) = TcpServerTransport::bind("127.0.0.1:0").unwrap();
    dbg!(server_addr);
    let client = TcpClientTransport::connect(server_addr).unwrap();
    (server, client, 100)
}

#[test]
fn simple() {
    run_simple(channel_transport());
    run_simple(tcp_transport());
}

fn run_simple(
    (server_transport, client_transport, ms): (
        impl ServerTransport + Send + Sync + 'static,
        impl ClientTransport + Send + Sync + 'static,
        u64,
    ),
) {
    let mut server = {
        let mut channels = Channels::new();
        channels.add_send::<ServerToClient>().add_recv::<ClientToServer>();
        Server::new(Arc::new(channels))
    };

    let mut client = {
        let mut channels = Channels::new();
        channels.add_send::<ClientToServer>().add_recv::<ServerToClient>();
        Client::new(Arc::new(channels))
    };

    server.start(server_transport);
    sleep(ms);
    client.connect(client_transport);
    sleep(ms);

    let server_event = server.process_events().next();
    let client_handle = match server_event {
        Some(ServerEvent::IncomingConnection(incoming)) => server.accept(incoming).unwrap(),
        event => panic!("unexpected event: {:?}", event),
    };
    assert!(server.process_events().next().is_none());

    assert!(matches!(client.process_events().next(), Some(ClientEvent::Connected)));
    assert!(client.process_events().next().is_none());

    server.send_to(ServerToClient { data: "Hello Client".to_string() }, client_handle);
    client.send(ClientToServer { data: "Hello Server".to_string() });
    sleep(ms);

    let (msg, msg_handle) = server.recv::<ClientToServer>().next().unwrap();
    assert!(msg.data == "Hello Server");
    assert!(msg_handle == client_handle);

    let msg = client.recv::<ServerToClient>().next().unwrap();
    assert!(msg.data == "Hello Client");

    client.disconnect();
    sleep(ms);

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
    const CHANNEL_CONFIG: ChannelConfig = ChannelConfig { reliable: true, ordered: true };
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientToServer {
    pub data: String,
}

impl NetworkMessage for ClientToServer {
    const CHANNEL_ID: ChannelId = ChannelId::new(0);
    const CHANNEL_CONFIG: ChannelConfig = ChannelConfig { reliable: true, ordered: true };
}

fn sleep(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}
