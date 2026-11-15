# #1 长期总管

状态：开放（长期保留）

[GitHub 总管 Issue #1](https://github.com/othercat/cardinal/issues/1)

## 任务索引

| Issue | 范围 | 状态 | 依赖 |
| --- | --- | --- | --- |
| [#2](issue-02-apple-silicon-search-performance.md) | 合并项目规范、Apple Silicon/macOS 15+ 独立应用、大结果集排序与搜索性能 | r4（0.1.26）已交付。旧库约 698 万项属性实测 172.863 秒补齐，重启后就绪时大结果集首次切换数值列约 0.8 秒，历史回放和重扫可延长等待 | 无 |
| [#3](issue-03-removable-volume-search.md) | HFS+、移动卷插拔和本机离线索引 | r5（0.1.27）已交付。HFS+、HFSX、APFS 映像及窗口离线重启验证通过，待实体移动硬盘实测 | #2 |
| [#4](issue-04-small-file-event-cpu.md) | 频繁小文件写入时的 CPU 和索引更新延迟 | r6（0.1.28）已交付。每秒 2,560 次冷门小文件覆写的前台整组 CPU 为 136.55% → 8.21%，待日常全盘验收 | #2、#3 |

## 历史产物

本次接入前无 `process-root/`、`delivery-root/`、`docs/`、`issues/`。已有产品文档位于 `doc/`。

2026-11-15 清理后，本机 `delivery-root/releases/` 只保留 r4 的 ZIP 和 SHA-256 校验文件。旧版解包副本、测试工作区和构建缓存已清除。实测报告与私有补齐快照归档到 `delivery-root/artifacts/cardinal-issue-2/`。源码与验收记录见 [GitHub Issue #2](https://github.com/othercat/cardinal/issues/2)。

随后新增 r5 的 ZIP 和 SHA-256 校验文件，保留 r4 供回退。r5 的映像实测及发行校验归档到本机 `delivery-root/artifacts/cardinal-issue-3/r5-validation/`。

r6 的安装包与校验文件也位于 `delivery-root/releases/`。频繁文件写入的 CPU 对照、窗口延迟和发行校验保存在 `delivery-root/artifacts/cardinal-issue-4/r6-validation/`。
