# Windows 0.2.0-rc.1 审阅记录

2026-10-21。基线 52fb0cb9615b966966f5a22a8d33de67f34dda14；独立审阅覆盖至 4d519e19fa5788165292c9e50e6241a6469de7e0。审阅模型 gpt-6-astra，xhigh。8 项重要问题进入同一次修复，未再派发重复审阅。

| 问题 | 修复与回归证据 |
| --- | --- |
| 手动旧后台未被迁移流程发现 | 检查当前用户的同名进程，未知身份时阻止切换；unmanaged_legacy_process_blocks_activation_without_a_scheduled_task 先失败后通过；额外原生进程检测通过 |
| 配置输入期间另一窗口升级 | 操作锁内重新确认安装版本，变更时要求重开向导；configuration_cannot_commit_after_another_window_upgrades 先失败后通过 |
| 已知任务状态掩盖部分不可见 | 状态与覆盖范围分离，屏幕和诊断保留部分可见标记；a_running_task_does_not_hide_partial_observation 先失败后通过 |
| 已移除额度窗口仍显示旧值 | 明确缺失清空、格式错误保留并过期；explicit_absent_windows_clear_cached_values 和 both_explicitly_absent_windows_clear_the_previous_account_quota 先失败后通过 |
| 单窗口时长缺失或出现虚构第二窗口 | 依据实际窗口数量排版，始终标注组和时长；single_integer_window_renders_its_actual_duration、single_fractional_window_does_not_draw_a_second_window 先失败后通过；单小数/仅第二窗口预览已目检 |
| 已迁移任务仍依赖旧下载路径 | 事务提交迁移记录，禁用且身份未变的旧任务不再读取旧 EXE；committed_migration_no_longer_depends_on_the_old_binary 先失败后通过；重新启用时不能绕过检查 |
| 自动发现反复选择损坏 Codex | 自动候选按序尝试初始化，显式指定仍不切换；automatic_discovery_tries_a_working_candidate_after_startup_failure 先失败后通过 |
| 时钟回拨缩短失败退避 | 回拨仍保留对应失败级别的退避；clock_rollback_preserves_the_failure_backoff 先失败后通过 |

完整测试 `cargo test --locked --all-targets --features test-support`：229/229 通过。fmt、clippy（全部 targets/features，警告视为错误）、锁文件 release 构建通过。真实 ZIP 验证通过：精确文件集合、hash、配对版本、中文/空格/特殊字符解压路径、400×300 一位灰度预览。没有真实 Key 或设备数据进入发布件。

## 实施决策及代价

1. 使用完整独立仓库副本保留原目录的未提交改动，再建立基线，未改原备份。代价：额外磁盘占用。
2. Windows 上用原生 PowerShell 记录执行证据，替代技能中的 bash 记账包装。代价：需要人工核对记录。
3. 自动迁移只接受已核实的 0.1.9 EXE 指纹；未知旧版停止自动切换。代价：其他旧构建需用户先处理旧启动项和旧进程。
4. 真实设备、迁移和 Desktop 跨会话状态不能由单元测试断言；以单独验收记录为准，观察不完整时明确显示部分可见。代价：尚不能承诺所有 Desktop 会话状态均可读取。
5. 远端 CI、Windows 10、登录/休眠、新用户和真实下载 SmartScreen 验收保持待验证，因此只交付 rc。代价：正式发布仍有环境验收工作。
6. Linux/macOS 统一发布不属于本轮；保留既有代码，不将 Windows 通过等同跨平台可用。代价：这些平台需要下一轮实现和验收。
7. 升级前后台已停止时，升级后仍保持停止；包内双 EXE 版本检查会执行，运行中升级额外验证本地就绪并支持失败回滚。代价：停止状态升级的首次真实后台启动仍可能暴露环境故障。
8. 旧计划任务以禁用状态保留，并用可回滚的迁移记录消除对旧 EXE 的后续依赖。代价：计划任务列表中保留一个禁用条目。
9. 按已批准计划保留本地 feature/windows-release 分支和交付源码，目标 GitHub 仓库确定前不推送。代价：目前不是已公开的 GitHub Release。

## 暂缓的小问题

从下载目录执行旧式 `status` 命令时，开机启动检测仍可能显示未能确认。中文菜单 5 使用已安装路径，显示正确；日常请用菜单 5。此项不影响后台启动或上传。

## 实机反馈后的 rc.2 修复

用户确认 rc.1 画面无重叠，但指出任务仍为“状态未知”。只读实测发现，同一活跃顶层对话在独立 app-server 的 `thread/list` 中是 `notLoaded`，`thread/read` 和 `thread/turns/list` 中是没有结束时间的 `interrupted`；Desktop 的本地执行元数据则是 `inProgress`，活动时间持续更新。因此 rc.1 的任务功能未通过实机验收，不能以“部分可见”代替修复。

rc.2 仅对 API 已枚举的顶层任务补充本地元数据核对，使用只读 SQLite 连接与一致快照；不查询正文列。增加真实问题的失败回归后修复，当前执行、结束时间、24 小时窗口、3 分钟活动过期、未来时间、其他任务隔离、未知格式、缺失文件不创建及数据库字节不变均有覆盖。全量 233/233 通过；fmt、clippy -D warnings、release 构建与真实 ZIP 冒烟检查通过。

补充决策：依据用户实机反馈扩充为本地执行元数据适配，保留 API 顶层任务过滤和官方 active 状态优先级。代价：这是当前本地存储格式的兼容层；未来 Codex 格式变化或长时间没有新活动证据会降级为未知，不能保证全部未来版本。详情及官方接口说明见 [Windows 使用说明](windows.md#desktop-本地执行状态) 和 [App Server 文档](https://learn.chatgpt.com/docs/app-server#read-a-stored-thread-without-resuming)。

## rc.5 五张自动更新审阅

2026-10-22；基线 `fa368b3d45ec17e1657ac037913f05533ab750aa`。独立只读审阅检查固定页码、独立重试、凭据错误停止、模式切换、安装兼容及状态恢复，提出两项 P2 问题，均在交付前修复并复审通过。

| 问题 | 修复与验证 |
| --- | --- |
| 五页共用一轮开始时间，较慢的前页请求使后页退避提前到期 | 每页请求前重新采样时间；慢请求回归原本得到 160，修复后为 280，符合后页在 220 请求后的 60 秒等待 |
| 只在整轮结束保存，后页请求中断会丢失前页成功记录 | 每页处理后立即保存，再处理下一页；部分批次重启仅发送剩余页，保存失败后不再发起下一页请求 |

全量 259 项测试、fmt、clippy、真实发布 ZIP 检查通过。已升级本机并启用五页，逐张真实云端上传成功，重启后五张 PNG 与成功记录的 SHA256 全部一致。设备物理同步与尚未覆盖的平台环境仍以 [验收记录](windows-acceptance.md) 为准。
