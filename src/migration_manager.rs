use crate::message_protocol::*;
use crate::types::{Instance, InstanceStatus};
use crate::distributed_registry::DistributedInstanceRegistry;
use crate::shadow_manager::ShadowStateManager;
use crate::streaming_manager::StreamingManager;
use crate::criu_manager::CriuManager;
use crate::process_manager::ProcessManager;
use crate::instance::InstanceManager;
use anyhow::{Result, Context};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, Mutex};
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Migration operation state
#[derive(Debug, Clone)]
pub struct MigrationOperation {
    pub migration_id: Uuid,
    pub instance_id: Uuid,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    pub status: MigrationStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

/// Status of a migration operation
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationStatus {
    Requested,
    Accepted,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// Events emitted by the migration manager
#[derive(Debug, Clone)]
pub enum MigrationEvent {
    /// Migration requested
    MigrationRequested(Uuid, NodeId, NodeId),
    /// Migration accepted
    MigrationAccepted(Uuid),
    /// Migration started
    MigrationStarted(Uuid),
    /// Migration completed successfully
    MigrationCompleted(Uuid),
    /// Migration failed
    MigrationFailed(Uuid, String),
}

/// Manager for coordinating instance migrations between nodes
pub struct MigrationManager {
    local_node_id: NodeId,
    active_migrations: Arc<RwLock<HashMap<Uuid, MigrationOperation>>>,
    registry: Arc<DistributedInstanceRegistry>,
    shadow_manager: Arc<ShadowStateManager>,
    streaming_manager: Arc<StreamingManager>,
    criu_manager: Arc<CriuManager>,
    process_manager: Arc<ProcessManager>,
    instance_manager: Arc<RwLock<InstanceManager>>,
    event_sender: mpsc::UnboundedSender<MigrationEvent>,
    event_receiver: Arc<Mutex<mpsc::UnboundedReceiver<MigrationEvent>>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
}

impl MigrationManager {
    pub fn new(
        local_node_id: NodeId,
        registry: Arc<DistributedInstanceRegistry>,
        shadow_manager: Arc<ShadowStateManager>,
        streaming_manager: Arc<StreamingManager>,
        criu_manager: Arc<CriuManager>,
        process_manager: Arc<ProcessManager>,
        instance_manager: Arc<RwLock<InstanceManager>>,
    ) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            local_node_id,
            active_migrations: Arc::new(RwLock::new(HashMap::new())),
            registry,
            shadow_manager,
            streaming_manager,
            criu_manager,
            process_manager,
            instance_manager,
            event_sender,
            event_receiver: Arc::new(Mutex::new(event_receiver)),
            network_sender: None,
        }
    }

    /// Set the network sender for migration coordination
    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Request migration of an instance to a target node
    pub async fn request_migration(&self, instance_id: Uuid, target_node_id: NodeId) -> Result<Uuid> {
        let migration_id = Uuid::new_v4();

        // Check if instance exists and is running on this node
        let instances = self.registry.get_instances_for_node(self.local_node_id).await;
        let instance = instances.iter()
            .find(|i| i.id == instance_id && i.status == InstanceStatus::Running)
            .ok_or_else(|| anyhow::anyhow!("Instance {} not found or not running on this node", instance_id))?;

        // Create migration operation
        let migration_op = MigrationOperation {
            migration_id,
            instance_id,
            source_node_id: self.local_node_id,
            target_node_id,
            status: MigrationStatus::Requested,
            started_at: Utc::now(),
            completed_at: None,
            error_message: None,
        };

        {
            let mut migrations = self.active_migrations.write().await;
            migrations.insert(migration_id, migration_op);
        }

        // Send migration request to target node
        if let Some(network_sender) = &self.network_sender {
            let migration_message = MigrationMessage {
                sender_id: self.local_node_id,
                migration_type: MigrationType::MigrateRequest,
                instance_id,
                target_node_id: Some(target_node_id),
                migration_id,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::Migration(migration_message);
            network_sender.send(network_message)
                .context("Failed to send migration request")?;
        }

        info!("Requested migration of instance {} to node {}", instance_id, target_node_id);
        let _ = self.event_sender.send(MigrationEvent::MigrationRequested(migration_id, self.local_node_id, target_node_id));

        Ok(migration_id)
    }

    /// Handle incoming migration message
    pub async fn handle_migration_message(&self, migration_message: MigrationMessage) -> Result<()> {
        match migration_message.migration_type.clone() {
            MigrationType::MigrateRequest => {
                self.handle_migration_request(migration_message).await
            }
            MigrationType::MigrateAccept => {
                self.handle_migration_accept(migration_message).await
            }
            MigrationType::MigrateReject { reason } => {
                self.handle_migration_reject(migration_message, reason).await
            }
            MigrationType::MigrateStart => {
                self.handle_migration_start(migration_message).await
            }
            MigrationType::MigrateComplete => {
                self.handle_migration_complete(migration_message).await
            }
            MigrationType::MigrateFailed { reason } => {
                self.handle_migration_failed(migration_message, reason).await
            }
        }
    }

    /// Handle migration request from another node
    async fn handle_migration_request(&self, migration_message: MigrationMessage) -> Result<()> {
        // Check if we have a shadow instance for this instance
        let shadow_state = self.shadow_manager.get_shadow_state(migration_message.instance_id).await;

        if shadow_state.is_some() {
            // Accept the migration
            self.send_migration_response(
                migration_message.migration_id,
                migration_message.sender_id,
                MigrationType::MigrateAccept,
            ).await?;

            info!("Accepted migration request for instance {}", migration_message.instance_id);
        } else {
            // Reject the migration
            self.send_migration_response(
                migration_message.migration_id,
                migration_message.sender_id,
                MigrationType::MigrateReject {
                    reason: "No shadow instance found".to_string()
                },
            ).await?;

            warn!("Rejected migration request for instance {} - no shadow found", migration_message.instance_id);
        }

        Ok(())
    }

    /// Handle migration acceptance
    async fn handle_migration_accept(&self, migration_message: MigrationMessage) -> Result<()> {
        // Update migration status and start the migration process
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration_op) = migrations.get_mut(&migration_message.migration_id) {
                migration_op.status = MigrationStatus::Accepted;
            }
        }

        // Start the actual migration process
        self.execute_migration(migration_message.migration_id).await?;

        let _ = self.event_sender.send(MigrationEvent::MigrationAccepted(migration_message.migration_id));
        Ok(())
    }

    /// Handle migration rejection
    async fn handle_migration_reject(&self, migration_message: MigrationMessage, reason: String) -> Result<()> {
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration_op) = migrations.get_mut(&migration_message.migration_id) {
                migration_op.status = MigrationStatus::Failed;
                migration_op.error_message = Some(reason.clone());
                migration_op.completed_at = Some(Utc::now());
            }
        }

        error!("Migration {} rejected: {}", migration_message.migration_id, reason);
        let _ = self.event_sender.send(MigrationEvent::MigrationFailed(migration_message.migration_id, reason));
        Ok(())
    }

    /// Handle migration start notification
    async fn handle_migration_start(&self, migration_message: MigrationMessage) -> Result<()> {
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration_op) = migrations.get_mut(&migration_message.migration_id) {
                migration_op.status = MigrationStatus::InProgress;
            }
        }

        let _ = self.event_sender.send(MigrationEvent::MigrationStarted(migration_message.migration_id));
        Ok(())
    }

    /// Handle migration completion
    async fn handle_migration_complete(&self, migration_message: MigrationMessage) -> Result<()> {
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration_op) = migrations.get_mut(&migration_message.migration_id) {
                migration_op.status = MigrationStatus::Completed;
                migration_op.completed_at = Some(Utc::now());
            }
        }

        // Update registry to reflect the migration
        self.registry.migrate_instance(
            migration_message.instance_id,
            self.local_node_id,
            migration_message.sender_id,
        ).await?;

        let _ = self.event_sender.send(MigrationEvent::MigrationCompleted(migration_message.migration_id));
        Ok(())
    }

    /// Handle migration failure
    async fn handle_migration_failed(&self, migration_message: MigrationMessage, reason: String) -> Result<()> {
        {
            let mut migrations = self.active_migrations.write().await;
            if let Some(migration_op) = migrations.get_mut(&migration_message.migration_id) {
                migration_op.status = MigrationStatus::Failed;
                migration_op.error_message = Some(reason.clone());
                migration_op.completed_at = Some(Utc::now());
            }
        }

        error!("Migration {} failed: {}", migration_message.migration_id, reason);
        let _ = self.event_sender.send(MigrationEvent::MigrationFailed(migration_message.migration_id, reason));
        Ok(())
    }

    /// Execute the actual migration process
    async fn execute_migration(&self, migration_id: Uuid) -> Result<()> {
        let migration_op = {
            let migrations = self.active_migrations.read().await;
            migrations.get(&migration_id).cloned()
                .ok_or_else(|| anyhow::anyhow!("Migration {} not found", migration_id))?
        };

        info!("Starting migration execution for instance {}", migration_op.instance_id);

        // Step 1: Create checkpoint of the running instance
        let checkpoint_name = format!("migration_{}", migration_id);

        // This would integrate with the existing CRIU checkpoint functionality
        // For now, we'll simulate the process

        // Step 2: Send migration start notification
        self.send_migration_response(
            migration_id,
            migration_op.target_node_id,
            MigrationType::MigrateStart,
        ).await?;

        // Step 3: The actual migration would happen here
        // This involves stopping the current process, transferring state, and starting on target

        // Step 4: Send migration complete notification
        self.send_migration_response(
            migration_id,
            migration_op.target_node_id,
            MigrationType::MigrateComplete,
        ).await?;

        Ok(())
    }

    /// Send migration response message
    async fn send_migration_response(
        &self,
        migration_id: Uuid,
        target_node_id: NodeId,
        migration_type: MigrationType,
    ) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let migration_message = MigrationMessage {
                sender_id: self.local_node_id,
                migration_type,
                instance_id: Uuid::nil(), // Will be filled by the specific handler
                target_node_id: Some(target_node_id),
                migration_id,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::Migration(migration_message);
            network_sender.send(network_message)
                .context("Failed to send migration response")?;
        }

        Ok(())
    }

    /// Get all active migrations
    pub async fn get_active_migrations(&self) -> HashMap<Uuid, MigrationOperation> {
        let migrations = self.active_migrations.read().await;
        migrations.clone()
    }

    /// Get event receiver for listening to migration events
    pub fn get_event_receiver(&self) -> Arc<Mutex<mpsc::UnboundedReceiver<MigrationEvent>>> {
        self.event_receiver.clone()
    }
}
