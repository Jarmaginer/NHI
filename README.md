# NHI (Node Host Infrastructure)

🚀 **A distributed process management system with live migration capabilities using CRIU**

NHI enables seamless live migration of running processes between cluster nodes, providing high availability and load balancing for distributed applications. This project evolved from 'criu-cli' to 'nhi', focusing on instance-based management where instances are persistent task containers that survive PID changes during checkpoint/restore operations.

## ✨ Features

- **🔄 Live Process Migration**: Seamlessly migrate running processes between nodes with zero downtime
- **⚡ Real-time Synchronization**: Automatic checkpoint synchronization across cluster nodes every 30 seconds
- **👥 Shadow Instances**: Maintain hot standby copies of processes for instant failover
- **🛠️ CRIU Integration**: Leverage CRIU for checkpoint/restore operations with TTY support
- **🌐 Distributed Architecture**: P2P multi-node cluster support with automatic discovery
- **💻 Interactive CLI**: User-friendly command-line interface for process management
- **📊 Process Monitoring**: Real-time process status and output monitoring with attach mode
- **🔄 Instance-based Management**: Persistent instances that survive PID changes during migration

## 🔧 Prerequisites

- **Linux system** with CRIU support (Ubuntu 20.04+ recommended)
- **Rust** (latest stable version)
- **sudo privileges** (required for CRIU operations)
- **Network connectivity** between cluster nodes
- **User credentials**: username: realgod, password: Jn89/*Qb+$

## 🚀 Quick Start

### 1. Clone and Build

```bash
git clone https://github.com/Jarmaginer/NHI.git
cd NHI
chmod +x setup.sh
./setup.sh
```

The setup script will:
- Check Rust installation
- Verify CRIU binary availability
- Build NHI with all dependencies
- Set up necessary permissions

### 2. Single Node Setup

```bash
sudo ./target/release/nhi
```

### 3. Multi-Node Cluster Setup

**Node A (Primary):**
```bash
sudo ./target/release/nhi --listen-addr 0.0.0.0:8080 --discovery-port 8081
```

**Node B (Secondary):**
```bash
sudo ./target/release/nhi --listen-addr 0.0.0.0:8082 --discovery-port 8083
```

Nodes will automatically discover each other on the same network using UDP broadcast.

## ⚙️ Command Line Options

NHI supports various command line options for configuration:

```bash
sudo ./target/release/nhi [OPTIONS]
```

### Available Options

| Option | Default | Description |
|--------|---------|-------------|
| `--listen-addr <ADDR>` | `0.0.0.0:8080` | Network listen address for P2P connections |
| `--discovery-port <PORT>` | `8081` | UDP port for node discovery |
| `--node-name <NAME>` | Auto-generated | Custom node name for cluster identification |
| `--criu-path <PATH>` | `./criu/bin/criu` | Path to CRIU binary executable |
| `--no-network` | false | Disable networking (Stage 1 compatibility mode) |
| `--log-level <LEVEL>` | `info` | Logging level (trace, debug, info, warn, error) |

### Examples

**Custom CRIU Path:**
```bash
sudo ./target/release/nhi --criu-path /usr/local/bin/criu
```

**Custom Network Configuration:**
```bash
sudo ./target/release/nhi --listen-addr 192.168.1.100:9000 --discovery-port 9001 --node-name production-node-1
```

**Standalone Mode (No Networking):**
```bash
sudo ./target/release/nhi --no-network
```

**Debug Mode:**
```bash
sudo ./target/release/nhi --log-level debug
```

## 📖 Usage Guide

### Starting a Process
```bash
nhi> start-detached simple_counter
nhi> start-detached my_app arg1 arg2
```

### Viewing Processes
```bash
nhi> list
# Shows: Instance ID, Status (Running/Shadow), PID, Program, Auto-sync status
```

### Process Migration
```bash
# Get cluster information
nhi> cluster list-nodes

# Migrate process to another node
nhi> migrate <instance_id> <target_node_id>
```

### Monitoring Process Output
```bash
nhi> attach <instance_id>
# Shows historical output and real-time streaming with input capability
```

### Cluster Management
```bash
nhi> cluster list-nodes  # Show all connected nodes
nhi> cluster status      # Show cluster health
```

## � 完整演示指南

### 📋 前置准备

#### 1. 环境要求
```bash
# 检查系统要求
uname -a  # Linux系统
whoami    # 需要sudo权限
```

#### 2. 构建项目
```bash
# 进入项目目录
cd /path/to/NHI

# 运行设置脚本
chmod +x setup.sh
./setup.sh

# 或者手动构建
cargo build --release
chmod +x criu/bin/criu
```

#### 3. 验证CRIU
```bash
# 检查CRIU版本
./criu/bin/criu --version

# 检查CRIU功能
sudo ./criu/bin/criu check
```

### 🚀 基础演示流程

#### 步骤1: 启动单节点NHI系统

```bash
# 基础启动（默认配置）
sudo ./target/release/nhi

# 完整参数启动
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8080 \
  --discovery-port 8081 \
  --node-name "demo-node-1" \
  --log-level info

# 无网络模式（单机测试）
sudo ./target/release/nhi --no-network
```

**启动参数说明：**
- `--listen-addr`: TCP监听地址，用于节点间通信（默认：0.0.0.0:8080）
- `--discovery-port`: UDP发现端口，用于自动节点发现（默认：8081）
- `--node-name`: 节点名称，用于集群标识（默认：自动生成）
- `--log-level`: 日志级别（debug/info/warn/error，默认：info）
- `--no-network`: 禁用网络功能，单机模式

#### 步骤2: 基本命令演示

```bash
# 在NHI CLI中执行以下命令：

# 1. 查看帮助
nhi> help

# 2. 查看当前实例
nhi> list

# 3. 启动一个测试进程（CRIU优化模式）
nhi> start-detached ./examples/simple_counter

# 4. 再次查看实例列表
nhi> list

# 5. 查看进程输出
nhi> logs ec754fcd  # 使用实际的实例ID

# 6. 进入交互模式查看实时输出
nhi> attach ec754fcd

# 7. 退出交互模式
nhi [ec754fcd]> detach
```

#### 步骤3: 检查点和恢复演示

```bash
# 1. 创建检查点
nhi> checkpoint ec754fcd checkpoint-1

# 2. 停止进程
nhi> stop ec754fcd

# 3. 从检查点恢复
nhi> restore ec754fcd checkpoint-1

# 4. 验证恢复成功
nhi> list
nhi> logs ec754fcd
```

#### 步骤4: TTY兼容性分析

```bash
# 分析进程的CRIU兼容性
nhi> analyze-tty ec754fcd

# 或使用别名
nhi> tty ec754fcd
```

### 🌐 分布式集群演示

#### 步骤1: 启动第一个节点（Node A）

```bash
# 终端1 - 启动主节点
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8080 \
  --discovery-port 8081 \
  --node-name "node-a"
```

#### 步骤2: 启动第二个节点（Node B）

```bash
# 终端2 - 启动从节点
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8082 \
  --discovery-port 8083 \
  --node-name "node-b"
```

#### 步骤3: 集群管理演示

```bash
# 在Node A中执行：

# 1. 查看集群节点
nhi> cluster list-nodes

# 2. 查看集群状态
nhi> cluster status

# 3. 查看本地节点信息
nhi> cluster node-info

# 4. 手动连接到另一个节点（如果自动发现失败）
nhi> cluster connect 127.0.0.1:8082
```

#### 步骤4: 进程迁移演示

```bash
# 在Node A中执行：

# 1. 启动一个进程
nhi> start-detached ./examples/simple_counter

# 2. 查看实例（应该显示为Running状态）
nhi> list

# 3. 获取Node B的ID
nhi> cluster list-nodes
# 记录Node B的ID，例如：b1234567-...

# 4. 执行迁移
nhi> migrate ec754fcd b1234567-8901-2345-6789-abcdef123456

# 5. 验证迁移结果
nhi> list  # 应该显示为Shadow状态

# 在Node B中验证：
nhi> list  # 应该显示为Running状态
```

### 🔧 高级功能演示

#### 1. 影子实例管理

```bash
# 查看影子实例
nhi> shadow-view ec754fcd

# 连接到影子实例
nhi> attach ec754fcd  # 在拥有影子实例的节点上
```

#### 2. 输出监控

```bash
# 查看最近20行输出
nhi> logs ec754fcd

# 查看最近50行输出
nhi> logs ec754fcd 50

# 实时监控输出
nhi> attach ec754fcd
```

#### 3. 进程生命周期管理

```bash
# 暂停进程
nhi> pause ec754fcd

# 恢复进程
nhi> resume ec754fcd

# 停止进程
nhi> stop ec754fcd
```

## �🏗️ Architecture

### Core Components

- **🎯 Node Manager** (`node_manager.rs`): Handles cluster membership and node discovery
- **⚙️ Process Manager** (`process_manager.rs`): Manages local process lifecycle and monitoring
- **🔄 Migration Manager** (`migration_manager.rs`): Orchestrates process migration between nodes
- **👥 Shadow Instance Manager** (`shadow_instance_manager.rs`): Maintains synchronized shadow copies
- **📦 Image Sync Manager**: Handles checkpoint data synchronization
- **🌐 Network Manager** (`network_manager.rs`): Manages inter-node communication

### Instance Management System

Each instance has:
- **Unique Instance ID**: Simplified 8-character identifier
- **Dedicated Folder**: `instances/instance_<id>/` containing CRIU images and output history
- **State Management**: Running, Shadow, or Stopped states
- **PID Tracking**: Validates process existence before operations
- **Output History**: Persistent storage of process output in `output/process_output.log`

### Migration Workflow

1. **Source Node**: Creates checkpoint, pauses process, sends data to target
2. **Target Node**: Receives checkpoint data, restores process, updates state
3. **State Swap**: Source becomes shadow, target becomes running
4. **Synchronization**: Automatic checkpoint sync every 30 seconds

## 🧪 Testing

### Manual Testing Environment
```bash
# Prepare distributed test environment
./manual_test.sh
```

This creates:
- `test_node_a/` and `test_node_b/` directories
- Copies NHI binary to each test directory
- Creates startup scripts for isolated testing

### Test Workflow
1. **Terminal 1**: `cd test_node_a && ./start.sh` (Node A: ports 8080/8081)
2. **Terminal 2**: `cd test_node_b && ./start.sh` (Node B: ports 8082/8083)
3. **Start Instance**: `start-detached simple_counter` on Node A
4. **Verify Shadow**: Check `list` on Node B shows shadow instance
5. **Execute Migration**: `migrate <instance_id> <node_b_id>` on Node A
6. **Verify Result**: Check process moved to Node B and Node A shows shadow

### Automated Tests
```bash
cargo test
```

## 🛠️ Development

### Project Structure
```
NHI/
├── src/
│   ├── main.rs                      # Entry point and CLI
│   ├── cli.rs                       # Command-line interface
│   ├── node_manager.rs              # P2P cluster management
│   ├── process_manager.rs           # Process lifecycle management
│   ├── migration_manager.rs         # Migration orchestration
│   ├── shadow_instance_manager.rs   # Shadow instance management
│   ├── network_manager.rs           # Inter-node communication
│   ├── criu_manager.rs              # CRIU operations wrapper
│   ├── types.rs                     # Core data structures
│   ├── instance.rs                  # Instance management
│   ├── colors.rs                    # Terminal color output
│   ├── logger.rs                    # File-based logging
│   └── output.rs                    # Formatted output utilities
├── deps/                            # Local dependencies
│   ├── rust-criu/                   # Rust CRIU bindings
│   └── criu-image-streamer/         # CRIU image streaming
├── criu/
│   └── bin/
│       └── criu                     # Pre-compiled CRIU binary
├── examples/
│   └── simple_counter               # C++ test program
├── setup.sh                        # Installation script
├── Cargo.toml                       # Project configuration
└── .gitignore                       # Git ignore rules
```

### Key Implementation Details

#### CRIU Integration
- **Path**: Uses relative path `./criu/bin/criu` for portability
- **TTY Support**: Handles TTY issues with `--external 'tty[rdev:dev]'` and `--inherit-fd`
- **Restore Process**: Kills original process before restoring to same PID
- **Signal Handling**: Sends SIGCONT to resume execution after restore

#### Migration Process
- **Checkpoint Creation**: Uses `--restore-detached` for consistency
- **File Path Mapping**: Creates compatible directory structure for path resolution
- **PID Management**: Reads PID from pidfile with absolute paths
- **State Synchronization**: Real-time output streaming between nodes

#### Network Protocol
- **Discovery**: UDP broadcast on discovery port for auto-discovery
- **Communication**: Direct TCP connections between nodes
- **Message Types**: Heartbeat, instance creation, migration requests, data sync

### Building from Source
```bash
cargo build --release
```

### Development Mode
```bash
cargo run
```

### Code Quality
- **No Warnings**: Code compiles cleanly without warnings
- **Error Handling**: Comprehensive error handling with anyhow
- **Logging**: File-based logging with tracing crate
- **Testing**: Manual and automated test coverage

## 🔍 Troubleshooting

### Common Issues

1. **Permission Denied**: Ensure running with sudo privileges
2. **CRIU Not Found**: Verify `./criu/bin/criu` exists and is executable
3. **Network Issues**: Check firewall settings for ports 8080-8083
4. **Migration Failures**: Check `/tmp/criu-*.log` for detailed CRIU errors
5. **Process Death**: Check working directory and file descriptor issues

### Debug Commands
```bash
# Enable debug logging
RUST_LOG=debug sudo ./target/release/nhi

# Check process status
ps aux | grep simple_counter

# Verify CRIU functionality
./criu/bin/criu --version

# Check network connectivity
netstat -tulpn | grep 808
```

### Migration Debugging
- **Source Node**: Check if checkpoint creation succeeded
- **Target Node**: Verify checkpoint data received and restored
- **Process Health**: Confirm migrated process is actually running
- **Working Directory**: Ensure process has correct working directory after migration

## 📊 Development History

### Stage 1: Basic CRIU Operations
- Single-node checkpoint/restore functionality
- Interactive CLI with start-detached command
- Instance-based management system

### Stage 2: Distributed Networking
- P2P node discovery and connection
- Real-time cluster state synchronization
- Network message protocol implementation

### Stage 3: Shadow State & Migration (Current)
- Shadow instance management with real-time synchronization
- Live process migration between nodes
- Automatic checkpoint synchronization every 30 seconds
- Migration state swapping (source→shadow, target→running)

### Recent Fixes
- **Migration Process Death**: Fixed with `--restore-detached` parameter
- **Path Resolution**: Implemented file path mapping for cross-node compatibility
- **PID Management**: Improved pidfile handling with absolute paths
- **Dependency Management**: Organized all dependencies in `deps/` directory

## 🔧 Technical Implementation Details

### CRIU Command Line Usage
```bash
# Checkpoint (dump) process - Updated with proper parameters
./criu/bin/criu dump --tree <pid> -D <images_dir> --leave-running --shell-job -v4

# Restore process - Updated with proper parameters
./criu/bin/criu restore -D <images_dir> --restore-detached --shell-job -v4
```

### CRIU Migration Improvements

**🎯 Key Fixes Applied:**
- **True Daemon Wrapper**: Added `daemon_wrapper.c` for proper process daemonization
- **Correct CRIU Parameters**: Using `--tree` instead of `-t` and `--shell-job` for session handling
- **TTY Independence**: Processes are now completely detached from TTY using double fork technique
- **Automatic Building**: Daemon wrapper is automatically compiled during build process

**🚀 Enhanced Detached Mode:**
The `start-detached` command now uses a proper daemon wrapper that:
- Performs double fork for complete TTY detachment
- Creates independent session with `setsid()`
- Redirects all file descriptors properly
- Ensures CRIU compatibility with `--shell-job` parameter

### Instance Directory Structure
```
instances/instance_<id>/
├── images/                    # CRIU checkpoint images
│   ├── core-<pid>.img
│   ├── mm-<pid>.img
│   ├── pagemap-<pid>.img
│   └── ...
├── output/
│   └── process_output.log     # Historical process output
├── config.json               # Instance configuration
└── pidfile                   # Current process PID
```

### Network Message Protocol
```rust
pub enum NetworkMessage {
    Heartbeat(HeartbeatMessage),
    InstanceCreated(InstanceCreatedMessage),
    MigrationRequest(MigrationRequestMessage),
    MigrationResponse(MigrationResponseMessage),
    CheckpointSync(CheckpointSyncMessage),
    DataStream(DataStreamMessage),
}
```

### Migration State Machine
```
Running → [Migration Request] → Checkpointing → Transferring → Shadow
Shadow ← [Migration Complete] ← Restoring ← Receiving ← Running (Target)
```

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes following the existing code style
4. Add tests if applicable
5. Ensure no compiler warnings
6. Submit a pull request

### Development Guidelines
- Use absolute paths for file operations
- Prefer colorized terminal output
- Implement file-based logging over console logging
- Test thoroughly with manual test scripts
- Follow Rust best practices and error handling patterns

### Testing Requirements
- Manual testing with `./manual_test.sh` script
- Distributed testing with separate working directories
- Process migration verification with health checks
- Shadow state synchronization validation

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- [CRIU Project](https://criu.org/) for checkpoint/restore functionality
- Rust community for excellent tooling and libraries
- Development focused on clean, warning-free code with comprehensive testing

## 📞 Support

For issues and questions:
1. Check the troubleshooting section above
2. Review CRIU logs in `/tmp/criu-*.log`
3. Enable debug logging with `RUST_LOG=debug`
4. Test with the provided `simple_counter` example program
5. Verify network connectivity between nodes

## 🎯 Future Roadmap

- **Performance Optimization**: Reduce migration time and checkpoint size
- **Security**: Add authentication and encryption for inter-node communication
- **Monitoring**: Enhanced metrics and monitoring capabilities
- **Load Balancing**: Automatic load balancing based on resource usage
- **Web Interface**: Web-based management interface
- **Container Support**: Docker container migration support
