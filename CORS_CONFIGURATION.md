# CORS 配置说明

## 🌐 CORS 问题解决

为了解决前端访问后端API时的CORS（跨域资源共享）错误，已经在NHI HTTP API服务器中添加了CORS支持。

## 🔧 技术实现

### 1. 添加依赖
在 `Cargo.toml` 中添加了 `tower-http` 依赖：

```toml
tower-http = { version = "0.5", features = ["cors"] }
```

### 2. 修改HTTP API服务器
在 `src/http_api.rs` 中添加了CORS中间件：

```rust
use tower_http::cors::{CorsLayer, Any};

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
        .layer(cors)  // 添加CORS中间件
        .with_state(state)
}
```

## 🎯 CORS 配置详情

### 允许的源
- `http://localhost:3000` - 前端React应用的默认地址

### 允许的方法
- `GET` - 用于获取系统状态、日志等
- `POST` - 用于执行命令
- `OPTIONS` - 浏览器预检请求

### 允许的头部
- `Any` - 允许所有请求头部

## 🚀 重新构建和启动

### 1. 重新构建NHI项目
```bash
# 在NHI项目根目录
cargo build --release
```

### 2. 启动后端服务
```bash
sudo ./target/release/nhi --http-port 8082
```

### 3. 启动前端服务
```bash
cd front
npm start
```

## 🔍 验证CORS配置

### 1. 检查响应头
启动服务后，可以在浏览器开发者工具的网络面板中查看API响应，应该包含以下CORS头：

```
Access-Control-Allow-Origin: http://localhost:3000
Access-Control-Allow-Methods: GET, POST, OPTIONS
Access-Control-Allow-Headers: *
```

### 2. 测试API调用
在前端浏览器控制台中测试：

```javascript
// 测试系统状态API
fetch('http://localhost:8082/api/status')
  .then(response => response.json())
  .then(data => console.log('Status:', data))
  .catch(error => console.error('Error:', error));

// 测试命令API
fetch('http://localhost:8082/api/command', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
  },
  body: JSON.stringify({ command: 'help' })
})
  .then(response => response.json())
  .then(data => console.log('Command result:', data))
  .catch(error => console.error('Error:', error));
```

## 🛠️ 自定义CORS配置

### 允许多个源
如果需要允许多个前端地址，可以修改CORS配置：

```rust
let cors = CorsLayer::new()
    .allow_origin([
        "http://localhost:3000".parse::<HeaderValue>().unwrap(),
        "http://localhost:3001".parse::<HeaderValue>().unwrap(),
        "http://your-domain.com".parse::<HeaderValue>().unwrap(),
    ])
    .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
    .allow_headers(Any);
```

### 允许所有源（仅开发环境）
⚠️ **注意：仅在开发环境使用，生产环境不推荐**

```rust
let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
    .allow_headers(Any);
```

## 🚨 常见问题

### 1. 仍然出现CORS错误
- 确保重新构建了NHI项目：`cargo build --release`
- 确保使用了新构建的二进制文件
- 检查前端是否运行在 `localhost:3000`

### 2. 预检请求失败
- 确保CORS配置中包含了 `OPTIONS` 方法
- 检查请求头是否被正确允许

### 3. 生产环境配置
- 将 `http://localhost:3000` 替换为实际的前端域名
- 考虑使用环境变量来配置允许的源

## 📝 更新日志

- **2024-XX-XX**: 添加了基本的CORS支持
- **配置**: 允许来自 `http://localhost:3000` 的请求
- **方法**: 支持 GET、POST、OPTIONS
- **头部**: 允许所有请求头

现在前端应该可以正常访问NHI后端API了！🎉
