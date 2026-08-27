# ADR-0007: Agent 组装与执行循环

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

ADR-0003/0004/0005/0006 确立了原语层契约（事件、取消、Tool、Model、
Message）。本 ADR 决定组合层的形态：`Agent` 是什么、如何组装、内部
循环如何执行，以及"Agent 模式"（ReAct / plan-execute / research 等）
的架构定位。

讨论中人类提出的关键质询：builder 形态是否照搬了 LangChain？各种
agent 模式的扩展性放哪？

## Problem（问题）

1. `Agent` 的语义：状态、组装方式、配置项；
2. 执行循环的内部结构与防失控机制；
3. ReAct / plan-execute / research 等"模式"是框架配置项还是别的什么；
4. `ask()` 与错误模型的映射；最终输出形态。

## Decision（决策）

### Agent 是无状态的配置体（P2 落点）

```rust
let agent = Agent::builder()
    .model(gpt4o)               // Arc<dyn Model>，必填
    .system_prompt("...")       // 可选——不设就不发 System 消息
    .tool(weather)
    .tools([a, b, c])
    .max_rounds(16)             // 显式预算，见下
    .build()?;
```

- `Agent` = model + tools + system_prompt + 限额的**不可变组合**；
- 状态只存在于 run 内部（消息累积在 run 的执行栈上）；同一个 Agent
  可并发跑多个 run，互不可见、互不污染——**没有隐藏可变状态**，这是
  行为可推理的前提；
- builder 是 Rust 构造惯例（同 `std::process::Command`），非继承自任何
  框架的范式。

### system_prompt：显式、无默认、单一来源

- `.system_prompt("S")` 的语义**就是**"该 Agent 每个 run 的消息列表以
  `Message { role: System, blocks: [Text("S")] }` 开头"——不是别的
  机制，不存在组合或追加；
- 不设置就**没有** System 消息（provider 适配层不翻译一条不存在的
  消息）——无隐藏默认 prompt（P2/no-magic）；
- S1 中 system prompt 是 Agent 的静态配置；按 run 动态变化的内容走
  input（用户消息）。

命名采用 `system_prompt` 而非 `role_def`/`instructions`：避免与
`Role` 枚举（消息角色）及 S3 团队角色概念撞车；行业通用语，零学习成本。

### max_rounds：显式防失控预算

- 模型-工具循环理论上可无限跑（模型持续调工具）；`max_rounds` 是
  显式预算：默认值存在且文档写明（16），可配置；
- 超限 → **`Failed` 事件**（`AgentError::MaxRoundsExceeded`），不是
  静默截断——预算耗尽是失败，必须显式。

### 执行循环内部结构（架构级，非公开 API）

```
run(input):
  发 Started { input }
  messages = [system?] + [user]
  循环:
    发 Requested { purpose: Reasoning, messages }
    消费 ModelStream → 转发 Delta 事件 → Finish
    发 Responded { message, usage }
    message 无 ToolCall 块 → 跳出
    并行 spawn 每个 ToolCall（select 取消信号，见 ADR-0004/0005）:
      发 CallRequested → 执行 → 发 CallCompleted
      软失败 → ToolResult::Err 喂回（不终止）
    messages += assistant 消息 + 工具结果消息
  终止事件（Completed / Failed / Cancelled）→ 关流
```

- 每个循环 await 点 select 取消信号（ADR-0004）；
- 消息累积维护 canonical Message 的三条不变量（ADR-0006）；
- 循环对事件流的投影满足 ADR-0003 的全部不变量。

### 模式扩展立场：原语公开 + Agent 罐装 + 模式层后置

```
┌─ 模式层   ReAct / plan-execute / research / reflection
│           plan-execute = 两个 Agent + 用户代码编排
│           research = 一个 Agent + 搜索工具集 + 好 prompt
├─ 组合层   Agent：一种罐装组合（model + tools + prompt + 循环）
├─ 原语层   Model / Tool / 事件流 / Message —— 全部公开
└──────────────────────────────────────────
```

- v1 的 `Agent` **就是且只是一种执行语义**（推理-行动-观察循环），
  服务 90% 场景，明码标价，不假装万能；
- **真正的扩展性在下一层**：想写自定义循环/执行引擎的用户绕过 Agent
  直接组合原语，框架不设墙——这是比"可插拔策略接口"诚实的扩展模型；
- plan-execute 等多 agent 模式 = S3 编排层的真实需求，到时候用真实
  用例驱动设计（LangGraph 的图运行时是被"预设通用性"逼重的反例）。

### ask() 映射与 AgentError

```rust
async fn ask(&self, input: AgentInput) -> Result<AgentOutput, AgentError>;
// Completed → Ok(AgentOutput)
// Failed    → Err(AgentError)
// Cancelled → Err(AgentError::Cancelled)

#[non_exhaustive]
enum AgentError {
    Model(ModelError),       // 模型调用失败（ADR-0006）
    MaxRoundsExceeded,       // 循环预算耗尽
    Cancelled(CancelReason), // 取消（ADR-0004）
    InvalidConfiguration,    // 组装错误（如缺 model）
}
```

### AgentOutput：最小输出 + 应用层扩展边界

```rust
#[non_exhaustive]
pub struct AgentOutput {
    pub message: Message,      // 最终 Assistant 消息（完整 blocks）
    pub usage: TokenUsage,     // 整个 run 累计
}
impl AgentOutput {
    pub fn text(&self) -> Option<&str>;   // 便利：拼接 Text 块
}
```

**业务场景判别（如 `[CARD][NEED_CLARIFY]` 标记或自定义 EVENT TYPE）属于
应用层，不在框架词汇表**：

- 形态 A（应用打标）：应用消费事件流，由应用代码包装自己的协议；
- 形态 B（模型打标）：prompt 约定模型输出标记，前端解析——概率性可靠；
  需要强保证时用工具化结构输出（定义 `submit_result(type, data)` 类
  工具，schema 保证结构）。

两种形态 v1 零件（Tool + prompt + 可序列化事件）均可支撑，框架保持中立。

## Alternatives Considered（备选方案）

### 模式扩展

- **可插拔执行策略**（`Executor` trait：ReActExecutor/PlanExecutor...）：
  策略接口会变成万能接口或过度泛型——每种模式想要不同输入与状态，
  trait 无法同时满足；LangGraph 图运行时是被通用性逼重的先例。被
  否决——原语公开 + 罐装 Agent + 模式层后置。

### 状态

- **Agent 持有会话状态/记忆**：S2 范畴；v1 无状态是 P2 的显式选择。
  后置追踪。

### system_prompt

- **`role_def` 命名**：与 `Role` 枚举撞概念，S3 团队角色将无干净名字；
  自造术语违反命名规则。被否决。
- **`instructions`**（OpenAI Assistants 术语）：可接受但非最直白。
  被否决——`system_prompt` 与其产生的 System 消息一一对应。
- **Prompt 对象/模板系统**：prompt 的多形态已在 `Role` + `ContentBlock`
  体系对象化；模板是 utility 层（`format!` 或任何模板引擎），进核心
  即投机抽象。被否决。

### max_rounds

- **无上限**：防失控缺失。被否决。
- **静默截断**（超限取当前结果）：预算耗尽被掩盖，违反显式失败原则。
  被否决。

## Consequences（后果）

### 正面

- 无状态 Agent 天然支持并发 run，无共享状态隐患；
- 一种执行语义做到透——文档、测试、推理都聚焦；
- 原语层公开构成诚实的扩展出口，自定义引擎不经过框架审批；
- 业务扩展边界清晰：框架不管业务 EVENT TYPE，但零件全部备齐。

### 成本与义务

- `Agent` 不满足的需求（非 ReAct 形态）在 v1 需要用户直接使用原语层
  自建——这是显式选择的成本，文档须给出指引（扩展点章节）；
- `max_rounds` 默认值需要文档化并可能在真实使用中调整；
- `AgentError` 的 `#[non_exhaustive]` 增长纪律。

### 风险

- 用户把 Agent 当万能抽象（期待它有模式参数）——通过文档的"分层结构"
  说明管理预期；
- 自建循环的用户绕过 `Agent` 后需要自行维护 canonical 不变量——
  文档须明示不变量（ADR-0006）。

### 关联

- 依赖 ADR-0003（事件投影）、ADR-0004（取消）、ADR-0005（工具执行与
  软失败）、ADR-0006（Model 消费与消息不变量）；
- S2 将在本 ADR 基础上引入会话状态（新 ADR）；
  实现落位 `synonz` crate（ADR-0008）。
