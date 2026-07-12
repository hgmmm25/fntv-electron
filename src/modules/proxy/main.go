package main

import (
	"flag"
	"fmt"
	"proxy/internal/logic"
	"proxy/pkg/logger"
)

func main() {
	port := flag.Int("port", 22345, "Proxy server listen port")
	flag.Parse()

	logger.SetLevel(logger.INFO)
	logger.SetColor(false)

	addr := fmt.Sprintf("127.0.0.1:%d", *port)
	err := logic.RunApiServer(addr)
	if err != nil {
		panic("启动服务器失败: " + err.Error())
	}
}
