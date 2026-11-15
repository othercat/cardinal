# Apple Silicon 构建与搜索优化

关联任务：[#2](../issues/issue-02-apple-silicon-search-performance.md)。本分支仅面向 M 系列芯片、macOS 15.0 及以上。

## 独立应用与另一台 Mac

交付物是 `Cardinal Local.app` 及其 ZIP，bundle ID 为 `com.othercat.cardinal`，独立于官网版 `com.cardinal.one`。应用内嵌前端资源和 Rust 原生程序；Python、Node.js、Rust 和 Homebrew 仅在研发/构建时使用，不随包分发，也不要求目标 Mac 安装。通用 arm64 目标不启用当前机器专属的 `target-cpu=native`。

将应用复制到另一台 M 系列 Mac（例如 M4 Mac mini，macOS 15+）即可使用；目标机首次仍需授予 macOS 文件访问权限。当前使用 ad-hoc 本地签名，没有 Developer ID notarization；跨机首次启动可能需要在系统设置中确认允许打开。分发前核对 Mach-O 架构、最低系统版本、签名、动态库和包内资源，确认没有个人目录或 Homebrew 依赖。

当前版本为 `Cardinal Local 0.1.26`，分发包为 `delivery-root/releases/cardinal-release-r4-20261115.zip`。退出旧 Local 后替换同名应用，保留应用数据，直接启动。签名、SHA-256 和实测记录见 Issue #2。

## r4 的索引恢复和大结果集排序

- Local 优先读取自身的 `cardinal.db`，可直接使用 r3 的格式 6。Local 库缺失时可读取同一台 Mac 官网版的格式 5 或 6，要求监视根目录、包含和排除规则一致。后续保存到 Local 目录，写入格式仍为 6。历史变更同步期间退出可以保存完整基线及已提交的事件游标。初建未完成时保留之前的快照。
- 「正在读取已有索引」表示读取磁盘快照。文件出现后，「正在同步文件变更」表示后台正在追赶 FSEvents。切换「事件」和「文件」保留查询和可见行。属性变更只更新对应目录节点，创建、移动等需要遍历目录的事件在后台枚举并分批应用。
- 匹配项超过 8,192 时，窗口先取全局顺序的 256 项。滚动到任意位置时按页取数。名称利用现有名称索引，路径按目录树的字节序排序。大结果集的大小和时间按已有数值全局排序，缺值项排在末尾。后台按目录补齐属性、分批刷新顺序，正常退出时已取得的属性随快照保存。
- M1 Pro、macOS 15.7.7 的隔离窗口用旧 Local 快照在 8.87 秒显示 8,514,614 项，同步期间四次切回文件均在 20–24 ms 恢复。该次名称升/降序为 23/45 ms，路径升/降序为 354/97 ms，大小升/降序为 716/1,435 ms。计时从点击至可见行与后端全局首屏一致。原旧库有 6,979,004 项缺少属性，完整补齐耗时取决于目录与文件提供程序。

## r3 的实时更新、排序与首次索引

- 普通文件名查询和空查询使用索引变更记录。FSEvents 目标延迟为 25 ms，前台按 32 ms 窗口合并事件。创建、删除、改名或大小变化时，只把受影响的结果和当前排序位置发给窗口。持续写入期间继续出批，空闲时不轮询。涉及目录范围、内容等复杂条件时，按 500 ms 窗口合并后完整求值。
- 点击列头先用已加载行的数据重排可见范围，后台负责全部匹配项的顺序。列头的省略号表示全局排序仍在进行。再次点击同列切换反向时复用已完成的全局顺序。名称、路径、大小、修改时间、创建时间均支持升降序。首屏文件名和属性可以先于图标显示。
- 没有可恢复快照时，用 Darwin 的 `getattrlistbulk` 并行按目录获取名称、类型、大小和时间。每批最多 1,024 项进入可搜索索引。扫描完成后，从扫描开始前记录的 FSEvents 游标接续变更。退出时只保存完整索引。手动重建会重新遍历所选范围。
- 旧索引保持原有磁盘格式。第一次对旧数据按大小或日期全局排序时，匹配集覆盖较多子项的目录批量补属性，稀疏匹配按需读取。特殊文件、不可读属性和不支持批量读取的卷使用回退路径。

Everything 在 NTFS 上可直接从 MFT 建立初始索引，再用 USN Journal 维护。此版本在 APFS 上使用系统提供的目录枚举与 FSEvents。首批可查、完整索引可查、快照写盘和窗口绘制分别计时；条目数、目录结构、卷类型和访问权限都会影响结果。[Everything 开发者说明](https://www.voidtools.com/forum/viewtopic.php?t=357) · [Apple 的批量目录接口](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/getattrlistbulk.2)

## 源码构建

Rust 版本以 `rust-toolchain.toml` 为准（当前 `nightly-2025-12-11`），Node.js 使用 24，需安装 Xcode Command Line Tools。

```sh
rustup toolchain install nightly-2025-12-11 --profile minimal --component rustfmt --component clippy
# 避免 Homebrew 的独立 rustc/cargo 抢先于 rustup 工具链。
export PATH="$(dirname "$(rustup which --toolchain nightly-2025-12-11 cargo)"):$PATH"
export RUSTC="$(rustup which --toolchain nightly-2025-12-11 rustc)"
export RUSTDOC="$(rustup which --toolchain nightly-2025-12-11 rustdoc)"
cd cardinal
npm ci --ignore-scripts
npm run typecheck
npm test -- --run
npm run tauri build -- --target aarch64-apple-darwin --bundles app
```

Agent 安装依赖与执行前端命令时使用 `python-and-node-assistant` 的受管入口；项目的 `.runtime-assistant.toml` 禁用依赖安装脚本。esbuild 使用锁定的 arm64 npm 二进制，fsevents 自带原生模块。不要升级锁文件来处理下载重试。

`.cargo/config.toml` 默认目标为 `aarch64-apple-darwin`，设置 `MACOSX_DEPLOYMENT_TARGET=15.0`；Tauri 的 `minimumSystemVersion` 同为 `15.0`。CI 使用 macOS 15 arm64 runner，Rust 版本与项目一致。配置依据：[Tauri macOS bundle](https://v2.tauri.app/distribute/macos-application-bundle/)、[GitHub runner 架构](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。

可用 `CARGO_TARGET_DIR` 把构建输出放入 `process-root/staging/cardinal-issue-2/`。最终应用交付路径、SHA-256 和实际验证写入 Issue；不覆盖 `/Applications` 中的官网下载版本。保留原应用即可回退，本次未改数据库格式。

## r1、r2 的 20,000 条限制与实现沿革

原来的 20,000 是 UI 排序阈值，超过后会禁用按钮并取消排序。引擎使用 slab、名称 BTreeMap、全局名称 interner，以及压缩持久化快照，并不是 SQL 数据库行数到达上限。

主要开销是原排序调用 `expand_file_nodes`，即使按名称排序，也会对所有未加载 metadata 的结果执行 `symlink_metadata`。这与 FSEvents、搜索共享后台索引线程。原 `SortedViewCache` 还会复制整批路径/metadata，且仅凭节点 IDs 命中，未随文件变动失效。

本次变更：

- 默认排序上限为 `0`（不限），旧默认 `20000` 自动采用新默认。仍可手动设置正数上限；达到自设上限时保留已选排序，回到范围内后恢复。
- 文本排序从索引构建快照，不读取文件 metadata；名称复用已 intern 的字符串，每个结果只分配一份路径。目录递归原本即保存 metadata，同名空目录仍优先于文件。
- 数值排序只补齐缺失 metadata，已失败的读取也保留缓存，文件事件更新后重新可用。排序快照在工作线程上分块排序并合并，索引线程继续处理查询/事件；取消在准备、块间和合并中检查。
- 删除第二份节点排序缓存。每次从当前索引取一致快照；前端与后端均防止乱序/过期请求覆盖更新请求。后台更新期间保留上一批完成的排序和数据版本，排序就绪后一起提交；相同顺序复用原数组，不在排序/原始顺序之间切换。
- 前缀与精确查询使用 BTreeSet 的 range/get；普通匹配和大小写无关匹配直接遍历当前索引的有效名称并输出 IDs，避免扫描 interner 中历史残留的名称、构建额外匹配树和二次查表。
- 文件变化以 1 秒固定窗口合并刷新；持续事件不会无限延后刷新，同一时间至多一个自动刷新。后台查询不触发整页 loading，也不取消用户查询或排序；发起查询和提交结果前都等待当前排序，避免 IPC 在途时点击表头仍被刷新打断。用户可在排序期间选择另一列/方向，由新请求替代旧请求。
- 同一查询的后台更新保留已绘制的行与图标，仅按变化路径重新读取受影响行，数据到达后更新；没有变化不反复提取图标。选中项按 IDs 重映射到新位置，并保留已捕获的路径；删除/重命名、涉及选中项的目录重扫或无路径增量的全量刷新，会保守取消可能被复用的身份。用户换查询仍按原语义重置选择和滚动。
- 隐藏文档只记待更新，恢复显示再查；变化路径队列有界，溢出后做全量复核。没有事件时不轮询搜索。

## 验证与性能口径

基准在 Apple Silicon、macOS 15.7.7 上以 release 编译运行。比较同一目录的 30,000 个真实空文件、同一旧排序实现与新引擎；计时不含首次扫描、WebView IPC、图标提取或绘制，未清空 OS 文件缓存。名称查找另使用 100,000 个合成名称。交替运行前后版本各 5 次，以下取中位数：

| 项目 | 修改前 | 修改后 |
| --- | --- | --- |
| 30,000 条结果首次名称排序 | 63.581 ms | 11.410 ms |
| 100 次前缀名称池查找 | 35.405 ms | 0.040 ms |
| 100 次精确名称池查找 | 35.221 ms | 0.013 ms |
| 整段基准进程 CPU 时间（user + sys） | 0.41 s | 0.27 s |

名称排序约快 5.6 倍。进程 CPU 指标包括名称池构建、查询、扫描与多次排序，不等于应用日常 CPU 占用比例。名称池查找是单独的 API 微基准，不代表所有查询语法。最初单次名称排序观测为 151 → 11 ms；以上采用重复测量以减小波动。完整命令、源码摘要与原始日志记录在 Issue/过程证据中。

项目 Criterion 入口也增加了 `indexed_sort`：

```sh
BENCH_CORPUS_DIR=/path/to/corpus cargo bench -p search-cache --bench walk_and_search -- indexed_sort
```

它测量快照生成与排序；数值字段经过 warmup 后是热 metadata，不应作为首次 metadata I/O 成本。

必要回归覆盖：超过 20,000 条的升降序与完整 IDs、跨排序块/同名/数值相同的合并、冷索引空目录、节点删除、同 IDs 的 metadata 更新、取消、旧 IPC 请求晚到、远程失败、列表刷新期间的旧行保护、持续事件合并、隐藏与恢复。真实 FSEvents 集成测试覆盖创建、修改、重命名和删除。执行结果以 Issue 为准，不将测试计划视作通过。

r2 新增实际 App/VirtualList/搜索/排序/缓存/选择的组合回归；只模拟原生 IPC 和操作系统边界，覆盖 500 ms 后台查询、2 秒排序与持续文件事件，并从 DOM 验证列表不被卸载、没有整页 spinner、滚动/选中保持、五列升降序和真实 metadata 更新。另覆盖全量刷新与祖先目录重扫下的 ID 复用，防止高亮路径和操作路径不一致。这些测试仍不能替代用户实际全盘数据上的交互验收。

## r2 验收时记录的成本

- 首次按大小/日期对大量文件排序仍可能进行大量 metadata I/O；目前没有为整盘预先读取全部 metadata。新查询能取消这项工作，热数据和正常文件事件会复用/更新缓存。
- 通用子串、regex、内容搜索仍按相应候选扫描；本次没有增加三元组索引或预先维护所有排序列。大结果集的重新排序仍随匹配数量增长。
- FSEvents 丢失导致的 `HandleFSEError::Rescan` 沿用现有错误提示与手动重建机制；本次不改变全盘重建策略。
- 局部基准不能代表用户全盘 CPU、首次索引时间或 Everything 的体验。使用交付构建打开实际索引后，仍需核对大查询、大小/日期排序、持续写文件和隐藏时 CPU；首次使用的 Full Disk Access 由 macOS 管理。
