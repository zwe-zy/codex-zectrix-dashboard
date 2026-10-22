# Windows 完整控制台 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付可由普通用户双击安装和配置的 Windows 中文完整控制台，保留现有五张墨卡实时更新。

**Architecture:** GUI 调用结构化桌面操作服务，CLI 复用同一配置与安装事务。独立 companion 仍是唯一持续观察和上传的进程，GUI 从有限大小的本地快照及成功上传图片缓存读取状态。

**Tech Stack:** Rust 2024；现有 Rust 1.99.1 MSVC 工具链；egui/eframe 固定 `=0.37.0`，新增可选 `desktop` feature，启用原生 wgpu、AccessKit 和内置西文字体；现有 image/png、serde、keyring、zeroize。中文嵌入 Google Fonts 官方 Noto Sans SC，保留 OFL、来源修订和 SHA256；不复制系统字体。

**Spec:** `docs/superpowers/specs/2026-10-22-windows-desktop-console-design.md`（用户已确认）。

## Global Constraints

- 本轮交付 Windows x64 图形版；Linux/macOS 安装、凭据、启动和实机验收留待后续版本。
- 左侧四个页面：运行总览、墨卡与主题、设备配置、设置与诊断。
- 窗口默认约 1040×760；100% / 150% / 200% 缩放下可操作；跟随系统明暗外观。
- 关闭窗口退出界面进程，独立后台继续运行；开机启动只启动后台。
- 五页固定为 1 怪盗、2 EVA、3 掌机、4 仪表、5 HUD；真实窗口及时长以接口为准。
- 额度每 10 分钟、任务每分钟读取；每页上传至少相隔 60 秒，设备自身同步周期不变。
- GUI 约每 2 秒读取本地状态；不能另开持续 Codex 会话。
- Key 只保存在既有凭据管理器；不出现在参数、日志、JSON 快照、截图、安装清单或 UI 回显中。
- 保留 settings.json、appearance.json、inkcards.json 及既有安装恢复记录格式；新增缓存与它们分离。
- 正常图形操作不弹黑色菜单窗口；卸载按用户明确选项清理，未知文件不删除。
- 不推送 GitHub；新发布候选采用 `0.3.0-rc.1`，保留 rc.5 作为回退版本。
- 文档路径以仓库根目录为基准；中间日志放仓库外 `../verification/`，最终 ZIP 和验收记录复制至本聊天 `outputs/`。

## Review Focus

- 发现设备后取消或换用新 Key，较晚完成的旧请求不得重新填入设备或保留旧密钥（Task 3）。
- GUI 正在编辑时另一窗口升级，提交必须拒绝旧版本；已开始提交后关闭窗口应等待完成或回滚（Tasks 1、3、4）。
- 系统时间回拨、旧设备快照、PNG 与成功记录不一致，不能误显示实时状态或最近成功图片（Task 2）。
- v1 安装、v2 新包、升级中断和被用户改过的开始菜单快捷方式混合存在时，只恢复或清理本程序可验证的文件（Task 5）。
- 中文输入、200% 缩放、长设备名及窗口缩小，重要操作不可被遮挡；图形初始化失败要有可见错误（Tasks 4、6）。

## 文件边界

| 文件 | 职责 |
| --- | --- |
| `src/setup/commit.rs`（新） | 从现有 setup.rs 抽出不依赖终端文本的配置提交与回滚 |
| `src/desktop_state.rs`（新） | 有限大小的状态快照、图片缓存、有效性与新鲜度判定；不依赖 GUI |
| `src/desktop/service.rs`（新） | 结构化 Windows 操作与凭据会话，不依赖 egui |
| `src/desktop/jobs.rs`（新） | 单工作线程、命令/结果、取消代次、提交期间关闭规则 |
| `src/desktop/app.rs`、`src/desktop/pages/*.rs`（新） | 应用框架和四个独立页面，绑定服务结果 |
| `src/bin/codex-zectrix-desktop.rs`（新） | 图形入口、版本探测、初始化错误提示 |
| `src/platform/shortcut_windows.rs`（新） | 归属检查后的当前用户开始菜单快捷方式维护 |
| `src/install/mod.rs`、`src/install/windows.rs`、`src/operator.rs` | 精确包格式 v1/v2、共享操作、安装/卸载的 GUI 支持 |
| `src/companion.rs`、`src/publisher.rs`、`src/setup.rs` | 写状态/缓存，沿用观察与上传规则，登记清理文件 |
| `assets/fonts/`、`THIRD-PARTY-NOTICES.txt`（新） | 可再分发中文字体、来源和许可证 |

## Task 1: 分离配置提交核心，保持 CLI 行为

**Files:** 新建 `src/setup/commit.rs`；修改 `src/setup.rs`、`src/lib.rs`；新增 `tests/setup_prepared.rs`，沿用 `tests/setup_flow.rs`、`tests/reconfigure.rs`。

**Interfaces:** `PreparedSetup` 持有 `Settings`、`SetupPreview`、`Zeroizing<String>` 及此前预览快照，字段不公开、无明文 Debug；`PreparedSetup::new(settings: Settings, preview: SetupPreview, secret: Zeroizing<String>, previous_preview: Option<Vec<u8>>) -> Result<Self, SetupError>`。新增 `commit_prepared_setup<C: SetupCloud, S: SetupFileStore, A: AutostartControl, K: SetupClock>(draft: PreparedSetup, cloud: &mut C, credentials: &impl CredentialStore, files: &mut S, autostart: &mut A, clock: &K, control: &mut impl SetupControl) -> Result<SetupCommitReport, SetupError>`。`SetupCommitReport` 提供 `outcome: SetupOutcome`、`background_warning: Option<SetupError>`；GUI 使用结构化结果，CLI 继续输出原提示。

- [ ] **1. RED:** 写 `prepared_commit_matches_console_rollback`、`configured_but_start_failed_is_reported_separately`、`cancelled_preview_keeps_previous_state`。复用隔离 mock：失败上传恢复旧 settings/key/preview；启动失败仍得到已配置报告；取消没有上传且旧状态不变。增加配置在另一窗口升级后提交被拒的既有回归。
  ```rust
  assert_eq!(cloud.upload_count(), 0); // 未确认的草稿不能提交
  assert_eq!(files.settings(), previous_settings);
  assert_eq!(report.outcome, SetupOutcome::Configured);
  assert!(report.background_warning.is_some());
  ```
- [ ] **2. 运行：** `cargo test --locked --test setup_prepared`，确认失败来自新接口/断言。
- [ ] **3. 实现：** 迁移当前 `run_setup_transaction` 中确认后的副作用至公共提交函数；维持 credential → settings → 上传 → 状态保存/回滚顺序及 `SetupControl` 保护。发现/选择/终端提示仍由 CLI 外层承担；为 `SetupDevice` 增加只读 `routing_id(&self)->&str`、`display_name(&self)->&str`，名称清理保持现有规则。
- [ ] **4. 验证：** `cargo test --locked --test setup_prepared --test setup_flow --test reconfigure --test setup_cli --test credential_api_boundary` 全部通过；旧测试语义不能因 GUI 便利而削弱。
- [ ] **5. 提交：** `refactor: share structured setup commit between CLI and desktop`。

## Task 2: 提供可信的状态快照和成功画面缓存

**Files:** 新建 `src/desktop_state.rs`、`tests/desktop_state.rs`；修改 `src/companion.rs`、`src/publisher.rs`、`src/lib.rs`、`src/setup.rs`；补充 `tests/inkcards.rs`、`tests/publish_coordinator.rs`。

**Interfaces:** `DesktopSnapshot` 固定 schema 1，含 app_version、settings_fingerprint、written_at、quota_observed_at、task_observed_at、`board: StatusBoardState`。`DesktopSnapshot::load(data: &Path, settings: &Settings, now: i64) -> Result<Option<Self>, DesktopStateError>`；`save_atomically(&self, data: &Path) -> Result<(), DesktopStateError>`。`assess_freshness(&self, now: i64, background_running: bool) -> DesktopFreshness`；`DesktopFreshness { quota: Freshness, task: Freshness }`，`Freshness::{Live, LastKnown, Stale, Unavailable}`。沿用额度 1800 秒、任务 180 秒阈值，不把两者可用性绑在一起。`load_uploaded_frame(data: &Path, page: u8, expected_sha256: &str) -> Result<Option<Vec<u8>>, DesktopStateError>` 验证 1–5、文件大小和 hash。

`CachingPublisher<P>` 包装现有 publisher，构造 `new(inner: P, data: PathBuf, default_page: u8) -> Self`，分别实现 `StatusFramePublisher` 和 `PagePublisher`，仅云端成功后原子写 `desktop-frame-{page}.png`；`take_cache_error(&mut self)->bool` 供调用方记录诊断，缓存失败不改变网络结果。

- [ ] **1. RED:** 写 `stopped_worker_never_claims_live`、`future_or_wrong_route_snapshot_is_unavailable`、`quota_and_task_age_independently`、`mismatched_png_is_not_a_successful_preview`、`cache_failure_does_not_retry_an_acknowledged_upload`。断言超过当前时刻 60 秒的快照被拒、131073 字节图片被拒、hash 不符返回 None、文件系统错误不会导致第二次网络上传。
  ```rust
  assert_ne!(snapshot.assess_freshness(now, false).task, Freshness::Live);
  assert!(load_uploaded_frame(data, 3, wrong_hash)?.is_none());
  assert_eq!(network.calls(), 1);
  ```
- [ ] **2. 运行：** `cargo test --locked --test desktop_state`，确认失败。
- [ ] **3. 实现：** 快照上限 64 KiB，图片上限 128 KiB，验证 400×300 PNG 解码边界；拒绝链接和非普通文件。GUI 状态只来自当前 `poll.state` 和已有来源时间；写缓存失败仅记录类别。上传记录仍按既有逐页 checkpoint 保存。登记新快照及五个固定缓存名为卸载数据；不扩充安装事务旧五文件快照集合。
- [ ] **4. 验证：** `cargo test --locked --test desktop_state --test inkcards --test publish_coordinator --test companion_scheduler --test windows_uninstall`。确认失效 Key、节流和逐页持久化测试仍通过。
- [ ] **5. 提交：** `feat: expose bounded desktop status and verified frame cache`。

## Task 3: 结构化桌面操作与异步任务

**Files:** 新建 `src/desktop/mod.rs`、`service.rs`、`jobs.rs`，新增 `tests/desktop_service.rs`、`tests/desktop_jobs.rs`；修改 `src/operator.rs`、`src/lib.rs`。

**Interfaces:** `CredentialChoice::{Saved, New(Zeroizing<String>)}` 不实现明文 Debug/Serialize；`DraftId(u64)`、`JobId(u64)` 作为不可混用标识。`DesktopCommand` 定义 Refresh、Discover{draft_id,credential}、Prepare{draft_id,device_index,page_id}、Commit{draft_id}、Start、Stop、SetTheme(BoardTheme)、SetMode(InkcardMode)、SetStartup(bool)、Install{package_dir:PathBuf}、Uninstall{delete_data:bool,delete_key:bool}、Diagnose。`DesktopReply` 定义 Status(DesktopStatus)、Devices{draft_id,labels:Vec<String>}、Preview{draft_id,png:Vec<u8>}、Completed(OperationReport)、Failed(DesktopError)。`DesktopStatus` 含版本、后台状态、模式/主题、脱敏设备标签、快照、新鲜度、每页成功记录及启动项状态；不含密钥。`OperationReport` 为 Installed/Configured/Updated/UninstallPending，附可选中文警告码。

`DesktopBackend: Send` 的 `execute(&mut self, command: DesktopCommand, cancellation: &DraftCancellation) -> DesktopReply`、`clear_draft(&mut self, draft_id: DraftId)` 由 `WindowsDesktopBackend::new(data: PathBuf, package_dir: PathBuf) -> Self` 实现。`DraftCancellation::is_cancelled(&self, draft_id: DraftId)->bool` 读取共享取消标记。`DesktopJobs::spawn(backend: impl DesktopBackend + 'static) -> Self`、`submit(&mut self, command: DesktopCommand)->Result<JobId,Busy>`、`try_recv(&mut self)->Option<(JobId,DesktopReply)>`、`cancel_draft(&mut self, draft_id: DraftId)->Result<(),Busy>`、`request_close(&mut self)->CloseDecision` 管理唯一工作线程；CloseDecision 为 CloseNow/WaitForCommit。取消通过独立控制标记立即使旧结果失效，不受普通命令队列 Busy 阻挡；已进入 Commit/Install 时返回 Busy。取消后的网络请求若尚未返回，界面显示“正在取消”，新发现按钮等待其现有超时结束；工作线程返回后先清理草稿和密钥，再接收新草稿，不积压带 Key 的请求。

- [ ] **1. RED:** 写 `late_discovery_after_cancel_is_discarded`、`cancel_bypasses_busy_discovery_and_clears_before_next_draft`、`refresh_does_not_create_codex_session`、`duplicate_action_is_busy`、`old_version_cannot_modify_new_install`、`close_waits_for_commit_but_not_draft`。使用带 channel 的假 backend 控制先后顺序，断言取消后旧 Devices 不被 UI 接受、正在发现时取消立即被接受且旧草稿先清理、新草稿后执行、每次提交只执行一次、等待提交期间不能杀掉 worker。
- [ ] **2. 运行：** `cargo test --locked --features test-support --test desktop_service --test desktop_jobs`，确认失败。
- [ ] **3. 实现：** 凭据和设备列表只留在 backend 草稿会话中，结果回传标签和索引；选择已保存 Key 时服务内部复用凭据。Prepare 只生成内存预览，Commit 调用 Task 1。所有修改在现有 OperationGuard、版本和未完成事务检查下执行；首次安装是版本检查的唯一入口例外。串行执行修改，Refresh 合并且不积压。关闭/取消使旧 generation 结果失效，提交开始后只允许等待结果。
- [ ] **4. 验证：** 上述测试及 `appearance`、`operator_cli`、`windows_background` 通过；注入假 Codex 启动器证明 Refresh/页面切换不会启动 Codex；检查所有 command/result 的 Debug 不暴露 Key。
- [ ] **5. 提交：** `feat: add guarded desktop operations and cancellable jobs`。

## Task 4: 原生完整控制台与四个页面

**Files:** 新建 `src/bin/codex-zectrix-desktop.rs`、`src/desktop/app.rs`、`src/desktop/pages/{overview,cards,device,settings}.rs`、`assets/fonts/NotoSansSC.ttf`、`assets/fonts/OFL.txt`、`assets/fonts/SOURCE.txt`、`THIRD-PARTY-NOTICES.txt`、`tests/desktop_app.rs`；修改 Cargo.toml、Cargo.lock、desktop/mod.rs。

**Interfaces:** `DesktopApp::new(cc: &eframe::CreationContext<'_>, jobs: DesktopJobs) -> Self` 实现 eframe::App。`AppModel::apply_reply(&mut self, job: JobId, reply: DesktopReply)`、`request_action(&mut self, action: UiAction)->Option<DesktopCommand>` 作为可无窗口测试的状态入口；UiAction 包含 SelectPage、SelectTheme、ApplyTheme、RequestMode、ConfirmMode、Discover、Prepare、ConfirmSetup、Start、Stop、SetStartup、Install、ConfirmUninstall、Diagnose；字段沿用 Task 3 命令所需类型。AppModel 分配单调递增 DraftId；取消和关闭由 DesktopApp 调用 Task 3 控制接口，同时清空表单并让 AppModel 拒绝旧 DraftId。`run_desktop() -> Result<(), DesktopLaunchError>` 创建约 1040×760 窗口；GUI feature 与后台核心隔离。

- [ ] **1. RED:** 写 `preview_click_never_emits_set_theme`、`all_pages_require_explicit_confirmation`、`only_explicit_stop_stops_background`、`configured_warning_keeps_dashboard_usable`、`stale_reply_cannot_replace_current_draft`。断言选图只更新 selected_page、确认五张模式前命令为 None、关闭不发 Stop、已配置但启动警告仍显示已保存。
- [ ] **2. 运行：** `cargo test --locked --features desktop,test-support --test desktop_app`，确认失败。
- [ ] **3. 实现：** 锁定 eframe 0.37.0 及其必要原生 feature；从官方字体来源取得资源、保存来源修订/hash/OFL 后嵌入。先实现窗口框架，再逐页连接 Task 3：总览显示 LastKnown/过期/不可用；墨卡区分真实成功图与示例；设备表单使用掩码临时缓冲并在退出草稿时清零；设置处理启动项、诊断、版本及卸载确认。所有耗时动作交给 jobs，界面只轮询结果；每 2 秒合并本地 Refresh。图形初始化失败用 Windows 可见错误框，提供已有 CLI 的位置。
- [ ] **4. 验证：** 运行 desktop_app、desktop_jobs；用隔离假 backend 驱动 1040×760 和较小窗口、100%/150%/200%、明暗、长设备名、中文输入、网络失败等状态。GUI 的 `--version`、`build-fingerprint` 不启动窗口且可捕获输出。本机真正窗口的焦点、中文输入与关闭行为留到 Task 6 实际检查，不用纯状态测试冒充。
- [ ] **5. 提交：** `feat: implement the four-page native NOTE4 console`。

## Task 5: GUI 发行包、开始菜单及安全卸载

**Files:** 修改 Cargo.toml/Cargo.lock 版本为 `0.3.0-rc.1`；修改 `src/install/{mod,windows}.rs`、`src/operator.rs`、`src/platform/mod.rs`、`scripts/build-release-windows.ps1`、`scripts/test-windows-package.ps1`、`.github/workflows/windows.yml`；新建 `src/platform/shortcut_windows.rs`、`tests/windows_shortcut.rs`；扩展 windows_package/install/uninstall、install_recovery。

**Interfaces:** `PackageManifest::allowed_files(&self)->Result<&'static [&'static str],InstallError>` 分别返回 schema 1/2 精确集合。v2 的 hash 字段包含 `NOTE4控制台.exe`、原 CLI、原 companion、LICENSE、THIRD-PARTY-NOTICES.txt、快速入门.txt；另有 package.json，共七文件。GUI 从 Cargo 输出 `codex-zectrix-desktop.exe` 复制并重命名；不修改旧两程序文件名。`ShortcutManager::new(programs_dir: PathBuf, app_root: PathBuf)->Self`、`ensure(&self, installed_gui:&Path)->Result<ShortcutOutcome,ShortcutError>`、`remove_owned(&self)->Result<ShortcutOutcome,ShortcutError>` 管理当前用户开始菜单 `Codex NOTE4.lnk`。`ShortcutOutcome::{Created, Updated, Removed, Absent, PreservedForeign}`；仅目标为已验证安装目录内 GUI、参数为空且工作目录匹配的现有快捷方式可更新/删除，未知归属保留。测试传入临时 Programs 目录。

- [ ] **1. RED:** 增加 `v1_to_v2_upgrade_and_failure_restore`、`package_rejects_missing_gui_and_extra_files`、`modified_shortcut_is_not_overwritten_or_deleted`、`gui_uninstall_uses_verified_cli_helper`。验证 schema 1 老包仍可读；v2 版本/hash 缺一拒绝；失败升级恢复 rc.5、原后台状态和旧设置；用户更改的快捷方式原样保留。
- [ ] **2. 运行：** `cargo test --locked --features desktop,test-support --test windows_package --test windows_install --test install_recovery --test windows_shortcut --test windows_uninstall`，确认新增断言失败。
- [ ] **3. 实现：** 以 manifest 版本驱动安装/清理名单，维持 64 MiB 单文件限制并实际核对 GUI 体积；校验全部三个程序版本。提交安装后维护受控快捷方式，失败返回可重试警告。GUI 卸载时复制并启动经校验的 CLI 作为隐藏延迟清理 helper，等待 GUI 父 PID+创建时间匹配的进程退出；禁止把 GUI 自身复制成不支持清理命令的 helper。SNAPSHOT_FILES 与旧恢复日志保持兼容。CI 同时覆盖原核心与 desktop feature。
- [ ] **4. 验证：** 上述测试全部通过；构建实际 v2 ZIP，验证七文件、全部 hash、三程序版本、中文空格路径、离线预览及 GUI 入口。旧包升级与失败回滚使用隔离副本，不卸载用户正在使用的生产配置。
- [ ] **5. 提交：** `feat(windows): package and install the desktop console safely`。

## Task 6: 完整回归、审阅和真实交付

**Files:** 修改 README.md、docs/windows.md、windows-quickstart.txt、windows-acceptance.md；新建 `docs/adr/0011-windows-desktop-console.md`；检查脚本/日志存 `../verification/`；交付 ZIP、源码 ZIP、SHA256 和验收记录到聊天 outputs/。

**Interfaces:** 使用 Tasks 1–5 的真实入口，不新增生产接口。本机 GUI 入口 `NOTE4控制台.exe`，原 CLI 保留用于受控检查；发布前构建锁文件与版本必须一致。

- [ ] **1. 运行核心与 GUI 全量回归：** `cargo fmt --all -- --check`；`cargo clippy --locked --all-targets --all-features -- -D warnings`；`cargo test --locked --all-targets --features test-support`；`cargo test --locked --all-targets --features desktop,test-support`。确认均 exit 0，记录实际测试数，不沿用 rc.5 的 259 作为新结果。
- [ ] **2. 最终独立审阅：** 按已选择的执行方法调用对应审阅流程；重点核查 Review Focus、凭据生命周期、GUI/后台边界、v1/v2 回滚和安全清理。先为重要缺陷增加有行为意义的回归，修复后仅重复受影响检查及最终必要全量检查。
- [ ] **3. 打包与隔离 GUI 验收：** release 构建启用 desktop feature；包内普通用户首次安装/草稿取消/错误 Key/设备选择/预览确认/关闭提交等待/卸载都用隔离数据和假服务验证。启动本机原生窗口检查中文、缩放、主题、按钮与键盘操作；若本次工具不能直接检查原生窗口，提供真实程序让用户确认，明确记录未验证项，不能以 HTML 样机截图替代原生验收。
- [ ] **4. 本机升级与五页检查：** 记录 rc.5 settings hash、模式、主题、后台身份和启动项；通过新包升级，保持旧配置与运行状态。GUI 总览应与 companion 同源；打开/关闭 GUI 不产生额外持续 Codex 会话。使用真实凭据经程序上传五页，核对各页成功指纹及失败数，验证后台重启恢复。测试中不输出 Key，也不改设备同步间隔；实体屏幕刷新仅按实际观察记录。
- [ ] **5. 更新文档并交付：** ADR 明确对 0010 的补充；快速入门改为双击中文 GUI；保留原作者许可证。生成候选 Windows ZIP、已提交源码 ZIP、校验值及准确验收记录。测试日志/真实 PNG/个人配置不得进入源码包；Git 工作树干净，不推送原作者仓库。
- [ ] **6. 提交：** `docs: record desktop console acceptance and distribution`；最终答复用简单中文说明能做什么、已验证什么、未验证什么，并链接 outputs 中的安装包。

## 计划自查

已对照设计检查：四页与首次向导由 Tasks 3–4 覆盖；真实/示例区分、来源新鲜度和缓存由 Tasks 2、4 覆盖；既有配置、并发版本保护及提交取消由 Tasks 1、3 覆盖；安装兼容、快捷方式和卸载由 Task 5 覆盖；真实五页、中文缩放与发布由 Task 6 覆盖。五项 Review Focus 均有对应回归或明确的原生验收项。自查修正了 Freshness 类型、忙碌请求的取消通道和快捷方式归属接口；没有把样机或单元测试当作原生实机验证。

## 执行说明与参考

实施时按 using-git-worktrees 流程复用合适的隔离 checkout，先记录基线与当前已安装版本。使用已有 `../cargo-task.ps1` 调用原生 Windows 工具链并保存日志；必要的沙箱审批走工具授权流程。不要执行用户未要求的仓库推送或消息发送。

建议采用 **Native（当前对话由主助手顺序实现，最后一次独立审阅）**：配置、状态、后台控制、界面和安装互相依赖，顺序实施能减少接口交接。本计划尚待用户审阅并选择执行方式；本文件不是已经实现或通过验收的声明。

框架 feature 和原生入口参考 [eframe 0.37 文档](https://docs.rs/eframe/0.37.0/eframe/)、[feature 清单](https://docs.rs/crate/eframe/0.37.0/features)。字体从 [Google Fonts 官方 Noto Sans SC](https://github.com/google/fonts/tree/main/ofl/notosanssc) 获取；来源记录使用实际下载修订与 hash。
