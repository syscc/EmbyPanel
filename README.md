# EmbyPanel

EmbyPanel 是一个 Rust + Vue 实现的 Emby STRM 直链反代面板。后端同时提供管理 UI、API 和 Emby 反代服务：管理面板默认监听 `8090`，反代端口在面板中按服务器单独配置。面板会自动维护 IP 归属数据库，并在实时播放、封禁记录、播放日志和反代请求明细中显示 IP 的国家、省市、区县和运营商信息。

302 直链反代实现原理见 [docs/302-redirect.md](docs/302-redirect.md)。

## 功能

- 多 Emby 服务器配置，每个服务器可使用独立反代端口。
- STRM 直链识别和 `302` 重定向，支持 OpenList `/d/...` 路径解析为 `raw_url`。
- 直链缓存支持 TTL、最大容量、域名黑白名单过滤。
- 支持 STRM URL 映射，普通替换和 `regex:` 正则规则都可用。
- 可选内部跳转解析：先 `HEAD` 跟随跳转，再把最终 URL 返回给客户端。
- 客户端 UA 拦截、播放频率限制、IP 屏蔽、用户禁用和解除封禁。
- IP 归属数据库自动下载到持久化 `data` 目录，IP 旁展示国家、省市、区县和运营商。
- Webhook 通知，支持多个 POST JSON Webhook。
- 实时播放、播放日志、运行信息日志，支持按服务器查看。
- 运维健康检查、反代端口状态、今日请求统计、播放频率窗口状态。
- 日志关键词搜索、级别筛选、时间筛选和 CSV 导出。
- 配置文件备份/恢复，包含服务器、客户端管控、通知和日志配置。
- 配置测试校验、配置审计日志、GitHub Release 更新检查。
- `/data/logs/embypanel.log` 文件日志，支持级别、单文件大小和保留数量配置。
- SQLite 持久化配置，Docker 下数据保存在 `/data/embypanel.db`。

## 目录结构

```text
.
├── .github/workflows/docker.yml   # GitHub Actions Docker 多架构构建
├── data/
│   └── config.toml.example        # 配置模板；运行数据库和日志不提交
├── docs/
│   ├── 302-redirect.md            # 302 直链反代实现原理
│   └── changelog.md               # 版本更新日志
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

首次启动缺少 `qqwry.ipdb` 时会从项目默认上游分块下载，默认最大 `67108864` 字节。可按部署需要设置：

- `EMBYPANEL_IPDB_URL`：自定义 IPDB 下载地址，仅支持 HTTP/HTTPS。
- `EMBYPANEL_IPDB_MAX_BYTES`：允许下载的最大字节数。
- `EMBYPANEL_IPDB_SHA256`：可选的 64 位十六进制 SHA-256；设置后校验不通过不会替换本地数据库。

查看日志：

```bash
docker logs -f embypanel
```

查看健康状态：

```bash
docker ps --filter name=embypanel
curl http://127.0.0.1:8090/healthz
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

## 备份和恢复

面板提供独立“备份”页面。点击备份会生成配置文件并触发浏览器下载；点击还原会弹出本机文件选择框，选择配置文件后自动还原。还原会覆盖当前配置并自动重启全部反代服务。

备份内容包含：

- Emby 服务器配置和 API Key。
- OpenList 地址和 Token。
- 客户端管控配置、播放频率限制、Webhook 通知。
- UA 规则只备份已禁用的拦截规则；自动记录且放行的客户端不会写入备份。
- 播放频率封禁只备份仍有效的封禁；已过期或已解除的 IP/用户封禁会自动清理。
- 系统日志文件配置。

备份文件使用导出时填写的备份密码加密保存，请妥善保管密码；备份不包含面板管理员用户名、管理员密码、登录会话、运行日志文件和请求统计数据。

## 运维接口

- `GET /healthz`：公开健康检查，不需要登录，不返回敏感信息。
- `GET /api/monitoring/healthz`：面板详细健康信息。
- `GET /api/monitoring/proxy-status`：每个反代端口的监听状态、启动时间、最近请求和最近错误。
- `GET /api/monitoring/stats`：今日请求、重定向、缓存命中、拦截、错误统计。统计持久化在 SQLite，并自动清理旧数据。
- `GET /api/app-info/update-check`：登录后检查 GitHub Releases 是否有新版本。

## License

本项目基于 MIT License 开源，详见 [LICENSE](LICENSE)。
