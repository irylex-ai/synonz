# Synonz 0.6.0 → 0.7.0 API 适配说明

- 状态: 已完成（0.7.0 实施记录）
- 日期: 2026-09-21
- 适用范围: 从 0.6.0 升级到 0.7.0 的下游应用；**0.6.0 序列化数据不支持**
  （无数据迁移；真相记录与记忆条目都不随行）
- 依据: ADR-0022（记忆组件边界与核心契约）、ADR-0023（核心契约收口）、
  ADR-0025（核心契约补强）、ADR-0026（分层记忆组件重定）、ADR-0027
  （主题检测独立扩展点）、架构设计文档 v8
- 配套: `CHANGELOG.md` 的 0.7.0 破坏项清单；`synonz-layered-memory` 的
  crate 文档（组件用法）

本文只讲"旧 → 新 / 替代做法"。架构理由见 ADR 与 v8，不在此重复。

---

## 1. 心智模型的三处变化

1. **记忆是能力，不是内核**：核心只持**契约族**（条目管理 / 读 / 写 /
   工厂）与**进程内默认实现**；分层记忆（L1/L2/L3）是官方组件
   `synonz-layered-memory`。
2. **读相位拥有模型输入**：装配器（assembler）产出
   `system(Agent)` 之后的**完整消息帧**（含本轮用户消息）；核心不再追加
   原始 input。装配器必须保证帧里含本轮用户消息，失败时自行降级（把原始
   input 放进帧 + 上报失败）；空帧时核心兜底（原始 input 组帧 + 失败
   事实）。
3. **模型访问统一在核心边界解析与叙述**：三个工厂各自可声明可选模型
   （`MemoryProvider` / `RewriterProvider` / `TopicDetectorProvider`），
   使用时按 `Provider 模型 ?? Agent 模型` 解析，交给实现者的句柄是核心的
   **叙述包装**（自动 `Requested`/`Responded`，无 `StreamDelta`）。

---

## 2. 旧类型 → 新类型（编译期对照）

| 旧（0.6.0） | 新（0.7.0） | 替代做法 |
|---|---|---|
| `Context`（公开具体类型） | crate 内部引擎 | 删除配置；读侧改写经 `AgentBuilder::rewriter_provider(...)`，其余由 runtime 的记忆 provider 承担 |
| `AgentBuilder::context(...)` | 同上 | 删除调用 |
| `ContextAssembler` | `MemoryContextAssembler` | 实现新 trait；载荷字段见下 |
| `ContextAssemblerInput` | `MemoryContextAssembleInput` | `new(reader, subject, conversation_id, topic, input, rewritten_input, model)` |
| `ContextAssemblerOutput` | `MemoryContextAssembleOutput` | `messages` = 完整消息帧（不再是"背景"）；`new(messages, failures)` |
| `AssemblyFailure { stage: MemoryFlowStage, … }` | `MemoryFailure { stage: String, … }` | 阶段自定字符串 |
| `MemorySummarizer` / `MemoryDistiller` | 组件语义策略 | 用 `synonz-layered-memory` 的 `Summarizer` / `EntityExtractor` 等，或自行实现 `MemoryPipeline` 钩子 |
| `ConversationTopicDetector` / `TopicDecision` | `TopicDetector` / `TopicDetectorProvider`（Agent 级） | 实现 `TopicDetector::detect(TopicDetectInput) -> Result<Option<String>, MemoryFailure>`；注册 `AgentBuilder::topic_detector_provider(...)` |
| `MemoryFlowError` | `MemoryFailure` | `stage`/`detail` 字符串 |
| `MemoryFlowStage` | 移除 | 阶段用字符串（核心不解释） |
| `Memory`（具体 facade） | `Memory`（trait） | `runtime.memory() -> Arc<dyn Memory>`；管理动词签名改为 `&str` 内容参数 |
| `MemoryType` | 组件富面 | 核心条目视图不再带类型；组件提供 `MemoryType`（`Summary`/`Knowledge`）与 `TypedItem` |
| `MemoryItem.memory_type` | 移除 | 用组件的 `list_typed` 或文档类型 |
| `MemoryQuery.memory_type` | 移除 | 用 `topic` / `scope` 过滤；类型过滤在组件富面 |
| `MemoryListCursor::new(memory_type, position)` | `MemoryListCursor::new(position)` / `start()` | 扁平列表 |
| `MemoryStoreQuery` / `L1Entry` / `L2Entry` / `L3Entry` / `L3Identity` | 移除 | 分层类型归组件（`L1MemoryEntry` / `L2MemoryEntry` / `L3MemoryGraphEntity` / `L3MemoryGraphEdge`；ADR-0028） |
| `MemoryL1Store` / `MemoryL2Store` / `MemoryL3Store` | 移除 | 存储轴归组件（四端口） |
| `RuntimeBuilder::memory_l1_store/l2_store/l3_store(...)` | `RuntimeBuilder::memory_provider(...)` | 注册 provider；缺省 = 核心进程内默认 |
| `Memory::reader` 载荷 | `MemoryReader`（`Memory` 的只读投影） | 由 `dyn Memory::reader()` 派生；只暴露条目级读（`list` / `get`，subject 按调用传）；策略经载荷获得 |
| `TurnInputRewriter::rewrite(input, history)` | `rewrite(RewriteInput)` | 材料对象：`input` / `history`（真相域最近成功轮）/ `model` |
| `MemoryEvent::FlowFailed` | `MemoryEvent::Failed { stage: String, … }` | `moment` 枚举改名 `MemoryFailedMoment` |
| `MemoryEvent::Compacted` / `Distilled` / `Promoted` | 移除 | 分层进展归组件观察面（`LayeredMemoryObserver`） |
| `MemoryEvent::Updated` / `Removed`（无 scope） | 增 `scope: MemoryScope` | 事实携带分区 |
| `Topic = String` | 保留 | 仍为 `MemorySource.topic` / `MemoryQuery.topic` 的别名 |

---

## 3. 事件与失败迁移

- 失败统一为 `MemoryEvent::Failed { stage, detail, moment }`：读侧
  （改写 / 装配）与写侧（钩子 / 后台任务 / 终结）都走它；
  `MemoryFailedMoment::{AfterTurn, AtConversationEnd, Background, Creation}`
  不变（仅改名）。
- 管理事实 `Updated { subject_id, scope, id }` / `Removed { subject_id,
  scope, ids }` 不含内容，携带**条目所在分区**。
- `ConversationEvent::TopicShifted { from, to }` 由核心在检测器判定
  **变化**时发出；首个 topic 的建立不算切换。

## 4. 管理面与 scope

- `MemoryItem` 新增 `scope: MemoryScope`（不透明字符串值，如
  `"project:abc"`）；核心不解释其语义。
- `MemoryQuery` 新增 `scope: Option<MemoryScope>`（`None` = 全部合并）与
  `topic: Option<String>`；`forget_matching` 可按 scope 批量。
- `get` / `edit` / `forget` 仍按 id；`edit` 的内容参数改为 `&str`。
- scope 的**来源、推导与注入属应用层**：核心不定义注入点，不引入项目 /
  工作区概念；组件（如 `synonz-layered-memory`）按自己的配置声明其服务的
  分区。

## 5. rewriter 的 history

- 核心从**真相域**取最近成功轮（用户输入 + 最终回答，过滤失败 / 取消）
  作为 `RewriteInput.history`；不再从记忆层取窗口，rewriter 契约保持
  实现中立。

## 6. 组件（新增 crate）

- `synonz-layered-memory` 是 0.7.0 新增的官方分层记忆组件（L1 按轮
  条目 / L2 批量压实 / L3 图谱+向量+蒸馏；四个存储契约；语义策略；
  embedding；组件观察面；ADR-0028 重定）。
- 注册方式：

```rust
use synonz::SynonzRuntime;
use synonz_layered_memory::LayeredMemoryProvider;

// 长记忆分区经 MemoryScopeResolver 在使用期解析（应用可查自己的
// 项目库；不配置则按 subject 推导 `user:<subject>`）。
let provider = LayeredMemoryProvider::builder()
    .scope_resolver(my_resolver)
    .build();
let actuator = provider.actuator();
let rewriter = provider.rewriter_provider();
let detector = provider.topic_detector_provider();
let runtime = SynonzRuntime::builder().memory_provider(provider).build();
let agent = synonz::Agent::builder()
    .runtime(&runtime)
    .model(model)
    .rewriter_provider(rewriter)
    .topic_detector_provider(detector)
    .build()?;
```

- 富面经 `provider.actuator()`（`l2_memory_entries` /
  `l3_memory_entities` / `l3_memory_relations` / `forget_l3_memory_relation`，
  一律按 subject 收窄）；核心管理面仍走 `runtime.memory()`（L2 条目与
  L3 实体映射为条目，同样按 subject 隔离）。组件模型可选：未配置时
  轮内维护回退 Agent 模型，会话终结维护以 `Failed` 事实显式失败。

## 7. 默认能力的变化（与 0.6.0 不同）

0.6.0 未配置存储时，核心自带**分层默认**（L1/L2/L3 + 可用的管理面）；
0.7.0 改为**进程内、非分层默认 provider**，能力边界如下：

- **有**：每个会话的最近消息窗口（有界，完成轮写入、读相位重放）——
  零配置即可多轮运行；
- **无**：管理面条目（`runtime.memory()` 的 `list` 恒空、`get` 恒
  `None`、`edit` 报 `EntryNotFound`、`forget*` 报 0）、跨会话 / 跨进程
  持久化、scope 语义（默认实现忽略 scope）；
- 真相记录（回合归档）与记忆是两条线：真相持久化由
  `RuntimeBuilder::conversation_store(...)` 承担（默认同样进程内）。

需要管理面 / 长期记忆 / 持久化时，注册 provider（官方
`synonz-layered-memory` 或自定义）；不注册即保持"零配置多轮"的开发
体验。

## 8. 未迁移项（明确边界）

- **0.6.0 序列化数据不支持**：`ConversationState` 与记忆条目都不做迁移；
  真相记录可用 `Conversation::export` 自行搬运，记忆需重新沉淀。
- 批量导入 / 迁移：由应用自持存储句柄在框架外完成（框架不提供日常新增
  动词）。
- scope 转换（`move` / `promote` / `demote`）：后续经组件富面加性引入。
