use netty::{
    transport::{
        AsyncTransport, ChannelClientTransport, ChannelServerTransport, ClientTransport,
        ServerTransport, TcpClientTransport, TcpServerTransport,
    },
    *,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(5);

#[allow(unused)]
fn channel_transport(
) -> (AsyncTransport<ChannelServerTransport>, AsyncTransport<ChannelClientTransport>) {
    let (server, client) = ChannelServerTransport::new_server_client_pair();
    (server, client)
}

#[allow(unused)]
fn tcp_transport() -> (AsyncTransport<TcpServerTransport>, AsyncTransport<TcpClientTransport>) {
    let (server_addr, server) = TcpServerTransport::bind("127.0.0.1:0");
    let client = AsyncTransport::new(|runtime| async {
        let server_addr = server_addr.get().await.unwrap();
        TcpClientTransport::connect(server_addr).start(runtime).await
    });
    (server, client)
}

#[allow(unused)]
#[cfg(feature = "webtransport")]
fn webtransport_transport() -> (
    AsyncTransport<netty::transport::WebTransportServerTransport>,
    AsyncTransport<netty::transport::WebTransportClientTransport>,
) {
    let (server_addr, server) = netty::transport::WebTransportServerTransport::bind(
        "127.0.0.1:0".parse().unwrap(),
        netty::transport::WebTransportServerTransportCertificate::SelfSigned,
    );
    let client = AsyncTransport::new(|runtime| async {
        let (server_addr, hashes) = server_addr.get().await.unwrap();
        netty::transport::WebTransportClientTransport::connect(
            format!("https://{server_addr}"),
            netty::transport::WebTransportClientTransportCertificateValidation::CertificateHashes(
                hashes,
            ),
        )
        .start(runtime)
        .await
    });
    (server, client)
}

macro_rules! simple_test {
    ($name:ident, $transport:expr) => {
        #[test]
        fn $name() {
            let runtime: Arc<dyn Runtime> = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .unwrap(),
            );
            run_simple($transport, runtime);
        }
    };
}

simple_test!(channel, channel_transport());
simple_test!(tcp, tcp_transport());
#[cfg(feature = "webtransport")]
simple_test!(webtransport, webtransport_transport());

fn run_simple(
    (server_transport, client_transport): (
        AsyncTransport<impl ServerTransport + Send + Sync + 'static>,
        AsyncTransport<impl ClientTransport + Send + Sync + 'static>,
    ),
    runtime: Arc<dyn Runtime>,
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

    // Start server
    server.start(server_transport, Arc::clone(&runtime));
    'server_up: loop {
        server.wait_timeout(TIMEOUT);
        while let Some(event) = server.process_events().next() {
            match event {
                ServerEvent::TransportIsUp(_) => (),
                ServerEvent::ServerIsUp => break 'server_up,
                _ => panic!("unexpected event: {:?}", event),
            }
        }
    }
    assert!(server.process_events().next().is_none());

    // Connect client
    client.connect(client_transport, runtime);
    client.wait_timeout(TIMEOUT);
    assert!(matches!(client.process_events().next(), Some(ClientEvent::Connected)));
    assert!(client.process_events().next().is_none());

    // Accept connection
    server.wait_timeout(TIMEOUT);
    let server_event = server.process_events().next();
    let client_handle = match server_event {
        Some(ServerEvent::IncomingConnection(incoming)) => server.accept(incoming).unwrap(),
        event => panic!("unexpected event: {:?}", event),
    };
    assert!(server.process_events().next().is_none());

    // Send messages
    server.send_to(ServerToClient { data: "Hello Client".to_string() }, client_handle);
    client.send(ClientToServer { data: "Hello Server".to_string() });

    // Receive messages
    server.wait_timeout(TIMEOUT);
    let (msg, msg_handle) = server.recv::<ClientToServer>().next().unwrap();
    assert!(msg.data == "Hello Server");
    assert!(msg_handle == client_handle);

    client.wait_timeout(TIMEOUT);
    let msg = client.recv::<ServerToClient>().next().unwrap();
    assert!(msg.data == "Hello Client");

    // Disconnect
    client.disconnect();

    server.wait_timeout(TIMEOUT);
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
