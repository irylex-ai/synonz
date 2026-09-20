# ADR-0022: 记忆组件边界与核心契约——抽象族、抽象类流水线与公开面重切

- 状态: APPROVED（2026-09-20，irylex 人工评审通过）
- 日期: 2026-09-20
- 决策者: irylex（人类逐点确认；本 ADR 每个决策均经弹窗逐项确认，
  遵循 `.opencode/rules/architecture.md` §5–§9 的渐进决策流程）
- 性质: 记忆子系统组件化边界 + 框架核心契约重切 + 公开面破坏；
  随 0.7.0 单波承载
- 关联: 承接 ADR-0012（记忆与上下文系统——三层模型保留，分层机制
  迁入官方组件）；重定位 ADR-0019（上下文引擎扩展面——策略槽不再
  公开，引擎收为内核内置）；调整 ADR-0020（记忆管理面——条目级
  管理抽象保留，归属与只读代理重新定义）；遵循 ADR-0009（插件化
  扩展原则——契约在原语层、实现在适配层）
- 版本承载: 0.7.0（"0.6.0 记忆面补正"；无数据迁移）
- 前提: 无旧数据迁移——0.6.0 序列化数据不支持；本 ADR 只定框架侧
  （核心与组件的边界、核心契约族、公开面变化与迁移）；
  `synonz-layered-memory` 组件的实现细节另立 ADR；条目视图与记忆
  单元语义（topic / unit_key 等）入 ADR-0023

## Context（背景）

0.6.0 发布后的实际产品开发（AI 原生 IDE 的项目记忆）暴露出**责任
错位**：框架承诺机制（同键原地更新、主题不变式、分组压实、失败
可见），但这些机制所依赖的语义步骤（话题探测、摘要、蒸馏）以
细粒度策略槽下放给开发者。结果是——开发者无法对整套记忆机制
负责，框架也无法保证其承诺在整体上成立。

同时确认：**分层记忆（L1/L2/L3 + 压实/蒸馏/促进 + 条目管理）是
框架的差异化能力**，适合作为官方组件独立演进（`synonz-layered-
memory`），而框架核心只保留契约与内置引擎。这样责任边界清晰：
**机制与担保归核心契约，记忆语义归组件实现**。

本 ADR 只讨论框架侧边界与契约。组件实现、条目视图与记忆单元语义
分别另立 ADR；框架遵循"契约在原语层、实现在适配层"的插件原则
（ADR-0009）。

## Problem（问题）

1. **责任半开**：机制承诺由框架给出、语义实现由策略槽承担，两者
   之间没有清晰的边界对象；细粒度槽既暴露机制细节，又无法保证
   一致性；
2. **契约缺失**：核心需要一个边界清晰、可替换、可组合的记忆契约
   族（管理面、读面、写面、工厂），并保证模型叙述、可观测、任务
   排空等担保不被实现者绕过；
3. **公开面重切**：`Context` 与旧策略/载荷类型的历史公开面必须
   收敛；迁移方式与版本承载需要结论。

## Decision（决策）

### 1. 边界总则：核心持有契约与内置引擎，组件实现分层机制

- **核心**：Agent 循环、模型与工具契约、会话真相记录、事件与
  调度、内核内置引擎（相位编排、模型解析与叙述）；
- **组件 `synonz-layered-memory`**：分层机制（L1/L2/L3、身份、
  存储契约与内置实现、策略、管理面实现、test-util、默认策略）；
- **责任不再半开**：核心对契约与担保负责，组件对记忆语义负责。

### 2. 核心契约组成：三个抽象 + 两个工厂

- **`Memory`**：条目级管理抽象（应用面；`list` / `get` / `edit` /
  `forget` 等动词沿用既有语义），由组件的 `LayeredMemory` 实现；
- **`MemoryContextAssembler`**：上下文组装抽象（读），由组件的
  `LayeredMemoryContextAssembler` 实现；
- **`MemoryPipeline`**：记忆流行为抽象（写），由组件的
  `LayeredMemoryPipeline` 实现；形态为**抽象类**（模板 + 业务钩子，
  见 §6）；
- **`MemoryProvider`**：工厂，行为 `memory()` / `context_assembler()`
  / `pipeline()`；并持有上下文管理模型配置（见 §4）；
- **`RewriterProvider`**：工厂，行为 `turn_input_rewriter()`（可选
  `model()`）；`TurnInputRewriter` 契约名不变。

### 3. `Context`：内核内置、不再公开

- `Context` 留在核心，是**内置引擎**：编排读相位（rewriter →
  assembler）与写相位（pipeline 模板），并完成模型解析与叙述；
- Agent 不再配置 `Context`；其旧的扩展槽（读/写策略、分层参数）
  不在核心配置面——分层参数与策略归组件；
- `Context` 收为 crate 内部，**不再作为公开类型**；
- 依据：其配置面已被契约取代，公开类型无面向应用的用途；仓库内
  无外部引用（examples 与适配 crate 均未使用）。

### 4. 模型：配置在 Provider、使用时解析、核心边界叙述

- **上下文管理模型配置在 `MemoryProvider`**（承接原
  `Context::with_model` 的用途，随记忆能力走，runtime 级一致）；
- 核心在**使用时**解析：**Provider 模型优先，未配置回退 Agent
  模型**；rewriter 同规则（`RewriterProvider` 模型优先，回退 Agent
  模型）；
- **所有模型调用在核心边界统一包叙述**，实现者不接触事件对象；
  Provider 通过模型访问方法把配置交给核心（机械性补齐）。

### 5. 物料化契约（读）

- **`MemoryContextAssembleInput`**：`reader`（`Memory` 的只读代理）、
  `subject`、`conversation_id`、`topic`、`input`、`rewritten_input`；
  `non_exhaustive` 预留材料域成长；
- **`MemoryContextAssembleOutput`**：`messages`（背景消息）与
  `failures`；
- **`reader` 由 `Memory` 抽象产出**（如 `reader(&subject)`）；核心
  从已持有的 `Arc<dyn Memory>` 取得，放入载荷；
- **`topic` 属核心**（会话属性）；话题的探测策略在组件（§6）。

### 6. 流水线契约（写）：抽象类（模板 + 业务钩子）

- 核心 trait 提供**模板方法**（轮末、会话终结），机制内置：写回
  topic、发射生命周期事实、派生后台任务；
- **业务钩子**（实现者提供，统一 async）：
  - `detect_topic`：探测话题（可失败）；
  - `archive_turn`：归档本轮；
  - `spawn_task`：派生后台维护任务（经上下文 `spawn`）；
  - `finalize_conversation`：会话终结收尾；**核心先有界排空会话
    任务，再调用**；
- **上下文** `PipelineTurnContext` / `PipelineConversationContext`：
  框架提供的契约，暴露数据读取与语义机制入口——`set_topic`（写回
  会话）、`spawn`（登记后台任务）、`report_failure`（失败转事实）；
  `EventSink` 与 `ConversationTaskSpawner` **不出现在实现者可见面**；
- **生命周期事实（归档、话题切换、失败等）由框架模板内部发出**；
  实现者只描述"业务上发生了什么"。

### 7. 失败表示与语义：字符串阶段 + 尽力而为

- **`MemoryFailure { stage: String, detail: String }`**：阶段标识由
  实现者定义，核心不解释；
- 后台任务结果为 **`Result<(), MemoryFailure>`**；核心把失败转成统一
  失败事实；
- **模板对钩子失败采用尽力而为**：每个钩子独立执行，失败经
  `report_failure` 上报后继续后续步骤（探测失败保留旧 topic 继续
  归档；归档失败仍派生后台任务）。

### 8. 异步形态：生命周期行为统一 async

- 模板、四个钩子、`MemoryContextAssembler::assemble`、
  `TurnInputRewriter::rewrite` 统一 async（返回 future）；
- 管理动词与工厂行为保持同步；
- 依据：钩子是扩展点，实现内部可能引入模型/IO；一次定型，避免
  后续破坏式调整。

### 9. 管理面接入：`runtime.memory()`

- **`runtime.memory() -> Arc<dyn Memory>`**；Runtime 在 build 时从
  `MemoryProvider` 取得并持有；
- 应用入口集中，且不依赖具体 provider 类型（应用无需长期保存
  provider 句柄）。

### 10. 公开面与迁移：硬移除、0.7.0 承载、无数据迁移

- **旧公开类型硬移除**（不留别名）：`Context`（收内部）、
  `ContextAssembler` / `ContextAssemblerInput` /
  `ContextAssemblerOutput`、`MemorySummarizer`、`MemoryDistiller`、
  `ConversationTopicDetector`、`TopicDecision`、`AssemblyFailure`、
  `MemoryFlowError`、`MemoryFlowStage`；
- **迁移说明**逐项给出"旧 → 新 / 替代做法"；
- **0.6.0 序列化数据不支持**（无数据迁移）；版本承载 0.7.0。

## Alternatives Considered（备选与否决）

**边界与组件化**

1. **核心持有分层契约**（L1/L2/L3 类型与存储契约留在核心，组件仅
   实现）——否决：把实现细节升格为长期契约；核心应持条目级契约，
   分层机制是组件能力；
2. **记忆与上下文全部留在核心、组件仅承载策略**——否决：责任仍
   半开，机制承诺与语义实现依旧无法分属；
3. **通用插件系统**（动态加载、注册表、生命周期）——否决：无真实
   需求，ADR-0009 已把通用插件机制挂起。

**接线与工厂**

4. **应用创建 + 构建期挂载宿主服务/注入对象**——否决：引入额外的
   注入协议与双态对象；宿主服务（模型、任务表）由上下文语义入口
   吸收，不需要暴露；
5. **工厂闭包 / 核心创建并下转型取回句柄**——否决：取回形态别扭
   （泛型化或向下转型），双态对象；
6. **管理面从 Provider 取**——否决：应用需长期保存 provider 句柄，
   且核心抽象却从组件工厂取回，语义不一致。

**模型与叙述**

7. **上下文管理模型归 Agent 配置**——否决：记忆模型属记忆能力，
   runtime 级一致；配置应随能力走；
8. **模型打进 Provider 的产出捆绑**——否决：配置与产出混淆，
   行为签名变形；
9. **rewriter 的模型经闭包注入 / builder 内配置由核心回填**——
   否决：协议重且不自然；改为材料层入参 + `RewriterProvider` 声明，
   核心在使用时解析并叙述。

**流水线形态**

10. **单插头**（一个对象同时暴露读与写）——否决：接口过宽，读/写
    受众不同；
11. **两个松散插头**（读对象与写对象分别注册）——否决：存在错配
    风险；改由同一工厂成对产出；
12. **钩子按需 async**——否决：形态不齐，未来同步钩子引入模型即
    破坏契约；
13. **失败即止**——否决：单点失败放大为整轮停止，与"不丢数据、
    可见降级"精神相悖；
14. **通用事实出口**（上下文暴露 `emit(fact)`）——否决：事实词汇
    进入实现者视野，观测一致性失去中心化；
15. **枚举阶段**（核心定义阶段枚举）——否决：核心需维护实现方的
    阶段集合，扩展僵硬，与"核心不解释实现细节"取向冲突。

**公开面**

16. **`Context` 保持公开（不可配置）**——否决：无配置用途的公开
    类型易被误用，且内部引擎形态被公开契约长期绑定；
17. **旧类型保留过渡别名**——否决：旧概念名长期驻留公开面，产生
    两套名字；部分语义变化无法用别名表达（如失败阶段由枚举变
    字符串）。

## Consequences（后果）

**破坏项（0.7.0 单波）**

- `Context` 收为 crate 内部；旧的策略槽与载荷类型硬移除（§10 清单）；
- `MemoryReader` 定位重定义：`Memory` 的只读代理，由 `Memory`
  产出；
- 公开面重切为新契约族：应用经 `runtime.memory()` 与契约编程，
  不再经 `Context`；
- 0.6.0 序列化数据不支持（无迁移）。

**加性项**

- 公开抽象：`Memory`、`MemoryContextAssembler`、`MemoryPipeline`
  （模板 + 钩子）、`MemoryProvider`、`RewriterProvider`；
- 载荷与上下文：`MemoryContextAssembleInput` /
  `MemoryContextAssembleOutput`、`PipelineTurnContext` /
  `PipelineConversationContext`、`MemoryFailure`；
- 入口：`runtime.memory()`；`TurnInputRewriter` 材料入参。

**边界（本 ADR 明确不做）**

- `synonz-layered-memory` 组件的实现细节（另立 ADR）；
- 条目视图与记忆单元语义（topic / unit_key / 条目字段——ADR-0023）；
- 后台分层进展的观测（组件观察面，组件 ADR）；存储与分层参数
  配置（组件内部）；
- 通用插件系统（ADR-0009 挂起保持不变）。

**文档义务**

- v8 架构文档（v7 转 SUPERSEDED）；
- 0.7.0 实施计划；
- 迁移说明：逐项"旧 → 新 / 替代做法"（API 适配；声明 0.6.0 数据
  不支持）；
- 扩展指南更新：契约用法、上下文语义入口、模型叙述边界；
- README 指针；CHANGELOG。

**验证要求**

- **公开面（编译期）**：`Context` 与旧类型不可见；`EventSink` /
  `ConversationTaskSpawner` 不出现在实现者可见签名；
- **模型**：实现者的模型调用全部经核心叙述（Provider 模型 → 回退
  Agent 模型）；
- **流水线**：尽力而为语义（探测失败保留旧 topic 继续；归档失败
  仍派生任务）；会话终结前有界排空；
- **物料化**：`reader` 由 `Memory` 产出、`topic` 来自核心；
- **管理面**：`runtime.memory()` 闭环可用；
- **迁移**：0.6.0 数据不支持的边界在文档中声明。
