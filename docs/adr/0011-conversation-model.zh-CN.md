# ADR-0011: S2 会话模型

- 状态: APPROVED（2026-08-29，irylex 人工评审通过）
- 日期: 2026-08-29
- 决策者: irylex（人类确认）

## Context（背景）

S2（有状态会话）是 ADR-0002 明确定义的后置场景，此刻激活。其设计受以下已批
决策约束：ADR-0007（Agent 保持无状态，会话状态是 S2 的事）、ADR-0008
（新场景新 crate 原则）、ADR-0009（G3 循环 Hook 点 / G4 契约粒度 / G5 跨插件
状态流在 S2 激活；契约随场景晋升）、ADR-0010（先定 80% 高层用法，High Level
由 Low Level 组合）。

讨论历程经过多轮模型推翻与重建（完整记录见 Alternatives），最终由人类开发者
主导收敛为 OO 风格的内聚设计：参数对象模式 + 实体模型。本 ADR 同时承担
M10（v1 API 演进）的决策依据——两者互为前提，合并定稿。

## Problem（问题）

在无状态 Agent 基座（一个 Agent 可并发服务任意多场对话）之上，如何承载多轮
会话？具体难点：

1. 多轮的历史如何到达 Agent——Rust 无方法重载，两形态入参如何表达；
2. 会话对象的角色定位——行为主体还是数据实体；
3. 流式与一次性结果的 API 关系——v1 的 ask（阻塞拿结果）语义直觉性差；
4. Rust 所有权模型与 OO 习惯（对象图、自引用）的冲突如何化解；
5. 会话的持久化与恢复边界。

## Decision（决策）

### 决策 1：实体模型

- **Agent = 行为实体**（能力 + 配置，无状态共享体）；
- **Conversation = 数据实体**（一场多轮对话的聚合：身份 + 轮次序列）。
  Conversation 与 Agent 解绑、可序列化、可移交——**多个 Agent 可续同一场
  会话**（S3 多 Agent 编排的自然地基）。

### 决策 2：TurnInput 参数对象（统一入参）

历史上下文不通过方法名区分（Rust 无重载）、不通过绑定持有，而**装进输入
对象传递**：

```rust
pub struct TurnInput<'a> {
    input: AgentInput,
    conv: Option<&'a mut Conversation>,
}

// 三个来源（impl Into<TurnInput<'_>> 糖）：
conv.turn_input("北京天气？")   // 会话轮
"1+1=?"                        // 一次性：From<&str>
AgentInput::new(...)            // 结构化：From<AgentInput>
```

`&mut conv` 借用使同会话轮次天然串行化（类型系统拒绝并发轮次）。

### 决策 3：ask / run 双语义 + 同构句柄家族

| 入口 | 主语义 | 返回 | 流式面 | 结果面 |
|---|---|---|---|---|
| `agent.ask(turn_input)` | **流式优先**（提问的直觉） | `Answer` | `next() -> Option<ModelDelta>` | `.await -> Result<AgentOutput>` |
| `agent.run(turn_input)` | **结果优先**（执行任务） | `Run` | `next() -> Option<AgentEvent>`（全事件流） | `.await -> Result<AgentOutput>` |

- 两者入参签名一致（决策 2），行为一致，仅流的"分辨率"不同；
- 句柄家族共同能力：`next()` / `.await` / `cancel()` / `with_timeout()`；
- **语义翻转（M10）**：v1 的 `ask` 阻塞拿结果、`run` 事件流——现 ask 主角色
  变为流式（`Answer` 流式面），run 主角色变为取结果（`.await`）；全事件流是
  run 的第二面（迭代时显现）。`agent.ask(x).await?` 等既有写法零破坏。
- 取消四入口汇一信号（ADR-0004 架构不变）：`cancel()` 显式（新增主入口）、
  drop RAII 兜底、`with_timeout`、外部 `CancellationToken`。

### 决策 4：Turn 结构与按轮组织

```rust
pub struct Turn {
    pub input: AgentInput,        // 本轮用户输入
    pub messages: Vec<Message>,   // 本轮 run 产生的全部 canonical 消息
                                  // （含中间工具往返——下一轮必须重放的完整上下文）
    pub output: AgentOutput,      // 最终结果快照 { message, usage }
}
```

- Conversation 内部按轮组织；对外双视图：`messages()`（摊平，喂模型）与
  `turns()`（轮次，审计/截断）；
- `truncate_last(n)` 按整轮切——永不产生半轮上下文；
- **output.message ≡ messages 最后一条 assistant 消息**（有意冗余：类型统一
  `AgentOutput` 贯穿 Answer.await / Turn / Completed 事件，O(1) 取结果）；
- 不存增量数组（增量是过程视图，完成后信息并入最终消息——P1 不存可推导数据）。

### 决策 5：落账时序（单次组装、同源分发）

```
t0  构造 TurnInput → agent.ask → 立即返回 Answer（执行后台启动）
t1  流式阶段：碎片三路——事件流 / next() / 执行体内部 messages 缓冲
t2  流完成（执行体收尾，唯一组装点）：
    ① AgentOutput 组装（唯一一次）
    ② 包成 Turn
    ③ 发 Completed 事件（携 ①）
    ④ 经 TurnInput 携带的 &mut conv 自落账（push_turn）——执行体内部调用
t3  answer.await 从 Completed 解析 ①（此刻会话已更新）
```

历史写入语义：**仅 Completed 入史；Cancelled / Failed 不入史**（历史停在轮前
状态，可推理）。

### 决策 6：Conversation 行为面（信息专家原则）

只碰自己消息的行为归实体；要动模型的行为归 Agent 侧：

```rust
impl Conversation {
    // 构造与身份：new()（自动生成 id）/ with_id(id) / id()
    // 读：messages() / turns()
    // 写：turn_input(text) -> TurnInput；push_turn(turn)
    //      —— push_turn 正常流程由执行体调用（用户不可见）；
    //         公开仅为手工构造通道（测试/导入/S2c 记忆注入），文档标注
    // 管理：truncate_last(n) / clear()
    // 持久化：export() / import()（serde）
}
```

动模型的压缩/摘要 = **S2b ContextManager**（作用于 `&mut Conversation` 的插件
契约，ADR-0012），不进实体。

### 决策 7：会话身份与恢复

- Conversation 是**有身份的实体**：`new()` 自动生成 id（时间戳+计数器，文档
  注明非加密唯一性）、`with_id(id)` 接受应用 ID 体系（工单号/用户会话键）；
- 恢复 = 应用用自有存储（Redis/DB）以 id 为键存取 `export()`/`import()`
  序列化产物；
- **ConversationStore 契约推迟**：框架内全局仓库 = 隐藏全局状态（违反 P2）；
  注入式仓库契约（`Conversation::for(&store, id)` 优雅形态）按 ADR-0009
  晋升标准在 S2a 落地后评估。

### 决策 8：system_prompt 留 Agent

人格随 Agent 走；多 Agent 续会话时新 Agent 的人格生效。Conversation 实体
只存轮次。

### 决策 9：不加会话级事件类别

轮次 = 一次 run，现有事件流（Started→…→Completed）已构成完整叙事。会话级
事件类别（ADR-0003 预告的增长点）在真实需求出现前不引入。

### 决策 10：M10（API 演进）并入本 ADR 实施

- 新增 `Answer` / `Run` 句柄类型（`RunStream` 重命名为 `Run`）；
- `ask` 语义翻转（流式优先）；`cancel()` 显式方法；
- `agent.ask` / `agent.run` 入参改为 `impl Into<TurnInput<'_>>`；
- 破坏性变更 pre-1.0 内接受，按 release.md 流程显式标注。

## Alternatives Considered（备选方案）

### 会话命名

- `Session`：与 Web/HTTP 会话（tower-sessions 等）正面冲突，否决；
- `Thread`：OpenAI Assistants 用词，但 Rust 中 `std::thread` 已占据，硬伤，
  否决；
- **`Conversation`**：语义直白、无冲突、与 canonical Message 体系贴合，采纳。

### 会话的角色定位（三版模型推翻）

- **模型一：会话行为主体**（`conv.ask()` 持有 Agent 引用）：会话绑定单一
  Agent，多 Agent 续会话不可能；"对话记录"充当"执行者"角色错位，推翻；
- **模型二：ask/conversation 双平行入口**：概念暧昧（"既然 agent.ask 能问，
  会话算什么"），且 Rust 无重载迫使双动词（ask/converse 或 ask/ask_once），
  推翻；
- **模型三：with() 绑定式**（`agent.with(&mut conv).ask(x)`）：行为一致但有
  多余一跳，且绑定对象多一类型，推翻；
- **模型四（采纳）**：TurnInput 参数对象——上下文装进输入，ask/run 永远
  同名同签名，"每次调用行为完全一致"（人类开发者主导的设计）。

### 在途轮次 vs 存档轮次（Rust 所有权约束）

Java 的自引用设计（Turn 持 Conversation 引用、Conversation 存 Turn——GC
对象图）在 Rust 中被借用检查器禁止：被存储者不能借用其容器。因此"在途
轮次"（持 `&mut conv`、完成时自落账）与"存档轮次"（纯数据）必须分离。
两个落地方案：

- 方案 A（采纳）：TurnInput + Answer 承载在途语义（Answer 跨执行期存活、
  携带引用、执行体收尾自落账），Turn 仅作存档数据——80% 流程用户无感知；
- 方案 B：显式在途 `Turn` 对象（`conv.begin_turn(x)` + `turn.commit()`）与
  存档 `TurnRecord` 两类型——最贴 Java 形态，但多一类型、主流程多一行
  仪式，按 ADR-0010 否决。

### 落账方法命名

- `turn_output`：读起来像 getter（方向歧义），否决；
- **`push_turn`**：容器惯例（Vec::push 同族），落账方向无歧义，采纳。

### 其他命名/形态

- `for_turn_input` → **`turn_input`**（方法名 = 产物名，`iter()`→`Iter` 惯例）；
- 会话级 `for(conversationId)` 静态还原 → 仓库缺失即隐藏全局状态，拆分为
  id+export/import（S2a）+ ConversationStore 契约（推迟）；
- 轮次扁平存储 → 按轮组织（截断安全）；
- `Answer.with_timeout().next()`（分片级超时）与 `agent.with_timeout(d)`
  （Agent 级默认预算）两级可选健壮性，采纳。

## Consequences（后果）

### 正面

- 统一心智模型：ask/run 同名同签名，上下文由输入对象携带；
- OO 内聚：Agent 行为实体 / Conversation 数据实体，职责从命名可推断；
- 多 Agent 续同一会话成为天然能力（S3 编排地基）；
- 实体身份 + 序列化让会话恢复成为应用侧一行代码；
- 流式（Answer）与结果（run.await）各归其位，直觉与 API 对齐。

### 成本与义务

- **M10 破坏性变更**：ask 语义翻转、新类型 Answer/Run、入参改为 TurnInput
  ——pre-1.0 接受，按 release.md 显式标注；
- TurnInput 生命周期泛型使签名复杂化（用户侧由 `impl Into` 糖缓解）；
- 执行体须持有 `&mut conv` 跨执行期（实现复杂度）；
- push_turn 公开语义需文档约束（正常流程勿用）。

### 推迟项（触发条件显式记录）

| 项 | 触发条件 |
|---|---|
| ConversationStore 契约（`Conversation::for(&store, id)`） | S2a 落地后按晋升五标准评估 |
| S2b ContextManager（G3 循环 Hook 点） | ADR-0012 |
| S2c Memory（G4 粒度 + G5 工具访问会话状态） | ADR-0013 |
| 种子上下文 / 历史导入（from_messages） | S2c 记忆注入设计时 |
| G6 契约版本兼容 | semver 政策制定时 |

### 关联

- 依赖 ADR-0002（S2 场景）、ADR-0007（无状态 Agent）、ADR-0009（契约
  随场景晋升）、ADR-0010（80% 高层优先）；
- 实施第一步 = M10（API 演进）+ S2a（Conversation/Turn/TurnInput/Answer/Run）。
