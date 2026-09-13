# ADR-0017: 事件总线与反应架构——EventBus、MemoryCurator 与 SynonzEvent

- 状态: APPROVED（2026-09-09，irylex 人工评审通过）
- 日期: 2026-09-09
- 决策者: irylex（人类逐点确认）
- 评审修订（DRAFT 期开放项决议落地，实施前）：①创建签名统一带
  runtime，`ConversationCreated` 升在册；②`TopicShifted` 升在册；
  ③`round: Option<usize>`（None=循环外维护调用）；④Memory 存储契
  约拆三层独立位（`MemoryL1Store`/`MemoryL2Store`/`MemoryL3Store`，
  L1 默认内置内存实现）；⑤`SynonzEvent: Serialize + Deserialize`
  立法（两级 tag，延续 ADR-0003）
- 评审修订二（v4 架构评审，2026-09-09，实施前落地）：⑥**MemoryCurator
  契约退役**——记忆策展能力并入 **Context 状态引擎**（`trait
  Context`：`assemble` + `on_turn_completed` 两方法，Agent 级注册
  AgentBuilder；多 Agent 策略差异；预设打包引擎配方）；三策略槽
  `ContextAssembler` / `MemorySummarizer` / `ConversationTopicDetector`
  + 楼层参数（builder 数值）；⑦**终结收尾去策略化**——
  `on_conversation_ended` 从契约删除，终结 = Runtime 结构行为
  （drain 会话维护表 + 机械促进 L2→L3，init 模式）；自定义终结走
  观察位（订阅 `ConversationEvent::Ended`）；总线行动车道立法保留
  （S3 预留，0.3.0 无派发点）；sweep_stale / conversation_idle_timeout
  维持现状为过渡态——框架内无自动调用方构成生命周期完备性缺口，
  系统级自动监控归 **Monitor 机制**（独立 ADR-0018，0.3.0 实施后
  启动）；⑧**EventPolicy 退役**（被总线体系
  结构性吸收：终结促进 = 结构行为、漂移冲刷 = 漂移的语义后果默认）
  与 **MemoryPolicies 退役**（楼层参数化）——RuntimeBuilder 收敛
  五主位（conversation_store / memory_l1_store / memory_l2_store /
  memory_l3_store / observer，方法名 = 类型指代名）、零便捷位；
  ⑨**Memory 一等持有对象**（Runtime build() 组装三存储位、唯一持有、
  `runtime.memory()` 唯一出口；行为与数据分离——引擎经载荷接收，
  Runtime 为编排点；后台任务登记 Runtime 会话维护表，引擎无实例
  状态）；⑩**命名族立法**：契约 = 行动者（-er）、实现 = 特征 +
  契约名对照、Input ↔ Output 对偶、主语补全、单数指代、Event 术语
  去污；默认实现一律内化（契约公开、实现 pub(crate)）：
  `LayeredMemoryContextAssembler` / `PromptMemorySummarizer` /
  `FirstSegmentTopicDetector`；⑪**上下文 = Agent 状态统合概念**
  （记忆 ⊂ 上下文 + 环境感知 + 运行时要素——Agent 有状态的载体）；
  装配 = 完整上下文物化，`ContextAssemblerInput` 原料域
  `non_exhaustive` 开放，"取材唯记忆"原则修订为"**装配不读真相域**"
  （背景与真相分离立法不变）；Conversation 独立于 Agent（1~N 参与，
  S3 友好；多引擎交替维护的一致性 = 应用/编排层责任）
- 评审修订三（ADR-0018 批准后补录，2026-09-11）：⑫**Monitor 延期
  记录兑现**——评审修订二⑦所述生命周期完备性缺口（过渡态
  `sweep_stale` / `conversation_idle_timeout` 无框架调用方）由
  ADR-0018 终结：公开 `Scheduler` 组件 + 内部系统调度器、Monitor
  空闲清扫（含崩溃孤儿对账）、显式 `shutdown()`、统一会话表、
  ConversationStore 查询面升级；总线词汇表与观察位契约不变
- 评审修订四（ADR-0019 批准后补录，2026-09-13）：⑬**引擎契约与配置面
  更新**——评审修订二⑥所述 `trait Context` 退役（ADR-0019：具体
  `Context` + 读/写两相位扩展面）；本文正文 §"配置面（浅定制）"的
  `with_summary_prompt`/`with_summary_model` 已失效——分别移除与改名为
  `with_model`（缺省 = agent 模型），装配输入 `memory: &Memory` 改为
  只读 `reader: MemoryReader`；总线词汇表、观察位契约、装配不读真相域
  等立法不变（`MemoryFlowStage` 增 `Rewrite` 变体为 0.5.0 附加项）
- 性质: 反应扩展性地基——新增总线设施与 Curator 契约，重整事件
  词汇表（含既有类型改名与迁移），破坏性变化随 0.3.0 单波发布
- 关联: 承接并修订 ADR-0015（记忆流触发形态）、ADR-0016（旁路
  派发合一于总线）；修订注记于本 ADR 达到 APPROVED 时补录
- 版本承载: 0.3.0（0.2.0 跳过 crates.io 发布，下游只见一次破坏波）

## Context（背景）

ADR-0015 完成执行契约封闭后，`Conversation::end(&runtime)` 的设计
缺陷暴露：纯数据会话在方法体内拉取 runtime 的记忆策略与存储、
内联编排 L2→L3 促进——数据实体越界执行了记忆业务。修复讨论逐步
升级为对反应架构的地基性审视：

1. **无总线的困境**：框架没有任何"事实发生→订阅者反应"的机制。
   EventPolicy（TopicShift/ConversationEnd）是策略标志在硬编码
   检查点上的内联条件调用，不是事件驱动；会话生命周期事实
   （创建/终结、压缩/蒸馏的发生）对任何旁观者完全不可见；
   `sweep_stale` 的失败路径存在四层静默黑洞（list 失败静默返回
   0、parse/of 失败静默跳过、软失败静默丢弃）。
2. **扩展性地基诉求**（irylex 硬要求）：底层架构必须让下游在不
   修改框架的前提下注册行为性反应（IDE 审计、任务联动、自定义
   记忆策略），否则后续又是 API 大改。
3. **已验证的雏形**：ADR-0016 的 EventTap 在单类型上已经长出
   "同步交付 + 旁路观测"的双车道形状——本 ADR 将其升格为通用
   事件骨架。讨论历程历经"场景→主体→设施"的收敛：先确立两个
   反应主体（Observer 既有 / MemoryCurator 新立），再统一其
   投递设施为总线——总线不是从概念先验发明的，是从已验证的
   双车道形状与已定的主体契约中收敛出来的。

## Problem（问题）

1. 反应（事实发生后触发行为）不是一等扩展点：记忆流是框架内联
   函数（有行为无身份），下游无法替换或新增反应主体。
2. run 外事实（会话终结、记忆流转、后台维护失败）无任何可见性
   通道；sweep 失败黑洞直接违反 never-silent 立法。
3. 会话生命周期主权分裂：创建时 runtime 不在场、终结入口越界、
   "已终结"状态无人记录（重复 sweep、无状态门依据）。
4. 事件词汇表按投递机制（run 内/外）分裂而非按领域实体组织；
   `MemoryFlowFailed` 两栖、`AgentEvent` 名不副实（实为 Turn
   叙事）。

## Decision（决策）

### 1. 范围四分解

"事件驱动"承载的四种工作分离，本 ADR 只立前两种的设施：

| 工作 | 归属 |
|---|---|
| ① 叙事（事实记录） | 本 ADR：总线观察车道 |
| ② 反应（事实触发的行为） | 本 ADR：总线行动车道 + Curator 契约 |
| ③ 控制流影响（拦截/改写执行） | 另立 ADR（暂无场景；middleware 性质） |
| ④ Agent 间通信 | S3 的 ADR（总线为其留缝：词汇表可扩展） |

### 2. 概念模型与术语立法

四概念（Java OO 直觉经 Rust 语言特性检验成立）：

- **EventBus**：Runtime 持有的派发设施——感知事件（被 emit 通知）
  → 派发给订阅位。与发射端是使用关系而非组成关系：它对"谁在
  告知、告知之余还做了什么"零知识。
- **EventSource**：通知总线的主体（执行循环、`Conversation::end`、
  sweep、Curator 流转）。**其定义性动作只有一个：emit。**
- **EventListener（订阅位）**：观察车道订阅者（N 个）与行动车道
  契约位（单实例）。
- **Event（SynonzEvent）**：总线承载的事实类型，入参类型锁死。

术语立法（唯一性）：

- **事件驱动模型 = 总线体系，有且仅有**。凡事件驱动皆经总线；
  交付不经总线，故不是事件驱动。
- **交付不是事件驱动**：产品消费者接收交付（ExecutionEvent 流，
  点对点、背压、per-run）是执行面固有的输出管道——如同函数
  返回值不叫事件驱动。交付不因总线存在与否而变化。
- 执行循环有两个无关身份：作为执行面交付叙事（非事件驱动）；
  作为 EventSource 通知总线（事件驱动）。
- Rust 落地三调整（OO 直觉的语言迁移）：事件需 `Clone`（重载荷
  用 `Arc` 包裹内部字段）；订阅生命周期用所有权表达（订阅端
  drop 即退订）；`emit` 非阻塞（旁路 try_send，绝不阻塞发射方）。

### 3. 总线形态

```
EventSource ──emit──→ EventBus（Runtime 持有）
                        ├── 观察车道：旁路·异步·丢弃+lag 计数·N 个
                        │   Observer（熔断隔离·保序·终态后 drain）
                        └── 行动车道：同步 await·契约位单实例
                            MemoryCurator 位（未来新行动位=新 ADR 立法）
```

- **双车道语义**：行动者同步等（结果可回收），观察者旁路看
  （绝不拖慢执行）。per-run 派发器与常驻通道两套设施合一为
  **总线常驻派发器**（run 事件归因移入事件载荷/上下文）。
- **订阅位制**：观察位 N 个（下游自由注册——扩展性的自由面）；
  行动位每契约单实例（记忆策展唯一哲学——两个 Curator 同时
  促进即重复执行）。扩展的两条路径：观察位注册（下游）、
  新行动契约位（框架立法，非破坏）。
- **事件类型模型**：`SynonzEvent` 封闭枚举（`non_exhaustive`），
  总线 publish/subscribe 入参类型锁死——Rust 类型系统在编译期
  拒绝非框架事件（结构性封闭，同 `AssemblyRequest` 类型锁死
  先例；"哪怕长得像也无法上总线"）。下游自定义事件进推迟表；
  新事件变体 = 新版本特性（版本化的特性发布节奏）。
- **序列化立法**：`SynonzEvent: Serialize + Deserialize`（延续
  ADR-0003"事件可序列化"立法）——审计落盘、事件导出、未来的
  跨进程观测都依赖此契约承诺。tag 结构两级（顶层 `type`=实体
  族、族内 `event`=变体，与现有模式一致）；版本演化遵守 serde
  兼容性惯例（加字段带默认值，不删不改名）。
- **交付/订阅分离**：EventTap 回归纯交付零件（实施期正名），
  与总线无隶属关系；发射顺序立法保留（总线先感知，订阅者
  不落后于交付）。
- **同实体多子族的 Rust 结构**：每实体一个顶层变体，子族在实体
  枚举内部扩展（Rust 枚举变体名唯一性约束 + 顶层实体词汇表
  跨版本稳定，扩展发生在内层）。

### 4. MemoryCurator（记忆策展人）

与 Observer（观察者）对标的反应主体：**订阅会话生命周期事实、
按策略执行记忆流转的行动者**。trigger.rs 是它的无身份内联形态。

**契约（两锚点，双重中立）**：

```rust
#[async_trait]
pub trait MemoryCurator: Send + Sync + 'static {
    async fn on_turn_completed(&self, ctx: &TurnContext<'_>)
        -> Vec<MemoryFlowError>;
    async fn on_conversation_ended(&self, ctx: &ConversationEndContext<'_>)
        -> Vec<MemoryFlowError>;
}
```

- 两锚点 = 领域事实最小完备集（轮次完成、会话终结），从现有
  两个真实调用点（`run_post_turn_flows`/`run_end_flows`）归纳，
  非设计发明。
- **契约中立第一课**：分层哲学（L1/L2/L3、楼层、递进逻辑）不下沉
  契约——hook 命名不得预设记忆模型，换记忆哲学（时间衰减、
  统一向量库）的下游零门槛。
- **契约中立第二课**：时序工程（后台化、顺序保证、延迟优化）
  不下沉契约——那是实现的家务，框架无需 join 点知识。
- 载荷：`TurnContext`（conversation/input/messages/`Memory<'_>`
  聚合/model/events——自 `PostTurn` 参数对象正名公开，字段锁死）；
  `MemoryPolicies` 与 `TopicDetector` 下沉为默认实现的私有依赖
  （自定义 Curator 有自己的策略体系，不被迫继承）。
- 依赖注入方向：Curator 从 Runtime 取——但取的主体是总线派发器
  （框架代码），**不是 Conversation 或任何实体**（实体点名行为
  主体即隐性业务耦合回归）；Conversation 只见通道的窄接口
  （emit），依赖链单向无回环。

**默认实现（主体化搬移，零逻辑重写）**：

- `DefaultCurator::new(policies, detector)`——trigger.rs 的
  `run_post_turn_flows`/`run_end_flows` 函数体逐行搬入两锚点；
  已批准的分层 ADR 规格（L1 窗口/L2 cap/递进逻辑/策略叠加）
  完整保持为默认实现的实现规格。
- **配置面（浅定制）**：`with_summary_prompt`/`with_summary_model`
  ——最常见定制（换摘要方式）零委托、纯配置。
- **步骤面（深定制零件）**：`advance_topic`/`archive_l1`/
  `compact_l1_to_l2`/`distill_l2_to_l3`/`promote_l2_to_l3`
  （compact/distill 接 owned Job 载荷，spawn 友好——后台任务
  不能借 ctx）。
- **时序内化**：同步段（归档+主题，即时感知档）+ 后台段（压缩+
  蒸馏，近期感知档，spawn + EventSink 克隆保活投递）；终结锚点
  先 drain 后台队列再促进（顺序保证在实现内部，L2 无未落地块）。
- **压缩竞态改序**：摘要→append→pop（L1 超窗=感知滞后，分层
  本义；append 后 pop 原子收口，无记忆空洞）。

**开发者三档路径**：零扩展（默认，90% 场景）/ 浅定制（配置委托）/
深定制（组合委托——自管后台与顺序，文档明确此责任）。

**注册**：`RuntimeBuilder::memory_curator`（全量替换）+
`memory_policies`/`topic_detector`（默认便捷位；设置 curator 后
被忽略，文档写明）。

**Memory 存储契约（三层独立位 + 聚合视图）**：

扩展性需求（irylex）与现实铁律：分层存储天然异构——常规部署
L1 纯内存、L2 内存+Redis、L3 向量库/图库。一体契约违反接口
隔离原则（想换 L3 被迫重写 L1/L2 的实现——"包三层"的适配器
脏活）。按层拆分为独立契约位，每层伴随自己的存储扩展：

```rust
// 存储侧：三个独立契约位，RuntimeBuilder 各一个注册位
pub trait MemoryL1Store: Send + Sync + 'static {
    fn append(&self, ..) -> Result<..>;  // 逐层 CRUD（方法名不重复层名）
    fn len(&self, ..) -> Result<usize>;
    fn pop_oldest(&self, ..) -> Result<..>;
}
pub trait MemoryL2Store: Send + Sync + 'static { /* append/len/pop_oldest */ }
pub trait MemoryL3Store: Send + Sync + 'static { /* upsert/query */ }

// 使用侧：Memory 聚合视图——领域概念的落位
pub struct Memory<'a> {
    pub l1: &'a dyn MemoryL1Store,
    pub l2: &'a dyn MemoryL2Store,
    pub l3: &'a dyn MemoryL3Store,
}
impl Memory<'_> {
    // 现有调用形态保持（转发到对应层位）
    pub fn l1_append(&self, ..) -> Result<..> { self.l1.append(..) }
    pub fn l2_append(&self, ..) -> Result<..> { self.l2.append(..) }
    pub fn l3_upsert(&self, ..) -> Result<..> { self.l3.upsert(..) }
    // …
}
// Curator/Assembly 拿到聚合：memory.l1_append(..) —— 调用侧零改动
```

- **合题**：领域概念在使用侧聚合（Memory 视图——"Memory 是
  一个对象"），工程拆分在存储侧独立（三契约位——"扩展伴随
  每层存储"）。概念完整与独立替换兼得：只换 L3 时 L1/L2 纹丝
  不动。
- **L1 低延迟归宿**：`MemoryL1Store` 默认 = 框架内置进程内存
  实现（结构上默认即内存）；换 L1 位理论上可能但无现实动机
  （纯内存是工作记忆的唯一合理介质）——"可换但没人换"的自然
  状态，不立法禁止。L2/L3 位的替换是真实场景。
- 感知延迟梯度（L1 即时 > L2 近期 > L3 长期；记忆感知延迟=
  记忆产生后 Agent 感知到它的速度）为分层设计核心 rationale
  ——写入各层 rustdoc 的性能特征引导（插件自带非功能特征
  责任，同 Model/Tool 插件原则），非框架结构约束。
- `MemoryStore`（统一三层契约）**退役**——拆分为三契约位；
  现有 `InMemoryStore` 拆分为各层默认实现。
- 跨层流转（压缩 L1→L2、蒸馏 L2→L3、促进）由 Curator 编排
  （聚合视图持全部三个句柄）——压缩改序（摘要→append→pop）
  已消除跨层原子性需求。**边界立法不变：存储位不得策展**。

### 5. Conversation 生命周期（双层主权）

> 状态主权在 Conversation（内聚自管理），用例编排与事件触发权
> 在总线派发装配（环境层）。OO 内聚与越界判定的调和：越界的
> 从来不是"end 是会话的方法"，而是"end 里做环境编排"。

```
Conversation（实体层 · 状态主权）
  new(runtime, subject) / with_id(runtime, subject, id)
      ★ 签名统一带 runtime（与 of/end 一致——环境见证），
        三通用动作：构造 + 初始 state 落库（目录立即一致，
        目录洞消除）+ emit ConversationCreated（通知总线）
  end(&runtime) -> EndOutcome   ★ async 化，三通用动作：
      ① 幂等标记 ended（is_ended/state 扩展——与 topic 同构：
         内部 Arc<Mutex>、state() 导出、of() 恢复）
      ② 自持久化（ended 入 state——环境设施正当使用）
      ③ emit ConversationEnded（通知总线）
      不做：任何记忆业务
```

- 两终结时机：`ConversationEndReason::{Explicit, IdleSwept}`（用户主动 /
  程序兜底）。Explicit 调用者在场（EndOutcome 返回值携带行动
  车道软失败——返回值即事实）；IdleSwept 无人在场（失败经总线
  `MemoryEvent::FlowFailed` 上报——sweep 四层静默黑洞就此修复）。
- `Runtime::end_conversation` **不设立**（讨论否决——它使 Runtime
  长出"终结要促进记忆"的业务知识，边界模糊）；runtime 只是
  sweep 兜底的**调用者**（调 `conv.end(self)`），非编排者。
- 创建路径的统一论证（评审修订）：`of`（恢复）与 `end`（终结）
  均带环境，`new` 不带是不一致；补齐后三个生命周期入口同构
  （构造/终结都是"标记 + 持久化 + 发事件"的环境见证——与纯
  数据会话不冲突：见证不是服务编排）。`ConversationCreated`
  升在册。
- ended 状态门（拒绝终结后续写）仍推迟，状态依据已就位。
- **Curator 派发的时序语义**：总线行动车道同步 await
  `on_conversation_ended`（end 返回即促进完成——Explicit 场景
  调用者拿到完整结果）。

### 6. SynonzEvent 词汇表（按领域实体划分）

```rust
#[non_exhaustive]
pub enum SynonzEvent {
    Turn(TurnEvent),
    Conversation(ConversationEvent),
    Memory(MemoryEvent),
}
```

**划分原则**：按实体/领域模型组族（开发者按实体直觉找事件），
不按投递机制；子族同视角（生命周期优先 + 领域行为子族）。

**Turn 族**（轮次执行叙事；`AgentEvent` 改名 `TurnEvent`——
实体词汇归位：一轮一执行一 Turn 记录，同一实体三视角）：

| 子族 | 事件 |
|---|---|
| Lifecycle | `Started`/`Completed`/`Failed`/`Cancelled` |
| Model | `Requested`/`StreamDelta`/`Responded` |
| Tool | `CallRequested`/`CallCompleted` |

Model/Tool 子族**全量携带 `round: Option<usize>`**（推理循环
序号，1-based；`Some(n)`=第 n 轮循环内，`None`=循环外的维护/辅助
调用——与 `purpose` 互证：`ContextManagement` ↔ `None`；命名避开
关键字 `loop`，沿用既有术语 round）——载荷自足原则强化（修订
现有 "rounds are derived by consumers, not stored" 文档为 stored
in the payload）；推理调用带所在循环序号，工具事件带触发它的
模型响应所在循环。

**Conversation 族**（平铺变体，子族为文档分组）——生命周期
两事件入册：

| 事件 | 载荷 | EventSource |
|---|---|---|
| `Created` | conversation_id, subject_id | `Conversation::new`/`with_id`（带 runtime 三动作） |
| `Ended` | conversation_id, subject_id, reason: ConversationEndReason::{Explicit, IdleSwept} | `Conversation::end` / sweep |

`TopicShifted { conversation_id, from, to }`（在册）——
EventSource=Curator（`advance_topic` 判定漂移时通知）。装配侧
联动无需事件驱动：`AssemblyRequest.topic` 每轮现读（既有机制），
漂移后下一轮装配自然切换检索焦点；记忆侧反应归 Curator 策略
（TopicShift 冲刷）；本事件只承担观察可见性（IDE 话题切换
提示、审计）。

**Memory 族**（平铺；流转即记忆条目的生命周期）：

`TurnArchived`/`Compacted`/`Distilled`/`Promoted`（在册）、
`FlowFailed { stage, detail, moment: MemoryFlowFailedMoment::{AfterTurn,
AtConversationEnd, Background} }`（在册——所有 run 外记忆失败
一个出口；run 内同步段失败维持 Turn 族叙事内表述）。

**入册原则（本 ADR 立法）**：事实发生即可见——emit 由事实发生
决定，不由消费者存在与否决定（与 Observer 全量目击哲学同源）。
故流转四事件入册：DefaultCurator 的每次真实流转都通知总线，
框架行为全景透明。

**Context 实体的设计判断**：装配事实由 `Turn::Model::Requested`
全量载荷覆盖（"模型看到了什么"），装配读取失败归
`Memory::FlowFailed { stage: AssembleRead }`——Context 不设
独立事件族，避免同一事实两处表述。

### 7. 发射源 × 订阅位矩阵

| EventSource | emit | 观察位 | 行动位 | 交付 |
|---|---|---|---|---|
| 执行循环 | Turn 三子族 | ✅ | — | ✅ ExecutionEvent 投影 |
| 执行循环（同步段失败） | MemoryEvent::FlowFailed{AfterTurn} | ✅ | — | — |
| `Conversation::new`/`with_id` | ConversationEvent::Created | ✅ | — | — |
| `Conversation::end`/sweep | ConversationEvent::Ended | ✅ | ✅ `on_conversation_ended` | — |
| Curator（`advance_topic` 判定漂移） | ConversationEvent::TopicShifted | ✅ | — | — |
| Curator 各流转/失败回收 | Memory 四流转/FlowFailed | ✅ | — | — |

治理原则：行动位只对生命周期事实反应（记忆业务边界）；Turn
事件只被目击与交付（不驱动记忆——记忆反应锚在 Curator 契约的
时刻上）。

### 8. 扩展点生效时序（全景契约）

| 扩展点 | 调用时机 | 生效边界 | 失败语义 |
|---|---|---|---|
| `Model` | 推理循环每轮 | 直接决定本轮输出 | 终态 Failed |
| `Tool` | 模型请求时（并行） | 结果回喂，影响本轮后续 | 软失败回喂 |
| `ContextAssembly` | 每 run 装配 await | 决定模型看到什么 | 降级可见 |
| `ConversationStore` | record! 时刻 | 真相持久化 | 软失败可见 |
| `MemoryL1/L2/L3Store`（经 `Memory` 聚合） | 被 Curator/Assembly 调用 | 跟随调用者时序 | MemoryEvent::FlowFailed |
| `TopicDetector` | Curator 同步段内 | 主题标签 | 纯计算无失败 |
| `Observer` | 事件 emit 时 | 永不影响执行 | 熔断+lag |
| `MemoryCurator::on_turn_completed` | 终态事件前 await | 同步段=下一轮必见；后台段=滞后 | 同步段返回值；后台段经总线 |
| `MemoryCurator::on_conversation_ended` | 总线行动车道 await | 返回即促进完成 | 软错误返回+总线 |

后台失败可见性：DefaultCurator 后台任务携带 EventSink 克隆
（tokio mpsc sender 保活语义），压缩/蒸馏失败投 `Memory::
FlowFailed{moment: Background}`——对工程管道（观察位）可见，
对产品叙事流不可见（终态不变量保持）——分层可见性。

## Alternatives Considered（备选与否决理由）

1. **总线不立（主体即架构）**：Observer+Curator 各自机制，扩展
   靠新主体立法。被否：双车道形状已把总线锻造为与全部约束兼容
   的形态；设施合一（两套派发器→一套）是真实红利；irylex 的
   扩展性地基诉求（"不是只有 S3 有真实需求"）。
2. **总线承载全部四工作**（含控制流拦截）：被否——一个概念承载
   四种语义（叙事/反应/控制流/通信）是魔法浓汤配方；控制流是
   middleware 性质另立，Agent 间通信归 S3。
3. **Curator 契约带分层 hook**（archive_l1/compact 等上 trait）：
   被否——分层哲学泄漏契约，换记忆模型（时间衰减等）的下游
   无处安放；单点扩展从"覆写 1 方法"退化为"抄 5 行编排"。
4. **两阶段时序契约**（on_turn_committed/on_curation 拆锚点）：
   被否——工程考量撕裂领域事实（"轮次完成"是一个不可再分的
   事实、一个方法）；时序内化默认实现，契约保纯净（irylex：
   工程层面要考虑，但不能把设计改乱了）。
5. **`Runtime::end_conversation` 用例编排**：被否——Runtime 长出
   业务知识（"终结要促进记忆"），边界模糊；改为 Conversation
   三通用动作 + 总线派发装配（记忆业务回到 Curator，全部内聚）。
6. **泛型路由开放事件总线**（下游自定义类型 publish/subscribe，
   TypeMap 路由）：被否（推迟）——订阅位制下自定义事件无行动位
   可认领，只剩目击价值；Observer 契约被迫复杂化（match 不了
   `Box<dyn Any>`）；每类型一通道 × N 观察者的派发矩阵复杂化。
7. **Run/Lifecycle 机制语族划分**：被否——投递语境不是领域语言；
   按实体划分（Turn/Conversation/Memory）且 moment 载荷消解
   `MemoryFlowFailed` 两栖。
8. **`AgentEvent` 保留原名**：被否——事件非 Agent 实体叙事
   （Agent 生灭无框架时刻；irylex：Agent 事件是初始化/运行中/
   消失），实为 Turn 叙事，词汇归位为 `TurnEvent`。
9. **Runtime 级创建工厂**（`Runtime::new_conversation` 收口）：
    被否——Runtime 长出生成职责，与 `end_conversation` 同款边界
    模糊。终选（评审修订）：实体构造器带环境（`Conversation::
    new(runtime, subject)`——与 `of`/`end` 同构的环境见证），见
    §5。
10. **Hook 划分照搬 trigger.rs 步骤为 trait 方法**：被否——伪
    代码先行暴露的错误（实现哲学焊进契约）；修正为"契约锚定
    生命周期事实，分层哲学住默认实现"。
11. **Memory 一体契约**（三层 CRUD 同一 trait）：被否——违反
    接口隔离原则；异构存储现实（L1 纯内存/L2 Redis/L3 向量库）
    下强迫插件"包三层"适配器，想换 L3 被迫重写 L1/L2。
12. **L1 切给框架 + 契约瘦身**（MemoryStore 只剩 L2/L3，L1 为
    Runtime 内置工作记忆）：被否——割裂 Memory 领域对象、
    框架/插件边界生硬（irylex：架构不清晰）。
13. **抽象类模拟**（`L1Core` 内核 + `l1_core()` 访问器 + trait
    默认方法，L1 可覆盖）：被否——Rust 无继承，插件必须手写
    "可继承成员变量"（字段+访问器样板）；且 L1 可覆盖自由度与
    低延迟结构保障相矛盾（覆盖即破坏感知延迟梯度），危险自由
    度多于价值。终选：三契约位 + 聚合视图（每层独立替换、
    L1 默认内置内存）。

## Consequences（后果）

**正面**：

- 反应成为一等扩展点：观察位自由注册 + Curator 可整体替换 +
  事件词汇表版本化生长——下游的三层扩展面。
- 四项缺口一次收口：conv.end 越界（三动作+总线）、sweep 失败
  黑洞（总线上报）、run 外事实不可见（Memory/Conversation 族）、
  生命周期状态无居所（ended 入 state）。
- 记忆两面对称：写入面 Curator ↔ 读取面 ContextAssembly，中间
  三层独立存储位（`Memory` 聚合视图统一使用侧）。**边界立法：
  存储位不得策展**——存储插件不得在 append 时压缩/蒸馏（策展
  时机由生命周期事实驱动，存储位感知不到；可见性立法在
  Curator 层，存储位内策展会绕过 never-silent）。扩展轴正交：
  换策展哲学改 Curator，逐层换存储改对应 `MemoryL*Store` 位，
  互不影响。
- 载荷自足强化：round 显式入载荷，消费者免除推导（不再数
  Requested、不再处理辅助调用干扰）。
- 术语边界清晰：交付/订阅分离，"事件驱动"有且仅有总线一义。

**代价与义务**：

- `async_trait` 风格依赖决策（dyn 兼容的 async trait desugar，
  成熟小依赖或手写 `Pin<Box<dyn Future>>`——实施期定，遵守
  保守依赖评估）。
- 契约不强制时序：全同步实现的自定义 Curator 复现延迟痛感——
  rustdoc 必须写明"同步段应快速返回，重维护应后台化"。
- 记忆行为事实全量上总线：派发开销与事件体积（Clone）——观察
  车道丢弃语义兜底；后台任务的派发器寿命随 EventSink 克隆
  延长（可接受，文档写明）。
- `AgentEvent`→`TurnEvent` 等类型改名：全部公开 API 破坏随 0.3.0
  单波发布（0.2.0 跳过发布，下游只见一次破坏波）。

**迁移工作清单（0.3.0 实施波次）**：

1. `AgentEvent` → `TurnEvent` 改名；`MemoryFlowFailed` 迁
   `MemoryEvent::FlowFailed`（moment 载荷）；Model/Tool 载荷
   加 `round: Option<usize>`。
2. Observer 契约签名改 `on_event(&SynonzEvent)`；per-run 派发器
   与常驻通道合一为总线派发器（观察位语义全套继承：保序/
   熔断/lag/终态 drain）。
3. `EventBus` 设施（双车道、订阅位制、`SynonzEvent` 类型锁死、
   emit 非阻塞、`Serialize + Deserialize` 两级 tag）。
4. `MemoryStore` → 三契约位拆分（`MemoryL1/L2/L3Store`，方法名
   去层前缀）+ `Memory<'_>` 聚合视图（转发保持调用形态）+
   `RuntimeBuilder::l1_store/l2_store/l3_store` 三个注册位 +
   `InMemoryStore` 拆为各层默认实现（L1 默认内置内存）。
5. trigger.rs → `DefaultCurator` 主体化搬移（配置面/步骤面/
   后台队列与 drain/压缩改序）；`TurnContext`/`ConversationEndContext`
   公开（memory 字段为 `Memory<'_>` 聚合）；`MemoryFlowError`
   typed 失败类型；`ConversationEvent::TopicShifted` 发射。
6. `Conversation` 生命周期改造：`new`/`with_id` 签名带 runtime
   （构造+初始 state 落库+emit Created）；`end` 三动作（async、
   幂等 ended、is_ended、state 扩展）；`ConversationState` 加
   ended 字段。
7. `sweep_stale` 重写（调 `conv.end`、失败经总线、ended 过滤、
   返回值语义保留）。
8. EventTap 正名（纯交付零件）与瘦身。
9. Context 内部化（`Conversation::context()` 删除、`Context` 降
   pub(crate)——既定决策并波次）。
10. v4 架构文档（承载本 ADR 全部决策 + v3 §6 生命周期与编程范式
    章节修正迁入；扩展点时序表进文档）。
11. CHANGELOG 0.3.0（破坏项+迁移指引）；ADR-0015/0016 补修订
    注记（APPROVED 时）。

## 开放项

- ended 状态门（终结后拒绝续写）：状态依据已就位，场景出现时
  带 ADR 立法。
- 控制流影响（middleware/拦截）：③ 工作另立 ADR，暂无场景。
- S3 Agent 间通信：④ 工作另立 ADR，总线词汇表为其留缝。

（原开放项 1/2/3/4/6 已在本轮评审决议中闭合并落入 Decision
相应小节：Created/TopicShifted 升在册、round: Option、Memory
三契约位+聚合视图、serde 立法——见头部评审修订注记。）
