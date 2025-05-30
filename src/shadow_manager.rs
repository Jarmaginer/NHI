use crate::message_protocol::*;
use crate::types::Instance;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Shadow state data for an instance
#[derive(Debug, Clone)]
pub struct ShadowState {
    pub instance_id: Uuid,
    pub data_version: u64,
    pub checkpoint_data: Option<Vec<u8>>,
    pub output_buffer: Vec<u8>,
    pub last_sync_time: DateTime<Utc>,
    pub source_node_id: NodeId,
}

/// Events emitted by the shadow manager
#[derive(Debug, Clone)]
pub enum ShadowEvent {
    /// Shadow state updated
    StateUpdated(Uuid, u64),
    /// Output data received
    OutputReceived(Uuid, Vec<u8>),
    /// Checkpoint data received
    CheckpointReceived(Uuid, Vec<u8>),
    /// Shadow synchronization failed
    SyncFailed(Uuid, String),
}

/// Manager for shadow state synchronization and real-time data streaming
pub struct ShadowStateManager {
    local_node_id: NodeId,
    shadow_states: Arc<RwLock<HashMap<Uuid, ShadowState>>>,
    event_sender: mpsc::UnboundedSender<ShadowEvent>,
    event_receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ShadowEvent>>>,
    network_sender: Option<mpsc::UnboundedSender<NetworkMessage>>,
}

impl ShadowStateManager {
    pub fn new(local_node_id: NodeId) -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        Self {
            local_node_id,
            shadow_states: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
            network_sender: None,
        }
    }

    /// Set the network sender for broadcasting shadow data
    pub fn set_network_sender(&mut self, sender: mpsc::UnboundedSender<NetworkMessage>) {
        self.network_sender = Some(sender);
    }

    /// Create a new shadow state for an instance
    pub async fn create_shadow_state(&self, instance: &Instance, source_node_id: NodeId) -> Result<()> {
        let shadow_state = ShadowState {
            instance_id: instance.id,
            data_version: 0,
            checkpoint_data: None,
            output_buffer: Vec::new(),
            last_sync_time: Utc::now(),
            source_node_id,
        };

        {
            let mut states = self.shadow_states.write().await;
            states.insert(instance.id, shadow_state);
        }

        info!("Created shadow state for instance {} from node {}", instance.short_id(), source_node_id);
        Ok(())
    }

    /// Update shadow state with new data from the running instance
    pub async fn update_shadow_state(&self, sync_message: ShadowSyncMessage) -> Result<()> {
        let updated = {
            let mut states = self.shadow_states.write().await;

            if let Some(shadow_state) = states.get_mut(&sync_message.instance_id) {
                // Only update if the version is newer
                if sync_message.data_version > shadow_state.data_version {
                    shadow_state.data_version = sync_message.data_version;
                    shadow_state.last_sync_time = Utc::now();

                    if let Some(checkpoint_data) = sync_message.checkpoint_data {
                        shadow_state.checkpoint_data = Some(checkpoint_data.clone());
                        let _ = self.event_sender.send(ShadowEvent::CheckpointReceived(
                            sync_message.instance_id,
                            checkpoint_data
                        ));
                    }

                    if let Some(output_data) = sync_message.output_data {
                        shadow_state.output_buffer.extend_from_slice(&output_data);
                        let _ = self.event_sender.send(ShadowEvent::OutputReceived(
                            sync_message.instance_id,
                            output_data
                        ));
                    }

                    true
                } else {
                    debug!("Ignoring older shadow sync data for instance {}", sync_message.instance_id);
                    false
                }
            } else {
                warn!("Received shadow sync for unknown instance {}", sync_message.instance_id);
                false
            }
        };

        if updated {
            let _ = self.event_sender.send(ShadowEvent::StateUpdated(
                sync_message.instance_id,
                sync_message.data_version
            ));
        }

        Ok(())
    }

    /// Stream real-time data from a running instance to all shadow instances
    pub async fn stream_data(&self, instance_id: Uuid, stream_type: StreamType, data: Vec<u8>) -> Result<()> {
        if let Some(network_sender) = &self.network_sender {
            let sequence_number = self.get_next_sequence_number(instance_id).await;

            let stream_message = DataStreamMessage {
                sender_id: self.local_node_id,
                instance_id,
                stream_type,
                data,
                sequence_number,
                timestamp: Utc::now(),
            };

            let network_message = NetworkMessage::DataStream(stream_message);

            if let Err(e) = network_sender.send(network_message) {
                error!("Failed to send data stream message: {}", e);
                return Err(anyhow::anyhow!("Failed to send data stream: {}", e));
            }
        }

        Ok(())
    }

    /// Handle incoming data stream from a running instance
    pub async fn handle_data_stream(&self, stream_message: DataStreamMessage) -> Result<()> {
        match stream_message.stream_type {
            StreamType::Stdout | StreamType::Stderr => {
                // Update shadow state with output data
                let mut states = self.shadow_states.write().await;
                if let Some(shadow_state) = states.get_mut(&stream_message.instance_id) {
                    shadow_state.output_buffer.extend_from_slice(&stream_message.data);
                    shadow_state.last_sync_time = Utc::now();

                    let _ = self.event_sender.send(ShadowEvent::OutputReceived(
                        stream_message.instance_id,
                        stream_message.data
                    ));
                }
            }
            StreamType::Checkpoint => {
                // Update shadow state with checkpoint data
                let mut states = self.shadow_states.write().await;
                if let Some(shadow_state) = states.get_mut(&stream_message.instance_id) {
                    shadow_state.checkpoint_data = Some(stream_message.data.clone());
                    shadow_state.last_sync_time = Utc::now();

                    let _ = self.event_sender.send(ShadowEvent::CheckpointReceived(
                        stream_message.instance_id,
                        stream_message.data
                    ));
                }
            }
            StreamType::Stdin => {
                // Forward input to the running instance
                // This would be handled by the migration manager
                debug!("Received stdin data for instance {}", stream_message.instance_id);
            }
            StreamType::Memory => {
                // Handle memory data updates
                debug!("Received memory data for instance {}", stream_message.instance_id);
            }
        }

        Ok(())
    }

    /// Get shadow state for an instance
    pub async fn get_shadow_state(&self, instance_id: Uuid) -> Option<ShadowState> {
        let states = self.shadow_states.read().await;
        states.get(&instance_id).cloned()
    }

    /// Get all shadow states
    pub async fn get_all_shadow_states(&self) -> HashMap<Uuid, ShadowState> {
        let states = self.shadow_states.read().await;
        states.clone()
    }

    /// Remove shadow state for an instance
    pub async fn remove_shadow_state(&self, instance_id: Uuid) -> Result<()> {
        {
            let mut states = self.shadow_states.write().await;
            states.remove(&instance_id);
        }

        info!("Removed shadow state for instance {}", instance_id);
        Ok(())
    }

    /// Start periodic synchronization of shadow states
    pub async fn start_periodic_sync(&self) -> Result<()> {
        let states = self.shadow_states.clone();
        let network_sender = self.network_sender.clone();
        let local_node_id = self.local_node_id;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5)); // Sync every 5 seconds

            loop {
                interval.tick().await;

                if let Some(sender) = &network_sender {
                    let states_guard = states.read().await;

                    for (instance_id, shadow_state) in states_guard.iter() {
                        // Only sync if we have data to sync
                        if shadow_state.checkpoint_data.is_some() || !shadow_state.output_buffer.is_empty() {
                            let sync_message = ShadowSyncMessage {
                                sender_id: local_node_id,
                                instance_id: *instance_id,
                                data_version: shadow_state.data_version,
                                checkpoint_data: shadow_state.checkpoint_data.clone(),
                                output_data: if shadow_state.output_buffer.is_empty() {
                                    None
                                } else {
                                    Some(shadow_state.output_buffer.clone())
                                },
                                timestamp: Utc::now(),
                            };

                            let network_message = NetworkMessage::ShadowSync(sync_message);

                            if let Err(e) = sender.send(network_message) {
                                error!("Failed to send periodic shadow sync: {}", e);
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Get the next sequence number for data streaming
    async fn get_next_sequence_number(&self, instance_id: Uuid) -> u64 {
        let states = self.shadow_states.read().await;
        if let Some(shadow_state) = states.get(&instance_id) {
            shadow_state.data_version + 1
        } else {
            1
        }
    }

    /// Get event receiver for listening to shadow events
    pub fn get_event_receiver(&self) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ShadowEvent>>> {
        self.event_receiver.clone()
    }
}
