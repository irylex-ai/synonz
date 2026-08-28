# Synonz v1 实现计划

- 状态: VERIFIED（2026-08-28 全部里程碑 M0-M8 完成；实现与文档一致性核对通过）
- 日期: 2026-08-28
- 依据: ADR-0001 ~ ADR-0009（均 APPROVED）、架构设计文档 v1（APPROVED）
- 性质: 开发文档（中文优先）——实现阶段的执行次序、验收基准与工程基线；
  架构决策理由见各 ADR，本文不重复论证

---

## 1. 目标与范围

实现 v1 = ADR-0002 定义的 **S1 锚定场景**：单 agent 循环的完整框架
（5 crates + examples，纯库形态）。本计划定义：里程碑次序与验收标准、
依赖决策、测试布局、工程基线。

不在范围：S2 会话/记忆、S3 编排、S4 嵌入形态、多模态、重试 utility、
embeddings、运行时插件加载（后置追踪见各 ADR Consequences）。

## 2. 已确认决策（本计划的前提）

| 决策 | 结论 |
|---|---|
| 里程碑切分 | M0-M8，每里程碑有可运行验收标准（对照 ADR 行为断言） |
| 开发策略 | Mock-first：M3 起以 MockModel 验证全部循环语义，M5/M6 才接真实 provider |
| MockModel 定位 | feature-gated `"test-util"`：默认不编译，下游可复用，公共 API 面最小 |
| 依赖 | 标准组 + schemars 1.2 + rmcp 3.1（官方 SDK），详见 §5 |
| git | M0 初始化 + 初始提交；此后每里程碑验收通过时一次提交（常设授权） |

## 3. 里程碑

### M0 — workspace 骨架

- **内容**：Cargo workspace 清单（5 成员 crate 空壳 + workspace 级
  依赖版本与 lints 统一管理）；clippy/rustfmt 基线配置；`.gitignore`；
  `rust-toolchain.toml`（stable 频道，≥1.88）；`git init` + 初始提交
- **验收**：`cargo check` / `cargo clippy` / `cargo test` 全绿（空壳级）
- **对应**：ADR-0008

### M1 — 核心类型（synonz）

- **内容**：`Message` / `Role` / `ContentBlock` / `CallId` / `ToolResult`
  / `ToolContent`；事件体系全量（`AgentEvent` / `LifecycleEvent` /
  `ModelEvent` / `ToolEvent` / `CallPurpose` / `CancelReason` /
  `TokenUsage` / `ModelDelta`）；`AgentError` / `ModelError` /
  `AgentInput` / `AgentOutput`；serde 序列化
- **验收**：canonical 三不变量测试；serde round-trip；事件序列化
  快照测试（`{"type":"model.requested",...}` tag 格式锁定）
- **对应**：ADR-0003、ADR-0006

### M2 — 契约与取消（synonz）

- **内容**：`Tool` trait + `ToolContext` + `ToolSpec` + `ToolError`；
  `Model` trait + `ModelRequest` / `ModelParams` / `ModelStreamItem` +
  `complete()` 便利函数；内部取消信号引擎（token/drop/timeout 三入口
  汇一信号，所有 await 点 select）
- **验收**：dyn 兼容性编译测试（`Arc<dyn Model>` / `Arc<dyn Tool>`
  可持有）；取消传播单元测试（mock future 在正确时点被 drop）
- **对应**：ADR-0004、ADR-0005、ADR-0006

### M3 — Agent 与循环（synonz + test-util）

- **内容**：`AgentBuilder`（model/system_prompt/tool/tools/max_rounds/
  build）；`run` / `run_with` / `with_timeout` / `ask`；`RunStream`
  （next/rounds 推导）；推理-行动-观察循环 + 事件投影；并行工具执行 +
  `CallId` 配对；`MockModel`（feature `"test-util"`）
- **验收**：端到端 mock 套件——正常单轮 / 工具循环 / 并行工具 /
  软失败回喂 / 三入口取消 / 超时（`CancelReason::Timeout`）/ max_rounds
  超限 `Failed` / 事件终止不变量 / 事件流录制回放
- **对应**：ADR-0003、ADR-0004、ADR-0007

### M4 — synonz-derive

- **内容**：起点先做 schemars 1.x 技术验证 spike（与 derive 设计的
  配合度）；`#[derive(Tool)]`：name ← 结构体名、description ← doc
  comment、schema ← 字段类型、`Value` → 类型化参数反序列化桥接
- **验收**：derive 生成与手写 `Tool` 实现的等价性对照测试
- **对应**：ADR-0005

### M5 — synonz-openai

- **内容**：canonical ↔ OpenAI 翻译层（system 消息、`tool_calls`、
  `role:"tool"`、arguments 字符串化互转）；SSE 流式（手写单请求流
  解析器）；reqwest client
- **验收**：fixture 双向测试；env-key 门控真实冒烟
  （`SYNONZ_OPENAI_API_KEY` 存在才执行）
- **对应**：ADR-0006、ADR-0002

### M6 — synonz-anthropic

- **内容**：canonical ↔ Anthropic 翻译层（重点：system 顶层参数、
  tool_result 并入 user 消息、`is_error` 映射、input 对象互转）
- **验收**：fixture 双向测试；env-key 冒烟（`SYNONZ_ANTHROPIC_API_KEY`）
- **对应**：ADR-0006、ADR-0002

### M7 — synonz-mcp

- **内容**：rmcp 3.1 桥接（锁 minor 版本）；MCP server 工具发现 →
  `ToolSpec`；调用转发与结果映射；stdio transport 优先
- **验收**：stdio MCP server fixture 集成测试
- **对应**：ADR-0005、ADR-0002

### M8 — examples 与收尾

- **内容**：每能力一个可运行示例（最小 ask / 事件流消费 / 取消 /
  自定义 Tool / MCP 接入 / 双 provider）；最小 README；rustdoc 完整性
  检查
- **验收**：全部示例可运行（需 API key 的显式标注）；`cargo doc`
  无警告
- **对应**：ADR-0002、AGENTS.md 文档标准

## 4. 开发策略

- **Mock-first**：M3 全部验收不依赖真实 provider——循环语义、取消、
  事件不变量在确定性环境下验证（P7 免费副产品的兑现）；provider
  差异从 M5 起由 fixture 锁定
- **验收即 ADR 断言**：每里程碑验收标准是对应 ADR 行为条款的可执行
  化；实现与 ADR 分歧时按 coding.md 上报而非静默吸收
- **增量提交**：里程碑验收通过 → 一次提交（信息按里程碑语义，如
  `M3: agent loop with mock-verified semantics`）；里程碑内部改动
  不产生中间提交噪音

## 5. 依赖决策记录

### 标准组（生态事实标准，MIT/Apache-2.0 兼容）

| 依赖 | 用途 | 引入于 |
|---|---|---|
| `serde` + `serde_json` | 序列化 / `Value` | M1 |
| `tokio` + `tokio-util` | 运行时 / `CancellationToken` | M2 |
| `thiserror` | 错误类型 derive（AGENTS 预留的 error crate 选型在此显式落定） | M1 |
| `reqwest` | provider HTTP | M5 |
| `syn` / `quote` / `proc-macro2` | derive 宏必需件 | M4 |

### 已验证组（2026-08-28 crates.io 核实）

| 依赖 | 核实事实 | 决策 |
|---|---|---|
| `schemars` 1.2.2 | MIT/Apache-2.0；总下载 4.2 亿；1.x 线稳定；2026-07 活跃维护 | derive Tool 的 schema 生成（M4，spike 先行） |
| `rmcp` 3.1.4 | Apache-2.0；**官方 SDK**（modelcontextprotocol/rust-sdk）；1.5 年 2200 万下载；client + stdio/streamable-http transport 齐备；MSRV 1.88 | MCP 桥接用官方 SDK，不手写协议；锁 minor（M7） |

### 实现期待定项（不阻塞）

- SSE 解析：倾向手写 ~50 行（单请求流，无重连），fixture 覆盖跨块
  分片；不契合再评估 eventsource-stream
- `futures` vs 仅 `futures-core`：按实际用量定

### 工具链

stable 频道，≥1.88（rmcp MSRV 下限）；Synonz 自身 MSRV 政策按 AGENTS
纪律不预设，发布前再定。

## 6. 测试布局

| 层级 | 位置 | 约定 |
|---|---|---|
| 单元测试 | `#[cfg(test)]` 就近 | 确定性，无网络 |
| 集成测试 | 各 crate `tests/` | MockModel / fixture 驱动 |
| 翻译层 fixture | provider crate `tests/` | canonical ↔ 厂商格式双向；API 漂移时测试先红 |
| 真实 API 冒烟 | provider crate `tests/` | env-key 门控，CI 默认跳过 |
| 循环语义套件 | synonz（`test-util`） | 事件终止不变量、取消时序、回放 |

## 7. 工程基线

- **rustdoc 义务**（AGENTS §7）：公共 API 文档随代码写（M1-M7 内含），
  覆盖用途/行为/约束/错误语义/取消语义；M8 只做完整性检查与 examples
- **lint/format**：rustfmt 默认；workspace 级 clippy lints 统一配置；
  新警告视为缺陷（AGENTS §7）
- **git 流程**：M0 `git init` + 初始提交；里程碑节点提交（常设授权）；
  提交信息英文、按里程碑语义

## 8. 风险与缓解

| 风险 | 缓解 |
|---|---|
| rmcp 3.x 迭代快（2026-08 三连发），API 可能变 | 锁 minor；桥接隔离在 synonz-mcp，不波及核心 |
| schemars 1.x 与 derive 设计的配合细节不确定 | M4 起点 spike 验证，不契合再评估替代 |
| SSE 手写解析边界情况 | 范围收敛（单请求流、无重连）；fixture 覆盖多事件/跨块分片 |
| Provider API 漂移 | 翻译层 fixture 测试锁定行为，漂移时测试先红 |
| 双 provider 的翻译维护面 | fixture 驱动 + 差异表（ADR-0006 对照表）为测试清单 |

## 9. 状态流转

```
DRAFT →（人工评审）→ APPROVED →（M0 启动）→ IMPLEMENTING
                                          →（M8 验收 + 文档核对）→ VERIFIED
```

里程碑进度在本文档追加记录（不改动已定章节；范围变更走新 ADR）。

## 10. 里程碑进度

| 里程碑 | 状态 | 完成日期 | 备注 |
|---|---|---|---|
| M0 | ✅ 完成 | 2026-08-28 | 工具链 rustup 1.98.0 安装；fmt/check/clippy/test 全绿；git main 双提交（docs + skeleton） |
| M1 | ✅ 完成 | 2026-08-28 | canonical Message + 事件体系 + 错误分类 + io 边界类型；20 测试全绿（不变量 6 / round-trip + 快照 9 / doctest 1）；serde 双 tag 格式锁定 |
| M2 | ✅ 完成 | 2026-08-28 | Tool/Model trait（dyn 兼容验证）+ ToolContext/ToolSpec/ToolError + ModelRequest/Params/StreamItem + complete() + 取消引擎（drop/token/timeout 三入口汇一，mock future 协作式中断测试）；依赖定案：futures 门面 |
| M3 | ✅ 完成 | 2026-08-28 | Agent/Builder/RunStream + 推理-行动-观察循环 + 事件投影 + 并行工具（CallId 配对、完成序事件、确定性会话序）+ MockModel（test-util feature）；14 场景集成套件全绿（含三入口取消、协作式中断哨兵验证、录制回放）；取消引擎重构为 Core/Handle 权属拆分 |
| M4 | ✅ 完成 | 2026-08-28 | schemars 1.2 spike 定案（doc→description、Option 可选、根含 $schema/title，已锁回归测试）；`#[derive(Tool)]`（snake_case 命名/doc 描述/schema 缓存/类型化反序列化）；synonz 重导出 schema_for/JsonSchema/Deserialize/serde_json + BoxFuture 别名——下游实现工具仅需 synonz 一个依赖；等价性对照测试 6 项 |
| M5 | ✅ 完成 | 2026-08-28 | synonz-openai：canonical↔OpenAI 翻译层（system/user/assistant+tool_calls/role:tool/arguments 字符串化/tool error 文本前缀）+ 手写 SSE 增量解析器 + reqwest client + SSE→ModelStream 状态机 unfold；wiremock 端到端流式测试 + env-key 门控冒烟（SYNONZ_OPENAI_API_KEY）；新增 ModelStreamItem::Failed 变体（流中途失败显式化）；futures/futures-core 合并为 futures 门面 |
| M6 | ✅ 完成 | 2026-08-28 | synonz-anthropic：翻译层（system 顶层参数、块数组内容、tool_use input 对象、tool_result 并入 user 消息 + 原生 is_error）+ block-indexed 流式累加器（text_delta/input_json_delta 合并、usage 双事件提取、stop_reason/message_stop 终态）+ x-api-key/anthropic-version 头；wiremock 端到端 + env-key 冒烟（SYNONZ_ANTHROPIC_API_KEY）；SSE 解析器刻意复制（~60 行不立共享 crate，第三个适配器出现时晋升） |
| M7 | ✅ 完成 | 2026-08-28 | synonz-mcp：rmcp 3.1 官方 SDK 桥接（锁 minor；client+transport-child-process features）；McpBridge（通用 connect(transport) + connect_stdio 便捷 + from_running 发现）+ McpTool（元数据→ToolSpec、call 转发）；双测试：tokio duplex 内存传输（发现/往返/软失败映射/非对象参数拒绝）+ re-exec 自身二进制真 stdio 子进程往返（--ignored 辅助测试）；MCP JSON-RPC 工具错误→ToolResult::Err 软失败 |
| M8 | ✅ 完成 | 2026-08-28 | examples 包（6 bin：custom_tool/events/cancellation/mcp_tools 离线 + openai_chat/anthropic_chat env-key 门控）；四个离线示例真实运行验证（工具循环/事件叙事/三取消入口/MCP 桥接）；最小 README；cargo doc 零警告（rustdoc 完整性）；workspace 成员 6 crates 全部 lint/format/test 绿 |
| M9 | ✅ 完成 | 2026-08-29 | v1.x 增量（ADR-0010 High Level 优先原则首次应用）：Agent::react/research/reflection 三预设（配方=prompt+参数，返回 AgentBuilder 全可覆盖）+ extend_system_prompt 组合扩展方法 + prompt 常量私有化（rustdoc 全文展示防错用）；7 预设单测（填充/扩展/覆盖断言 + 行为冒烟） |
