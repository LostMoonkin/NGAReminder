# NGA Reminder

[English](README.en.md)

NGA Reminder 用于 NGA 主题与用户监控、内容保存和通知。服务端准备使用 Go 重新实现，当前处于归档完成、详细设计待讨论的阶段。

## 仓库组成

| 目录 | 状态 | 说明 |
| --- | --- | --- |
| [`extension-standalone/`](extension-standalone/) | 独立维护 | 无需服务端的 Chromium 扩展，使用浏览器 Cookie |
| [`archive/rust-service/`](archive/rust-service/ARCHIVE.md) | 历史归档 | Rust v0.1.4 的代码、测试、迁移、部署配置、项目计划和设计文档 |

Go 服务端的工程结构、技术方案和实施计划将在后续设计讨论中确定。

## Rust 服务端归档

归档基于提交 `7a9c9dd3c7fa0d33cdadcf6829f2c8f7d0ca0d63`，并包含重构前的服务端评审报告。

- [归档索引与原目录映射](archive/rust-service/ARCHIVE.md)
- [原项目计划](archive/rust-service/PROJECT_PLAN.md)
- [原服务端说明](archive/rust-service/service/README.md)
- [服务端复杂度评审](archive/rust-service/service/docs/SERVICE_SIMPLIFICATION_REVIEW.md)

归档中的设计、里程碑和 Rust 工程约束描述历史实现，供 Go 设计参考。旧 Rust 镜像工作流已移出活动工作流目录，服务端发布入口暂停。

## Standalone 扩展

扩展保持独立运行、存储和版本发布。安装和配置见 [扩展说明](extension-standalone/README.md)。

扩展发布继续使用：

```bash
scripts/release.sh extension <x.y.z>
```

脚本创建本地提交和 `vX.Y.Z-standalone` 标签；需要推送时显式追加 `--push`。验证和发布步骤见 [扩展工作流](.github/workflows/extension-release.yml)。

## 许可证

[MIT](LICENSE)
