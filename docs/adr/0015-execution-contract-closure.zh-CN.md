# ADR-0015: 执行契约封闭与 S2 收敛

- 状态: APPROVED（2026-09-07，irylex 人工评审通过）
- 评审修订: "执行入口同源校验"被**结构性消除**取代——会话改为纯数据
  实体（identity + turns + topic，不持有 runtime），跨 runtime 混用
  在结构上无法造成分裂写入，校验、`belongs_to` 谓词与 panic 路径一
  并删除；持久化由执行链（agent 的 runtime）显式驱动，`end` /
  `context` 操作由调用方显式传入 runtime（2026-09-08，0.2.0 实施
  期修订，未发布）
- 日期: 2026-09-07
- 决策者: irylex（人类确认）
- 性质: 公开 API 重构（破坏性，0.2.0）——执行契约封闭、Context 职责
  升格、记忆流立法、API 卫生与缺陷修复；与 ADR-0014、ADR-0016
  同发 0.2.0

## Context（背景）

ADR-0014 确立单一执行面后，执行契约仍留有未封闭口：`TurnInput` 的
会话引用可选（三个裸 `From` 构造器）、Agent 不持有 Runtime（ADR-0012
的"零环境知识"原则）。

2026-09-07 对全部 14 份 ADR 做了系统性审计，确认这是**模式而非孤例**：
S1→S2 演变中以"收编 / 保留 / 完全不变"处理旧行为而非做出取舍，形成
未封闭决策链。已确诊案例：

- ADR-0012 三处"收编"：裸 TurnInput（§10"一次性路径完全不变"）、
  ConversationHistory（§8"v1 行为收编为内置策略"）、循环裸历史回退
  （违反其自身"一切组装皆策略"戒律）；
- ADR-0011 的落账语义（仅 Completed 入史）、无决策的 fork、push_turn
  公开后门（预留理由" S2c"已被 0012 溶解）；
- 取代链断裂：0002/0003/0007/0008/0011 经多重演变零取代标注；
- 实现层静默回退家族：装配三层吞错、压缩失败全文转写、soft_errors
  无消费终点、persist 丢弃（详见决策六）；
- 一个真 bug：`sweep_stale` 主体重建错误导致空闲超时兜底从未生效
  （详见决策十）。

关键触发：可观测性讨论确立了"**观测是执行的旁路属性**"（LLM 概率性
禁止重执行观测）。由此暴露：无会话 run 是拿不到 Runtime 的孤儿执行
（不可观测、不可溯源）；Agent 不知道自己的 Runtime，则 Runtime 级
服务（Observer）没有全量挂载点。

IDE 产品视角的判断（irylex）：Agent 必须知道自己活在哪个 Runtime；
Context 的行为边界应完整（组装、归档、压缩），不能与循环混裁。

## Problem（问题）

1. **执行双语义**：会话可选（`conv: Option` + 三个裸 `From`）产生
   无会话孤儿 run——不可观测、不可溯源、无记忆服务；
2. **Agent 环境无知**：Runtime 只随会话可达 → Runtime 级服务无全量
   挂载点；且手动挂载模式制造了它想防止的分裂——
   `with_context(conv_A.context()).run(conv_B.turn_input(...))` 背景
   取自 A、轮次记入 B，无校验无报错；
3. **Context 职责劈裂**：组装在 Context、归档/压缩在执行循环
   （循环直捣 `conversation.runtime()` 掏 policies/detector）——概念
   模型（第三持续对象）与实现不一致；
4. **装配数据源双轨**：`ConversationHistory` 直读真相实体 +
   循环硬编码裸历史回退，两条路径绕过记忆层，违反"一切组装皆策略"；
5. **落账语义缺口**：仅 Completed 入史 → Failed/Cancelled 轮零痕迹，
   真相归档缺审计轨迹；
6. **静默回退家族**：装配三层 `unwrap_or_default`（检索失败静默变
   "无记忆"）、压缩 LLM 失败→原始全文充当摘要（无事件）、
   soft_errors 返回后全库无消费、persist 失败 `let _` 丢弃——违反
   "显式可观测"支柱；
7. **API 残留**：`truncate_last`/`clear` 破坏真相归档且与记忆层撕裂、
   fork 无决策依据且同 id 复制（持久化互相覆盖）、push_turn 公开后门
   理由消失、export 边界未声明、ModelParams 公开可设但 agent 路径
   不可达；
8. **投机面与死路径**：三个零产生者枚举变体无触发条件标注、
   TopicShift 默认死路径未文档化、ContextManagement 过期注释；
9. **真 bug**：`sweep_stale` 主体重建错误（Display 全格式二次包装 →
   永远 NotFound）+ 硬编码 `SubjectType::User`。

## Decision（决策）

### 一、会话必选

- `TurnInput.conv` 从 `Option` 改**必选**；删除
  `From<&str>` / `From<String>` / `From<AgentInput>` 三个裸构造器；
  唯一入口 `conversation.turn_input(...)`；
- **会话就是会话**：不存在"一次性任务"这类特殊类别，不需要特殊的
  构造策略或持久化开关——单轮会话就是只有一轮的普通会话，落账与
  持久化和其他会话无差别（持久化与否由应用注册的 Store 决定）；
- 收益：孤儿 run 消失，全量 run 可达 Runtime → 可观测全覆盖，
  "无会话不可观测"边界消失。

### 二、Agent 持有 Runtime

- `AgentBuilder::runtime(&rt)` **必选**（与 `.model()` 同级）；
  `build()` 缺失即报错（`"a runtime is required"`）；
- 三个 preset 签名前置 runtime（与 `Conversation::new(&runtime,
  &subject)` 的"容器先于内容"参数序一致）：
  `Agent::react(&runtime, model, tools)` /
  `Agent::research(&runtime, model, tools)` / `Agent::reflection
  (&runtime, model)`；
- 执行入口 `Arc::ptr_eq` 校验 `agent.runtime` 与 `conversation.runtime`
  同源，错配**显式报错**——ADR-0012 恐惧的"致命且无报错"环境分裂，
  正解是校验+报错，不是无知；
- **显式取代 ADR-0012 "Agent 零环境知识"原则**。翻转理由：① 无知
  挡住 Runtime 级服务的全量挂载；② 无知未消灭分裂，只是上移一层
  （with_context 手动挂载即分裂口）；③ Agent 仍无运行时状态，持有
  的是容器引用（ADR-0007 的"无状态配置体"相应修订表述）。

### 三、Context 升格：完整背景引擎

- `Context` 结构体**保留**，职责补全为三行为：**组装**（`assemble`，
  现有）+ **归档**（L1 写入、主题更新）+ **压缩**（TurnCount/
  L2Overflow 强制保底、可叠加事件策略、L1→L2 摘要、L2→L3 蒸馏）——
  归档与压缩的编排从 AgentLoopTask/trigger 引擎**迁入**
  `Context::on_turn_completed`；
- 压缩所需 LLM 句柄以参数传入（保持"用该 agent 的模型做摘要"现状；
  后台专用模型见推迟表）；
- `Agent::with_context` **删除**——派生取代挂载：执行循环从必选会话
  派生 `conversation.context()`，背景/轮次静默分裂口消灭；
- 循环硬编码裸历史回退**删除**——执行循环三路径收敛为一条；
- 职责边界（本 ADR 确立的最终分布）：

  | 实体 | 边界 | 行为 |
  |---|---|---|
  | Conversation | 真相 | `push_turn`（Turn 落账，含 outcome）、历史读取 |
  | Context | 背景 | 组装 / 归档背景 / 压缩背景，**取材唯记忆** |
  | AgentLoopTask | 编排 | 模型调用、工具、取消、事件发射；三时刻委托 Context |
  | ContextAssembly | 策略 | 只决定"发什么" |

  对称性：**Conversation 归档真相（Turn），Context 归档背景（记忆
  层）**——两种归档，互不越界。

### 四、装配数据源唯一：Memory

- `ConversationHistory` **删除**（类型 + lib.rs 导出）——它直读真相
  实体（`conversation.messages()`），违反"Context 出身于会话、取材
  唯记忆"的边界；纯回放需求未来作为 LayeredMemory 配置表达
  （L1 全窗 + L3 预算 0）——同一数据源的配置，不是第二数据源；
- `AssemblyRequest` **类型锁死**为最小充分集：

  ```rust
  pub struct AssemblyRequest<'a> {
      pub memory: &'a dyn MemoryStore,
      pub subject: &'a Subject,
      pub conversation_id: &'a str,
      pub topic: &'a str,
      pub input: &'a str,
  }
  ```

  策略读会话实体在**类型上不可表达**——边界不靠文档约定，靠类型
  系统锁死；
- 组装策略只在 Runtime 注册，**无 agent/run 级覆灭入口**（YAGNI）。

### 五、落账语义：全入史 + outcome

- `Turn` 增加 outcome 字段（`Completed` / `Failed` / `Cancelled`）；
- **全部轮次入史**——失败与中断也是真相，审计轨迹不丢（取代
  ADR-0011 "仅 Completed 入史"）；
- 记忆触发面相应定义：**仅成功轮写入 L1**（失败轮的上下文不污染
  记忆层）；`Context::on_turn_completed` 的触发面随之封闭。

### 六、记忆流失败不得静默（立法）

- **原则**：记忆/装配路径的任何失败必须可见，不得静默降级为
  "无记忆"或静默跳过；
- 消单（随本 ADR 实施）：装配三层 `unwrap_or_default`（L1/L2/L3
  检索失败）、压缩 LLM 失败的全文转写回退（改为显式降级并记录）、
  soft_errors 的消费终点（当前全库无人读）、persist 失败丢弃、
  sweep list 失败静默 0；
- 可见性机制（事件形态、上浮通道）与 **ADR-0016 Observer 联动
  设计**——本 ADR 立法原则，0016 给机制。

### 七、Conversation API 收敛

- `truncate_last` / `clear` **删除**——窗口管理归 Context（L1 策略
  +压缩），真相归档不提供破坏性操作；数据清理是存储层事务；
- `fork` **删除**——无决策依据 + 同 id 复制缺陷；S3 分支场景真实
  出现时带完整决策回归（新 id、记忆层分叉语义一并设计）；
- `push_turn` **行为保留、pub → pub(crate)**——它是真相归档的唯一
  写入点（框架所有）；公开理由"S2c 记忆注入"已随 0012 溶解，测试
  走真实执行路径；
- `export` **保留**，文档明示边界：导出的是真相轮次（可迁移）；
  记忆层（L2/L3）与主题是 MemoryStore 自己的事务，不随行。

### 八、ModelParams 归位：model 行为

- 判断依据（全链路事实）：请求级 params 的生产点只有两处——推理
  调用永远 `default()`（agent 路径无入口可设），压缩调用
  `with_max_tokens(256)`（全系统唯一非默认使用）；适配器逐请求消费
  但消费到的恒为默认值——**管道通了，源头没阀门**；
- **params 绑定适配器构造**（irylex：参数是 model 行为的一部分，
  随 model 传入，不传走默认）：

  ```rust
  OpenAiModel::builder().api_key(k).temperature(0.3).build()
  AnthropicModel::builder().api_key(k).max_tokens(1024).build()
  ```

- `ModelRequest` **删除 params 字段**（瘦身 `{messages, tools}`）——
  契约纯化，不留内部覆灭通道；`ModelParams` 类型保留，语义改为
  适配器构造配置（provider 特有参数放各自 builder）；
- 压缩 256 预算护栏暂由 agent 模型绑定参数承担；正式解法 =
  **后台工作专用模型**（见推迟表，真实触发已现）。

### 九、投机面与死路径处置

- 三个零产生者变体**保留 + 标注触发条件**（irylex 判断；
  `#[non_exhaustive]` 保护下保留成本低）：`CancelReason::Parent`（S3
  上游取消传播）、`CallPurpose::Classification`（S3 意图路由）、
  `SubjectType::Agent`（S3 多 Agent 主体）——注释补触发条件与
  产生者形态；
- `ContextManagement` 的 "(future, S2)" 过期注释**修正**（M11b 已
  兑现）；
- TopicShift 默认死路径**文档化**：FirstSegmentDetector 语义 =
  首轮建立主题后恒不判定 shift；TopicShift 策略需注册自定义
  TopicDetector 激活；
- `Agent::with_timeout` **保留**——显式 opt-in 不属于 ADR-0004 否决
  的"框架内建默认超时"；0004 补注澄清此边界。

### 十、缺陷修复

- `sweep_stale` 主体重建错误随本 ADR 实施修复（持久化/重建的
  subject 编解码对称化），补测试覆盖 User 与 Agent 两种主体；
- 硬编码 `SubjectType::User` 改为使用会话状态中记录的真实主体类型。

## Alternatives Considered（备选方案）

| 备选 | 否决理由 |
|---|---|
| 保留无会话路径（S1 兼容） | 孤儿执行不可观测不可溯源；每一次执行都是会话中的一轮，不存在会话之外的执行 |
| Agent 不持 Runtime（维持 0012 零环境知识） | Runtime 级服务无挂载点；分裂恐惧的正解是校验+报错，无知只是上移分裂 |
| Context 解散（assembly 直接从会话取） | Context 有特定行为边界（组装/归档/压缩），职责不能混入其他实体 |
| ConversationHistory 保留为兼容策略 | 双数据源长期共存；纯回放 = 配置不是第二来源 |
| truncate/clear 保留并同步清理记忆层 | 复杂度上升；数据清理是存储层事务，真相归档不应提供破坏性操作 |
| fork 修复保留（新 id） | 无决策依据的功能带缺陷，删除比带病保留诚实；S3 带完整设计回归 |
| ModelParams 挂 AgentBuilder | params 是 model 行为不是执行配置（irylex 判断） |
| ModelRequest 保留 params 内部通道 | 后门仍在，契约不纯；同一配置两个来源即双语义 |
| 三个投机变体删除 | 保留+标注成本低（non_exhaustive 保护），S3 到来时零成本启用 |
| 落账维持仅 Completed | 失败/中断轮零痕迹，真相归档缺审计轨迹 |

## Consequences（后果）

### 正面

- 契约封闭：无双语义、无孤儿执行、无裸路径、无后门；
- 概念对齐：Context 三行为完整，四实体边界清晰（真相/背景/编排/
  策略）；
- 可观测完整：全量 run 可达 Runtime；记忆流失败可见（立法）；
- 真相归档完整（全入史 + outcome）；
- 消灭一个真 bug（sweep_stale）；API 面收敛（删 4 个公开项、
  收 1 个公开性、瘦 1 个契约结构）。

### 成本与义务

- **0.2.0 破坏性**（与 0014/0016 同发）：`TurnInput` / `AgentBuilder`
  / 三 preset / `ContextAssembly` 契约 / `ModelRequest` /
  `Conversation` API / `Turn` 结构全面变更；
- 迁移清单扩大：全部裸字符串调用、`with_context` 使用者、
  `ConversationHistory` 注册者（memory.rs 测试改自定义策略）、
  两个适配器的 params 消费点、`ModelRequest` 全部构造点、
  120 测试全面回归；
- 实施顺序：**0014 → 0015 → 0016**，同发 0.2.0；
- 压缩预算护栏暂缺（由后台模型推迟项承接）；
- **取代链补注义务**：本 ADR 定稿时统一补注 0002 / 0003 / 0004 /
  0006 / 0007 / 0008 / 0011 / 0012 的取代关系（参照 0013 头部格式，
  旧文不改写内容）。

### 关联

- 前置 ADR-0014（单一执行面）——本 ADR 是其契约封闭的纵深；
- **取代 ADR-0012 四项**：零环境知识原则、ConversationHistory 收编、
  裸路径保留（§10）、with_context 挂载模式；0012 的触发体系、
  三兄弟契约、装配格式不变；
- **取代 ADR-0011 多项**：落账语义（仅 Completed）、行为面
  （truncate/clear/fork）、push_turn 公开性；
- ADR-0016（Observer）以本 ADR 的"全量 run 可达 Runtime"与
  "记忆流失败可见"为前提。

## 推迟项

| 项 | 触发条件 |
|---|---|
| **后台工作专用模型**（Runtime 注册，承担压缩/摘要/主题检测的参数预算） | **已触发**（压缩护栏暂缺），0.2.0 后优先落地 |
| 纯回放配置（LayeredMemory L1 全窗 + L3 预算 0） | 用户真实需求出现 |
| fork 完整设计（新 id + 记忆层分叉语义） | S3 分支场景真实出现 |
| 组装策略覆灭入口 | 真实需求出现（当前无覆灭，刻意） |
| 纯文本适配器（execution.text()） | 用户明确需求（0014 已列，维持） |
| 会话级事件类别 | 真实需求出现（0011 决策 9 维持） |
