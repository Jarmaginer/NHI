use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Unique identifier for a node in the cluster
pub type NodeId = Uuid;

/// Network message types for P2P communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// Node discovery and handshake
    Discovery(DiscoveryMessage),
    /// Cluster state synchronization
    ClusterSync(ClusterSyncMessage),
    /// Request-response pattern
    Request(RequestMessage),
    Response(ResponseMessage),
    /// Heartbeat for connection health
    Heartbeat(HeartbeatMessage),
    /// Node leaving notification
    Goodbye(GoodbyeMessage),
    /// Instance registry synchronization
    InstanceSync(InstanceSyncMessage),
    /// Instance stop notification
    InstanceStop(InstanceStopMessage),
    /// Shadow state data synchronization
    ShadowSync(ShadowSyncMessage),
    /// Shadow instance input forwarding
    ShadowInput(ShadowInputMessage),
    /// Migration command and coordination
    Migration(MigrationMessage),
    /// Real-time data streaming
    DataStream(DataStreamMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryMessage {
    pub node_id: NodeId,
    pub node_info: NodeInfo,
    pub cluster_nodes: Vec<NodeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSyncMessage {
    pub sender_id: NodeId,
    pub cluster_state: ClusterState,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub request_id: Uuid,
    pub sender_id: NodeId,
    pub request_type: RequestType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub request_id: Uuid,
    pub sender_id: NodeId,
    pub response_type: ResponseType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatMessage {
    pub sender_id: NodeId,
    pub timestamp: DateTime<Utc>,
    pub load_info: NodeLoadInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodbyeMessage {
    pub sender_id: NodeId,
    pub reason: String,
}

/// Types of requests that can be sent between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestType {
    /// Request node information
    NodeInfo,
    /// Request cluster status
    ClusterStatus,
    /// Request to connect to this node
    ConnectRequest { listen_addr: SocketAddr },
    /// Ping request for connectivity test
    Ping,
}

/// Response types for requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseType {
    /// Node information response
    NodeInfo(NodeInfo),
    /// Cluster status response
    ClusterStatus(ClusterState),
    /// Connection acceptance/rejection
    ConnectResponse { accepted: bool, reason: Option<String> },
    /// Pong response
    Pong,
    /// Error response
    Error(String),
}

/// Information about a node in the cluster
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeInfo {
    pub node_id: NodeId,
    pub name: String,
    pub listen_addr: SocketAddr,
    pub version: String,
    pub capabilities: Vec<String>,
    pub joined_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub status: NodeStatus,
}

/// Status of a node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    Online,
    Offline,
    Connecting,
    Disconnecting,
}

/// Current load and resource information for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLoadInfo {
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub active_instances: u32,
    pub network_connections: u32,
}

/// Complete cluster state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    pub nodes: HashMap<NodeId, NodeInfo>,
    pub cluster_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}

impl NodeInfo {
    pub fn new(node_id: NodeId, name: String, listen_addr: SocketAddr) -> Self {
        let now = Utc::now();
        Self {
            node_id,
            name,
            listen_addr,
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                "instance_management".to_string(),
                "criu_checkpoint".to_string(),
                "criu_restore".to_string(),
            ],
            joined_at: now,
            last_seen: now,
            status: NodeStatus::Online,
        }
    }

    pub fn update_last_seen(&mut self) {
        self.last_seen = Utc::now();
    }

    pub fn set_status(&mut self, status: NodeStatus) {
        self.status = status;
        self.update_last_seen();
    }
}

impl ClusterState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            nodes: HashMap::new(),
            cluster_id: Uuid::new_v4(),
            created_at: now,
            last_updated: now,
        }
    }

    pub fn add_node(&mut self, node_info: NodeInfo) {
        self.nodes.insert(node_info.node_id, node_info);
        self.last_updated = Utc::now();
    }

    pub fn remove_node(&mut self, node_id: &NodeId) -> Option<NodeInfo> {
        self.last_updated = Utc::now();
        self.nodes.remove(node_id)
    }

    pub fn update_node(&mut self, node_info: NodeInfo) {
        if let Some(existing) = self.nodes.get_mut(&node_info.node_id) {
            *existing = node_info;
            self.last_updated = Utc::now();
        }
    }

    pub fn get_node(&self, node_id: &NodeId) -> Option<&NodeInfo> {
        self.nodes.get(node_id)
    }

    pub fn get_online_nodes(&self) -> Vec<&NodeInfo> {
        self.nodes
            .values()
            .filter(|node| node.status == NodeStatus::Online)
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn online_node_count(&self) -> usize {
        self.get_online_nodes().len()
    }
}

impl Default for ClusterState {
    fn default() -> Self {
        Self::new()
    }
}

/// Network configuration for the node
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub listen_addr: SocketAddr,
    pub node_name: String,
    pub discovery_port: u16,
    pub heartbeat_interval_secs: u64,
    pub connection_timeout_secs: u64,
    pub max_connections: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".parse().unwrap(),
            node_name: format!("nhi-node-{}", Uuid::new_v4().to_string()[..8].to_uppercase()),
            discovery_port: 8081,
            heartbeat_interval_secs: 30,
            connection_timeout_secs: 10,
            max_connections: 100,
        }
    }
}

/// Instance registry synchronization message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSyncMessage {
    pub sender_id: NodeId,
    pub instances: Vec<InstanceInfo>,
    pub timestamp: DateTime<Utc>,
}

/// Instance stop notification message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceStopMessage {
    pub sender_id: NodeId,
    pub instance_id: Uuid,
    pub timestamp: DateTime<Utc>,
}

/// Shadow state synchronization message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowSyncMessage {
    pub sender_id: NodeId,
    pub instance_id: Uuid,
    pub data_version: u64,
    pub checkpoint_data: Option<Vec<u8>>,
    pub output_data: Option<Vec<u8>>,
    pub timestamp: DateTime<Utc>,
}

/// Shadow instance input forwarding message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowInputMessage {
    pub sender_id: NodeId,
    pub target_node_id: NodeId,
    pub instance_id: Uuid,
    pub input_data: String,
    pub timestamp: DateTime<Utc>,
}

/// Migration coordination message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationMessage {
    pub sender_id: NodeId,
    pub migration_type: MigrationType,
    pub instance_id: Uuid,
    pub target_node_id: Option<NodeId>,
    pub migration_id: Uuid,
    pub timestamp: DateTime<Utc>,
}

/// Real-time data streaming message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataStreamMessage {
    pub sender_id: NodeId,
    pub instance_id: Uuid,
    pub stream_type: StreamType,
    pub data: Vec<u8>,
    pub sequence_number: u64,
    pub timestamp: DateTime<Utc>,
}

/// Types of migration operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MigrationType {
    /// Request to migrate instance to target node
    MigrateRequest,
    /// Accept migration request
    MigrateAccept,
    /// Reject migration request
    MigrateReject { reason: String },
    /// Start migration process
    MigrateStart,
    /// Migration completed successfully
    MigrateComplete,
    /// Migration failed
    MigrateFailed { reason: String },
}

/// Types of data streams
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamType {
    /// Standard output stream
    Stdout,
    /// Standard error stream
    Stderr,
    /// Standard input stream
    Stdin,
    /// Checkpoint data stream
    Checkpoint,
    /// Process memory data
    Memory,
}

/// Instance information for registry synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub id: Uuid,
    pub program: String,
    pub args: Vec<String>,
    pub status: crate::types::InstanceStatus,
    pub node_id: NodeId, // Node where this instance is located
    pub created_at: DateTime<Utc>,
    pub source_node_id: Option<NodeId>, // For shadow instances
}
