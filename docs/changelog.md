# 版本更新日志

## v0.0.8

发布时间：2026-05-24

### 调整

- 版本检查入口调整为左侧品牌区版本号点击触发。
- 移除本地更新检查测试开关，恢复正常 GitHub Release 检查流程。
- 右上角不再显示版本检查按钮，避免重复展示。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cd frontend && npm run build`
- `cargo build --release --offline`

## v0.0.7

发布时间：2026-05-24

### 新增

- 新增服务器连通性定时巡检，覆盖 Emby、OpenList 和反代端口。
- 新增首页运维状态合并展示，统一显示监听、巡检、异常和自动重启信息。
- 新增巡检失败后的自动重启配置，可设置连续无响应多久后自动重启反代进程。

### 优化

- 健康检查和反代监听状态合并为一组服务器状态展示，减少重复信息。
- 连通性异常会写入日志并支持 webhook 通知。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cd frontend && npm run build`
- `cargo build --release --locked`

## v0.0.6

发布时间：2026-05-24

### 新增

- 新增公开健康检查 `/healthz` 和面板详细健康检查。
- 新增反代端口状态，显示监听状态、启动时间、最近请求和最近错误。
- 新增请求统计，按服务器持久化保存今日请求、重定向、缓存命中、拦截和错误数，并自动清理旧数据。
- 新增配置文件备份和还原，还原后自动刷新配置并重启反代服务。
- 新增配置测试校验，可检测本地端口、Emby API Key 和连接状态。
- 新增 GitHub Releases 更新检查。
- 新增配置审计日志，记录服务器配置、客户端规则、通知、账户、备份恢复等管理操作。
- 新增日志文件写入 `/data/logs/embypanel.log`，支持日志级别、单文件大小和保留数量配置。
- 新增日志关键词、级别、时间范围筛选和 CSV 导出。
- 新增播放频率限制窗口可视化，显示 IP 当前次数、阈值、剩余次数、重置时间和封禁状态。
- 备份还原调整为独立页面，点击备份自动下载配置文件，点击还原弹出本机文件选择框，备份范围明确排除面板管理员用户名和密码。
- 备份客户端 UA 规则时只保留已禁用的拦截规则，自动记录且放行的客户端不再写入备份。
- 播放频率封禁记录改为自动清理，已过期或已解除的 IP/用户封禁不再保留到数据库和备份中。

### 优化

- CSV 日志导出改为带登录态下载，避免直接打开导出接口导致 401。
- 配置测试时当前服务已经监听的反代端口不再误报为端口占用。
- 代理 Emby 失败会记录到端口状态和今日错误统计。
- 首页新增运维概览，服务器页新增配置测试，日志页新增文件日志配置，账户页新增审计日志。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cd frontend && npm run build`
- `cargo build --release --locked`

## v0.0.5

发布时间：2026-05-24

### 新增

- Docker 启动日志增加项目名称、项目地址、项目版本号和管理 UI 地址。
- 新增 `/api/app-info` 接口，前端左上角版本号从后端读取，不再硬编码。
- Docker 构建时自动把 Git tag 注入容器版本号，例如 `v0.0.5`。

### 优化

- 服务器配置里的 `Emby API Key` 默认隐藏，改为眼睛按钮切换显示/隐藏。
- 管理员登录后，服务器配置页面支持查看已保存的 `Emby API Key`。
- 源码运行时版本号与 `Cargo.toml` 保持一致。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cd frontend && npm run build`
- `cargo build --release --locked`

## v0.0.4

发布时间：2026-05-24

### 修复

- 过期面板登录态请求不再写入容器运行日志。
  - 旧浏览器页面、旧手机页面或未刷新的面板标签页可能继续用失效 token 轮询接口。
  - 这类请求属于正常鉴权拒绝，不影响 Emby 反代和播放，不再输出 `invalid or expired session` 日志。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## v0.0.3

发布时间：2026-05-24

### 修复

- 修复 Docker 容器重启后旧页面继续轮询导致 `invalid or expired session` 反复刷错误日志的问题。
  - 面板接口返回登录过期后，前端会自动清理本地登录状态、停止轮询并回到登录页。
  - 后端将登录过期类请求从 `ERROR` 降为 `INFO`，避免正常会话失效刷屏污染错误日志。

- 修复容器重启后旧页面可能使用旧 RSA 公钥提交登录或配置请求，导致 `failed to decrypt request key: decryption error` 的问题。
  - 前端每次提交加密请求前都会重新获取当前后端公钥。
  - `/api/public-key` 返回 `Cache-Control: no-store`，避免浏览器或中间代理缓存旧公钥。
  - 解密失败这类请求格式问题调整为 `WARN` 日志。

### 验证

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cd frontend && npm run build`
- `cargo build --release --locked`

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
