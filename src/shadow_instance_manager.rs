use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use crate::instance::InstanceManager;
use anyhow::{Result, Context};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Manages shadow instances across the cluster
pub struct ShadowInstanceManager {
    local_node_id: NodeId,
    instance_manager: Arc<tokio::sync::Mutex<InstanceManager>>,
    shadow_registry: Arc<RwLock<HashMap<Uuid, ShadowInstanceInfo>>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
}

/// Information about a shadow instance
#[derive(Debug, Clone)]
pub struct ShadowInstanceInfo {
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub created_at: DateTime<Utc>,
    pub last_sync_time: DateTime<Utc>,
    pub output_buffer: Vec<u8>,
    pub latest_checkpoint: Option<Vec<u8>>,
    pub data_version: u64,
}

impl ShadowInstanceManager {
    pub fn new(local_node_id: NodeId, instance_manager: Arc<tokio::sync::Mutex<InstanceManager>>) -> Self {
        Self {
            local_node_id,
            instance_manager,
            shadow_registry: Arc::new(RwLock::new(HashMap::new())),
            network_sender: None,
        }
    }

    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Broadcast instance creation to all other nodes (they will create shadow instances)
    pub async fn broadcast_instance_creation(&self, instance: &Instance) -> Result<()> {
        if instance.status != InstanceStatus::Running {
            return Ok(()); // Only create shadows for running instances
        }

        // Broadcast instance creation to all nodes
        if let Some(network_sender) = &self.network_sender {
            let instance_info = InstanceInfo {
                id: instance.id,
                program: instance.program.clone(),
                args: instance.args.clone(),
                status: instance.status.clone(),
                node_id: self.local_node_id,
                created_at: instance.created_at,
                source_node_id: None,
            };

            let sync_message = InstanceSyncMessage {
                sender_id: self.local_node_id,
                instances: vec![instance_info],
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::InstanceSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to broadcast instance creation: {}", e);
            } else {
                info!("Broadcasted instance creation for {}", instance.short_id());
            }
        }

        Ok(())
    }

    /// Handle incoming instance sync message (create shadow instances)
    pub async fn handle_instance_sync(&self, sync_message: InstanceSyncMessage) -> Result<()> {
        if sync_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        for instance_info in sync_message.instances {
            if instance_info.status == InstanceStatus::Running {
                self.create_local_shadow_instance(&instance_info, sync_message.sender_id).await?;
            }
        }

        Ok(())
    }

    /// Create a local shadow instance from remote instance info
    async fn create_local_shadow_instance(&self, instance_info: &InstanceInfo, source_node_id: NodeId) -> Result<()> {
        // Check if we already have this instance (running or shadow)
        {
            let instance_manager = self.instance_manager.lock().await;
            if instance_manager.has_instance(&instance_info.id.to_string()) {
                debug!("Instance {} already exists locally, skipping shadow creation", instance_info.id);
                return Ok(());
            }
        }

        // Create shadow instance in local instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;

            let mut shadow_instance = Instance::new(
                instance_info.program.clone(),
                instance_info.args.clone(),
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")),
            );

            // Override the UUID to match the source instance
            shadow_instance.id = instance_info.id;
            shadow_instance.status = InstanceStatus::Shadow;
            shadow_instance.source_node_id = Some(source_node_id);
            shadow_instance.created_at = instance_info.created_at;
            shadow_instance.pid = None; // Shadow instances don't have actual processes

            // Save shadow instance metadata
            if let Err(e) = shadow_instance.save_metadata() {
                warn!("Failed to save shadow instance metadata: {}", e);
            }

            // Add to instance manager
            instance_manager.add_instance(shadow_instance);
        }

        // Add to shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            let shadow_info = ShadowInstanceInfo {
                instance_id: instance_info.id,
                source_node_id,
                created_at: Utc::now(),
                last_sync_time: Utc::now(),
                output_buffer: Vec::new(),
                latest_checkpoint: None,
                data_version: 0,
            };
            registry.insert(instance_info.id, shadow_info);
        }

        info!("Created shadow instance {} from source node {}", instance_info.id, source_node_id);
        Ok(())
    }

    /// Handle incoming shadow data synchronization
    pub async fn handle_shadow_sync(&self, sync_message: ShadowSyncMessage) -> Result<()> {
        if sync_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        let mut registry = self.shadow_registry.write().await;

        if let Some(shadow_info) = registry.get_mut(&sync_message.instance_id) {
            // Only update if the version is newer
            if sync_message.data_version > shadow_info.data_version {
                shadow_info.data_version = sync_message.data_version;
                shadow_info.last_sync_time = Utc::now();

                if let Some(checkpoint_data) = sync_message.checkpoint_data {
                    shadow_info.latest_checkpoint = Some(checkpoint_data);
                    debug!("Updated checkpoint data for shadow instance {}", sync_message.instance_id);
                }

                if let Some(output_data) = sync_message.output_data {
                    shadow_info.output_buffer.extend_from_slice(&output_data);
                    debug!("Updated output buffer for shadow instance {}", sync_message.instance_id);
                }
            }
        } else {
            debug!("Received shadow sync for unknown instance {}", sync_message.instance_id);
        }

        Ok(())
    }

    /// Handle instance stop notification
    pub async fn handle_instance_stop(&self, stop_message: InstanceStopMessage) -> Result<()> {
        if stop_message.sender_id == self.local_node_id {
            return Ok(()); // Ignore our own messages
        }

        info!("Received instance stop notification for instance {} from node {}",
              stop_message.instance_id, stop_message.sender_id);

        // Remove the shadow instance if it exists
        if let Err(e) = self.remove_shadow_instance(stop_message.instance_id).await {
            warn!("Failed to remove shadow instance {}: {}", stop_message.instance_id, e);
        } else {
            info!("Successfully removed shadow instance {} after source stop", stop_message.instance_id);
        }

        Ok(())
    }

    /// Stream output data from a running instance to all shadow instances
    pub async fn stream_output_to_shadows(&self, instance_id: Uuid, output_data: Vec<u8>, stream_type: StreamType) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let data_version = self.get_next_data_version(instance_id).await;
            debug!("Streaming output to shadows: {} bytes, version {} for instance {}",
                   output_data.len(), data_version, instance_id);

            let sync_message = ShadowSyncMessage {
                sender_id: self.local_node_id,
                instance_id,
                data_version,
                checkpoint_data: None,
                output_data: Some(output_data.clone()),
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::ShadowSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to stream output to shadows: {}", e);
            } else {
                debug!("Successfully sent shadow sync message for instance {}", instance_id);
            }
        } else {
            debug!("No network sender available for streaming output to shadows");
        }

        Ok(())
    }

    /// Stream checkpoint data from a running instance to all shadow instances
    pub async fn stream_checkpoint_to_shadows(&self, instance_id: Uuid, checkpoint_data: Vec<u8>) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let sync_message = ShadowSyncMessage {
                sender_id: self.local_node_id,
                instance_id,
                data_version: self.get_next_data_version(instance_id).await,
                checkpoint_data: Some(checkpoint_data),
                output_data: None,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::ShadowSync(sync_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to stream checkpoint to shadows: {}", e);
            } else {
                info!("Streamed checkpoint data for instance {} to shadow instances", instance_id);
            }
        }

        Ok(())
    }

    /// Get all shadow instances managed by this node
    pub async fn get_shadow_instances(&self) -> Vec<ShadowInstanceInfo> {
        let registry = self.shadow_registry.read().await;
        registry.values().cloned().collect()
    }

    /// Get shadow instance info by ID
    pub async fn get_shadow_instance(&self, instance_id: Uuid) -> Option<ShadowInstanceInfo> {
        let registry = self.shadow_registry.read().await;
        registry.get(&instance_id).cloned()
    }

    /// Forward input from shadow instance to source instance
    pub async fn forward_input_to_source(&self, shadow_instance_id: Uuid, input: String) -> Result<()> {
        let registry = self.shadow_registry.read().await;
        if let Some(shadow_info) = registry.get(&shadow_instance_id) {
            if let Some(network_sender) = &self.network_sender {
                let input_message = ShadowInputMessage {
                    sender_id: self.local_node_id,
                    target_node_id: shadow_info.source_node_id,
                    instance_id: shadow_instance_id,
                    input_data: input,
                    timestamp: Utc::now(),
                };

                let network_message = NetworkMessage::ShadowInput(input_message);

                if let Err(e) = network_sender.send(network_message) {
                    error!("Failed to forward input to source: {}", e);
                } else {
                    debug!("Forwarded input to source node {} for instance {}",
                           shadow_info.source_node_id, shadow_instance_id);
                }
            }
        } else {
            return Err(anyhow::anyhow!("Shadow instance not found: {}", shadow_instance_id));
        }

        Ok(())
    }

    /// Broadcast instance stop to all shadow instances
    pub async fn broadcast_instance_stop(&self, instance_id: Uuid) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let stop_message = InstanceStopMessage {
                sender_id: self.local_node_id,
                instance_id,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::InstanceStop(stop_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to broadcast instance stop: {}", e);
            } else {
                info!("Broadcasted instance stop for {}", instance_id);
            }
        }

        Ok(())
    }

    /// Remove shadow instance when the source instance is stopped
    pub async fn remove_shadow_instance(&self, instance_id: Uuid) -> Result<()> {
        // Remove from shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            registry.remove(&instance_id);
        }

        // Remove from instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            instance_manager.remove_instance(&instance_id.to_string());
        }

        info!("Removed shadow instance {}", instance_id);
        Ok(())
    }

    /// Promote shadow instance to running instance (for migration)
    pub async fn promote_shadow_to_running(&self, instance_id: Uuid, new_pid: u32) -> Result<()> {
        // Update in instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            if let Some(instance) = instance_manager.get_instance_by_id_mut(&instance_id.to_string()) {
                instance.status = InstanceStatus::Running;
                instance.pid = Some(new_pid);
                instance.source_node_id = None; // No longer a shadow

                // Save updated metadata
                if let Err(e) = instance.save_metadata() {
                    warn!("Failed to save instance metadata after promotion: {}", e);
                }
            }
        }

        // Remove from shadow registry (it's now a running instance)
        {
            let mut registry = self.shadow_registry.write().await;
            registry.remove(&instance_id);
        }

        info!("Promoted shadow instance {} to running with PID {}", instance_id, new_pid);
        Ok(())
    }

    /// Demote running instance to shadow instance (for migration)
    pub async fn demote_running_to_shadow(&self, instance_id: Uuid, new_source_node_id: NodeId) -> Result<()> {
        // Update in instance manager
        {
            let mut instance_manager = self.instance_manager.lock().await;
            if let Some(instance) = instance_manager.get_instance_by_id_mut(&instance_id.to_string()) {
                instance.status = InstanceStatus::Shadow;
                instance.pid = None; // Shadow instances don't have processes
                instance.source_node_id = Some(new_source_node_id);

                // Save updated metadata
                if let Err(e) = instance.save_metadata() {
                    warn!("Failed to save instance metadata after demotion: {}", e);
                }
            }
        }

        // Add to shadow registry
        {
            let mut registry = self.shadow_registry.write().await;
            let shadow_info = ShadowInstanceInfo {
                instance_id,
                source_node_id: new_source_node_id,
                created_at: Utc::now(),
                last_sync_time: Utc::now(),
                output_buffer: Vec::new(),
                latest_checkpoint: None,
                data_version: 0,
            };
            registry.insert(instance_id, shadow_info);
        }

        info!("Demoted running instance {} to shadow for source node {}", instance_id, new_source_node_id);
        Ok(())
    }

    // Private helper methods

    async fn get_next_data_version(&self, instance_id: Uuid) -> u64 {
        // For source nodes, we need to track data versions separately
        // since they don't have shadow registry entries for their own instances
        static DATA_VERSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        DATA_VERSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }
}
