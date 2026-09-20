# NGA Reminder

[English](README.en.md)

NGA Reminder 用于 NGA 主题与用户监控、内容保存和通知。服务端正在使用 Go 重构，已完成运行基础、NGA 账号配置、TID/UID 监控、自动调度与免拉取、Bark/飞书通知与收件箱、飞书 Bot、Cookie 续期、手动采集、富文本浏览、Markdown/ZIP 导出、资源维护、UID 历史回填和楼层补偿；其余业务功能按阶段继续实现。

## 仓库组成

| 目录 | 状态 | 说明 |
| --- | --- | --- |
| [`service/`](service/docs/README.md) | 阶段 01～09 已完成 | Go 服务端，使用 Gin、GORM、纯 Go SQLite 和 zerolog，单机运行、禁用 CGO |
| [`extension-standalone/`](extension-standalone/) | 独立维护 | 无需服务端的 Chromium 扩展，使用浏览器 Cookie |
| [`archive/rust-service/`](archive/rust-service/ARCHIVE.md) | 历史归档 | Rust v0.1.4 的代码、测试、迁移、部署配置、项目计划和设计文档 |

Go 服务端从 [开发规范](service/AGENTS.md) 和 [重构计划与阶段 Spec](service/docs/plan/README.md) 开始。文档统一在 `service/docs/` 下按 `plan/`、`spec/`、`ticket/` 维护。重构保留当前在用功能，移除运行时 PostgreSQL 和多实例设计；现有部署按“Rust 停机 → PG 迁移 SQLite → 核验 → Go 上线”切换，见 [数据迁移 Spec](service/docs/spec/09-data-migration.md)。详细实现随阶段确定，暂不拆 tickets。

本地启动和配置说明见 [服务端文档](service/docs/README.md#本地运行)，需要 Go 1.27.0 或更新版本，构建和自测均使用 `CGO_ENABLED=0`。

PG 迁移使用 [migrate-pg.sh](service/migrate-pg.sh)，输出完整表数据快照、Go SQLite 文件和核验报告；assets 继续挂载原目录。参数与切换步骤见[迁移操作说明](service/docs/plan/migration-runbook.md)。实际旧库迁移与上线留待部署时执行。

容器部署见 [Docker Compose 说明](service/docs/README.md#docker-compose-部署)：复制 [Compose 模板](service/compose.example.yaml)，直接填写 `environment`，只挂载数据目录。

[服务端镜像工作流](.github/workflows/service-image.yml) 将 Go 镜像发布到 `ghcr.io/<owner>/<repo>`，支持 `main` 服务端变更、服务端版本标签和手动运行；标签及权限说明见 [GHCR 镜像发布](service/docs/README.md#ghcr-镜像发布)。

## Rust 服务端归档

归档基于提交 `7a9c9dd3c7fa0d33cdadcf6829f2c8f7d0ca0d63`，并包含重构前的服务端评审报告。

- [归档索引与原目录映射](archive/rust-service/ARCHIVE.md)
- [原项目计划](archive/rust-service/PROJECT_PLAN.md)
- [原服务端说明](archive/rust-service/service/README.md)
- [服务端复杂度评审](archive/rust-service/service/docs/SERVICE_SIMPLIFICATION_REVIEW.md)

归档中的设计、里程碑和 Rust 工程约束描述历史实现，供 Go 设计参考。旧 Rust 镜像工作流保留在归档中，活动服务端工作流只构建 Go。

## Standalone 扩展

扩展保持独立运行、存储和版本发布。安装和配置见 [扩展说明](extension-standalone/README.md)。

扩展发布继续使用：

```bash
scripts/release.sh extension <x.y.z>
```

脚本创建本地提交和 `vX.Y.Z-standalone` 标签；需要推送时显式追加 `--push`。验证和发布步骤见 [扩展工作流](.github/workflows/extension-release.yml)。

## 许可证

[MIT](LICENSE)
