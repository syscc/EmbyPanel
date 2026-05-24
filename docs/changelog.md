# 版本更新日志

## v0.0.2

发布时间：2026-05-24

### 修复

- 修复客户端 UA 拦截误命中问题。
  - 之前 UA 拦截会同时参考 `X-Emby-Client` 客户端名，可能出现规则 `infuse-library` 误拦 `Infuse-Direct` 类型客户端。
  - 现在 UA 拦截只按实际 `User-Agent` 做大小写不敏感关键词匹配。
  - 示例：规则 `infuse-library` 只会命中包含 `Infuse-Library` 的 UA，不会仅因为客户端类型是 `Infuse-Direct` 而拦截。

- 修复容器日志时间显示不易阅读的问题。
  - 之前 tracing 默认输出 UTC RFC3339，例如 `2026-05-24T03:06:09.524129Z`。
  - 现在按 `TZ` 环境变量输出 24 小时制本地时间。
  - Docker 默认 `TZ=Asia/Shanghai` 时，日志格式为 `2026-05-24 11:06:09.524`。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo build --release --locked`

## v0.0.1

发布时间：2026-05-24

### 首个版本

- Rust 后端 + Vue 前端管理面板。
- 多 Emby 服务器反代端口配置。
- STRM 直链识别和 HTTP 302 重定向。
- OpenList `/d/...` 路径解析为 `raw_url`。
- 直链缓存、缓存容量限制、缓存域名黑白名单。
- STRM URL 映射，支持普通替换和 `regex:` 正则规则。
- 内部 HEAD 跳转解析。
- 客户端 UA 拦截、播放频率限制、IP 屏蔽、用户禁用和解除封禁。
- Webhook 命中通知。
- 实时播放、播放日志、运行信息日志。
- Docker Compose 部署和 GitHub Actions 多架构 Docker 构建。
