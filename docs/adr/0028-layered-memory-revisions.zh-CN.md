# ADR-0028: 分层记忆组件重定——按轮 L1、批量压实 L2、可配置 Schema L3 与管理边界补充

- 状态: APPROVED（2026-09-23，irylex 人工评审通过）
- 日期: 2026-09-23
- 决策者: irylex（本次逐项确认；遵循 `.opencode/rules/architecture.md` §5–§9
  的渐进决策流程）
- 性质: 组件设计 ADR——**修订 ADR-0026** 的 §1（层次模型与单元）、§2（管线
  映射的写入机制）、§3（存储契约与 Schema）、§6（观察事件清单）、§7（管理
  映射）、§8（分区来源）；**命名与分包随文落定**
- 关联: 引用 ADR-0022/0023/0025/0027；设计基线 =
  《上下文记忆系统全链路技术方案》（下称"文档"）；ADR-0026 未废止，其余
  决策继续有效
- 版本承载: 0.7.0（组件尚未发布：无外部兼容影响、无数据迁移）

## Context（背景）

ADR-0026 按文档重定了官方分层记忆组件（L1 双轨消息 / L2 事件摘要（逐轮
LLM 判断）/ L3 图谱+向量 / 四阶段管线 / 四端口 / 语义策略 / 组件观察面），
并在 0.7.0 落地。实施后的逐项评审确认了六处问题：

1. **L1 单元与召回单位分裂**：文档的"双轨分存"以**消息**为单位，而召回
   需要的是**一轮问答**的闭环；消息级拆分会导致召回半轮，或必须在装配时
   再拼回轮次；
2. **L2 逐轮判官的 token 成本与窗口内冗余**：稳态每轮一次判官调用；而
   L1 窗口之内，逐轮事件与 L1 原文互为冗余——L2 的独特价值出现在内容
   **滚出窗口之后**；
3. **L3 Schema 内置业务域词**：文档 §6.4 的类型/关系 Schema
   （PRODUCT / BRAND / LIKES…）是电商域词表，被原样落成公开枚举——
   通用组件不应内置任何业务域的实体分类；
4. **管理边界不完整**：IDE 场景中"纠正一条错误关系"是常态，而"L3 实体
   → 条目"的映射无法覆盖（只能删整个实体，粒度太粗）；
5. **分区声明生命周期错位**：provider 在应用启动时构造，而项目在运行期
   才创建——启动期无法声明 `project:xxx`；写死字符串的调用形态不合理；
6. **命名与分包不合规**：层编号进入公开类型（`L1Message` / `EventSummary`
   / `Entity` 三种风格）、`*Port` 后缀、配置字段无域前缀、
   `maintenance` 等运维词出现在业务组件里。

## Problem（问题）

L1 的存储单元必须与召回单元一致；L2 的产出机制应以"滚出窗口"为触发、
以批量为粒度，同时保留文档的条目语义（内容 + 版本历史）；L3 的 Schema
必须是应用/策略配置而不是框架内置；管理面必须覆盖"关系的纠正"；长记忆
分区必须在使用期解析；组件内部命名与分包必须自表达。

## Decision（决策）

### 1. L1：按轮条目（修订 ADR-0026 §1 的层次模型）

- **一条 = 一轮**：`L1MemoryEntry { id, scope, topic, input, response,
  created_at }`——`input` 用户侧原文、`response` 本轮**最终回答**
  （工具结果与中间消息不进 L1，它们在真相存档里）、`topic` 为归档时的
  会话 topic；
- **双轨不再分存为两条序列**，而是条目内的两侧字段：`input` 是记忆来源
  （L2/L3 只取 `input` 侧；召回语义分以两侧较大者计算、乘用户权重），
  `response` 作为上下文随条目带入；
- **容量 10 轮**：追加时整轮淘汰（一轮一条目）；`id` 由创建方生成
  （uuid），存储不分配；
- 存储契约 `L1MemoryStore`：`append(entry)` / `recent(scope, turns)` /
  `count(scope)` / `remove(scope, id)` / `clear(scope)`（数据自带 scope，
  写从数据取、读带分区）；
- 进程内默认实现 `InProcessL1MemoryStore::new(retained_turns)`。

### 2. L2：批量压实（修订 ADR-0026 §1/§2）

- **产出模型改为批量压实**：触发 = 该会话未压实轮数达到 `l1_turns`
  （满窗）/ 主题切换 / 会话结束；一次摘要调用产出 **1..N 条** L2 条目
  （按事件/主题拆分）；
- 条目 `L2MemoryEntry { id, scope, topic, content, versions, embedding,
  importance, created_at, updated_at }`：
  - `content` 上限 `l2_content_chars`（默认 30 字，文档口径）；
  - `versions` 为历次 content（倒序、≤`l2_versions`）——更新时旧 content
    进 `versions` 头部，与文档 §3.2 的 event/content 数组一致；
  - `importance`（0..1）由摘要策略逐条评出，进入召回评分；
- **内容合法性由策略保证**：非空且 ≤ `max_chars`，违约返回 `Err`——
  组件**不截断**（宁可失败可见，不落残缺内容）；
- **取消**：逐轮判官、`key_facts` / `topic_tag` 独立结构化摘要类型、
  `origin` 区分——"主题总结"即压实产出的条目本身；L3 的原料由"本批 L1
  用户侧文本 + 会话 L2 条目"承担；
- 存储契约 `L2MemoryStore`：`upsert(entry)` / `get(scope, id)` /
  `list(scope)`（最近更新在前）/ `remove(scope, id)` / `count(scope)`；
  容量（条目上限、版本上限）由实现自持（构造时给）；
- 轻量预判（极短/确认性输入）在批量语义下不再需要逐轮跳过：批次内由
  摘要策略决定是否产出条目（空输出 = 不落库）。

### 3. L3：可配置 Schema、实体 id 与蒸馏（修订 ADR-0026 §1/§3）

- **Schema 可配置**：实体类型与关系类型在记录中是**不透明字符串**；
  词表由配置提供：
  ```rust
  pub struct L3MemorySchema {
      pub entity_types: Vec<String>,       // 默认中性集；空 = 不约束
      pub relation_types: Vec<String>,
      pub entity_type_fallback: String,    // 默认 "other"
      pub relation_type_fallback: String,  // 默认 "related_to"
  }
  ```
  提取提示词按词表约束；**组件侧归一化统一兜底**（大小写不敏感匹配规范
  拼写、词表外映射到兜底项）——领域词表（PRODUCT / BRAND…）不再出现在
  类型系统里；
- **实体节点新增合成 `id`**（uuid，每条记录唯一）：管理面目（核心条目
  映射）按 id 寻址；图存储提供 `get_entity_by_id` / `remove_entity_by_id`
  （级联删边）；`upsert_entity` 按 `(scope, canonical_name)` MERGE 且
  **保留原记录的 id 与 created_at**（身份连续性）；
- **边不加 id**：身份 = `(scope, from, relation_type, to)` 复合
  自然键；边的删除由 `remove_edge(scope, from, relation_type, to)`
  提供（供句柄的关系遗忘）；
- **别名合并恢复 LLM 判断**（修订 ADR-0026 §1 的"embedding 候选 + LLM
  判断"）：向量候选相似度 ≥ `l3_alias_similarity`（默认 0.85）时调用一次
  LLM 判断是否同一实体；
- **词汇统一**：`compact` = 压实（L1→L2）、`distill` = 蒸馏（→L3）；
  `L3Memory::distill(scope, batch, entries, model)` 的触发 = 主题切换 /
  会话结束；输入 = 本批 L1（用户侧）+ 会话 L2 条目 + **相关已知实体子集**
  （embedding 检索 Top-K，有界）；
- 提取器 `L3MemoryEntityExtractor` 输出 `L3MemoryGraph`（pre-normalization，
  节点 + 边）；写入顺序**先向量后图**（部分失败只留无害的孤儿向量）；
- 向量存储契约 `L3MemoryVectorStore`：`upsert(scope, canonical_name,
  vector)` / `remove` / `search`——键 = 图身份（与文档 Milvus 主键一致）；
- 终局**限定修复**：记录本会话触碰过的实体，`finalize` 只为它们补写向量
  （成本有界，不做全量重写）。

### 4. 管理边界与管理面（补充 ADR-0026 §7）

- **进核心条目映射**：L2 条目 + L3 实体 → 核心 `Memory` 五动词
  （`list` / `get` / `edit` / `forget` / `forget_matching`，直接走存储
  契约）。`edit` 原地替换内容并**把旧 content 推入 `versions`**、
  **重嵌入**（L3 重算实体描述向量）；`forget` 级联删向量与相连边；
  管理事实 `Updated` / `Removed` 按 scope 分组、无内容；
- **关系的遗忘**由组件句柄提供：
  `LayeredMemoryActuator::forget_l3_memory_relation(subject, scope, from,
  relation_type, to)`（核心契约与 ADR-0026 §7 的映射不变；这是 IDE
  场景"纠正错误关系"的落点）；
- `LayeredMemoryActuator`（公开）= 富面读（`l2_memory_entries` /
  `l3_memory_entities` / `l3_memory_relations`）+
  `forget_l3_memory_relation`；原 `MemoryType` / `TypedItem` /
  `list_typed` **删除**（核心条目视图不带类型；Summary/Knowledge 的映射
  保留为文档描述）；
- **subject 隔离**：会话分区在归档时记录 subject 归属；管理面与富面
  只在该 subject 的会话分区与 `resolve(subject, "")` 的 L3 分区内查找；
  显式 `scope` / `conversation_id` 过滤同样校验归属（不属于返回空）；
  管理面无创建动词（新增恒来自对话沉淀）。

### 5. 长记忆分区：`MemoryScopeResolver`（修订 ADR-0026 §8 的"应用配置"）

- 启动期声明分区的调用形态不成立（项目运行期才创建），改为**使用期解析**：
  ```rust
  pub trait MemoryScopeResolver: Send + Sync + 'static {
      fn resolve(&self, subject: &Subject, conversation_id: &str) -> Vec<MemoryScope>;
  }
  ```
  builder `.scope_resolver(..)`；组件在**蒸馏/召回时**按会话求值；应用可
  查自己的项目库；
- 未配置 = 按 subject 推导 `user:<subject>`（零配置可用）；返回空 = 该
  会话暂不写、不查 L3；
- 会话分区（`conversation:<id>`）仍由组件内部从 `conversation_id` 推导，
  不需要配置。

### 6. 召回与配置细化（细化 ADR-0026 §2/§4）

- **召回评分**：`语义×0.5 + 时间×0.3 + 重要×0.2`，乘双轨权重（用户 ×1.2 /
  代理 ×1.0）与 **topic 一致加成**（`recall_source_boost`）——不使用
  文档的"意图感知源权重"（需要额外的意图分类来源，成本与不确定性高）；
- **重要度来源**：L2 用条目自带 `importance`；L1 / L3 无字段，评分时用
  中性常量 `recall_default_importance`（默认 0.65，文档默认）；
- **L1 语义分**取 `input` 与 `response` 的较大者（避免代理侧信息漏召回）；
- **去重**：文本相同或向量相似度 ≥ `recall_dedupe_similarity`（默认 0.95，
  文档口径）；
- **冷启动门槛**保持：L1 + L2 候选皆空时剔除 L3；
- 上下文改写（`LayeredMemoryContextRewriter`）是**组件级扩展点**（非核心
  扩展点），仅在召回非空时调用；
- **配置**：`LayeredMemoryConfig` 扁平，字段带域前缀（`l1_` / `l2_` /
  `l3_` / `recall_` / `topic_`）；压实节奏复用 `l1_turns`（不再单独设
  批次参数）。

### 7. 命名与分包（随文落定）

| 文件 | 定稿内容 |
|---|---|
| `l1_memory.rs` | `L1MemoryEntry` / `L1MemoryStore` / `InProcessL1MemoryStore` / `L1Memory` |
| `l2_memory.rs` | `L2MemoryEntry` / `L2MemoryStore` / `InProcessL2MemoryStore` / `L2Memory` / `L2MemorySummarizer`(+Input/Output/Summary) / `L2MemoryPromptSummarizer` |
| `l3_memory.rs` | `L3MemoryGraphEntity` / `L3MemoryGraphEdge` / `L3MemoryGraph` / `L3MemoryGraphStore` / `L3MemoryVectorStore` / 两个 `InProcess*` / `L3Memory` / `L3MemoryEntityExtractor`(+Input) / `L3MemorySchema` / `L3MemoryPromptEntityExtractor` / `entity_text` |
| `assembler.rs` | `LayeredMemoryContextAssembler` / `LayeredMemoryContextRewriter`(+Input) / `LayeredMemoryPromptContextRewriter` / `Candidate`（私有） |
| `pipeline.rs` | `LayeredMemoryPipeline` / `TurnBackgroundTask`（私有） |
| `memory.rs` | `LayeredMemory`（核心 `Memory` 实现）/ `LayeredMemoryActuator` / `MemoryRecord`+`freshness`（私有） |
| `rewriter.rs` / `topic_detector.rs` | `CoreferenceInputRewriter`(+Provider) / `SimilarityTopicDetector`(+Provider) |
| `embedding.rs` / `observation.rs` | `Embedding` / `HashEmbedding`；`LayeredMemoryObserver` / `LayeredMemoryEvent { Compacted, Distilled, Normalized, Skipped }` |
| `config.rs` / `utils.rs` | `LayeredMemoryConfig`（域前缀）；`message_text` / `final_answer` / `first_segment` / `is_trivial` / `cosine` / `now_epoch` |
| `provider.rs` / `lib.rs` | `LayeredMemoryProvider` + `LayeredMemoryProviderBuilder`（含 `.scope_resolver(..)`）；门面 |

- `*Port` 一律改 `*Store`；**无** `strategies.rs`（策略跟随其定义所在文件）、
  **无** `maintenance`（后台作业 = `TurnBackgroundTask`）；管理面文件叫
  `memory.rs`；组件句柄叫 `LayeredMemoryActuator`。

## Alternatives Considered（备选与否决）

1. **L1 保持消息级（文档字面）**——否决：存储单位与召回单位分裂，召回要
   么半轮、要么装配时重拼；
2. **L2 保持逐轮判官（模型甲）**——否决：稳态每轮一次 LLM 调用；
   窗口内与 L1 原文冗余；
3. **L2 主题总结独立落库（模型 A / origin 区分）**——否决：与"事件条目"
   两套产出方，`origin` 只为容量口径服务；批量压实后不再需要；
4. **L3 保持固定词表**——否决：业务域词表焊进通用组件，且未知值静默降级；
5. **关系也进核心条目映射**——否决：`edit` 对边无语义（要解析重连），
   核心条目列表无类型过滤（实体与边混列）；
6. **启动期声明长记忆分区**——否决：生命周期错位（项目运行期才创建）；
7. **会话登记分区（push）**——否决：多一份登记状态与"忘记登记"的失效率；
8. **每项目一套 runtime/provider**——否决：跨项目用户记忆分裂、实例成本高；
9. **向量修复全量重写**——否决：成本随图谱规模增长；改为限本会话触碰的
   实体 + 先向量后图的顺序保险。

## Consequences（后果）

**破坏项（0.7.0，相对 ADR-0026）**

- L1：双轨消息分存 → **按轮条目**（两侧字段）；`L1MemoryEntry` 的字段与
  存储契约重定；
- L2：逐轮 LLM 判断更新/创建 → **批量压实**；条目新增 `importance`；取消
  独立结构化摘要类型与 `origin`；
- L3：固定枚举 Schema → **可配置字符串词表**；实体新增 `id`；别名合并
  恢复 LLM 判断；`absorb` → `distill`；
- 管理：删除 `MemoryType` / `TypedItem` / `list_typed`；新增
  `LayeredMemoryActuator`（含 `forget_l3_memory_relation`）；
- 分区：provider 声明 → **`MemoryScopeResolver`**（使用期解析）。

**加性项**

- `MemoryScopeResolver`、`L3MemorySchema`、`remove_edge` /
  `get_entity_by_id` / `remove_entity_by_id`、`Distilled` / `Compacted`
  观察事件、`Candidate` / `MemoryRecord`（内部）。

**边界（本 ADR 明确不做）**

- L1 参与管理、scope 转换（`move` / `promote` / `demote`）、语义搜索 /
  矛盾消解、真实后端包（Redis / MongoDB / Neo4j / Milvus）随原边界保持；
- L1/L3 的真实重要度字段（当前用中性常量）。

**文档义务**

- v8 组件章节与 §4/§9/§11/§12 同步；0.7.0 计划（M41–M43 追加"组件重定"
  记录与旧名→新名对照）；`migration-0.7.0`；CHANGELOG；扩展指南；
  组件 README；组件测试拆分（`l1_memory` / `l2_memory` / `l3_memory` /
  `assembler` / `pipeline` / `memory` / `provider`）。

**验证要求**

- L1：按轮条目、两侧字段、10 轮整轮淘汰、topic 盖章；
- L2：批量压实（三个触发）、1..N 条产出、`content ≤ max_chars`（违约
  Err 不截断）、旧 content 进 `versions`（≤5）、`importance` 参与评分；
- L3：词表约束与兜底映射、实体 id 寻址与身份连续性、别名合并的 LLM
  判断、蒸馏的 Top-K 已知实体、先向量后图、限定向量修复；
- 召回：评分公式、双轨权重、topic 加成、去重阈值、冷启动门槛、无召回
  不调改写；
- 管理：五动词（含 edit 推版本与重嵌入、forget 级联）、
  `forget_l3_memory_relation`、管理事实的 scope 分组；
- 分区：`MemoryScopeResolver` 使用期求值、缺省推导、空返回；
- 观察面：`Compacted` / `Distilled` / `Normalized` / `Skipped`；
- 全量：fmt / clippy / test / doc / 示例（含 `layered_memory`）。
