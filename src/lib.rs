use anyhow::bail;
use quinn::RecvStream;
use tokio::io::AsyncReadExt;
use tracing::instrument;

pub const MESSAGE_LENGTH_LIMIT: u64 = 128000000; // Bytes

pub mod server {
    use std::{
        collections::HashMap,
        net::{Ipv6Addr, SocketAddr, SocketAddrV6},
        path::PathBuf,
        sync::Arc,
        time::Duration,
    };

    use anyhow::bail;
    use dashmap::DashMap;
    use quinn::{
        rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer},
        Endpoint, RecvStream, SendStream, ServerConfig,
    };
    use tokio::{
        io::AsyncReadExt,
        select,
        sync::broadcast::{Receiver, Sender},
    };
    use tokio_util::sync::CancellationToken;
    use tracing::{event, span, Level};

    use crate::{read_message_length, Message, MessageType};

    /// Creates a custom ```(ServerConfig, CertificateDer<'static>)``` instance. The Certificate is insecure.
    pub fn configure_server() -> anyhow::Result<(ServerConfig, CertificateDer<'static>)> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = CertificateDer::from(cert.cert);
        let priv_key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

        let mut server_config =
            ServerConfig::with_single_cert(vec![cert_der.clone()], priv_key.into()).unwrap();
        let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();

        transport_config.max_concurrent_uni_streams(0_u8.into());
        transport_config.max_idle_timeout(Some(Duration::from_secs(2 * 60 * 60).try_into()?));

        Ok((server_config, cert_der))
    }

    pub fn start_server(
        port: u16,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ServerInstance> {
        #[cfg(not(debug_assertions))]
        let addr = Ipv6Addr::UNSPECIFIED;

        #[cfg(debug_assertions)]
        let addr = Ipv6Addr::LOCALHOST;

        let (server_config, _server_cert) = configure_server().unwrap();

        let endpoint = Endpoint::server(
            server_config,
            std::net::SocketAddr::V6(SocketAddrV6::new(addr, port, 0, 0)),
        )?;

        let shared_files: Arc<DashMap<String, PathBuf>> = Arc::new(DashMap::new());

        spawn_service_workers(shared_files.clone(), Arc::new(endpoint), cancellation_token);

        let server_instace = ServerInstance {
            client_list: shared_files,
        };

        Ok(server_instace)
    }

    pub fn spawn_service_workers(
        shared_files: Arc<DashMap<String, PathBuf>>,
        endpoint: Arc<Endpoint>,
        cancellation_token: CancellationToken,
    ) {
        let client_conn_span = span!(Level::WARN, "client_connector");
        let server_sender_span = span!(Level::WARN, "server_sender");
        let client_listn_span = span!(Level::WARN, "client_listener");

        let cancellation_token_clone_3 = cancellation_token.clone();

        // Spawn incoming connecting client listener
        tokio::spawn(async move {
            let _span = client_conn_span.enter();
            let endpoint = endpoint.clone();
            loop {
                select! {
                    Some(incoming_conn) = endpoint.accept() => {
                        let remote_addr = incoming_conn.remote_address();
                        event!(Level::INFO, "Incoming connection from: {}", remote_addr);
                        if let Ok(connecting) = incoming_conn.accept() {
                            event!(Level::INFO, "Accepted connection from: {}", remote_addr);
                            if let Ok(connection) = connecting.await {
                                let (send_stream, mut recv_stream) = connection.accept_bi().await.unwrap();

                                event!(
                                    Level::INFO,
                                    "Accepted bidirectional stream from: {}",
                                    remote_addr
                                );

                                let (send, mut recv) = tokio::sync::mpsc::channel::<Message>(1000);

                                // Enter client span
                                let _span = client_listn_span.enter();

                                let cancellation_token_clone_1 = cancellation_token.clone();
                                let cancellation_token_clone_2 = cancellation_token.clone();

                                // Spawn client listener
                                tokio::spawn(async move {
                                    loop {
                                        select! {
                                            _ = cancellation_token_clone_1.cancelled() => break,
                                            Ok(message_length) = read_message_length(&mut recv_stream) => {
                                                let mut buf = vec![0; message_length as usize];

                                                if let Err(err) = recv_stream.read_exact(&mut buf).await {
                                                    event!(Level::ERROR, "Error occured while reading from client ({remote_addr}) stream: {err}");
                                                }

                                                match serde_json::from_slice::<Message>(&buf) {
                                                    Ok(message) => {
                                                        send.send(message).await.unwrap();
                                                    },
                                                    Err(err) => {
                                                        event!(Level::ERROR, "Received malformed input from client ({remote_addr}): {err}");
                                                    },
                                                }
                                            }
                                        }
                                    }
                                });

                                let _spawn = server_sender_span.enter();
                                // Spawn client sender
                                tokio::spawn(async move {
                                    loop {
                                        select! {
                                            _ = cancellation_token_clone_2.cancelled() => break,

                                            Some(received_message) = recv.recv() => {

                                            }
                                        }
                                    }
                                });
                            } else {
                                event!(Level::WARN, "All references to the `Connection` have been dropped on the client side.");
                            }
                        } else {
                            event!(
                                Level::WARN,
                                "An error occured while trying to accept the connection."
                            );
                        }
                    }

                    _ = cancellation_token_clone_3.cancelled() => break,
                }
            }
        });
        tokio::spawn(async move {});
    }

    /// This struct contains a HashMap of hashes and the paired ```PathBuf```s
    pub struct FileList(HashMap<String, PathBuf>);

    pub struct RemoteClient {
        recv: tokio::sync::mpsc::Receiver<Message>,
        send: tokio::sync::mpsc::Sender<Message>,
    }

    pub struct ServerInstance {
        pub client_list: Arc<DashMap<String, PathBuf>>,
    }
}

pub mod client {
    use std::sync::Arc;

    use quinn::{
        rustls::{
            self,
            pki_types::{CertificateDer, ServerName, UnixTime},
        },
        RecvStream, SendStream,
    };

    pub struct ConnectionInstance {
        recv: RecvStream,
        send: SendStream,
    }

    #[derive(serde::Deserialize, serde::Serialize, Clone)]
    pub enum FileTree {
        Folder((String, Vec<FileTree>)),
        File((String, String)),
        Empty,
    }

    /// Custom certificate, this doesnt verify anything I should implement a working one.
    #[derive(Debug)]
    struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

    impl SkipServerVerification {
        fn new() -> Arc<Self> {
            Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
        }
    }

    /// Trait implementation for the custom certificate struct
    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct Message(Option<MessageType>);

#[derive(serde::Deserialize, serde::Serialize)]
enum MessageType {
    FileTreeRequest(Vec<client::FileTree>),
    FileRequest(String),
}

#[instrument]
pub async fn read_message_length(recv: &mut RecvStream) -> anyhow::Result<u64> {
    let message_length = recv.read_u64().await?;

    if message_length > MESSAGE_LENGTH_LIMIT {
        bail!("Message length is too long, rejecting request.");
    } else {
        Ok(message_length)
    }
}
