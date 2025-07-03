#!/bin/bash

# NHI 演示启动脚本
# 自动启动后端和前端服务

echo "🚀 启动 NHI 演示系统..."

# 检查是否已构建
if [ ! -f "./target/release/nhi" ]; then
    echo "❌ 未找到构建的二进制文件，请先运行: cargo build --release"
    exit 1
fi

# 检查前端依赖
if [ ! -d "./front/node_modules" ]; then
    echo "📦 安装前端依赖..."
    cd front
    npm install
    cd ..
fi

# 启动后端 (在后台)
echo "🔧 启动 NHI 后端服务 (端口 8082)..."
sudo ./target/release/nhi --http-port 8082 &
BACKEND_PID=$!

# 等待后端启动
echo "⏳ 等待后端服务启动..."
sleep 3

# 检查后端是否启动成功
if ! curl -s http://localhost:8082/api/status > /dev/null; then
    echo "❌ 后端服务启动失败"
    kill $BACKEND_PID 2>/dev/null
    exit 1
fi

echo "✅ 后端服务启动成功"

# 启动前端
echo "🌐 启动前端服务 (端口 3000)..."
cd front
npm start &
FRONTEND_PID=$!
cd ..

echo ""
echo "🎉 演示系统启动完成！"
echo ""
echo "📊 服务信息:"
echo "  - 后端 API: http://localhost:8082"
echo "  - 前端界面: http://localhost:3000"
echo ""
echo "🔗 连接说明:"
echo "  1. 打开浏览器访问 http://localhost:3000"
echo "  2. 在连接面板中输入:"
echo "     主机地址: localhost"
echo "     端口:     8082"
echo "  3. 点击 '连接' 按钮"
echo "  4. 连接成功后即可使用所有功能"
echo ""
echo "🔍 测试连接:"
echo "  curl http://localhost:8082/api/status"
echo ""
echo "⚠️  停止服务:"
echo "  - 按 Ctrl+C 停止前端"
echo "  - 运行: ./stop_demo.sh"
echo ""
echo "📝 进程ID:"
echo "  - 后端 PID: $BACKEND_PID"
echo "  - 前端 PID: $FRONTEND_PID"

# 等待前端启动
wait $FRONTEND_PID
