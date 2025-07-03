# NHI HTTP API 完整文档

## 📋 概述

NHI (Node Host Infrastructure) 提供了完整的HTTP API接口，支持命令执行和系统监控。API分为两大类：

1. **命令执行API** - 用于执行NHI命令
2. **系统监控API** - 用于获取系统状态信息

## 🌐 基础信息

- **基础URL**: `http://localhost:3000` (默认端口)
- **内容类型**: `application/json`
- **认证**: 无需认证
- **协议**: HTTP/1.1

## 📡 命令执行API

### 1. JSON命令API

**端点**: `POST /api/command`

**请求格式**:
```json
{
  "command": "string"
}
```

**请求示例**:
```bash
curl -X POST http://localhost:3000/api/command \
  -H "Content-Type: application/json" \
  -d '{"command": "list"}'
```

**响应格式**:
```json
{
  "success": boolean,
  "message": "string",
  "output": "string | null"
}
```

**成功响应示例**:
```json
{
  "success": true,
  "message": "Command executed successfully",
  "output": "Command executed via JSON API"
}
```

**错误响应示例**:
```json
{
  "success": false,
  "message": "❌ Command execution failed: Instance not found: 12345678",
  "output": null
}
```

### 2. 纯文本命令API

**端点**: `POST /command`

**请求格式**: 纯文本字符串作为请求体

**请求示例**:
```bash
curl -X POST http://localhost:3000/command \
  -d "list"
```

**响应格式**: 与JSON API相同
```json
{
  "success": boolean,
  "message": "string",
  "output": "string | null"
}
```

### 支持的命令列表

| 命令 | 描述 | 示例 |
|------|------|------|
| `help` | 显示帮助信息 | `help` |
| `list` | 列出所有实例 | `list` |
| `start-detached <program> [args...]` | 启动分离进程 | `start-detached ./examples/simple_counter` |
| `stop <instance_id>` | 停止实例 | `stop 51603c64` |
| `pause <instance_id>` | 暂停实例 | `pause 51603c64` |
| `resume <instance_id>` | 恢复实例 | `resume 51603c64` |
| `logs [instance_id] [lines]` | 查看日志 | `logs 51603c64 20` |
| `checkpoint <instance_id> <name>` | 创建检查点 | `checkpoint 51603c64 backup-1` |
| `restore <instance_id> <checkpoint_name>` | 恢复检查点 | `restore 51603c64 backup-1` |
| `migrate <instance_id> <target_node_id>` | 迁移实例 | `migrate 51603c64 node-uuid` |
| `cluster list-nodes` | 列出集群节点 | `cluster list-nodes` |
| `cluster status` | 集群状态 | `cluster status` |

## 📊 系统监控API

### 1. 获取日志

**端点**: `GET /api/logs`

**查询参数**:
- `lines` (可选): 获取的日志行数，默认100行

**请求示例**:
```bash
# 获取默认100行日志
curl http://localhost:3000/api/logs

# 获取最近20行日志
curl "http://localhost:3000/api/logs?lines=20"

# 获取最近5行日志
curl "http://localhost:3000/api/logs?lines=5"
```

**响应格式**:
```json
{
  "success": boolean,
  "logs": ["string"],
  "total_lines": number
}
```

**成功响应示例**:
```json
{
  "success": true,
  "logs": [
    "{\"timestamp\":\"2025-07-03T04:38:34.185072Z\",\"level\":\"INFO\",\"fields\":{\"message\":\"HTTP API: Fetching logs, lines: 3\"},\"target\":\"nhi::http_api\",\"filename\":\"src/http_api.rs\",\"line_number\":262,\"threadId\":\"ThreadId(4)\"}",
    "{\"timestamp\":\"2025-07-03T04:38:34.185559Z\",\"level\":\"INFO\",\"fields\":{\"message\":\"Successfully read 3 lines from logs\"},\"target\":\"nhi::http_api\",\"filename\":\"src/http_api.rs\",\"line_number\":267,\"threadId\":\"ThreadId(4)\"}"
  ],
  "total_lines": 2
}
```

**日志格式说明**:
每条日志都是JSON字符串格式，包含以下字段：
- `timestamp`: 时间戳 (ISO 8601格式)
- `level`: 日志级别 (INFO, WARN, ERROR, DEBUG)
- `fields.message`: 日志消息内容
- `target`: 日志来源模块
- `filename`: 源文件名
- `line_number`: 源代码行号
- `threadId`: 线程ID

**解析日志示例**:
```javascript
// 解析日志条目
function parseLogEntry(logString) {
  try {
    const logEntry = JSON.parse(logString);
    return {
      timestamp: logEntry.timestamp,
      level: logEntry.level,
      message: logEntry.fields.message,
      source: `${logEntry.filename}:${logEntry.line_number}`,
      thread: logEntry.threadId
    };
  } catch (e) {
    return { message: logString }; // 降级处理
  }
}
```

**错误响应示例**:
```json
{
  "success": false,
  "logs": ["Failed to read logs: Permission denied"],
  "total_lines": 0
}
```

### 2. 获取CPU使用率

**端点**: `GET /api/cpu`

**请求示例**:
```bash
curl http://localhost:3000/api/cpu
```

**响应格式**:
```json
{
  "success": boolean,
  "cpu_usage_percent": number,
  "load_average": [number, number, number]
}
```

**成功响应示例**:
```json
{
  "success": true,
  "cpu_usage_percent": 4.81,
  "load_average": [0.92, 0.55, 0.47]
}
```

**字段说明**:
- `cpu_usage_percent`: CPU使用率百分比 (0-100)
- `load_average`: 系统负载平均值 [1分钟, 5分钟, 15分钟]

### 3. 获取内存使用率

**端点**: `GET /api/memory`

**请求示例**:
```bash
curl http://localhost:3000/api/memory
```

**响应格式**:
```json
{
  "success": boolean,
  "memory_usage_percent": number,
  "total_memory_mb": number,
  "used_memory_mb": number,
  "available_memory_mb": number
}
```

**成功响应示例**:
```json
{
  "success": true,
  "memory_usage_percent": 26.71,
  "total_memory_mb": 7716,
  "used_memory_mb": 2061,
  "available_memory_mb": 5655
}
```

**字段说明**:
- `memory_usage_percent`: 内存使用率百分比 (0-100)
- `total_memory_mb`: 总内存大小 (MB)
- `used_memory_mb`: 已使用内存 (MB)
- `available_memory_mb`: 可用内存 (MB)

### 4. 获取系统综合状态

**端点**: `GET /api/status`

**请求示例**:
```bash
curl http://localhost:3000/api/status
```

**响应格式**:
```json
{
  "success": boolean,
  "uptime_seconds": number,
  "cpu_usage_percent": number,
  "memory_usage_percent": number,
  "active_instances": number,
  "node_name": "string",
  "api_version": "string"
}
```

**成功响应示例**:
```json
{
  "success": true,
  "uptime_seconds": 5676,
  "cpu_usage_percent": 4.81,
  "memory_usage_percent": 26.92,
  "active_instances": 0,
  "node_name": "standalone",
  "api_version": "1.0.0"
}
```

**字段说明**:
- `uptime_seconds`: 系统运行时间 (秒)
- `cpu_usage_percent`: CPU使用率百分比
- `memory_usage_percent`: 内存使用率百分比
- `active_instances`: 当前活跃实例数量
- `node_name`: 节点名称
- `api_version`: API版本号

## 🔧 错误处理

### HTTP状态码

- `200 OK`: 请求成功
- `400 Bad Request`: 请求格式错误
- `500 Internal Server Error`: 服务器内部错误

### 错误响应格式

所有API在发生错误时都会返回统一的错误格式：

```json
{
  "success": false,
  "message": "错误描述信息",
  "output": null
}
```

## 📝 使用示例

### JavaScript/TypeScript 示例

```javascript
// 获取系统状态
async function getSystemStatus() {
  try {
    const response = await fetch('http://localhost:3000/api/status');
    const data = await response.json();

    if (data.success) {
      console.log('CPU使用率:', data.cpu_usage_percent + '%');
      console.log('内存使用率:', data.memory_usage_percent + '%');
      console.log('活跃实例:', data.active_instances);
    }
  } catch (error) {
    console.error('获取状态失败:', error);
  }
}

// 执行命令
async function executeCommand(command) {
  try {
    const response = await fetch('http://localhost:3000/api/command', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ command: command })
    });

    const data = await response.json();
    return data;
  } catch (error) {
    console.error('命令执行失败:', error);
    return { success: false, message: error.message };
  }
}

// 获取日志
async function getLogs(lines = 50) {
  try {
    const response = await fetch(`http://localhost:3000/api/logs?lines=${lines}`);
    const data = await response.json();

    if (data.success) {
      // 解析JSON格式的日志
      return data.logs.map(logString => {
        try {
          const logEntry = JSON.parse(logString);
          return {
            timestamp: logEntry.timestamp,
            level: logEntry.level,
            message: logEntry.fields.message,
            source: `${logEntry.filename}:${logEntry.line_number}`,
            thread: logEntry.threadId,
            raw: logString
          };
        } catch (e) {
          return { message: logString, raw: logString };
        }
      });
    }
    return [];
  } catch (error) {
    console.error('获取日志失败:', error);
    return [];
  }
}
```

### Python 示例

```python
import requests
import json

class NHIClient:
    def __init__(self, base_url="http://localhost:3000"):
        self.base_url = base_url

    def execute_command(self, command):
        """执行NHI命令"""
        url = f"{self.base_url}/api/command"
        data = {"command": command}

        try:
            response = requests.post(url, json=data)
            return response.json()
        except Exception as e:
            return {"success": False, "message": str(e)}

    def get_system_status(self):
        """获取系统状态"""
        url = f"{self.base_url}/api/status"

        try:
            response = requests.get(url)
            return response.json()
        except Exception as e:
            return {"success": False, "message": str(e)}

    def get_logs(self, lines=100):
        """获取日志"""
        url = f"{self.base_url}/api/logs"
        params = {"lines": lines}

        try:
            response = requests.get(url, params=params)
            return response.json()
        except Exception as e:
            return {"success": False, "logs": [str(e)], "total_lines": 0}

    def get_cpu_usage(self):
        """获取CPU使用率"""
        url = f"{self.base_url}/api/cpu"

        try:
            response = requests.get(url)
            return response.json()
        except Exception as e:
            return {"success": False, "message": str(e)}

    def get_memory_usage(self):
        """获取内存使用率"""
        url = f"{self.base_url}/api/memory"

        try:
            response = requests.get(url)
            return response.json()
        except Exception as e:
            return {"success": False, "message": str(e)}

# 使用示例
client = NHIClient()

# 获取系统状态
status = client.get_system_status()
if status["success"]:
    print(f"CPU: {status['cpu_usage_percent']:.2f}%")
    print(f"内存: {status['memory_usage_percent']:.2f}%")

# 执行命令
result = client.execute_command("list")
print(result)

# 获取最近20行日志
logs = client.get_logs(20)
if logs["success"]:
    for log_string in logs["logs"]:
        try:
            # 解析JSON格式的日志
            log_entry = json.loads(log_string)
            timestamp = log_entry["timestamp"]
            level = log_entry["level"]
            message = log_entry["fields"]["message"]
            print(f"[{timestamp}] {level}: {message}")
        except json.JSONDecodeError:
            # 降级处理：直接打印原始字符串
            print(log_string)
```

## 🚀 前端集成建议

### 1. 实时监控

建议使用定时器定期获取系统状态：

```javascript
// 每5秒更新一次系统状态
setInterval(async () => {
  const status = await getSystemStatus();
  updateDashboard(status);
}, 5000);
```

### 2. 日志流式显示

```javascript
// 实现日志的流式显示
let lastLogCount = 0;

async function updateLogs() {
  const logs = await getLogs(100);
  if (logs.length > lastLogCount) {
    // 只显示新增的日志
    const newLogs = logs.slice(lastLogCount);
    appendLogsToUI(newLogs);
    lastLogCount = logs.length;
  }
}

setInterval(updateLogs, 2000);
```

### 3. 日志处理最佳实践

```javascript
class LogProcessor {
  constructor() {
    this.logBuffer = [];
    this.maxBufferSize = 1000;
  }

  // 解析单条日志
  parseLogEntry(logString) {
    try {
      const logEntry = JSON.parse(logString);
      return {
        timestamp: new Date(logEntry.timestamp),
        level: logEntry.level,
        message: logEntry.fields.message,
        source: `${logEntry.filename}:${logEntry.line_number}`,
        thread: logEntry.threadId,
        raw: logString
      };
    } catch (e) {
      return {
        timestamp: new Date(),
        level: 'UNKNOWN',
        message: logString,
        raw: logString
      };
    }
  }

  // 批量处理日志
  processLogs(logs) {
    const parsedLogs = logs.map(log => this.parseLogEntry(log));

    // 添加到缓冲区
    this.logBuffer.push(...parsedLogs);

    // 保持缓冲区大小
    if (this.logBuffer.length > this.maxBufferSize) {
      this.logBuffer = this.logBuffer.slice(-this.maxBufferSize);
    }

    return parsedLogs;
  }

  // 按级别过滤日志
  filterByLevel(level) {
    return this.logBuffer.filter(log => log.level === level);
  }

  // 搜索日志
  searchLogs(keyword) {
    return this.logBuffer.filter(log =>
      log.message.toLowerCase().includes(keyword.toLowerCase())
    );
  }

  // 格式化显示
  formatLog(logEntry) {
    const time = logEntry.timestamp.toLocaleTimeString();
    const level = logEntry.level.padEnd(5);
    return `[${time}] ${level} ${logEntry.message}`;
  }
}

// 使用示例
const processor = new LogProcessor();

async function fetchAndProcessLogs() {
  const response = await fetch('http://localhost:3000/api/logs?lines=50');
  const data = await response.json();

  if (data.success) {
    const processedLogs = processor.processLogs(data.logs);

    // 显示最新日志
    processedLogs.forEach(log => {
      console.log(processor.formatLog(log));
    });

    // 获取错误日志
    const errors = processor.filterByLevel('ERROR');
    if (errors.length > 0) {
      console.warn(`发现 ${errors.length} 条错误日志`);
    }
  }
}
```

### 4. 错误处理

```javascript
function handleAPIError(response) {
  if (!response.success) {
    // 显示错误消息给用户
    showErrorMessage(response.message);
    return false;
  }
  return true;
}
```

## 📋 注意事项

1. **CORS**: 如果前端和API不在同一域名，需要配置CORS
2. **轮询频率**: 建议系统监控API的轮询间隔不少于1秒
3. **错误重试**: 网络错误时建议实现重试机制
4. **日志分页**: 大量日志时建议实现分页加载
5. **实例ID**: 实例ID通常是8位短ID，如 `51603c64`
6. **命令格式**: 命令参数之间用空格分隔，与CLI使用方式相同

## 🔗 相关链接

- [NHI 完整演示指南](./DEMO_GUIDE.md)
- [项目README](./README.md)

---

**API版本**: v1.0.0
**文档更新时间**: 2025-07-03
**联系方式**: 请通过项目Issues反馈问题
