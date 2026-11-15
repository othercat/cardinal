# 项目工作规范（合并 agent-roles 原则）

本文件适用于 Cardinal 的人工开发与 Agent。已合并 `<ROOT>/agent-roles/AGENTS.md` 的原则；与原项目规则冲突时以该来源为准，用户在当前任务中的明确要求优先。来源仓库的角色/工具布局和游戏发行约定仅在涉及相应工作时适用。现有 checkout 路径保持不变，不因默认路径约定移动仓库。

### 设计原则

1. 任何事情，都不能主动中断FlClash，Fishport，Clash等代理客户端，一定要询问用户
2. 核心原则是`docs/Skill的约束原则.md`，如果下面有任何原则和本原则冲突，以本原则为准。
3. 这个项目是尽可能使用离线模型和培训skill以及mcp，尽可能不使用computer-use和browser-user，
4. 所有项目项目参考的<ROOT>目录是：macOS/Linux一律 `~/workspace`, win一律`C:\workspace`
5. 每个项目都有自己的<PROJECT-NAME>，即自己的仓库名称，如果仓库改名，则设置为<PROJECT-NAME>的别名，不轻易改动<PROJECT-NAME>
6. 具体每个项目都有自己的<PROJECT-ROOT>，都在`<ROOT>/projects`文件夹
7. 每个项目都有自己的中间产物和交付产物目录，具体可以参考下面的`过程和交付产物位置`内容

## 环境准备

- 每个Session创建时，先读取`docs`和`issues`下面的全部文件，了解当前需求，然后核对 `roles.md` 中登记的角色，以及 Issue 要求的项目专用 Skill 和 MCP 是否已部署。



### 工作角色安排

- 请读取<ROOT>下`agent-roles`文件夹下的roles.md，了解角色分工，之后会根据角色来分配任务



## Issue 任务入口与总管

- 本项目每项独立任务都必须关联真实的本地 Issue文件，包括代码、规范、环境修复、验证和产物整理。用户指定 Issue 时先读取并核对任务范围；未指定时先检查同一任务是否已有关联 Issue，没有则在 项目根目录下的`issues`文件夹创建，issue命名规则为`issue-<issue-id>-<issue-title>`的md文件，再修改文件、建立分支或生成过程产物。任务的后续补充沿用同一 Issue，避免重复创建；不得用虚构编号、日期或无 Issue 的任务名代替。文件暂不可用时先保留可恢复状态并说明阻塞，不能把占位编号当作已创建的 Issue。
- `issues`文件夹下面的`issue-01-overall-management.md`是长期总管，维护具体任务索引、依赖及验收状态。具体任务在自身 Issue 中引用 `#1`，并更新总管索引；除总管本身的维护外，不把所有任务归到 #1。总管长期保持开放，子任务完成、合并或清理不关闭总管。除 #1 总管外，通用规范用 `<issue-id>` 表示具体任务编号。实际编号从当前仓库的本地 Issue 取得，参考仓的编号只用于追溯其自身的实现。
- 创建 Issue 和维护本任务及总管索引属于本项目任务流程；对于Github 如果产生提交、推送、PR、软件卸载及其他动作仍须符合当前请求的授权范围，并且创建对应的Github Issue。Issue 创建后记录其编号和链接，后续分支、提交与目录都使用该真实编号；跨仓库提交使用 `Refs othercat/repository#<issue-id>`。

## 修改与Github分支

- 修改前读取适用规范，检查当前分支、HEAD 和工作区状态，保留已有改动。采用最小、可回滚的 diff。
- 本地 Issue 开发及重开后的修复，默认从最新 `dev` 创建新分支，统一命名为 `richardli/<feat or fix>/<issue-id>-<issue-title>`：新功能用 `feat`，缺陷修复用 `fix`，Issue 编号不带 `#`，标题转为简短、可读的分支 slug（空格改为 `-`，移除 Git 不允许的字符）。先核实实际远端与基线；`dev` 不存在时先确认基线，不擅自把 `main` 当作 `dev` 或创建替代分支。
- worktree 仅在任务明确要求或确有隔离需求时创建，记录对应 Issue、分支和路径。分支名与 worktree 路径是临时入口，提交历史与 Issue 记录才是长期追溯依据。
- 本规范不自动授权暂存、提交、推送、开 PR 或批量清理历史分支；按当前任务授权执行。推送前确认 GitHub 仓库 description 非空。

## 项目规范与本地入口

- 通用角色的 Skill 源码位于 `<ROOT>/agent-roles/roles/<角色名>/`，由该仓库 `roles.md` 登记；Cardinal 项目专用 Skill 和 MCP 沿用 `skills/`、`mcps/`。
- AGENTS 入口及对应 Skills/mcps 属于跨设备 Agent 配置，需要随产品仓库提交到 GitHub。
- 排除规则只保护未跟踪文件，不能自动撤销已跟踪内容或阻止 `git add -f`。提交前核对 `git ls-files` 和暂存 diff，确认没有将入口、技能副本、个人绝对路径或本地产物混入产品提交。新 clone 和独立 checkout 必须重新应用本地排除规则；worktree 应通过 Git 查询其共享 exclude 路径。
- 本仓库的本地 `core.symlinks` 固定为 `true`。初次 clone 使用 `git clone --config core.symlinks=true ...`；已有仓库修改配置前先备份。创建入口后验证链接类型、相对目标及读回内容，不能把仅含目标路径的普通文本文件当作有效链接。

## 平台脚本与任务提示词

- 新建平台入口时，先从本仓总管索引取得所属任务的真实 Issue 编号。脚本放在 `scripts/`，采用 `issue-<编号>-<任务短名>-macos.zsh` 或 `issue-<编号>-<任务短名>-win.ps1`。两端共用的命令行实现放在 `tools/`，平台入口负责定位解释器或工具、转交参数和返回退出码。
- `prompts/` 的直接子目录统一命名为 `issue-<issue-id>-<workflow-slug>/`。`<issue-id>` 取自本仓所属 Issue，用十进制正整数，不带 `#`、不补前导零，即使 `issues/` 文件名补零也一样。`<workflow-slug>` 描述具体工作流，使用小写英文字母、数字和单个连字符分词；有对应的 Skill 或入口时优先沿用其工作流短名。
- 一个目录对应一个工作流。同一 Issue 涵盖多个工作流时，使用同一编号分别建目录，不以项目名或宽泛的 Issue 总标题合并收纳。同一工作流的不同平台、输出格式或执行步骤共用目录，用文件名区分；该工作流的最小示例输入也可随提示词放置。
- 可重放的提示词保存为 UTF-8 Markdown，由实际入口或任务包按项目相对路径读取。维护时逐项核对「入口或任务包 → 提示词文件 → 工作流目录 → 本仓 Issue」，并从分发副本验证引用可读。Skill 保存可复用的工具用法、平台模板和纠错规则。
- 新增或改写上述平台启动器时，随任务交付生成提示词、含已审定脚本模板的 Skill、宿主生成命令和可直接使用的脚本。通过本机模型加载该 Skill 生成脚本，先将输出写入任务工作区。宿主核对真实 Skill 回执、Harness 成功结束、实际使用的 MCP 回执，以及生成文件与已审定模板的 SHA-256，再将匹配文件发布到约定路径。已有不同内容时保留并报告差异。
- 验收时从缺少入口的目录重生成并回读脚本，在所属 Issue 留下命令与回执。改名或调整目录时同步更新调用方、任务包、文档和发行清单；从仓库外验证可分发入口及其依赖，在可用平台检查参数和退出码，对端未实测的部分单独登记。

## 过程和交付产物位置

- 每个项目都有自己的中间产物和交付产物目录，<PROCESS-ROOT>为<PROJECT-ROOT>下面`process-root`文件夹为过程产物根目录，如无文件夹可新建；<DELIVERY-ROOT>为<PROJECT-ROOT>下面有`delivery-root`文件夹，作为交付产物根目录。过程和交付产物的目录即子内容都要加入`.gitignore`排除

- 产品源码留在正式项目 checkout；临时脚本、构建中间产物、日志、截图、验收证据和同步备份和其他所有过程产物优先放在<PROCESS-ROOT>目录下已有的分类目录中，如果分类目录没有合适的类别，可以新建类别放入，确保全部囊括。注意中间产物临时产生的`simulated-output`下的`artifacts`文件夹，也直接放入<PROCESS-ROOT>根目录下，并重命名`simulated-outputs`，命名方式为``<PROJECT-NAME>-issue-<issue-id>-artifacts`,避免多层目录。中间产物产生的`offline-model`以及下面的子文件夹，也直接放入`<PROCESS-ROOT>`根目录下，尽可能把session统一归类到具体的harness工具下。如`<PROCESS-ROOT>\runtime\<PROJECT-NAME>-issue-<issue-id>\offline-model\dsh-home\sessions`要调整为`PALWork\offline-model\dsh-home\sessions`。

- 除最终发行物外，每个过程分类目录下直接使用` <PROJECT-NAME>-issue-<issue-id>` 作为任务目录，再按需要追加任务名、时间或分支 slug，避免并行任务覆盖；按需创建，不预建空目录。`<issue-id>` 是对应任务的真实本地 Issue 编号，不带 `#`；不能继续用单独的 ``<PROJECT-NAME>` 项目目录、`<issue-or-task>` 或默认 #1 收纳所有任务。3060主机分类约定参考（下列带占位符的路径是目录模板）：

```text
<PROCESS-ROOT>\artifacts\<PROJECT-NAME>-issue-<issue-id>\<task>\<timestamp>   # 报告、日志、截图、验收证据
<PROCESS-ROOT>\staging\<PROJECT-NAME>-issue-<issue-id>\<task>\<timestamp>     # 下载、解包、临时脚本、构建中间产物
<PROCESS-ROOT>\runtime\<PROJECT-NAME>-issue-<issue-id>\<task>                 # 本地运行与实机验证目录
<PROCESS-ROOT>\worktrees\<PROJECT-NAME>-issue-<issue-id>\<branch-slug>       # 已授权创建的临时 worktree
<PROCESS-ROOT>\sync-backups\<PROJECT-NAME>-issue-<issue-id>\<timestamp>     # 配置、规范入口与迁移备份
```

- 完整游戏发布包统一为 `<DELIVERY-ROOT>/releases/<PROJECT-NAME>-release-r<N>-YYYYMMDD/`，包括旧版、独立变体和撤回版；一r一包，入口在版本根目录，同名ZIP平行，ZIP内仅同名目录。完整游戏不能因旧版/暂缓等状态转入artifacts。
- 完整包之外的素材、源码、交接资料与证据全部归 `<DELIVERY-ROOT>/artifacts/<PROJECT-NAME>-issue-<issue-id>/`；合集内的完整游戏分离登记。随包Tools组件、许可/必需源码及回滚文件保留。日常最大r加1，用户授权迁移可按历史重编号；按最终目录重打ZIP并逐条校验，相同完整副本归并，保存原ZIP身份和条目映射，包内文件/存档字节及验收状态保持。

- 建立该约定前的历史产物保留原位置，由总管 #1 记录范围和原路径，供已有报告与回滚记录追溯；新任务从创建时使用上述 Issue 目录。确需迁移历史产物时，先核对引用和使用者，再记录新旧位置与验证结果，不因本约定批量搬移、改写历史证据或创建兼容链接。
- 工具可配置输出路径时显式使用对应外部目录；必须生成在 checkout 内的缓存按最小范围本地忽略，完成后清理本任务新增的无用项。可复用的产品脚本、测试和正式文档仍按产品需要管理，不因“过程产物”规则丢失必要源码。
- 暂存副本的递归清理被自动审查拦下时，保留拒绝记录，在副本所属的本地 Issue 登记待清理的具体相对路径和完整保留件。先通过本项目的只读复核入口比较两边的目录清单、文件大小与 SHA-256；使用本地 Agent 时，加载相应 Skill，由只读 MCP 返回清单，宿主独立复核。只有内容完整一致且已有对应清理授权时，才按已核实的目标逐件清理。完成后确认目标消失、保留件仍可读，将结果和证据回填所属 Issue。缺少可用复核入口、出现差异或权限仍被拒绝时保留现场。
- Issue 中记录验收结论、对应提交/构建和证据标识；只有本地证据时注明存放位置与可访问边界，不能把本机路径当作 GitHub 上可访问的链接。删除临时 worktree 不得顺带删除其验收证据或备份。

## 默认普通合并

- 默认采用保留原始提交的普通 merge。合入 Issue 分支时使用 `git merge --no-ff <Issue分支>`，为每个 Issue 保留明确的合并节点；使用 GitHub 时选择 **Create a merge commit**。
- 不默认采用 Squash and merge 或 Rebase and merge。只有大量临时提交等确有整理价值的个别分支才考虑 Squash，并在 Issue 中记录原因、原实现提交与最终 squash 提交的对应关系。
- 普通 merge 后，原始提交及合并提交进入目标分支的历史；删除 Issue 分支不会删除仍可从目标分支到达的提交。不得通过重写目标分支历史破坏这条追溯链。
- Squash 不保留原始提交的祖先关系；仅记录旧 SHA 不保证旧对象长期存在。确需保留原过程时另存可恢复的 Git bundle 等归档，不能声称 squash 后仍可仅凭目标分支恢复全部原始提交。

## Issue 与提交关联

- Issue 相关的实现提交、后续修补提交、合并提交及 PR 描述（如有）使用普通引用，如 `Refs #<issue-id>`；跨仓库引用写 `Refs owner/repository#<issue-id>`。提交必须有 subject 和非空 body，body 说明改动、验证结果及 Issue 引用。
- 实机验收前，提交信息和 PR 描述禁止使用会自动关闭 Issue 的 `Fixes`、`Closes`、`Resolves` 及其变体。即使先合入非默认分支，也不能留下以后进入默认分支时会触发关闭的关键词。
- 通过普通引用、提交链接和 Issue 评论记录关联。实机验收前不使用 GitHub Development 中会在 PR 合入默认分支后自动关闭 Issue 的关联，也不启用合并即关闭的自动化。
- Issue 中记录实现提交（完整 SHA 或稳定提交链接）、合并提交、目标分支、验证结果和后续验收状态。发生合并冲突时说明最终处理及复验结果，不能只留下已删除的分支名或本地路径。
- 每次实现、合并、清理和实机验收后更新对应记录；可使用以下模板，未执行项明确写“未执行”或“待实机验收”，不得把计划写成结果：

```text
实现：<实现提交 SHA/链接；多次修补逐项列出>
合并：<目标分支、合并提交 SHA/链接；如为 squash，补充原提交映射与原因>
验证：<实际测试的提交/构建、环境、命令或操作、结果、未覆盖范围>
清理：<Issue 分支、worktree；完整合入检查结果及清理状态>
实机验收：<待验收 / 测试人员、日期、设备、步骤、结果、证据>
Issue 状态：<保持开放 / 实机通过后关闭 / 复现后重开>
```

## 完整合入后清理

- 实现与必要自动验证已经完成、仅剩直播复测或用户实机验收时，在当前任务已有提交、合并和推送授权的前提下，先提交实现，以 `git merge --no-ff` 合入最新集成分支并推送；无需为等待实机验收长期保留开发 worktree。跨仓库使用各仓已核实的集成分支，不擅自替换其基线。
- 推送后先在原 Issue 写明实现 SHA、合并 SHA、集成分支、测试包版本及 SHA-256、实际验证和待测项，标记“已合入，待实机验收”。未提交文件、未跟踪文件和必要忽略产物须另存可校验的归档并记录恢复位置；Issue 中的 SHA 只能恢复已提交内容。
- 清理与关闭 Issue 分开执行。确认完整合入后删除该 Issue 的临时分支和 worktree；Issue 继续开放，等待实机验收。
- 清理前记录最终分支 tip 与合并 SHA，核对最新远端状态，确认原分支没有新增遗漏提交。普通 merge 至少满足 `git merge-base --is-ancestor <分支tip> <目标分支>` 成功，且 `git rev-list <目标分支>..<分支tip>` 为空；同时检查合并后的实际代码与必要验证，祖先关系本身不能证明冲突处理正确。
- 若涉及已发布分支，确认合并提交和分支 tip 均可从最新远端目标分支到达。只有本地合并、目标尚未推送或远端未核实时，不删除唯一的工作分支与 worktree。
- Squash 例外不能用上述祖先检查证明完整合入，须逐项核对实现范围、最终差异、验证结果和必要归档；任何遗漏或不确定都先保留分支。
- 移除 worktree 前检查已修改、未跟踪和需要保留的忽略文件，确认没有其他任务在使用。所需本地文件先保存，脏 worktree 不强制删除。
- 先移除 worktree，再删除本地分支和存在的远端 Issue 分支；普通 Git worktree 使用 `git worktree remove <已核实路径>`，本地分支优先使用 `git branch -d <Issue分支>`。受客户端管理的 worktree 使用其归档/清理入口。命令拒绝时先调查，不用 `--force`、`-D` 或递归删目录绕过保护。
- Windows 清理前核实解析后的绝对路径位于本次预期目录。不得删除当前工作目录、集成分支、默认分支或其他任务的分支；完成后将清理结果写回 Issue。
- 后续优先将已完成工作提交并合入集成分支，再开展下一个 Issue，减少未提交改动跨 worktree 复制。明确暂缓或尚有实现缺口的工作独立记录，保留可恢复源码，不把它标记为已完成合入。

## 实机验收与重开

- 合并成功、编译通过、自动测试通过或删除分支均不等于实机验收通过。Issue 保持开放，直到用户确认或已授权的实机测试给出可核对的通过证据。
- 实机验收记录实际运行的提交/构建、设备与运行环境、复现/回归步骤、结果及证据；确认原问题和相关回归通过后才关闭 Issue。尚无法运行实机测试时写清待测项，不提前关闭。
- 直播或实机复测发现问题时（包括尚未关闭的 Issue），先从记录的实现与合并提交分析原因，再从最新 `feature` 创建新的修复分支，使用原 Issue 的 `Refs` 引用；跨仓库遵循其已核实的集成分支。已关闭的原 Issue 先重开。旧提交用于追溯，新修复基于当前代码；无需恢复或长期保留旧 Issue 分支。

## 官方依据

- [Git merge：普通合并与 --no-ff](https://git-scm.com/docs/git-merge)
- [GitHub：Issue 关联与自动关闭规则](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue)
- [fnm：安装与 shell 初始化](https://github.com/Schniz/fnm#readme)

---

# Cardinal 项目约定

## 交流、授权与委派

- 默认中文；代码标识、关键词、特殊术语与已有代码注释风格保留。先说明复杂任务的短计划，报告结果、验证与未覆盖边界。
- 遵循用户与客户端的模型选择，不在 AGENTS/Skills 指定模型或推理档位。
- 按需委派至多一个独立 subagent；明确只读/可写文件边界、验证与禁止项，不重复调查，同一时间最多一个可写 agent。更多或嵌套委派需用户明确要求。主 agent 负责整合与最终验证。
- 不输出密钥、token、cookie、session、私钥或证书，不修改无关的认证、provider、telemetry 或机器权限字段。
- 可逆、已授权的范围内工作直接执行；不擅自暂存、提交、推送或开 PR。没有 `dev` 时核实实际基线并记录，不创建替代集成分支。本地改动可先保留在现有工作区。
- 只有用户明确 `/goal` 或要求建立 Goal 才创建 Goal；当前请求决定范围，历史记录不扩大授权。

## 平台与目录

- 当前产品仅支持 Apple Silicon（`aarch64-apple-darwin`）、macOS 15.0 及以上；不为 Intel、Windows 或较旧 macOS 增加兼容工作。
- 交付独立的 `Cardinal Local.app`（bundle ID：`com.othercat.cardinal`），与官网下载版分开保存状态。其他 M 系列 Mac 运行时不要求 Python、Node.js、Rust 或 Homebrew；分发前核对动态库依赖与应用内资源。
- 桌面应用在 `cardinal/`：React UI 为 `src/`，Tauri/native 为 `src-tauri/`。`cardinal/dist/` 为生成物。
- 根 Rust workspace 包括 `lsf/`（CLI）、`cardinal-sdk/`（FSEvents/共享类型）、`fswalk/`、`fs-icon/`、`file-tags/`、`namepool/`、`query-segmentation/`、`search-cache/`、`search-cancel/`、`cardinal-syntax/`、`slab-mmap/` 和 `was/`（事件 CLI）。Tauri 有独立 Cargo workspace/lock。
- 正式架构文档在 `doc/inner/`，用户与搜索语法文档在 `doc/pub/`，本地任务在 `issues/`。按任务读取相关代码和引用，勿无故批量改写历史文档。
- 遵循上述 `process-root/`、`delivery-root/` 和真实 Issue 编号规则；`target/`、`cardinal/dist/`、依赖和 vendor 生成物不提交。桌面发行物放 `delivery-root/releases/`，构建记录包含命令、平台、SHA-256。

## 环境、构建与验证

- 安装依赖、运行脚本、构建、测试、启动服务前使用 `$python-and-node-assistant` 核对 Python/Node.js 声明和锁，并复用回执的绝对入口；公共环境优先，不创建项目 `.venv`。环境与依赖变更由该角色办理。
- Rust 以 `rust-toolchain.toml` 为准，当前 `nightly-2025-12-11`，组件为 `rustfmt`、`clippy`；不得照搬过时文档中的版本。使用锁文件，不无故升级依赖。
- `cargo check --workspace` 验证共享 crates；共享 crate 修改后运行 `cargo test --workspace`，查询/索引还需覆盖 `search-cache`、`lsf`。Tauri 在 `cardinal/src-tauri/` 单独验证。
- `cargo clippy --workspace --all-targets` 做 lint；仅格式化本次修改的文件，避免全仓无关格式化 diff。
- 前端在 `cardinal/`：角色安装冻结依赖后，`npm run typecheck`、`npm test -- --run`、`npm run build`；必要时使用 `npm run format:check` 或只检查改动文件。
- 开发：`npm run tauri dev -- --release --features dev`；发行：`npm run tauri build -- --target aarch64-apple-darwin --bundles app`。确认产物架构及最低系统版本。
- 修改前定义与风险相称的成功标准；测试行为与边界。性能改动保留可重复的前后基准，区分合成数据与实机数据，不把未运行的检查写成通过。
- 不覆盖已安装应用或现有索引；需要试运行时隔离状态，核对数据路径、权限和回滚方式。

## 代码与评审

- 保持最小、可回滚 diff，不为美化架构扩大范围，不批量重命名、格式化或引入大型依赖。
- Rust：4 空格，模块/函数 `snake_case`、类型/trait `PascalCase`，沿用 grouped imports；可失败 API 优先 `anyhow::Result`，日志用 `tracing`。
- React：组件 `PascalCase.tsx`，hooks/utils 导出 camelCase，目录沿用 kebab-case；保持 Vite 和 tsconfig 的有效配置。
- 单元测试与代码相邻；跨模块用各 crate 的 `tests/`。大结果集验证排序正确性、分页、取消与实时更新；按需检查 UI 响应、内存和 CPU。
- 如获提交授权，使用 Conventional Commits，subject 与非空 body；body 说明修改、验证和 `Refs #<issue-id>`。默认普通 merge，原项目的默认 squash 约定被上游规则替代。
- 如获 PR 授权，列实际 cargo/npm 验证、Issue、相关 UI 截图与索引/搜索/图标风险；不把本机证据路径当成 GitHub 可访问链接。
