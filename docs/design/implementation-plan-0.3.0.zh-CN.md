# Synonz 0.3.0 实现计划

- 状态: RELEASED（2026-09-09 计划 VERIFIED；2026-09-10 发布执行完成——
  五 crate 0.3.0 已发布 crates.io、tag v0.3.0 已推送、GitHub Release
  已建；验证：139/139 全绿、clippy 零警告、旧 API 零残留、文档一致）
- 日期: 2026-09-09
- 依据: ADR-0017（APPROVED，含评审修订与评审修订二）、架构设计文档
  v4（APPROVED，含 Monitor 机制延期记录）、v4 评审决议
- 性质: 开发文档——0.3.0 破坏性单波发布的执行次序、迁移映射与验收
  基准；架构决策理由见 ADR-0017 与 v4 文档，本文不重复论证
- 前置: 0.2.0 代码完成但**不发布**（单波发布）；当前基线 = a339dd1
  之后的纯数据会话 + 命名清理（126/126 全绿、clippy 零警告基线）
- 关联延期项: Monitor 机制（ADR-0018，0.3.0 实施后启动）、后台工作
  专用模型（0.3.0 后优先）、S3 / 控制流影响（另立）

---

## 1. 总览

0.3.0 是**反应架构落地波**：把 ADR-0017（含评审修订二）与 v4 的
全部决策一次性实施为代码。五个里程碑按依赖链排序：

```
M16 事件词汇表与总线基础（类型骨架 + 总线设施）
  ↓
M17 记忆存储契约拆分（三契约位 + Memory 一等对象）
  ↓
M18 Context 状态引擎（Agent 级：契约 + 三槽 + 维护编排）
  ↓
M19 会话生命周期与执行链整合（new/end 三动作 + 执行循环接线）
  ↓
M20 收尾与验证（测试迁移、文档同步、全量回归、发布决策）
```

依赖理由：词汇表先立（全部波次的事件载荷类型）；存储拆分为引擎的
数据底座；引擎依赖总线（事件出口）与 Memory（载荷）；会话与执行链
接线依赖引擎就位；收尾扫全局。

**每一波的完成定义**：子项全部落地 + 新增/迁移测试绿 + 全量回归绿
（`cargo fmt --check` / `clippy --workspace --all-targets --all-features`
零警告 / `cargo test --workspace --all-features` 全通过）+ 与 v4 决策
落点核对。

---

## 2. 里程碑明细

### M16 事件词汇表与总线基础

| # | 子项 | 要点 |
|---|---|---|
| 1 | `AgentEvent` → `TurnEvent` 改名 | 类型族迁移；`LifecycleEvent`（Started/Completed/Failed/Cancelled）保持；`MemoryFlowFailed` 移出 |
| 2 | `SynonzEvent` 三实体族 | `Turn(TurnEvent)` / `Conversation(ConversationEvent)` / `Memory(MemoryEvent)`；`non_exhaustive`；Clone；serde 两级 tag |
| 3 | `ConversationEvent` | `Created { conversation_id, subject_id }` / `Ended { conversation_id, subject_id, reason: ConversationEndReason }`（`{Explicit, IdleSwept}`）/ `TopicShifted { conversation_id, from, to }` |
| 4 | `MemoryEvent` | `TurnArchived` / `Compacted` / `Distilled` / `Promoted` / `FlowFailed { stage, detail, moment: MemoryFlowFailedMoment }`（`{AfterTurn, AtConversationEnd, Background}`）；`MemoryFlowStage` 保留 |
| 5 | `round: Option<usize>` | Model/Tool 子族全量携带；执行循环维护计数并注入；`None` = 循环外维护调用（与 `CallPurpose` 互证） |
| 6 | `EventBus` 设施 | 观察车道（旁路 try_send·丢弃+lag·N 个 Observer·熔断·保序·终态 drain——全套继承 ObserverDispatcher 语义）+ 行动车道立法保留（0.3.0 无派发点）；`emit(SynonzEvent)` 类型锁死；常驻派发器（Runtime 生灭） |
| 7 | Observer 契约改签名 | `on_event(&ObserverContext, &SynonzEvent)`（`execution_id: Option<u64>`）；per-run 派发器与常驻通道合一 |
| 8 | EventTap 溶解 | 交付 = 显式 `consumer.send`（背压不变）；总线发布 = 显式 `bus.emit`——发射点双路显式化；旧 tap 类型退役 |

验收：SynonzEvent 全族 serde 往返测试；总线观察车道语义测试（保序/
丢弃计数/lag/熔断/终态 drain）；双路发射顺序测试（总线先感知）。

### M17 记忆存储契约拆分

| # | 子项 | 要点 |
|---|---|---|
| 1 | 三契约位 | `MemoryL1Store` / `MemoryL2Store` / `MemoryL3Store`（方法名不重复层名：append/len/pop_oldest；L3 upsert/query） |
| 2 | `Memory` 一等对象 | Arc 聚合三句柄；领域转发方法（l1_append/l2_append/l3_upsert…）；`runtime.memory()` 唯一出口；字段私有（无人可构造） |
| 3 | RuntimeBuilder 三注册位 | `memory_l1_store` / `memory_l2_store` / `memory_l3_store`（方法名 = 类型指代名） |
| 4 | 默认实现拆分 | `InMemoryStore` 退役 → 各层内置默认（L1 内置内存；L2/L3 进程内），实现 pub(crate) |
| 5 | 旧 `MemoryStore` 退役 | 统一三层契约删除；消费点迁移（现 trigger/Context 的 memory 调用改走 Memory 聚合） |

验收：三契约位独立可换测试（自定义实现注入）；Memory 聚合转发等价
测试；RuntimeBuilder 五主位形态定型（conversation_store / memory_l1 /
memory_l2 / memory_l3 / observer）。

### M18 Context 状态引擎（Agent 级）

| # | 子项 | 要点 |
|---|---|---|
| 1 | `trait Context` | 两方法：`assemble(ContextAssemblerInput) -> ContextAssemblerOutput` + `on_turn_completed(TurnContext) -> Vec<MemoryFlowError>`；`#[async_trait]`（依赖决策见 §5 风险） |
| 2 | `DefaultContext` | `new()` 零参数全默认 + `with_assembler` / `with_summarizer` / `with_topic_detector` / `l1_window` / `l2_cap` 链；**无实例状态** |
| 3 | 三策略槽契约（公开） | `ContextAssembler`（Input↔Output 对偶，原料域 non_exhaustive）/ `MemorySummarizer` / `ConversationTopicDetector` |
| 4 | 默认实现内化（pub(crate)） | `LayeredMemoryContextAssembler`（现 LayeredMemory 逻辑）/ `PromptMemorySummarizer`（现 summarize_l1 逻辑，with_summary_prompt/model 配置位）/ `FirstSegmentTopicDetector`（现 FirstSegmentDetector） |
| 5 | 维护编排搬移 | trigger.rs 逻辑零重写搬入：同步段（主题推进+L1 归档）+ 后台段（压缩+蒸馏，压缩改序：摘要→append→pop）+ Job 载荷（owned，spawn 友好） |
| 6 | 载荷类型 | `TurnContext { conversation, input, messages, memory: &Memory, model, events: EventSink, tasks: TaskRegistry }`；`ContextAssemblerInput { memory, subject, conversation_id, topic, input }` |
| 7 | 会话维护表 + TaskRegistry | Runtime 系统设施：`conversation_id → 后台任务句柄`；引擎后台段 spawn 登记；EventSink 克隆保活投递（后台失败 `FlowFailed{Background}`） |
| 8 | AgentBuilder 注册位 | `.context(ctx)`（缺省 = DefaultContext 全默认）；预设（react/research/reflection）打包引擎配方 |
| 9 | 事件接线 | 维护段 emit：`TurnArchived` / `Compacted` / `Distilled` / `Promoted` / `TopicShifted`（漂移判定语义后果）+ `FlowFailed{AfterTurn/Background}` |
| 10 | 终结收尾（Runtime 结构行为） | drain 会话维护表 + 机械促进 L2→L3（无模型调用无策略）；失败 `FlowFailed{AtConversationEnd}`；**on_conversation_ended 不入契约**（行动车道 0.3.0 无派发点） |

验收：引擎两方法时序测试（同步段下一轮必见/后台段滞后）；三槽独立
替换测试；整体替换（impl Context）测试；终结收尾 drain+促进测试；
后台失败经总线可见测试。

### M19 会话生命周期与执行链整合

| # | 子项 | 要点 |
|---|---|---|
| 1 | `Conversation::new/with_id` 带 runtime | 三动作：构造 + 初始 state 落库 + emit `Created`；签名与 of/end 同构 |
| 2 | `Conversation::end` 三动作 | async 化；幂等 ended（首次判定收口）；`is_ended()`；emit `Ended{reason}`；终结收尾调 Runtime 结构行为 |
| 3 | `ConversationState` 扩展 | 加 `ended` 字段；of 恢复 ended；state() 导出 |
| 4 | 执行循环接线 | 装配改 `agent.context().assemble(...)`；终态前 `on_turn_completed`（终态事件**之前**）；双路发射（总线先感知+交付）；round 注入 |
| 5 | 旧件退役 | 旧 Context 壳 / `conv.context()` / trigger.rs / `PostTurn` / `AgentEvent` / `MemoryStore` / `EventPolicy` / `MemoryPolicies` / 旧 builder 便捷位——全删 |
| 6 | sweep 重写（过渡态） | 调 `conv.end`、失败经总线、ended 过滤、返回值语义保留；`conversation_idle_timeout` 改名（Monitor ADR-0018 前的过渡态） |

验收：生命周期三入口幂等测试；连续对话竞态测试（快速 run 间归档
可见性）；sweep IdleSwept 路径测试（含失败经总线）；旧 API 零残留
扫描。

### M20 收尾与验证

| # | 子项 | 要点 |
|---|---|---|
| 1 | 测试全量迁移与补强 | 126 基线测试迁移 + 新增行为测试（总线语义/引擎时序/终结幂等/多 Agent 引擎交替）；全程 `--all-features` |
| 2 | 文档同步 | README / examples（events 观测示例改总线形态）/ CHANGELOG 0.3.0（破坏项+迁移指引=§3 映射表）/ rustdoc 全面更新（无 ADR 编号引用——coding.md 立法） |
| 3 | 全量回归 | fmt / clippy（零警告）/ test --all-features 全绿 |
| 4 | 发布决策 | 单波发布流程（bump 0.1.2→0.3.0 五 crate → dry-run → 依序 publish → tag v0.3.0 → GitHub Release）——**等 irylex 确认后执行** |

---

## 3. 迁移映射表（旧 → 新）

| 旧（0.2.0 代码） | 新（0.3.0） |
|---|---|
| `AgentEvent`（类型） | `TurnEvent`（经 `SynonzEvent::Turn`） |
| `AgentEvent::Lifecycle::MemoryFlowFailed` | `MemoryEvent::FlowFailed`（+ moment 载荷） |
| `ModelEvent` / `ToolEvent` | 同名（载荷加 `round: Option<usize>`） |
| `ExecutionEvent` | 不变（投影源改为 TurnEvent） |
| `Observer::on_event(&AgentEvent)` | `on_event(&SynonzEvent)`；`execution_id: Option<u64>` |
| `AgentBuilder::observability(bool)` | **退役**（总线无条件发布：观察位注册即全量目击，无 per-agent 门——0016 完备性原则的延伸） |
| ObserverDispatcher（per-run） | 总线常驻派发器（观察车道） |
| `EventTap` | 溶解（显式双路：bus.emit + consumer.send——`EventSink::emit_turn`/`emit_memory`） |
| `EventBus::emit`（v4 草图为 async + DispatchOutcome） | 实施勘误：**同步** `fn emit`（try_send 非阻塞立法 + 同步发射点需要：`Conversation::new`）且无 DispatchOutcome（行动车道 0.3.0 无派发点，无消费者）——M20 文档同步 |
| `MemoryStore`（统一三层） | `MemoryL1/L2Store`/`MemoryL3Store` 三契约位 + `Memory` 一等对象 |
| `InMemoryStore` | 拆为各层内置默认（pub(crate)） |
| `runtime.memory_store()` / `.memory_policies()` / `.topic_detector()` | `runtime.memory()`；策略进 DefaultContext |
| `RuntimeBuilder.memory_store` / `.memory_policies` / `.topic_detector` | `memory_l1_store` / `memory_l2_store` / `memory_l3_store`（+ Agent 级 `.context()`） |
| 旧 Context 壳 / `conv.context()` / `Context::for_conversation` | 删除；`trait Context` 状态引擎（Agent 级）新立 |
| `ContextAssembly` / `AssemblyRequest` / `AssemblyOutput` | `ContextAssembler` / `ContextAssemblerInput` / `ContextAssemblerOutput` |
| `LayeredMemory` | `LayeredMemoryContextAssembler`（pub(crate)） |
| `summarize_l1`（硬编码） | `MemorySummarizer` 契约 + `PromptMemorySummarizer`（pub(crate)） |
| `TopicDetector` / `FirstSegmentDetector` | `ConversationTopicDetector` / `FirstSegmentTopicDetector`（pub(crate)） |
| `MemoryPolicies` / `EventPolicy` | 退役（`l1_window`/`l2_cap` builder 数值 + 结构行为） |
| `trigger.rs`（run_post_turn_flows / run_end_flows / flush_l1_into_l2 / summarize_l1 / PostTurn） | `DefaultContext` 维护编排（同步段/后台段/Job）+ Runtime 终结收尾（drain+机械促进） |
| `Conversation::new(&subject)`（纯构造） | `Conversation::new(&runtime, &subject)`（三动作） |
| `Conversation::end`（同步、内联促进） | async 三动作 + Runtime 收尾（init） |
| `Conversation::context()` | 删除 |
| `idle_timeout`（builder 位） | `conversation_idle_timeout`（过渡态，Monitor ADR-0018 后重审） |
| `CallPurpose` / `MemoryFlowStage` / `CancelReason` / `TokenUsage` / `ModelDelta` / 消息与工具载荷类型 | 不变 |

---

## 4. 验收总标准

1. 五里程碑逐项完成，每波全量回归绿；
2. 旧 API 零残留扫描：`AgentEvent` / `MemoryStore`（统一契约）/
   `ContextAssembly` / `AssemblyRequest` / `AssemblyOutput` /
   `MemoryPolicies` / `EventPolicy` / `EventTap` / `PostTurn` /
   `LayeredMemory`（公开形态）/ `FirstSegmentDetector`（公开形态）/
   `conv.context()` / 旧 `Conversation::new` 签名 / `l1_store`（旧
   builder 名）/ `idle_timeout`（旧 builder 名）——全部为 0；
3. 行为验收：总线观察车道全套语义 / 双路发射顺序 / 引擎时序（同步段
   屏障 + 后台段滞后 + 终结 drain）/ 终结幂等 / 多 Agent 引擎交替
   （策略一致性归应用——文档写明）/ 后台失败经总线可见；
4. 文档一致：v4 与代码落点核对通过；CHANGELOG 0.3.0 完整（破坏项 +
   迁移指引）；rustdoc 无 ADR 编号引用；
5. 延期项记录在案：Monitor（ADR-0018）/ 后台专用模型 / S3 / 控制
   流影响 / 下游自定义事件 / ended 状态门。

---

## 5. 风险与开放项

| 风险/开放项 | 处置 |
|---|---|
| `async_trait` 依赖 | dyn 兼容 async trait 需要 desugar（成熟小依赖 vs 手写 `Pin<Box<dyn Future>>`）——M18 开工时定，遵守保守依赖评估 |
| serde 演进 | 两级 tag 起步；版本兼容惯例（加字段带默认值）——非破坏演进 |
| 测试迁移规模 | 126 基线 + 新增行为测试——M16-M19 每波随迁，不积压到 M20 |
| `EventSink` / `TaskRegistry` 精确形态 | M18 实施期按命名族定稿（载荷字段类型） |
| Monitor 机制（ADR-0018） | 0.3.0 实施完成后启动——不在本计划 |
| 后台专用模型 | 0.3.0 后优先（Summarizer model 参数已预留） |
| 发布节奏 | M20 的发布执行等 irylex 单独确认 |
