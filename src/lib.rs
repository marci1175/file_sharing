use std::{fs::Metadata, mem, path::PathBuf};

use anyhow::bail;
use chrono::{DateTime, Local};
use quinn::RecvStream;
use tokio::{fs::File, io::AsyncReadExt};

pub const MESSAGE_LENGTH_LIMIT: u64 = 128000000; // Bytes

pub mod server {
    use std::{
        collections::HashMap,
        io::{Read, Seek},
        net::{Ipv6Addr, SocketAddrV6},
        path::PathBuf,
        sync::Arc,
        time::Duration,
    };

    use quinn::{
        rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer},
        Endpoint, ServerConfig,
    };
    use tokio::{
        fs::File,
        io::{AsyncReadExt, AsyncSeekExt},
        select,
        sync::Mutex,
    };
    use tokio_util::sync::CancellationToken;
    use tracing::{event, span, Level};
    use uuid::Uuid;

    use crate::{
        client::FileTree, get_file_handle, read_message_length, FileReponseHeader, Message,
        Sendable, MESSAGE_LENGTH_LIMIT,
    };

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
        shared_files: HashMap<String, PathBuf>,
        file_tree: Vec<FileTree>,
    ) -> anyhow::Result<Ipv6Addr> {
        #[cfg(not(debug_assertions))]
        let addr = Ipv6Addr::UNSPECIFIED;

        #[cfg(debug_assertions)]
        let addr = Ipv6Addr::LOCALHOST;

        event!(
            Level::DEBUG,
            r#"Starting server on local address: "{addr}"..."#
        );

        let (server_config, _server_cert) = configure_server().unwrap();

        let endpoint = Endpoint::server(
            server_config,
            std::net::SocketAddr::V6(SocketAddrV6::new(addr, port, 0, 0)),
        )?;

        event!(Level::INFO, "Starting service workers...");

        spawn_service_workers(
            shared_files.clone(),
            Arc::new(endpoint),
            cancellation_token,
            file_tree,
        );

        event!(Level::INFO, "Server startup complete.");

        Ok(addr)
    }

    pub fn spawn_service_workers(
        shared_files: HashMap<String, PathBuf>,
        endpoint: Arc<Endpoint>,
        cancellation_token: CancellationToken,
        file_tree: Vec<FileTree>,
    ) {
        let cancellation_token_clone_3 = cancellation_token.clone();

        // Spawn incoming connecting client listener
        tokio::spawn(async move {
            let endpoint = endpoint.clone();
            loop {
                select! {
                    Some(incoming_conn) = endpoint.accept() => {
                        let remote_addr = incoming_conn.remote_address();
                        event!(Level::INFO, "Incoming connection from: {}", remote_addr);
                        if let Ok(connecting) = incoming_conn.accept() {
                            event!(Level::INFO, "Accepted connection from: {}", remote_addr);
                            if let Ok(connection) = connecting.await {
                                let (mut send_stream, mut recv_stream) = connection.accept_bi().await.unwrap();

                                let _ = recv_stream.read_exact(&mut [0; 1]).await;

                                event!(
                                    Level::INFO,
                                    "Accepted bidirectional stream from: {}",
                                    remote_addr
                                );

                                let (send, mut recv) = tokio::sync::mpsc::channel::<Message>(1000);

                                let cancellation_token_clone_1 = cancellation_token.clone();
                                let cancellation_token_clone_2 = cancellation_token.clone();

                                //Send file tree automaticly
                                if let Err(err) = send_stream.write_all(&Message(Some(crate::MessageType::FileTreeResponse(file_tree.clone()))).to_sendable()).await {
                                    event!(Level::ERROR, "Error occured while writing to client ({remote_addr}) stream: {err}");
                                }

                                // Spawn client listener
                                tokio::spawn(async move {
                                    loop {
                                        select! {
                                            _ = cancellation_token_clone_1.cancelled() => {
                                                event!(Level::INFO, "Client listener shut down: ({remote_addr})");

                                                break
                                            },

                                            Ok(message_length) = read_message_length(&mut recv_stream) => {
                                                let mut buf = vec![0; message_length as usize];

                                                if let Err(err) = recv_stream.read_exact(&mut buf).await {
                                                    event!(Level::ERROR, "Error occured while reading from client ({remote_addr}) stream: {err}");
                                                }

                                                match Message::try_from(buf.as_slice()) {
                                                    Ok(message) => {
                                                        match &message.0 {
                                                            Some(inner) => {
                                                                event!(Level::DEBUG, "Received message ({inner}) from client: {remote_addr}.",);
                                                            },
                                                            None => {
                                                                event!(Level::DEBUG, "Received message empty `None` from client: {remote_addr}.",);
                                                            },
                                                        }
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

                                // Enter span
                                let shared_files = shared_files.clone();
                                let file_tree = file_tree.clone();

                                // Spawn client sender
                                tokio::spawn(async move {
                                    let send_stream = Arc::new(Mutex::new(send_stream));
                                    loop {
                                        let send_stream = send_stream.clone();

                                        select! {
                                            _ = cancellation_token_clone_2.cancelled() => break,

                                            Some(received_message) = recv.recv() => {
                                                if let Message(Some(message)) = received_message.clone() {
                                                    match message {
                                                        crate::MessageType::FileTreeRequest => {
                                                            if let Err(err) = send_stream.lock().await.write_all(&Message(Some(crate::MessageType::FileTreeResponse(file_tree.clone()))).to_sendable()).await {
                                                                event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                            }
                                                        },
                                                        crate::MessageType::FileRequest(file_hash) => {
                                                            event!(Level::INFO, "Client has requested a file: {file_hash}");
                                                            if let Some(file_path) = shared_files.get(&file_hash) {
                                                                event!(Level::INFO, "Client has requested a shared file available at: {}", file_path.as_os_str().to_string_lossy());

                                                                match get_file_handle(file_path.to_path_buf()) {
                                                                    Ok((mut file_handle, file_metadata)) => {
                                                                        let file_length = file_metadata.len();

                                                                let packet_length = MESSAGE_LENGTH_LIMIT / 2;
                                                                let packet_count =  file_length / packet_length + 1;
                                                                let parent_header_id = Uuid::new_v4();

                                                                let file_header = FileReponseHeader {
                                                                    file_name: file_path.file_name().unwrap().to_string_lossy().to_string(),
                                                                    file_hash: file_hash.clone(),
                                                                    total_size: file_length,
                                                                    file_packets: vec![],
                                                                    file_packet_count: packet_count,
                                                                    packet_identificator: parent_header_id.to_string(),
                                                                };

                                                                if let Err(err) = send_stream.lock().await.write_all(&Message(Some(crate::MessageType::FileResponse(file_header.clone()))).to_sendable()).await {
                                                                    event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                                }

                                                                event!(Level::INFO, "Sending file header packet: {file_header:?}");
                                                                event!(Level::INFO, "Sending {packet_count} packets of bytes. With a total of {file_length} bytes.");

                                                                let file_hash_clone = file_hash.clone();

                                                                tokio::spawn(async move {
                                                                    for packet_number in 0..packet_count {
                                                                        let mut buf = if packet_number + 1 == packet_count || file_length < packet_length {
                                                                            let cursor_pos = file_handle.stream_position().unwrap();
                                                                            vec![0; (file_length - cursor_pos) as usize]
                                                                        }
                                                                        else {
                                                                            vec![0; packet_length as usize]
                                                                        };

                                                                        file_handle.read_exact(&mut buf).unwrap();

                                                                        if let Err(err) = send_stream.lock().await.write_all(&Message(Some(crate::MessageType::FilePacket(crate::FilePacket {
                                                                            file_hash: file_hash_clone.clone(),
                                                                            packet_id: packet_number as usize,
                                                                            parent_id: parent_header_id.to_string(),
                                                                            bytes_length: buf.len(),
                                                                            bytes:buf
                                                                        }))).to_sendable()).await {
                                                                            event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                                        }
                                                                    }
                                                                });

                                                                event!(Level::INFO, "Finished sending all the bytes of {file_hash} for: {remote_addr}.")
                                                                    },
                                                                    Err(_) => {
                                                                        event!(Level::ERROR, "Client has requested a file which is doesn't exist on the server.");
                                                                        if let Err(err) = send_stream.lock().await.write_all(&Message(None).to_sendable()).await {
                                                                            event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                                        }
                                                                    },
                                                                }


                                                            }
                                                            //If the file the client was not found we should indicate it by sending a ```None``` back.
                                                            else {
                                                                event!(Level::ERROR, "Client has requested a file which is not shared by the server.");
                                                                if let Err(err) = send_stream.lock().await.write_all(&Message(None).to_sendable()).await {
                                                                    event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                                };
                                                            }
                                                        },
                                                        crate::MessageType::KeepAlive => {
                                                            //Echo back to client
                                                            if let Err(err) = send_stream.lock().await.write_all(&received_message.to_sendable()).await {
                                                                event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                            }

                                                        }
                                                        crate::MessageType::FileTreeResponse(_) => unreachable!(),
                                                        crate::MessageType::FileResponse(_) => unreachable!(),
                                                        crate::MessageType::FilePacket(_) => unreachable!(),
                                                    }
                                                }
                                                else {
                                                        if let Err(err) = send_stream.lock().await.write_all(&Message(Some(crate::MessageType::FileTreeResponse(file_tree.clone()))).to_sendable()).await {
                                                            event!(Level::ERROR, "Error occured while writing to a client: {err}.")
                                                        }
                                                }
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
    }
}

pub mod client {
    use std::{net::Ipv6Addr, sync::Arc, time::Duration};

    use egui::Context;
    use quinn::{
        crypto::rustls::QuicClientConfig,
        rustls::{
            self,
            pki_types::{CertificateDer, ServerName, UnixTime},
        },
        ClientConfig, Endpoint, RecvStream,
    };
    use tokio::{
        io::AsyncReadExt,
        select,
        sync::mpsc::{channel, Receiver, Sender},
    };
    use tokio_util::sync::CancellationToken;
    use tracing::{event, Level};

    use crate::{read_message_length, Message, MessageType, Sendable};

    pub struct ConnectionInstance {
        pub file_trees: Vec<FileTree>,
        pub from_server_recv: Receiver<Message>,
        pub to_server_send: Sender<Message>,
    }

    pub async fn connect_to_server(
        address: String,
        ctx: Context,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ConnectionInstance> {
        let mut endpoint = Endpoint::client((Ipv6Addr::UNSPECIFIED, 0).into())?;

        endpoint.set_default_client_config(ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(SkipServerVerification::new())
                    .with_no_client_auth(),
            )?,
        )));

        let parsed_address = address.parse()?;

        event!(
            Level::INFO,
            "Opened local endpoint at: {:?} connecting to remote address: {parsed_address}",
            endpoint.local_addr()
        );

        let client: quinn::Connection = endpoint.connect(parsed_address, "localhost")?.await?;

        event!(Level::INFO, "Opening bidirectional connection...");

        let (mut send_stream, mut recv_stream) = client.clone().open_bi().await?;

        event!(
            Level::INFO,
            "Accepted bidirectional connection to {parsed_address}..."
        );

        //Send empty packet
        let _ = send_stream.write(&[0]).await;

        //Begin conversation
        //Read file tree automaticly sent by server
        let message_length = recv_stream.read_u64().await?;

        let mut buf = vec![0; message_length as usize];

        event!(Level::INFO, "Getting file tree...");

        recv_stream.read_exact(&mut buf).await?;

        let message = Message::try_from(buf.as_slice())?;

        if let Some(MessageType::FileTreeResponse(file_tree)) = message.0 {
            // The service uses `to_server_listener` to listen for messages coming from `to_server_send` (From the front end to the async task to be sent to the server)
            let (to_server_send, mut to_server_listener) = channel::<Message>(1000);

            // The service uses `from_server_send` to send messages to `from_server_listener` (To the front end, the non-async main thread to be interpreted by the client)
            let (from_server_send, from_server_listener) = channel::<Message>(1000);

            let listener_cancellation_token_clone = cancellation_token.clone();
            let sender_cancellation_token_clone = cancellation_token.clone();

            event!(Level::INFO, "Starting server listener...");

            //Spawn client communicator service handlers
            //Listener
            tokio::spawn(async move {
                loop {
                    select! {
                        _ = listener_cancellation_token_clone.cancelled() => break,

                        message_header = read_message_length(&mut recv_stream) => {
                            match message_header {
                                Ok(header_length) => {
                                    match handle_incoming_message(header_length as usize, &mut recv_stream).await  {
                                        Ok(message) => {
                                            match &message.0 {
                                                Some(inner) => {
                                                    event!(Level::DEBUG, "Received message ({inner}) from server.",);
                                                },
                                                None => {
                                                    event!(Level::ERROR, "Received message empty `None` from server indicating an issue.");
                                                },
                                            }

                                            from_server_send.send(message).await.unwrap();
                                            ctx.request_repaint();
                                        },
                                        Err(err) => {
                                            event!(Level::ERROR, "Received invalid input from server: {err}")
                                        },
                                    }
                                },
                                Err(err) => {
                                    event!(Level::ERROR, "Error occured while reading from the server, if it has timed out after inactivity this is expected.: {err}");
                                    panic!()
                                },
                            }
                        }
                    }
                }
            });

            event!(Level::INFO, "Starting client sender...");

            //Sender
            tokio::spawn(async move {
                loop {
                    select! {
                        _ = sender_cancellation_token_clone.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(10)) => {
                            if let Err(err) = send_stream.write_all(&Message(Some(MessageType::KeepAlive)).to_sendable()).await {
                                event!(Level::ERROR, "Error occured while trying to write to the server: {err}");
                            }

                            event!(Level::DEBUG, "Sent KeepAlive message to server.")
                        }
                        Some(message) = to_server_listener.recv() => {
                            if let Err(err) = send_stream.write_all(&message.clone().to_sendable()).await {
                                event!(Level::ERROR, "Error occured while trying to write to the server: {err}");
                            }

                            event!(Level::DEBUG, "Message sent to server: {message:?}");
                        },
                    }
                }
            });

            event!(
                Level::INFO,
                "Successfully established connection with remote address ({parsed_address})."
            );

            Ok(ConnectionInstance {
                file_trees: file_tree,
                from_server_recv: from_server_listener,
                to_server_send,
            })
        } else {
            Err(anyhow::Error::msg(
                "Received invalid data exchange from server.",
            ))
        }
    }

    #[derive(serde::Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
    pub enum FileTree {
        Folder((String, Vec<FileTree>)),
        File((String, String)),
        Empty,
    }

    async fn handle_incoming_message(
        header_length: usize,
        recv_stream: &mut RecvStream,
    ) -> anyhow::Result<Message> {
        let mut buf = vec![0; header_length];

        recv_stream.read_exact(&mut buf).await?;

        let mut message = Message::try_from(buf.as_slice())?;

        if let Some(MessageType::FilePacket(file_packet)) = &mut message.0 {
            let file_bytes_length = file_packet.bytes_length;

            let mut buf = vec![0; file_bytes_length];

            recv_stream.read_exact(&mut buf).await?;

            file_packet.bytes = buf;
        }

        Ok(message)
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

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct Message(pub Option<MessageType>);

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, strum::Display)]
pub enum MessageType {
    #[strum(to_string = "FileTreeRequest")]
    FileTreeRequest,
    #[strum(to_string = "FileRequest({0})")]
    FileRequest(String),

    #[strum(to_string = "FileResponse({0:?})")]
    FileResponse(FileReponseHeader),

    #[strum(to_string = "FileTreeResponse")]
    FileTreeResponse(Vec<client::FileTree>),

    #[strum(to_string = "FilePacket")]
    FilePacket(FilePacket),
    #[strum(to_string = "KeepAlive")]
    KeepAlive,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct FileReponseHeader {
    pub file_name: String,
    pub file_hash: String,
    pub total_size: u64,
    pub packet_identificator: String,
    pub file_packets: Vec<String>,
    pub file_packet_count: u64,
}

#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct FilePacket {
    pub file_hash: String,
    pub packet_id: usize,
    pub parent_id: String,
    pub bytes_length: usize,
    pub bytes: Vec<u8>,
}

/// The Sendable traits implements functions for being able to turn the `Struct` into Bytes.
trait Sendable {
    /// This function serializes `self` with rmp_serde and returns the Bytes.
    fn to_sendable(self) -> Vec<u8>;
}

impl Sendable for Message {
    fn to_sendable(mut self) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::with_capacity(size_of::<usize>() + size_of::<Self>());

        if let Some(MessageType::FilePacket(file_packet)) = &mut self.0 {
            let bytes = mem::take(&mut file_packet.bytes);

            let message = rmp_serde::to_vec(&self).unwrap();

            buf.extend(message.len().to_be_bytes().to_vec());
            buf.extend(message);
            buf.extend(bytes);
        } else {
            let message = rmp_serde::to_vec(&self).unwrap();
            buf.extend(message.len().to_be_bytes().to_vec());
            buf.extend(message);
        }

        buf
    }
}

impl TryFrom<&[u8]> for Message {
    type Error = rmp_serde::decode::Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        rmp_serde::from_slice(value)
    }
}

pub async fn read_message_length(recv: &mut RecvStream) -> anyhow::Result<u64> {
    let message_length = recv.read_u64().await?;

    if message_length > MESSAGE_LENGTH_LIMIT {
        bail!("Message length is too long, rejecting request.");
    } else {
        Ok(message_length)
    }
}

/// Stores information about a download.
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct DownloadHeader {
    /// The file's `FileResponseHeader`.
    pub file_response: FileReponseHeader,
    /// The list of the arrived `FilePacket`'s packet_id.
    pub packet_list: Vec<usize>,
    /// Arrived byte count.
    pub arrived_bytes: usize,
    /// The timestamp when this instance of `DownloadHeader` got initiated.
    pub initiated_stamp: DateTime<Local>,
}

pub fn get_file_handle(file_path: PathBuf) -> anyhow::Result<(std::fs::File, Metadata)> {
    let file_handle = std::fs::File::open(file_path)?;

    let file_metadata = file_handle.metadata()?;

    Ok((file_handle, file_metadata))
}
