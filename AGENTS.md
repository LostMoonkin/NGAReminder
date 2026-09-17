# 仓库工作规范

## 项目范围

- `service/` 是 Go 服务端，按单人维护、单机运行的 side project 开发。先读 [服务端规范](service/AGENTS.md) 和 [重构计划](service/docs/plan/README.md)，只实现当前任务对应的阶段；其他文档从 [文档入口](service/docs/README.md) 按需读取。
- `extension-standalone/` 是独立 Chromium 扩展，存储、运行环境、版本和发布流程均独立于服务端。
- `archive/rust-service/` 保存 Rust v0.1.4 及其代码、测试、计划和设计。历史文档提供行为参考，不构成 Go 重构的工程约束。
- 新增和更新的项目文档使用中文，专业术语可保留英语；按需维护双语说明。

## 按任务读取

- Go 服务端：以 `service/AGENTS.md` 和当前阶段 Spec 为准；历史协议、功能和评审按 Spec 中的链接按需查阅。
- Rust 历史查询：从 [归档索引](archive/rust-service/ARCHIVE.md) 开始。只有任务明确要求修改归档时，才按 [归档规范](archive/rust-service/AGENTS.md) 开展修改。
- 扩展开发：读取 [扩展说明](extension-standalone/README.md) 和 [发布工作流](.github/workflows/extension-release.yml)。只有任务明确包含两个产品时，才将服务端改动扩展到浏览器端。

## 扩展约定

- 使用 Chrome local storage，遵守 Manifest V3 service worker 的生命周期限制。
- 保持 `manifest.json` 的独立版本及 `vX.Y.Z-standalone` 发布标签。
- 扩展改动运行发布工作流规定的检查。

## 完成检查

- 文档改动检查相对链接并运行 `git diff --check`；归档移动还需检查内容完整性。
- 旧 Rust 工作流留在归档中。Go 发布阶段完成前，活动发布脚本仅支持扩展。
- 凭据、数据库、下载资源和构建产物不进入版本控制；本地 Rust 历史运行文件保存在被忽略的 `.local/rust-service/`。
