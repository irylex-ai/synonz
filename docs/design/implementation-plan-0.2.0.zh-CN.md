# Synonz 0.2.0 实现计划

- 状态: VERIFIED（2026-09-08 全部里程碑 M12-M15 完成；实现与 ADR-0014/
  0015/0016 决策落点核对通过）
- 日期: 2026-09-07
- 依据: ADR-0014 / ADR-0015 / ADR-0016（均 APPROVED）、架构设计文档
  v3（APPROVED）
- 性质: 开发文档——0.2.0 破坏性发布的执行次序、迁移映射与验收基准；
  架构决策理由见各 ADR 与 v3 文档，本文不重复论证
- 前置: 0.1.2 已发布（crates.io 五 crate + GitHub Release v0.1.2）

---

## 1. 目标与范围

实现 0.2.0 = ADR-0014/0015/0016 的完整落地：单一执行面（run →
Execution）、执行契约封闭（会话必选 / Agent 持 Runtime / Context
升格 / 落账全入史 / API 收敛）、Observer 旁路契约。破坏性变更集中
释放（pre-1.0 契约允许）。

不在范围（推迟表承接）：S3 编排、后台专用模型（已触发，0.2.0 后
第一优先）、纯回放配置、fork 重设计、策略覆灭入口、日志栈选型。

## 2. 已确认决策（本计划的前提）

| 决策 | 结论 |
|---|---|
| 实施顺序 | 0014 → 0015 → 0016 → 迁移收尾（依赖决定，严格串行） |
| 里程碑切分 | M12-M15，每包有可运行验收标准；每包 `cargo test` 全绿后进下一包 |
| 开发策略 | 沿用 Mock-first；每包同步迁移对应测试，不留赤字 |
| 包内提交 | 每工作包验收通过时一次提交（常设授权）；M13 包内允许多提交（子项按依赖排序） |
| 版本策略 | 0.1.2 → **0.2.0**（破坏性集中释放，pre-1.0 MINOR 升位） |
| 发布 | 按 release 规则：CHANGELOG → dry-run → 依序 publish → tag → GitHub Release → 验证 |

## 3. 里程碑

### M12 — 单一执行面（ADR-0014）

- **内容**：`AgentRunner`（内部）沿用；新增 `Execution` 公开句柄与
  `ExecutionEvent` 六变体（Delta / ToolRequested / ToolCompleted /
  Failed / Cancelled / Completed）；`Agent::run` / `run_with` 返回
  `Execution`（Stream<Item=ExecutionEvent> + Future + cancel /
  with_timeout / rounds）；事件映射：AgentRunner 的 AgentEvent 过滤
  映射为 ExecutionEvent（Started / Requested / Responded 跳过；
  Completed 携带 AgentOutput）；删除 `Agent::ask` / `Answer` / 旧
  `Run`；会话序列化守卫转移至 `Execution`
- **验收**：handles.rs 迁移重写全绿（流式 + await 双面 / 交错 / 取消
  映射 / 链式超时 / 默认预算）；新增 ExecutionEvent 六变体映射测试、
  Completed 流自足测试（纯流拿结果）、终态不变量测试；
  `cargo test -p synonz` 全绿
- **对应**：ADR-0014

### M13 — 执行契约封闭（ADR-0015）

包内子项按依赖排序（允许多提交）：

1. **会话必选**：`TurnInput.conv` 必选；删三个裸 `From`；唯一入口
   `conversation.turn_input(...)`
2. **Agent 持 Runtime**：`AgentBuilder::runtime` 必选 + `build()`
   校验；三 preset 前置 runtime 参数；执行入口 `Arc::ptr_eq` 同源
   校验（错配显式报错）
3. **Context 升格**：`on_turn_completed` 收编归档 + 压缩编排（从
   trigger/循环迁入）；`with_context` 删除；裸历史回退删除；执行
   循环三路径归一（派生 context）
4. **装配锁死**：`ConversationHistory` 删除；`AssemblyRequest` 收窄
   为 `{memory, subject, conversation_id, topic, input}`
5. **落账全入史**：`Turn.outcome`（Completed/Failed/Cancelled）；
   失败/取消轮 push_turn；**仅成功轮写 L1**
6. **记忆流可见**：LifecycleEvent 新增 non_exhaustive 变体（阶段 +
   详情，最小形状）；装配三层吞错消亡；压缩回退显式降级并记录；
   soft_errors 上浮；persist 失败可见
7. **Conversation API 收敛**：删 `truncate_last` / `clear` / `fork`；
   `push_turn` 收 pub(crate)；export 边界文档明示
8. **ModelParams 归位**：`ModelRequest` 删 params 字段；双适配器
   构造绑定（builder 方法 temperature / max_tokens，不传 = 默认）；
   trigger 压缩调用改造
9. **sweep_stale 修复**：主体编解码对称化 + 真实主体类型 + 双主体
   测试

- **验收**：conversation / memory / context 测试迁移全绿；新增
  outcome 全入史测试、ptr_eq 错配报错测试、装配失败事件测试、
  sweep_stale 双主体测试；ADR-0015 二十项决策落点核对表 100%
- **对应**：ADR-0015

### M14 — Observer 契约（ADR-0016）

- **内容**：`Observer` trait（on_event / on_lagged）+
  `ObserverContext`（execution_id 进程内自增）；`RuntimeBuilder::
  observer` 注册表；`AgentBuilder::observability` 开关（默认 false）；
  派发器——每 run 后台任务、有界队列（固定容量常量）、try_send
  非阻塞、FIFO 保序、catch_unwind + 本次 run 熔断、收尾清空队列、
  on_lagged 丢弃告知；记忆流失败事件（M13 变体）经派发器流出
- **验收**：专项测试六项——投递顺序 = 发射序、丢弃计数 + on_lagged
  触发、观察者 panic 熔断（执行不受影响）、收尾不丢终态事件、开关
  off 零派发、并发 run 的 execution_id 归属；全量测试绿
- **对应**：ADR-0016

### M15 — 迁移收尾与文档同步

- **内容**：examples 改写（anthropic_chat / cancellation /
  custom_tool / openai_chat 随 API 演进；events.rs 改为 Observer
  旁路演示）；lib.rs 导出更新（+Execution / ExecutionEvent /
  Observer / ObserverContext；-Answer / Run / ConversationHistory）；
  README 示例代码同步；rustdoc 全面校对（术语与 v3 一致，doc test
  全跑）；CHANGELOG 0.2.0 条目（Summary / Highlights / Breaking
  逐项 / Migration）
- **验收**：`cargo test --workspace --all-features` 全绿（迁移后
  全量）；`cargo clippy --all-targets` 零警告；examples 全部可编译
  可运行（离线 Mock）；ADR-0014/0015/0016 决策落点核对表 100%
- **对应**：三 ADR 收尾 + release 规则文档同步要求

## 4. 迁移映射表（下游视角，CHANGELOG 素材）

| 0.1.x | 0.2.0 |
|---|---|
| `agent.ask(x).await` | `agent.run(conv.turn_input(x)).await` |
| `agent.run("裸字符串")` | `agent.run(conv.turn_input("..."))` |
| `Answer` / `Run`（事件消费面） | `Execution`（唯一执行句柄） |
| 迭代匹配 `AgentEvent` | 迭代匹配 `ExecutionEvent`（六变体） |
| `run_with(x, token)` | 同形（x 改为 turn_input） |
| `with_context(conv.context())` | **删除**（背景自动派生） |
| 注册 `ConversationHistory` | 默认 `LayeredMemory`（或自定义策略） |
| `truncate_last` / `clear` / `fork` | **删除**（窗口管理归 Context；数据清理属存储层） |
| `ModelRequest { .., params }` | `ModelRequest { messages, tools }`；params 进适配器构造 |
| `Agent::react(model, tools)` | `Agent::react(&runtime, model, tools)`（三 preset 同） |
| `Conversation::push_turn` 外部调用 | 走真实执行路径（pub(crate)） |

## 5. 测试布局

| 文件 | 覆盖 |
|---|---|
| tests/agent.rs | 执行面（run → Execution 端到端、事件映射） |
| tests/handles.rs | Execution 双面（Stream + Future + 控制器） |
| tests/conversation.rs | 落账 / outcome 全入史 / 收敛后 API |
| tests/memory.rs | 触发体系 + 记忆流失败可见 + 自定义策略注册 |
| tests/observer.rs（新增） | 派发器专项六项 + ObserverContext |
| tests/serialization.rs | outcome 字段 / AgentEvent 形状不变 |
| tests/derive.rs | 不变 |

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 破坏面大（公开 API 十余处变更） | 迁移映射表 + CHANGELOG 逐项列出；pre-1.0 契约允许 |
| M13 包内改动面广（9 子项） | 子项按依赖排序、包内多提交、每子项测试随行 |
| 派发器并发正确性 | M14 专项测试六项全覆盖 |
| rustdoc 与实现漂移 | M15 统一校对 + doc test 全跑 |
| 记忆流事件形状设计反复 | non_exhaustive + 最小形状（阶段 + 详情），契约允许演进 |
| 压缩预算护栏暂缺（后台模型推迟） | 接受 agent 模型默认参数；推迟项已标注"0.2.0 后优先" |

## 7. 状态流转

DRAFT →（irylex 评审）APPROVED → M12 起转 IMPLEMENTING → 全部里程碑
完成 + 验收核对 → VERIFIED → 发布后记录 RELEASED 信息。

## 8. 里程碑进度

| 里程碑 | 状态 | 完成日期 | 交付摘要 |
|---|---|---|---|
| M12 单一执行面 | ✅ 完成 | 2026-09-08 | Execution/ExecutionEvent（d661bd6）：六变体叙事流 + 流自足 + 终态不变量；删 ask/Answer/旧 Run；测试/示例迁移 |
| M13 执行契约封闭 | ✅ 完成 | 2026-09-08 | 九子项（1ad3abd）：会话必选、Agent 持 Runtime + ptr_eq 校验、Context 三行为升格、装配锁死 + ConversationHistory 删除、Turn.outcome 全入史、MemoryFlowFailed 事件立法、API 收敛、ModelParams 归位、sweep_stale 修复 |
| M14 Observer 契约 | ✅ 完成 | 2026-09-08 | Observer/ObserverContext/派发器（3114cc8）：Runtime 注册 + Agent 开关、EventTap 旁路、六项专项测试（顺序/丢弃/熔断/收尾/开关/归属） |
| M15 迁移收尾与文档同步 | ✅ 完成 | 2026-09-08 | README 0.2.0 形态、events.rs 观测旁路演示、CHANGELOG 0.2.0 全条目（a853533）；126 测试全绿 + clippy 零警告 |
