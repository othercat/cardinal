# #2 Apple Silicon 源码构建与搜索性能

所属总管：[#1](issue-01-overall-management.md)

[GitHub Issue #2](https://github.com/othercat/cardinal/issues/2)

状态：已针对 r3 切回文件后停在「正在搜索」、旧库恢复与全盘排序等待交付 r4（0.1.26）。已用 851 万条快照的隔离 WKWebView 验证，待用户在日常环境复测。Issue 保持开放。

## 用户目标与范围

1. 将 `<ROOT>/agent-roles/AGENTS.md` 的原则合并到本项目 `AGENTS.md`；冲突以上游原则为准，尽量中文。
2. 仅支持 Apple Silicon（aarch64）Mac、macOS 15.0 及以上，交付可运行的源码构建。
3. 调查超过 20,000 条结果无法排序的原因；优化索引、查询、排序和更新路径，降低不必要的 CPU/磁盘工作。
4. 交付独立 `.app`，可复制到另一台 Apple Silicon Mac（包括 M4 Mac mini）使用，不要求额外准备 Python/Node.js/Rust/Homebrew 环境。

## 基线与授权

- 基线：`4c50734f9a09d88110f96652b43634b412f79449`，当前 `master`，初始工作区干净。
- 已核对 `origin` 为 `othercat/cardinal`，远端仅有 `master`，无 `dev`；远端 `master` 与当前 HEAD 相同。保留当前工作区，不创建替代集成分支。
- 初次交付在本地完成。2026-11-15 用户随后授权提交当前源码、推送到远端，并清理中间产物与多余发行副本。
- 用户已通过 `/goal` 建立本任务 Goal。

## 成功标准

- 新规范保留原项目有效指引，解决与上游在路径、合并策略等方面的冲突；关键引用可读。
- 构建产物只有 arm64，应用声明最低 macOS 15.0，可核对版本、构建命令与 SHA-256。
- 新应用命名为 `Cardinal Local.app`，独立 bundle ID `com.othercat.cardinal`，与官网版隔离状态。检查运行时动态库、资源、symlink，不能依赖研发机的个人目录、Homebrew 或语言运行时。
- 超过 20,000 条结果仍可正确排序，覆盖升降序、分页和更新后的排序；旧请求取消不产生过期结果。
- 用可重复的基准比较修改前后查询/排序的耗时与工作量；明确合成数据、实机数据及权限限制。
- 运行必要的 Rust/前端测试、类型检查和 release 构建；记录实际结果与未覆盖项。

## 发现与决策

- 实机：Apple Silicon arm64，macOS 15.7.7。
- 实际 Rust 锁定为 `nightly-2025-12-11`，原 `AGENTS.md` 和 CI 的 `2025-05-09` 已过时。
- Node/Python 已经受管角色核对；依赖安装与构建复用该入口。

## 验证与交付记录

### r4 旧库完整补齐后重启排序（用户追加实测）

- 经 SHA-256 核对的原 Local 副本包含 8,514,614 项，其中 6,979,004 项未采集属性。真实窗口点击大小时排入 6,978,955 项，172.863 秒后完成，正常退出保存用了 2.988 秒。退出时独立解码得到 8,649,106 项，待采集 0，无法获取属性 2,338，三项属性均有值 8,645,472。同期 FSEvents 继续增删文件。
- 五个有效的新进程从相同的已保存副本启动，都恢复了 8,649,106 项，首次出现文件耗时 9.28–10.26 秒。在「就绪」状态下，大小、修改时间、创建时间升序，点击至正确首屏后的渲染帧三轮中位数分别为 783、775、773 ms。同一列反向时分别测到 25–2,174、24–39、26–783 ms。另一次刚重开就点大小，首屏耗时 2,062 ms。
- 一次重开在恢复快照后又因历史事件处理进入完整重扫，期间点击修改时间后，表头用 66.969 秒完成。该次未记录具体事件标记。保存的库已经通过 r4 的格式、结构、监视范围和事件游标校验。
- 实测原生排序顺序只存在内存；真实文件变化可以使其失效。保留补齐副本供后续验证。未更改用户实际索引或产品源码，Issue 继续开放。全部输入、计时口径、42 次有效点击和退出快照摘要见本机 `process-root/artifacts/cardinal-issue-2/r4-full-hydration/完整补齐与重启排序实测.md` 及相邻的 `r4-full-audit/`。

### r4 修复与验收（2026-11-15）

- r3 切回文件会清空查询并重新发起全盘搜索。运行中 r3 的原生线程采样显示，事件回放在索引线程递归读取目录，取行和搜索也需要这条线程。r4 保留文件面板及其查询、选择和滚动位置，属性事件按节点处理，结构性事件放到工作线程枚举。只有完整处理一批事件后才推进其游标。
- 现有 Local 磁盘库为格式 6，共 8,514,614 项。官网版磁盘库为格式 5，共 8,542,246 项。两者配置相容。Local 优先使用原库，仅当 Local 缺失时才只读采用相容的官网库。v5 转存 v6 后节点数和事件游标一致。本机独立进程加载 Local、加载官网库、再次加载转换后的库分别为 8.838、10.270、9.183 秒。新写入使用校验和、刷盘和原子替换，旧格式 6 无需转换。
- 从旧 Local 快照打开真实隔离窗口，在 8.872 秒显示 8,514,614 项，状态为「正在同步文件变更」。四次切回文件耗时 20–24 ms，新增前台查询为 0。正常退出并重开同一测试库后，在 9.034 秒显示保存过的 8,515,148 项。原官网库和 Local 库保留原字节。
- 全盘首次运行点击列头至 DOM 首屏与已发布顺序一致，名称升/降序 23/45 ms，路径升/降序 354/97 ms，大小升/降序 716/1,435 ms，修改时间和创建时间升序 746/725 ms。每次大查询最多向 WebView 返回 256 个 ID。独立深滚动验证了路径排序的第 425 万行附近、末页和回到首行，含验证 IPC 的耗时为 233/16/198 ms。
- 原 Local 快照有 6,979,004 项（81.965%）尚未取得大小和时间。大结果集先排全局已有值，两种方向都将缺值放在末尾。后台限并发读取，进度可以跨正常退出保存。一次隔离运行在退出前已报告处理 82,036 项，随后运行恢复并继续，另观测到 83,291 项。这里未计量全库全部补齐时间。
- 同一 851 万条只读快照，两版 release 交替运行各两次。名称升序首 256 个 ID 的摘要四次相同。r3 原生层首次名称排序为 3,977/3,983 ms，r4 为 1.10/3.30 ms，不含快照加载和 UI。空查询本身分别为 54–55 ms、51–56 ms。返回的 ID 数量从 8,514,614 减到 256。r4 额外校验索引结构，原生整库加载从 7.52–7.59 秒变为 8.27 秒。
- 22,050 个真实测试文件逐项比较五列的十种全局顺序，抽验 40 个滚动窗口，创建/改名/修改/删除到可见 DOM 分别为 64/61/62/65 ms。测试期间列表卸载、整页 spinner、JS 错误均为 0。该测试有独立 bundle ID、数据库和测试窗口，权限检查由测试入口代替。日常长时间 CPU、另一台 M4、所有旧文件属性补齐的总耗时待实机验证。
- 锁定环境下通过 Rust workspace 检查和测试（1,817 passed，6 ignored）、Tauri 测试（45 passed）、两个 workspace 的 Clippy（含所有 targets，warnings 当错误）、前端类型检查、303 项前端测试、Prettier、生产构建。未修改 Cargo/npm 锁文件。发行包和解包后应用均通过 arm64、macOS 15.0、ad-hoc 签名和 24 项系统库依赖核对，7 个文件逐项摘要一致。
- 分发包 `delivery-root/releases/cardinal-release-r4-20261115.zip`，5,839,199 bytes，SHA-256 `c8d6b058ff801fbdf96d0c337bf2f97103d632bdba5202af86ac157b9d8ad352`。主程序 SHA-256 `42d0a68ff12ee0c300631eddef8146a5641200121764ec7725a3d8bd79064e3d`。未提交工作区的 288 个构建输入摘要 `1eb7a1036ba05661eb98a513e60ea326d30519045f9c9dd5224923a63840338b`。
- 本机证据在 `process-root/artifacts/cardinal-issue-2/r4-view/`、`r4-persistence/`、`r4-webview-qa/`，包括采样、原始时间、独立窗口报告、测试日志、源文件摘要和解包验证。对应测试与原生基准入口分别在 `process-root/staging/cardinal-issue-2/r4-webview-qa/`、`r4-view/`，基准只需提供与测试配置相匹配的快照副本。配置版本变更备份在 `process-root/sync-backups/cardinal-issue-2/r4/`。

### r3 实现与交付（2026-11-15）

- 文件事件按名称索引的实际变更更新当前查询与活动排序，同一名称的节点和复用的 slab ID 可正确增删。对目录范围、复杂表达式、历史游标过期或变更量过大的情况重新求值。后端目标 FSEvents 延迟 25 ms，普通名称查询在前台按 32 ms 合批，复杂查询按 500 ms 合批。持续写入可以持续出批。
- 表头先重排已加载的可见行，随后采用一致的全局顺序。全局排序按查询版本取得输入，使用二进制 ID 返回结果，反向可以直接复用。大小、时间、名称与父路径复用索引数据。图标异步加载，行文字可先呈现。
- APFS 批量读取目录与 metadata，按目录并行交付，每批最多 1,024 项进入可搜索索引。建立第一批前先取历史游标，完成初建时接续 FSEvents。可恢复旧快照，退出时只保存完整索引。旧快照中的缺失 metadata 在首次需要时补齐。丢失事件需要全量重扫时实际重建。
- r3 ZIP 为 `delivery-root/releases/cardinal-release-r3-20261115.zip`（5,776,551 bytes），SHA-256 为 `c3ea6b6cb751022021dff445a2590625204a32970693eac6026972bedfed3432`。主程序 SHA-256 为 `cc974f233d20b1689cea86f06eae1de7b8eed5f461580e2fedc72473f1098be4`。源输入清单 SHA-256 为 `89fe3a5935e29f0a916f02ec40a782f7724715b1ddc0a0d2af0eee44cb398971`。
- 使用未提交工作区，基线同上。已运行根 workspace 的 check、test、clippy 及 Tauri 独立 workspace 的 test、clippy，均带锁。根 workspace 1,806 通过、6 忽略，Tauri 40 通过。前端类型检查、301 项测试、Prettier 与正式 release 构建通过。原有三个锁文件均与 HEAD 相同。打包命令为 `npm run tauri -- build --target aarch64-apple-darwin --bundles app --ci -- --locked`。已核对 arm64、minos 15.0、ad-hoc 签名、系统动态库与内嵌前端资源。ZIP 解包后 7 个文件逐项 SHA-256 相符，主程序可执行、签名通过。
- 安装版及用户索引保持原状。回退可从托盘退出后用本机保留的 r2 ZIP 恢复同名应用。未暂存、提交、合并或推送。

### r3 窗口验收

Apple M1 Pro、macOS 15.7.7，使用独立 bundle ID、独立数据库和带观测桥的真实 WKWebView 测试构建。22,050 个匹配文件覆盖五列各两个方向，全部成员及顺序与独立文件清单一致，另核对 40 个首、中、尾等滚动窗口。点击至可见顺序改变为 10–11 ms，全局顺序提交为 21–29 ms。默认列表原已是名称升序，第一次点击该列没有位置变化。实际创建、改名、修改、删除至 DOM 的延迟分别为 74.8、40.5、42.3、51.4 ms。原有无关选择保持有效，列表卸载、整页 loading 和 JS 错误均为 0。

### r3 首次建库实测

从空的 Cardinal 索引读取本机内置 APFS 数据卷，使用发行版的索引代码、8 个 Rayon 线程。数据卷容量约 2.00 TB，已用约 1.93 TB。只访问元数据，统计数字为可访问范围，排除外接卷、挂载入口与本任务生成物。逻辑大小是各普通文件长度之和，含 APFS 克隆、硬链接及稀疏文件的影响。未清空 macOS 文件缓存。

| 扫描范围 | 可查条目 | 普通文件逻辑大小合计 | 首批可查 | 全部可查 | 全部可查并写出快照 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 包含云存储目录 | 8,092,935 | 3.384 TB | 3.44 ms | 186.90 s | 此轮未计时 |
| 再排除当前用户的 `Library/CloudStorage` 和 `Library/Mobile Documents` | 7,953,200 | 3.366 TB | 1.96 ms | 58.28 s | 60.29 s |

第一轮第 60 秒已有 7,181,184 项可查，末段采样显示工作线程在等待云存储目录的 `getattrlistbulk` 系统调用。第二轮在第一轮之后执行，两轮覆盖范围和系统缓存状态均有差异。第二轮用隔离临时目录写出 230,016,646 bytes 快照，耗时 2.004 s，计时后删除该测试快照；汇总日志只含数字。上述时间是原生索引时间，窗口的真实延迟使用上一节的独立测试。

同一目录、同一线程数、r2 与 r3 各交替运行 5 次，快照读回后条目数和全路径摘要相同。96,618 项样本的中位首批可查时间从 111.65 降到 0.66 ms，建库含写盘从 142.35 增至 232.72 ms。142,064 项 Xcode 目录树的中位首批可查从 288.75 降到 0.49 ms，建库含写盘从 361.21 增至 491.62 ms。r3 提前提供可查结果并在首次建库时取得普通文件大小和日期，完整建库的实测成本一并保留。

本机证据在 `process-root/artifacts/cardinal-issue-2/r3-live/`（测试、构建、数据卷采样、A/B、源码摘要和分发校验）及 `r3-webview-qa/`（窗口与文件变更验收）。前者的重放程序在 `process-root/staging/cardinal-issue-2/r3-live/`。用户实际全盘窗口及另一台 M4 Mac mini 待复测。

### r1 实机反馈与修复验收标准

- 2026-11-15，用户将 r1 放入“应用程序”并启动，反馈界面一直刷新、排序似乎无效。r1 的自动测试与包检查不能证明实际刷新体验通过，本次沿用 #2 修复。
- 已确认 r1 每批全盘文件事件都触发搜索；后台搜索沿用前台 loading、无条件提升数据版本并清空缓存，慢请求使 VirtualList 卸载重挂；新搜索还取消上一轮排序，投影在已排序/未排序间切换。
- 修复标准：后台更新不显示整页 loading、不卸载列表；无关事件与相同结果不清空行/图标缓存；后台刷新不抢占用户查询或排序；排序按钮能完成名称/路径/大小/日期的升降序并保持当前顺序可见；真实增删改仍更新，保持未受影响的选择和滚动位置。
- 在组合回归中覆盖持续事件、延迟搜索/排序、相同 IDs、真实 metadata 变化与用户查询优先；重新构建独立 r2，保留 r1 供追溯，不覆盖运行中的 App 或索引。

### r2 修复与验证（2026-11-15）

- 后台搜索不派发前台 loading、失败或取消不替换当前页面；相同 IDs 和高亮复用原数组，避免背景噪声触发不必要的投影变化。
- 原生 `background_search` token 不增加搜索代次、不取消前台查询/排序；后续用户查询仍可取消它。后台发起查询和提交结果前都等待当前排序，覆盖“请求已在途后用户再点排序”的竞态。
- 同一查询在排序完成前保留上一批完成的顺序、数据版本与文件增量，完成后一并提交。只有可见顺序实际变化才提升显示版本；排序期间仍可换列、换方向或取消排序。
- 事件合并窗口改为 1 秒，队列最多 2,048 个路径；隐藏时暂停，没有待处理变化时不轮询。用户输入/排序优先，延后的事件不丢弃。
- 后台数据更新保留行与图标，按变化路径 revalidate，只提交真正变化的 row payload；纯 metadata 更新不重复提取图标。旧批次不能释放新 epoch 对同一 ID 的 loading 标记。
- 未受影响的选择按 ID 重映射，并保留捕获路径。删除/重命名、祖先目录扫描或无增量的 full refresh 保守取消身份，避免 slab ID 复用后“高亮新文件、操作旧文件”。用户新查询继续重置滚动和选择。
- 新增 `App.liveRefresh.test.tsx`，实际组合 App、FilesTabContent、VirtualList、搜索、排序、缓存、选择；仅模拟原生 IPC、事件与系统接口。模拟 500 ms 后台请求、2 秒排序、连续文件事件，验证列表 DOM 不卸载、无整页 spinner、滚动/选择保持、无关变化不重复请求 metadata/图标、五列升降序和真实 metadata 变化后重排。独立单测覆盖新查询优先、ID 复用、旧批次竞态，原生单测验证 token 和十种排序 payload 的反序列化。
- 前端：34 files、299 tests passed；`npm run typecheck`、`npm run format:check` 通过。
- Rust workspace：1,799 passed、0 failed、6 ignored；Tauri：40 passed、0 failed。workspace/Tauri 的 `clippy --all-targets --locked -- -D warnings` 及本次 Rust 文件 rustfmt check 通过。
- `npm run tauri -- build --target aarch64-apple-darwin --bundles app --ci -- --locked` 通过。沿用已核对的受管 Node 24.20.0 与 nightly-2025-12-11，未安装或升级依赖，三个锁文件未改变。
- 版本为 `0.1.24`，仍使用 `com.othercat.cardinal`，索引/设置路径不变。r1 源码快照与配置备份位于 `process-root/sync-backups/cardinal-issue-2/r1-refresh-regression/`；r1 ZIP 哈希复核未变。
- 本次证据位于 `process-root/artifacts/cardinal-issue-2/r2-refresh-fix/`，仅本机可访问。尚未用用户全盘索引运行 r2 或在 M4 Mac mini 实机复测，不能把模拟 IPC/事件的组合测试写成真实全盘验收或 CPU 百分比改善。

### r2 交付

- ZIP：`delivery-root/releases/cardinal-release-r2-20261115.zip`，5,659,073 bytes（约 5.4 MiB）。
- ZIP SHA-256：`c83e7cfe9ac89e5a4af56e25758b0395ef2857379a34c51c8914c0cdb7047a26`；相邻 `.zip.sha256` 可核验。
- 应用：`delivery-root/releases/cardinal-release-r2-20261115/Cardinal Local.app`，17,706,764 bytes；主程序哈希与全部 App 文件清单在随包 `manifest.json`。
- 当前 273 个源输入摘要：`a90ef88d82e0bd315fe252d4fba633ea31eac25d964dc0b8cb7253bfe9d566d1`。
- arm64、Info.plist 与 Mach-O 的 macOS 最低版本均为 15.0；24 项动态依赖全部为系统库，不要求 Python/Node.js/Rust/Homebrew。独立 ad-hoc 签名无上游 entitlements，未 Apple notarization。
- ZIP CRC、单一顶层目录与相对路径、7 个文件逐项哈希、解包后可执行权限及 `codesign --verify --deep --strict` 均通过。详细证据为本次 `distribution-verification.json`。
- 用户更新步骤：退出旧版 Cardinal Local（含托盘），复制 r2 的同名 App 到“应用程序”替换旧版后打开；按 macOS 实际提示授予文件访问权限。本任务未安装/覆盖用户 App 或索引。
- 用户复测重点：持续文件活动时列表稳定；在超过 20,000 条结果时分别点击五列升降序，等待首次数值字段 metadata 完成；排序期间换列和输入新查询；选中文件、滚动、文件增删改后检查排序与路径。若仍异常，记录实际查询、排序列、结果规模、索引状态和构建版本，继续沿用本 Issue。

### 已完成实现

- `AGENTS.md` 以 agent-roles 原则为主，保留并中文化项目约定，复制核心 `docs/Skill的约束原则.md`，默认普通 merge；不更改原 checkout 路径。
- arm64 默认编译目标、macOS 15.0 最低版本、原生架构编译保护及 CI 工具链/runner 已同步。
- 排序默认不限；文本排序不读取 metadata；索引线程生成快照后在工作线程做可取消的分块排序与合并。
- 移除不随文件变动失效的节点排序缓存；前后端请求版本防止过期/乱序结果覆盖。
- BTreeSet 前缀/精确查询使用 range/get，普通匹配只扫描当前索引的有效名称，避免额外匹配树与全局历史名称扫描。
- 文件事件以 300 ms 窗口合并刷新，隐藏文档暂停重查，恢复后更新。
- 区分用户查询与后台数据更新；后台刷新不再把列表滚动位置重置到顶部。

### 已执行验证（2026-11-15）

- `cargo test --workspace --locked`：1,798 passed、0 failed、6 ignored；包括真实 FSEvents 创建/修改/重命名/删除与排序联动。
- Tauri 独立 `cargo test --locked`：39 passed、0 failed。
- workspace 与 Tauri 的 Clippy（`--all-targets --locked -- -D warnings`）：通过。
- Rust rustfmt、前端 `npm run format:check`、`git diff --check`：通过。
- 受管 Node.js 24.20.0 按原锁备料、同步依赖完成；`npm run typecheck` 通过，`npm test -- --run`：33 个文件、282 tests passed。
- `npm run tauri -- build --target aarch64-apple-darwin --bundles app --ci -- --locked`：通过，包括 Vite production build 与 Rust release bundle。显式使用受管 Node、锁定 nightly 的 cargo/rustc/rustdoc/PATH，`CARGO_TARGET_DIR=process-root/staging/cardinal-issue-2/cargo-target`（执行时为绝对路径）。
- 最终 bundle 及 ZIP 解包后的应用：`lipo -archs` 仅 `arm64`；Info.plist 最低系统版本与 Mach-O `minos` 均为 `15.0`；bundle ID 为 `com.othercat.cardinal`。
- `codesign --verify --deep --strict` 通过；24 项动态依赖全部位于 `/System/Library/` 或 `/usr/lib/`，包内没有研发机外链或语言运行时依赖。签名未携带上游 Team/bundle entitlements；源码中的原 `Entitlements.plist` 没有被当前 Tauri 配置引用。
- ZIP CRC、单一顶层目录与安全相对路径检查通过；使用 `ditto -x -k` 解包后 7 个文件逐项 SHA-256 一致，全部 App 文件与 manifest 匹配，主程序保留可执行权限及有效签名。
- 当前 272 个构建源输入文件哈希与产物记录一致；根 workspace、Tauri、npm 的三个锁文件均与基线逐字节相同。
- 以上是本机自动验证与包检查；尚未在 M4 Mac mini 交互运行，也没有把局部基准当作全盘日常 CPU/FPS 的结论。

### 性能证据

- 同一 30,000 个真实空文件目录，旧/新 release 二进制交替运行各 5 次。首次名称排序中位数：`63.581 → 11.410 ms`，约 5.6 倍；整段基准进程 user+sys CPU 时间中位数 `0.41 → 0.27 s`。
- 100,000 个合成名称中重复 100 次前缀/精确查找，中位数分别为 `35.405 → 0.040 ms`、`35.221 → 0.013 ms`。这是 NamePool API 微基准，不代表所有查询语法。
- 排序计时不含首次扫描、WebView IPC 或绘制，未清空 OS 文件缓存。CPU 时间包含基准中的扫描、名称池构建和多次排序，不等于应用 CPU 百分比；旧实现热缓存行为不同，不报告热排序的前后对比。
- 重放：从仓库根目录交替执行 `/usr/bin/time -lp process-root/artifacts/cardinal-issue-2/perf-before process-root/staging/cardinal-issue-2/sort-corpus` 与相同参数的 `perf-after`，各 5 次。快照源码位于 `process-root/staging/cardinal-issue-2/perf-harness/`；原始 `perf-*-run-*.log` 与汇总 `performance-summary.json` 位于本 Issue 的 artifacts 目录。
- 正式 Criterion 也执行了 `BENCH_CORPUS_DIR=<上述 corpus 绝对路径> cargo bench -p search-cache --bench walk_and_search -- indexed_sort`，日志为 `indexed-sort-bench.log`。数值字段在 warmup 后是热 metadata，不能替代首次数值排序 I/O 测量。

### 交付 r1

- 应用：`delivery-root/releases/cardinal-release-r1-20261115/Cardinal Local.app`，17,706,652 bytes（约 16.9 MiB）。
- 分发 ZIP：`delivery-root/releases/cardinal-release-r1-20261115.zip`，5,658,804 bytes（约 5.4 MiB），相邻 `.zip.sha256` 可核验。
- ZIP SHA-256：`396e96b3c3bdec7ad8c737e67f4b0e5c131dc361915617604f923163a382abeb`。
- 主程序 SHA-256：`f4d037fbce76bbb56bfedd20d9923e738b837e606c8c95af358df05d3a17a42d`。
- 源输入摘要：`bd30a474a2c8453c9434a7316d32dc089ae56e9145a24da124aec4ddc3d36148`，详见 `source-input-manifest.json`；不以未提交工作区冒充实现 commit。
- ZIP 内仅同名发行目录，包含 App、MIT LICENSE、中文使用说明和 `manifest.json`。运行只依赖 macOS 系统库与 WebKit，不要求安装 Python/Node.js/Rust/Homebrew。
- 使用 ad-hoc 签名，未做 Apple notarization；跨机首次启动可能需在“系统设置 → 隐私与安全性”确认允许。新身份需要单独授予 Full Disk Access，并建立自己的索引。

### 待跨机验收

1. 在 macOS 15+ 的 M4 Mac mini 解包，把 `Cardinal Local.app` 复制到“应用程序”，按系统提示允许打开并单独授予 Full Disk Access；确认没有语言环境安装步骤。
2. 为避免双索引和快捷键冲突，测试时只运行需要验收的版本；确认原版设置、索引仍可独立使用。
3. 索引就绪后查询超过 20,000 条，检查名称/路径/大小/日期升降序、滚动加载、切换排序与快速换查询；首次数值排序允许有 metadata 读取成本。
4. 在查询范围创建、改内容、重命名、删除文件，检查结果和当前排序自动刷新；持续写入时仍可操作，后台刷新不跳回列表顶部。
5. 用活动监视器观察建索引、空闲、持续写文件、隐藏与恢复时的 CPU/内存；记录实际文件规模、机器、构建 SHA 与现象。FSEvents 丢失需要重建时沿用现有提示与手动重建机制。

### 过程记录与回滚

- 配置/规范备份：`process-root/sync-backups/cardinal-issue-2/20261115-091117/`；本机 Git 配置另存 `git-config-before-symlinks` 后设置 `core.symlinks=true`。
- Tauri 生成 schema 已由 `.prettierignore` 最小排除；修改前备份为 `prettierignore-before`。依赖备料的间歇 SSL 失败使用任务局部 curl 重试配置恢复，未变更全局网络设置。
- 日志与基准源码/二进制：`process-root/artifacts/cardinal-issue-2/`、`process-root/staging/cardinal-issue-2/perf-harness/`；均为本机证据，不是 GitHub 可访问链接。
- 最终分发检查：`process-root/artifacts/cardinal-issue-2/distribution-verification.json`；该文件记录解包副本位置、签名/架构/动态库命令输出。前端、Rust、构建日志均保留在同一 artifacts 目录。
- 本机 Homebrew 的 Rust 可绕过项目 nightly；验证显式使用 rustup 工具链的 cargo/rustc/rustdoc/PATH。最初的错误尝试日志保留，后续已通过的检查使用正确 nightly。
- Context7 CLI 查询失败后改查 Tauri/GitHub 官方文档，具体参考链接在 `docs/apple-silicon-search.md`。
- 原应用、原索引未修改；未修改任何 Cargo/npm 锁文件，无索引格式迁移。未暂存、提交、推送或合并。
- 最终只读检查发现 `/Applications/Cardinal.app` 当前不存在；本任务没有安装、删除或移动该应用，不推断其消失原因。新 App 只交付在 `delivery-root`，尚未安装到“应用程序”。

实现：未提交。
合并：未执行。
验证：r4 的全盘旧库恢复、真实窗口、排序、深分页、属性进度、各 workspace 测试和解包检查见上文；APFS 空库初建沿用 r3 专项测量。
清理：无分支或 worktree；新过程产物按本 Issue 分类。
实机验收：M1 Pro 上以真实旧库副本运行 r4 隔离窗口。待用户安装 r4，在日常目录与权限下复测。另一台 M4 Mac mini 尚未测试。
Issue 状态：保持开放。
