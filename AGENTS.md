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
