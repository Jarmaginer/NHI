use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Events emitted by the distributed registry
#[derive(Debug, Clone)]
pub enum RegistryEvent {
    /// New instance created on a node
    InstanceCreated(Uuid, NodeId),
    /// Instance status changed
    InstanceStatusChanged(Uuid, InstanceStatus),
    /// Shadow instance created
    ShadowInstanceCreated(Uuid, NodeId),
    /// Instance migrated between nodes
    InstanceMigrated(Uuid, NodeId, NodeId), // instance_id, from_node, to_node
    /// Instance removed
    InstanceRemoved(Uuid),
}

/// Distributed instance registry that maintains consistency across all nodes
pub struct DistributedInstanceRegistry {
    local_node_id: NodeId,
    instances: Arc<RwLock<HashMap<Uuid, InstanceInfo>>>,
    event_sender: mpsc::UnboundedSender<RegistryEvent>,
    event_receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<RegistryEvent>>>,
}

impl DistributedInstanceRegistry {
    pub fn new(local_node_id: NodeId) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            local_node_id,
            instances: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
        }
    }

    /// Register a new instance in the distributed registry
    pub async fn register_instance(&self, instance: &Instance, node_id: NodeId) -> Result<()> {
        let instance_info = InstanceInfo {
            id: instance.id,
            program: instance.program.clone(),
            args: instance.args.clone(),
            status: instance.status.clone(),
            node_id,
            created_at: instance.created_at,
            source_node_id: instance.source_node_id,
        };

        {
            let mut instances = self.instances.write().await;
            instances.insert(instance.id, instance_info);
        }

        info!("Registered instance {} on node {}", instance.short_id(), node_id);
        let _ = self.event_sender.send(RegistryEvent::InstanceCreated(instance.id, node_id));

        Ok(())
    }

    /// Create shadow instances on all other nodes when a new instance is started
    pub async fn create_shadow_instances(&self, instance: &Instance, source_node_id: NodeId) -> Result<Vec<InstanceInfo>> {
        let mut shadow_instances = Vec::new();

        // Get all nodes except the source node
        let instances = self.instances.read().await;
        let all_nodes: std::collections::HashSet<NodeId> = instances
            .values()
            .map(|info| info.node_id)
            .collect();

        for node_id in all_nodes {
            if node_id != source_node_id {
                let shadow_info = InstanceInfo {
                    id: instance.id,
                    program: instance.program.clone(),
                    args: instance.args.clone(),
                    status: InstanceStatus::Shadow,
                    node_id,
                    created_at: instance.created_at,
                    source_node_id: Some(source_node_id),
                };

                shadow_instances.push(shadow_info.clone());
                info!("Created shadow instance {} on node {}", instance.short_id(), node_id);
                let _ = self.event_sender.send(RegistryEvent::ShadowInstanceCreated(instance.id, node_id));
            }
        }

        Ok(shadow_instances)
    }

    /// Update instance status in the registry
    pub async fn update_instance_status(&self, instance_id: Uuid, new_status: InstanceStatus) -> Result<()> {
        let updated = {
            let mut instances = self.instances.write().await;
            if let Some(instance_info) = instances.get_mut(&instance_id) {
                let old_status = instance_info.status.clone();
                instance_info.status = new_status.clone();
                old_status != new_status
            } else {
                false
            }
        };

        if updated {
            debug!("Instance {} status changed to {:?}", instance_id, new_status);
            let _ = self.event_sender.send(RegistryEvent::InstanceStatusChanged(instance_id, new_status));
        }

        Ok(())
    }

    /// Migrate instance from one node to another
    pub async fn migrate_instance(&self, instance_id: Uuid, from_node: NodeId, to_node: NodeId) -> Result<()> {
        {
            let mut instances = self.instances.write().await;

            // Update the running instance to be on the target node
            if let Some(instance_info) = instances.get_mut(&instance_id) {
                if instance_info.node_id == from_node && instance_info.status == InstanceStatus::Running {
                    instance_info.node_id = to_node;
                    instance_info.status = InstanceStatus::Running;
                    instance_info.source_node_id = None;
                } else {
                    anyhow::bail!("Instance {} is not running on node {}", instance_id, from_node);
                }
            } else {
                anyhow::bail!("Instance {} not found in registry", instance_id);
            }

            // Update all other instances to be shadows pointing to the new running node
            for (_, instance_info) in instances.iter_mut() {
                if instance_info.id == instance_id && instance_info.node_id != to_node {
                    instance_info.status = InstanceStatus::Shadow;
                    instance_info.source_node_id = Some(to_node);
                }
            }
        }

        info!("Migrated instance {} from node {} to node {}", instance_id, from_node, to_node);
        let _ = self.event_sender.send(RegistryEvent::InstanceMigrated(instance_id, from_node, to_node));

        Ok(())
    }

    /// Get all instances in the registry
    pub async fn get_all_instances(&self) -> HashMap<Uuid, InstanceInfo> {
        let instances = self.instances.read().await;
        instances.clone()
    }

    /// Get instances for a specific node
    pub async fn get_instances_for_node(&self, node_id: NodeId) -> Vec<InstanceInfo> {
        let instances = self.instances.read().await;
        instances
            .values()
            .filter(|info| info.node_id == node_id)
            .cloned()
            .collect()
    }

    /// Get running instances (not shadows)
    pub async fn get_running_instances(&self) -> Vec<InstanceInfo> {
        let instances = self.instances.read().await;
        instances
            .values()
            .filter(|info| info.status == InstanceStatus::Running)
            .cloned()
            .collect()
    }

    /// Get shadow instances for a specific source node
    pub async fn get_shadow_instances(&self, source_node_id: NodeId) -> Vec<InstanceInfo> {
        let instances = self.instances.read().await;
        instances
            .values()
            .filter(|info| info.status == InstanceStatus::Shadow && info.source_node_id == Some(source_node_id))
            .cloned()
            .collect()
    }

    /// Synchronize registry with remote registry data
    pub async fn synchronize_registry(&self, remote_instances: Vec<InstanceInfo>) -> Result<()> {
        let mut conflicts = Vec::new();

        {
            let mut local_instances = self.instances.write().await;

            for remote_instance in remote_instances {
                match local_instances.get(&remote_instance.id) {
                    Some(local_instance) => {
                        // Check for conflicts and resolve them
                        if local_instance.status != remote_instance.status ||
                           local_instance.node_id != remote_instance.node_id {
                            // Use the most recent timestamp to resolve conflicts
                            if remote_instance.created_at > local_instance.created_at {
                                local_instances.insert(remote_instance.id, remote_instance.clone());
                                debug!("Updated instance {} from remote registry", remote_instance.id);
                            } else {
                                conflicts.push(remote_instance.id);
                            }
                        }
                    }
                    None => {
                        // New instance from remote node
                        local_instances.insert(remote_instance.id, remote_instance.clone());
                        debug!("Added new instance {} from remote registry", remote_instance.id);
                    }
                }
            }
        }

        if !conflicts.is_empty() {
            warn!("Registry synchronization conflicts detected for {} instances", conflicts.len());
        }

        Ok(())
    }

    /// Remove instance from registry
    pub async fn remove_instance(&self, instance_id: Uuid) -> Result<()> {
        {
            let mut instances = self.instances.write().await;
            instances.remove(&instance_id);
        }

        info!("Removed instance {} from registry", instance_id);
        let _ = self.event_sender.send(RegistryEvent::InstanceRemoved(instance_id));

        Ok(())
    }

    /// Get event receiver for listening to registry events
    pub fn get_event_receiver(&self) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<RegistryEvent>>> {
        self.event_receiver.clone()
    }
}
