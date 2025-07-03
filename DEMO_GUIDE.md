# NHI (Node Host Infrastructure) 完整演示指南

## 📋 前置准备

### 1. 环境要求
```bash
# 检查系统要求
uname -a  # Linux系统 (Ubuntu 20.04+ 推荐)
whoami    # 需要sudo权限
```

### 2. 构建项目
```bash
# 进入项目目录
cd /home/realgod/sync2/nhi2/NHI

# 运行设置脚本
chmod +x setup.sh
./setup.sh

# 或者手动构建
cargo build --release
chmod +x criu/bin/criu
```

### 3. 验证CRIU
```bash
# 检查项目自带的CRIU版本
./criu/bin/criu --version

# 检查系统CRIU（如果已安装）
which criu && criu --version

# 检查CRIU功能
sudo ./criu/bin/criu check
```

## 🚀 启动参数详解

### 完整启动命令格式
```bash
sudo ./target/release/nhi [OPTIONS]
```

### 所有可用参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--listen-addr <ADDR>` | `0.0.0.0:8080` | TCP监听地址，用于节点间通信 |
| `--discovery-port <PORT>` | `8081` | UDP发现端口，用于自动节点发现 |
| `--node-name <NAME>` | 自动生成 | 节点名称，用于集群标识 |
| `--criu-path <PATH>` | `./criu/bin/criu` | CRIU二进制文件路径 |
| `--http-port <PORT>` | `3000` | HTTP API端口 (0=禁用) |
| `--log-level <LEVEL>` | `info` | 日志级别 (trace/debug/info/warn/error) |
| `--no-network` | false | 禁用网络功能，单机模式 |
| `-h, --help` | - | 显示帮助信息 |

### 启动示例

#### 1. 基础启动（使用项目自带CRIU）
```bash
sudo ./target/release/nhi
```

#### 2. 使用系统全局CRIU
```bash
# 使用系统安装的CRIU
sudo ./target/release/nhi --criu-path /usr/bin/criu

# 使用自定义路径的CRIU
sudo ./target/release/nhi --criu-path /usr/local/bin/criu
```

#### 3. 完整参数启动
```bash
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8080 \
  --discovery-port 8081 \
  --node-name "demo-node-1" \
  --criu-path /usr/bin/criu \
  --log-level debug
```

#### 4. 单机模式（无网络）
```bash
sudo ./target/release/nhi \
  --no-network \
  --criu-path /usr/bin/criu \
  --log-level info
```

## 🎯 基础功能演示

### 步骤1: 启动NHI系统
```bash
# 使用系统CRIU启动
sudo ./target/release/nhi \
  --criu-path /usr/bin/criu \
  --node-name "demo-node"
```

### 步骤2: 基本命令演示
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

# 5. 查看进程输出（使用实际的实例ID）
nhi> logs ec754fcd

# 6. 进入交互模式查看实时输出
nhi> attach ec754fcd

# 7. 退出交互模式
nhi [ec754fcd]> detach
```

### 步骤3: 检查点和恢复演示
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

### 步骤4: TTY兼容性分析
```bash
# 分析进程的CRIU兼容性
nhi> analyze-tty ec754fcd

# 或使用别名
nhi> tty ec754fcd
```

## 🌐 HTTP API 演示

### HTTP API 启动

NHI支持HTTP API，可以通过HTTP请求执行命令，非常适合演示和自动化。

```bash
# 启动NHI并启用HTTP API（默认端口3000）
sudo ./target/release/nhi \
  --criu-path /usr/bin/criu \
  --http-port 3000

# 自定义HTTP端口
sudo ./target/release/nhi \
  --criu-path /usr/bin/criu \
  --http-port 8080

# 禁用HTTP API
sudo ./target/release/nhi \
  --criu-path /usr/bin/criu \
  --http-port 0
```

### HTTP API 端点

NHI提供完整的HTTP API接口：

**命令执行API**:
1. **纯文本API**: `POST /command` - 接收纯文本命令
2. **JSON API**: `POST /api/command` - 接收JSON格式命令

**系统监控API**:
3. **CPU监控**: `GET /api/cpu` - 获取CPU使用率和负载
4. **内存监控**: `GET /api/memory` - 获取内存使用情况
5. **日志获取**: `GET /api/logs` - 获取系统日志
6. **系统状态**: `GET /api/status` - 获取综合系统状态

### 纯文本API演示

```bash
# 基本命令
curl -X POST http://localhost:3000/command -d "help"
curl -X POST http://localhost:3000/command -d "list"

# 启动进程
curl -X POST http://localhost:3000/command -d "start-detached ./examples/simple_counter"

# 查看实例
curl -X POST http://localhost:3000/command -d "list"

# 查看日志（使用实际的实例ID）
curl -X POST http://localhost:3000/command -d "logs 51603c64"

# 创建检查点
curl -X POST http://localhost:3000/command -d "checkpoint 51603c64 demo-checkpoint"

# 迁移实例（需要启用网络）
curl -X POST http://localhost:3000/command -d "migrate 51603c64 5c1d4e26-3097-4d9e-b19f-677d943e6933"

# 停止实例
curl -X POST http://localhost:3000/command -d "stop 51603c64"
```

### 系统监控API演示

```bash
# 获取CPU使用率
curl http://localhost:3000/api/cpu

# 获取内存使用率
curl http://localhost:3000/api/memory

# 获取系统综合状态
curl http://localhost:3000/api/status

# 获取日志（默认100行）
curl http://localhost:3000/api/logs

# 获取最近20行日志
curl "http://localhost:3000/api/logs?lines=20"
```

### JSON API演示

```bash
# JSON格式的命令
curl -X POST http://localhost:3000/api/command \
  -H "Content-Type: application/json" \
  -d '{"command": "help"}'

curl -X POST http://localhost:3000/api/command \
  -H "Content-Type: application/json" \
  -d '{"command": "list"}'

curl -X POST http://localhost:3000/api/command \
  -H "Content-Type: application/json" \
  -d '{"command": "start-detached ./examples/simple_counter"}'
```

### HTTP API 响应格式

所有API响应都是JSON格式：

```json
{
  "success": true,
  "message": "Command executed successfully",
  "output": "Command executed via HTTP API"
}
```

错误响应：
```json
{
  "success": false,
  "message": "❌ Command execution failed: Instance not found: 12345678",
  "output": null
}
```

### 终端日志显示

当通过HTTP API执行命令时，NHI终端会显示醒目的日志：

```
════════════════════════════════════════════════════════════
🎯 🌐 HTTP TEXT Command Received
════════════════════════════════════════════════════════════

ℹ️ 📝 Command: migrate 51603c64 5c1d4e26-3097-4d9e-b19f-677d943e6933
2025-07-03T04:26:00.478006Z  INFO HTTP TEXT API received command: migrate 51603c64 5c1d4e26-3097-4d9e-b19f-677d943e6933
✅ ✅ Command executed successfully: migrate 51603c64 5c1d4e26-3097-4d9e-b19f-677d943e6933
```

## 🌐 分布式集群演示

### 步骤1: 启动第一个节点（Node A）
```bash
# 终端1 - 启动主节点
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8080 \
  --discovery-port 8081 \
  --node-name "node-a" \
  --criu-path /usr/bin/criu
```

### 步骤2: 启动第二个节点（Node B）
```bash
# 终端2 - 启动从节点
sudo ./target/release/nhi \
  --listen-addr 0.0.0.0:8082 \
  --discovery-port 8083 \
  --node-name "node-b" \
  --criu-path /usr/bin/criu
```

### 步骤3: 集群管理演示
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

### 步骤4: 进程迁移演示
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

## 🔧 高级功能演示

### 1. 影子实例管理
```bash
# 查看影子实例
nhi> shadow-view ec754fcd

# 连接到影子实例
nhi> attach ec754fcd  # 在拥有影子实例的节点上
```

### 2. 输出监控
```bash
# 查看最近20行输出
nhi> logs ec754fcd

# 查看最近50行输出
nhi> logs ec754fcd 50

# 实时监控输出
nhi> attach ec754fcd
```

### 3. 进程生命周期管理
```bash
# 暂停进程
nhi> pause ec754fcd

# 恢复进程
nhi> resume ec754fcd

# 停止进程
nhi> stop ec754fcd
```

## 🎯 HTTP API 完整演示脚本

### 自动化演示脚本

项目包含一个完整的HTTP API演示脚本：

```bash
# 运行演示脚本
./test_http_api.sh
```

### 手动演示步骤

```bash
# 1. 启动NHI（带HTTP API）
sudo ./target/release/nhi --http-port 3000 --no-network

# 2. 在另一个终端中测试API
# 基本命令测试
curl -X POST http://localhost:3000/command -d "help"
curl -X POST http://localhost:3000/command -d "list"

# 启动进程
curl -X POST http://localhost:3000/command -d "start-detached ./examples/simple_counter"

# 查看实例
curl -X POST http://localhost:3000/command -d "list"

# 演示迁移命令（即使在单机模式下也会显示日志）
curl -X POST http://localhost:3000/command -d "migrate 51603c64 5c1d4e26-3097-4d9e-b19f-677d943e6933"
```

### HTTP API 优势

1. **演示友好** - 通过HTTP请求可以轻松演示所有功能
2. **自动化集成** - 可以集成到CI/CD流程中
3. **远程控制** - 可以远程控制NHI节点
4. **脚本化** - 支持批量操作和自动化脚本
5. **醒目日志** - 终端显示清晰的HTTP请求日志

## 📝 重要注意事项

### 1. CRIU路径配置
- **项目自带CRIU**: `--criu-path ./criu/bin/criu` (默认)
- **系统CRIU**: `--criu-path /usr/bin/criu`
- **自定义路径**: `--criu-path /path/to/your/criu`

### 2. HTTP API配置
- **默认端口**: `--http-port 3000`
- **自定义端口**: `--http-port 8080`
- **禁用API**: `--http-port 0`

### 3. 权限要求
- 所有操作都需要 `sudo` 权限
- CRIU需要root权限进行进程检查点操作
- 确保CRIU二进制文件有执行权限

### 4. 网络配置
- 确保防火墙允许指定端口通信
- 默认端口：8080 (TCP), 8081 (UDP), 3000 (HTTP)
- 可以通过参数自定义端口

### 5. 实例管理
- 使用 `start-detached` 获得更好的CRIU兼容性
- 实例ID是8位短ID，可以用于所有命令
- 使用 `list` 命令获取准确的实例ID

### 6. 日志和调试
- 使用 `RUST_LOG=debug` 获取详细日志
- CRIU日志保存在 `/tmp/criu-*.log`
- 实例数据保存在 `instances/` 目录

### 7. 清理和维护
- 测试后清理 `instances/` 目录避免冲突
- 定期检查磁盘空间（检查点文件可能很大）
- 监控系统资源使用情况

## 🎉 演示总结

这个完整的演示指南涵盖了NHI的所有主要功能：

✅ **基础功能**
- 进程启动、停止、暂停、恢复
- 检查点创建和恢复
- TTY兼容性分析

✅ **分布式功能**
- 多节点集群管理
- 自动节点发现
- 进程迁移

✅ **高级功能**
- 影子实例管理
- 实时输出监控
- HTTP API控制

✅ **演示友好**
- 醒目的终端日志显示
- HTTP API支持远程控制
- 完整的自动化脚本

按照这个指南，您可以完整体验NHI的强大功能，从基础的进程管理到高级的分布式迁移，特别是新增的HTTP API功能让演示变得更加优雅和自动化。
