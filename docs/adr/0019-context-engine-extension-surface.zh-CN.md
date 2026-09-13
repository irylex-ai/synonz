# ADR-0019: 上下文引擎扩展面——具体引擎、两相位策略与模型叙述归属

- 状态: DRAFT（待 irylex 评审后转 APPROVED）
- 日期: 2026-09-13
- 决策者: irylex（人类逐点确认；本 ADR 每个决策均经弹窗逐项确认，
  遵循 `.opencode/rules/architecture.md` §5–§9 的渐进决策流程）
- 性质: 引擎扩展模型转型 + 读侧契约补齐 + 命名/API 修补；破坏性
  变化随 0.5.0 单波发布
- 关联: 细化 ADR-0012（记忆与上下文系统——分层记忆与装配源）、
  ADR-0017（事件总线与反应架构——"装配不读真相域"、策展归引擎层、
  策略槽结构）、ADR-0009（插件化扩展原则——扩展点须有真实场景）、
  ADR-0015（模型行为与参数归位）
- 版本承载: 0.5.0（破坏性单波）
- 草稿沿革: 本文取代本 ADR 此前两版草稿（`0efd19d` 草案、
  `49fd4c0` 槽可观测性修订）。ADR 尚处 `DRAFT`、未获批准，故按
  `.opencode/rules/documentation.md` §9（append-only 约束已批准 ADR）
  在草稿阶段重写；slug 由 `read-side-extension-surface` 改为
  `context-engine-extension-surface`。

## Context（背景）

0.4.0 发布后，用一份经过实战的上下文记忆系统技术方案（三层记忆 +
双轨存储 + 事件摘要 + 图谱归一化；四阶段流程"预处理 → 召回评分 →
改写 → 异步写"）对本框架做扩展性验证。结论是：这类产品化系统应作为
**外部实现**（自持客户端与存储），不内建进默认引擎。但验证过程暴露了
真正的架构问题，触发本决策：

1. **引擎契约是开放 trait，框架担保无法强制**。`trait Context`
   （`assemble` + `on_turn_completed`）是整台引擎的唯一扩展契约，
   `Agent` 持 `Arc<dyn Context>`。这带来两个后果：一是存在"实现整台
   引擎（trait）"与"替换策略槽"两条扩展路径，认知负担大；二是
   `dyn` 类型擦除使框架读不到引擎自身的模型配置（`with_model` 的
   解析与叙述无处落地）。
2. **读侧缺一等的输入预处理**。来源方案的链路是
   `指代消解(前) → 召回/评分(用消解后的 query) → 上下文改写(后) →
   LLM`：指代消解是一次**独立、发生在召回之前**的 LLM 步骤，用于
   提升召回 query 质量（取 L1 最近 3 轮历史）。0.4.0 的 `assemble`
   只能追加 background 消息，**不能改写本轮输入**——引擎内无法闭环。
3. **策略槽拿到过宽能力**。`ContextAssemblerInput.memory: &Memory`
   是完整 facade，装配器可写 L1/L2/L3、可绕过引擎策展，与"错误的能力
   不可表达"的 ISP 纪律矛盾；写侧维护槽的能力面同样不成体系
   （L2→L3 蒸馏需要读 L1/L2/既有 L3，但既无读取面也无策略槽）。
4. **记忆条目命名不宣告层号**，与 `MemoryL1/L2/L3Store` 词汇脱节；
   外部 L1 store 无法构造 `L1Entry`（无公开构造器）。

决策过程按架构规范渐进展开：先定引擎扩展模型，再定写侧去留，再定
读侧形态与顺序，最后落命名与 API 修补（详见各 Decision 小节）。

## Problem（问题）

1. 引擎应以"开放契约（实现整台引擎）"还是"具体引擎 + 阶段策略"
   作为扩展模型？框架能否对叙述、失败、drain、模型解析提供不可绕开的
   担保？
2. 写侧是否暴露整段相位扩展？若暴露，为使其工作必须交出完整写权限
   （`Memory` 写 + 后台派发 + 事件出口），开发者即可破坏三层记忆
   不变量——与框架担保冲突。
3. 读侧如何表达"召回前的输入预处理"？改写与组装谁先谁后？
   `rewritten_input` 是输入物料还是组装产物？
4. 策略槽的能力边界如何立法（可读不可写；facade 不进槽）？
5. 模型调用（尤其是引擎发起的辅助调用）的可观测性由谁保证？

## Decision（决策）

### 1. 引擎形态：具体 `Context`，退役 `trait Context`

- **删除公开 `trait Context`**。原 `DefaultContext` **改名为具体类型
  `Context`**（保留 `new()` / `Default`）。
- `Agent` 由 `Arc<dyn Context>` 改为持有 **`Arc<Context>`**（无类型
  擦除）；`AgentBuilder::context(Context)` 按值接收、内部转 `Arc`。
- `Context::assemble` 与 `Context::on_turn_completed` 收为
  **`pub(crate)` 内部入口**（唯一调用者是 agent 循环）——用户不直接
  驱动引擎，公开面即配置面 + 策略槽。

```rust
pub struct Context {
    // 读相位
    assembler: Arc<dyn ContextAssembler>,
    rewriter:  Option<Arc<dyn TurnInputRewriter>>,
    // 写相位（无相位扩展；以下为内置维护的子钩子）
    topic_detector: Arc<dyn ConversationTopicDetector>,
    summarizer:     Arc<dyn MemorySummarizer>,
    distiller:      Arc<dyn MemoryDistiller>,
    // 配置
    model: Option<Arc<dyn Model>>,   // with_model
    l1_window: usize,
    l2_cap:    usize,
}
```

- **收益**：框架可对引擎内部机制（模型解析与叙述、失败上报、后台
  drain、`TurnContext` 构建）提供不可绕开的担保；扩展模型收敛为
  "具体基座 + 窄策略"；命名与文档面更直。
- **代价**：不再支持"从零替换整台引擎"，最大可换单位 = 相位策略
  （读侧整段、写侧子钩子）——见 §2 与 Consequences 的边界记录。

### 2. 两相位扩展面：读可扩展、写框架独占

引擎的两个相位采用**不对称**的扩展模型，依据是"读不可腐化状态、写可"：

**读相位（可整段扩展）**：
- `ContextAssembler` 整段可替换——它决定模型看到哪些背景（召回/组合/
  格式/预算），入参为只读门面（见 §4）。读侧不触碰真相域、不写状态，
  无法破坏数据不变量，故保留整段扩展。

**写相位（无相位扩展，框架独占）**：
- **不提供整段写侧扩展**。`on_turn_completed` 的编排（主题推进 →
  L1 归档 → 后台压实/蒸馏）、记忆写入与流程事实发射**全部由框架
  完成**。写侧定制仅限三个窄子钩子：
  `ConversationTopicDetector` / `MemorySummarizer` / `MemoryDistiller`，
  以及三层存储槽（`MemoryL1/L2/L3Store`）。
- 依据：写侧要工作就必须拥有写权限；把该权限交给任意用户代码即等于
  放弃三层记忆不变量与框架担保。以类型而非文档约束"不可越界"。

### 3. 模型解析与叙述归属

- **引擎模型**：`Context::with_model(Arc<dyn Model>)`（替换
  `with_summary_model`）；缺省 = agent 模型。解析规则
  **`with_model ?? agent_model`** 在引擎内部完成；**记忆跟随上下文**
  （不设独立记忆模型——双旋钮无自然优先级、无真实场景）。
- **移除 `with_summary_prompt`**：提示词是**策略内容**不是引擎配置；
  改提示词 = 替换 `MemorySummarizer`。内置 `PromptMemorySummarizer`
  随之失去 `prompt`/`model` 字段（无字段的私有默认实现即可），
  "prompt/model 互相覆盖"的构造顺序缺陷一并消除。
- **叙述保证**：
  - 推理循环**保持手动叙述不变**（`emit!` 依赖 `EventSink::emit_turn`
    的消费端掉线终止，且 `Requested` 必须确定）——**不统一包装循环**。
  - 引擎交给用户代码的**辅助模型句柄**统一包成 crate 内部
    `NarratedModel`：调用前发 `ModelEvent::Requested`、`Finish` 时发
    `Responded`（`CallPurpose::ContextManagement`、`round: None`），
    **永不发 `StreamDelta`**（辅助调用取最终结果，无需流式），因此
    **不需要 `emit_deltas` 开关**。
  - **内容变换槽不持 `EventSink`**：发不发事件不是槽实现的决定。
    流程事实（`TurnArchived`/`TopicShifted`/`Compacted`/`Distilled`/
    `FlowFailed`）由引擎发。自持模型客户端的实现，其调用可观测性
    由实现自担——明确为边界，不假称框架保证。

### 4. 读侧契约

**顺序：先改写、后组装**。指代消解是预处理，其模型视图参与后续组装/
召回（对应来源方案 stage 1 的"影响 query 质量"）。

```rust
pub trait TurnInputRewriter: Send + Sync + 'static {
    /// 产出本轮模型视图输入；None = 保持原文。
    /// Err = 可见降级（保留原文并上报），绝不静默。
    fn rewrite<'a>(
        &'a self,
        input: &'a str,          // 用户原文
        history: &'a [Message],  // 引擎提供的近期 L1 历史（来源方案取最近 3 轮）
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}
```

- **`rewritten_input` 是输入物料，不是组装产物**：只加在
  `ContextAssemblerInput` 上，`ContextAssemblerOutput` 维持 0.4.0
  形状 `{ messages, failures }`。Input/Output 成对，产物不二次传递。
- `Context::assemble` **签名不变**（仍收 `ContextAssemblerInput`、
  返 `ContextAssemblerOutput`）；引擎内部：用 `input.reader` 取近期
  历史 → `rewriter.rewrite(input.input, history)` → 用
  `rewritten_input` 重建 input → `assembler.assemble(input)`。
  该字段由引擎填充（agent 构造时为 `None`）。
- 无 rewriter 时 `rewritten_input = None`，行为与 0.4.0 一致
  （纯加性）。
- 模型可见的**当前 user 消息仍由 agent 追加原始输入**；`rewritten_input`
  仅供组装内部（召回/组合）使用——与来源方案一致（消解只影响召回；
  模型靠背景历史自行消解）。
- **不引入"召回后 LLM 上下文改写"作为框架特性**：来源方案需要它是
  因为其对外契约是单字符串（`process_v2 → rewritten_input`）；本框架
  以结构化背景 `messages` 交付召回，无需把上下文融进单一输入。若个别
  场景确需，可由自定义 `ContextAssembler` 自行实现（自持客户端）。

**只读记忆门面 `MemoryReader`**：

```rust
pub struct MemoryReader<'a> { /* &dyn MemoryL1Store + L2 + L3 + &Subject */ }

impl<'a> MemoryReader<'a> {
    pub fn l1_window(&self, conversation_id: &str) -> Result<Vec<L1Entry>, MemoryStoreError>;
    pub fn l1_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError>;
    pub fn l2_read(&self, conversation_id: &str) -> Result<Vec<L2Entry>, MemoryStoreError>;
    pub fn l2_len(&self, conversation_id: &str) -> Result<usize, MemoryStoreError>;
    pub fn l3_query(&self, query: &str, topic: &Topic, budget: usize)
        -> Result<Vec<L3Entry>, MemoryStoreError>;
    pub fn l3_len(&self) -> Result<usize, MemoryStoreError>;
}

impl Memory {
    pub fn reader<'a>(&'a self, subject: &'a Subject) -> MemoryReader<'a>;
}
```

- **取代 `ContextAssemblerInput.memory: &Memory`**：
  `ContextAssemblerInput.reader: MemoryReader<'a>`（破坏项）。写与策展
  在类型上不存在（无 append/pop/upsert）——编译期拒绝，无需运行期
  错误；写入路径永远由引擎收口。

### 5. 写侧契约（子钩子）

```rust
pub trait MemorySummarizer: Send + Sync + 'static {
    /// L1→L2 内容变换；模型调用由框架叙述（句柄为 NarratedModel）。
    fn summarize<'a>(&'a self, entries: &'a [L1Entry], model: &'a dyn Model)
        -> BoxFuture<'a, Result<String, String>>;
}

pub trait MemoryDistiller: Send + Sync + 'static {
    /// 生成 L3 知识内容；可用 reader 读 L1/L2/既有 L3（锚定与归一化）。
    /// 只返回知识文本；L3Entry 的身份与时间由引擎补齐。
    fn distill<'a>(
        &'a self,
        blocks: &'a [L2Entry],        // 本批待加工的 L2 溢出块
        conversation_id: &'a str,     // 供 reader 查 L1/L2
        topic: &'a str,
        reader: MemoryReader<'a>,
        model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;
}
```

- `MemorySummarizer::summarize` **移除 `events` 参数**（破坏项）；
  内置实现的手工 Requested/Responded 发射删除，改由框架统一叙述。
- `MemoryDistiller` 默认实现 = 现有机械提升；失败语义改良为
  **先变换、成功后再 pop**（失败保留 L2 + `FlowFailed { stage: Distill }`，
  不丢数据）。
- 槽能力纪律：**入参 = 完成职责所需的最小物料 + 最小能力；facade
  不进槽；写永远由引擎收口**。大槽（原料域开放增长）用输入对象
  （`ContextAssemblerInput`），小槽用松散参数。

### 6. 命名统一与 API 修补

| 现名 | 新名 |
|---|---|
| `L1Entry` | 不变 |
| `SummaryBlock` | `L2Entry` |
| `KnowledgeFragment` | `L3Entry` |
| `FragmentIdentity` | `L3Identity` |

（核实：`FragmentIdentity` 仅被 L3 路径使用——定义、`L3Entry` 构造、
蒸馏、测试种子；无其他引用。）

- `L1Entry::new(...)`（**硬缺口**：`#[non_exhaustive]` 且无公开构造器，
  外部 L1 store 无法返回条目）+ `created_at`（时间衰减）；
- `L2Entry.created_at`（同上，`new` 自动置时间）；
- `Conversation::topic()` / `set_topic()` 公开（T2：活跃主题=框架标签；
  多主题集合/切换轨迹归引擎态，见扩展指南）。

## Alternatives Considered（备选与否决）

1. **保留 `trait Context`（整引擎替换）**——否决：框架无法在开放契约
   上强制叙述/失败/drain/模型解析担保，且 `dyn` 擦除使引擎模型配置
   不可读；两条扩展路径并存。代价是失去整段编排自由（已记为边界）。
2. **混合：具体引擎 + 保留自定义整引擎注入口**——否决：长期维护两套
   路径、扩展模型不收敛，是方案 1 的复杂化。
3. **保留整段写侧扩展 `TurnCompletionHandler`**——否决：为使其可用需
   交出完整写权限（`Memory` 写 + 派发 + 事件出口），可破坏三层记忆
   不变量；"不可越界"应以类型约束而非文档。
4. **只暴露窄写侧钩子（只读视野 + 结构化贡献）**——本轮否决：需定义
   公开"贡献"词汇，属为文档型场景预支的抽象，当前无可确认需求；
   记为未来触发项。
5. **统一包装推理循环以自动叙述所有模型调用**——否决：破坏循环的
   确定 `Requested` 与消费端掉线终止，流式/取消语义风险大。
6. **内容槽自持 `EventSink` 手发事件**——否决：可实现漏发/伪造，
   观测不一致，与能力纪律冲突。
7. **把 `rewritten_input` 放在 `ContextAssemblerOutput` 上**——否决
   （irylex 评审意见）：它是组装阶段的**输入物料**，Input/Output 成对、
   产物不二次传递；放输入对象即可，输出维持 `{ messages, failures }`。
8. **模型视图同样作为 agent 追加的当前 user 消息**——否决：输出已不
   携带该值；且来源方案用原始输入，模型凭背景历史自行消解。若将来看
   到必须替换模型可见消息的需求，再以加性字段引入。
9. **召回后再做一次 LLM"上下文改写"（来源方案阶段 3）**——本轮否决：
   单字符串契约的产物；本框架以结构化 `messages` 交付召回，无需融合；
   真需要可由自定义 `ContextAssembler` 自持客户端实现。
10. **在契约上加引擎模型声明渠道（`ContextAssemblerOutput.model` /
    `AgentBuilder::context_model`）**——否决：引擎已是具体类型，框架
    可直接读其 `with_model`；无需新公开面。
11. **读侧也收归框架、仅露窄钩子 / 删除 `ContextAssembler`**——否决：
    读侧只读、不触碰真相域，无法破坏不变量；召回/组合/格式是下游最
    常改的定制点，砍掉会严重削弱扩展性。
12. **只读能力用 trait（`MemoryRead`）而非值类型门面**——否决：值类型
    门面（`MemoryReader`）更贴门面模式，避免与 facade 方法面重复。
13. **`with_prompt(PromptConfig)` 提示词配置**——否决：会冻结不稳定的
    操作目录（摘要之外还有主题/改写/蒸馏）；提示词属策略内容。
14. **蒸馏槽使用输入对象（`DistillInput`）**——否决：小槽保持松散参数，
    与 `MemorySummarizer` 风格一致；仅原料域开放的 `ContextAssembler`
    使用输入对象。
15. **`TurnCompletedInput` 输入对象**——否决（irylex 评审意见）：
    无意义的新对象且入参不清晰；`on_turn_completed` 用显式参数。
16. **多主题会话状态（T1 注册表 / T3 轨迹）**——本轮否决：结论为 T2
    （活跃主题=框架标签；多主题集合与切换轨迹归引擎态），无真实触发
    条件；记录于扩展指南，不单独立 ADR。

## Consequences（后果）

**破坏项（0.5.0 单波）**：

- 删除公开 `trait Context`；`DefaultContext` 改名 `Context`；
  `Agent` 持 `Arc<Context>`；`AgentBuilder::context(Context)`；
- `Context::assemble` / `Context::on_turn_completed` 收为 `pub(crate)`；
  `TurnContext` 不再是公开类型（写侧无相位消费者，内部化/移除）；
- `on_turn_completed` 由 `&TurnContext` 改为显式 7 参
  （`conversation`/`input`/`messages`/`memory`/`agent_model`/`events`/
  `task_spawner`）；
- `ContextAssemblerInput.memory: &Memory` → `reader: MemoryReader<'a>`；
- `MemorySummarizer::summarize` 移除 `events` 参数；
- `with_summary_prompt` 移除；`with_summary_model` 改名 `with_model`；
- `SummaryBlock` → `L2Entry`、`KnowledgeFragment` → `L3Entry`、
  `FragmentIdentity` → `L3Identity`。

**加性项**：

- `ContextAssemblerInput.rewritten_input`；`TurnInputRewriter` 槽；
  `MemoryReader` 只读门面；`MemoryDistiller` 槽；`L1Entry::new` 与
  `created_at`；`L2Entry.created_at`；公开 `Conversation::topic()` /
  `set_topic()`。

**边界（不入本轮）**：

- 整段写侧扩展：写权限与三层记忆不变量不可兼得；当前无已确认需求。
  若将来出现无法用子钩子/存储满足的真实需求，再以"窄钩子（只读视野 +
  结构化贡献）"增量引入。
- 会话终结 flush 保证：应用层观察 `ConversationEvent::Ended` + 幂等
  补偿归档（引擎终结钩子需 conversation→engine 接线）。
- 远程存储不入默认 Memory 管线：Memory 插槽面向本地/进程内介质；
  Redis/Neo4j/Milvus 类系统走自定义策略实现或外部编排。
- 自持模型客户端（含自带 LLM 的 rewriter/assembler）的调用可观测性
  由实现自担。
- 叙述 `emit_deltas` 开关：当前辅助调用恒无 `StreamDelta`；若将来出现
  "需要流式叙述的包装句柄"（无当前需求），再引入开关。

**文档义务**：v6 架构文档（0.5.0 目标形态，v5 转 SUPERSEDED）、0.5.0
实施计划、扩展指南（扩展点映射 + 边界 + 模型角色 + T2 主题）、
ADR-0017 修订注记（配置面变化）、CHANGELOG（Added/Breaking/Migration）、
README 指针更新。

**验证要求**：全量回归 + 新测试——

- 改写进入组装输入（`rewritten_input`）而真相（canonical/`Turn`/L1）
  保留原文；无 rewriter 时行为与 0.4.0 一致（纯加性）；
- `MemoryReader` 只读面可用，装配器在类型上不可写；
- 蒸馏默认/自定义路径，失败不丢 L2（先变换后 pop）；
- 命名迁移后编译面完整；
- 槽可观测性由框架保证：自定义槽只调用传入的 `model`、自身不发事件，
  观察者仍能收到 `ModelEvent::Requested`/`Responded`（框架叙述），且
  流程事实（`Compacted`/`Distilled`）不依赖槽实现；
- 模型解析 `with_model ?? agent_model` 生效，且 `with_model` 句柄的
  辅助调用被自动叙述。
