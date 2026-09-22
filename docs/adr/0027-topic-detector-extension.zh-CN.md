# ADR-0027: 主题检测独立扩展点——`TopicDetectorProvider`（修订 ADR-0022 §6）

- 状态: APPROVED（2026-09-20，irylex 人工评审通过）
- 日期: 2026-09-20
- 决策者: irylex（逐点确认；遵循 `.opencode/rules/architecture.md` §5–§9
  的渐进决策流程）
- 性质: 核心契约修订（主题检测从流水线钩子独立为 Agent 级扩展点）+
  模型配置面扩充；随 0.7.0 单波承载
- 关联: **修订 ADR-0022 §6**（`MemoryPipeline` 四钩子 → 三钩子；
  `detect_topic` 移出）；补充 ADR-0025（模型解析与叙述规则同 rewriter）；
  引用 ADR-0023（`TopicShifted` 属核心事实）；不改写 ADR-0022 正文
- 版本承载: 0.7.0
- 前提: 无旧数据迁移；topic 状态、写回与事实仍归核心；检测算法归实现
  （官方组件提供文档算法的实现）

## Context（背景）

ADR-0022 §6 把 `detect_topic` 作为 `MemoryPipeline` 的四个业务钩子之一
（provider / Runtime 级）。落地分析（官方组件按《上下文记忆系统全链路
技术方案》实现）暴露出三个问题：

1. **概念归属错位**：topic 是**核心会话概念**（状态在 `Conversation`、
   `TopicShifted` 属核心事实、写回入口 `set_topic` 在核心）；把它作为
   记忆写机制的钩子，把会话概念绑进了记忆管线；
2. **粒度与可换性退化**：旧的核心策略槽 `ConversationTopicDetector` 是
   **Agent 级**（每个 Agent 可配不同检测器，ADR-0019）；改为 pipeline
   钩子后变成 **Runtime 级**（只能换 provider 或换组件内部策略），且
   无法"用组件的记忆 + 自己的检测器"；
3. **变更流缺失**：检测结果只停在钩子内，后续钩子（`spawn_task` 触发
   结构化摘要 / 图谱更新）拿不到"本轮发生了切换"——契约里没有这个流。

## Problem（问题）

1. 主题检测需要一个**独立的扩展点**（与 rewriter 对称：读侧预处理 /
   写侧预处理各一），恢复 Agent 级粒度与独立替换；
2. 检测器的**模型配置**需要与记忆模型解耦（检测可用轻量模型或纯
   embedding，与记忆处理不同）；
3. 检测结果的**写回、事实与变更传递**需要明确的机制归属（核心）。

## Decision（决策）

### 1. 独立契约与工厂（Agent 级）

- 新增 **`TopicDetectorProvider`**（工厂）：`topic_detector()` + 可选
  `model()`；
- 注册在 **Agent 级**（同 `RewriterProvider`）；核心在轮末（写相位
  前）调用；
- 与 `MemoryProvider` / `RewriterProvider` 并列，三者独立配置。

### 2. 模型规则（同 rewriter）

- 解析：`TopicDetectorProvider 模型 ?? Agent 模型`（使用时解析、核心
  统一包叙述；**不回落记忆模型**）；
- 检测器可以不用 LLM（文档算法 = embedding 相似度 / 轮次 / 信息熵；
  embedding 由**组件侧端口**自持，核心只给材料）。

### 3. 检测器材料与返回

- 材料：本轮输入 / 真相域近期历史 / 当前 topic / 可选叙述模型
  （形状以实施为准）；
- 返回：新 topic（`Option<String>`；`None` = 保持当前 topic）。

### 4. 核心的机制职责

- 轮末调用检测器 → **写回** `Conversation.topic` → **变化时**发
  `TopicShifted { from, to }`（核心事实，ADR-0023）；
- **变更传递**：核心把本轮 topic 变更经管线上下文暴露给后续钩子
  （数据读取的一部分，如 `topic_change()`）——`archive_turn` /
  `spawn_task` 据此反应（切换触发结构化摘要 / 图谱更新）；
- **失败语义**：检测失败保留旧 topic，核心发 `Failed` 事实
  （never-silent）；不阻断归档与后台任务。

### 5. 流水线钩子调整

- `MemoryPipeline` 业务钩子由四个减为**三个**：`archive_turn` /
  `spawn_task` / `finalize_conversation`；
- 模板机制不变（写回 topic、发事实、派生后台任务、终结先有界排空）。

### 6. 组件实现

- 官方组件提供**检测器实现**（文档算法：embedding 相似度 / 轮次 /
  信息熵），经 `TopicDetectorProvider` 注册；embedding 用组件侧端口；
- 组件侧不再把"主题检测"作为 `MemoryPipeline` 内部策略（策略清单
  相应调整：事件判断 / 摘要 / 实体提取 / 上下文改写）。

## Alternatives Considered（备选与否决）

1. **留在 `MemoryPipeline` 钩子**（现状）——否决：概念归属错位、粒度
   退化为 Runtime 级、变更流缺失（需另补）；
2. **核心固定算法**（不给扩展点）——否决：检测策略与产品/模型相关，
   必须可换；
3. **Runtime 级独立扩展点**（放 `MemoryProvider` 行为）——否决：与
   rewriter 不对称、无法 per-Agent；
4. **变更由组件自持**（pipeline 实例按会话记标志）——否决：共享实例
   的按会话状态与清理、并发 / 生命周期成本高；核心本就承载检测结果。

## Consequences（后果）

**破坏项（0.7.0）**

- `MemoryPipeline` 钩子清单变化（四 → 三）：`detect_topic` 不再是钩子；
- `TopicShifted` 的触发源表述变化（核心模板 → 核心检测器调用路径）；
- 组件"语义策略"清单去掉主题检测（改由检测器实现承载）。

**加性项**

- `TopicDetectorProvider` 契约 + 工厂（Agent 级、可选模型）；
- 管线上下文新增本轮 topic 变更的数据读取。

**边界（本 ADR 明确不做）**

- 检测算法的具体参数与提示词（实现 / 组件设计）；
- 多 Agent 同一会话的检测器一致性（应用 / 编排层责任，既有立场）。

**文档义务**

- v8 同步（§4.5 钩子与检测器伪代码、§5.1 轮末流程、§8.2、§9.3/9.4、
  §11 扩展点、§12 术语、§13 映射、§14）；
- 0.7.0 计划同步（M39/M42/M43、迁移表、已决项）；CHANGELOG 与 API
  适配说明。

**验证要求**

- 检测器 Agent 级注册与替换；模型解析（Provider → Agent 回退、核心
  叙述）；
- 写回与 `TopicShifted`（变化才发）；变更经上下文到达后续钩子
  （切换触发摘要 / 图谱）；
- 检测失败保留旧 topic + `Failed` 事实；
- 组件检测器实现（embedding 相似度 / 轮次 / 信息熵）与 `MemoryPipeline`
  三钩子回归。
