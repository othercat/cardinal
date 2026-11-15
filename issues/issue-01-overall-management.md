# #1 长期总管

状态：开放（长期保留）

[GitHub 总管 Issue #1](https://github.com/othercat/cardinal/issues/1)

## 任务索引

| Issue | 范围 | 状态 | 依赖 |
| --- | --- | --- | --- |
| [#2](issue-02-apple-silicon-search-performance.md) | 合并项目规范、Apple Silicon/macOS 15+ 独立应用、大结果集排序与搜索性能 | r4（0.1.26）已交付。旧库约 698 万项属性实测 172.863 秒补齐，重启后就绪时大结果集首次切换数值列约 0.8 秒，历史回放和重扫可延长等待 | 无 |

## 历史产物

本次接入前无 `process-root/`、`delivery-root/`、`docs/`、`issues/`。现有产品文档位于 `doc/`；用户指定的官网下载应用路径为 `/Applications/Cardinal.app`，本任务不移动、不覆盖。最终只读复核时该路径不存在；本任务未对“应用程序”目录执行安装或删除，新应用保存在 delivery-root。
