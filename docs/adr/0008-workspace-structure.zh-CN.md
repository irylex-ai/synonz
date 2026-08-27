# ADR-0008: Workspace 与 crate 结构

- 状态: APPROVED（2026-08-28，irylex 人工评审通过）
- 日期: 2026-08-28
- 决策者: irylex（人类确认）

## Context（背景）

v1 的全部架构决策（ADR-0001 ~ 0007）需要落为具体的工程包装：Cargo
workspace 布局、crate 划分与命名、依赖方向、增长原则。

工程约束：

- Rust 规定 proc-macro crate（derive 宏）不能与普通库 crate 是同一个；
- provider（OpenAI/Anthropic）与 MCP 桥接是可选依赖——用不到的用户
  不应编译其代码；
- 终态愿景（S2/S3/S4）将持续带来新组件。

## Problem（问题）

如何组织 crate 使依赖树精简、核心编译快、边界清晰，并为后续场景增长
预留不污染核心的结构原则？

## Decision（决策）

### Workspace 布局

```
synonz/                        ← Cargo workspace（虚拟清单）
├── crates/
│   ├── synonz/                ← 核心：全部 trait、事件、Message、循环
│   ├── synonz-derive/         ← #[derive(Tool)] proc-macro crate
│   │                            由 synonz re-export，用户不直接依赖
│   ├── synonz-openai/         ← OpenAI 兼容 Model 实现（翻译层）
│   ├── synonz-anthropic/      ← Anthropic Model 实现（翻译层）
│   └── synonz-mcp/            ← MCP 桥接：MCP 工具 → Tool 实现
└── examples/                  ← 每个能力一个可运行示例（ADR-0002）
```

### 命名与角色

- **核心 crate 直接叫 `synonz`**（非 `synonz-core`）：`use synonz::{Agent,
  Tool}` 最短路径——核心即框架本体；
- `synonz-derive`：实现 ADR-0005 的类型化壳；用户通过 `synonz` 的
  re-export 获得宏，永远不直接依赖；
- `synonz-openai` / `synonz-anthropic`：实现 ADR-0006 的 `Model` trait，
  承载全部翻译层；
- `synonz-mcp`：实现 ADR-0005 的动态 `Tool` 桥接。

### 依赖方向原则

```
synonz-derive ─┐
synonz-openai ─┤
synonz-anthropic ─┼──→ synonz（核心）
synonz-mcp ───┘
```

- **单向**：adapter/derive → core，核心不依赖任何 Synonz 组件，无环；
- MCP 与 providers 是同层逻辑：都是把外部协议/服务桥接为核心契约的
  适配层。

### 增长原则：新场景 = 新 crate，叠加不吸收

- **核心永不吸收上层能力**：S2（会话/记忆）、S3（编排）作为新 crate
  叠加在核心之上，依赖方向永远向下（orchestration → synonz）；
- 不编排多 agent 的用户，编译树里永远没有编排代码；
- **未来 crate 的名字现在不定**（P1：不给不存在的功能命名）——场景
  启动时带真实需求决定，多 agent 编排绝不进 `synonz` 核心。

### 实现层选型的边界

provider 内部 HTTP 客户端选型、MCP client 实现（手写或用库）、
schema 生成库等属**实现层决策**，架构只锁定 crate 边界与依赖方向；
具体依赖引入时按 AGENTS.md 第 9 节评审（重大依赖决策需 ADR）。

## Alternatives Considered（备选方案）

### crate 组织

- **单一 crate**：proc-macro 必须独立（编译器规定），provider 依赖
  无法可选——两个硬约束都不满足。被否决。
- **核心叫 `synonz-core`**：路径更长（`synonz_core::Agent`）且"核心
  即框架"的关系被名字模糊。被否决。
- **平铺布局**（crate 直接放 workspace 根）：可行但随 crate 增多根目录
  混乱；`crates/` 是 tokio/bevy 等成熟项目惯例。未采用。

### 增长方式

- **多 agent 编排进核心 crate**：不编排的用户被迫编译无关代码；核心
  膨胀违背分层原则（ADR-0001/0007）。被否决。
- **现在为 S2/S3 预建空 crate 占位**：为不存在的功能命名与维护空壳，
  违反最小核心纪律。被否决。

## Consequences（后果）

### 正面

- 用户依赖树精确：只用 OpenAI 的项目不含 anthropic/mcp 代码；
- 核心小而稳定，adapter 可独立演进发布；
- 增长原则让 S2/S3/S4 的加入路径明确且零污染；
- examples 随仓库提供可复制的真实用法（开源可发现性）。

### 成本与义务

- 多 crate 的版本协同与发布负担（核心先行、adapter 跟进）；
- re-export 布线（synonz → synonz-derive）需要维护；
- workspace 工程配置（lint、测试、CI 覆盖全部成员）一次性建立。

### 风险

- 核心 API 破坏性变更会波及全部 adapter——pre-1.0 阶段预期内，
  通过真实使用尽快稳定核心契约缓解；
- 未来 crate 命名未定可能导致讨论滞后于需求——以"场景启动即决策"
  的流程纪律缓解。

### 关联

- 依赖 ADR-0002（v1 范围：库形态、双 provider、MCP）、ADR-0005
  （derive 壳与 MCP 桥接）、ADR-0006（provider 翻译层）、ADR-0007
  （核心内容物）。
