use crate::message_protocol::*;
use anyhow::{Result, Context};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Discovery events emitted by the discovery service
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// New node discovered
    NodeDiscovered(NodeInfo),
    /// Node announcement received
    NodeAnnouncement(NodeInfo),
    /// Discovery error
    DiscoveryError(String),
}

/// UDP-based node discovery service for automatic peer detection
pub struct NodeDiscovery {
    config: NetworkConfig,
    local_node_info: NodeInfo,
    discovered_nodes: Arc<RwLock<HashSet<SocketAddr>>>,
    event_sender: mpsc::UnboundedSender<DiscoveryEvent>,
    event_receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<DiscoveryEvent>>>,
}

/// Discovery message sent over UDP
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DiscoveryPacket {
    message_type: DiscoveryMessageType,
    node_info: NodeInfo,
    timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum DiscoveryMessageType {
    /// Announce this node's presence
    Announce,
    /// Request other nodes to announce themselves
    Probe,
    /// Response to a probe request
    ProbeResponse,
}

impl NodeDiscovery {
    pub fn new(config: NetworkConfig, local_node_info: NodeInfo) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            config,
            local_node_info,
            discovered_nodes: Arc::new(RwLock::new(HashSet::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
        }
    }

    /// Start the discovery service
    pub async fn start(&self) -> Result<()> {
        info!("Starting node discovery on port {}", self.config.discovery_port);

        // Start UDP listener for discovery messages
        self.start_discovery_listener().await?;

        // Start periodic announcement
        self.start_periodic_announcement().await;

        // Start initial network probe
        self.start_network_probe().await;

        Ok(())
    }

    /// Get the next discovery event
    pub async fn next_event(&self) -> Option<DiscoveryEvent> {
        let mut receiver = self.event_receiver.lock().await;
        receiver.recv().await
    }

    /// Get list of discovered nodes
    pub async fn get_discovered_nodes(&self) -> Vec<SocketAddr> {
        let nodes = self.discovered_nodes.read().await;
        nodes.iter().cloned().collect()
    }

    /// Manually probe for nodes on the network
    pub async fn probe_network(&self) -> Result<()> {
        info!("Probing network for NHI nodes");

        let probe_packet = DiscoveryPacket {
            message_type: DiscoveryMessageType::Probe,
            node_info: self.local_node_info.clone(),
            timestamp: chrono::Utc::now(),
        };

        let data = bincode::serialize(&probe_packet)
            .context("Failed to serialize probe packet")?;

        // Send probe to broadcast address
        let broadcast_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::BROADCAST),
            self.config.discovery_port
        );

        let socket = UdpSocket::bind("0.0.0.0:0").await
            .context("Failed to bind UDP socket for probing")?;

        socket.set_broadcast(true)
            .context("Failed to enable broadcast on UDP socket")?;

        if let Err(e) = socket.send_to(&data, broadcast_addr).await {
            warn!("Failed to send broadcast probe: {}", e);
        }

        // Also probe localhost and common ports for development
        let localhost_ports = vec![8081, 8082, 8083, 8084, 8085];
        for port in localhost_ports {
            if port != self.config.discovery_port {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                if let Err(e) = socket.send_to(&data, addr).await {
                    debug!("Failed to send probe to localhost:{}: {}", port, e);
                }
            }
        }

        // Also probe common local network ranges
        let local_ranges = vec![
            "192.168.1.255",
            "192.168.0.255",
            "10.0.0.255",
            "172.16.0.255",
        ];

        for range in local_ranges {
            if let Ok(addr) = format!("{}:{}", range, self.config.discovery_port).parse::<SocketAddr>() {
                if let Err(e) = socket.send_to(&data, addr).await {
                    debug!("Failed to send probe to {}: {}", addr, e);
                }
            }
        }

        Ok(())
    }

    /// Start UDP listener for discovery messages
    async fn start_discovery_listener(&self) -> Result<()> {
        let bind_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            self.config.discovery_port
        );

        let socket = UdpSocket::bind(bind_addr).await
            .context("Failed to bind discovery UDP socket")?;

        info!("Discovery listener bound to {}", bind_addr);

        let event_sender = self.event_sender.clone();
        let local_node_info = self.local_node_info.clone();
        let discovered_nodes = self.discovered_nodes.clone();

        tokio::spawn(async move {
            let mut buffer = vec![0u8; 4096];

            loop {
                match socket.recv_from(&mut buffer).await {
                    Ok((len, sender_addr)) => {
                        if let Err(e) = Self::handle_discovery_packet(
                            &buffer[..len],
                            sender_addr,
                            &local_node_info,
                            &discovered_nodes,
                            &event_sender,
                            &socket,
                        ).await {
                            debug!("Error handling discovery packet from {}: {}", sender_addr, e);
                        }
                    }
                    Err(e) => {
                        error!("Error receiving discovery packet: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    /// Start periodic node announcement
    async fn start_periodic_announcement(&self) {
        let event_sender = self.event_sender.clone();
        let local_node_info = self.local_node_info.clone();
        let discovery_port = self.config.discovery_port;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10)); // Announce every 10 seconds

            loop {
                interval.tick().await;

                if let Err(e) = Self::send_announcement(&local_node_info, discovery_port).await {
                    let _ = event_sender.send(DiscoveryEvent::DiscoveryError(
                        format!("Failed to send announcement: {}", e)
                    ));
                }
            }
        });
    }

    /// Start initial network probe
    async fn start_network_probe(&self) {
        let discovery = self.clone();

        tokio::spawn(async move {
            // Wait a bit before starting probe to let listener start
            tokio::time::sleep(Duration::from_millis(500)).await;

            if let Err(e) = discovery.probe_network().await {
                error!("Initial network probe failed: {}", e);
            }
        });
    }

    /// Handle incoming discovery packet
    async fn handle_discovery_packet(
        data: &[u8],
        sender_addr: SocketAddr,
        local_node_info: &NodeInfo,
        discovered_nodes: &Arc<RwLock<HashSet<SocketAddr>>>,
        event_sender: &mpsc::UnboundedSender<DiscoveryEvent>,
        socket: &UdpSocket,
    ) -> Result<()> {
        let packet: DiscoveryPacket = bincode::deserialize(data)
            .context("Failed to deserialize discovery packet")?;

        // Ignore packets from ourselves
        if packet.node_info.node_id == local_node_info.node_id {
            return Ok(());
        }

        debug!("Received discovery packet from {}: {:?}", sender_addr, packet.message_type);

        match packet.message_type {
            DiscoveryMessageType::Announce => {
                // Node is announcing its presence
                let tcp_addr = packet.node_info.listen_addr;

                {
                    let mut nodes = discovered_nodes.write().await;
                    if nodes.insert(tcp_addr) {
                        info!("Discovered new node: {} at {}", packet.node_info.node_id, tcp_addr);
                        let _ = event_sender.send(DiscoveryEvent::NodeDiscovered(packet.node_info));
                    }
                }
            }
            DiscoveryMessageType::Probe => {
                // Someone is probing for nodes, respond with our info
                let response_packet = DiscoveryPacket {
                    message_type: DiscoveryMessageType::ProbeResponse,
                    node_info: local_node_info.clone(),
                    timestamp: chrono::Utc::now(),
                };

                if let Ok(response_data) = bincode::serialize(&response_packet) {
                    if let Err(e) = socket.send_to(&response_data, sender_addr).await {
                        debug!("Failed to send probe response to {}: {}", sender_addr, e);
                    }
                }
            }
            DiscoveryMessageType::ProbeResponse => {
                // Response to our probe
                let tcp_addr = packet.node_info.listen_addr;

                {
                    let mut nodes = discovered_nodes.write().await;
                    if nodes.insert(tcp_addr) {
                        info!("Discovered node via probe response: {} at {}", packet.node_info.node_id, tcp_addr);
                        let _ = event_sender.send(DiscoveryEvent::NodeDiscovered(packet.node_info));
                    }
                }
            }
        }

        Ok(())
    }

    /// Send announcement packet
    async fn send_announcement(node_info: &NodeInfo, discovery_port: u16) -> Result<()> {
        let announcement_packet = DiscoveryPacket {
            message_type: DiscoveryMessageType::Announce,
            node_info: node_info.clone(),
            timestamp: chrono::Utc::now(),
        };

        let data = bincode::serialize(&announcement_packet)
            .context("Failed to serialize announcement packet")?;

        let socket = UdpSocket::bind("0.0.0.0:0").await
            .context("Failed to bind UDP socket for announcement")?;

        socket.set_broadcast(true)
            .context("Failed to enable broadcast on UDP socket")?;

        // Send to broadcast address
        let broadcast_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::BROADCAST),
            discovery_port
        );

        if let Err(e) = socket.send_to(&data, broadcast_addr).await {
            debug!("Failed to send broadcast announcement: {}", e);
        }

        // Also send to localhost ports for development
        let localhost_ports = vec![8081, 8082, 8083, 8084, 8085];
        for port in localhost_ports {
            if port != discovery_port {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
                if let Err(e) = socket.send_to(&data, addr).await {
                    debug!("Failed to send announcement to localhost:{}: {}", port, e);
                }
            }
        }

        debug!("Sent node announcement to broadcast and localhost ports");
        Ok(())
    }
}

// Implement Clone for NodeDiscovery to allow sharing between tasks
impl Clone for NodeDiscovery {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            local_node_info: self.local_node_info.clone(),
            discovered_nodes: self.discovered_nodes.clone(),
            event_sender: self.event_sender.clone(),
            event_receiver: self.event_receiver.clone(),
        }
    }
}
