use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StartMode {
    Normal,
    Detached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: Uuid,
    pub program: String,
    pub args: Vec<String>,
    pub status: InstanceStatus,
    pub pid: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub working_dir: PathBuf,
    pub checkpoints: HashMap<String, CheckpointInfo>,
    pub start_mode: StartMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InstanceStatus {
    Starting,
    Running,
    Paused,
    Stopped,
    Failed,
}

impl std::fmt::Display for InstanceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceStatus::Starting => write!(f, "Starting"),
            InstanceStatus::Running => write!(f, "Running"),
            InstanceStatus::Paused => write!(f, "Paused"),
            InstanceStatus::Stopped => write!(f, "Stopped"),
            InstanceStatus::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub checkpoint_dir: PathBuf,
    pub original_instance_id: Uuid,
}

#[derive(Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub child: tokio::process::Child,
    pub output_history: Arc<Mutex<Vec<String>>>,
    pub stdout_handle: Option<tokio::task::JoinHandle<()>>,
    pub stderr_handle: Option<tokio::task::JoinHandle<()>>,
    pub output_sender: Option<tokio::sync::broadcast::Sender<String>>,
    pub stdin_sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl Instance {
    pub fn new(program: String, args: Vec<String>, working_dir: PathBuf) -> Self {
        Self::new_with_mode(program, args, working_dir, StartMode::Normal)
    }

    pub fn new_with_mode(program: String, args: Vec<String>, working_dir: PathBuf, start_mode: StartMode) -> Self {
        Self {
            id: Uuid::new_v4(),
            program,
            args,
            status: InstanceStatus::Starting,
            pid: None,
            created_at: Utc::now(),
            working_dir,
            checkpoints: HashMap::new(),
            start_mode,
        }
    }

    pub fn add_checkpoint(&mut self, name: String, checkpoint_dir: PathBuf) {
        let checkpoint = CheckpointInfo {
            name: name.clone(),
            created_at: Utc::now(),
            checkpoint_dir,
            original_instance_id: self.id,
        };
        self.checkpoints.insert(name, checkpoint);
    }

    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CriuCliError {
    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Instance is not running: {0}")]
    InstanceNotRunning(String),

    #[error("Instance is not paused: {0}")]
    InstanceNotPaused(String),

    #[error("Checkpoint not found: {0}")]
    CheckpointNotFound(String),

    #[error("Process error: {0}")]
    ProcessError(String),

    #[error("CRIU error: {0}")]
    CriuError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(String),
}

pub type Result<T> = std::result::Result<T, CriuCliError>;
