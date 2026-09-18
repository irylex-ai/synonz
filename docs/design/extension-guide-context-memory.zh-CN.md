# Synonz 上下文记忆扩展指南（0.6.0）

- 状态: VERIFIED（2026-09-19，随 0.6.0 波次实施；内容与实测行为一致）
- 日期: 2026-09-19
- 依据: ADR-0019、ADR-0020（均 APPROVED）、架构设计文档 v7（APPROVED）
- 性质: 开发文档——扩展实操指引（映射、配方、边界、常见坑）；
  架构决策理由见 ADR-0019/0020 与 v7，本文不重复论证
- 适用: 0.6.0 起；替换 0.4.0 的"实现 `trait Context`"扩展路径

---

## 1. 扩展点总图（四层）

| 层 | 扩展点 | 职责 | 接入方式 | 缺省 |
|---|---|---|---|---|
| 引擎 | `Context`（具体类型） | 读写两相位的基座 | `AgentBuilder::context(Context)` | `Context::new()` |
| 读侧槽 | `ContextAssembler` | 组装背景（模型看到什么） | `Context::with_assembler` | 内置分层装配（内化） |
| 读侧槽 | `TurnInputRewriter` | 组装前预处理本轮输入（模型视图） | `Context::with_rewriter` | 无改写 |
| 写侧子钩子 | `ConversationTopicDetector` | 主题标签与漂移判定 | `Context::with_topic_detector` | 首段启发式（内化） |
| 写侧子钩子 | `MemorySummarizer` | L1→L2 内容变换 | `Context::with_summarizer` | 提示词摘要（内化） |
| 写侧子钩子 | `MemoryDistiller` | L2→L3 内容生成 | `Context::with_distiller` | 机械提升 |
| 存储位 | `MemoryL1/L2/L3Store` | 逐层持久化（L2/L3 含按 id 的 `get`/`update`/`remove`/`list`） | `RuntimeBuilder::memory_*_store` | 进程内 |
| 旁路 | `Observer` | 全量事件目击 | `RuntimeBuilder::observer` | 无 |
| 应用面 | `Memory` | 条目管理（查看/纠正/遗忘） | `runtime.memory()` | — |

配置面：`l1_window` / `l2_cap`（楼层参数）、`with_model`（引擎模型）。
载荷门面：`MemoryReader`（只读；策略槽的物料面，框架构造、随载荷交
给槽）。

**不可扩展**（ADR-0019 边界）：写相位的整段编排（主题→归档→压实→
蒸馏的顺序与写入）由框架独占——写权限不外放。

---

## 2. 选择扩展档位

| 需求 | 做法 |
|---|---|
| 换摘要提示词 / 摘要方式 | 实现 `MemorySummarizer`（提示词是策略内容） |
| 换知识提取（LLM 抽取、图谱写入前的归一化） | 实现 `MemoryDistiller`（可读 L1/L2/既有 L3） |
| 换主题判定 | 实现 `ConversationTopicDetector` |
| 换召回/组合/格式（不同装配哲学） | 实现 `ContextAssembler` |
| 组织前的指代消解 / 输入规范化 | 实现 `TurnInputRewriter` |
| 换持久化介质 | 实现对应的 `MemoryL*Store`（L2/L3 必选补齐按 id 的方法，见 §5） |
| 会话终结时做外部收尾 | 订阅 `ConversationEvent::Ended`（Observer） |
| 用户/应用纠正与遗忘记忆 | `Memory` 应用面（`list`/`get`/`edit`/`forget`/`forget_matching`，见 §2.1） |
| 批量导入 / 迁移 | 框架外：自持存储句柄直接写；测试/fixtures 用 `test-util` 种子 |

### 2.1 应用面：记忆管理（ADR-0020）

```rust
let memory = runtime.memory();              // 应用面（条目管理）
let subject = Subject::of(SubjectType::User, "u-1");

// 统一的记忆列表：Summary 块在前、Knowledge 块在后（各按更新时间倒序）
let page = memory.list(&subject, MemoryQuery::new(20))?;
for item in &page.items {
    // MemoryItem { id, memory_type, content, source, created_at, updated_at }
}

// 纠正：原地更新（id / 类型 / 来源不变，更新时间刷新）
memory.edit(&subject, &item.id, "corrected text")?;

// 遗忘：单条（幂等）/ 批量过滤（逐条尽力、limit 为批量上限）
memory.forget(&subject, &item.id)?;
memory.forget_matching(&subject, MemoryQuery::new(100).with_keyword("outdated"))?;
```

- **条目 = L2 情景摘要（`MemoryType::Summary`）+ L3 知识
  （`MemoryType::Knowledge`）**；L1 是原始材料，不在管理面；
- **分页**：keyset（`MemoryQuery` / `MemoryListCursor` /
  `MemoryPage`），`limit` 有界；列表顺序是**结构序**（不是严格全局
  时间序）；
- **新鲜度**：统一 `updated_at`（创建 = `created_at`；编辑/更新
  刷新）——列表排序、分页游标与召回排序同一口径；
- **同权**：用户编辑与系统写入走同一路径、无优先级（同身份槽原地
  更新，后到者生效）；**错误覆盖属于策略质量**（算法/提示词/写前
  对账），在策略槽里改进；
- **遗忘有效性**：操作返回后所有读路径不可见；在途维护经"按 id
  重校验 + 写前认领"不回写（框架内部，槽无感）；被删记忆在新对话中
  被重新告知 = 新证据（会再次生成）；
- **事实**：`MemoryEvent::Updated` / `Removed`（不含内容）；
- **不做日常新增**：新记忆来自对话沉淀；测试/fixtures 用
  `test-util`（`seed_l1/l2/l3` + `reader_for_tests`）；批量导入/
  迁移由应用自持存储句柄在框架外完成（框架不感知、无事实、无有序
  保证——文档化边界）。

---

## 3. 读侧配方

### 3.1 自定义装配（`ContextAssembler`）

```rust
impl ContextAssembler for MyRecall {
    fn assemble<'a>(&'a self, input: ContextAssemblerInput<'a>)
        -> BoxFuture<'a, ContextAssemblerOutput> {
        Box::pin(async move {
            let mut output = ContextAssemblerOutput::default();
            // 只读门面：l1_window / l1_len / l2_read / l2_len /
            //            l3_query / l3_len
            let query = input.rewritten_input.unwrap_or(input.input);
            match input.reader.l3_query(query, input.topic, 5) {
                Ok(entries) => { /* 组装消息 */ }
                Err(error) => output.failures.push(AssemblyFailure {
                    stage: MemoryFlowStage::AssembleRead,
                    detail: format!("l3 query: {error}"),
                }),
            }
            output
        })
    }
}
```

- **只读**：`MemoryReader` 没有 append/pop/upsert——越界不可表达；
- **不读真相域**：输入没有会话实体、没有 history turns（背景与真相
  分离）；近期对话经 `reader.l1_window` 读取；
- **降级可见**：读失败进 `output.failures`，不静默。

### 3.2 输入改写（`TurnInputRewriter`）

```rust
impl TurnInputRewriter for ResolveCoreference {
    fn rewrite<'a>(&'a self, input: &'a str, history: &'a [Message])
        -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            // `history` = 本会话近期 L1 消息（引擎提供；策略决定用量）
            // 规则型可直接处理；LLM 型自持客户端（其调用可观测性自担）
            Ok(Some(format!("…{input}…")))
        })
    }
}
```

- **顺序**：改写 → 组装；改写结果进入
  `ContextAssemblerInput::rewritten_input`，供组装/召回使用；
- **模型视图 vs 真相**：模型可见的当前 user 消息仍是原文；canonical
  messages / `Turn` / L1 全留原文；
- **失败语义**：`Ok(None)` = 原文；`Err` = 可见降级（保留原文 +
  `FlowFailed { stage: Rewrite }`）。

---

## 4. 写侧配方

### 4.1 摘要（`MemorySummarizer`）

```rust
impl MemorySummarizer for MySummary {
    fn summarize<'a>(&'a self, entries: &'a [L1Entry], model: &'a dyn Model)
        -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            // 只调用传入的 model——框架已把句柄包成叙述包装；
            // 不要自己发事件（叙述是框架的保证）。
            let call = ModelRequest::new(vec![/* … */], Vec::new());
            synonz::complete(model, call).await
                .map(|(message, _)| /* 文本 */ String::new())
                .map_err(|error| error.to_string())
        })
    }
}
```

- 触发由框架驱动（L1 溢出 / 主题漂移冲刷），不是逐轮；
- `Err` = 无损降级（原文转录入 L2）+ 可见事实。

### 4.2 蒸馏（`MemoryDistiller`）

```rust
impl MemoryDistiller for EntityExtraction {
    fn distill<'a>(
        &'a self,
        blocks: &'a [L2Entry],
        conversation_id: &'a str,
        topic: &'a str,
        reader: MemoryReader<'a>,
        model: &'a dyn Model,
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            // 可跨层只读：reader.l1_window（锚定）/ l2_read / l3_query
            // （别名与归一化对齐）
            Ok(vec!["knowledge text".into()])
        })
    }
}
```

- 返回**知识文本**；`L3Entry` 的身份 `(subject, conversation, topic)`
  与时间由引擎补齐；
- **先变换、成功后再认领**：`Err` 保留 L2（不丢数据）+ 可见事实；
  在途遗忘经"变换前按 id 重校验 + 写前按 id 认领"处理——来源集合
  有变即丢弃本次产物（框架内部，槽无感）；
- 默认机械实现：每条 L2 → 一条知识文本（无模型调用）。

---

## 5. 存储接入与远程介质

- 三存储位为**同步契约**；注册即用，逐层可换（L2 Redis / L3 向量库
  等）；
- 条目构造：`L1Entry::new(conversation_id, topic, messages)`；`L2Entry::new(conversation_id, content, index).with_topic(topic)`；`L3Entry::new(identity, content)`——`id` 与时间戳自动生成（旧数据缺省见迁移指南）；
- **自定义存储迁移（0.6.0 必选）**：`MemoryL2Store` / `MemoryL3Store`
  需补齐 `get` / `update` / `remove` / `list`（按 id 与 keyset 枚举）；
  `MemoryL3Store::upsert` 同身份替换需**保留原 id**；完整清单见
  `docs/design/migration-0.6.0.zh-CN.md`；
- **边界 G3（远程存储）**：网络 I/O 型介质不进入默认 Memory 管线的
  保障范围——两条可行路径：
  1. **策略内自持**：在 `ContextAssembler` / `MemoryDistiller` 等槽
     内自持客户端/连接（其调用的可观测性与失败语义自担）；
  2. **应用层编排**：在 Agent 之外同步数据（写入走应用自己的服务，
     框架只负责对话与本地管线）。
- 存储位**不得策展**：append 时不得压缩/蒸馏（策展时机由生命周期
  事实驱动）。

---

## 6. 模型角色

| 用途 | 模型 |
|---|---|
| 推理循环（含工具） | agent 模型（`AgentBuilder::model`） |
| 引擎维护（摘要 / 蒸馏） | `Context::with_model` ?? agent 模型 |
| 策略自持调用的模型 | 策略自己持有（框架不叙述） |

- 引擎交给子钩子的句柄是**叙述包装**：辅助调用自动发
  `ModelEvent::Requested` / `Responded`（`round: None`），**不发
  `StreamDelta`**；
- 记忆跟随上下文：不设独立的记忆模型配置；
- 自持客户端的调用**不在框架叙述保证内**（需要观测就自己发，或改用
  传入句柄）。

---

## 7. 边界与绕行

| 边界 | 现状 | 绕行 |
|---|---|---|
| 整段写侧扩展（换编排/时序/写入） | 框架独占（ADR-0019） | 用子钩子 + 存储位表达；确需整段编排时提出触发需求 |
| 会话终结 flush（G2） | 无引擎终结钩子 | Observer 订阅 `ConversationEvent::Ended` + 幂等补偿归档 |
| 远程存储（G3） | 不进默认 Memory 管线 | §5 两条路径 |
| 辅助叙述流式（`emit_deltas`） | 辅助调用恒无 `StreamDelta` | 无当前需求；出现再引入开关 |
| 多主题集合 / 切换轨迹 | 框架只有活跃主题标签（T2） | 见 §8 |
| 应用日常新增（`add`） | 不提供（ADR-0020） | 对话沉淀；测试走 `test-util`；批量导入在框架外（自持存储句柄） |
| 严格全局时间序（跨类型归并） | 列表为结构序（Summary 块 → Knowledge 块） | 应用各自拉两页归并（可加性后续） |
| 语义搜索 | v1 由 `list` 的关键字过滤表达 | 将来单独立项 |
| 事实聚合 / 矛盾消解 | 归策略层（ADR-0020） | 自定义 `MemoryDistiller` 用查询 + 管理动词实现 |
| 物理删除 / 副本 / 备份 / 加密擦除 | 归存储实现 | 自定义存储自行负责并在文档承诺 |
| L1 留存与会话级清理 | 另行立项 | 自持存储句柄在框架外清理 |

---

## 8. 主题层次（T2 结论）

- **活跃主题 = 框架标签**：`ConversationEvent::TopicShifted` 事实 +
  `Conversation::topic()/set_topic()`（公开）；
- **多主题集合 / 切换轨迹 = 引擎态**：由自定义
  `ConversationTopicDetector` 或应用自行持久化，框架不设注册表；
- 漂移的语义后果（冲刷前段进 L2 + 发事实）由框架执行——策略只判定
  `TopicDecision`。

---

## 9. Route A 迁移指引（服务化记忆系统 → 扩展点映射）

以三层记忆 + 双轨 + 图谱的实战文档为例：

| 文档组件 | Synonz 映射 |
|---|---|
| 指代消解（阶段 1，LLM，近 3 轮） | `TurnInputRewriter`（`history` = L1 窗口） |
| 三层召回 + 统一评分（阶段 2） | `ContextAssembler`（`MemoryReader` 六方法；评分/去重/预算自持） |
| 双轨（user/agent） | 自行在存储位或装配器内编码轨道语义（L1 条目可按内容分类） |
| L2 事件摘要（每轮 LLM 判定） | **边界**：默认写侧按 L1 溢出触发，不逐轮；逐轮事件可用外部编排，或接受溢出触发语义 |
| L3 图谱 + 向量（Neo4j/Milvus） | `MemoryL3Store` 自定义（同步包装自持客户端）或策略内自持（G3） |
| 异步写（asyncio 队列） | 框架后台任务（压缩/蒸馏）自动异步；外部写走应用层队列 |
| 会话结束批量同步 | Observer 订阅 `ConversationEvent::Ended` 触发 |
| 上下文改写（阶段 3，融合成单串） | 本框架以结构化 `messages` 交付召回；如需单串增强输入，可在 `ContextAssembler` 内自持 LLM 实现（`rewritten_input` 不回流模型） |

---

## 10. 常见坑

1. **不要自己发事件**：内容变换槽（摘要/蒸馏/改写/装配）拿到的模型
   句柄已叙述；手工发事件会双重发射或绕过叙述；
2. **写侧没有相位扩展**：需要"换整套写侧哲学"时，先确认能否用
   `MemorySummarizer` / `MemoryDistiller` / 存储位表达；
3. **`rewritten_input` 只影响组装**：模型可见的当前 user 消息是
   原文——不要把消解结果当成模型输入视图；
4. **`MemoryReader` 只读**：装配器/蒸馏器的越界写在类型上不存在，
   不要试图绕过（框架写入收口）；
5. **无改写 = 0.4.0 行为**：`Context::new()` 未配置新槽时读侧行为与
   0.4.0 完全一致（迁移期可逐步启用）；
6. **L3 身份去重**：`(subject_id, conversation_id, topic)` 相同的
   条目按 upsert 语义合并——一批蒸馏文本共享同一身份，返回多文本时
   注意最终保留语义；
7. **终结收尾不属于引擎**：`shutdown()` / `end()` 的收尾是 Runtime
   结构行为（drain + 机械促进），不要指望引擎钩子；
8. **编辑会被后续系统写入覆盖**：同权语义（同身份槽原地更新、后到
   者生效）——要"编辑优先"就在策略层解决（写前对账/提示词），内核
   不设优先级；
9. **列表不是严格全局时间序**：Summary 块整块在前、Knowledge 块在
   后；需要混合时间线时应用自行归并；
10. **`MemoryReader` 不能自行构造**：由框架经载荷交给策略槽；自测
    走 `test-util`（`reader_for_tests`）；
11. **遗忘后重新被告知 = 新证据**：会再次生成条目（预期行为，不是
    复活）；物理不可恢复由存储实现负责。
