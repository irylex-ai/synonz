# Synonz 0.4.0 实现计划

- 状态: IMPLEMENTING（2026-09-11，计划 APPROVED；M21-M23 完成）
- 日期: 2026-09-11
- 依据: ADR-0018（APPROVED——系统调度与生命周期完备）、架构设计
  文档 v5（APPROVED）、ADR-0011/0017 修订注记
- 性质: 开发文档——0.4.0 破坏性单波发布的执行次序、迁移映射与验收
  基准；架构决策理由见 ADR-0018 与 v5 文档，本文不重复论证
- 前置: 0.3.0 已发布（0.3.1 文档补丁）；代码基线 = 0.3.0 反应架构
  （139/139 全绿、clippy 零警告）；文档基线 = ADR-0018 + v5（均
  APPROVED，v4 已 SUPERSEDED）
- 关联延期项: 后台工作专用模型（0.4.0 后优先）、调度扩展暂缓项
  （pause/resume/status、cron 类规则、一次性/延迟任务、自建池配套
  便利、正文检索、Scheduler 专用事件词条）、S3 / 控制流影响（另立）、
  ended 状态门（另立）

---

## 1. 总览

0.4.0 是**调度与生命周期完备落地波**：把 ADR-0018 与 v5 的全部
决策一次性实施为代码。六个里程碑按依赖链排序：

```
M21 前提工程（drain 有界化 + panic 可见化）
  ↓
M22 存储查询面（list_stale 下推 + list 查询 + sweep 分页改造）
  ↓
M23 Scheduler 组件（计时线程 + 任务模型 + 三策略 + W1 宿主模型 + 公开 API）
  ↓
M24 系统调度器与 Monitor（build 装配 + 空闲清扫 + 崩溃孤儿对账 + 只读快照）
  ↓
M25 统一会话表与 shutdown（会话表升级 + 停机顺序 + 观测 flush）
  ↓
M26 收尾与验证（测试迁移、文档同步、示例、命名终稿、全量回归、发布决策）
```

依赖理由：前提工程先填终结可靠性的两个洞（M21）；查询面与 sweep
分页是 Monitor 的数据通路（M22）；Scheduler 是系统调度与用户调度
的共同底座（M23）；Monitor 是系统调度器的首个注册任务（M24）；
shutdown 停止调度器并消费会话表（M25）；收尾扫全局。

**每一波的完成定义**：子项全部落地 + 新增/迁移测试绿 + 全量回归绿
（`cargo fmt --check` / `clippy --workspace --all-targets --all-features`
零警告 / `cargo test --workspace --all-features` 全通过）+ 与 v5 决策
落点核对 + **里程碑完成记录**（含 B 类决议与 A 类请示结果）写入
本计划。

---

## 2. 里程碑明细

### M21 前提工程

| # | 子项 | 要点 |
|---|---|---|
| 1 | drain 有界化 | `finalize_conversation` 的后台任务等待加超时上限（**固定 60s，按会话总预算**——已预决）；超时继续收尾（不无界挂起），失败经总线 `FlowFailed{AtConversationEnd}` 可见；测试经内部覆盖机制 |
| 2 | panic 可见化 | 维护任务 panic（JoinError）不再吞没；经总线 `FlowFailed` 事实可见 |
| 3 | 验收测试 | 挂死任务 → 有界 drain → 终结仍完成 + 事实可见；panic 任务 → 事实可见（不静默） |

### M22 存储查询面

| # | 子项 | 要点 |
|---|---|---|
| 1 | 查询类型（新公开） | `ConversationSummary` {id, subject_id, topic, last_active, ended}；`ConversationCursor` {last_active, id}；`ConversationPage` {items, next}；`ConversationQuery` {keyword, after, limit}；全部 `#[non_exhaustive]` + 公开构造器 |
| 2 | ConversationStore 契约 | 新增 `list_stale(before, after, limit)`（未结束且 `0 < last_active <= before`）；`list(query)`（元数据关键字：id/subject_id/topic 大小写不敏感子串；游标分页）；全序 `(last_active 降序, id 升序)`；旧全量 list 形态退役 |
| 3 | in-process 默认实现 | 内部线性扫描 + 排序 + keyset 跳过 + 摘要投影（不含 turns） |
| 4 | sweep 改造（内部化） | 内部 sweep 改为分页循环：逐页 `list_stale`，游标越过失败条目（不阻塞推进；失败下一 tick 重试）；去除「全量拉回 + `of` 二次加载」；公开入口退役随 M24（Monitor 接入）执行 |
| 5 | 验收测试 | 分页全序/游标推进/边界（空页、满页、末页）/关键字匹配（三字段、大小写）/摘要不含 turns/清扫分页推进（含失败条目被越过） |

### M23 Scheduler 组件

| # | 子项 | 要点 |
|---|---|---|
| 1 | 任务模型 | 任务登记表（deadline/period/policy/running/pending/body）；单调时钟；固定速率（`deadline += period`）+ 追赶保护（按周期倍数跳过积压） |
| 2 | 计时线程 | 单条专用系统线程：登记/deadline 计算/等待（Condvar）/触发/提交；不执行任何任务体；注册/取消/停止均可唤醒 |
| 3 | 触发-执行分离 | 触发 → 非阻塞提交宿主 Executor（`Handle::spawn`）；提交移到锁外；panic 隔离（不杀死计时线程） |
| 4 | 三策略 | Skip（运行中丢弃）/ Concurrent（总是提交）/ Queue（合并：至多一次 pending；RAII guard 清除或续跑；panic/取消安全） |
| 5 | 立即首触发 | 注册即触发一次，之后每周期 |
| 6 | W1 宿主模型 | `Scheduler::new(executor)`；Runtime build 捕获宿主 Handle（`Handle::try_current`）；同步构建场景 builder 显式注入；两者皆无且配置 `idle_timeout` → build panic（已预决，消息指明修复方式） |
| 7 | 公开 API | `Scheduler::new` / `schedule(Schedule, OverlapPolicy, body) -> TaskHandle` / `TaskHandle::cancel` / `tasks()` 只读信息；停止随实例（shutdown 停系统实例） |
| 8 | 验收测试 | 场景推演（周期 10s/执行 30s 与周期 5s/执行 1s 的 Skip 语义——触发准时、执行隔离）；Queue 合并（背靠背、积压上限 1）；Concurrent 并发层数；长任务不阻塞短任务触发；panic 后循环存活；立即首触发 |

### M24 系统调度器与 Monitor

| # | 子项 | 要点 |
|---|---|---|
| 1 | build 装配 | 系统调度器创建 + 计时线程启动（不阻塞 build）；配置 `conversation_idle_timeout` 时注册 Monitor 任务（Skip、立即首触发；tick = `clamp(timeout/4, 100ms, 60s)`——已预决） |
| 2 | Monitor 清扫 | 周期调用分页 sweep；**崩溃孤儿对账**：启动首扫即发现上一进程遗留（store 中陈旧 `last_active`）并终结（`IdleSwept`） |
| 3 | 只读快照 | 开发者只读快照（任务名/周期/策略/距下次触发/运行中）；无注册入口（系统调度器不可注册） |
| 4 | 清扫入口退役 | 公开 `sweep_stale` 删除（Monitor 唯一路径；内部 sweep 为任务体）；原 sweep 集成测试迁移为真实 Monitor 路径（短超时 + 轮询断言，宽裕余量） |
| 5 | 验收测试 | 空闲会话自动终结（零应用调用；`IdleSwept`）；崩溃孤儿模拟（预置陈旧会话 → 新 runtime 启动首扫回收）；快照只读（无注册 API）；未配置 `idle_timeout` 时不注册任务；`sweep_stale` 零残留 |

### M25 统一会话表与 shutdown

| # | 子项 | 要点 |
|---|---|---|
| 1 | 会话表升级 | 原维护表 → 会话表：条目 = {会话句柄 + 后台任务}；`new`/`with_id`/`of` 登记；任何 end 路径经 finalize 移除；`TaskRegistry` 适配 |
| 2 | `runtime.shutdown()` | 显式 async：停系统调度器（幂等；在途执行不等待）→ 快照会话表 → 逐会话 `end_with(Shutdown)`（drain + 机械促进 + 持久化 + emit）→ 观测 flush → 返回 |
| 3 | 观测 flush | 内部 `EventBus::flush()`（队列屏障 + oneshot 确认，不进公开面） |
| 4 | 词表 | `ConversationEndReason` 增 `Shutdown` 变体 |
| 5 | 验收测试 | shutdown 幂等（多次调用）；并发调用等待完成；归属范围（只收本 runtime 拥有的会话）；flush 送达（shutdown 返回时观察者已收到 `Ended`）；在途 turn 不等待（文档声明）；崩溃孤儿不归 shutdown |

### M26 收尾与验证

| # | 子项 | 要点 |
|---|---|---|
| 1 | 测试全量迁移与补强 | 全量回归（fmt/clippy/test --all-features）；时间语义测试用短周期 + 宽裕余量（防 flaky——新引入 flaky 即缺陷） |
| 2 | 文档同步 | v5 落点核对；CHANGELOG 0.4.0（破坏项 + §3 迁移表）；README 文档指针（v4 → v5）；rustdoc 更新（无 ADR 编号引用——coding.md 立法） |
| 3 | 示例 | 新增开发者自定义 Scheduler 示例（自动保存类场景：Queue 策略）；其余示例随 API 演进核对 |
| 4 | 命名终稿 | 按 coding.md §5 命名立法执行（Scheduler 族、查询类型、快照类型） |
| 5 | 发布决策 | 单波发布流程（bump 五 crate 0.3.1→0.4.0 → dry-run → 依序 publish → tag v0.4.0 → GitHub Release）——**等 irylex 确认后执行** |

---

## 3. 迁移映射表（旧 → 新）

| 旧（0.3.x 代码） | 新（0.4.0） |
|---|---|
| `ConversationStore::list() -> Vec<ConversationState>`（全量） | `list_stale(before, after, limit)`（清扫下推）+ `list(ConversationQuery) -> ConversationPage`（查询面；旧全量形态退役） |
| `ConversationState` 全量返回（列表场景） | `ConversationSummary`（不含 turns；全量状态仍走 load/of） |
| 会话维护表（仅后台任务句柄） | 会话表（会话句柄 + 后台任务；new/with_id/of 登记，任何 end 路径移除） |
| `sweep_stale`（公开手动兜底） | **退役**——Monitor 自动清扫（系统调度器）为唯一路径；内部 sweep 为任务体，不开公开面 |
| `ConversationEndReason {Explicit, IdleSwept}` | + `Shutdown`（进程退场收尾） |
| `finalize_conversation` drain（无界、JoinError 吞没） | drain（有界 + panic 可见） |
| `build()`（无执行环境要求） | 捕获宿主 Executor Handle（W1；或 builder 显式注入） |
| `l1_store` 等旧 builder 名 | 已在 0.3.0 清理，无变更 |
| （无） | 公开 `Scheduler` / `Schedule` / `OverlapPolicy` / `TaskHandle` / 系统调度只读快照 / 会话查询类型 |

---

## 4. 验收总标准

1. 六里程碑逐项完成，每波全量回归绿；
2. 行为验收：自动空闲清扫（零应用调用，`IdleSwept`）/ 崩溃孤儿
   对账 / 三策略语义（Skip/Concurrent/Queue-合并）/ 触发隔离
   （长任务不阻塞触发）/ shutdown 幂等与停机顺序 / flush 送达 /
   drain 有界 / panic 可见；
3. 公开面验收：Scheduler 三策略可用（示例验证）；系统调度器不可
   注册（无 API）；只读快照可用；查询面分页/关键字/摘要正确；
4. 旧 API 零残留扫描：旧 list 全量形态 / 公开 `sweep_stale` /
   会话维护表命名——全部清零（退役/改名已在 M22-M25 落地）；
5. 文档一致：v5 与代码落点核对通过；CHANGELOG 0.4.0 完整（破坏项 +
   迁移指引）；README 指针更新；rustdoc 无 ADR 编号引用；
6. 延期项记录在案：调度扩展暂缓项 / 后台专用模型 / S3 / 控制流
   影响 / ended 状态门。

---

## 5. 风险与开放项

**已决项**（2026-09-11 预先定案——不留到实施期逐段处理，避免遗漏）：

| 项 | 决议 |
|---|---|
| **W1 缺失执行环境形态** | 同步 build 保持；配置 `conversation_idle_timeout` 且既无宿主上下文又未注入 Handle → **build panic**（消息指明两种修复方式；程序配置错误——tokio `spawn`/`Handle::current` 先例） |
| **drain 超时值** | **固定 60s**（按会话总预算，非每任务累加）；超时后放弃等待、不取消任务（迟完成任务的孤儿 L2 块记为已知边界，写入文档）；测试经内部覆盖机制（不开公开配置面） |
| **sweep tick 间隔** | **`clamp(conversation_idle_timeout / 4, 100ms, 60s)`**：清理延迟 ≤ timeout/4；崩溃恢复 ≤ 60s；测试随短超时自动变快 |
| 只读快照字段 | 语义内容：任务名/周期/策略/距下次触发/运行中；字段命名按立法实施期落 |
| 会话表内部结构 | `Mutex<HashMap<String, SessionEntry { conversation, tasks }>>`；new/with_id/of 登记，finalize 移除 |
| flush 屏障 | 队列控制项 + oneshot 确认；不开公开面 |
| 零任务时计时线程 | 首个任务注册即启动；零任务不空转（配置 `idle_timeout` 的正常路径随 build 启动） |
| 测试稳定性 | 短周期 + 宽裕余量；新引入 flaky 即缺陷 |
| 多实例计时线程成本 | 文档写明（每 Scheduler 一条线程，进程级环境通常一个） |

**实施期决策协议**（对涌现事项——实施中新出现的决策点）：

- **A 类（用户门控）**：影响公开 API 契约、可观察行为语义或不可逆
  承诺的事项——带选项与推荐向 irylex 请示，确认后实施；
- **B 类（代理可决，按既定规则）**：内部结构、命名（coding.md §5
  命名立法）、有界影响的具体值与测试策略——由实施代理按既定规则
  定案：开工时明确、完工时记录；
- **记录义务**：A 类进 ADR/v5（涉及语义时）；B 类进本计划的
  "里程碑完成记录"与提交信息——决议不得只存在于聊天或代码中。

**执行期约定**：

| 项 | 约定 |
|---|---|
| 发布节奏 | M26 发布执行等 irylex 单独确认 |
| 后台专用模型 | 0.4.0 后优先，不在本计划 |

---

## 6. 里程碑完成记录

### M21 前提工程 ✅（2026-09-11）

- **drain 有界化**：`finalize_conversation` 的后台任务等待改为每会话
  总预算（固定 60s；测试经 `#[cfg(test)]` 内部覆盖，不开公开配置
  面）；超时后放弃等待（不取消任务），失败经
  `FlowFailed { stage: Drain, moment: AtConversationEnd }` 可见；
- **panic 可见化**：JoinError 不再吞没，经同一 `FlowFailed` 事实可见；
- **词表**：`MemoryFlowStage` 增 `Drain` 变体（non_exhaustive，附加性）；
- **测试**：单元测试 2 项（挂死任务 → 有界收尾 + 超时事实可见；
  panic 任务 → 事实可见）；全量回归 141/141 全绿、clippy 零警告、
  fmt 干净。

### M22 存储查询面 ✅（2026-09-11）

- **查询类型（新公开）**：`ConversationSummary` / `ConversationCursor`
  / `ConversationPage` / `ConversationQuery`（`#[non_exhaustive]` +
  公开构造器；全序 `(last_active 降序, id 升序)`）；
- **ConversationStore 契约**：`list()` 全量形态退役 → 新增
  `list_stale(before, after, limit)`（清扫下推：非 ended 且
  `0 < last_active <= before`）+ `list(query)`（元数据关键字：id /
  subject_id / topic 大小写不敏感子串；游标分页）；
- **应用面出口（B 类决议）**：`runtime.list_conversations(query)`
  （Low Level track——查询面可达性出口；`save`/`load` 仍不公开）；
- **sweep 改造**：分页循环（`SWEEP_PAGE_SIZE = 128`），游标越过失败
  条目（下一 tick 重试），去除「全量拉回 + `of` 二次加载」；
- **测试**：单元测试 5 项（过滤排序/游标分页/并列 id/关键字三字段）
  + 跨页 sweep（130 会话）+ 外部查询面测试；全量回归 147/147 全绿、
  clippy 零警告、fmt 干净。

### M23 Scheduler 组件 ✅（2026-09-11）

- **核心实现**：`Scheduler`（单条计时线程：登记 / deadline 计算 /
  等待 / 触发 / 提交，不执行任何任务体）+ 任务登记表（deadline /
  period / policy / inflight / pending / body）+ 固定速率推进 +
  追赶保护（过载后按周期倍数跳过积压，不补发一串触发）；
- **触发-执行分离**：触发 → 非阻塞提交宿主 Executor
  （`Handle::spawn`）；执行完成回接经 RAII guard（panic/取消安全
  释放运行位）；
- **三策略**：Skip（运行中丢弃）/ Concurrent（总是提交、可重叠）/
  Queue（合并：至多一次 pending、完成即背靠背续跑）；
- **公开 API**：`Scheduler::new(executor)`、`schedule`、`schedule_named`
  （B 类增补：与 `TaskInfo.name` 字段对称，用户任务可命名）、
  `TaskHandle::cancel`、`tasks()` 只读信息；`Schedule::every`（立即
  首触发）；停止随实例消亡（`stop` 供 Runtime 停机调用）；
- **零任务形态**：计时线程随首个任务注册启动（无任务不空转）；
- **测试**：7 项（立即首触发 / 三策略语义 / 长任务不阻塞短任务触发
  / panic 不杀循环 / 任务快照）；全量回归 154/154 全绿、clippy 零
  警告、fmt 干净。
