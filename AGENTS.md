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
- `Cargo.lock`
- `frontend/package-lock.json`
- `frontend/node_modules/`

## 当前启动方式

- 后端：`cargo run`，监听 `http://127.0.0.1:18090`，同时反代端口 `8096`、`8097`。
- 前端：`npx vite --host 0.0.0.0 --strictPort true`，监听 `http://localhost:8090/ui/`。

执行任务时优先使用源码开发模式完成验证，不要生成 release 编译产物。

## 模块复用与禁止重复造轮子

实现新功能前必须先使用 `rg` 搜索 `frontend/src/components`、`frontend/src/composables`、`frontend/src/lib` 和对应 Rust 领域模块。已有模块能覆盖需求时，优先扩展现有契约；只有出现至少两个真实复用点，或复杂逻辑已经形成清晰职责边界时，才新增共享抽象。禁止为单一调用点创建万能组件、Base 类、通用 Store 或无实际复用的 helper。

### 前端强制复用

- 普通 JSON API 请求统一使用 `frontend/src/lib/api-client.ts` 的 `createApiClient`，不要在页面或新 composable 中重复实现 token、`Authorization`、401 处理和 JSON 错误映射。文件下载、图片、流式响应等非 JSON 请求可以使用专用 `fetch`，但必须沿用现有登录失效处理和请求取消模式。
- 敏感载荷统一使用 `frontend/src/composables/usePayloadEncryption.ts` 的 `encryptPayload`。禁止在页面中自行实现 Web Crypto、RSA、AES、Base64URL 或 `node-forge` 兼容逻辑。
- 简单确认和单字段输入使用 `frontend/src/composables/useActionDialog.ts`，由 `frontend/src/components/ActionDialogHost.vue` 统一承载。删除确认、危险操作确认和密码输入不得再使用 `window.confirm`、`window.prompt` 或另写一套 Dialog。
- 复杂表单、详情和结果列表弹窗使用 `frontend/src/components/ui/SettingsDialogShell.vue`，复用其标题、描述、遮罩、关闭按钮、滚动正文和固定 footer。不要在页面中重复拼装 `DialogRoot`、`DialogPortal`、`DialogOverlay`、`DialogContent`；不要把复杂内容强塞进 `ActionDialogHost`，也不要嵌套卡片式弹窗。
- 二元状态开关使用 `frontend/src/components/ui/UiSwitch.vue`；独立复选项使用 `frontend/src/components/ui/CheckboxField.vue`。禁止页面自行复制 track/knob、`role="switch"`、`aria-checked` 或 `.check` 模板。
- 用户策略表单复用 `frontend/src/components/UserPolicyFields.vue`；策略默认值、用户摘要转换、深拷贝、提交归一化和模板合并统一使用 `frontend/src/lib/user-policy.ts` 的 `defaultPolicyDraft`、`policyFromUser`、`copyPolicy`、`policyPayload`、`applyTemplateTo`。新增策略字段时必须同步类型、这些 helper、表单和后端契约，禁止在页面中再写一套策略转换逻辑。
- 用户与模板 CRUD 优先扩展 `frontend/src/composables/useUsersController.ts`；备份导入导出优先扩展 `frontend/src/composables/useBackupController.ts`。页面只负责交互编排，不重复实现同一套 API、加密、loading 和错误状态。
- 时间、容量、播放进度、URL 和请求状态格式化复用 `frontend/src/lib/panel-formatters.ts`。通用 class 合并使用 `frontend/src/lib/utils.ts` 的 `cn`。禁止各页面复制格式化函数导致显示规则分叉。
- 所有用户可见文案通过现有 `t()` 和 `frontend/src/lib/translations.ts` 管理；新增中文键时同时补英文映射。禁止在同一功能中混用硬编码文案和翻译键。

### 后端强制复用

- Emby 数据查询和管理能力集中在 `src/emby.rs`。新增普通 Emby API 调用时在该模块内复用 `emby_api_url`、`send_checked` 和已有领域函数，其他 handler 调用公开领域函数；禁止在 API handler 中复制 API Key、状态码检查和 JSON 解包。反代转发、媒体图片或流式响应属于传输通道，可保留在 `proxy.rs`、`main.rs`、`monitoring.rs` 的专用路径，不要强行套用 JSON helper。
- 鉴权统一使用 `src/auth.rs` 的 `require_auth` 或 `require_auth_user_id`，禁止 handler 自行解析 Bearer token、查询 session 或实现管理员判断。
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
