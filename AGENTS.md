# EmbyPanel Agent Instructions

你正在维护当前项目根目录 `./`。

## 项目运行约束

项目当前处于测试/开发阶段，必须遵守以下约束：

1. 不要执行 `cargo build --release`。
2. 不要打包、分发或运行 release 二进制。
3. 不要依赖 `target/release/emby302gateway-rs`。
4. 后端按源码开发模式运行：使用 `./data/embypanel-dev.sh -r` 重启服务，脚本内部通过 `cargo run` 启动后端。
5. 允许 Cargo 生成 `target/debug` 增量编译产物，这是 Rust 开发运行必需的。
6. 前端使用 Vite 开发服务运行，访问地址为 `http://localhost:8090/ui/`。
7. 后端源码修改后，重启 `./data/embypanel-dev.sh -r` 即可运行最新代码。
8. 前端源码修改通常由 Vite 热更新，不需要重启。
9. 启动脚本和文档不要依赖机器上的绝对项目路径；默认从项目根目录启动，使用 `./data/embypanel-dev.sh -r`。
10. 脚本内如需引用项目目录，基于脚本位置运行时推导，不写入 `/Users/.../EmbyPanel` 这类机器绝对路径。

## 清理规则

可以删除：

- `target/`
- `frontend/dist/`
- `frontend/tsconfig.tsbuildinfo`
- `data/backend.log`
- `data/frontend.log`
- `data/embypanel-local.log`
- `data/logs/`
- `data/backend.pid`
- `data/frontend.pid`
- `.DS_Store`
- `data/.DS_Store`
- `frontend/.DS_Store`

不要删除：

- `data/config.toml`
- `data/config.toml.example`
- `data/embypanel.db`
- `data/qqwry.ipdb`
- `data/embypanel-dev.sh`
- `data/clean-target.sh`
- `Cargo.lock`
- `frontend/package-lock.json`
- `frontend/node_modules/`

## 当前启动方式

- 后端：`cargo run`，监听 `http://127.0.0.1:18090`，同时反代端口 `8096`、`8097`。
- 前端：`npx vite --host 0.0.0.0 --strictPort true`，监听 `http://localhost:8090/ui/`。

执行任务时优先使用源码开发模式完成验证，不要生成 release 编译产物。

## 编译成功后清理

`data/clean-target.sh` 与 `data/embypanel-dev.sh` 是**本地开发工具，不在版本库**（`.gitignore` 忽略 `data/*`，只保留 `data/config.toml.example`）。新克隆的环境里没有这两个脚本，此时应直接跳过清理而不是报错。

每次源码重新编译成功后，有该脚本时必须清理上一轮编译遗留的过时产物，避免 `target/debug` 无限膨胀。

执行时机：

- `cargo check`、`cargo test`、`cargo build` 等验证命令**成功结束后**再清理，且必须等所有 cargo / rustc 进程退出、最后一次编译完成之后再清理。
- 编译失败时不清理。
- 禁止与编译并行清理。清理脚本自身会先取 `target/debug/.cargo-lock` 的独占锁，遇到正在编译的 cargo 会直接跳过，并把原因写到 stderr。
- 开发模式 `./data/embypanel-dev.sh -r` 启动成功后脚本已自动调用一次清理，不需要再手动重复执行。

常用命令：

```bash
./data/clean-target.sh            # 清理并报告释放的空间
./data/clean-target.sh --dry-run  # 只列出计划删除的内容，不做任何改动
./data/clean-target.sh --quiet    # 只清理，不打印逐项报告
./data/clean-target.sh --help     # 用法说明
```

清理范围与保留范围：

- 只处理本项目 `target/debug/deps` 下 `emby302gateway_rs-<16位hash>` 形式的本 crate 可执行文件、该 crate 的 `*.rcgu.o` 对象文件，以及 `target/debug/incremental` 下 `emby302gateway_rs-<哈希>` 形式的本 crate 增量目录；不碰依赖产物。
- 只处理在 `target/debug/.fingerprint/emby302gateway-rs-<hash>` 下存在 `bin-emby302gateway-rs` 或 `test-bin-emby302gateway-rs` 成功标记的编译单元；没有成功标记的一律保留。
- 始终保留：最新的普通运行二进制和最新的测试二进制各一套；与 `target/debug/emby302gateway-rs` 同文件（硬链接）或内容相同的 `deps` 副本；依赖的 `lib*.rlib` / `*.rmeta`；`target/debug/emby302gateway-rs`；`target/debug/build`；`target/debug/.fingerprint`。
- 顶层程序 `target/debug/emby302gateway-rs` 本身是软链接时，脚本按它**指向的真实文件**取 inode，并把该 inode 与同内容副本纳入保留集合，不会删掉软链接目标后留下悬空入口。
- 对象文件只在对应二进制被本次清理删除、对象文件时间不晚于该成功二进制、且已不被任何保留二进制引用时才删除。macOS 默认 `split-debuginfo=unpacked` 时 DWARF 调试信息保存在 `*.rcgu.o` 内，脚本用 `dsymutil --dump-debug-map` 读取保留二进制的 debug map，被引用的对象文件一律保留；`dsymutil` 报告无法打开的对象文件同样受保护。
- debug map 读取有 120 秒超时；超时、`dsymutil` 缺失，或输出的格式不被识别（stderr 会打印 `debug map output not understood (unsupported dsymutil format)`）时，脚本**保守降级**：保留当前二进制的整组对象文件，并且对本次要删除的二进制，只回收既**早于 48 小时**、又不晚于对应二进制时间的对象文件。48 小时保守窗口是降级时的兜底判据，48 小时内的新对象一律保留。
- 降级与跳过的原因始终写 stderr，不受 `--quiet` 影响；`--quiet` 只抑制逐项报告。否则 `./data/embypanel-dev.sh -r` 里的静默调用会让清理失效而无任何痕迹。
- 不保证清空所有过时对象文件：归属或时间关系无法确认的对象文件一律保留。
- 增量缓存不是整目录删除：`target/debug/incremental` 下**最多保留 2 个** `emby302gateway_rs-<哈希>` 目录——按 mtime 最新的一个，加上当前运行程序所属编译单元的一个（同一目录不重复计）——每个保留目录内再保留**最新的一个** `s-*` session 目录，更旧的增量目录与更旧的 session 目录会被删除以回收空间。额外保留当前编译单元的目录，是为了保证该单元下次编译仍走增量缓存；否则它会退化为全量重编该单元（结果正确，只是更慢）。
- `target/debug/incremental` 下不符合 `emby302gateway_rs-<哈希>` 形式的目录（例如其他 crate 的目录）一律跳过，并在清理报告里说明。
- 清理范围不得离开 `target/debug`：`target/debug/deps`、`.fingerprint`、`incremental` 如果是指向「项目内但 `target/debug` 之外」位置（例如 `target/build`）的软链接，脚本拒绝清理并说明原因，而不是跟着软链接删到别处。
- `deps` 里的 `*.rcgu.o` 与 `incremental` session 内的对象文件是同一 inode 的硬链接：只删 `deps` 副本不会释放磁盘。脚本先删旧增量目录/session，再删计划内的 `deps` 对象文件与过时可执行文件。
- 报告里的 freed 是**真实释放量**：计划内的 `deps` 副本照常删除，但**计数**时只有 inode 的最后一个硬链接被删掉才计入 freed（因此可能为 0，这是真实结果而不是统计错误）。
- 脚本以退出码 0 表示成功，删除失败会打印错误并返回非 0；清理失败不影响编译结果与运行中的服务。

## 模块复用与禁止重复造轮子

实现新功能前必须先使用 `rg` 搜索 `frontend/src/components`、`frontend/src/composables`、`frontend/src/lib` 和对应 Rust 领域模块。已有模块能覆盖需求时，优先扩展现有契约；只有出现至少两个真实复用点，或复杂逻辑已经形成清晰职责边界时，才新增共享抽象。禁止为单一调用点创建万能组件、Base 类、通用 Store 或无实际复用的 helper。

### 前端强制复用

- 普通 JSON API 请求统一使用 `frontend/src/lib/api-client.ts` 的 `createApiClient`，不要在页面或新 composable 中重复实现 token、`Authorization`、401 处理和 JSON 错误映射。文件下载、图片、流式响应等非 JSON 请求可以使用专用 `fetch`，但必须沿用现有登录失效处理和请求取消模式。
- 敏感载荷统一使用 `frontend/src/composables/usePayloadEncryption.ts` 的 `encryptPayload`。禁止在页面中自行实现 Web Crypto、RSA、AES、Base64URL 或 `node-forge` 兼容逻辑。
- 简单确认和单字段输入使用 `frontend/src/composables/useActionDialog.ts`，由 `frontend/src/components/ActionDialogHost.vue` 统一承载。删除确认、危险操作确认和密码输入不得再使用 `window.confirm`、`window.prompt` 或另写一套 Dialog。
- 复杂表单、详情和结果列表弹窗使用 `frontend/src/components/ui/SettingsDialogShell.vue`，复用其标题、描述、遮罩、关闭按钮、滚动正文和固定 footer。不要在页面中重复拼装 `DialogRoot`、`DialogPortal`、`DialogOverlay`、`DialogContent`；不要把复杂内容强塞进 `ActionDialogHost`，也不要嵌套卡片式弹窗。
- 二元状态开关使用 `frontend/src/components/ui/UiSwitch.vue`；独立复选项使用 `frontend/src/components/ui/CheckboxField.vue`。禁止页面自行复制 track/knob、`role="switch"`、`aria-checked` 或 `.check` 模板。
- 数据刷新按钮统一使用 `frontend/src/components/ui/RefreshButton.vue`，传入原 `action`、`loading` 和 `disabled` 条件，文案使用 `t('刷新')` / `t('刷新中')`；重试、测试和检查更新不混入数据刷新。
- 用户策略表单复用 `frontend/src/components/UserPolicyFields.vue`；策略默认值、用户摘要转换、深拷贝、提交归一化和模板合并统一使用 `frontend/src/lib/user-policy.ts` 的 `defaultPolicyDraft`、`policyFromUser`、`copyPolicy`、`policyPayload`、`applyTemplateTo`。新增策略字段时必须同步类型、这些 helper、表单和后端契约，禁止在页面中再写一套策略转换逻辑。
- 用户与模板 CRUD 优先扩展 `frontend/src/composables/useUsersController.ts`；备份导入导出优先扩展 `frontend/src/composables/useBackupController.ts`。页面只负责交互编排，不重复实现同一套 API、加密、loading 和错误状态。
- 只需要服务器 ID/名称的页面复用 `usePanelController.ts` 的 `ensureServerOptions` 和 `serverOptions`，通过 `/api/servers/options` 获取授权选项；不要为了填充筛选器依赖全局配置读取权限。播放控制按钮同时遵循响应中的 `can_control_sessions`，后端继续强制校验 `sessions.control`。
- 时间、容量、播放进度、URL 和请求状态格式化复用 `frontend/src/lib/panel-formatters.ts`；`formatTimestampMs` 的可选第三参数为 `Intl.DateTimeFormatOptions`，诊断时间使用毫秒精度和时区，其他调用保持默认格式。日志关联 ID 使用同模块 `logRequestId` 从受限前缀提取并独立显示，URL 展示/复制继续复用 `formatLogDetail` / `copyableLogDetail`，不将关联 ID 拼回 URL。通用 class 合并使用 `frontend/src/lib/utils.ts` 的 `cn`。禁止各页面复制格式化函数导致显示规则分叉。
- 链路诊断前端统一扩展 `useDiagnosticsController.ts` 与 `DiagnosticsPage.vue`；单条分享和当前页导出使用安全字段白名单，移除用户、IP、设备标识与自由文本路径。关联日志跳转复用 `usePanelController.ts` 的 `openDiagnosticLogs`，带入关联 ID 和服务器，不复制日志查询或放宽授权。日志页服务器选项复用 `serverOptions`；日志配置依 `/api/profile` 的 `can_manage_log_config` 能力独立加载，缺失字段按无权限处理，不阻塞日志读取，后端继续要求超级管理员会话。
- 播放日志分组只在 `LogsPage.vue` 对 `visibleActivityLogRows` 做显示折叠：仅合并有效 `playback_group_id` 加服务器 ID 下的 success/info 播放记录；warn/error、缺少标识和旧记录保持独立。使用稳定分组 key 和原生 details 保留轮询展开状态，展开保留逐请求详情与复制入口。数量明确区分显示条目和当前已加载匹配的原始请求，加载更多、CSV、请求统计与诊断始终沿用原始记录语义，不做前端散列或按标题/IP/时间窗猜测。
- 反代请求视图默认不再重复列出播放请求：`usePanelController.ts` 只在反代请求视图且请求类型筛选为「全部请求」时向 `/api/monitoring/request-details` 发送 `exclude_path_type=video_stream`；明确选择「视频流」或其它具体类型时不发送，保证诊断深链（`path_type=video_stream` + 关联 ID 关键词）仍能查到原始行，下拉框的「视频流」选项保留。拦截日志视图走独立端点，不发送该参数，被拦截的播放请求照常显示。计数沿用实际展示行数，原始行不删除、不合并，播放日志分组、CSV、诊断与请求计数语义不变。
- 所有用户可见文案通过现有 `t()` 和 `frontend/src/lib/translations.ts` 管理；新增中文键时同时补英文映射。禁止在同一功能中混用硬编码文案和翻译键。

### 全站字体规范

- 字体角色统一复用 `frontend/src/style.css` 的 `--font-size-*`：独立页标题 page22、面板 panel17、弹窗 dialog16、分区 section14、正文 body13、操作 control12、字段 label11/700、元数据 meta11、帮助 help12（单位 px）。只有现有独立页标题使用 page，不为普通面板新增标题；移动端不降低页/面板标题。
- 正文行高 1.6，标题 1.3/1.4；字重使用 400/600/700。字段标签与输入正文分离，Checkbox 正文保持 13/400；Portal 菜单/弹窗同样接入角色，不依赖页面祖先继承。
- 不修改 html/root 字号，保留默认 16px 的 rem 尺度；≤700px 输入框/select/textarea 保留 16px，scoped CSS 不得压回小字号。导航、账户和语言菜单为 13/600；RefreshButton 的 36px 高度、8px 间距、12px 字号和 UiSwitch 契约不变。
- 品牌标题（登录 27/移动24）、指标大数字、P2LineChart 刻度、代码等宽与复制/换行规则属于例外，保留专用尺寸；新增局部字体规则应使用角色变量，不复制另一套字号体系。

### 后端强制复用

- Emby 数据查询和管理能力集中在 `src/emby.rs`。新增普通 Emby API 调用时在该模块内复用 `emby_api_url`、`send_checked` 和已有领域函数，其他 handler 调用公开领域函数；禁止在 API handler 中复制 API Key、状态码检查和 JSON 解包。反代转发、媒体图片或流式响应属于传输通道，可保留在 `proxy.rs`、`main.rs`、`monitoring.rs` 的专用路径，不要强行套用 JSON helper。
- 鉴权与授权统一使用 `src/auth.rs` 的 `require_auth_context`、`require_permission`、`require_server_filter` 和 `require_server_access`，禁止 handler 自行解析 Bearer token、查询 session 或实现权限判断；认证失败返回 401，授权失败返回 403。全局设置写入、备份导入导出、Key 查看要求 `require_super_admin_session`；常规配置读取依 `settings.read` 和服务器授权过滤。
- 会话读取使用 `monitoring.read`，停止播放和发送消息使用 `sessions.control`。API Token 签发使用 `SettingsStore::create_admin_api_token_from_session`，在同一事务中验证来源会话和当前权限，禁止管理接口调用无来源校验的测试/内部签发入口。
- 高频代理/播放统计写入统一经过 `TelemetryWriteQueue`，禁止在 handler 请求路径中同步写入。
- 播放活动日志的可选媒体 DTO 复用 `activity_log::PlaybackMedia`，由 `ActivityLogStore::record_playback` 统一脱敏；Name/SeriesName 使用 `diagnostics::sanitize_media_label` 保留自然语言标题并清理明确凭据，ID 继续使用严格清洗。季集号仅取 Type=Episode，Type 缺失且有非空 SeriesName 时兼容旧响应；显式 Audio/Movie 不映射碟/曲号。视频流标签复用既有 Items 结果；generic 确认 GET 播放入口复用 `main.rs` 的有界媒体缓存与短超时合并补查，直链缓存命中不额外探测，不从媒体源 Name 或路径猜片名。回源播放的 `request_path` / `range` 同样由存储边界清洗并追加到 detail：路径去查询串且限长，Range 只保留受限数字区间，非法值写固定标记；缺失不伪造。每个 HTTP 请求各记一次，不按媒体名称吞掉不同 Range 请求，直链 detail 保持纯 URL 契约。可选 `playback_group_id` 只供页面折叠，复用 `playback_media_key` 的服务器/config/item 分区及明确会话、设备、源、UserId/认证身份，用版本域和长度前缀 SHA256 生成 `play-` 加 64 位小写十六进制；存储拒绝非法格式。显示用户名不参与分组输入，名称补查失败或恢复不会拆散同一播放，每条记录仍保留各自用户名。仅 GET 已确认播放入口可生成，单项身份限 1024 字节并拒绝空白/控制字符，缺少必需上下文不生成。不输出原始会话/设备/凭据或中间摘要，不增加查询，不用于鉴权，不改变原始记录、CSV、计数及诊断。
- 链路诊断的上下文、结果、缓存、路径与耗时由 `PlaybackDiagnosisTrace` 采集，通过 `src/diagnostics.rs` 的 DTO 与脱敏边界输出。`started_at_ms` / `finished_at_ms` 为 Unix 毫秒时间戳，基于起点时钟与单调时钟偏移；记录 `duration_ms` 为整段诊断耗时，未归入步骤的时间按真实操作区间并集计算。缓存步骤耗时为多次缓存操作累计，不可由步骤时长累加伪造总耗时；旧数据和未采集的网络分段保留空值。持久化统一经过 `TelemetryWriteQueue::diagnosis_record` 和 `SettingsStore`，保留 7 天或最近 10000 条；授权筛选先于分页与计数，清空须排除旧队列写入复活。结果 302 不表示播放成功。详情见 `docs/diagnostics.md`。
- 名称解析与连接建立观测复用 `src/diagnostics.rs::network` 的逐次请求作用域、系统 GAI resolver 适配与 reqwest 连接器薄层。用户解析及播放策略等多请求步骤通过 `observe_operation` 汇总内部各次 `observe_send` 的网络数值，步骤 observation 只接收这一次汇总，不重复合并局部副本；不传播可选子查询的状态码和错误。播放诊断与 PlaybackInfo 流地址生成复用 `src/main.rs` 的 `playback_context_value`、`playback_device_name` 和 `playback_device_id`，读取明确的 `Client`、`Device`/`DeviceName`、`DeviceId`、`UserId` 及对应 `X-Emby-*` 查询参数、请求头；`Authorization` 与 Emby 专用授权头兼容 `MediaBrowser`/`Emby` 前缀。生成流地址只从请求头补充这些上下文字段，不复制 Token/API Key，保留原查询鉴权参数的现有传递行为。只保存已完成事件的数值汇总和固定状态；连接池后台事件必须显式继承原发送作用域，发送快照关闭后不得晚到污染。两项可能重叠，不计入诊断总耗时；不推断响应使用了新连接，不从总耗时倒推未提供的网络分段。前端类型、详情、分享白名单与 `docs/diagnostics.md` 同步扩展。
- 加密请求统一使用 `src/crypto_api.rs` 的 `EncryptedRequest` 和 `CryptoKeys::decrypt_named`。禁止新增自定义 RSA/AES 解密、字段扫描或 Base64URL 解码实现。
- 错误统一返回 `src/error.rs` 的 `AppResult`/`AppError`；写日志和审计前使用 `AppError::safe_log_message` 或 `safe_error_message`。禁止把 reqwest 原始错误、凭证、token、API Key、密码或完整敏感请求写进响应和日志。
- JSON 配置持久化统一使用 `src/db.rs` 的 `SettingsStore::load_setting_json` / `save_setting_json`，管理员操作审计使用 `record_audit` 或 `record_audit_best_effort`。禁止业务模块自行打开 SQLite 连接、创建新的 settings 表或复制审计插入 SQL。
- 多服务器监控查询应保留 `src/monitoring.rs` 现有的有界并发和配置顺序语义，优先扩展 `collect_server_queries` 所在模式；禁止无上限 `tokio::spawn` 或返回顺序随机化。

### 不可强行合并的边界

- `SettingsStore::record_audit*` 是持久化管理员操作审计；`ActivityLogStore` 是运行时结构化活动/播放日志；`FileLogStore` 是系统文件日志；`BlockLogStore` 是请求拦截与解封记录。四者保留各自 retention、字段和消费者，禁止互相替代或合并成万能日志模块。
- `ActionDialogHost` 只处理 confirm/prompt；`SettingsDialogShell` 承载复杂内容。两者统一视觉但不合并职责。
- `UiSwitch` 表示立即切换的二元状态；`CheckboxField` 表示表单选项。不得为了“统一”而破坏原生表单语义。
- `UserPolicyFields`、`useUsersController`、`useBackupController` 是明确的领域模块，不要并入臃肿的全局组件或继续堆回 `usePanelController.ts`。
- 普通 JSON API、二进制下载、图片和代理流量使用不同传输契约；只复用共同的认证与错误边界，不创建能处理所有响应类型的万能请求函数。

### 修改与 review 检查

- 新增组件、composable、helper 或 Rust 模块前，先说明现有模块为何不能满足；若只是参数或 slot 差异，优先扩展现有模块。
- 修改共享模块后，检查所有调用点的 props、返回类型、焦点/ARIA、loading、错误和取消语义，并按影响范围补充源码模式验证。
- review 时重点搜索重复的 `DialogRoot`、自制 switch/checkbox、`window.confirm`/`window.prompt`、页面内加密、普通 JSON raw `fetch`、handler 内 Emby raw `reqwest`、自建鉴权和直接 SQLite settings SQL。
- 新增了真正可复用的模块或改变共享契约时，同步更新本节清单，避免文档与代码再次分叉。

## 版本发布提示词

当需要整理本次修改并发布新版本时，可以使用以下提示词：

```text
把本次修改整理成新版本发布到 GitHub。

要求：
1. 先检查当前 git 状态和最新 tag。
2. 版本号在当前最新 tag 基础上加 1；补丁号小于 9 时递增补丁号，例如 v0.1.3 -> v0.1.4；补丁号为 9 时进位到下一 minor 并把补丁号归 0，例如 v0.1.9 -> v0.2.0。
3. 更新 Cargo.toml / Cargo.lock 里的版本号。
4. 把本次修改内容写入 docs/changelog.md 顶部，包含新增、优化、修复和验证项。
5. 不要提交 data 目录、日志文件、数据库、IP 库、本地测试脚本或编译产物。
6. 按源码模式检查，不要打包二进制：
   - cargo fmt
   - cargo check
   - cd frontend && npx vue-tsc -b --noEmit
7. 检查通过后提交，提交信息用 Release vX.X.X。
8. 创建同名 tag，例如 vX.X.X。
9. 推送 main 和 tag 到 GitHub。
10. 最后告诉我提交号、tag、推送结果和检查结果。
```
