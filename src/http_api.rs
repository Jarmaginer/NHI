use crate::cli::CliCommand;
use crate::instance::InstanceManager;
use crate::process_manager::ProcessManager;
use crate::criu_manager::CriuManager;
use crate::node_manager::NodeManager;
use crate::shadow_instance_manager::ShadowInstanceManager;
use crate::migration_manager::MigrationManager;
use crate::cli::CliState;
use crate::output::Output;
use axum::{
    extract::{State, Query},
    http::{StatusCode, HeaderMap, HeaderValue, Method},
    response::Json,
    routing::{post, get},
    Router,
    body::Bytes,
};
use tower_http::cors::{CorsLayer, Any};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, error, warn};
use chrono;

#[derive(Clone)]
pub struct ApiState {
    pub cli_state: Arc<Mutex<CliState>>,
    pub instance_manager: Arc<Mutex<InstanceManager>>,
    pub process_manager: Arc<ProcessManager>,
    pub criu_manager: Arc<CriuManager>,
    pub node_manager: Option<Arc<NodeManager>>,
    pub shadow_manager: Option<Arc<RwLock<ShadowInstanceManager>>>,
    pub migration_manager: Option<Arc<MigrationManager>>,
}

#[derive(Deserialize)]
pub struct CommandRequest {
    pub command: String,
}

#[derive(Serialize)]
pub struct CommandResponse {
    pub success: bool,
    pub message: String,
    pub output: Option<String>,
}

#[derive(Serialize)]
pub struct LogsResponse {
    pub success: bool,
    pub logs: Vec<String>,
    pub total_lines: usize,
}

#[derive(Serialize)]
pub struct CpuResponse {
    pub success: bool,
    pub cpu_usage_percent: f64,
    pub load_average: Vec<f64>,
}

#[derive(Serialize)]
pub struct MemoryResponse {
    pub success: bool,
    pub memory_usage_percent: f64,
    pub total_memory_mb: u64,
    pub used_memory_mb: u64,
    pub available_memory_mb: u64,
}

#[derive(Serialize)]
pub struct SystemStatusResponse {
    pub success: bool,
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub active_instances: usize,
    pub node_name: String,
    pub api_version: String,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub lines: Option<usize>,
}

pub fn create_router(state: ApiState) -> Router {
    // 配置CORS以允许前端访问
    let cors = CorsLayer::new()
        .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);

    Router::new()
        .route("/api/command", post(execute_command_handler))
        .route("/command", post(execute_text_command_handler))
        .route("/api/logs", get(get_logs_handler))
        .route("/api/cpu", get(get_cpu_usage_handler))
        .route("/api/memory", get(get_memory_usage_handler))
        .route("/api/status", get(get_system_status_handler))
        .layer(cors)
        .with_state(state)
}

pub async fn start_http_server(
    state: ApiState,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = create_router(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    info!("🌐 HTTP API server listening on port {}", port);
    Output::network(&format!("HTTP API available at:"));
    Output::network(&format!("  Commands:"));
    Output::network(&format!("    JSON: http://0.0.0.0:{}/api/command", port));
    Output::network(&format!("    TEXT: http://0.0.0.0:{}/command", port));
    Output::network(&format!("  System Info:"));
    Output::network(&format!("    Logs: http://0.0.0.0:{}/api/logs", port));
    Output::network(&format!("    CPU:  http://0.0.0.0:{}/api/cpu", port));
    Output::network(&format!("    Memory: http://0.0.0.0:{}/api/memory", port));
    Output::network(&format!("    Status: http://0.0.0.0:{}/api/status", port));

    axum::serve(listener, app).await?;

    Ok(())
}

async fn execute_command_handler(
    State(state): State<ApiState>,
    Json(payload): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, StatusCode> {
    let command_text = payload.command.trim();

    // Log the received command prominently
    Output::header(&format!("📡 HTTP API Command Received"));
    Output::info(&format!("Command: {}", command_text));
    info!("HTTP API received command: {}", command_text);

    // Parse the command
    let command = match CliCommand::parse_from_str(command_text) {
        Ok(cmd) => cmd,
        Err(e) => {
            let error_msg = format!("Failed to parse command: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);
            return Ok(Json(CommandResponse {
                success: false,
                message: error_msg,
                output: None,
            }));
        }
    };

    // Execute the command using main.rs execute_command function
    match crate::execute_command(
        command_text,
        &state.cli_state,
        &state.instance_manager,
        &state.process_manager,
        &state.criu_manager,
        &state.node_manager,
        &state.shadow_manager,
        &state.migration_manager,
    ).await {
        Ok(_should_exit) => {
            let success_msg = format!("Command executed successfully: {}", command_text);
            info!("{}", success_msg);
            Output::success(&success_msg);

            Ok(Json(CommandResponse {
                success: true,
                message: "Command executed successfully".to_string(),
                output: Some("Command executed via JSON API".to_string()),
            }))
        }
        Err(e) => {
            let error_msg = format!("Command execution failed: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);

            Ok(Json(CommandResponse {
                success: false,
                message: error_msg,
                output: None,
            }))
        }
    }
}

async fn execute_text_command_handler(
    State(state): State<ApiState>,
    body: Bytes,
) -> Result<Json<CommandResponse>, StatusCode> {
    // Convert bytes to string
    let command_text = match String::from_utf8(body.to_vec()) {
        Ok(text) => text.trim().to_string(),
        Err(_) => {
            let error_msg = "Invalid UTF-8 in request body".to_string();
            error!("{}", error_msg);
            Output::error(&error_msg);
            return Ok(Json(CommandResponse {
                success: false,
                message: error_msg,
                output: None,
            }));
        }
    };

    if command_text.is_empty() {
        let error_msg = "Empty command received".to_string();
        error!("{}", error_msg);
        Output::error(&error_msg);
        return Ok(Json(CommandResponse {
            success: false,
            message: error_msg,
            output: None,
        }));
    }

    // Log the received command prominently
    Output::header("🌐 HTTP TEXT Command Received");
    Output::info(&format!("📝 Command: {}", command_text));
    info!("HTTP TEXT API received command: {}", command_text);

    // Use the same execute_command function from main.rs
    match crate::execute_command(
        &command_text,
        &state.cli_state,
        &state.instance_manager,
        &state.process_manager,
        &state.criu_manager,
        &state.node_manager,
        &state.shadow_manager,
        &state.migration_manager,
    ).await {
        Ok(_should_exit) => {
            let success_msg = format!("✅ Command executed successfully: {}", command_text);
            info!("{}", success_msg);
            Output::success(&success_msg);

            Ok(Json(CommandResponse {
                success: true,
                message: "Command executed successfully".to_string(),
                output: Some("Command executed via HTTP API".to_string()),
            }))
        }
        Err(e) => {
            let error_msg = format!("❌ Command execution failed: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);

            Ok(Json(CommandResponse {
                success: false,
                message: error_msg,
                output: None,
            }))
        }
    }
}

// GET /api/logs - 获取日志
async fn get_logs_handler(
    Query(params): Query<LogsQuery>,
) -> Result<Json<LogsResponse>, StatusCode> {
    let lines_to_read = params.lines.unwrap_or(100);

    Output::info(&format!("📋 HTTP API: Fetching {} lines of logs", lines_to_read));
    info!("HTTP API: Fetching logs, lines: {}", lines_to_read);

    match read_log_files(lines_to_read).await {
        Ok(logs) => {
            let total_lines = logs.len();
            info!("Successfully read {} lines from logs", total_lines);

            Ok(Json(LogsResponse {
                success: true,
                logs,
                total_lines,
            }))
        }
        Err(e) => {
            let error_msg = format!("Failed to read logs: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);

            Ok(Json(LogsResponse {
                success: false,
                logs: vec![error_msg],
                total_lines: 0,
            }))
        }
    }
}

// GET /api/cpu - 获取CPU使用率
async fn get_cpu_usage_handler() -> Result<Json<CpuResponse>, StatusCode> {
    Output::info("💻 HTTP API: Fetching CPU usage");
    info!("HTTP API: Fetching CPU usage");

    match get_cpu_usage().await {
        Ok((cpu_usage, load_avg)) => {
            info!("CPU usage: {:.2}%, Load average: {:?}", cpu_usage, load_avg);

            Ok(Json(CpuResponse {
                success: true,
                cpu_usage_percent: cpu_usage,
                load_average: load_avg,
            }))
        }
        Err(e) => {
            let error_msg = format!("Failed to get CPU usage: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);

            Ok(Json(CpuResponse {
                success: false,
                cpu_usage_percent: 0.0,
                load_average: vec![],
            }))
        }
    }
}

// GET /api/memory - 获取内存使用率
async fn get_memory_usage_handler() -> Result<Json<MemoryResponse>, StatusCode> {
    Output::info("🧠 HTTP API: Fetching memory usage");
    info!("HTTP API: Fetching memory usage");

    match get_memory_usage().await {
        Ok((usage_percent, total_mb, used_mb, available_mb)) => {
            info!("Memory usage: {:.2}%, Total: {}MB, Used: {}MB, Available: {}MB",
                  usage_percent, total_mb, used_mb, available_mb);

            Ok(Json(MemoryResponse {
                success: true,
                memory_usage_percent: usage_percent,
                total_memory_mb: total_mb,
                used_memory_mb: used_mb,
                available_memory_mb: available_mb,
            }))
        }
        Err(e) => {
            let error_msg = format!("Failed to get memory usage: {}", e);
            error!("{}", error_msg);
            Output::error(&error_msg);

            Ok(Json(MemoryResponse {
                success: false,
                memory_usage_percent: 0.0,
                total_memory_mb: 0,
                used_memory_mb: 0,
                available_memory_mb: 0,
            }))
        }
    }
}

// GET /api/status - 获取系统综合状态
async fn get_system_status_handler(
    State(state): State<ApiState>,
) -> Result<Json<SystemStatusResponse>, StatusCode> {
    Output::info("📊 HTTP API: Fetching system status");
    info!("HTTP API: Fetching system status");

    // 获取实例数量
    let active_instances = {
        let manager = state.instance_manager.lock().await;
        manager.get_all_instances().len()
    };

    // 获取节点名称
    let node_name = if let Some(ref node_mgr) = state.node_manager {
        node_mgr.local_node_info().name.clone()
    } else {
        "standalone".to_string()
    };

    // 获取系统信息
    let uptime = get_system_uptime().await.unwrap_or(0);
    let cpu_usage = get_cpu_usage().await.map(|(cpu, _)| cpu).unwrap_or(0.0);
    let memory_usage = get_memory_usage().await.map(|(usage, _, _, _)| usage).unwrap_or(0.0);

    info!("System status - Uptime: {}s, CPU: {:.2}%, Memory: {:.2}%, Instances: {}",
          uptime, cpu_usage, memory_usage, active_instances);

    Ok(Json(SystemStatusResponse {
        success: true,
        uptime_seconds: uptime,
        cpu_usage_percent: cpu_usage,
        memory_usage_percent: memory_usage,
        active_instances,
        node_name,
        api_version: "1.0.0".to_string(),
    }))
}

// 辅助函数：读取日志文件
async fn read_log_files(lines: usize) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    use tokio::fs;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let log_dir = crate::logger::default_log_dir();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let log_file = log_dir.join(format!("nhi.log.{}", today));

    if !log_file.exists() {
        return Ok(vec!["No log file found for today".to_string()]);
    }

    let file = fs::File::open(&log_file).await?;
    let reader = BufReader::new(file);
    let mut all_lines = Vec::new();
    let mut lines_reader = reader.lines();

    while let Some(line) = lines_reader.next_line().await? {
        all_lines.push(line);
    }

    // 返回最后N行
    let start_index = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };

    Ok(all_lines[start_index..].to_vec())
}

// 辅助函数：获取CPU使用率
async fn get_cpu_usage() -> Result<(f64, Vec<f64>), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::fs;

    // 读取 /proc/stat 获取CPU使用率
    let stat_content = fs::read_to_string("/proc/stat").await?;
    let cpu_line = stat_content.lines().next().unwrap_or("");

    if cpu_line.starts_with("cpu ") {
        let values: Vec<u64> = cpu_line
            .split_whitespace()
            .skip(1)
            .take(7)
            .map(|s| s.parse().unwrap_or(0))
            .collect();

        if values.len() >= 4 {
            let idle = values[3];
            let total: u64 = values.iter().sum();
            let usage = if total > 0 {
                ((total - idle) as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            // 读取负载平均值
            let loadavg_content = fs::read_to_string("/proc/loadavg").await?;
            let load_values: Vec<f64> = loadavg_content
                .split_whitespace()
                .take(3)
                .map(|s| s.parse().unwrap_or(0.0))
                .collect();

            return Ok((usage, load_values));
        }
    }

    Ok((0.0, vec![]))
}

// 辅助函数：获取内存使用率
async fn get_memory_usage() -> Result<(f64, u64, u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::fs;

    let meminfo_content = fs::read_to_string("/proc/meminfo").await?;
    let mut total_kb = 0u64;
    let mut available_kb = 0u64;

    for line in meminfo_content.lines() {
        if line.starts_with("MemTotal:") {
            total_kb = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            available_kb = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }

    if total_kb > 0 {
        let used_kb = total_kb - available_kb;
        let usage_percent = (used_kb as f64 / total_kb as f64) * 100.0;

        Ok((
            usage_percent,
            total_kb / 1024,     // 转换为MB
            used_kb / 1024,      // 转换为MB
            available_kb / 1024, // 转换为MB
        ))
    } else {
        Ok((0.0, 0, 0, 0))
    }
}

// 辅助函数：获取系统运行时间
async fn get_system_uptime() -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    use tokio::fs;

    let uptime_content = fs::read_to_string("/proc/uptime").await?;
    let uptime_seconds = uptime_content
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;

    Ok(uptime_seconds)
}


