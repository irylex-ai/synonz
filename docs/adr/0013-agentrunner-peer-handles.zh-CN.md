# ADR-0013: AgentRunner 平级句柄架构

- 状态: APPROVED（2026-08-29，irylex 人工评审通过）
- 日期: 2026-08-29
- 决策者: irylex（人类确认）
- 性质: 内部架构修正——公开 API 签名不变，纠正 ADR-0011 实现中的
  设计偏差

## Context（背景）

ADR-0011 确立了"收敛句柄家族"设计：`ask` 返回 `Answer`（流式优先）、
`run` 返回 `Run`（结果优先），两者共享同一执行管线。实现中，
`Answer` 被实现为 `Run` 的**过滤视图**——内部包装 `Run`，逐事件扫描
并只放行文本增量，丢弃其余事件。

实际使用（IDE 开发场景）中，人类开发者观察到 `Answer.next()` 内部
经 `Run.next()` 过滤，发现 **Answer 从属于 Run**——与"两个平级消费
面"的设计意图产生偏差：

- **信息不对等**：Run 的消费者能看到全事件流（工具调用、用量、生命
  周期），Answer 的消费者只能看到文本增量——这不是"流式 vs 一次性"
  的差异，是**信息量的差异**；
- **概念从属**：Answer 作为 Run 的投影，破坏了"ask 和 run 是同一个
  执行的两种消费面"的平级关系；
- **设计意图违背**：人类开发者的原始设计意图是"行为不同，但最终结果
  一模一样"——`ask` 流式、`run` 一次性，两者**信息等价**。

## Problem（问题）

Answer 与 Run 之间的主从嵌套关系导致：

1. Answer 的消费者丢失非文本事件（工具调用、用量、生命周期），只能
   通过降级到 Run 获取——违反"两种行为最终结果一模一样"的设计意图；
2. Run 的消费者被迫面对全事件流（包含不关心的文本增量），当只需要
   最终结果时，需要自行过滤；
3. 未来扩展（如 S2c 记忆注入、S3 编排事件）时，Run 与 Answer 的
   差异会进一步扩大，主从嵌套关系的维护成本递增。

## Decision（决策）

### 1. 新增内部类型 AgentRunner

从 `Run` 中提取执行句柄（事件接收器、取消句柄、会话引用、状态缓存）
为独立的内部类型 `AgentRunner`（`pub(crate)`）：

```rust
pub(crate) struct AgentRunner {
    receiver: mpsc::Receiver<AgentEvent>,
    handle: CancelHandle,
    rounds_seen: usize,
    terminal: Option<Result<AgentOutput, AgentError>>,
}
```

`AgentRunner` 持有已启动执行的共享句柄；`Answer` 和 `Run` 各包装一个
`AgentRunner`，**互相不知道对方存在**——平级关系，消除嵌套。

### 2. Answer 与 Run 平级包装 AgentRunner

```rust
pub struct Answer<'a> {
    runner: AgentRunner,       // 平级：不再包装 Run
}

pub struct Run<'a> {
    runner: AgentRunner,       // 平级：不再被 Answer 包装
}
```

两个公开类型各自实现 `Stream` + `Future`，共享同一 `AgentRunner`
实例（共享同一执行）。

### 3. 语义等价原则

**`ask` 与 `run` 的最终结果必须一模一样**——只是消费方式不同：

| 行为 | Answer | Run |
|---|---|---|
| `next()` | 文本增量（`ModelDelta`） | 完整事件（`AgentEvent`） |
| `.await` | `AgentOutput` | `AgentOutput` |
| 消费面 | 流式文本（80% 场景） | 全事件叙事（可观测/审计） |
| 取消 | `cancel()` / drop / timeout | 同左 |

不引入"过滤"概念：Answer 直接从执行体接收事件并转发文本增量，
Run 直接接收全部事件。两者信息等价，只是呈现粒度不同。

### 4. LoopTask 改名为 AgentLoopTask

`LoopTask` 名称歧义（什么循环？），更名为 `AgentLoopTask`——直接
说明角色（运行 Agent 的循环任务）。该类型为私有内部实现，公开 API
零影响。

### 5. AgentRunner 命名原则

内部类型命名应直接说明角色：`AgentRunner` = "执行 Agent 的那个
东西"。比 `RunCore`（过于抽象）更符合框架命名原则（名字直接说明
角色，符合"从对象职责+命名可推断用途"的公共 SDK 标准）。

## Alternatives Considered（备选方案）

### 维持现状（Answer 包装 Run，过滤视图）

- 被否——人类开发者在实际使用（IDE 开发场景）中观察到过滤行为，
  判定为"与设计意图不一致"。
- 过滤丢弃事件导致 Answer 消费者信息不对等，虽然可通过降级到 Run
  获取全事件，但需要放弃 Answer 句柄、改用 Run 句柄——**同一执行
  不能同时以两种粒度消费**。

### 只改命名不改结构

- 概念从属关系不因命名改变；"Answer 是 Run 的投影"的架构事实仍然
  存在，后续维护者仍会困惑。被否决。

### 引入第三个类型（如 `EventStream` 独立于 Answer 和 Run）

- 增加公开 API 面积，且与"双轨 API"设计意图（ask/run 两个入口）
  不符——额外类型是投机性抽象。被否决。

### 泛型抽象（`Execution<'a, F>` 统一 Answer 和 Run）

- 过度抽象——Answer 与 Run 的差异是行为语义（流式 vs 全事件），
  不是类型参数可以表达的；强行统一会引入不必要的类型复杂度。
  被否决。

## Consequences（后果）

### 正面

- Answer 与 Run 平级，消除了概念从属——"两种行为，同一结果"的
  设计意图在结构上得到表达；
- 消除嵌套后，Answer 和 Run 各自直接对接执行体，减少一层间接；
- AgentLoopTask 命名消除了歧义。

### 成本与义务

- 内部重构约 200 行（Answer、Run、AgentRunner 三个类型的实现）；
- 全量测试回归（120 测试）确保行为不变；
- 未来扩展（S2c 记忆注入、S3 编排事件）需同时考虑 Answer 和 Run
  两个消费面——平级关系要求同步演进。

### 关联

- 修正 ADR-0011 的"收敛句柄家族"实现（Answer 包装 Run 的嵌套
  结构）；
- 依赖 ADR-0012（触发策略体系在 run_post_turn_flows 中执行，经
  AgentRunner 的 sender 产出事件）。

## 推迟项

| 项 | 触发条件 |
|---|---|
| AgentRunner 的 SweepStale 集成 | M11 空闲超时实现需要 |
| Answer 的 cancel() 显式方法 | 已实现，无变化 |
| 事件流过滤策略（Answer 是否可选择性地透传非文本事件） | 用户需求驱动 |
