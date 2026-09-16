# Rust 服务端归档

归档日期：2026-09-17。原服务版本：**v0.1.4**。基准提交：`7a9c9dd3c7fa0d33cdadcf6829f2c8f7d0ca0d63`（`release: service v0.1.4`）。

服务端下一阶段将使用 Go 重新实现。本目录集中保存 Rust 实现及相关资料，供后续讨论需求、核对 NGA 协议和参考历史行为。Go 的详细设计尚未开始。

## 内容与目录映射

| 原仓库位置 | 归档位置 | 内容 |
| --- | --- | --- |
| `service/` | [service/](service/) | 全部 Rust 源码、Cargo 清单与锁文件、vendor、测试与合成 fixture、双数据库迁移、管理页面、部署文件和服务文档 |
| `PROJECT_PLAN.md` | [PROJECT_PLAN.md](PROJECT_PLAN.md) | 原 Rust 项目计划、M0～M8 里程碑与历史验收记录 |
| `CONTEXT.md` | [CONTEXT.md](CONTEXT.md) | 原监控领域术语 |
| `docs/adr/` | [docs/adr/](docs/adr/) | 已记录的架构决策 |
| `README.md` / `README.en.md` | [README.md](README.md) / [README.en.md](README.en.md) | 原仓库的中英文产品说明 |
| `AGENTS.md` | [REPOSITORY_GUIDANCE.md](REPOSITORY_GUIDANCE.md) | 原仓库开发指导快照 |
| `.github/workflows/service-image.yml` | [.github/workflows/service-image.yml](.github/workflows/service-image.yml) | 原 Rust 验证和镜像发布工作流；此位置不会被 GitHub Actions 自动执行 |
| `scripts/release.sh` | [scripts/release.sh](scripts/release.sh) | 原双产品发布脚本快照，供查阅，不作为当前发布入口 |
| `.gitignore` / `LICENSE` | [.gitignore](.gitignore) / [LICENSE](LICENSE) | 原忽略规则与许可 |

归档也包含基准提交之后产生、尚未提交的[服务端复杂度评审](service/docs/SERVICE_SIMPLIFICATION_REVIEW.md)。该报告保留当时的调查结果和渐进精简建议；用户随后决定采用 Go 完整重构，以这个最新方向为准。

## 阅读入口

- 总体范围与历史计划：[PROJECT_PLAN.md](PROJECT_PLAN.md)。其中的技术选型、任务清单和完成状态属于 Rust 实现。
- 服务使用：[中文说明](service/README.md)、[English](service/README.en.md)、[更新日志](service/CHANGELOG.md)。
- NGA 协议：[API 契约](service/docs/NGA_API_CONTRACT.md)、[fixture 说明](service/tests/fixtures/nga/README.md)。它们记录了历史观察，后续仍需结合新任务核实。
- 监控与通知：[设计](service/docs/MONITORING_NOTIFICATION_REDESIGN.md)、[免拉取时段](service/docs/NO_FETCH_PERIODS_DESIGN.md)、[ADR](docs/adr/0001-separate-no-fetch-periods-from-fetch-schedules.md)。
- 增量补偿：[楼层缺口恢复](service/docs/THREAD_FLOOR_GAP_RECOVERY_SPEC.md)、[UID 历史回帖回填](service/docs/UID_REPLY_HISTORY_BACKFILL_SPEC.md)。
- Bot 与登录：[交互和 Cookie 续期设计](service/docs/BOT_INTERACTION_AND_COOKIE_RENEWAL_DESIGN.md)。
- 内容与部署：[导出、资源和界面设计](service/docs/EXPORT_RESOURCE_AND_CONTENT_UI_DESIGN.md)、[运维手册](service/docs/OPERATIONS.md)。

历史资料可能存在设计与后续实现不同步的情况；评审报告已记录其中的例子。归档保留这些材料，不把它们直接变成 Go 版本的约束。

## 历史工程的路径与验证

内部 `service/` 布局保持原样。原文档中的仓库根目录命令，可从本目录执行：

```bash
cd archive/rust-service
cargo fmt --manifest-path service/Cargo.toml --all -- --check
cargo test --manifest-path service/Cargo.toml --locked --all-targets
cargo clippy --manifest-path service/Cargo.toml --locked --all-targets --all-features -- -D warnings
```

原发布工作流和发布脚本按历史内容保留；其中的 Git 根目录、标签及工作流上下文假设并未迁移为可发布的归档工程。当前仓库的 [发布脚本](../../scripts/release.sh) 只发布 Standalone 扩展。未来 Go 发布流程随新设计确定。

## 本地状态与归档范围

可提交归档只包含源文件、模板、测试和文档。原工作区的 `.env`、`data/` 和 `target/` 已保留到仓库根目录的 `.local/rust-service/`，由根 `.gitignore` 排除；未读取或提交真实凭据和运行数据，构建产物也不纳入 Git。这些本地文件不会随 Git 克隆分发。

Standalone 扩展继续位于 [extension-standalone/](../../extension-standalone/)，保持原有实现、版本和活动发布工作流。归档操作不改变已发布镜像、Git 标签或已运行的服务实例。
