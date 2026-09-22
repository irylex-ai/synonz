# Synonz 0.7.0 实现计划

- 状态: DRAFT（待 irylex 人工评审）
- 日期: 2026-09-20
- 依据: ADR-0022（记忆组件边界与核心契约）、ADR-0023（核心契约收口）、
  ADR-0025（核心契约补强）、ADR-0026（分层记忆组件按《上下文记忆系统
  全链路技术方案》重定）、ADR-0027（主题检测独立扩展点）、架构设计
  文档 v8（APPROVED）；ADR-0021 / ADR-0024 已 SUPERSEDED
- 性质: 开发文档——0.7.0 破坏性单波发布的执行次序、迁移映射与验收
  基准；架构决策理由见上述 ADR 与 v8，本文不重复论证
- 前置: 0.6.0 已发布（2026-09-19；232/232 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.6.0；文档基线 = ADR-0022/0023/0025/0026 +
  v8（均 APPROVED；v7/0021/0024 已 SUPERSEDED）
- 关联延期项（已记录边界）: provider 装配命名细节、组件包装、观察
  事件清单与命名、L2 缓存/持久化拆分、scope 转换、scope 获取机制
  （应用层）、L1 留存与会话级清理、语义搜索/矛盾消解（组件策略层）

---

## 1. 总览

0.7.0 是**记忆组件化与分层记忆模型落地波**：核心收口为"契约 + 内置
引擎 + 进程内默认实现"（并入 ADR-0025 的补强），官方组件
`synonz-layered-memory` 按《上下文记忆系统全链路技术方案》重定。
六个里程碑按依赖链排序：

```
M39 核心契约族与事件/视图重切（三契约 + 两工厂 + 叙述 LLM + 完整帧 +
    MemoryScope + Failed/事实 scope + 主题检测独立扩展点；
    ADR-0022/0023/0025/0027）
  ↓
M40 内置引擎接线与核心默认 provider（Context 内部化 / 装配点 /
    完整帧产出 / 空帧兜底 / 角色约定）
  ↓
M41 组件骨架：crate + 分层模型 + 四端口（L1 双轨 / L2 事件 /
    L3 图谱+向量；进程内默认）
  ↓
M42 组件管线（四阶段）：指代消解 / 主题检测 / 召回评分去重冷启动
    改写成帧 / 写侧异步（L2 事件、结构化摘要、图谱）
  ↓
M43 组件 provider + 富面 + 观察面 + embedding 端口 + 策略与参数
  ↓
M44 收尾与验证（示例 / API 适配说明 / 扩展指南 / CHANGELOG /
    README / 全量回归；发布决策留后）
```

依赖理由：M39 先立核心契约与词汇（含 0025 的叙述 LLM、完整帧、
`MemoryScope`）；M40 完成引擎内部化与默认能力（零配置即用、核心
回归绿）；M41 建组件 crate 与分层模型/端口；M42 落四阶段管线
（文档实现逻辑的主体）；M43 完成组件公开面（富面、观察面、
embedding、策略与真实后端适配）；M44 收尾扫全局。

**每一波的完成定义**：子项全部落地 + 新增/迁移测试绿 + 全量回归绿
（`cargo fmt --check` / `clippy --workspace --all-targets --all-features`
零警告 / `cargo test --workspace --all-features` 全通过 /
`cargo doc --workspace --no-deps` 零警告）+ 与 v8 决策落点核对 +
**里程碑完成记录**（含 B 类决议与 A 类请示结果）写入本计划。

---

## 2. 里程碑明细

### M39 核心契约族与事件/视图重切

| # | 子项 | 要点 |
|---|---|---|
| 1 | 契约族 | `Memory`（trait，条目管理）、`MemoryContextAssembler`（`MemoryContextAssembleInput{reader, subject, conversation_id, topic, input, rewritten_input, **model**} / `MemoryContextAssembleOutput{messages, failures}`）、`MemoryPipeline`（模板 + **三钩子** `archive_turn`/`spawn_task`/`finalize_conversation`）、`PipelineTurnContext`/`PipelineConversationContext`（+ **叙述 LLM** + **本轮 topic 变更**）、`MemoryProvider` / `RewriterProvider`、**`TopicDetectorProvider` / `TopicDetector`**（Agent 级；可选模型同 rewriter；ADR-0027）、`MemoryFailure`、`RewriteInput` |
| 2 | 作用域 | 新增**不透明 `MemoryScope`**（字符串值）；`MemoryItem.scope`；`MemoryQuery.scope: Option<MemoryScope>`（None = 全部合并）；`forget_matching` 按 scope 批量；`get`/`edit`/`forget` 按 id |
| 3 | 事件词汇 | `MemoryEvent::{TurnArchived, Updated{subject_id, scope, id}, Removed{subject_id, scope, ids}, Failed{stage, detail, moment}}`；`MemoryFailedMoment`；移除 `FlowFailed`/`MemoryFlowStage`/`Compacted`/`Distilled`/`Promoted` |
| 4 | 核心视图 | `MemoryItem` 最小集（id/content/source/scope/created_at/updated_at）；`MemoryQuery` 去 `memory_type`、加 `topic`/`scope`；`MemoryListCursor` 扁平；`MemoryType` 移出核心；`MemoryStoreError` 留核心 |
| 5 | 旧类型硬移除 | `Context`（收内部）、`ContextAssembler*`、`MemorySummarizer`、`MemoryDistiller`、`ConversationTopicDetector`、`TopicDecision`、`AssemblyFailure`、`MemoryFlowError` 硬移除；分层类型与存储契约不在核心公开面 |
| 6 | rewriter history | 核心从**真相域**取最近成功轮（可过滤失败/取消）作为 `RewriteInput.history`（ADR-0025 §7） |
| 7 | 主题检测机制 | 核心轮末调用检测器 → 写回 `Conversation.topic` → **变化时**发 `TopicShifted` → 变更经上下文传给后续钩子；失败保留旧 topic + `Failed` 事实（ADR-0027） |
| 8 | 验证 | 公开面编译期核对（旧类型不可见）；事件序列化形状；scope 过滤/批量；检测器注册/模型解析/变更传递；全量回归（测试面暂由旧实现承载至 M40） |

### M40 内置引擎接线与核心默认 provider

| # | 子项 | 要点 |
|---|---|---|
| 1 | Context 内部化 | `Context` 收为 crate 内部内置引擎：读相位（rewriter → assembler）+ 写相位（流水线模板 + 三钩子 + 轮末主题检测）；入口 `pub(crate)` |
| 2 | 装配点 | `RuntimeBuilder::memory_provider(...)`（缺省 = 核心进程内默认）；`AgentBuilder::rewriter_provider(...)`（可选）；`AgentBuilder::topic_detector_provider(...)`（可选；缺省 = 无检测，保持 topic；ADR-0027）；`runtime.memory() -> Arc<dyn Memory>`；`ConversationTaskSpawner` 收为内部 |
| 3 | 核心默认 provider | `inprocess.rs` 一套**进程内、非分层**默认实现：assembler 产出**完整帧**（原始 input 作为 user 消息；忽略 scope）；管理面最小（空/最小条目）；pipeline 归档钩子 |
| 4 | 组帧与归档 | 核心前置 `system(Agent)` + assembler 帧；`Turn.messages` = 完整帧 + 本轮后续消息；`Turn.input` 仍为原始文本；**空帧兜底**（原始 input 组帧 + 失败事实） |
| 5 | 角色约定 | system 只放 Agent 指令；记忆上下文与增强输入进 user 侧（默认 assembler 同步改） |
| 6 | 模型解析 | `Provider 模型 ?? Agent 模型` 使用时解析；叙述包装（无 `StreamDelta`）；rewriter 与主题检测器同规则 |
| 7 | 验证 | 未配置 provider 时多轮运行（默认实现承载上下文）；替换 provider 后行为随之；空帧兜底不静默；检测器缺省保持 topic；全量回归绿 |

### M41 组件骨架：crate + 分层模型 + 四端口

| # | 子项 | 要点 |
|---|---|---|
| 1 | crate | 新增 `crates/synonz-layered-memory`（依赖核心；核心不反向依赖）；workspace 成员与发布顺序更新（六 crate） |
| 2 | L1 模型 | 双轨原始消息（user / agent 分存）；最近 10 轮 FIFO；会话级、不随主题切换清空 |
| 3 | L2 模型 | 事件单元：`event_id` / `event`（≤30 字）/ `event_vector` / `last_update_time` / `content`（历史数组 ≤5、时间倒序）；每会话 ≤5 事件；会话隔离 |
| 4 | L3 模型 | 实体（`canonical_name` / `aliases` / `entity_type`）+ 三元组；类型 Schema（PERSON/PRODUCT/BRAND/LOCATION/PREFERENCE/TOPIC/EVENT/OTHER）与关系 Schema（LIKES/DISLIKES/OWNS/WANTS/BELONGS_TO/RELATED_TO）；**跨会话**、scope 隔离 |
| 5 | 四端口 | L1 消息 / L2 事件 / L3 图 / L3 向量（按数据对象）；端口内以 scope 键/过滤分区；各端口进程内默认实现 |
| 6 | 验证 | 模型类型往返；端口 CRUD；scope 分区；L2 会话隔离；L3 跨会话；L1 容量淘汰 |

### M42 组件管线（四阶段）

| # | 子项 | 要点 |
|---|---|---|
| 1 | 阶段1 指代消解 | 组件经 `RewriterProvider` 提供实现；材料 history 由核心给（真相域）；消解结果用于召回 |
| 2 | 阶段1 主题检测 | 组件提供检测器实现（经 `TopicDetectorProvider`，ADR-0027；算法：embedding 相似度 / 轮次 / 信息熵，embedding 用组件端口）；核心写回 topic、发事实、把变更传给后续钩子 |
| 3 | 阶段2 召回评分 | assembler 内：三层召回（L1 双轨 / L2 事件 / L3 锚点先行 + 1–2 跳）→ 统一评分（语义 0.5 / 时间 0.3 / 重要 0.2 × 双轨加权 + source boost）→ 去重（content_hash + 向量）→ Top-N → 冷启动门槛（L1+L2 为空剔除 L3） |
| 4 | 阶段3 改写成帧 | assembler 内（叙述 LLM）：上下文 + 原始输入 → 增强输入；产物 = 完整消息帧（system 之后） |
| 5 | 阶段4 写入 | `archive_turn`（L1 双轨）；`spawn_task`（L2 事件判断更新；**本轮 topic 变更**时结构化摘要（summary + key_facts + importance + topic_tag）→ 持久化；实体提取 → 归一化（alias 合并 + Schema）→ 图谱 + 向量）；`finalize_conversation`（会话结束批量同步） |
| 6 | 纪律 | 写操作全量后置；读 / 消费分离；不丢数据（失败保留来源 + `report_failure`）；无内容路由（数据自带 scope，按 scope 分组） |
| 7 | 验证 | 四阶段端到端；L2 更新/创建与淘汰；图谱写入与归一化；主题切换触发摘要与图谱；冷启动门槛 |

### M43 组件 provider + 富面 + 观察面 + embedding + 策略

| # | 子项 | 要点 |
|---|---|---|
| 1 | 组件实现 | `LayeredMemory` / `LayeredMemoryContextAssembler` / `LayeredMemoryPipeline` / `LayeredMemoryProvider`（工厂三行为 + 模型配置）；组件检测器（`TopicDetector` 实现，经 `TopicDetectorProvider` 注册，ADR-0027） |
| 2 | 富管理面 | L2 事件 → `MemoryType::Summary`、L3 实体 → `MemoryType::Knowledge`；富面文档类型（`EventSummary` / `Entity` / `Triple`）；按 `(subject, scope)` 列表 / 批量遗忘；组件句柄取用 |
| 3 | embedding 端口 | 组件侧端口（进程内默认 + 真实后端适配）；调用归组件观察面 |
| 4 | 语义策略 + 参数 | 事件判断 / 摘要 / 实体提取 / 上下文改写策略化（提示词随策略）；参数集中配置（容量 / Top-N / 阈值 / 权重 / `HOP_DECAY`；检测器参数随检测器实现） |
| 5 | 观察面 | 组件 trait（provider 注入；可与 runtime observer 同对象双注册）；进展事件（事件更新 / 摘要 / 图谱 / 归一化 / 跳过 + scope + count） |
| 6 | 真实后端适配 | Redis / MongoDB / Neo4j / Milvus（可独立成包；ADR-0023 决策 4） |
| 7 | 验证 | 富面映射与过滤；观察面事件；替换端口/策略；真实后端适配（可用时） |

### M44 收尾与验证

| # | 子项 | 要点 |
|---|---|---|
| 1 | 测试全量 | 全量回归（fmt / clippy / doc / test --all-features）；新引入 flaky 即缺陷 |
| 2 | **示例验证**（契约变更必做） | `cargo build -p synonz-examples --bins` 全部通过；**离线示例实跑**（`custom_tool` / `events` / `cancellation` / `scheduler` / `mcp_tools`）exit 0；**网络示例**（`openai_chat` / `anthropic_chat`）无 key 时提示退出（exit 0）；`chat_tui` 编译通过 + 人工交互实跑；**触及变更面**（核心契约 / 事件词汇 / 装配点）的示例同步改写并记录；新增"分层组件用法"示例（如适用） |
| 3 | 文档 | **API 适配说明**（`docs/design/migration-0.7.0.zh-CN.md`：核心公开面迁移、事件与失败迁移、rewriter history、scope 管理面、组件为新增 crate）；**扩展指南更新**（契约族用法、组件策略与端口、embedding、观察面）；CHANGELOG 0.7.0（破坏项 + 适配表 + 边界）；README 七份同步（v8 + 计划指针）；v8 落点核对与实施记录 |
| 4 | 命名终稿 | 按 coding.md §5 核对：`MemoryProvider` / `RewriterProvider` / `MemoryPipeline` / `MemoryContextAssembler` / `MemoryScope` / 组件类型族（`EventSummary` / `Entity` / `Triple` / 端口与策略） |
| 5 | 发布决策 | bump 0.6.0→0.7.0、七份 crate README 同步、dry-run、依序 publish（`synonz-derive` → `synonz` → `synonz-layered-memory` → 适配层）、tag `v0.7.0`、GitHub Release——**等 irylex 确认后执行** |

---

## 3. 迁移映射表（旧 → 新）

| 旧（0.6.0 代码） | 新（0.7.0） |
|---|---|
| `Context`（公开具体类型）与 `AgentBuilder::context(...)` | 引擎收为 crate 内部；`AgentBuilder::rewriter_provider(...)`（可选） |
| `ContextAssembler` / `ContextAssemblerInput` / `ContextAssemblerOutput` | `MemoryContextAssembler` / `MemoryContextAssembleInput`（+ 叙述 LLM）/ `MemoryContextAssembleOutput`（messages = 完整消息帧） |
| 模型可见输入 = 原始 input（核心追加） | assembler 产出 `system(Agent)` 之后的完整消息帧；空帧兜底（原始 input + 失败事实） |
| `MemorySummarizer` / `MemoryDistiller` | 组件语义策略（事件判断 / 摘要 / 实体提取 / 上下文改写） |
| `ConversationTopicDetector` / `TopicDecision`（核心策略槽） | 移除；新 `TopicDetectorProvider` / `TopicDetector`（Agent 级独立扩展点，可选模型同 rewriter；ADR-0027） |
| `MemoryPipeline` 四钩子（`detect_topic`/`archive_turn`/`spawn_task`/`finalize_conversation`） | 三钩子（`archive_turn`/`spawn_task`/`finalize_conversation`）；主题检测移出；核心写回/事实/变更传递 |
| `MemoryFlowError` / `AssemblyFailure` | `MemoryFailure { stage: String, detail: String }` |
| `MemoryEvent::FlowFailed { stage: MemoryFlowStage, moment }` | `MemoryEvent::Failed { stage: String, detail: String, moment }` |
| `MemoryEvent::Compacted` / `Distilled` / `Promoted` | 移出核心 → 组件自有观察面（进展事实） |
| `MemoryEvent::Updated` / `Removed`（无 scope） | 增 `scope`（删除事实事后可定位分区） |
| `MemoryItem`（无 scope） | 增 `scope`；保持最小集（id/content/source/scope/时间戳） |
| `MemoryQuery`（memory_type 过滤） | 去 `memory_type`；增 `topic` / `scope`（None = 全部合并） |
| `MemoryListCursor`（类型相位） | 扁平游标（`position`） |
| `Memory`（具体 facade） | `Memory`（trait，provider 产出）；`runtime.memory() -> Arc<dyn Memory>` |
| `RuntimeBuilder::memory_l1/l2/l3_store(...)` | `RuntimeBuilder::memory_provider(...)`（缺省 = 核心进程内默认 provider） |
| 分层类型 / 存储契约（`L1Entry` 等） | 组件公开面（L1 双轨消息 / L2 事件 / L3 实体与三元组；四端口） |
| 组件旧模型（ADR-0024：时间片摘要段 / 知识单元键集 / `unit_key`） | 按《上下文记忆系统全链路技术方案》重定（ADR-0026）：L2 事件摘要 / L3 图谱+向量 |
| `rewriter` history 取自核心 L1 窗口 | 取真相域最近成功轮（ADR-0025 §7） |
| 0.6.0 序列化数据 | **不支持**（无数据迁移；仅 API 适配说明） |

---

## 4. 验收总标准

1. **六里程碑逐项完成**，每波全量回归绿（fmt / clippy / doc / test）；
2. **核心公开面**：旧类型硬移除（`Context` / `ContextAssembler*` /
   策略槽 / `AssemblyFailure` / `MemoryFlowError` / `MemoryFlowStage` /
   分层类型与读方法）；`MemoryReader` 为条目级只读投影；
   `memory_type` 与组件语义字段不在核心视图（编译期）；
3. **核心契约族**：`Memory` / `MemoryContextAssembler` /
   `MemoryPipeline`（模板 + **三钩子**）/ `MemoryProvider` /
   `RewriterProvider` / **`TopicDetectorProvider`** 落地；assembler 与
   pipeline 上下文带**叙述 LLM**（含本轮 topic 变更）；生命周期行为
   async、管理动词与工厂 sync；
4. **消息帧与归档**：assembler 产出完整帧（含本轮用户消息）；核心
   不追加原始 input；`Turn.messages` = 帧 + 后续、`Turn.input` 原始；
   **空帧兜底**（原始 input + 失败事实，不静默）；角色约定（system
   只放 Agent 指令）；
5. **默认能力**：未配置 provider 时使用核心进程内默认实现，agent
   可多轮运行；替换 provider 后记忆行为随之替换；
6. **作用域**：`MemoryScope` 不透明；`MemoryItem.scope` 暴露、
   `MemoryQuery.scope` 过滤（None = 全部）、`forget_matching` 按
   scope 批量；管理事实携带 scope；
7. **主题检测**：Agent 级注册与替换；模型解析（Provider → Agent
   回退、核心叙述）；写回与 `TopicShifted`（变化才发）；变更经上下文
   到达后续钩子（切换触发摘要/图谱）；失败保留旧 topic + `Failed`；
8. **组件分层模型**：L1 双轨消息（10 轮、会话级）；L2 事件（LLM
   判断更新/创建、≤5 事件、content ≤5、仅 user_track）；L3 实体
   图谱 + 向量（跨会话、Schema 约束、alias 合并、锚点先行多跳、
   冷启动门槛）；
9. **四阶段管线**：指代消解 → 召回评分去重冷启动 → 改写成帧 →
   保存与异步写（L2 事件 / 结构化摘要 / 图谱）；读 / 消费分离、
   不丢数据、写操作全量后置；
10. **组件公开面**：四端口（L1 消息 / L2 事件 / L3 图 / L3 向量）+
    进程内默认 + 真实后端适配；embedding 端口；语义策略 + 参数集中
    配置；观察面 trait（进展事件）；富面（L2→Summary、L3→Knowledge；
    `EventSummary` / `Entity` / `Triple`）；
11. **文档一致**：API 适配说明与扩展指南落地；CHANGELOG 0.7.0 完整；
    v8 落点核对通过；README 七份同步；
12. **示例验证（契约变更必做）**：构建全部通过；五个离线示例实跑
    exit 0；两个网络示例无 key 时提示退出；`chat_tui` 编译通过并
    人工交互实跑；触及变更面的示例已同步改写并记录；
13. **延期项记录在案**：provider 装配命名细节、组件包装、观察事件
    清单与命名、L2 缓存/持久化拆分、scope 转换、scope 获取（应用
    层）、L1 留存/清理、语义搜索/矛盾消解。

---

## 5. 风险与开放项

**已决项**（预先定案——不留到实施期逐段处理）：

| 项 | 决议 |
|---|---|
| `RewriterProvider` 装配名 | `AgentBuilder::rewriter_provider(...)` |
| `MemoryProvider` 装配名 | `RuntimeBuilder::memory_provider(...)`（工作名） |
| 默认能力 | 核心 `inprocess.rs` 进程内、非分层默认 provider；未配置即用；`runtime.memory()` 恒可用 |
| 读相位模型 | assembler 材料带叙述 LLM；`Provider 模型 ?? Agent 模型` 使用时解析并包叙述 |
| 消息帧 | assembler 产出 `system(Agent)` 之后的完整帧；核心不追加原始 input；`Turn.messages` 记完整帧、`Turn.input` 原始 |
| 帧降级 | 契约义务（必返含用户消息的帧）+ 空帧兜底（原始 input + 失败事实） |
| 角色约定 | system 只放 Agent 指令；记忆上下文与增强输入进 user 侧 |
| rewriter history | 真相域最近成功轮（不引入组件读通道） |
| 主题检测 | Agent 级独立扩展点 `TopicDetectorProvider`（可选模型同 rewriter；ADR-0027）；核心写回/事实/变更传递；`MemoryPipeline` 三钩子 |
| scope | 不透明字符串；条目暴露、查询可选过滤（None=全部）、批量按 scope；事实携带 scope；核心无项目、不定义注入/推导、不做转换 |
| 组件模型 | 按《上下文记忆系统全链路技术方案》：L1 双轨 / L2 事件 / L3 图谱+向量；四阶段管线；四端口；语义策略 + 参数；embedding 组件侧；观察面 trait；无内容路由；无回合序号 |
| 管理映射 | L2 事件 → `Summary`；L3 实体 → `Knowledge`；富面文档类型 |
| 数据迁移 | 无（0.6.0 序列化数据不支持；只有 API 适配说明） |

**实施期决策协议**（对涌现事项）：

- **A 类（用户门控）**：影响公开 API 契约、可观察行为语义或不可逆
  承诺的事项——带选项与推荐向 irylex 请示，确认后实施；
- **B 类（代理可决，按既定规则）**：内部结构、命名（coding.md §5
  命名立法）、有界影响的具体值与测试策略——由实施代理按既定规则
  定案：开工时明确、完工时记录；
- **记录义务**：A 类进 ADR/v8（涉及语义时）；B 类进本计划的
  "里程碑完成记录"与提交信息——决议不得只存在于聊天或代码中。

**执行期约定**：

| 项 | 约定 |
|---|---|
| 发布节奏 | M44 发布执行等 irylex 单独确认 |
| 版本号 | 工作标签 0.7.0（MINOR：组件化 + 破坏项同波）；发布准备时按纳入范围定 |
| 单波策略 | 破坏项集中本波发布，不拆分（与 ADR-0022/0023/0025/0026 一致） |
| 组件依赖方向 | 组件 → 核心；核心不反向依赖；适配层不受影响 |

---

## 6. 里程碑完成记录

（实施期填写：每个里程碑完成后记录子项落地、B 类决议与 A 类请示
结果、验证数字与提交信息。）
