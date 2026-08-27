# ADR-0006: Model 抽象与规范消息形态

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

LLM 是 Synonz 的一等公民（ADR-0001），v1 需要为"与模型交互"定义核心
契约。ADR-0002 确定双 provider（OpenAI 兼容 + Anthropic）以验证抽象
普适性。两家 API 存在真实差异：

| 方面 | OpenAI | Anthropic |
|---|---|---|
| system prompt | `role:"system"` 消息 | 顶层 `system` 参数，不在 messages 里 |
| 工具调用 | `tool_calls: [{id, function:{name, arguments(JSON **字符串**)}}]` | `content: [{type:"tool_use", id, name, input(对象)}]` |
| 工具结果 | `role:"tool"` 消息 + `tool_call_id` 字段 | **合并进 user 消息**的 `tool_result` 块 + `is_error` |
| 工具错误标记 | 无原生字段 | 原生 `is_error: true` |

S1 有两种真实用法：交互式（UI 要 token 流）与 headless（`ask()` 只要
结果）。命名体系（ADR-0003）确定 trait 名为 `Model`。

## Problem（问题）

1. Model trait 的方法形态：流式与非流式是一个方法还是两个？
2. 消息如何表示：规范形态如何定义才能隔离 provider 差异？
3. 请求参数、持有方式、错误与重试的策略边界在哪？

## Decision（决策）

### 单方法契约：`stream()`，非流式是退化实现

```rust
trait Model: Send + Sync {
    fn stream(&self, request: ModelRequest)
        -> BoxFuture<'_, Result<ModelStream, ModelError>>;
}

type ModelStream = BoxStream<'static, ModelStreamItem>;

#[non_exhaustive]
enum ModelStreamItem {
    Delta  { text: String },   // v1 仅文本增量；工具调用增量不做，
                               // 工具调用在 Finish 中完整出现
    Finish { message: Message, usage: TokenUsage },  // 恰好一次，终结项
}

// 便利函数：非流式 = fold 流直到 Finish（derive 出来，不进 trait）
async fn complete(model: &dyn Model, req: ModelRequest)
    -> Result<(Message, TokenUsage), ModelError>;
```

- **非流式不是缺失的能力，是 stream 的退化实现**：不支持流式的 provider
  内部用非流式 HTTP 调用实现，只 yield 一个 `Finish`；流式 provider
  （SSE）逐项 yield；
- `Finish` 恰好一次且为终结项——与事件模型（ADR-0003）的终止不变量
  同构；
- `complete()` 是派生便利，不占 trait 表面。

### 规范消息形态（canonical Message）

核心层定义自己的规范形态，provider 适配层负责双向互转——所有 provider
怪癖封死在翻译层，agent 循环、事件、Tool 桥接只认识规范形态。

```rust
pub struct Message {
    pub role: Role,
    pub blocks: Vec<ContentBlock>,   // 块序列，理由见下
}

#[non_exhaustive]
pub enum Role { System, User, Assistant, Tool }

#[non_exhaustive]
pub enum ContentBlock {
    /// 文本块
    Text { text: String },

    /// 模型发起的工具调用——只出现在 Assistant 消息中
    ToolCall {
        call_id: CallId,             // provider 生成，用于配对
        name: String,
        arguments: Value,            // JSON 对象
    },

    /// 工具执行结果——只出现在 Tool 消息中
    ToolResult {
        call_id: CallId,
        result: ToolResult,          // Ok/Err，见 ADR-0005
    },
}
```

**为什么是块序列而不是 string content**：

1. 并行工具调用：一条 Assistant 消息带 N 个 `ToolCall` 块——string
   表达不了；
2. 混排内容是真实行为：模型边说话边调工具（`Text` + `ToolCall` 混排）；
3. 多模态是既定方向：User 消息 `[Text, Image]` 混排——blocks 天然容纳，
   `#[non_exhaustive]` 加块型非破坏。

**合法性不变量**（agent 循环维护，provider 翻译层双向保持）：

1. `ToolCall` 块只出现在 `Assistant` 消息；
2. `ToolResult` 块只出现在 `Tool` 消息；
3. 每个 `ToolResult.call_id` 必须引用先前出现的 `ToolCall.call_id`。

**一个规范形态的对话示例**：

```
[System: Text("你是天气助手")]
[User: Text("北京天气怎么样？")]
[Assistant: Text("让我查一下。"), ToolCall{call_id:"x1", weather, {city:"北京"}}]
[Tool: ToolResult{call_id:"x1", Ok: Text("晴，28°C")}]
[Assistant: Text("北京今天晴，28 度。")]
```

### 请求与参数

```rust
struct ModelRequest {
    messages: Vec<Message>,
    tools: Vec<ToolSpec>,      // ToolSpec { name, description,
                               //   parameters_schema }——与 ADR-0005 对接
    params: ModelParams,
}

#[non_exhaustive]
struct ModelParams {
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}
```

provider 特有配置（base URL、鉴权、超时等）在 client 构造期给定，
不进每请求参数——请求参数保持最小充分集。

### 持有方式与错误

- agent 持有 `Arc<dyn Model>`：配置跨 run、跨并发 run 共享（配合
  ADR-0007 的无状态 Agent）；
- `ModelError` 小分类 + `#[non_exhaustive]`：Transport / Api /
  RateLimited / InvalidRequest——错误可行动，不吞上下文。

### 无隐藏重试

默认**不做任何自动重试**。重试是策略：隐藏重试 = 隐藏延迟与成本
（P2）。需要重试的用户自行包装（v1 不提供重试 utility，后置决策追踪）。

### 取消契约

`stream()` 返回的 future 被 drop = 放弃请求；provider 实现必须保证
drop 安全（连接清理），见 ADR-0004 分级表"模型请求"行。

## Alternatives Considered（备选方案）

### 方法形态

- **双方法 `complete()` + `stream()`**：非流式 provider 被迫实现两个
  方法或其中一个是摆设；且非流式本质是流式的不变量特例。被否决——
  单方法 + 派生便利函数覆盖两种用法。
- **仅非流式 `complete()`**：交互式场景（SSE 透传）无法支撑，v1 事件
  模型的 `StreamDelta` 无来源。被否决。

### 消息形态

- **string content**：无法表达并行工具调用与混排内容（见 Decision 论证）。
  被否决。
- **每 provider 各自的消息类型**：换 provider = 重写代码，抽象失败。
  被否决。
- **`serde_json::Value` 全动态消息**：失去类型安全与不变量可校验性。
  被否决。

### 参数与重试

- **通用参数袋（Map 或 provider 直通）**：要么类型安全缺失，要么核心
  层泄漏 provider 概念。被否决——最小类型化参数 + client 构造期配置。
- **框架内建重试（指数退避等）**：隐藏延迟与成本，违反 no-magic。
  被否决。

### 命名

- `LlmClient` / `ChatModel` / `CompletionModel` 家族：见 ADR-0003 命名
  体系备选记录，`Model` 胜出。

## Consequences（后果）

### 正面

- 单方法契约最小且完备：两种真实用法、两类 provider 全覆盖；
- canonical Message 隔离全部 provider 怪癖，翻译层是唯一差异点；
- 块序列模型为多模态、并行调用、混排内容预留了非破坏演进通道；
- 无隐藏重试让成本与延迟完全可预测。

### 成本与义务

- 翻译层是持续维护面：provider API 演进（新字段、新块型）需要跟进，
  canonical 需评估吸收或暂缓；
- 用户需要的 provider 特性若不在 canonical 形态中，须等 canonical 扩展
  （透明化权衡：刻意为之，防止抽象渗漏）；
- `Finish` 恰好一次是 provider 实现的文档化义务。

### 风险

- canonical 形态是"最大公约数"，可能滞后于单家 provider 的新能力——
  以 `#[non_exhaustive]` 渐进吸收缓解；
- v1 的 `Delta` 仅文本增量，工具调用增量流不暴露——UI 无法实时展示
  工具参数装配过程，后置按需评估。

### 关联

- 依赖 ADR-0002（双 provider 验证目标）、ADR-0003（命名体系与
  `StreamDelta`/`TokenUsage` 事件载荷）、ADR-0004（drop 契约）、
  ADR-0005（`ToolSpec` 对接、`ToolResult` 共用）；
- agent 循环消费 `ModelStream` 并投影为事件，见 ADR-0007；
  provider 实现位于 `synonz-openai` / `synonz-anthropic`（ADR-0008）；
  embeddings 未来是独立概念（如 `Embedder`），不并入 `Model`。
