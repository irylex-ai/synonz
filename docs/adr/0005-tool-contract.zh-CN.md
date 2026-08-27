# ADR-0005: Tool 抽象契约

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

Tool 是 agent 的能力单元。ADR-0002 确定 v1 同时支持进程内工具（用户
Rust 代码）与 MCP 桥接（运行时动态发现）。取消语义（ADR-0004）要求
工具执行可取消。事件模型（ADR-0003）已为并行工具调用预留 `CallId`
配对。

工具的现实形态分类：

| 形态 | 执行方式 | 取消的物理语义 |
|---|---|---|
| 进程内异步工具 | 直接函数调用 | await 点 abort |
| 进程内 CPU 工具 | `spawn_blocking` | 线程跑完、结果丢弃 |
| 远程适配工具（MCP 属于这类） | 内部 HTTP/stdio JSON-RPC | 同模型请求——drop 连接，服务端可能继续 |
| 子进程工具（未来） | OS 进程 | kill 进程 |

Tool trait 必须宽到容纳全部形态。

## Problem（问题）

trait 的形态如何同时服务两类实现者：进程内强类型 Rust 工具，与运行时
动态发现、schema 来自远端的 MCP 工具？工具失败在 agent 循环里是什么
语义？并行调用如何组织？

## Decision（决策）

### 两层结构：动态核心 trait + 类型化 derive 壳

MCP 工具是运行时动态发现的（schema 来自 server，工具列表连接后才知道），
这构成硬约束：**核心 Tool trait 必须是动态（untyped）且 dyn 兼容的**。

```rust
// ═══ 核心层 trait：动态、dyn 兼容（MCP 桥接直接实现这个）═══
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> &Schema;
    fn execute<'a>(&'a self, args: Value, ctx: ToolContext)
        -> BoxFuture<'a, Result<ToolResult, ToolError>>;
}
```

要点：

- 参数为 `serde_json::Value`，schema 为 JSON Schema——动态契约；
- 返回 `BoxFuture`（手动 boxing），不依赖 `async_trait` 派生宏——
  trait 实现主要由 derive 壳和桥接层生成，直接依赖面更小；
- dyn 兼容：agent 可持有工具集合（`Arc<[Arc<dyn Tool>]>` 之类的内部
  结构），MCP/进程内工具混存无差别。

```rust
// ═══ 便利层：derive 宏恢复类型安全（进程内工具用这个）═══
#[derive(Tool)]                     // synonz-derive 提供，synonz re-export
struct Weather {
    /// 城市名                        // description 从 doc comment 生成
    city: String,
}

impl Weather {
    async fn run(&self) -> Result<ToolResult, ToolError> { /* 全类型安全 */ }
}
```

derive 生成：name（结构体名）、description（doc comment）、
parameters_schema（字段类型经 JSON Schema 生成）、
`execute`（`Value` → 类型化参数反序列化，失败自动映射为 `ToolError`，
再调 `run`）。

### 错误语义：工具软失败，agent 硬失败

```
ToolError（工具级）  → 不终止 run。错误内容作为 ToolResult::Err 喂回模型，
                       模型自行重试/调整/放弃
AgentError（run 级） → 终止 run，发 Failed 事件（ADR-0003）
```

工具失败是 agent 循环的**正常输入**，不是异常："查询失败"对模型是有用
信息。只有框架级错误（模型不可达、循环超限）才终止 run。

```rust
#[non_exhaustive]
enum ToolResult {
    Ok  { content: ToolContent },
    Err { message: String },      // 软失败，喂回模型
}

#[non_exhaustive]
enum ToolContent {
    Text { text: String },        // v1 最小集
    Json { value: Value },        // 多模态内容后加（Image 等）
}
```

### ToolContext：第一天就有，可增长

```rust
#[non_exhaustive]
struct ToolContext {
    cancel: CancelSignal,   // v1：工具内可感知取消（ADR-0004 契约）
    // S2+ 加字段：run 元信息、状态访问……对实现者非破坏
}
```

参数对象 + `#[non_exhaustive]` = 加能力不破坏实现者（与 ADR-0003 的
purpose 字段同款演进算术）。

### 并行工具调用

模型的单次回复可发起多个工具调用：并行执行（spawn + join），取消时
全部 abort；事件按完成序产生，以 `CallId` 配对请求与结果
（ADR-0003 的 `ToolEvent` 已按此设计）。

### MCP 桥接的定位

MCP 桥接（`synonz-mcp` crate，ADR-0008）把 MCP server 的工具呈现为
`Tool` trait 实现——对 agent 循环而言，MCP 工具与本地函数工具无差别。
它属于适配层，与 providers 同层同逻辑；取消语义按"远程适配工具"分级
（连接关闭，服务端可能继续）。

## Alternatives Considered（备选方案）

### trait 形态

- **类型化泛型 trait**（`type Args: DeserializeOwned`）：对 MCP 是死路
  ——`Args` 必须是编译期已知类型，而 MCP 工具参数是运行时 schema；且
  带关联类型的 trait 无法 `dyn`，agent 不能持有异构工具集合。**这是
  MCP 进 v1（ADR-0002）的直接架构后果**。被否决。
- **同步 trait**（`fn execute(&self, args)`）：远程/子进程工具天然异步，
  同步签名容纳不了真实形态。被否决。

### 错误语义

- **工具错误终止 run**：剥夺模型自我修正的机会，把可恢复失败当成系统
  故障。被否决——软失败是 agent 循环的核心语义。
- **ToolResult 为纯字符串**：无法区分成功/失败通道，未来内容种类
  （多模态）无处安放。被否决。

### 执行组织

- **串行执行工具调用**：模型的并行工具调用是真实行为（OpenAI/Anthropic
  均支持单回复多调用），串行浪费且事件模型已为并行设计。被否决。

### 桥接定位

- **MCP 协议进核心**：把远程协议细节引入核心层，污染原语抽象。被
  否决——桥接属适配层（ADR-0008 的依赖方向原则）。

## Consequences（后果）

### 正面

- 一个契约容纳全部工具形态：进程内/远程/（未来）子进程对循环无差别；
- 类型安全的开发体验由 derive 壳完整恢复，用户不接触 `Value`；
- 软失败语义让模型获得自我修正能力，同时 run 级失败保持显式；
- 并行执行 + `CallId` 配对与事件模型自然咬合。

### 成本与义务

- 直接实现核心 trait 的作者需处理 `Value` 与 schema（人体工学成本），
  由 derive 壳缓解；
- derive 依赖 serde/schemars——具体依赖选型在实现阶段按依赖管理规则
  评审（AGENTS.md 第 9 节）；
- `ToolContext` 的增长字段须向后兼容；
- 工具实现者须按 ADR-0004 分级表声明取消安全类别。

### 风险

- 软失败的实际效果依赖模型质量（弱模型可能反复重试）——`max_rounds`
  （ADR-0007）是最终安全网；
- MCP 协议演进可能要求桥接层跟进——隔离在适配层，不波及核心。

### 关联

- 依赖 ADR-0002（MCP 进 v1 的决策）、ADR-0003（事件与 `CallId`）、
  ADR-0004（取消分级契约）；
- `ToolSpec { name, description, parameters_schema }` 汇入
  `ModelRequest`（ADR-0006）——Tool 与 Model 契约在此对接；
  derive 壳位于 `synonz-derive`（ADR-0008）。
