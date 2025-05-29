use crate::message_protocol::*;
use anyhow::{Result, Context};
use bytes::{Buf, BufMut, BytesMut};
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::codec::{Decoder, Encoder, Framed};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Codec for encoding/decoding network messages
pub struct MessageCodec;

impl Decoder for MessageCodec {
    type Item = NetworkMessage;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

        if src.len() < 4 + length {
            return Ok(None);
        }

        src.advance(4);
        let data = src.split_to(length);
        let message: NetworkMessage = bincode::deserialize(&data)
            .context("Failed to deserialize network message")?;

        Ok(Some(message))
    }
}

impl Encoder<NetworkMessage> for MessageCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: NetworkMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let data = bincode::serialize(&item)
            .context("Failed to serialize network message")?;

        let length = data.len() as u32;
        dst.reserve(4 + data.len());
        dst.put_u32(length);
        dst.put_slice(&data);

        Ok(())
    }
}

/// Connection information for a peer
#[derive(Debug, Clone)]
pub struct PeerConnection {
    pub node_id: NodeId,
    pub addr: SocketAddr,
    pub sender: mpsc::UnboundedSender<NetworkMessage>,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

/// Events emitted by the network manager
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// New peer connected
    PeerConnected(NodeId, SocketAddr),
    /// Peer disconnected
    PeerDisconnected(NodeId, String),
    /// Message received from peer
    MessageReceived(NodeId, NetworkMessage),
    /// Connection error
    ConnectionError(SocketAddr, String),
    /// Listening started
    ListeningStarted(SocketAddr),
}

/// Network manager for handling TCP connections and message routing
pub struct NetworkManager {
    config: NetworkConfig,
    node_id: NodeId,
    connections: Arc<RwLock<HashMap<NodeId, PeerConnection>>>,
    event_sender: mpsc::UnboundedSender<NetworkEvent>,
    event_receiver: Arc<Mutex<mpsc::UnboundedReceiver<NetworkEvent>>>,
    broadcast_sender: mpsc::UnboundedSender<NetworkMessage>,
    broadcast_receiver: Arc<Mutex<mpsc::UnboundedReceiver<NetworkMessage>>>,
}

impl NetworkManager {
    pub fn new(config: NetworkConfig, node_id: NodeId) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let (broadcast_sender, broadcast_receiver) = mpsc::unbounded_channel();

        Self {
            node_id,
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            event_receiver: Arc::new(Mutex::new(event_receiver)),
            broadcast_sender,
            broadcast_receiver: Arc::new(Mutex::new(broadcast_receiver)),
        }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Get a sender for broadcasting messages to all connected peers
    pub fn get_sender(&self) -> mpsc::UnboundedSender<NetworkMessage> {
        self.broadcast_sender.clone()
    }

    /// Start the broadcast handler
    pub async fn start_broadcast_handler(&self) {
        let broadcast_receiver = self.broadcast_receiver.clone();
        let connections = self.connections.clone();

        tokio::spawn(async move {
            let mut receiver = broadcast_receiver.lock().await;
            while let Some(message) = receiver.recv().await {
                let connections_read = connections.read().await;
                for connection in connections_read.values() {
                    if let Err(e) = connection.sender.send(message.clone()) {
                        warn!("Failed to send broadcast message to {}: {}", connection.node_id, e);
                    }
                }
            }
        });
    }

    /// Start listening for incoming connections
    pub async fn start_listening(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr).await
            .context("Failed to bind TCP listener")?;

        info!("Network manager listening on {}", self.config.listen_addr);

        let _ = self.event_sender.send(NetworkEvent::ListeningStarted(self.config.listen_addr));

        let connections = self.connections.clone();
        let event_sender = self.event_sender.clone();
        let node_id = self.node_id;

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        debug!("Accepted connection from {}", addr);

                        let connections = connections.clone();
                        let event_sender = event_sender.clone();

                        tokio::spawn(async move {
                            if let Err(e) = Self::handle_incoming_connection(
                                stream, addr, node_id, connections, event_sender
                            ).await {
                                error!("Error handling incoming connection from {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    /// Connect to a remote peer
    pub async fn connect_to_peer(&self, addr: SocketAddr) -> Result<()> {
        info!("Connecting to peer at {}", addr);

        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.connection_timeout_secs),
            TcpStream::connect(addr)
        ).await
            .context("Connection timeout")?
            .context("Failed to connect to peer")?;

        let connections = self.connections.clone();
        let event_sender = self.event_sender.clone();
        let node_id = self.node_id;

        tokio::spawn(async move {
            if let Err(e) = Self::handle_outgoing_connection(
                stream, addr, node_id, connections, event_sender
            ).await {
                error!("Error handling outgoing connection to {}: {}", addr, e);
            }
        });

        Ok(())
    }

    /// Send a message to a specific peer
    pub async fn send_to_peer(&self, peer_id: &NodeId, message: NetworkMessage) -> Result<()> {
        let connections = self.connections.read().await;

        if let Some(connection) = connections.get(peer_id) {
            connection.sender.send(message)
                .context("Failed to send message to peer")?;
            Ok(())
        } else {
            anyhow::bail!("Peer {} not connected", peer_id);
        }
    }

    /// Broadcast a message to all connected peers
    pub async fn broadcast(&self, message: NetworkMessage) -> Result<()> {
        let connections = self.connections.read().await;

        for connection in connections.values() {
            if let Err(e) = connection.sender.send(message.clone()) {
                warn!("Failed to send broadcast message to {}: {}", connection.node_id, e);
            }
        }

        Ok(())
    }

    /// Get list of connected peers
    pub async fn get_connected_peers(&self) -> Vec<(NodeId, SocketAddr)> {
        let connections = self.connections.read().await;
        connections.values()
            .map(|conn| (conn.node_id, conn.addr))
            .collect()
    }

    /// Disconnect from a peer
    pub async fn disconnect_peer(&self, peer_id: &NodeId) -> Result<()> {
        let mut connections = self.connections.write().await;

        if let Some(_connection) = connections.remove(peer_id) {
            // Sender will be dropped, causing the connection task to terminate
            info!("Disconnected from peer {}", peer_id);
            let _ = self.event_sender.send(NetworkEvent::PeerDisconnected(
                *peer_id,
                "Manual disconnect".to_string()
            ));
        }

        Ok(())
    }

    /// Get the next network event
    pub async fn next_event(&self) -> Option<NetworkEvent> {
        let mut receiver = self.event_receiver.lock().await;
        receiver.recv().await
    }

    /// Handle incoming TCP connection
    async fn handle_incoming_connection(
        stream: TcpStream,
        addr: SocketAddr,
        local_node_id: NodeId,
        connections: Arc<RwLock<HashMap<NodeId, PeerConnection>>>,
        event_sender: mpsc::UnboundedSender<NetworkEvent>,
    ) -> Result<()> {
        Self::handle_connection(stream, addr, local_node_id, connections, event_sender, true).await
    }

    /// Handle outgoing TCP connection
    async fn handle_outgoing_connection(
        stream: TcpStream,
        addr: SocketAddr,
        local_node_id: NodeId,
        connections: Arc<RwLock<HashMap<NodeId, PeerConnection>>>,
        event_sender: mpsc::UnboundedSender<NetworkEvent>,
    ) -> Result<()> {
        Self::handle_connection(stream, addr, local_node_id, connections, event_sender, false).await
    }

    /// Handle a TCP connection (common logic for incoming/outgoing)
    async fn handle_connection(
        stream: TcpStream,
        addr: SocketAddr,
        local_node_id: NodeId,
        connections: Arc<RwLock<HashMap<NodeId, PeerConnection>>>,
        event_sender: mpsc::UnboundedSender<NetworkEvent>,
        is_incoming: bool,
    ) -> Result<()> {
        let mut framed = Framed::new(stream, MessageCodec);
        let (message_sender, mut message_receiver) = mpsc::unbounded_channel();

        // Perform handshake to exchange node IDs
        let peer_node_id = if is_incoming {
            // Wait for discovery message from peer
            match framed.next().await {
                Some(Ok(NetworkMessage::Discovery(discovery))) => {
                    debug!("Received discovery from {}: {}", addr, discovery.node_id);

                    // Send our discovery response
                    let our_discovery = DiscoveryMessage {
                        node_id: local_node_id,
                        node_info: NodeInfo::new(local_node_id, "nhi-node".to_string(), addr),
                        cluster_nodes: vec![], // TODO: Add current cluster nodes
                    };

                    framed.send(NetworkMessage::Discovery(our_discovery)).await?;
                    discovery.node_id
                }
                Some(Ok(msg)) => {
                    warn!("Expected discovery message, got {:?}", msg);
                    anyhow::bail!("Invalid handshake");
                }
                Some(Err(e)) => return Err(e),
                None => anyhow::bail!("Connection closed during handshake"),
            }
        } else {
            // Send discovery message to peer
            let our_discovery = DiscoveryMessage {
                node_id: local_node_id,
                node_info: NodeInfo::new(local_node_id, "nhi-node".to_string(), addr),
                cluster_nodes: vec![], // TODO: Add current cluster nodes
            };

            framed.send(NetworkMessage::Discovery(our_discovery)).await?;

            // Wait for discovery response
            match framed.next().await {
                Some(Ok(NetworkMessage::Discovery(discovery))) => {
                    debug!("Received discovery response from {}: {}", addr, discovery.node_id);
                    discovery.node_id
                }
                Some(Ok(msg)) => {
                    warn!("Expected discovery response, got {:?}", msg);
                    anyhow::bail!("Invalid handshake response");
                }
                Some(Err(e)) => return Err(e),
                None => anyhow::bail!("Connection closed during handshake"),
            }
        };

        // Register the connection
        let connection = PeerConnection {
            node_id: peer_node_id,
            addr,
            sender: message_sender,
            connected_at: chrono::Utc::now(),
        };

        {
            let mut conns = connections.write().await;
            conns.insert(peer_node_id, connection);
        }

        let _ = event_sender.send(NetworkEvent::PeerConnected(peer_node_id, addr));

        // Split the framed stream for sending and receiving
        let (mut sink, mut stream) = framed.split();

        // Handle message sending
        let send_task = tokio::spawn(async move {
            while let Some(message) = message_receiver.recv().await {
                if let Err(e) = sink.send(message).await {
                    error!("Failed to send message: {}", e);
                    break;
                }
            }
        });

        // Handle message receiving
        let event_sender_clone = event_sender.clone();
        let connections_clone = connections.clone();
        let receive_task = tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(message) => {
                        let _ = event_sender_clone.send(NetworkEvent::MessageReceived(peer_node_id, message));
                    }
                    Err(e) => {
                        error!("Error receiving message from {}: {}", peer_node_id, e);
                        break;
                    }
                }
            }

            // Clean up connection
            {
                let mut conns = connections_clone.write().await;
                conns.remove(&peer_node_id);
            }

            let _ = event_sender_clone.send(NetworkEvent::PeerDisconnected(
                peer_node_id,
                "Connection closed".to_string()
            ));
        });

        // Wait for either task to complete
        tokio::select! {
            _ = send_task => {},
            _ = receive_task => {},
        }

        Ok(())
    }
}
