# ADR-0003: 事件模型与可观测契约

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

四支柱中的"内生可观测"（P6）与"行为可控"（P2）要求观察成为框架的结构
性能力而非外挂。S1 锚定场景（ADR-0002）需要确定开发者与 agent 执行的
主交互方式，以及事件体系的类型形态。

S1 用户心智词汇表确认为四个概念：`Model` / `Tool` / `Agent` / `AgentEvent`。
其余概念（Context/Session/Memory 等）按场景推迟。

## Problem（问题）

1. 可观测性如何成为结构性实现而非可选外挂？
2. 事件体系的类型结构如何组织（关注点分离 vs 扁平混合）？
3. 生命周期事件的最小集是什么？"工作阶段"要不要显式事件？
4. 辅助性模型调用（摘要、意图识别）在事件流里如何定位？
5. 公开类型的命名体系如何建立？

## Decision（决策）

### 主交互模型：双层 API——事件流为核，便捷层为壳

```rust
// 核心层：run() 返回事件流（含取消语义，见 ADR-0004）
let mut run = agent.run(input).await?;
while let Some(event) = run.next().await { /* 消费 AgentEvent */ }

// 便捷层：ask() 一行取最终结果——用事件流实现，只是壳
let answer = agent.ask(input).await?;
```

关键结构性保证：便捷层必须构建在事件流之上。这构成完备性证明——事件流
是唯一信息通道，不存在绕过它的路径；简单用法与深度观测共用同一核心。
可观测性因此无法被绕过（P6 结构性实现）。

### 事件类型：两级 enum，顶层为关注点分类

```rust
#[non_exhaustive]
enum AgentEvent {
    Lifecycle(LifecycleEvent),
    Model(ModelEvent),
    Tool(ToolEvent),
}

#[non_exhaustive]
enum LifecycleEvent {
    Started   { input: AgentInput },
    Completed { response: AgentOutput },
    Failed    { error: AgentError },
    Cancelled { reason: CancelReason },
}

#[non_exhaustive]
enum CancelReason {
    UserRequested,   // 用户主动取消
    Timeout,         // with_timeout 触发
    Parent,          // 上游取消传播（S3 预留语义，v1 不产生）
}

#[non_exhaustive]
enum ModelEvent {
    Requested {
        purpose: CallPurpose,
        messages: Vec<Message>,      // canonical Message，见 ADR-0006
    },
    StreamDelta { delta: ModelDelta },
    Responded {
        message: Message,
        usage: TokenUsage,
    },
}

#[non_exhaustive]
enum CallPurpose {
    Reasoning,          // 推理循环（v1 框架唯一产生的值）
    ContextManagement,  // S2 摘要/记忆时才有
    Classification,     // S3 意图识别/路由时才有
}

#[non_exhaustive]
enum ToolEvent {
    CallRequested { call: ToolCall },                      // call 内含 CallId
    CallCompleted { call_id: CallId, result: ToolResult },
}
```

语义说明：两级 enum 是标签联合（tagged union）而非继承——顶层按关注点
分类，各类内聚自己的变体；单一有序叙事流 + 封闭集合 + 穷尽匹配。

### 生命周期最小集与不变量

- 不变量：每个 run 的**最后一个事件**是 `Lifecycle` 终止变体
  （`Completed` / `Failed` / `Cancelled` 三选一，恰好一次），随后流关闭；
- `Started` 携带 `AgentInput`——事件流自足可回放的起点；
- **"工作阶段"不是事件，是区间**：Started 之后、终止事件之前的活动状态
  由 Model/Tool 事件流表征；
- **轮次（round）用消费端推导，不存储冗余事件**：每个
  `ModelEvent::Requested { purpose: Reasoning }` 即一轮的开始，框架提供
  推导式辅助 API（如 `rounds()`）；
- 事件模型对 agent 内部形态不可知：单次调用 agent、循环 agent、纯工作流
  agent 产生各自合法的叙事序列。

### 辅助模型调用的定位：purpose 区分 + no-magic

原则一：**run 内发生的一切模型消耗都出事件**——隐藏的模型调用意味着
无法解释的延迟与成本，直接违反 P2。

原则二（no-magic）：**v1 框架核心不内置任何自动摘要、自动意图识别、
隐藏 prompt 调整**。run 里的模型调用只有两个来源：推理循环本身、用户
的显式代码。摘要/记忆管理是 S2 功能，意图识别/路由是 S3 功能；当它们
作为框架功能出现时，按原则一带 `CallPurpose` 出现在事件流里。

轮次推导只统计 `Reasoning` 调用，辅助调用不打断轮次计数。

### 载荷设计：自足性原则

每个事件携带理解（和响应）该步骤所需的一切，无需查询外部状态：
`ModelEvent::Requested` 带全量 messages、`Responded` 带 `TokenUsage`、
`ToolEvent` 带调用与结果。这是"用载荷重量换回放能力"的显式权衡：长对话
中每轮携带全量 messages，录制全流为 O(n²) 字节量。若未来实测成瓶颈，
优化发生在录制层（增量压缩），而不是削薄事件。

### 可序列化

全部事件类型可序列化（serde tag 表示，形如
`{"type": "model.requested", ...}`），同时服务两个需求：录制/回放（P6、
确定性测试的基础）与传输（嵌入服务时的 wire 格式）。

### 命名体系：Model 根词 + 纪律性派生

命名四规则：

1. **根词只出现在高频处**：`Model`（trait 名，见 ADR-0006）与 `ModelEvent`
   使用根词，派生物不重复堆前缀；
2. **禁止前缀堆砌**：字段已活在语境中，类型名不带冗余语境——是
   `CallPurpose` 而非 `ModelCallPurpose`，是 `TokenUsage` 而非
   `ModelTokenUsage`；
3. **朗读测试**：每个名字在 builder 方法、泛型约束、match 分支三个
   使用点读起来自然；
4. **语境消歧**：`Model` 在 agent 框架 prelude（`Agent`/`Tool`/`Model`
   并列）中不会误解为领域模型，且与行业口语一致（"call the model"）。

### v1 事件的单向性

v1 事件是单向的（agent → 消费者），只观测不控制。通过事件干预 agent
行为（如工具调用审批）属人机协作场景，后置。

## Alternatives Considered（备选方案）

### 主交互模型

- **纯事件流接口**（无便捷层）：最纯粹但简单场景也必须写循环，人体工学
  不足。被否决。
- **请求/响应为主 + 事件订阅可选**：可观测性沦为二级公民，结构性背叛
  P6，重蹈现有框架覆辙。被否决。

### 事件类型结构

- **扁平 enum**（所有变体混在一层）：三类关注点（生命周期结果语义、
  模型交互、工具循环）混杂，随场景增长无限膨胀，结构不可见。被否决。
- **trait 对象事件**（完全可扩展）：失去穷尽性检查，match 无法被编译器
  兜底；开放集合与"框架定义 run 叙事"的单一来源原则冲突。被否决。
- **多通道分流**（生命周期/模型/工具各一个通道）：摧毁单一有序叙事流，
  跨通道顺序无法保证，回放需重新合并。被否决。

### 生命周期事件

- **`Working`/`Loop` 阶段事件**：对 S1 无信息增量（Started 与首次模型
  调用之间无事件间隙）；且"Loop"假设了 agent 是循环结构，对单次/工作流
  形态的 agent 不成立。被否决——工作状态由活动事件表征，轮次用推导 API。
- **轮次存储为事件**（`LoopIteration` 等）：轮次可从 `Requested` 无歧义
  推导，存储冗余数据违反最小自足集纪律。被否决——提供消费端推导 API。

### purpose 字段时机

- **S2 有第二个 purpose 时再加字段**：给已发布的变体加字段会破坏下游
  每一个匹配模式（破坏性变更）；现在加字段（v1 只有 `Reasoning` 值）+
  `#[non_exhaustive]`，后续加变体是非破坏性演进。总成本更低，被采纳。

### 命名体系

- **`Llm` 家族**（LlmClient/LlmEvent）：语义诚实但缩写大小写形态不佳
  （人类评审明确否定其观感）。被否决。
- **`Chat` 家族**（ChatModel/ChatEvent）：贴合 API 术语但"chat"偏窄——
  工具调用循环、摘要调用不是"聊天"。被否决。
- **`Completion` 家族**（CompletionModel/CompletionEvent，Rig 先例）：
  精确但冗长、行话感重，builder 方法与泛型约束朗读测试不佳。被否决。
- **`Model` 家族 + 纪律派生**：行业通用语、短、语境无歧义。被采纳。

## Consequences（后果）

### 正面

- 可观测性为结构性实现：事件流是唯一信息通道，无法绕过；
- 单一有序叙事流支持完整回放、审计、调试，并为确定性测试奠基；
- 两级结构让后续增长（S2 会话类别、S3 编排类别）局部化；
- 命名体系为公开 API 建立了可执行的规则而非个案决定。

### 成本与义务

- 事件载荷可能较重（自足性权衡，O(n²) 录制体积已在明面）；
- 序列化格式在公开发布后成为事实 wire 契约，需要稳定性意识；
- 事件枚举的增长只经 `#[non_exhaustive]` 通道，破坏性变更需显式声明；
- 轮次推导 API 是框架义务（文档 + 实现）。

### 风险

- `ModelDelta` 的具体形态（v1 文本增量）在多模态时代需扩展——依赖
  `#[non_exhaustive]` 演进；
- 事件粒度（如工具执行进度 `ToolEvent::Progress`）可能在长时工具场景
  被要求提前——按场景触发，当前刻意后置。

### 关联

- 依赖 ADR-0001（四支柱）、ADR-0002（S1 锚定）；
- 取消事件与终止语义由 ADR-0004 实现；
- `Message` 载荷类型定义于 ADR-0006；
- `AgentOutput` 由 ADR-0007 定义。
