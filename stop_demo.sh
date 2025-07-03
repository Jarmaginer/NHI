#!/bin/bash

# NHI 演示停止脚本
# 停止所有相关服务

echo "🛑 停止 NHI 演示系统..."

# 停止前端服务
echo "🌐 停止前端服务..."
pkill -f "npm.*start" 2>/dev/null
pkill -f "react-scripts" 2>/dev/null

# 停止后端服务
echo "🔧 停止后端服务..."
sudo pkill -f "nhi.*http-port" 2>/dev/null

# 等待进程完全停止
sleep 2

# 检查是否还有残留进程
REMAINING_FRONTEND=$(pgrep -f "npm.*start" 2>/dev/null)
REMAINING_BACKEND=$(pgrep -f "nhi.*http-port" 2>/dev/null)

if [ -n "$REMAINING_FRONTEND" ]; then
    echo "⚠️  强制停止前端进程: $REMAINING_FRONTEND"
    kill -9 $REMAINING_FRONTEND 2>/dev/null
fi

if [ -n "$REMAINING_BACKEND" ]; then
    echo "⚠️  强制停止后端进程: $REMAINING_BACKEND"
    sudo kill -9 $REMAINING_BACKEND 2>/dev/null
fi

# 检查端口占用
PORT_8082=$(lsof -ti:8082 2>/dev/null)
PORT_3000=$(lsof -ti:3000 2>/dev/null)

if [ -n "$PORT_8082" ]; then
    echo "⚠️  端口 8082 仍被占用，进程: $PORT_8082"
    sudo kill -9 $PORT_8082 2>/dev/null
fi

if [ -n "$PORT_3000" ]; then
    echo "⚠️  端口 3000 仍被占用，进程: $PORT_3000"
    kill -9 $PORT_3000 2>/dev/null
fi

echo "✅ 演示系统已停止"
echo ""
echo "📊 端口状态检查:"
echo "  - 端口 8082: $(lsof -ti:8082 2>/dev/null || echo '空闲')"
echo "  - 端口 3000: $(lsof -ti:3000 2>/dev/null || echo '空闲')"
