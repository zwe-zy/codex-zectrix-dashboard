# Windows 发布版 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans for inline execution, or superpowers:subagent-driven-development if the user selects delegated execution. Steps use checkboxes for tracking. Never modify the live installation before candidate verification.

**Goal:** 交付可从 GitHub 下载、普通用户自行配置并可靠运行的 Windows x64 NOTE4 指示牌候选版本。

**Architecture:** 保留 Rust 观察、渲染与上传核心。Windows 安装、后台控制和自动启动各自封装；安装及配置变更使用事务与操作锁，数据观察按能力和额度窗口分别降级。首次配置、升级和卸载共用同一后台控制与恢复规则。

**Tech Stack:** Rust 2024、现有 Cargo.lock、windows-sys、Windows 凭据管理器、PowerShell 打包脚本、GitHub Actions。依赖升级仅限实现需要的 API，不混入无关升级。

**Spec:** [Windows发布版设计](../specs/2026-10-21-windows-release-design.md)，已由用户确认修订范围。实施与本文冲突时以设计为准并记录调整。

## Global Constraints

- 只交付 Windows x64；保留 macOS 代码，不在本轮新增 Linux/macOS 可用性承诺。
- 普通新用户安装不触发 UAC；旧计划任务迁移可单独请求系统确认。
- 数据根目录为 `%LOCALAPPDATA%\CodexZectrixDashboard`；程序位于 `app\<version>\`。
- 凭据 service 为 `com.barrybarrywu.codex-zectrix-dashboard`，account 为 `zectrix-api-key`。
- HKCU Run 值名为 `CodexZectrixDashboard`；迁移对象为经过归属核验的旧任务 `Codex ZECTRIX Dashboard`。
- 任务轮询 60 秒、额度轮询 600 秒；过期阈值分别为 180 秒、1800 秒；上传至少间隔 60 秒。
- Key 不进入参数、日志、JSON、测试快照或发布包。CI 使用 fake server 和内存凭据。
- 手动下载 ZIP 升级；不实现在线自动更新或完整图形界面。
- 原备份、已运行版本和未提交源码修改必须保留；不自动推送到原作者仓库。
- 每项业务变更先写能复现问题的测试，观察失败，完成实现后通过对应测试，再提交；不为纯文案添加镜像测试。

## Review Focus

1. 中文、空格、特殊字符及超长安装路径：正确引用或清楚拒绝，不执行意外命令。任务 2、3、7 覆盖。
2. 双击两次、配置与升级并发、旧进程未退出：不得双刷或交叉覆盖数据。任务 2、3、4 覆盖。
3. 进程在提交中途退出、磁盘写失败、凭据写失败：恢复到已知一致状态，未知时停止上传。任务 3、4 覆盖。
4. 无关额度组异常、只有一个有效窗口、任务来源不支持：局部降级，不显示虚假的正常或零值。任务 5 覆盖。
5. Codex 更新删除原路径、重启首个内容已变、时钟变化：重新发现、重新分类，保持退避和无补图循环。任务 5、6 覆盖。

## 工作目录和记录

源备份：用户保留的原项目备份目录；具体本机路径不纳入公开文档。

实施副本：本聊天 `work\windows-release\project`，使用独立 Git 分支；从备份复制或独立克隆并完整保留未提交改动，禁止以旧 HEAD 代替当前源码。交付时将审阅所需源码包、程序包及验收记录放入 `outputs`。

在副本中将设计和计划放入 `docs/superpowers/specs/2026-10-21-windows-release-design.md` 和 `docs/superpowers/plans/2026-10-21-windows-release.md`。执行记录保存在本计划专属工作目录，记录基线、失败测试、通过测试、提交及必要决策；不要在本轮源码中提交个人设备数据。

## Task 1 恢复可复现的构建基线

**Files:** 检查 `Cargo.toml`、`Cargo.lock`、`.git`、`tests/`；新增 `docs/adr/0010-windows-standalone-distribution.md` 记录已批准的 Windows 独立发布方式。

**Interfaces:** 产出可构建的工作副本、基线 commit、实际工具链版本记录；不改变运行行为。

- [ ] 复制含未提交修改的源快照，核对 `.rs`、清单和锁文件与原件相同，确认分支不为 main/master；将快照作为明确的实施基线记录。
- [ ] 检查 Rust MSVC 工具链和 Windows 构建工具。缺失时安装官方构建依赖，并记录实际版本；不把开发依赖带进用户包。
- [ ] 执行 `cargo test --locked --all-targets --features test-support`，保存原始结果。若失败，先区分环境失败和原代码失败，修正环境后重跑，不跳过测试冒充基线通过。
- [ ] 执行 `cargo build --locked --release --bin codex-zectrix-dashboard --bin codex-zectrix-companion`，运行离线 preview 并查看图像；此步骤不连接设备。
- [ ] 添加 ADR，明确对 ADR 0001 的 Windows 分发补充，保留原署名、凭据和 Rust 决策。

## Task 2 普通权限自动启动与后台控制

**Files:** 修改 `src/autostart.rs`、`src/companion.rs`、`src/platform/mod.rs`、`Cargo.toml`；新增 `src/platform/background_windows.rs`、`tests/windows_background.rs`；更新 `tests/windows_autostart.rs`。

**Interfaces:** 保留 `AutostartControl::{install, uninstall, status}`；新增 `BackgroundController::new(data_dir: &Path, executable: &Path)` 和 `start() -> Result<BackgroundStatus, BackgroundError>`、`stop() -> Result<(), BackgroundError>`、`status() -> Result<BackgroundStatus, BackgroundError>`。`BackgroundStatus` 至少区分 Stopped、Starting、Running；无权检查通过错误返回，不能冒充 Stopped。操作锁由 `OperationGuard::acquire(data_dir: &Path)` 持有，RAII 释放。

- [ ] 写失败回归：普通权限注册 HKCU Run，不调用 schtasks；引用含空格和特殊字符的绝对路径；完整命令超过 260 字符明确失败；注册失败不能被误报为已设置。
- [ ] 关键测试命名为 `run_registration_needs_no_scheduler`、`second_instance_never_publishes`、`maintenance_blocks_start`、`stop_rejects_reused_pid`；分别断言 scheduler 调用数为 0、第二实例上传数为 0、维护中 start 返回 Busy、PID 身份不符时终止调用数为 0。
- [ ] 写后台行为回归：第二实例不发出 RPC/上传；维护期间启动受阻；stop 等待指定实例退出；陈旧 PID 或其他 EXE 不被终止；控制窗口退出不结束后台。
- [ ] 执行 `cargo test --locked --features test-support --test windows_autostart --test windows_background`，确认上述行为在现有实现失败。
- [ ] 实现 Windows registry 后端和正式数据目录范围内的 OS 独占锁。后台标识带实例代号，控制端同时核验进程身份；停止通过本产品退出请求处理，不使用按名称批量杀进程。
- [ ] 后台主循环纳入退出检查，等待期间可响应停止；app-server 子进程仍由现有 Job Object 回收。添加本地就绪信号，避免仅以 PID 存在判为启动成功。
- [ ] 再跑该测试组及 `cargo test --locked --features test-support --test companion_scheduler --test app_server_client --test platform`；全部通过后提交。

## Task 3 固定安装目录和可恢复升级

**Files:** 新增 `src/install/mod.rs`、`src/install/transaction.rs`、`src/install/windows.rs`、`tests/windows_install.rs`、`tests/install_recovery.rs`；修改 `src/lib.rs`、`src/settings.rs`，仅对本产品安装状态增加独立版本格式。

**Interfaces:** 消费任务 2 的控制器与操作锁。新增 `InstallManager::install_or_update(package_dir: &Path) -> Result<InstallOutcome, InstallError>` 和 `recover() -> Result<RecoveryOutcome, InstallError>`。安装管理器通过可注入的文件、自动启动和后台控制边界测试；生产数据使用 `Installation { schema_version, active_version, previous_version }` 与不含密钥的事务阶段记录。

- [ ] 写失败回归：复制后的安装版本与原 ZIP 解压目录无依赖；混版本双 EXE、错误平台、缺文件和校验失败均不改变已安装版本。
- [ ] 写迁移回归：旧任务所有者或动作不匹配则不触碰；停用被拒绝、UAC 取消、旧后台无法退出时，新 Run 项不生效、新后台不启动。
- [ ] 写恢复回归：每个提交边界中断后重新启动，可恢复程序指向和旧版可读数据；未知 schema 清楚失败；新程序无法就绪则恢复旧版本；网络失败不引发无限升级回滚。
- [ ] 写删除边界回归：安装清单之外的文件、路径穿越、目录链接和意外解析路径不得被清理。
- [ ] 关键测试 `legacy_stop_denied_preserves_old_install` 断言新 Run 项不存在、新后台启动数为 0；`interrupted_upgrade_restores_old_readable_state` 用旧格式解码恢复文件并比对原值；`cleanup_rejects_link_outside_install` 断言目标外的 sentinel 文件不变。
- [ ] 执行 `cargo test --locked --features test-support --test windows_install --test install_recovery`，确认失败原因是缺失上述功能。
- [ ] 实现版本目录、白名单快照、原子事务日志及启动时恢复。快照发生在旧后台确认退出后；程序升级不改写 Key。旧任务迁移按照方案先停旧、后验证新、最后清理旧入口。
- [ ] 通过同一测试组及任务 2 测试后提交；不在这一步替换本机正在使用的版本。

## Task 4 配置事务和中文操作入口

**Files:** 修改 `src/setup.rs`、`src/main.rs`、`src/credentials.rs`；新增 `src/operator.rs`、`tests/operator_cli.rs`、`tests/reconfigure.rs`；更新 `tests/setup_flow.rs`、`tests/setup_cli.rs`。

**Interfaces:** 复用 `CredentialStore`、`SetupFileStore`、`AutostartControl` 和任务 2 的控制边界；`run_setup_transaction` 通过参数显式注入后台控制，不在业务函数内直接启动真实进程。新增 `run_operator_menu()` 作为 Windows 无参数入口，命令行参数入口继续保留。`SetupOutcome` 区分配置完成、取消、配置完成但启动项设置失败；失败细节不得含服务原始响应或 Key。

- [ ] 写回归：输入阶段取消不改旧服务；正式提交先停后台；提交失败恢复旧设置及原运行状态；自动启动失败保留配置和 Key；换路由不复用旧图片去重状态。
- [ ] 写中断回归：凭据和文件提交之间出现未提交记录时，后台禁止上传，菜单进入恢复向导；凭据快照和任何密码值都不进入日志或磁盘事务文件。
- [ ] 写 CLI 回归：无参数进入菜单；缺少 Codex、登录不可用、错误 Key、无设备、网络失败和上传结果未知给出不同提示；多设备名称可辨认；错误后窗口可读、可重试。
- [ ] 关键测试 `autostart_failure_keeps_committed_route` 断言新 settings 和凭据仍存在；`reconfigure_stops_old_route_before_upload` 断言旧后台退出事件早于新设备上传；`credential_commit_interrupted_blocks_publish` 断言恢复前上传数为 0。
- [ ] 执行 `cargo test --locked --features test-support --test setup_flow --test setup_cli --test reconfigure --test operator_cli`，观察新断言失败。
- [ ] 实现菜单、首次安装入口、配置提交协调及成功后立即启动；自动启动设置独立于设备事务。测试图增加“测试”标记。已发生的云端显示不承诺撤回。
- [ ] 通过上述测试及凭据边界测试；测试数据包含特殊 Key 标记，断言 stdout/stderr/文件均无该标记，之后提交。

## Task 5 Codex 兼容性与双窗口额度

**Files:** 修改 `src/app_server.rs`、`src/codex_status.rs`、`src/companion.rs`、`src/status_board/model.rs`、`src/status_board/render.rs`；新增 `src/codex_discovery.rs`、`tests/codex_discovery.rs`；更新 `tests/codex_status.rs`、`tests/companion_scheduler.rs`、`tests/status_board_model.rs`、`tests/status_board_render.rs`、`tests/support/fake_codex.rs`、相关 fixtures。

**Interfaces:** `CodexLocator::resolve() -> Result<ResolvedCodex, DiscoveryError>` 共用于向导、后台和状态检查；`ResolvedCodex` 包含路径和是否用户指定。将单窗口 `QuotaStatus` 升级为 `QuotaSnapshot { bucket_id, primary, secondary }`；每个 `QuotaWindowStatus` 独立保存 availability、remaining_percent、window_duration_mins、resets_at 及观察时间。任务状态结果含已验证状态与观察能力，不能用全局“成功”替代能力判定。模型和调用点在同一任务中更新，保证提交可编译。

- [ ] 写发现回归：仅 Desktop 安装、没有 PATH；指定路径失效不得默默换账户；自动发现的旧版本路径被删除后重新发现，旧 Job Object 已回收且重连仍有退避。
- [ ] 写额度回归：选 codex 组、legacy 有依据回退、多组旁支畸形不影响目标组；单窗口、双窗口、小数、百分比越界、缺失时长和重置时间；每个窗口独立保留与过期。
- [ ] 写任务回归：请求显式 sourceKinds；父关系缺失不假设顶层；notLoaded 不转完成；已验证运行优先于旧失败；未知时间不能重造近期结束；任务能力失败不影响额度。
- [ ] 关键测试 `malformed_other_bucket_keeps_codex_windows` 使用 codex primary.usedPercent=24.5、secondary.usedPercent=30，断言剩余为 75.5、70；`not_loaded_never_means_completed` 断言 Unknown；`codex_update_rediscovers_only_automatic_path` 分别断言自动路径可换、显式路径返回需修正。
- [ ] 执行 `cargo test --locked --features test-support --test codex_discovery --test codex_status --test companion_scheduler --test status_board_model --test status_board_render`，确认回归失败。
- [ ] 实现选择后解析及逐窗口状态；窗口标签来自真实时长，两个窗口调整布局并保留 400×300 黑白风格。所有观察时间都不能单独触发新图片上传。
- [ ] 用明确的 fake 协议验证能力判定；通过测试后生成各状态预览并查看，修正裁切和重叠。先记录本机 Desktop 语义实测所需版本和情景，不把 fake 测试称作 Desktop 验证；提交。

## Task 6 发布语义和时间恢复

**Files:** 修改 `src/publisher.rs`、`src/settings.rs`、`src/companion.rs`、`src/setup.rs`；更新 `tests/publish_coordinator.rs`、`tests/publisher.rs`、`tests/companion_scheduler.rs`。

**Interfaces:** `StatusFramePublisherState` 新增独立格式版本、内容来源类型、最近成功的归一化内容指纹，以及 `last_successful_http_upload_epoch_seconds`。现有真实内容成功时间维持独立语义；旧字段到新字段通过显式迁移，不直接扩写给旧版读取。指纹排除观察/上传时间及所有敏感字段。

- [ ] 写回归：setup 测试图后首次真实状态须 Classified SourceChange；重启首次观察已变同样按内容更新；纯补图只更新 HTTP 时间；来源未变无重复上传；新路由不用旧指纹；旧格式缺指纹只保守更新一次。
- [ ] 关键测试 `first_observation_after_restart_is_new_content` 在 100 秒内容成功、161 秒重启新内容成功后断言内容时间为 161；`timestamp_followup_changes_http_time_only` 在 222 秒补图后断言内容时间仍为 161、HTTP 时间为 222，之后无第三次仅时间上传。
- [ ] 执行 `cargo test --locked --features test-support --test publish_coordinator --test publisher`，验证已有恢复分类暴露失败，同时保留已有重试、凭据拒绝和 coalesced 测试。
- [ ] 实现持久来源指纹与迁移、内容成功和 HTTP 成功的独立记录；重建 pending 时先核对来源和内容，再判断是补图还是变化。保持 60 秒节流与既有退避。
- [ ] 同一测试组与 scheduler 测试通过后提交；检查真实内容时间、HTTP 时间、数据时间的标签在菜单和文档一致。

## Task 7 Windows 发布包和使用说明

**Files:** 新增 `scripts/build-release-windows.ps1`、`scripts/test-windows-package.ps1`、`.github/workflows/windows.yml`、`docs/windows.md`；修改 `README.md`、`Cargo.toml`、`Cargo.lock`；新增 `tests/windows_package.rs`。

**Interfaces:** 构建脚本从 Cargo 版本生成 `codex-zectrix-dashboard-<version>-windows-x86_64.zip`，包中含主程序、companion、LICENSE、中文快速入门、版本与平台 manifest；manifest 记录配套文件 hash，外部 SHA256SUMS 校验 ZIP。初版候选版本采用 `0.2.0-rc.1`，正式版需验收后另打标签。

- [ ] 写包验证回归：缺 companion、错误 hash、混版本、额外 settings/日志/凭据样本均被拒绝；解压到中文和带空格目录，离线预览成功。验证运行的是包内 EXE。
- [ ] 关键测试 `package_rejects_mismatched_companion`、`package_excludes_local_state`、`packaged_preview_runs_from_unicode_path` 分别断言验证失败、配置样本未打包，以及包内程序退出码为 0 且输出 PNG 为 400×300。
- [ ] 执行 `cargo test --locked --features test-support --test windows_package`，确认错误包不能被现有流程误放行。
- [ ] 实现锁文件构建与打包，不把 target、Git 元数据、真实缓存及 dist 旧包带入发布件；快速入门提供 Key 获取位置、安装、重新配置、停止、升级、卸载和错误处理。
- [ ] Windows CI 执行 fmt、clippy、完整测试、release build 和真实包冒烟测试；PR 不需要 secrets。标签生成候选 artifact，实际发布步骤单独明确，不能因加入工作流就报告远端已通过。
- [ ] 执行 `cargo fmt --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`cargo test --locked --all-targets --features test-support`、`powershell -NoProfile -File scripts/build-release-windows.ps1`、`powershell -NoProfile -File scripts/test-windows-package.ps1`；通过后提交。

## Task 8 独立审阅和真实验收

**Files:** 交付 `outputs` 下候选 ZIP、源码快照和 `Windows发布验收记录.md`。记录包含每项结果、实际版本、复现步骤和未验证项；不包含密钥和原始私人任务载荷。

**Interfaces:** 消费前述最终候选包和基线到最终提交的 diff；不把旧 0.1.9 的成功计入新版验收。

- [ ] 完成一次独立全分支代码审阅，聚焦并发变更、权限、回滚、进程生命周期、状态含义及打包。对实质问题先写失败回归再修复，重跑相关检查；记录未采纳意见及原因。
- [ ] 在隔离数据目录用 fake cloud/凭据做新用户安装和故障注入。确认默认安装不调用需要管理员权限的操作；确认二次安装和中断恢复。
- [ ] 候选通过代码及包检查后，备份当前非密钥配置，按迁移流程替换本机版本，保留回退路径；出现失败立即停止切换并恢复。
- [ ] 验证 NOTE4 实际显示、Desktop 任务状态对照、双额度、断网恢复、重新配置、程序重启、登录启动和休眠唤醒。需用户参与或无法执行的场景标明未验证，不以脚本模拟替代。
- [ ] 确认真实 GitHub 下载包的签名/SmartScreen 体验，以及未参与开发者独立配置。尚无独立使用者时只交付明确标注的候选版，并列出剩余验收，不伪造通过。
- [ ] 汇总交付和剩余限制；GitHub 目标仓库未确定前不创建仓库、不推送、不发布。源码及本地包先完整可审阅，再办理实际发布。

## 计划自检

- 设计的七项审阅意见分别由任务 2–6 的实现及测试承接；任务 7–8覆盖发布与实机边界。
- Task 2 的操作锁和后台控制由 Task 3、4 消费；不能各自实现不互通的锁。
- Task 5 的额度模型由 Task 6 指纹使用；生成指纹时排除独立观察时间，避免每次轮询都上传。
- Task 3 的快照迁移须包含 Task 6 最终的发布状态格式，在 Task 6 完成后补跑 Task 3 的旧版恢复测试。
- 本计划保留最初的验收清单；执行结果以 `docs/windows-acceptance.md` 和 `docs/windows-release-review.md` 为准。Windows 候选实现、233 项测试及本机运行验收已完成；需独立设备或使用者参与的项目仍明确列为待验收。
