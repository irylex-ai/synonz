# ADR-0019: 读侧扩展面——输入改写、只读记忆门面与槽的最小能力

- 状态: 提议（DRAFT，待 irylex 评审后转 APPROVED）
- 日期: 2026-09-13
- 决策者: irylex（人类逐点确认——结论均经弹窗逐项确认）
- 性质: 上下文扩展面的契约补齐——读侧输入改写（模型视图 vs 真相）、
  `InputRewriter` 策略槽、`MemoryReader` 只读门面（取代策略槽的全
  facade 访问）、槽最小能力纪律、L2→L3 蒸馏槽；破坏性变化随 0.5.0
  单波发布
- 关联: 承接并细化 ADR-0012（记忆与上下文系统——分层记忆、装配源）、
  ADR-0017（事件总线与反应架构——"装配不读真相域"、策略槽结构）、
  ADR-0009（插件化扩展原则——扩展点须有真实场景）、ADR-0015（模型
  行为与参数归位）
- 版本承载: 0.5.0

## Context（背景）

0.4.0 发布后，用一份经过实战的上下文记忆系统设计文档（三层记忆 +
双轨存储 + 事件摘要 + 图谱归一化 + 四阶段流程：预处理 → 召回评分 →
改写 → 异步写）对本框架做扩展性验证。结论是：这类系统应作为
**外部实现**（自定义 `Context` 自持客户端与存储），不应内建进默认
引擎；但验证过程暴露了三个**读侧契约缺口**与一处命名问题：

1. **输入改写没有一等位置**：外部系统的核心链路是
   `preprocess → build → rewrittenInput → LLM`（指代消解、上下文
   增强、多因素改写）；本框架的 `assemble` 只能追加 background 消息，
   **不能改写本轮 user 输入**——引擎内无法闭环，只能退到应用层编排
   或自定义 `Model` 包装器。
2. **策略槽拿到过宽能力**：`ContextAssemblerInput.memory: &Memory`
   是完整 facade——装配器可以写 L1/L2/L3、可以绕过引擎策展，与
   本框架"错误的能力不可表达"的 ISP 纪律矛盾。
3. **维护槽能力面不成体系**：`MemorySummarizer` 只需 L1 物料；而
   L2→L3 蒸馏（尤其是实体归一化/别名匹配）需要结合 **L1（锚定）+
   L2（加工对象）+ 既有 L3（对齐）**，现在既没有读取面、也没有
   策略槽（`distill` 是硬编码机械提升）。
4. **命名不一致**：`L1Entry` / `SummaryBlock` / `KnowledgeFragment`
   只有 L1 的类型名宣告层号，后两者指代不明确，与
   `MemoryL1/L2/L3Store` 的层号词汇脱节。

## Problem（问题）

1. 外部上下文系统的"改写输入"在框架内不可表达（模型只能看到原始
   user 文本）；
2. 策略槽的能力边界过宽（可写、可策展）→ 框架层不可控；
3. 蒸馏无法读取既有知识 → LLM 归一化/别名匹配不可行；L2→L3 内容
   变换不可插拔；
4. 记忆条目类型命名不宣告所属层，与 store 词汇体系不一致。

## Decision（决策）

### 1. 读侧改写成为一等阶段：模型视图与真相分离

- `ContextAssemblerOutput`（`non_exhaustive`，可加字段）新增
  `pub rewritten_input: Option<String>`：本轮**模型可见输入**；
  `None` = 使用原文（与 0.4.0 行为完全一致，纯加性）。
- agent 循环为该 run 构造**模型视图**：本轮 user 消息使用改写文本；
  发往模型的每个 `ModelRequest`（以及 `ModelEvent::Requested` 的
  `messages`）都是模型视图。
- **canonical messages / `Turn.messages` / `Turn.input` / L1 归档 /
  审计全部保留原文**：改写只影响"模型看到什么"，不污染真相域
  （延续 ADR-0017"装配不读真相域"的边界立法）。
- 改写仅作用于本轮；下一轮从 L1 的原文重新装配，可再次改写。

### 2. 新增 `InputRewriter` 策略槽（默认引擎）

```rust
pub trait InputRewriter: Send + Sync + 'static {
    /// 产出模型可见输入；None = 保持原文。
    /// Err = 可见降级（保留原文并上报 AssemblyFailure），绝不静默。
    fn rewrite<'a>(
        &'a self,
        input: &'a str,             // 用户原文
        background: &'a [Message],  // 装配产物（召回后的事实背景）
        model: &'a dyn Model,       // 引擎上下文模型
        events: &'a EventSink,
    ) -> BoxFuture<'a, Result<Option<String>, String>>;
}
```

- `DefaultContext::with_rewriter(...)`；执行序：装配 → 改写槽（若配置）
  → 填 `rewritten_input`；**有槽时槽胜出**，无槽时装配器自填值透传。
- 自定义整引擎（`trait Context`）可直接填输出字段——改写通道在 trait
  输出上，不强制走默认引擎的槽。
- 改写不预设必须使用 LLM（规则、画像、策略开关均可参与）；`model`/
  `events` 供 LLM 型实现使用与观测，规则型可忽略。

### 3. `MemoryReader`：只读、按主体作用域绑定的记忆门面

```rust
/// 只读、按主体作用域绑定的记忆视图（引擎与策略的物料面）。
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

- **写与策展在类型上不存在**（没有 append/pop/upsert 方法）——编译期
  拒绝，无需运行期 Error；比"运行期报错"更强（无漏网路径、无错误
  处理负担）。`Memory` 自身的读写 facade 保持不变（应用面照用）；
  写入路径永远由引擎收口（槽只返回内容）。
- **取代 `ContextAssemblerInput.memory: &Memory`**：
  `ContextAssemblerInput.reader: MemoryReader<'a>`（装配器只读）；
  `ContextAssemblerInput::new(memory, subject, ...)` 构造签名保持
  不变（内部构建 reader）。

### 4. 槽的最小能力纪律（ISP 立法）

- **槽入参 = 完成职责所需的最小物料 + 最小能力；facade 不进槽；
  写永远由引擎收口。** 大槽（原料域开放增长）用输入对象
  （`ContextAssemblerInput`，ADR-0017）；小槽用松散参数
  （summarize/distill/rewrite），风格一致、不新造输入对象。
- 新增 `MemoryDistiller` 槽——职责是**生成 L3**，原料不限于 L2：

```rust
pub trait MemoryDistiller: Send + Sync + 'static {
    /// 生成 L3 知识内容；可用 reader 读 L1/L2/既有 L3（锚定与归一化）。
    /// 只返回知识文本，L3Entry 的身份与时间由引擎补齐。
    fn distill<'a>(
        &'a self,
        blocks: &'a [L2Entry],        // 本批待加工的 L2 溢出块
        conversation_id: &'a str,     // 供 reader 查 L1/L2
        topic: &'a str,
        reader: MemoryReader<'a>,
        model: &'a dyn Model,
        events: &'a EventSink,
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;
}
```

- `DefaultContext::with_distiller(...)`；默认实现 = 现有机械提升；
  失败语义顺带改良：**先变换、成功后再 pop**（失败保留 L2 +
  `FlowFailed { stage: Distill }`，不丢数据）。
- `MemorySummarizer` 签名保持不变（`entries, model, events`）——它
  只做 L1→L2 内容变换；**L1→L2 的完整策略（何时压、压多少、原子序）
  是引擎职责**（`l1_window`/`l2_cap`），换策略 = 换引擎。

### 5. 模型角色（本波附带收敛）

- `DefaultContext::with_model(Arc<dyn Model>)`（替换
  `with_summary_model`）：**引擎上下文模型**，缺省 = run 模型；引擎
  内全部模型操作（摘要、蒸馏 LLM 化、改写槽调用等）统一解析到它。
- **记忆跟随上下文**：不设独立的记忆模型配置（记忆策展是引擎维护
  的一部分；双旋钮无自然优先级、无真实场景）。
- **移除 `with_summary_prompt`**：提示词是**策略内容**不是引擎配置；
  自定义提示词 = 替换策略（实现 `MemorySummarizer`）。同时修复
  现有"prompt/model 互相覆盖"的构造顺序缺陷（引擎持有 model 字段，
  调用时把解析后的模型传给 `summarize`）。

### 6. 命名统一（记忆条目层号化）

| 现名 | 新名 |
|---|---|
| `L1Entry` | 不变 |
| `SummaryBlock` | `L2Entry` |
| `KnowledgeFragment` | `L3Entry` |
| `FragmentIdentity` | `L3Identity` |

（核实：`FragmentIdentity` 仅被 L3 路径使用——定义、`L3Entry` 构造、
蒸馏、测试种子；无其他引用。）

### 7. 本波附带的 API 修补

- `L1Entry::new(...)`（**硬缺口**：`#[non_exhaustive]` 且无公开构造器，
  外部 L1 store 无法返回条目）+ `created_at`（时间衰减）；
- `L2Entry.created_at`（同上，`new` 自动置时间）；
- `Conversation::topic()` / `set_topic()` 公开（T2：活跃主题=框架标签；
  多主题集合/切换轨迹归引擎态，见扩展指南）。

## Alternatives Considered（备选与否决）

1. **把文档的上下文系统内建进默认 Context**——否决：产品化策略组合
   （事件摘要、双轨权重、意图 boost、图谱 Schema）与基础设施假设
   （Redis/Neo4j/Milvus/embedding）会硬编码进核心，违反 ADR-0009；
   默认保持最小参考实现，该系统作为外部 Context 的旗舰案例。
2. **输入改写只走应用层编排或自定义 Model 包装器**——部分保留（应用
   层仍可行）但不作为唯一路径：引擎内无法闭环；Model 适配器做记忆
   召回与改写语义不自然。
3. **改写文本同时替换真相**（canonical/turn/L1 存改写版）——否决：
   真相污染；后续轮次重放与审计失真。模型视图与真相分离是本 ADR
   的核心边界。
4. **把 `Memory` 全 facade 传给维护槽**——否决：越界可表达（可写、
   可策展），框架不可控；与 `ContextAssemblerInput` 不暴露会话实体
   的同一条 ISP 纪律矛盾。（讨论中一度提议，经 irylex 质疑后否决。）
5. **只读能力用 trait（`MemoryRead`）而非值类型视图**——否决：值类型
   门面（`MemoryReader`）更贴"门面模式"，且避免与 facade 方法面重复
   的 trait 面；无额外收益。
6. **引擎级提示词配置 `with_prompt(PromptConfig)`**——否决：会冻结
   不稳定的操作目录（摘要之外将来还有主题/改写/蒸馏）；提示词属策略
   内容，自定义即替换策略。
7. **蒸馏槽使用输入对象（`DistillInput`）**——否决：小槽保持松散参数
   风格与 `MemorySummarizer` 一致；仅原料域开放的 `ContextAssembler`
   使用输入对象。
8. **多主题会话状态（T1 注册表 / T3 轨迹）**——本轮否决：主题结论为
   T2（活跃主题=框架标签；多主题集合与切换轨迹归引擎态、自持久化），
   无真实触发条件；记录于扩展指南，不单独立 ADR。

## Consequences（后果）

**破坏项（0.5.0 单波）**：

- `ContextAssemblerInput.memory: &Memory` → `reader: MemoryReader<'a>`；
- `with_summary_prompt` 移除；`with_summary_model` 改名 `with_model`；
- `SummaryBlock` → `L2Entry`、`KnowledgeFragment` → `L3Entry`、
  `FragmentIdentity` → `L3Identity`。

**加性项**：

- `ContextAssemblerOutput.rewritten_input`；`InputRewriter` 槽；
  `MemoryReader` 只读门面；`MemoryDistiller` 槽；`L1Entry::new` 与
  `created_at`；`L2Entry.created_at`；公开 `Conversation::topic()` /
  `set_topic()`。

**边界（不入本轮）**：

- 会话终结 flush 保证：应用层观察 `ConversationEvent::Ended` + 幂等
  补偿归档（引擎终结钩子需 conversation→engine 接线，另行立项）；
- 远程存储不进默认 Memory 管线：Memory 插槽面向本地/进程内介质；
  Redis/Neo4j/Milvus 类系统走自定义 `Context`（Route A）。

**文档义务**：v6 架构文档（0.5.0 目标形态，v5 转 SUPERSEDED）、0.5.0
实施计划、扩展指南（扩展点映射 + 边界 + 模型角色）、ADR-0017 修订
注记（配置面变化）、CHANGELOG（Added/Breaking/Migration）、README
指针更新。

**验证要求**：全量回归 + 新测试——改写进入模型视图而真相（`Turn`/
L1）保留原文；默认（无改写）行为与 0.4.0 一致；`MemoryReader` 只读
面可用且装配器不可写；蒸馏默认/自定义路径与失败不丢 L2；命名迁移
后编译面完整。
