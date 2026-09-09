package main

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"proxy/internal/logic"
	"proxy/pkg/logger"
	"strings"
)

// 合并自：
// - 本地扩展：--port 参数（Tauri proxy_daemon 端口冲突自动选择）
// - 上游 v2.6.2：从 stdin 读取 64hex 代理认证密钥，与 Electron proxySecret 协议对齐
func main() {
	port := flag.Int("port", 22345, "proxy server listen port")
	flag.Parse()

	logger.SetLevel(logger.INFO)
	logger.SetColor(false)

	secret, readErr := bufio.NewReader(os.Stdin).ReadString('\n')
	secret = strings.TrimSpace(secret)
	if readErr != nil {
		panic("启动服务器失败: 无法读取代理认证密钥")
	}
	if secret == "" {
		panic("启动服务器失败: 缺少代理认证密钥")
	}

	addr := fmt.Sprintf("127.0.0.1:%d", *port)
	err := logic.RunApiServer(addr, secret)
	if err != nil {
		panic("启动服务器失败: " + err.Error())
	}
}
