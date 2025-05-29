use crate::cluster_state::{ClusterEvent, ClusterStateManager};
use crate::message_protocol::*;
use crate::network_manager::{NetworkEvent, NetworkManager};
use crate::node_discovery::{DiscoveryEvent, NodeDiscovery};
use crate::shadow_instance_manager::ShadowInstanceManager;
use anyhow::{Result, Context};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// High-level node manager that coordinates networking, discovery, and cluster state
pub struct NodeManager {
    network_manager: Arc<NetworkManager>,
    discovery_service: Arc<NodeDiscovery>,
    cluster_state: Arc<ClusterStateManager>,
    local_node_info: NodeInfo,
    is_running: Arc<Mutex<bool>>,
    shadow_manager: Arc<Mutex<Option<Arc<RwLock<ShadowInstanceManager>>>>>,
}

impl NodeManager {
    pub fn new(config: NetworkConfig) -> Result<Self> {
        let node_id = Uuid::new_v4();

        // Create local node info
        let local_node_info = NodeInfo::new(
            node_id,
            config.node_name.clone(),
            config.listen_addr,
        );

        // Initialize components
        let network_manager = Arc::new(NetworkManager::new(config.clone(), local_node_info.node_id));
        let discovery_service = Arc::new(NodeDiscovery::new(config.clone(), local_node_info.clone()));
        let cluster_state = Arc::new(ClusterStateManager::new(node_id));

        Ok(Self {
            network_manager,
            discovery_service,
            cluster_state,
            local_node_info,
            is_running: Arc::new(Mutex::new(false)),
            shadow_manager: Arc::new(Mutex::new(None)),
        })
    }

    /// Start the node manager and all its services
    pub async fn start(&self) -> Result<()> {
        {
            let mut running = self.is_running.lock().await;
            if *running {
                return Ok(());
            }
            *running = true;
        }

        info!("Starting NHI node manager");

        // Add local node to cluster state
        self.cluster_state.add_local_node(self.local_node_info.clone()).await?;

        // Start network manager
        self.network_manager.start_listening().await
            .context("Failed to start network manager")?;

        // Start broadcast handler
        self.network_manager.start_broadcast_handler().await;

        // Start discovery service
        self.discovery_service.start().await
            .context("Failed to start discovery service")?;

        // Start event processing loops
        self.start_event_loops().await;

        // Start periodic tasks
        self.start_periodic_tasks().await;

        info!("Node manager started successfully");
        Ok(())
    }

    /// Stop the node manager
    pub async fn stop(&self) -> Result<()> {
        let mut running = self.is_running.lock().await;
        *running = false;

        // Send goodbye message to all connected peers
        let goodbye_message = NetworkMessage::Goodbye(GoodbyeMessage {
            sender_id: self.node_id(),
            reason: "Node shutting down".to_string(),
        });

        if let Err(e) = self.network_manager.broadcast(goodbye_message).await {
            warn!("Failed to send goodbye message: {}", e);
        }

        info!("Node manager stopped");
        Ok(())
    }

    /// Get the local node ID
    pub fn node_id(&self) -> NodeId {
        self.local_node_info.node_id
    }

    /// Get the local node info
    pub fn local_node_info(&self) -> &NodeInfo {
        &self.local_node_info
    }

    /// Get cluster state manager
    pub fn cluster_state(&self) -> &Arc<ClusterStateManager> {
        &self.cluster_state
    }

    /// Get network manager
    pub fn network_manager(&self) -> &Arc<NetworkManager> {
        &self.network_manager
    }

    /// Set shadow manager for handling instance sync messages
    pub async fn set_shadow_manager(&self, shadow_manager: Arc<RwLock<ShadowInstanceManager>>) {
        let mut mgr = self.shadow_manager.lock().await;
        *mgr = Some(shadow_manager);
    }

    /// Connect to a specific peer
    pub async fn connect_to_peer(&self, addr: SocketAddr) -> Result<()> {
        info!("Attempting to connect to peer at {}", addr);
        self.network_manager.connect_to_peer(addr).await
    }

    /// Disconnect from a peer
    pub async fn disconnect_peer(&self, node_id: &NodeId) -> Result<()> {
        info!("Disconnecting from peer {}", node_id);

        // Update cluster state
        self.cluster_state.remove_node(node_id, "Manual disconnect".to_string()).await?;

        // Disconnect from network
        self.network_manager.disconnect_peer(node_id).await
    }

    /// Get list of connected peers
    pub async fn get_connected_peers(&self) -> Vec<(NodeId, SocketAddr)> {
        self.network_manager.get_connected_peers().await
    }

    /// Get cluster information
    pub async fn get_cluster_info(&self) -> String {
        self.cluster_state.format_cluster_info().await
    }

    /// Get node list
    pub async fn get_node_list(&self) -> String {
        self.cluster_state.format_node_list().await
    }

    /// Start event processing loops
    async fn start_event_loops(&self) {
        // Network events loop
        let network_manager = self.network_manager.clone();
        let cluster_state = self.cluster_state.clone();
        let shadow_manager = self.shadow_manager.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            while *is_running.lock().await {
                if let Some(event) = network_manager.next_event().await {
                    if let Err(e) = Self::handle_network_event(event, &cluster_state, &network_manager, &shadow_manager).await {
                        error!("Error handling network event: {}", e);
                    }
                }
            }
        });

        // Discovery events loop
        let discovery_service = self.discovery_service.clone();
        let network_manager = self.network_manager.clone();
        let cluster_state = self.cluster_state.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            while *is_running.lock().await {
                if let Some(event) = discovery_service.next_event().await {
                    if let Err(e) = Self::handle_discovery_event(event, &network_manager, &cluster_state).await {
                        error!("Error handling discovery event: {}", e);
                    }
                }
            }
        });

        // Cluster events loop
        let cluster_state = self.cluster_state.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            while *is_running.lock().await {
                if let Some(event) = cluster_state.next_event().await {
                    Self::handle_cluster_event(event).await;
                }
            }
        });
    }

    /// Start periodic maintenance tasks
    async fn start_periodic_tasks(&self) {
        // Heartbeat task
        let network_manager = self.network_manager.clone();
        let local_node_id = self.node_id();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10)); // Send heartbeat every 10 seconds

            while *is_running.lock().await {
                interval.tick().await;

                let heartbeat = NetworkMessage::Heartbeat(HeartbeatMessage {
                    sender_id: local_node_id,
                    timestamp: chrono::Utc::now(),
                    load_info: NodeLoadInfo {
                        cpu_usage: 0.0, // TODO: Get actual system metrics
                        memory_usage: 0.0,
                        active_instances: 0,
                        network_connections: 0,
                    },
                });

                if let Err(e) = network_manager.broadcast(heartbeat).await {
                    error!("Failed to send heartbeat: {}", e);
                } else {
                    debug!("Sent heartbeat from {}", local_node_id);
                }
            }
        });

        // Node timeout cleanup task
        let cluster_state = self.cluster_state.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60)); // Check every minute

            while *is_running.lock().await {
                interval.tick().await;

                let now = chrono::Utc::now();
                let timeout_duration = chrono::Duration::seconds(300); // 5 minutes timeout

                let cluster_state_data = cluster_state.get_cluster_state().await;
                let mut nodes_to_remove = Vec::new();

                for (node_id, node_info) in &cluster_state_data.nodes {
                    if *node_id != cluster_state.local_node_id() {
                        let time_since_last_seen = now - node_info.last_seen;
                        if time_since_last_seen > timeout_duration {
                            warn!("Node {} has been unresponsive for {} seconds",
                                  node_id, time_since_last_seen.num_seconds());
                            nodes_to_remove.push(*node_id);
                        }
                    }
                }

                for node_id in nodes_to_remove {
                    warn!("Removing unresponsive node: {}", node_id);
                    if let Err(e) = cluster_state.remove_node(&node_id, "Node timeout".to_string()).await {
                        error!("Failed to remove timed out node {}: {}", node_id, e);
                    }
                }
            }
        });

        // Cluster state sync task
        let network_manager = self.network_manager.clone();
        let cluster_state = self.cluster_state.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));

            while *is_running.lock().await {
                interval.tick().await;

                let sync_message = cluster_state.create_sync_message().await;
                let network_message = NetworkMessage::ClusterSync(sync_message);

                if let Err(e) = network_manager.broadcast(network_message).await {
                    debug!("Failed to send cluster sync: {}", e);
                }
            }
        });
    }

    /// Handle network events
    async fn handle_network_event(
        event: NetworkEvent,
        cluster_state: &Arc<ClusterStateManager>,
        network_manager: &Arc<NetworkManager>,
        shadow_manager: &Arc<Mutex<Option<Arc<RwLock<ShadowInstanceManager>>>>>,
    ) -> Result<()> {
        match event {
            NetworkEvent::PeerConnected(node_id, addr) => {
                info!("Peer connected: {} at {}", node_id, addr);

                // Update cluster state
                cluster_state.update_node_status(&node_id, NodeStatus::Online).await?;
            }
            NetworkEvent::PeerDisconnected(node_id, reason) => {
                info!("Peer disconnected: {} ({})", node_id, reason);

                // Update cluster state to offline but don't remove immediately
                // Let the timeout mechanism handle removal after grace period
                cluster_state.update_node_status(&node_id, NodeStatus::Offline).await?;
            }
            NetworkEvent::MessageReceived(sender_id, message) => {
                Self::handle_network_message(sender_id, message, cluster_state, network_manager, shadow_manager).await?;
            }
            NetworkEvent::ConnectionError(addr, error) => {
                warn!("Connection error to {}: {}", addr, error);
            }
            NetworkEvent::ListeningStarted(addr) => {
                info!("Network manager listening on {}", addr);
            }
        }

        Ok(())
    }

    /// Handle discovery events
    async fn handle_discovery_event(
        event: DiscoveryEvent,
        network_manager: &Arc<NetworkManager>,
        cluster_state: &Arc<ClusterStateManager>,
    ) -> Result<()> {
        match event {
            DiscoveryEvent::NodeDiscovered(node_info) => {
                info!("Discovered node: {} at {}", node_info.node_id, node_info.listen_addr);

                // Add to cluster state
                cluster_state.add_node(node_info.clone()).await?;

                // Attempt to connect
                if let Err(e) = network_manager.connect_to_peer(node_info.listen_addr).await {
                    warn!("Failed to connect to discovered node {}: {}", node_info.node_id, e);
                }
            }
            DiscoveryEvent::NodeAnnouncement(node_info) => {
                debug!("Node announcement from: {}", node_info.node_id);
                cluster_state.add_node(node_info).await?;
            }
            DiscoveryEvent::DiscoveryError(error) => {
                warn!("Discovery error: {}", error);
            }
        }

        Ok(())
    }

    /// Handle cluster events
    async fn handle_cluster_event(event: ClusterEvent) {
        match event {
            ClusterEvent::NodeJoined(node_info) => {
                info!("Node joined cluster: {} ({})", node_info.node_id, node_info.name);
            }
            ClusterEvent::NodeLeft(node_id, reason) => {
                info!("Node left cluster: {} ({})", node_id, reason);
            }
            ClusterEvent::NodeStatusChanged(node_id, status) => {
                debug!("Node {} status changed to {:?}", node_id, status);
            }
            ClusterEvent::StateSynchronized => {
                debug!("Cluster state synchronized");
            }
            ClusterEvent::StateConflict(conflict) => {
                warn!("Cluster state conflict: {}", conflict);
            }
        }
    }

    /// Handle incoming network messages
    async fn handle_network_message(
        sender_id: NodeId,
        message: NetworkMessage,
        cluster_state: &Arc<ClusterStateManager>,
        network_manager: &Arc<NetworkManager>,
        shadow_manager: &Arc<Mutex<Option<Arc<RwLock<ShadowInstanceManager>>>>>,
    ) -> Result<()> {
        match message {
            NetworkMessage::Discovery(discovery) => {
                // Add discovered node to cluster
                cluster_state.add_node(discovery.node_info).await?;

                // Add any nodes from their cluster list
                for node_info in discovery.cluster_nodes {
                    cluster_state.add_node(node_info).await?;
                }
            }
            NetworkMessage::ClusterSync(sync) => {
                debug!("Received cluster sync from {}", sender_id);
                cluster_state.synchronize_state(sync.cluster_state).await?;
            }
            NetworkMessage::Request(request) => {
                Self::handle_request(request, cluster_state, network_manager).await?;
            }
            NetworkMessage::Response(response) => {
                debug!("Received response from {}: {:?}", sender_id, response.response_type);
            }
            NetworkMessage::Heartbeat(heartbeat) => {
                debug!("Received heartbeat from {}", sender_id);
                // Update node's last seen time and status
                if let Some(mut node_info) = cluster_state.get_node_info(&sender_id).await {
                    node_info.update_last_seen();
                    node_info.set_status(NodeStatus::Online);
                    cluster_state.add_node(node_info).await?;
                    debug!("Updated heartbeat for node {}", sender_id);
                } else {
                    // If we don't have this node, it might be a new connection
                    warn!("Received heartbeat from unknown node: {}", sender_id);
                }
            }
            NetworkMessage::Goodbye(goodbye) => {
                info!("Received goodbye from {}: {}", goodbye.sender_id, goodbye.reason);
                cluster_state.remove_node(&goodbye.sender_id, goodbye.reason).await?;
            }
            NetworkMessage::InstanceSync(instance_sync) => {
                debug!("Received instance sync from {}", sender_id);
                // Forward to shadow manager if available
                if let Some(shadow_mgr) = shadow_manager.lock().await.as_ref() {
                    let shadow_mgr_read = shadow_mgr.read().await;
                    if let Err(e) = shadow_mgr_read.handle_instance_sync(instance_sync).await {
                        error!("Failed to handle instance sync: {}", e);
                    }
                }
            }
            NetworkMessage::InstanceStop(instance_stop) => {
                debug!("Received instance stop from {} for instance {}", sender_id, instance_stop.instance_id);
                // Forward to shadow manager if available
                if let Some(shadow_mgr) = shadow_manager.lock().await.as_ref() {
                    let shadow_mgr_read = shadow_mgr.read().await;
                    if let Err(e) = shadow_mgr_read.handle_instance_stop(instance_stop).await {
                        error!("Failed to handle instance stop: {}", e);
                    }
                }
            }
            NetworkMessage::ShadowSync(shadow_sync) => {
                debug!("Received shadow sync from {}", sender_id);
                // Forward to shadow manager if available
                if let Some(shadow_mgr) = shadow_manager.lock().await.as_ref() {
                    let shadow_mgr_read = shadow_mgr.read().await;
                    if let Err(e) = shadow_mgr_read.handle_shadow_sync(shadow_sync).await {
                        error!("Failed to handle shadow sync: {}", e);
                    }
                }
            }
            NetworkMessage::ShadowInput(shadow_input) => {
                debug!("Received shadow input from {} for instance {}", sender_id, shadow_input.instance_id);
                // Forward input to the local process if this is the target node
                if shadow_input.target_node_id == cluster_state.local_node_id() {
                    // TODO: Forward input to the actual running process
                    // This would require access to the process manager
                    debug!("Would forward input '{}' to instance {}",
                           shadow_input.input_data, shadow_input.instance_id);
                }
            }
            NetworkMessage::Migration(_migration) => {
                // TODO: Handle migration coordination
                debug!("Received migration message from {}", sender_id);
            }
            NetworkMessage::DataStream(_data_stream) => {
                // TODO: Handle real-time data streaming
                debug!("Received data stream from {}", sender_id);
            }
        }

        Ok(())
    }

    /// Handle incoming requests
    async fn handle_request(
        request: RequestMessage,
        cluster_state: &Arc<ClusterStateManager>,
        network_manager: &Arc<NetworkManager>,
    ) -> Result<()> {
        let response_type = match request.request_type {
            RequestType::NodeInfo => {
                if let Some(node_info) = cluster_state.get_node_info(&cluster_state.local_node_id()).await {
                    ResponseType::NodeInfo(node_info)
                } else {
                    ResponseType::Error("Local node info not found".to_string())
                }
            }
            RequestType::ClusterStatus => {
                let cluster_state_data = cluster_state.get_cluster_state().await;
                ResponseType::ClusterStatus(cluster_state_data)
            }
            RequestType::ConnectRequest { listen_addr: _ } => {
                ResponseType::ConnectResponse {
                    accepted: true,
                    reason: None
                }
            }
            RequestType::Ping => {
                ResponseType::Pong
            }
        };

        let response = NetworkMessage::Response(ResponseMessage {
            request_id: request.request_id,
            sender_id: cluster_state.local_node_id(),
            response_type,
        });

        network_manager.send_to_peer(&request.sender_id, response).await?;
        Ok(())
    }
}
