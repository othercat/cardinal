# #1 长期总管

状态：开放（长期保留）

[GitHub 总管 Issue #1](https://github.com/othercat/cardinal/issues/1)

## 任务索引

| Issue | 范围 | 状态 | 依赖 |
| --- | --- | --- | --- |
| [#2](issue-02-apple-silicon-search-performance.md) | 合并项目规范、Apple Silicon/macOS 15+ 独立应用、大结果集排序与搜索性能 | r4（0.1.26）已交付。旧库约 698 万项属性实测 172.863 秒补齐，重启后就绪时大结果集首次切换数值列约 0.8 秒，历史回放和重扫可延长等待 | 无 |
| [#3](issue-03-removable-volume-search.md) | HFS+、移动卷插拔和本机离线索引 | r5（0.1.27）已合入 `master`。HFS+、HFSX、APFS 映像及窗口离线重启验证通过，待实体移动硬盘实测 | #2 |
| [#4](issue-04-small-file-event-cpu.md) | 频繁小文件写入时的 CPU 和索引更新延迟 | r6（0.1.28）已合入 `master`。每秒 2,560 次冷门小文件覆写的前台整组 CPU 为 136.55% → 8.21%，待日常全盘验收 | #2、#3 |
| [#5](issue-05-context-path-exclusions.md) | 右键排除文件或父目录，评估 Spotlight 对文件名搜索的影响 | r7（0.1.29）已在本机交付。原生菜单、撤销、重启、离线卷与旧库副本通过验证，待日常使用验收 | #2、#3、#4 |
| [#6](issue-06-allocated-size-sort.md) | 文件大小列与排序使用实际磁盘占用，沿用旧索引 | r8（0.1.30）本机交付。Docker.raw 实占 8.55 GB，旧库补读 96.64 秒，重开后的全局排序 0.76 秒。原生窗口与离线卷已验收 | #2、#3、#4 |

## 历史产物

本次接入前无 `process-root/`、`delivery-root/`、`docs/`、`issues/`。已有产品文档位于 `doc/`。

r4 的旧索引实测报告和唯一的补齐后测试快照保存在本机 `delivery-root/artifacts/cardinal-issue-2/`。r5 的磁盘映像实测与发行校验保存在 `delivery-root/artifacts/cardinal-issue-3/r5-validation/`。r6 的 CPU 对照、窗口延迟与发行校验保存在 `delivery-root/artifacts/cardinal-issue-4/r6-validation/`。[r4 记录](https://github.com/othercat/cardinal/issues/2) · [r5 记录](https://github.com/othercat/cardinal/issues/3) · [r6 记录](https://github.com/othercat/cardinal/issues/4)

2026-11-15 推送 r6 后，本机 `delivery-root/releases/` 仅保留 `cardinal-release-r6-20261115.zip` 和同名 `.sha256` 文件。清理前该目录已无 r4、r5 安装包。r6 解包残留的许可证、清单、使用说明逐一与 ZIP 核对后删除。
