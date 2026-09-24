# Synonz 上下文记忆扩展指南（0.7.0）

面向"要替换或接入记忆能力"的下游开发者。0.6.0 的旧扩展面（公开
`Context`、`ContextAssembler`、策略槽、三存储契约位）已移除；本指南
按 0.7.0 的契约族重写。架构理由见 `docs/architecture/v8.zh-CN.md` 与
`docs/adr/0022`–`0027`；API 迁移见
`docs/design/migration-0.7.0.zh-CN.md`。

---

## 1. 扩展点总图

```
核心（契约 + 内置引擎 + 进程内默认）
  Memory                    条目管理（应用面；runtime.memory()）
  MemoryContextAssembler    读：产出 system(Agent) 之后的完整消息帧
  MemoryPipeline            写：三钩子（archive_turn / spawn_task /
                            finalize_conversation）+ 核心模板
  MemoryProvider            记忆能力工厂（memory / assembler / pipeline
                            + 可选上下文管理模型）
  RewriterProvider          读侧预处理工厂（Agent 级，可选模型）
  TopicDetectorProvider     写侧主题检测工厂（Agent 级，可选模型）

官方组件 synonz-layered-memory（配置面）
  四个存储契约              L1 消息 / L2 条目 / L3 图 / L3 向量
  embedding 端口            向量化
  语义策略                  批量压实（1..N 条）/ 实体提取 / 上下文改写
  组件观察面                分层进展事实
  参数                      LayeredMemoryConfig（域前缀）
```

选择顺序：**先看官方组件是否够用**（配置端口 / 策略 / 参数），不够
再换 provider（实现整套契约族），最后才是逐契约替换（assembler /
pipeline 可分别实现，工厂成对产出保证读写成对）。

## 2. 应用面：记忆管理

```rust
let memory = runtime.memory();                    // Arc<dyn Memory>
let page = memory.list(&subject, MemoryQuery::new(20))?;   // 全部 scope 合并
let scoped = memory.list(&subject, MemoryQuery::new(20).with_scope("project:abc"))?;
memory.edit(&subject, &id, "corrected")?;         // 原地更新，id 不变
memory.forget_matching(&subject, MemoryQuery::new(100).with_scope("project:abc"))?;
```

- `scope` 是**不透明字符串**：核心只按相等性分组 / 过滤，不解释语义；
  取值（`"project:abc"` / `"user:u1"` / …）由应用定义。
- **subject 隔离**：每次调用传入的 subject 决定可见范围——默认列出
  该 subject 的全部分区；显式 `scope` / `conversation_id` 过滤同样
  校验归属（不属于该 subject 的分区返回空），按 id 的
  `get` / `edit` / `forget` 也只在该 subject 的分区内查找。组件的
  会话归属**随数据走**（L2 条目携带 `subject`，存储提供
  `scopes(subject)` 枚举分区），因此重启后管理面仍然完整，组件
  不持有随会话数增长的内存索引。
- 管理事实：`MemoryEvent::Updated { subject_id, scope, id }` /
  `Removed { subject_id, scope, ids }`（无内容，携带分区）。
- 没有日常新增（`add`）：新增恒来自对话沉淀；批量导入 / 迁移由应用
  自持存储句柄在框架外完成。

## 3. 读侧配方

### 3.1 自定义装配（`MemoryContextAssembler`）

```rust
impl MemoryContextAssembler for MyAssembler {
    fn assemble<'a>(&'a self, input: MemoryContextAssembleInput<'a>)
        -> BoxFuture<'a, MemoryContextAssembleOutput>
    {
        Box::pin(async move {
            // 读状态：input.reader（条目级只读投影）、input.subject /
            // conversation_id / topic / input / rewritten_input
            // 调模型：input.model（核心已叙述：Requested/Responded，无 StreamDelta）
            MemoryContextAssembleOutput::new(
                vec![Message::user("...")],   // system(Agent) 之后的完整帧
                vec![],                        // 失败清单（降级可见）
            )
        })
    }
}
```

契约义务：**必须**返回含本轮用户消息的可用帧；失败时自行降级（把
原始 input 放进帧 + `failures` 上报）。空帧时核心兜底（原始 input +
`Failed` 事实）。角色约定：system 只放 Agent 指令，记忆上下文与增强
输入进 user 侧。

### 3.2 输入改写（`TurnInputRewriter`，Agent 级）

```rust
impl TurnInputRewriter for MyRewriter {
    fn rewrite<'a>(&'a self, input: RewriteInput<'a>)
        -> BoxFuture<'a, Result<Option<String>, MemoryFailure>>
    {
        // input.input / input.history（真相域最近成功轮）/ input.model
        Box::pin(async move { Ok(None) })   // None = 保持原文
    }
}
let agent = Agent::builder()
    .runtime(&runtime)
    .model(model)
    .rewriter_provider(MyRewriterProvider)   // turn_input_rewriter() + 可选 model()
    .build()?;
```

`Err` = 可见降级（保留原文 + `Failed` 事实）；模型规则
`Provider 模型 ?? Agent 模型`，核心统一叙述。

### 3.3 主题检测（`TopicDetector`，Agent 级）

```rust
impl TopicDetector for MyDetector {
    fn detect<'a>(&'a self, input: TopicDetectInput<'a>)
        -> BoxFuture<'a, Result<Option<String>, MemoryFailure>>
    {
        // input.input / input.history / input.topic / input.model
        Box::pin(async move { Ok(None) })   // None = 保持当前 topic
    }
}
```

核心在轮末调用：写回 `Conversation.topic` → 变化时发
`TopicShifted { from, to }`（首个 topic 的建立不算切换）→ 本轮变更经
管线上下文（`ctx.topic_change()`）传给后续钩子。检测失败保留旧 topic
+ `Failed` 事实，不阻断归档与后台任务。

## 4. 写侧配方

### 4.1 自定义流水线（`MemoryPipeline`）

```rust
impl MemoryPipeline for MyPipeline {
    fn archive_turn<'a>(&'a self, ctx: &'a PipelineTurnContext<'a>)
        -> BoxFuture<'a, Result<(), MemoryFailure>>
    {
        // ctx.input() / frame() / responses() / topic() / topic_change() /
        // reader() / model()；写入口：ctx.spawn(...) / set_topic(...) /
        // report_failure(...)
        Box::pin(async move { Ok(()) })
    }
    fn spawn_task<'a>(&'a self, ctx: &'a PipelineTurnContext<'a>) -> /* 同上 */;
    fn finalize_conversation<'a>(&'a self, ctx: &'a PipelineConversationContext<'a>) -> /* 同上 */;
}
```

- 模板由核心持有：发 `TurnArchived` / `TopicShifted` / `Failed` 事实、
  派生后台任务、会话终结先有界排空再调 `finalize_conversation`。
- 失败语义尽力而为：钩子返回 `Err` → `Failed` 事实，模板继续后续
  步骤（归档失败仍派生后台任务）。
- `spawn` 的后台任务返回 `Result<(), MemoryFailure>`；失败转
  `Failed { moment: Background }`。
- 实现者不接触 `EventSink` / `ConversationTaskSpawner`（不在可见面）。

### 4.2 组件策略（`synonz-layered-memory`）

```rust
LayeredMemoryProvider::builder()
    .summarizer(MySummarizer)          // 批量压实：一批 → 1..N 条条目
    .entity_extractor(MyExtractor)     // 实体 + 关系（蒸馏）
    .context_rewriter(MyRewriter)      // 增强输入
    .build();
```

策略收到核心叙述的模型句柄；提示词随策略；确定性流程（召回评分 /
去重 / 归一化）固定，参数经 `LayeredMemoryConfig` 配置（扁平、带
`l1_` / `l2_` / `l3_` / `recall_` / `topic_` 前缀）。

## 5. 存储接入与 embedding

- 四个存储契约按数据对象划分：`L1MemoryStore` / `L2MemoryStore` /
  `L3MemoryGraphStore` / `L3MemoryVectorStore`；写从数据取 scope、
  读/删带分区（数据自带 scope）。
- 存储契约是**组件的持久化端口**：应用实现它们、组件调用它们；
  它们不是应用写记忆的 API（管理面无创建动词，记忆由会话产生）。
  应用若直接写入（批量导入 / 迁移），属于框架外操作：条目**带上
  正确的 `subject`** 后即被管理面覆盖（L2 的归属随条目持久化，
  存储经 `L2MemoryStore::scopes(subject)` 枚举分区）；未带归属的
  数据不会被管理面索引。L2 契约方法集：`upsert` / `get` / `list` /
  `scopes(subject)` / `remove` / `count`。
- 每个契约有进程内默认；真实后端（Redis / MongoDB / Neo4j / Milvus）
  建议独立成包，只依赖 `synonz-layered-memory`。
- L3 的实体/关系词表由 `L3MemorySchema` 配置（默认中性集；空 =
  不约束）；组件侧归一化统一兜底（大小写不敏感、词表外映射到
  兜底项）。
- 长记忆分区经 `MemoryScopeResolver` 在**使用期**解析（启动期只
  注册解析器；未配置 = 按 subject 推导 `user:<subject>`；返回空 =
  该会话不写/不查 L3）。
- embedding 是组件侧端口（`Embedding`）：默认 `HashEmbedding` 是确定性
  开发实现；生产接入替换为本地模型或托管端点。
- 组件观察面（`LayeredMemoryObserver`）上报分层进展
  （`Compacted` / `Distilled` / `Normalized` / `Skipped`）；可与
  runtime observer 同对象双注册（两个 trait，事件类型不同）。

## 6. 模型角色

| 工厂 | 用途 | 解析规则 |
|---|---|---|
| `MemoryProvider::model` | 上下文管理（读 / 写相位的辅助调用） | Provider 模型 ?? Agent 模型 |
| `RewriterProvider::model` | 输入改写 | 同上（不回落记忆模型） |
| `TopicDetectorProvider::model` | 主题检测 | 同上（不回落记忆模型） |

- 解析在**使用时**进行；交给实现者的句柄是核心的叙述包装（自动
  `Requested` / `Responded`、`round: None`、`purpose:
  ContextManagement`、永不发 `StreamDelta`）。
- 会话终结时无 Agent 在作用域内：`PipelineConversationContext::model()`
  返回 Provider 配置的模型（未配置为 `None`）。
- 没有独立"记忆模型"：记忆跟随上下文。

## 7. 边界与绕行

- **scope 的来源 / 推导 / 注入**属应用层：核心不定义注入点；组件按
  自己的配置声明服务的分区（组件默认把会话 scope 约定为
  `conversation:<id>`，长记忆默认 `user:<subject>`）。
- **scope 转换**（`move` / `promote` / `demote`）为后续加性项；当前
  只支持"按 scope 读 / 批量遗忘"。
- **应用日常新增**：不提供；导入 / 迁移走框架外（自持端口句柄）。
- **0.6.0 数据**：不支持迁移。
- 组件管理面按 id 查找覆盖"存储中带该 subject 归属的会话分区 + 配置的
  长记忆分区"；归属随 L2 条目持久化（`scopes(subject)` 枚举），所以
  组件写入与框架外导入（带 subject）都在覆盖范围内。管理面与组件富面
  （`LayeredMemoryActuator` 的 `l2_memory_entries` /
  `l3_memory_entities` / `l3_memory_relations` /
  `forget_l3_memory_relation`）一律按 subject 收窄。

## 8. 常见坑

1. 装配器返回"背景消息"而不是完整帧 → 模型看不到本轮输入（空帧兜底
   只救空帧，不救"缺用户消息的非空帧"）。帧里必须含本轮用户消息。
2. 在 system 里塞记忆上下文 → 违反角色约定（system 只放 Agent 指令）。
3. 钩子里直接调用模型而不使用 `ctx.model()` → 调用不被叙述，观测
   缺环。
4. 期望 `topic_change()` 在首个 topic 建立时触发 → 建立不是切换。
5. 依赖组件的默认 embedding 做生产召回 → 它是确定性开发实现，不是
   语义模型。
6. 把 `MemoryScope` 当枚举用 → 它是不透明字符串，核心不解释取值。
