# ADR-0004: 取消与生命周期语义

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

四支柱中的"生命周期完备"（P5）要求取消成为一等公民。取消的**触发者**
不止一个：

1. 消费端放弃（UI 关闭、调用方不再需要结果）；
2. 超时（run 超过时间预算）；
3. 外部显式取消（另一个任务/组件持有取消权）；
4. 父任务传播（S3 编排者取消子 agent，v1 预留语义）。

取消还必须**传播**：一个 run 取消时，正在进行的模型调用、正在执行的
工具都要收到，且资源要清理。事件模型（ADR-0003）已定义
`Cancelled { reason: CancelReason }` 终止事件。

## Problem（问题）

1. 取消的入口 API 形态：谁持有取消权、如何触发？
2. 取消如何传播到执行中的模型调用与工具？
3. 取消的物理语义边界是什么（能否"强制中断"）？
4. 超时是框架内建还是用户组合？

## Decision（决策）

### 核心立场：不发明取消原语，拥抱 tokio 生态标准

取消的事实标准是 `CancellationToken`（tokio-util）+ select。框架自造
取消机制 = 生态摩擦。Synonz 不引入自己的取消类型。

### 取消入口：组合方案（三入口，一信号）

```rust
// 入口一：drop 流即取消（消费者放弃）
let mut run = agent.run(input).await?;
drop(run);   // 触发取消

// 入口二：可选 token（任何持有者可取消）
let mut run = agent.run_with(input, token.clone()).await?;

// 入口三：超时组合子（便捷方法，给 CancelReason::Timeout 真实来源）
let mut run = agent.run(input).with_timeout(dur).await?;
```

三个入口汇到同一个内部取消信号。框架不内建全局超时策略——超时是用户
策略，`with_timeout` 只是让 `Timeout` 原因可被事件流区分的便捷封装。

### 传播机制：所有 await 点可取消

```
内部取消信号（token 触发或 drop 触发）
    │
    ├── agent 循环的每个 await 点 select 它
    │       ├── model 调用 future 被 drop → 连接关闭
    │       └── 工具 task 被 abort → await 点停止
    │
    └── 收到信号后：发 Cancelled { reason } → 清理资源 → 关闭流
```

取消后事件流仍满足 ADR-0003 的终止不变量：`Cancelled` 是最后一个事件，
随后流关闭。

### 物理语义：协作式中断，分级取消安全契约

Rust 没有安全的"杀死执行中的代码"原语。取消 = 不再 poll future 并
drop 它：代码停在**下一个 await 点**，同步代码段跑完。取消延迟由
"到下一个 await 点的距离"决定，这是必须写进文档的契约。

| 执行中对象 | 取消机制 | 实际效果 | 副作用 |
|---|---|---|---|
| 模型请求（HTTP in-flight） | drop future → 连接关闭 | 客户端立刻停止等待/读取 | 服务端可能已推理、已计费——不受客户端控制 |
| 异步工具（有 await 点） | abort 所在 task | 下一个 await 点干净停止 | 已发生的副作用保留 |
| 同步长计算工具（无 await） | abort task | 等到它让出 | 计算继续到 yield 点 |
| `spawn_blocking` 工具 | 放弃等待 | 阻塞线程跑完，结果丢弃 | 副作用完整发生 |

Tool 文档必须按此分级声明各自实现的取消安全类别（用户能推理出自己工具
的取消行为——P2）。

### 诚实边界：模型取消救不回钱

请求发出后服务端的推理和计费不受客户端控制；取消的是"等待和消费响应"。
`ModelEvent::Requested` 已在事件流中记录调用发生（可观测成本）。

## Alternatives Considered（备选方案）

### 取消入口

- **仅 CancellationToken 直传**：可行但不规定 drop 行为会留下语义空洞
  （消费者放弃时流仍继续跑）。被组合方案吸收。
- **handle + stream 分离**（`run()` 返回取消句柄）：`handle.cancel()`
  本质是 CancellationToken 的重新包装——多造一层概念，违反 P1。被否决。
- **仅 drop 取消（RAII）**：只有流的持有者能取消，超时与外部控制器场景
  必须先把流拿过来再 drop，且 S3 父任务传播没有着落。被否决。

### 超时

- **框架内建全局超时策略**（默认超时/重试配置）：隐藏延迟行为，违反
  no-magic（P2）。被否决——超时是用户策略，框架只提供 `with_timeout`
  便捷封装让 `CancelReason::Timeout` 可区分。
- **不提供 with_timeout**（用户自行组合 tokio timeout）：可行，但超时
  与主动取消在事件流中不可区分（都表现为 token 触发）。被否决——
  便捷方法的成本极低，可观测性收益明确。

### 强制中断

- **强杀执行中的模型调用/工具**：Rust 无安全原语（OS 级线程强杀破坏
  内存安全）。物理不可行，如实文档化为协作式语义。

## Consequences（后果）

### 正面

- 取消权正交：drop（消费者）、token（任何持有者）、timeout（组合子）
  三入口汇一信号，语义统一；
- 取消后事件流满足终止不变量，回放完整（含取消原因）；
- 零新增概念：token 是生态类型，非 Synonz 类型；
- 分级取消安全契约让工具的取消行为可推理。

### 成本与义务

- Model trait 实现者契约：future 被 drop 时必须可安全放弃（连接清理），
  写入 provider 实现文档；
- Tool 实现者契约：按分级表声明取消安全类别；
- 取消延迟有界性依赖被取消代码的 await 密度——异步工具应在合理粒度
  到达 await 点；CPU 密集工具要么分块 yield，要么进 `spawn_blocking`
  并接受"跑完、结果丢弃"；
- `CancelReason::Parent` 在 v1 是无产生者的预留值——文档须明确。

### 风险

- 服务端副作用（已计费）不可消除——如实告知而非掩盖；
- 用户对"取消 = 立即停止"的直觉与协作式现实有差距——文档与事件流
  （`Requested` 已记录）共同弥合。

### 关联

- 依赖 ADR-0003（事件模型、`Cancelled` 终止事件）；
- Model 实现契约细节见 ADR-0006，Tool 的 abort 语义见 ADR-0005；
- S3 编排层将消费 `CancelReason::Parent` 语义。
