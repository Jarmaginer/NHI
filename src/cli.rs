use crate::types::{CriuCliError, Result};

#[derive(Debug, Clone)]
pub enum CliCommand {
    Help,
    Exit,
    Start {
        program: String,
        args: Vec<String>,
    },
    StartDetached {
        program: String,
        args: Vec<String>,
    },
    Stop {
        instance_id: String,
    },
    Pause {
        instance_id: String,
    },
    Resume {
        instance_id: String,
    },
    List,
    Attach {
        instance_id: String,
    },
    Detach,
    Logs {
        instance_id: Option<String>,
        lines: Option<usize>,
    },
    Checkpoint {
        instance_id: String,
        name: String,
    },
    Restore {
        checkpoint_name: String,
    },
    Cd {
        directory: String,
    },
    AnalyzeTty {
        instance_id: String,
    },
}

impl CliCommand {
    pub fn parse_from_str(input: &str) -> Result<Self> {
        let parts: Vec<&str> = input.trim().split_whitespace().collect();

        if parts.is_empty() {
            return Err(CriuCliError::ParseError("Empty command".to_string()));
        }

        match parts[0] {
            "help" | "h" => Ok(CliCommand::Help),
            "exit" | "quit" | "q" => Ok(CliCommand::Exit),
            "start" => {
                if parts.len() < 2 {
                    return Err(CriuCliError::ParseError(
                        "start command requires a program name".to_string(),
                    ));
                }
                let program = parts[1].to_string();
                let args = parts[2..].iter().map(|s| s.to_string()).collect();
                Ok(CliCommand::Start { program, args })
            }
            "start-detached" | "startd" => {
                if parts.len() < 2 {
                    return Err(CriuCliError::ParseError(
                        "start-detached command requires a program name".to_string(),
                    ));
                }
                let program = parts[1].to_string();
                let args = parts[2..].iter().map(|s| s.to_string()).collect();
                Ok(CliCommand::StartDetached { program, args })
            }
            "stop" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "stop command requires an instance ID".to_string(),
                    ));
                }
                Ok(CliCommand::Stop {
                    instance_id: parts[1].to_string(),
                })
            }
            "pause" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "pause command requires an instance ID".to_string(),
                    ));
                }
                Ok(CliCommand::Pause {
                    instance_id: parts[1].to_string(),
                })
            }
            "resume" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "resume command requires an instance ID".to_string(),
                    ));
                }
                Ok(CliCommand::Resume {
                    instance_id: parts[1].to_string(),
                })
            }
            "list" | "ls" => Ok(CliCommand::List),
            "attach" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "attach command requires an instance ID".to_string(),
                    ));
                }
                Ok(CliCommand::Attach {
                    instance_id: parts[1].to_string(),
                })
            }
            "detach" => Ok(CliCommand::Detach),
            "logs" => {
                let instance_id = if parts.len() > 1 {
                    Some(parts[1].to_string())
                } else {
                    None
                };
                let lines = if parts.len() > 2 {
                    parts[2].parse().ok()
                } else {
                    Some(20) // Default to 20 lines
                };
                Ok(CliCommand::Logs { instance_id, lines })
            }
            "checkpoint" | "cp" => {
                if parts.len() != 3 {
                    return Err(CriuCliError::ParseError(
                        "checkpoint command requires instance ID and checkpoint name".to_string(),
                    ));
                }
                Ok(CliCommand::Checkpoint {
                    instance_id: parts[1].to_string(),
                    name: parts[2].to_string(),
                })
            }
            "restore" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "restore command requires a checkpoint name".to_string(),
                    ));
                }
                Ok(CliCommand::Restore {
                    checkpoint_name: parts[1].to_string(),
                })
            }
            "cd" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "cd command requires a directory path".to_string(),
                    ));
                }
                Ok(CliCommand::Cd {
                    directory: parts[1].to_string(),
                })
            }
            "analyze-tty" | "tty" => {
                if parts.len() != 2 {
                    return Err(CriuCliError::ParseError(
                        "analyze-tty command requires an instance ID".to_string(),
                    ));
                }
                Ok(CliCommand::AnalyzeTty {
                    instance_id: parts[1].to_string(),
                })
            }
            _ => Err(CriuCliError::ParseError(format!(
                "Unknown command: {}. Type 'help' for available commands.",
                parts[0]
            ))),
        }
    }
}

#[derive(Debug)]
pub struct CliState {
    pub attached_instance: Option<String>,
    pub output_task: Option<tokio::task::JoinHandle<()>>,
}

impl CliState {
    pub fn new() -> Self {
        Self {
            attached_instance: None,
            output_task: None,
        }
    }
}
