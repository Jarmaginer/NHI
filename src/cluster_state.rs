use crate::message_protocol::*;
use anyhow::Result;

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Events emitted by the cluster state manager
#[derive(Debug, Clone)]
pub enum ClusterEvent {
    /// Node joined the cluster
    NodeJoined(NodeInfo),
    /// Node left the cluster
    NodeLeft(NodeId, String),
    /// Node status updated
    NodeStatusChanged(NodeId, NodeStatus),
    /// Cluster state synchronized
    StateSynchronized,
    /// Cluster state conflict detected
    StateConflict(String),
}

/// Manages the distributed cluster state
pub struct ClusterStateManager {
    local_node_id: NodeId,
    cluster_state: Arc<RwLock<ClusterState>>,
    event_sender: mpsc::UnboundedSender<ClusterEvent>,
    event_receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ClusterEvent>>>,
}

impl ClusterStateManager {
    pub fn new(local_node_id: NodeId) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            local_node_id,
            cluster_state: Arc::new(RwLock::new(ClusterState::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
        }
    }

    /// Get the local node ID
    pub fn local_node_id(&self) -> NodeId {
        self.local_node_id
    }

    /// Add the local node to the cluster
    pub async fn add_local_node(&self, node_info: NodeInfo) -> Result<()> {
        let mut state = self.cluster_state.write().await;
        state.add_node(node_info.clone());

        info!("Added local node {} to cluster", node_info.node_id);
        let _ = self.event_sender.send(ClusterEvent::NodeJoined(node_info));

        Ok(())
    }

    /// Add a remote node to the cluster
    pub async fn add_node(&self, node_info: NodeInfo) -> Result<()> {
        let node_id = node_info.node_id;
        let is_new = {
            let mut state = self.cluster_state.write().await;
            let was_present = state.nodes.contains_key(&node_id);
            state.add_node(node_info.clone());
            !was_present
        };

        if is_new {
            info!("Node {} joined the cluster", node_id);
            let _ = self.event_sender.send(ClusterEvent::NodeJoined(node_info));
        } else {
            debug!("Updated existing node {} in cluster", node_id);
        }

        Ok(())
    }

    /// Remove a node from the cluster
    pub async fn remove_node(&self, node_id: &NodeId, reason: String) -> Result<()> {
        let removed = {
            let mut state = self.cluster_state.write().await;
            state.remove_node(node_id)
        };

        if removed.is_some() {
            info!("Node {} left the cluster: {}", node_id, reason);
            let _ = self.event_sender.send(ClusterEvent::NodeLeft(*node_id, reason));
        }

        Ok(())
    }

    /// Update node status
    pub async fn update_node_status(&self, node_id: &NodeId, status: NodeStatus) -> Result<()> {
        let updated = {
            let mut state = self.cluster_state.write().await;
            if let Some(node) = state.nodes.get_mut(node_id) {
                let old_status = node.status.clone();
                node.set_status(status.clone());
                old_status != status
            } else {
                false
            }
        };

        if updated {
            debug!("Node {} status changed to {:?}", node_id, status);
            let _ = self.event_sender.send(ClusterEvent::NodeStatusChanged(*node_id, status));
        }

        Ok(())
    }

    /// Get current cluster state
    pub async fn get_cluster_state(&self) -> ClusterState {
        let state = self.cluster_state.read().await;
        state.clone()
    }

    /// Get information about a specific node
    pub async fn get_node_info(&self, node_id: &NodeId) -> Option<NodeInfo> {
        let state = self.cluster_state.read().await;
        state.get_node(node_id).cloned()
    }

    /// Get all online nodes
    pub async fn get_online_nodes(&self) -> Vec<NodeInfo> {
        let state = self.cluster_state.read().await;
        state.get_online_nodes().into_iter().cloned().collect()
    }

    /// Get cluster statistics
    pub async fn get_cluster_stats(&self) -> ClusterStats {
        let state = self.cluster_state.read().await;

        let total_nodes = state.node_count();
        let online_nodes = state.online_node_count();
        let offline_nodes = total_nodes - online_nodes;

        ClusterStats {
            total_nodes,
            online_nodes,
            offline_nodes,
            cluster_id: state.cluster_id,
            created_at: state.created_at,
            last_updated: state.last_updated,
        }
    }

    /// Synchronize cluster state with remote state
    pub async fn synchronize_state(&self, remote_state: ClusterState) -> Result<()> {
        let mut conflicts = Vec::new();

        {
            let mut local_state = self.cluster_state.write().await;

            // Check for conflicts and merge states
            for (node_id, remote_node) in &remote_state.nodes {
                if let Some(local_node) = local_state.nodes.get(node_id) {
                    // Node exists in both states, check for conflicts
                    if local_node.last_seen < remote_node.last_seen {
                        // Remote state is newer, update local
                        local_state.update_node(remote_node.clone());
                        debug!("Updated node {} from remote state", node_id);
                    } else if local_node.last_seen > remote_node.last_seen {
                        // Local state is newer, keep local
                        debug!("Keeping local state for node {} (newer)", node_id);
                    } else {
                        // Same timestamp, check for actual differences
                        if local_node != remote_node {
                            conflicts.push(format!(
                                "Node {} has conflicting state with same timestamp",
                                node_id
                            ));
                        }
                    }
                } else {
                    // Node only exists in remote state, add it
                    local_state.add_node(remote_node.clone());
                    info!("Added node {} from remote state", node_id);
                    let _ = self.event_sender.send(ClusterEvent::NodeJoined(remote_node.clone()));
                }
            }

            // Check for nodes that exist locally but not remotely
            let local_only_nodes: Vec<NodeId> = local_state.nodes.keys()
                .filter(|id| !remote_state.nodes.contains_key(id) && **id != self.local_node_id)
                .cloned()
                .collect();

            for node_id in local_only_nodes {
                warn!("Node {} exists locally but not in remote state", node_id);
                // Don't automatically remove - might be a network partition
            }
        }

        // Report conflicts
        for conflict in conflicts {
            warn!("State conflict: {}", conflict);
            let _ = self.event_sender.send(ClusterEvent::StateConflict(conflict));
        }

        let _ = self.event_sender.send(ClusterEvent::StateSynchronized);
        Ok(())
    }

    /// Create a cluster sync message
    pub async fn create_sync_message(&self) -> ClusterSyncMessage {
        let state = self.cluster_state.read().await;

        ClusterSyncMessage {
            sender_id: self.local_node_id,
            cluster_state: state.clone(),
            timestamp: Utc::now(),
        }
    }

    /// Get the next cluster event
    pub async fn next_event(&self) -> Option<ClusterEvent> {
        let mut receiver = self.event_receiver.lock().await;
        receiver.recv().await
    }

    /// Format cluster information for display
    pub async fn format_cluster_info(&self) -> String {
        let state = self.cluster_state.read().await;
        let stats = ClusterStats {
            total_nodes: state.node_count(),
            online_nodes: state.online_node_count(),
            offline_nodes: state.node_count() - state.online_node_count(),
            cluster_id: state.cluster_id,
            created_at: state.created_at,
            last_updated: state.last_updated,
        };

        let mut output = String::new();
        output.push_str(&format!("Cluster ID: {}\n", stats.cluster_id));
        output.push_str(&format!("Created: {}\n", stats.created_at.format("%Y-%m-%d %H:%M:%S UTC")));
        output.push_str(&format!("Last Updated: {}\n", stats.last_updated.format("%Y-%m-%d %H:%M:%S UTC")));
        output.push_str(&format!("Total Nodes: {}\n", stats.total_nodes));
        output.push_str(&format!("Online Nodes: {}\n", stats.online_nodes));
        output.push_str(&format!("Offline Nodes: {}\n", stats.offline_nodes));

        if !state.nodes.is_empty() {
            output.push_str("\nNodes:\n");
            for node in state.nodes.values() {
                let status_icon = match node.status {
                    NodeStatus::Online => "🟢",
                    NodeStatus::Offline => "🔴",
                    NodeStatus::Connecting => "🟡",
                    NodeStatus::Disconnecting => "🟠",
                };

                let is_local = if node.node_id == self.local_node_id { " (local)" } else { "" };

                output.push_str(&format!(
                    "  {} {} - {} at {}{}\n",
                    status_icon,
                    node.node_id.to_string()[..8].to_uppercase(),
                    node.name,
                    node.listen_addr,
                    is_local
                ));
            }
        }

        output
    }

    /// Format node list for display
    pub async fn format_node_list(&self) -> String {
        let state = self.cluster_state.read().await;

        if state.nodes.is_empty() {
            return "No nodes in cluster".to_string();
        }

        let mut output = String::new();
        output.push_str("Cluster Nodes:\n");
        output.push_str("ID       | Name           | Address           | Status      | Last Seen\n");
        output.push_str("---------|----------------|-------------------|-------------|------------------\n");

        for node in state.nodes.values() {
            let status_str = match node.status {
                NodeStatus::Online => "Online",
                NodeStatus::Offline => "Offline",
                NodeStatus::Connecting => "Connecting",
                NodeStatus::Disconnecting => "Disconnecting",
            };

            let is_local = if node.node_id == self.local_node_id { " (local)" } else { "" };
            let last_seen = node.last_seen.format("%H:%M:%S");

            output.push_str(&format!(
                "{:<8} | {:<14} | {:<17} | {:<11} | {}{}\n",
                node.node_id.to_string()[..8].to_uppercase(),
                node.name,
                node.listen_addr,
                status_str,
                last_seen,
                is_local
            ));
        }

        output
    }
}

/// Cluster statistics
#[derive(Debug, Clone)]
pub struct ClusterStats {
    pub total_nodes: usize,
    pub online_nodes: usize,
    pub offline_nodes: usize,
    pub cluster_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}
