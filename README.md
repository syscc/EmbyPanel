# EmbyPanel

EmbyPanel 是一个 Rust + Vue 实现的 Emby STRM 直链反代面板。后端同时提供管理 UI、API 和 Emby 反代服务：管理面板默认监听 `8090`，反代端口在面板中按服务器单独配置。

302 直链反代实现原理见 [docs/302-redirect.md](docs/302-redirect.md)。

## 功能

- 多 Emby 服务器配置，每个服务器可使用独立反代端口。
- STRM 直链识别和 `302` 重定向，支持 OpenList `/d/...` 路径解析为 `raw_url`。
- 直链缓存支持 TTL、最大容量、域名黑白名单过滤。
- 支持 STRM URL 映射，普通替换和 `regex:` 正则规则都可用。
- 可选内部跳转解析：先 `HEAD` 跟随跳转，再把最终 URL 返回给客户端。
- 客户端 UA 拦截、播放频率限制、IP 屏蔽、用户禁用和解除封禁。
- Webhook 通知，支持多个 POST JSON Webhook。
- 实时播放、播放日志、运行信息日志，支持按服务器查看。
- SQLite 持久化配置，Docker 下数据保存在 `/data/embypanel.db`。

## 目录结构

```text
.
├── .github/workflows/docker.yml   # GitHub Actions Docker 多架构构建
├── data/
│   └── config.toml.example        # 配置模板；运行数据库和日志不提交
├── docs/
│   └── 302-redirect.md            # 302 直链反代实现原理
├── frontend/
│   ├── src/                       # Vue 面板源码
│   ├── index.html
│   ├── package.json
│   └── vite.config.ts
├── src/                           # Rust 后端源码
├── Dockerfile                     # 多阶段 Docker 构建
├── docker-compose.yml             # 推荐容器部署示例
├── Cargo.toml
└── README.md
```

## Docker Compose

推荐使用 Docker Compose 运行。`8090` 是管理 UI/API 端口，`8091-8095` 预留给 Emby 反代服务器使用。

```yaml
services:
  embypanel:
    image: syscc/embypanel:latest
    environment:
      - TZ=Asia/Shanghai
      - EMBYPANEL_API_ADDR=0.0.0.0:8090
    container_name: embypanel
    restart: always
    volumes:
      - ./data:/data
    ports:
      - "8090:8090"
      - "8091-8095:8091-8095"
    network_mode: bridge
```

启动：

```bash
docker compose up -d
```

查看日志：

```bash
docker logs -f embypanel
```

停止：

```bash
docker compose down
```

面板入口：

```text
http://服务器IP:8090/ui/
```

如果需要更多反代端口，扩大 `ports` 里的范围，例如：

```yaml
ports:
  - "8090:8090"
  - "8091-8100:8091-8100"
```

然后在面板中把服务器反代端口配置到同一范围内。

## License

本项目基于 MIT License 开源，详见 [LICENSE](LICENSE)。
