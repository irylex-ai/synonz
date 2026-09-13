# Synonz 0.5.0 实现计划

- 状态: VERIFIED（2026-09-13，M28-M32 完成并验证——全量回归
  221/221、clippy/doc 零警告、fmt 干净；发布决策待 irylex 确认）
- 日期: 2026-09-13
- 依据: ADR-0019（APPROVED——上下文引擎扩展面）、架构设计文档
  v6（APPROVED）
- 性质: 开发文档——0.5.0 破坏性单波发布的执行次序、迁移映射与验收
  基准；架构决策理由见 ADR-0019 与 v6，本文不重复论证
- 前置: 0.4.0 已发布（2026-09-13；216/216 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.4.0；文档基线 = ADR-0019 + v6（均
  APPROVED，v5 已 SUPERSEDED）
- 关联延期项: 整段写侧扩展（触发条件：无法用子钩子/存储满足的真实
  需求；形态：窄钩子=只读视野+结构化贡献）、会话终结 flush（G2）、
  远程存储（G3）、辅助叙述 `emit_deltas` 开关、T2 主题注册表、
  S3 / 控制流影响 / ended 状态门（另立）

---

## 1. 总览

0.5.0 是**上下文引擎扩展面转型落地波**：把 ADR-0019 与 v6 的全部
决策一次性实施为代码。五个里程碑按依赖链排序：

```
M28 引擎形态转型（trait Context 退役 + 具体 Context + 内部入口）
  ↓
M29 命名统一与 API 修补（L2Entry/L3Entry/L3Identity + 时间戳 + 主题访问器）
  ↓
M30 读侧：MemoryReader 只读门面 + TurnInputRewriter 槽 + 组装输入改造
  ↓
M31 写侧：摘要收敛 + MemoryDistiller 槽 + with_model 与叙述包装
  ↓
M32 收尾与验证（文档、扩展指南、示例、全量回归、发布决策留后）
```

依赖理由：M28 先定引擎形态（具体 `Context`、`Agent` 无擦除、内部
入口）——其余全部工作挂在它上面；M29 是机械改名与 API 修补，越早
落定越少冲突；M30 建只读门面与读侧改写（`MemoryReader` 同时是
M31 蒸馏槽的物料面）；M31 收敛写侧与模型归属；M32 收尾扫全局。

**每一波的完成定义**：子项全部落地 + 新增/迁移测试绿 + 全量回归绿
（`cargo fmt --check` / `clippy --workspace --all-targets --all-features`
零警告 / `cargo test --workspace --all-features` 全通过 /
`cargo doc --workspace --no-deps` 零警告）+ 与 v6 决策落点核对 +
**里程碑完成记录**（含 B 类决议与 A 类请示结果）写入本计划。

---

## 2. 里程碑明细

### M28 引擎形态转型（契约退役）

| # | 子项 | 要点 |
|---|---|---|
| 1 | 具体 `Context` | 删除公开 `trait Context`；`DefaultContext` **改名 `Context`**（保留 `new()` / `Default`）；字段 = 读侧槽 + 写侧子钩子 + `with_model` + 楼层参数 |
| 2 | Agent 接线 | `Agent` / `AgentLoopTask` 由 `Arc<dyn Context>` 改为 `Arc<Context>`；`AgentBuilder::context(context: Context)` 按值接收、内部转 `Arc`；缺省 `Context::new()` |
| 3 | 内部入口 | `assemble` / `on_turn_completed` 收为 `pub(crate)`；`on_turn_completed` 改**显式 7 参**：`conversation` / `input` / `messages` / `memory` / `agent_model` / `events` / `task_spawner`；**移除公开 `TurnContext`**（内置维护改用私有载荷，行为等价） |
| 4 | 调用点与导出 | agent 循环装配调用保持（`assemble` 签名不变）；轮末调用改显式参数；`lib.rs` 重导出：`Context` 取代 `DefaultContext`，移除 `Context` trait / `TurnContext` |
| 5 | 测试迁移 | `tests/agent.rs` 的 `TranscriptContext`（自实现契约）迁移为 `Context` 配置（确定性：低于 L1 溢出的轮数或确定性摘要实现，避免后台摘要竞争） |
| 6 | 验证 | 全量回归（行为等价）；全仓无 `impl Context for` / `Arc<dyn Context>` / 公开 `TurnContext` 残留 |

### M29 命名统一与 API 修补

| # | 子项 | 要点 |
|---|---|---|
| 1 | 条目改名 | `SummaryBlock` → `L2Entry`、`KnowledgeFragment` → `L3Entry`、`FragmentIdentity` → `L3Identity`——定义、构造、`Memory` facade、惰性存储实现（`inprocess.rs`）、运行时终结促进（`runtime.rs`）、测试与 rustdoc 全量替换 |
| 2 | 时间戳与构造器 | `L1Entry::new(...)` + `created_at`（外部 L1 store 返回条目的硬缺口）；`L2Entry.created_at`（`new` 自动置时间）；时间源统一（`now_epoch`） |
| 3 | 主题访问器 | `Conversation::topic()` / `set_topic()` 由 `pub(crate)` 改**公开**（rustdoc 写明 T2 语义：活跃主题=框架标签；多主题集合/切换轨迹归引擎态） |
| 4 | 验证 | 全仓旧名零残留（`grep` 扫描）；文档引用同步；编译面完整 |

### M30 读侧：只读门面与改写槽

| # | 子项 | 要点 |
|---|---|---|
| 1 | `MemoryReader` | 新公开只读值门面（subject 作用域绑定；`l1_window` / `l1_len` / `l2_read` / `l2_len` / `l3_query` / `l3_len`）；`Memory::reader(&subject)`；写与策展方法**在类型上不存在**；`Memory` 自身读写 facade 保持 |
| 2 | 组装输入改造 | `ContextAssemblerInput.memory: &Memory` → `reader: MemoryReader<'a>`（破坏项）；`ContextAssemblerInput::new(...)` 同步改签名；内置装配器改用 `reader` |
| 3 | `TurnInputRewriter` | 新公开策略槽：`rewrite(input, history) -> Result<Option<String>, String>`（`None`=原文；`Err`=可见降级保留原文）；`Context::with_rewriter`（缺省 = 无改写） |
| 4 | 引擎编排 | `Context::assemble` 签名不变；内部：`input.reader` 取 L1 窗口（本波 B 类：历史 = L1 窗口，按 `l1_window` 上限）→ rewriter → 用 `rewritten_input` 重建 input → `ContextAssembler::assemble` |
| 5 | 失败语义 | rewriter `Err` → 保留原文 + 追加 `AssemblyFailure { stage: Rewrite, detail }`（`MemoryFlowStage` 加性变体）；无 rewriter 时行为与 0.4.0 完全一致 |
| 6 | 验证 | 改写进入组装输入而真相（canonical / `Turn` / L1）保留原文；无 rewriter 与 0.4.0 等价；`MemoryReader` 只读（装配器不可写）；失败可见 |

### M31 写侧：摘要收敛、蒸馏槽与模型叙述归属

| # | 子项 | 要点 |
|---|---|---|
| 1 | 摘要收敛 | `MemorySummarizer::summarize(entries, model)` **移除 `events` 参数**（破坏项）；内置实现删除手工 `Requested`/`Responded` 发射；`PromptMemorySummarizer` 退化为无字段私有默认实现（名字不再保留，原因：prompt/model 均已不可配） |
| 2 | `MemoryDistiller` 槽 | 新公开策略槽：`distill(blocks, conversation_id, topic, reader, model) -> Result<Vec<String>, String>`；`Context::with_distiller`；默认实现 = 现有机械提升；**先变换、成功后再 pop**（失败保留 L2 + `FlowFailed{Distill}`） |
| 3 | 模型角色 | `with_summary_model` → **`with_model`**（破坏项）；**移除 `with_summary_prompt`**（破坏项）；解析 `with_model ?? agent_model` 收口引擎（轮末内部）；记忆跟随上下文（无独立记忆模型） |
| 4 | 叙述包装 | crate 内部 `NarratedModel`：调用前 `ModelEvent::Requested`、`Finish` 时 `Responded`（`CallPurpose::ContextManagement`、`round: None`）；**永不发 `StreamDelta`、无 `emit_deltas` 开关**；引擎交给摘要/蒸馏的模型句柄为叙述包装；推理循环手动叙述不变 |
| 5 | 观测一致性 | 流程事实（`TurnArchived`/`TopicShifted`/`Compacted`/`Distilled`/`FlowFailed`）由引擎发；槽契约不含 `EventSink`；自持客户端调用可观测性由实现自担（边界） |
| 6 | 验证 | 自定义槽只调用传入 `model`、自身不发事件 → 观察者仍收 `Requested`/`Responded`；`with_model` 生效（摘要/蒸馏调用网关）；蒸馏失败不丢 L2；移除项零残留 |

### M32 收尾与验证（含发布决策留后）

| # | 子项 | 要点 |
|---|---|---|
| 1 | 测试全量 | 全量回归（fmt / clippy / doc / test --all-features）；新增测试按 §4；新引入 flaky 即缺陷 |
| 2 | 文档 | v6 落点核对；**扩展指南** `docs/design/extension-guide-context-memory.zh-CN.md`（扩展点映射 + G2/G3 边界 + 模型角色 + T2 主题 + Route A 迁移指引 + 常见坑）；**ADR-0017 修订注记**（配置面 `with_summary_prompt`/`with_summary_model` 失效、装配输入 `memory`→`reader`）；CHANGELOG 0.5.0（Highlights / 破坏项 / 迁移表）；README 指针（v5→v6、实施计划 0.4.0→0.5.0）；rustdoc（无 ADR 编号引用——coding.md 立法） |
| 3 | 示例 | `chat_tui` 等示例随 API 迁移核对；如触及变更则更新并实跑验证 |
| 4 | 命名终稿 | 按 coding.md §5 核对：`Context` / `TurnInputRewriter` / `MemoryDistiller` / `MemoryReader` / `L2Entry` / `L3Entry` / `L3Identity` |
| 5 | 发布决策 | 单波发布流程（bump 五 crate 0.4.0→0.5.0、同步五份 crate README、dry-run、依序 publish、tag `v0.5.0`、GitHub Release）——**等 irylex 确认后执行**；版本号按纳入范围在发布准备时定 |

---

## 3. 迁移映射表（旧 → 新）

| 旧（0.4.0 代码） | 新（0.5.0） |
|---|---|
| `impl Context`（自定义整台引擎） | 配置 `Context`：读侧槽（`with_rewriter`/`with_assembler`）+ 写侧子钩子（`with_topic_detector`/`with_summarizer`/`with_distiller`）；**整段写侧扩展退役**（边界，触发条件见 §5） |
| `DefaultContext` | `Context`（具体类型；`new()` 全默认） |
| `Agent` 持 `Arc<dyn Context>` | `Arc<Context>`；`AgentBuilder::context(context: Context)` |
| `ContextAssemblerInput.memory: &Memory` | `.reader: MemoryReader<'_>`（`new()` 签名同步） |
| （无） | `ContextAssemblerInput.rewritten_input: Option<&str>`（引擎填充） |
| （无） | `TurnInputRewriter` 槽（组装前执行；缺省无改写） |
| `MemorySummarizer::summarize(entries, model, events)` | `summarize(entries, model)`（叙述由框架统一保证） |
| （硬编码机械蒸馏） | `MemoryDistiller` 槽（默认机械提升；先变换后 pop） |
| `with_summary_model` | `with_model`（引擎模型；缺省 = agent 模型） |
| `with_summary_prompt` | **移除**——提示词是策略内容；自定义 = 实现 `MemorySummarizer` |
| （无） | `MemoryReader` / `Memory::reader()`（策略槽只读物料面） |
| `SummaryBlock` / `KnowledgeFragment` / `FragmentIdentity` | `L2Entry` / `L3Entry` / `L3Identity` |
| `L1Entry`（无公开构造器） | `L1Entry::new(...)` + `created_at`；`L2Entry.created_at` |
| `Conversation::topic()` / `set_topic()`（`pub(crate)`） | 公开（T2：活跃主题=框架标签） |
| `TurnContext`（公开轮终态载荷） | **移除**（引擎内部；不外放写侧能力） |

---

## 4. 验收总标准

1. **五里程碑逐项完成**，每波全量回归绿（fmt / clippy / doc / test）；
2. **行为验收**：无 rewriter 时与 0.4.0 完全等价（纯加性兼容）；
   有 rewriter 时只影响组装输入（召回），canonical / `Turn` / L1 保留
   原文；`with_model ?? agent_model` 解析并在摘要/蒸馏调用上生效；
   辅助调用自动叙述（`Requested`/`Responded`、无 `StreamDelta`）；
   蒸馏失败不丢 L2（先变换后 pop）；
3. **公开面验收**：读写不对称（读侧整段可换、写侧仅子钩子）；
   `MemoryReader` 在类型上不可写；`trait Context` / 公开 `TurnContext`
   / `DefaultContext` 零残留；移除项（`with_summary_prompt`）零残留；
4. **命名验收**：`L2Entry` / `L3Entry` / `L3Identity` 全仓零旧名；
   `L1Entry::new` 可用；`Conversation::topic()`/`set_topic()` 公开可用；
5. **文档一致**：v6 与代码落点核对通过；扩展指南落地；ADR-0017
   修订注记在案；CHANGELOG 0.5.0 完整（破坏项 + 迁移表）；README
   指针更新；rustdoc 无 ADR 编号引用；
6. **观测验收**：自定义槽不持 `EventSink`、只调用传入模型，观察者
   仍能收到模型叙述事实；流程事实不依赖槽实现；
7. **延期项记录在案**：整段写侧扩展（窄钩子触发条件）/ G2 会话终结
   flush / G3 远程存储 / `emit_deltas` 开关 / T2 主题注册表 / S3 等。

---

## 5. 风险与开放项

**已决项**（预先定案——不留到实施期逐段处理，避免遗漏）：

| 项 | 决议 |
|---|---|
| rewriter 历史窗口 | 引擎传入 **L1 窗口**（按 `l1_window` 上限；不额外配置面）；rewriter 自行决定使用多少 |
| rewriter 失败上浮 | 保留原文 + `AssemblyFailure { stage: Rewrite }`；`MemoryFlowStage` 增 `Rewrite` 变体（`non_exhaustive`，加性） |
| `NarratedModel` 归属 | crate 内部（`model.rs` 或 `context.rs`）；仅服务辅助调用；**无** `emit_deltas` 开关 |
| 蒸馏默认实现 | 现有机械提升；**先变换后 pop**（失败保留 L2） |
| `TurnContext` | **移除**（无公开消费者）；内置维护用私有载荷 + `BackgroundMaintenanceTask` |
| 兼容承诺 | `Context::new()` 且未配置新槽时行为与 0.4.0 等价（纯加性兼容判断基准） |
| `TranscriptContext` 迁移 | 改为 `Context` 配置；测试确定性：轮数低于 L1 溢出阈值或确定性摘要实现 |
| 装配失败 moment | 保持 `AfterTurn` 语义（`AssemblyFailure` 上浮路径不变） |
| 扩展指南命名 | `docs/design/extension-guide-context-memory.zh-CN.md` |
| 轮末失败时刻 | 流程事实沿用既有 `MemoryFlowFailedMoment`（不新增变体） |

**实施期决策协议**（对涌现事项——实施中新出现的决策点）：

- **A 类（用户门控）**：影响公开 API 契约、可观察行为语义或不可逆
  承诺的事项——带选项与推荐向 irylex 请示，确认后实施；
- **B 类（代理可决，按既定规则）**：内部结构、命名（coding.md §5
  命名立法）、有界影响的具体值与测试策略——由实施代理按既定规则
  定案：开工时明确、完工时记录；
- **记录义务**：A 类进 ADR/v6（涉及语义时）；B 类进本计划的
  "里程碑完成记录"与提交信息——决议不得只存在于聊天或代码中。

**执行期约定**：

| 项 | 约定 |
|---|---|
| 发布节奏 | M32 发布执行等 irylex 单独确认 |
| 版本号 | 工作标签 0.5.0（MINOR：加性新能力 + 破坏项同波）；发布准备时按纳入范围定 |
| 单波策略 | 破坏项集中本波发布，不拆分（与 ADR-0019 一致） |

---

## 6. 里程碑完成记录

### M28 引擎形态转型（契约退役）✅（2026-09-13）

- **具体 `Context`**：公开 `trait Context` 删除；`DefaultContext` 改名
  具体类型 `Context`（`new()` / `Default` / 构造链不变）；`Agent` 与
  `AgentLoopTask` 持 `Arc<Context>`（无类型擦除）；`AgentBuilder::context`
  按值接收 `Context`；缺省 `Context::new()`；
- **内部入口**：`assemble` / `on_turn_completed` 收为 `pub(crate)` 固有
  方法；`on_turn_completed` 改显式 7 参（`conversation` / `input` /
  `messages` / `memory` / `agent_model` / `events` / `task_spawner`）；
  公开 `TurnContext` 移除（内置维护直接用参数 + 既有
  `BackgroundMaintenanceTask`）；
- **导出面**：`lib.rs` 重导出更新——`Context` 取代 `DefaultContext`，
  移除 trait `Context` 与 `TurnContext`；
- **测试迁移**：`tests/agent.rs` 的 `TranscriptContext`（自实现契约）
  退役，回归测试改走默认引擎（确定性来源：轮数低于 L1 溢出阈值）；
  `tests/memory.rs` 三个直接驱动引擎的装配测试改为 **Agent 端到端
  观测**（模型请求内容 + 总线 `FlowFailed` 事实，新增 `StageRecorder`
  观察者）——验证切入点从"引擎方法"转为"外部可观察行为"；
- **B 类决议**：`on_turn_completed` 保留最小充分集 7 参（clippy 阈值
  处）——局部 `#[allow(clippy::too_many_arguments)]` + 理由注记；不开
  公开入口；
- **验证**：全量回归 **216/216** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净；全仓无 `impl Context for` /
  `Arc<dyn Context>` / 公开 `TurnContext` 残留。

（后续里程碑按 M21-M27 体例继续写入。）

### M29 命名统一与 API 修补 ✅（2026-09-13）

- **条目改名**：`SummaryBlock` → `L2Entry`、`KnowledgeFragment` →
  `L3Entry`、`FragmentIdentity` → `L3Identity`——定义、`Memory` facade、
  惰性存储（`inprocess.rs`）、终结促进（`runtime.rs`）、蒸馏
  （`context.rs`）、导出面（`lib.rs`）、测试与 rustdoc 全量替换；
- **时间戳与构造器**：`L1Entry::new(conversation_id, topic, messages)` +
  `created_at`（外部 L1 store 无法构造条目的硬缺口关闭）；
  `L2Entry.created_at`（`new` 自动置时间）；`L3Entry::new` 沿用；
- **主题访问器**：`Conversation::topic()` / `set_topic()` 公开（rustdoc
  写明 T2 语义：活跃主题=框架标签；多主题集合/切换轨迹归引擎态）；
- **验证**：全量回归 **216/216** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净；全仓旧名零残留。

### M30 读侧：只读门面与改写槽 ✅（2026-09-13）

- **`MemoryReader`**：公开只读值门面（`Memory::reader(subject)`；
  `l1_window` / `l1_len` / `l2_read` / `l2_len` / `l3_query` /
  `l3_len`）——写与策展方法**在类型上不存在**（编译期拒绝）；`Memory`
  读写 facade 保持（应用面照用）；
- **组装输入改造**：`ContextAssemblerInput.memory: &Memory` →
  `reader: MemoryReader<'a>`（破坏项）；`new()` 同步改签名并新增
  `rewritten_input: Option<&str>`（引擎填充）；内置装配器改用
  `reader`，L3 召回按 `rewritten_input.unwrap_or(input)`（模型视图）；
- **`TurnInputRewriter`**：新公开读侧槽（`rewrite(input, history)`；
  `None`=原文；`Err`=可见降级保留原文）；`Context::with_rewriter`
  （缺省 = 无改写）；
- **引擎编排**：`Context::assemble` 内部——reader 取 L1 窗口 →
  rewriter → 用 `rewritten_input` 重建 input → assembler；无 rewriter
  时行为与 0.4.0 等价（纯加性兼容）；
- **词表**：`MemoryFlowStage` 增 `Rewrite` 变体（`non_exhaustive`，
  加性）；失败经 `AssemblyFailure { stage: Rewrite }` 上浮；
- **测试**：新增 2 项——rewriter 模型视图进入组装（自定义装配器观测
  `rewritten_input`；当前 user 消息仍为原文）、rewriter 失败可见且
  降级（总线 `FlowFailed { Rewrite }` + 原输入到达模型）；
- **验证**：全量回归 **218/218** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净。

### M31 写侧：摘要收敛、蒸馏槽与模型叙述归属 ✅（2026-09-13）

- **摘要收敛**：`MemorySummarizer::summarize(entries, model)` **移除
  `events` 参数**（破坏项）；内置实现（`DefaultMemorySummarizer`）
  删除手工 `Requested`/`Responded` 发射；`PromptMemorySummarizer` 名字
  退役（prompt/model 均不可配，无字段默认实现）；
- **`MemoryDistiller` 槽**：新公开策略槽 `distill(blocks,
  conversation_id, topic, reader, model) -> Result<Vec<String>, String>`；
  `Context::with_distiller`；默认 `MechanicalMemoryDistiller`（机械
  提升）；**先变换、成功后再 pop**（失败保留 L2 +
  `FlowFailed { Distill }`）；
- **模型角色**：`with_summary_model` → **`with_model`**（破坏项）；
  **移除 `with_summary_prompt`**（破坏项）；`Context` 持 `model` 字段，
  轮末解析 `with_model ?? agent_model`；
- **叙述包装**：crate 内部 `NarratedModel`——`Requested` 前置、
  `Responded` 于 `Finish`（`CallPurpose::ContextManagement`、
  `round: None`），**永不发 `StreamDelta`、无 `emit_deltas` 开关**；
  推理循环手动叙述不变；内容槽不持 `EventSink`；
- **测试**：新增 3 项——辅助调用由框架叙述（自定义 summarizer 只调用
  传入句柄、自身不发事件，观察者仍收 `Requested`/`Responded`）、
  `with_model` 路由（推理走 agent 模型、辅助走引擎模型）、蒸馏失败
  保留 L2 且可见（`FlowFailed { Distill }`、L3 为空）；
- **验证**：全量回归 **221/221** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净。

### M32 收尾与验证 ✅（2026-09-13）

- **文档**：v6 落点核对通过（引擎形态 / 读侧槽 / 写侧子钩子 /
  `MemoryReader` / 命名 / 时间戳与代码一致；v6 补实施记录与状态表
  更新）；**扩展指南**落地 `docs/design/extension-guide-context-memory.
  zh-CN.md`（扩展点总图、四档路径、读/写配方、存储接入与 G3、模型
  角色、边界绕行、T2 主题、Route A 映射、常见坑）；**ADR-0017 评审
  修订四**（引擎契约与配置面更新：`with_summary_prompt` 移除、
  `with_summary_model` → `with_model`、装配输入 `memory` → `reader`）；
  **CHANGELOG 0.5.0**（Highlights / Added / Changed / Breaking /
  Migration / Notes——Unreleased，发布准备时定稿）；README 指针
  （v6 + 0.5.0 计划）已在 v6 批准时更新；
- **示例**：`scheduler` 实跑通过（自动保存 ×3、系统任务快照、
  shutdown 收尾）；其余示例（含 `chat_tui`）随 API 编译通过，未触及
  移除面；
- **命名终稿**：按 coding.md §5 核对通过（`Context` /
  `TurnInputRewriter` / `MemoryDistiller` / `MemoryReader` / `L2Entry` /
  `L3Entry` / `L3Identity`）；rustdoc / 测试注释无 ADR 编号引用
  （coding.md 立法）——清出 2 处并修正；
- **全量验证**：`cargo fmt --check` 干净、`clippy --workspace
  --all-targets --all-features` 零警告、`cargo doc --workspace
  --no-deps` 零警告、`cargo test --workspace --all-features`
  **221/221** 全绿；
- **发布决策**：**待 irylex 确认**——版本号 0.5.0（工作标签）、五份
  crate README 同步、依序 publish、tag `v0.5.0`、GitHub Release；
  本波止于"等待发布"。
