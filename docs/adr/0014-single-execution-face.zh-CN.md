# ADR-0014: 单一执行面——`run` → `Execution` 与叙事事件流

- 状态: APPROVED（2026-09-07，irylex 人工评审通过）
- 日期: 2026-09-07
- 决策者: irylex（人类确认）
- 性质: 公开 API 重构（破坏性，0.2.0）——执行面收敛为单一入口，
  可观测性职责解耦至独立的 Observer 契约（ADR-0015，另立讨论）

## Context（背景）

ADR-0013 将 `Answer` 与 `Run` 修正为平级句柄（各自包装
`AgentRunner`），消除了结构上的从属嵌套。但**信息不对等依然存在**：
`Answer` 只透传文本增量，工具调用、状态流转只有 `Run` 能看到。

在 IDE 产品对标讨论（OpenCode 聊天框）中确认了两个关键事实：

1. **产品 UI 需要的是输出侧完整叙事**——文本逐字出现、工具卡片实时
   出现/落定、状态流转、终态结果。当前 `ask` 面拿不到工具与状态，
   `run` 面拿到的却是含输入装配载荷的全量事件——两个面都答不对题。
2. **可观测性不能绑定在消费面上**。LLM 是概率的：同样的输入不会有
   同样的输出，"用 run 再执行一遍来观测 ask"不成立。观测必须与执行
   同时发生——它是执行的**旁路属性**，正确形态是独立的旁路订阅契约
   （Observer，社区可扩展，AGENTS.md §16 的可观测性要求），而不是
   第二种驱动方式。

由此推论：当执行面收敛为单一入口（叙事完整）+ 观测解耦为旁路契约
（全量事件）后，`run` 作为独立执行面**不再有存在价值**——它的全部
真实场景（测试断言全量事件、trace 工具）都被 Observer 捕获替代。

同时，`ask` 的命名存在语境错位：提问语境对框架的中心场景（IDE 的
todo 任务执行、后台任务、编码指令）是错位的——`agent.ask("fix the
lint errors")` 语义别扭。业界动词调查：`run` 是 Agent 执行的事实
标准（OpenAI Agents SDK `Runner.run`、pydantic-ai `agent.run`、
AutoGen、smolagents、Semantic Kernel）。`ask` 当初存在的理由（与
`run` 区分）随 `run` 废弃而消失。

## Problem（问题）

1. `ask` 面输出侧信息缺口：工具事件、状态流转、终态事件不可见，
   纯流消费者（UI）无法只靠 `ask` 渲染完整叙事；
2. `run` 面的"可观测通道"定位不成立：观测不能靠第二次执行（概率性），
   录制/回放/审计本质是当次执行的旁路捕获，不需要第二种驱动方式；
3. 两个执行面冗余：ask + Observer 覆盖一切后，run 无独立价值——
   双面维持是公开 API 面积的净浪费；
4. 命名：`ask` 提问语境错位；`Run` 动词当名词、无指代感
   （`run() -> Run` 重复），公开 API 编程体验差。

## Decision（决策）

### 1. 单一执行面

```rust
Agent::run(input)             -> Execution<'_>
Agent::run_with(input, token) -> Execution<'_>   // 签名沿用
```

`Execution` 三合一：叙事流 + 终点 Future + 控制器。

```rust
Execution: Stream<Item = ExecutionEvent>
         + Future<Output = Result<AgentOutput, AgentError>>
         + cancel() / with_timeout() / rounds()
```

### 2. ExecutionEvent：输出侧叙事等价

```rust
enum ExecutionEvent {
    Delta(ModelDelta),                  // 文本增量
    ToolRequested(ToolCall),            // 工具卡片出现
    ToolCompleted { call_id, result },  // 工具卡片落定
    Failed(AgentError),                 // 失败终态
    Cancelled(CancelReason),            // 中断终态
    Completed(AgentOutput),             // 成功终态，携带结果
}
```

- **叙事等价原则**：执行面的流覆盖"输出了什么"的全部信息（文本
  增量、工具活动、状态流转、终态结果）——产品消费者不需要第二个
  通道补齐叙事；
- **流自足原则**：`Completed` 携带 `AgentOutput`，纯流消费者不需要
  补一次 `await`；`await` 消费者拿到相同结果（终态缓存机制保证两种
  方式一致，且与旧 `AgentEvent::Completed { response }` 的携带语义
  连续）；
- **终态不变量延续**：终态事件（`Failed` / `Cancelled` / `Completed`）
  必为最后一项，之后流关闭；
- **不含输入侧载荷**（`Requested` 装配消息、`Responded` 快照）——
  那是 Observer 观测契约（ADR-0015）的领地。

### 3. 移除清单

- `Agent::ask`（方法删除）；
- `Answer`（类型删除）；
- 旧 `Run`（裸事件执行面，类型删除——职责移交 Observer）。

`run_with(input, token)` 签名不变，继续作为外部取消令牌入口
（此前"ask 是否需要 ask_with"的挂起问题就此消解）。

### 4. 命名论证（依 coding.md §5 Naming Discipline）

- **方法 = 动词，类型 = 指代名词**：`run` 是业界执行动词标准；
  句柄 `Execution` 指代"一次进行中的执行"，动词→名词的落差是
  `tokio::spawn → JoinHandle` 的同构；
- **否决 `Run` 作类型名**：动词当名词、`run() -> Run` 重复、无指代；
- **否决 `AgentRun`**：同样的别扭加前缀；
- **否决 `Answer` 回归**：提问语境对任务执行错位（废弃 `ask` 的
  同一理由）；
- **否决 `Response`**：与 `AgentOutput`、`ModelEvent::Responded`
  概念撞车；
- **否决 `Turn`**：与轮次记录数据类型撞名（活句柄 vs 死记录）；
- **派生命名**：`Execution → ExecutionEvent`，机械可推导。

概念位分布零撞车：`Execution`（进行中的执行）/ `AgentOutput`
（终点产物）/ `Turn`（落账记录）/ `AgentRunner`（内部机器）/
`Observer`（旁路观测，待 ADR-0015）。

### 5. 执行面不承载观测职责

全量 `AgentEvent`（含输入侧载荷）经 Observer 旁路契约提供——观测
挂在执行的发射点，无论何种驱动方式都可见。本 ADR 只确立前提；
trait 形态、注册层级、投递语义、故障隔离由 ADR-0015 讨论后另行
起草（不预写设计）。

## Alternatives Considered（备选方案）

### 保留双面（ask 叙事化 + run 观测面）

- 被否——观测不能靠重执行（LLM 概率性）；run 的全部真实场景
  （测试全量断言、trace 工具）被 Observer 捕获替代后，run 是
  公开面的净冗余，违反最小公开面原则。

### Answer 保留并补透传（不删 run）

- 被否——"补透传"即本 ADR 的叙事等价设计，但保留 `Answer` 名字
  意味着保留 ask 提问语境的错位；且双面继续维持两套公开类型。

### 执行面直接暴露裸 `AgentEvent`

- 被否——`Requested` 携带完整装配消息（输入侧载荷），对产品消费
  者是噪音与负担；叙事面应输出侧自足。输入侧事件是观测契约的领域。

### `ExecutionEvent` 不含 `Completed` 变体（流尾表达成功）

- 被否——强制纯流消费者多调一次 `await`；双通道不是冗余而是两种
  消费风格（流式渲染 vs 一次性结果）各自的需要，且与旧设计连续。

### 句柄命名 `Run` / `RunEvent`

- 被否——动词当名词、无指代感、调用点 `let mut run = agent.run()`
  重复别扭；详见决策 4 的命名论证。

## Consequences（后果）

### 正面

- 公开面收敛：一个执行入口 + 一个观测扩展点，职责清晰；
- 产品（IDE）全程用 `run`：叙事完整（输出侧零缺口）、流自足；
- 命名指代明确、业界对齐、概念位零撞车；
- 迁移连续性好：`agent.run(input).await` 的 await 语义跨版本不变，
  `ask` 一次性用户只需纯改名。

### 成本与义务

- **0.2.0 破坏性发布**：删除 `Agent::ask` / `Answer` / 旧 `Run`；
  迭代消费者从 `AgentEvent` 匹配改为 `ExecutionEvent` 匹配；
- 迁移清单（已核）：17 个 `agent.run` 测试调用点、19 个 `ask`
  调用点、5 个示例文件、agent.rs 模块文档示例、`handles.rs` 重写；
- **必须与 ADR-0015 同版本（0.2.0）发布**：否则出现全量事件真空期
  （测试无法断言 `Requested` 载荷、`events.rs` 示例无处安放）；
  实施顺序 0014 → 0015，发布捆绑；
- 全量测试回归；
- 已知限制：执行面消费者无法基于输入侧事件做流中反应（如
  "Requested 载荷超限即取消"）——Observer 是旁路无句柄。此为
  推测性场景（YAGNI），真实需求出现时再评估策略钩子。

### 关联

- 取代 ADR-0013 确立的双面格局；`AgentRunner` 内部结构保留沿用；
- 前置 ADR-0011（会话模型）、ADR-0012（触发策略经 sender 产出
  事件，路径不变）；
- ADR-0015（Observer 契约）以本 ADR 的"执行面不承载观测"为前提，
  待专项讨论后起草。

## 推迟项

| 项 | 触发条件 |
|---|---|
| 纯文本适配器（如 `execution.text()` 返回纯增量流） | 用户明确需求（本次讨论两次搁置，先 YAGNI） |
| 流中反应式策略（基于输入侧事件的即时取消等） | 真实需求出现时评估 |
