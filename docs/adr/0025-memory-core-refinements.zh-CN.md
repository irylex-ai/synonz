# ADR-0025: 记忆核心契约补强——读相位模型访问、完整消息帧与作用域寻址

- 状态: APPROVED（2026-09-20，irylex 人工评审通过）
- 日期: 2026-09-20
- 决策者: irylex（逐点确认；遵循 `.opencode/rules/architecture.md` §5–§9
  的渐进决策流程）
- 性质: 核心契约补强（读/写相位的模型访问、读相位消息帧语义、管理面
  作用域寻址）+ 行为调整；随 0.7.0 单波承载
- 关联: **补充/修订 ADR-0022**（§4 模型叙述、§5 读相位载荷、§6 上下文
  语义入口）；**补充 ADR-0023**（§2 默认实现、§4 扩展轴）；**调整
  ADR-0020 的管理面寻址**（新增 scope 维度）；不改写上述 ADR 正文
- 版本承载: 0.7.0
- 前提: 无旧数据迁移（0.6.0 序列化数据不支持）；本 ADR 只做核心侧
  补强；组件按《上下文记忆系统全链路技术方案》重定另立 ADR-0026；
  scope 的来源、推导与注入由应用层决定（核心不定义）

## Context（背景）

ADR-0022/0023 完成记忆组件边界与核心收口后，官方组件开始按实际设计
基线《上下文记忆系统全链路技术方案》（服务端服务设计；其分层模型为
框架所借鉴，实现逻辑按文档）落地。落地分析暴露出三处核心契约缺口：

1. **模型访问缺口**：assembler 输入没有模型句柄（只有 rewriter 材料
   带模型）；pipeline 上下文只有数据读取 / `set_topic` / `spawn` /
   `report_failure`。文档的读侧阶段 3（召回后 LLM 改写）与写侧 LLM
   调用（事件判断 / 摘要 / 实体提取）无法实现；
2. **消息帧语义缺口**：读相位输出是"背景消息"，核心恒定追加原始
   input 作为模型可见的 user 消息。文档阶段 3 的产物是**模型可见的
   增强输入**（上下文并进 user 消息）；且在新语义下降级时若实现未
   放入用户消息，模型将看不到本轮输入（静默破坏）；
3. **寻址缺口**：管理面按 subject 寻址，无法表达与管理"用户级 /
   项目级"等**记忆分区**。

## Problem（问题）

1. 组件实现需要**叙述模型**（assembler 与 pipeline 两处），核心需
   给出统一的解析（Provider → Agent 回退）与叙述规则；
2. 模型可见输入的所有权与降级语义需要明确（含"模型看不到输入"的
   兜底）；
3. 记忆分区需要一个核心可解释的寻址维度（scope），且**不能**把
   应用概念（项目 / 工作区）固化进核心；
4. 管理事实需要携带分区信息（删除事实事后不可定位）。

## Decision（决策）

### 1. 读相位模型访问

- `MemoryContextAssembleInput` 增**叙述 LLM 句柄**；解析规则与
  rewriter 同：Provider 模型 → 回退 Agent 模型；核心统一包叙述
  （自动 `Requested` / `Responded`、无 `StreamDelta`）。

### 2. 完整消息帧

- `MemoryContextAssembleOutput.messages` = **`system(Agent)` 之后的
  完整消息帧**（含本轮模型可见用户消息；角色组织由实现决定——约定：
  system 只放 Agent 指令，记忆上下文与增强输入进 user 侧）；
- 核心前置 `system(Agent)`（若有）后**直接发模型**——**不再追加
  原始 input**；
- 真相归档：`Turn.messages` = 完整帧 + 本轮后续消息；`Turn.input`
  仍为**原始文本**（真相）。

### 3. 降级与空帧兜底

- **契约义务**：assembler 必须返回**含本轮用户消息的可用帧**；
  失败时自行降级（把原始 input 作为 user 消息放进帧 + `failures`
  上报）；
- **核心兜底**：**空帧**时核心用原始 input 组帧并发失败事实
  （never-silent）。

### 4. 写相位模型访问

- `PipelineTurnContext` / `PipelineConversationContext` 增**叙述 LLM
  句柄**（写侧 LLM 调用：事件判断 / 摘要 / 实体提取等）。

### 5. 作用域寻址（scope）

- 新增**不透明 `MemoryScope`**（字符串值；核心不解释其语义与取值；
  命名与包装形态以实施为准）；
- `MemoryItem` 增 `scope`；
- `MemoryQuery` 增 `scope: Option<MemoryScope>`（`None` = 全部 scope
  合并列表）；
- `forget_matching` 可按 scope 批量；
- `get` / `edit` / `forget` 仍按 id（scope 从条目取）；
- **不做**：核心不引入项目 / 工作区概念；不定义 scope 的来源、推导
  与注入（应用层决定）；不做 scope 转换（边界，后续经组件富面加性）。

### 6. 管理事实携带 scope

- `MemoryEvent::Updated { subject_id, scope, id }`；
- `MemoryEvent::Removed { subject_id, scope, ids }`（删除事实事后可
  定位分区）。

### 7. rewriter 的 history 来源

- 核心从**真相域**取最近成功轮（user / assistant；可过滤失败 / 取消）
  作为 `RewriteInput.history`；与文档"L1 近 3 轮"语义等价；
- 不引入组件读通道（rewriter 契约保持实现中立）。

### 8. 明确不做

- 核心不引入回合序号概念（L1/L2/L3 为队列语义，取尾 / 最近 N 足够）；
- 核心不定义 scope 的获取机制（应用层 / 组件设计）。

## Alternatives Considered（备选与否决）

**模型访问**

1. **组件自持模型**（provider 配置原始句柄，组件自行调用）——否决：
   叙述与 Provider → Agent 回退失效，观测一致性破坏；
2. **读相位重排**（assembler → rewriter，把召回与改写并入 rewriter）
   ——否决：改动大、职责错位。

**消息帧**

3. **形状 1**（背景 + `model_input` 两段式，核心组帧）——否决：文档
   实现下最终提示相同，但"读相位拥有模型输入"更贴合实际需要，且
   避免核心对"最后一条 user"的特殊约定；
4. **帧尾约定**（要求帧以本轮 user 消息收尾）——否决：限制实现组织
   消息的自由。

**降级**

5. **仅契约义务**（核心不兜底）——否决：空帧会让模型看不到输入，
   静默破坏；
6. **核心强校验**（校验帧尾）——否决：回到帧尾约定。

**作用域**

7. **核心枚举 scope 类型**——否决：把应用概念固化进核心；
8. **项目进核心**（会话关联项目 / Project 实体 / runtime 即项目）
   ——否决：偏离框架初衷（框架管通用原语，应用管项目语义）；
9. **scope 仅组件侧**——否决：管理面分裂成两套；
10. **scope 注入由核心定义**——否决：应用层的事，核心不规定。

**事实**

11. **只 `Removed` 带 scope**——否决：两个管理事实不对称；
12. **都不带**——否决：删除事实事后无法定位分区。

**rewriter history**

13. **组件读通道**——否决：rewriter 契约与组件实现耦合；真相域
    读取稳定且语义等价。

## Consequences（后果）

**破坏项（0.7.0 单波）**

- `MemoryContextAssembleOutput.messages` 语义变更（背景 → 完整帧）；
- 核心组帧不再追加原始 input；`Turn.messages` 记录完整帧；
- `MemoryItem` 增 `scope`；`MemoryQuery` 增 `scope`；
- `MemoryEvent::Updated` / `Removed` 增 `scope`。

**加性项**

- assembler / pipeline 的叙述 LLM 句柄；
- `MemoryScope`（不透明）；
- 空帧兜底（原始 input 组帧 + 失败事实）。

**边界（本 ADR 明确不做）**

- 核心项目 / 工作区概念；scope 的来源 / 推导 / 注入；scope 转换
  （后续经组件富面加性）；回合序号；组件设计（ADR-0026）。

**文档义务**

- v8 修订（读相位、执行流、管理面、事实面、扩展点）；
- 0.7.0 实施计划重排；CHANGELOG 与 API 适配说明。

**验证要求**

- 帧含本轮用户消息（改写路径与降级路径）；
- 空帧兜底（原始 input + 失败事实，不静默）；
- 管理面按 scope 列表 / 批量遗忘；事实携带 scope；
- assembler / pipeline 的模型调用经核心叙述（无 `StreamDelta`）；
- rewriter history 取自真相域成功轮。
