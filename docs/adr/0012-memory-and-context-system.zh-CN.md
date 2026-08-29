# ADR-0012: 记忆与上下文系统

- 状态: DRAFT
- 日期: 2026-08-29
- 决策者: irylex（人类确认）
- 性质: 系统级 ADR——原 S2b（ContextManager）与 S2c（Memory）的旧切分
  溶解，统一为一个记忆与上下文系统的整体设计

## Context（背景）

S2a（会话实体，ADR-0011）完成后，进入上下文管理与记忆的设计。讨论从
"ContextManager 契约"起步，经多轮深化（多次推翻重建，完整记录见
Alternatives），最终收敛为一套以**主体（Subject）**为中心、分层记忆 +
内聚上下文引擎 + 三兄弟插件契约的完整系统。

本 ADR 受以下已批决策约束：ADR-0007（Agent 无状态）、ADR-0009（契约
随场景晋升；G3/G4/G5 激活）、ADR-0010（High Level 优先；先定 80%
用法）、ADR-0011（Conversation 真相实体；TurnInput 参数对象）。

## Problem（问题）

1. 长对话的上下文预算管理：超限时的截断/摘要机制（原 S2b）；
2. 跨会话的记忆：主体经验的沉淀、组织与检索（原 S2c）；
3. 上下文与记忆的概念边界与行为模型——两者如何关联、各自的边界；
4. 开发模型：High Level（框架定时机）与 Low Level（开发者自控）双轨
   如何在记忆与上下文领域落地；
5. 插件化：存储、检索、组装的可插拔边界。

## Decision（决策）

### 一、概念模型（三个概念的精确边界）

| 概念 | 定义 | 在哪里 |
|---|---|---|
| **Conversation** | 对话真相实体：完整记录发生过什么；身份、轮次、借用串行化 | ADR-0011 已有 |
| **Context** | 第三持续对象：**会话级 Runtime**（叙事背景引擎）——持有记忆句柄 + 主题状态 + 组装策略；"让无状态的 Agent 有状态地执行" | 本 ADR 新增 |
| **Memory** | 主体拥有的、对交互输入的抽象沉淀；在交互过程中自动形成，**开发者永不创建** | 本 ADR 新增 |

关键切分：**装配源 = 记忆（L1/L2/L3）+ 本轮 input；Conversation 不参与
装配**。Conversation 是审计/回放的权威，不是发给模型的来源；轮次完成
写入 L1 是下一次装配能看到本轮内容的前提（写入时序是装配正确性的
硬保证）。

### 二、主体模型（Subject 一等实体）

```rust
pub enum SubjectType {
    User,    // 用户主体（v1）
    Agent,   // Agent 主体（变体自 v1 存在；其交互记忆的完整语义随 S3 落地）
}

let subject = Subject::of(SubjectType::User, "user-42");   // of 家族：按身份解析
```

- **主体身份 = (SubjectType, id) 二元组**：`user-42` 与 `agent-42` 是
  不同主体；记忆系统的归属键使用完整主体身份（片段三元组中的
  subject 即此完整身份）——为 S3 的 Agent-Agent 记忆拓扑预留无需
  破坏性变更的扩展；
- 记忆的主体是**交互双方**：用户（现在实现）与 Agent（S3 多 Agent 时
  预留——"问的人和答的人都产生记忆"）；`SubjectType::Agent` 变体
  第一天即存在于枚举中——可预知的后续扩展，代价为零；
- 应用层为主体声明身份；记忆实体由框架经 Runtime 解析，开发者全程
  只接触 Subject 对象；
- 会话创建**强制携带主体**（`Conversation::new(&runtime, &subject)`），
  不存在无主体的会话。

### 三、分层记忆（L1/L2/L3）

| 层 | 作用域 | 内容 | 度量 |
|---|---|---|---|
| **L1** | 当前会话 | 本会话最近 N 轮原文 | 轮次数（TurnCount） |
| **L2** | 当前会话 | 本会话较早轮次的摘要缓存 | 摘要块数（L2Overflow） |
| **L3** | 跨会话（主体级） | 蒸馏的长期知识（主题 + 溯源） | 检索预算 |

- **故障隔离**：L3 不可用（检索失败）时，L1/L2 保证当前沟通不偏航——
  "至少是能用的"；
- **串扰防护**：其他会话的经验必须经 L3 检索闸门（主题/相关性过滤）
  才能进入当前上下文——不"串来串去"；
- **片段三元组**：每个 Memory 片段由 `(subject_id, conversation_id,
  topic)` 唯一定位——溯源是身份的组成部分。

### 四、SynonzRuntime（进程级 Bootstrap）

```rust
let runtime = SynonzRuntime::builder()
    // .register_conversation_store(...)   // 可选；缺省进程内默认
    // .register_memory(...)               // 可选；缺省进程内默认
    // .register_assembly(...)             // 可选；缺省 LayeredMemory
    .build();
```

- **显式构造，无隐式默认**：多实体隐式解析导致环境分裂（同一交互中
  会话与记忆落在不同 Runtime——致命且无报错）；一致性由**同源**保证；
- **启动注册表 + 缺省默认实现**：不注册即用进程内默认实现；无 enable
  配置族（API 爆炸，不利长期迭代）；
- **Agent 零环境知识**：环境链 `runtime → conversation → context` 全部
  由实体携带，随 TurnInput / with_context 流入执行体；Agent 不持有、
  不感知 Runtime（依赖差异如实呈现在参数上）。

### 五、工厂族与调用形态

```rust
let runtime = SynonzRuntime::builder().build();
let subject = Subject::of(SubjectType::User, "user-42");
let mut conv = Conversation::new(&runtime, &subject)?;          // new=创建
// Conversation::of(&runtime, &subject, conv_id)?               // of=恢复
// (S3) Conversation::ensure(&runtime, &subject, id)?           // get-or-create

let agent = Agent::react(model, tools).build()?;                // 纯净执行体
let answer = agent
    .with_context(conv.context())                               // 会话产出背景
    .ask(conv.turn_input("北京天气？"))                          // 唯一参数模型
    .await?;
```

- `new`=创建 / `of`=恢复 / `ensure`=get-or-create（S3）：语义对纯粹，
  of 不存在则报错、不静默新建；
- **工厂归属原则**：工厂方法合法当且仅当主语能完整决定产物的身份，
  否则应为挂载（`with_*`）。`conv.context()` 合法（会话决定背景身份）；
  `runtime.agent(...)` 非法（Agent 身份来自模型/工具/prompt，不来自
  Runtime）；
- **ask/run 唯一参数模型（TurnInput）永不简化**：绑定会话是语义声明
  （背景基于这场对话），每次传 turn_input 是轮次输入与借用串行化——
  各表其意，互不替代。

### 六、触发策略体系（层间流转的时机）

| 流转点 | 强制保底（确定性资源约束） | 可叠加（事件驱动） |
|---|---|---|
| L1→L2 | `TurnCount(n)` | TopicShift / ConversationEnd / 自定义 |
| L2→L3 | `L2Overflow(n)` | ConversationEnd / Demotion / Retrieval / Periodic / TopicShift / 自定义 |

原则：

- **强制保底不可移除、只能叠加**：有效策略集 = 默认 ∪ 开发者显式指定；
- **保底必须是确定性资源约束**：TurnCount/L2Overflow 可预测不可绕过；
  非确定性事件（ConversationEnd 等）只能作为可叠加策略，开发者按需
  引入，框架不强加不可控默认；
- L2 超限动作 = **蒸馏进 L3 后移除**（向下沉淀），不是丢弃——与分层
  流转语义一致，无不可逆信息损失。

**ConversationEnd 判定闭环**：显式 `conv.end()`（触发权在发起方——交互
双方中掌握主动的一方）+ **空闲超时兜底**（Runtime 后台扫描注册会话的
最后活动时间）。技术闭环：超时流转作用于主体记忆与存储层，**不依赖
会话句柄存活**；超时后用户返回则会话复活（L1/L2 数据仍在），语义自然。

**主题机制**：会话级主题状态机 + 片段继承（写入零额外成本）；检测时机
= 会话首轮（建立初始主题，可由应用显式提供）+ 每 K 轮定期（默认 K=3）
+ TopicShift 策略启用时逐轮；检测器可插拔（默认 LLM 轻量分类，可注册
规则实现）。诚实标注：切换点附近片段归前主题（检测延迟），v1 不做
回溯修正（L1 只含最近轮次，误差窗口小）。

### 七、三兄弟契约（同一注册模式，业务实体不耦合）

```rust
// ① ConversationStore：会话真相的存取（of 恢复源、自动保存）
// ② MemoryStore：三层记忆的存取与检索
trait MemoryStore: Send + Sync {
    // L1（当前会话）：append / window / oldest
    // L2（当前会话）：append / read / oldest
    // L3（跨会话）：upsert / retrieve(query, budget)
}
// ③ ContextAssembly：组装策略
trait ContextAssembly: Send + Sync {
    async fn assemble(&self, sources, input: &AgentInput) -> Result<Vec<Message>>;
}
```

- **编排在框架，存取在插件**：四时机、触发体系、摘要/蒸馏的 LLM 调用
  编排是框架行为模型；存什么、怎么存、检索时选什么全在实现侧；
- **检索逻辑是实现的内部事务**：默认实现内部用主题匹配 + 时间衰减
  （TopicRecency）；向量库插件内部可用混合检索——框架只传 Query
  （subject_id + current_topic + input_text）与 Budget，**永不决策怎么选**；
- **Embedder 契约永不进入框架**：语义检索所需的嵌入是自定义
  MemoryStore 实现的内部依赖（ADR-0006 搁置的 Embedder 概念在此
  消解）。

### 八、组装策略模型（统一双路径）

| 实现 | 提供方 | 行为 |
|---|---|---|
| `LayeredMemory`（默认） | 框架内置 | L1 窗口 + L2 摘要 + L3 检索 + input 分层装配 |
| `ConversationHistory` | 框架内置 | 会话完整历史直发（v1 行为收编为内置策略） |
| 开发者自定义 | 开发者 | 经策略接口组装——Low Level |

设计意图：

1. **内聚**：策略接口内部使用记忆读取/会话读取 API——Context 里的对象
   经统一入口消费，不散落；
2. **约束边界**：策略只决定"**发什么**"；"何时发"（ask 时机）、"落账"
   （Turn 记录）、"写入时机"（触发体系）锁定在框架——开发者无法
   无限发挥破坏行为模型；
3. 无特殊路径：不存在"裸路径绕过 Context"——一切组装皆策略。

### 九、装配行为

- **每次 ask 新鲜装配**（不是 context() 创建时一次性）——对话进行中
  L1 持续增长，装配必须取最新；
- **装配格式**（发给模型的消息序列）：

```
[System: agent 的 system_prompt]          ← 开发者人格（纯净）
[System: "Memory recall: ..."]            ← L3 检索注入（独立消息）
[L2 摘要块（本会话较早内容）]
[L1 最近 N 轮原文消息（原序）]
[User: 本轮 input]
```

  L3 注入为**独立 System 消息**（不拼接进 system_prompt）：开发者人格
  与框架注入的记忆分离可辨识（P2：Requested 事件里一眼分清）；canonical
  Message 天然支持多条 System；
- **事件可见闭环**：装配产物即 `ModelRequest.messages` → Requested 事件
  可见（回放可查）；装配期 LLM 调用（如 L2 懒摘要）以 `ContextManagement`
  purpose 出现（ADR-0003 的事件设计在此兑现）；
- **配置**：L1 窗口 / L3 预算等参数挂 SynonzRuntime builder（环境级默认，
  v1 不做会话级覆盖面——YAGNI）；token 级总预算分配器后置为优化项。

### 十、既有 API 演进标注（破坏性，pre-1.0 按 release 规则显式标注）

- `Conversation::new()`（无参版）被 `Conversation::new(&runtime, &subject)`
  取代——会话必须有主体与环境；
- 一次性路径 `agent.ask("1+1")`（&str TurnInput）完全不变。

## Alternatives Considered（备选方案——完整推翻史）

本 ADR 的讨论经历了多轮推翻重建，以下按主题记录被否决的方案及其理由。

### 概念定位

- **ContextManager 大契约**（初期形态）：把整个组装与历史压缩混为一谈，
  词太大定位错。被三概念切分（Conversation/Context/Memory）取代。
- **临时视图哲学**（A）vs **持久压缩哲学**（B）之争：B（压缩会话本体）
  与 P6 可观测冲突被否；A 最终也被修正——装配源改为记忆而非"会话的
  视图"。
- **Context = 策略挂 Agent**（策略无状态、状态归实体）：被用户否决——
  Context 应是独立的第三持续对象（Runtime 类比，像 Go 的 context /
  Spring 的 ApplicationContext——用 Context 让无状态 Agent 有状态地
  执行）。

### 记忆分层

- **L1 = Conversation 的视图**（不落存储，单一事实源）：被否——耦合。
- **L1 = 跨会话经验流**（主体最近 N 轮跨会话聚合）：被人类开发者自我
  修正否决——L1/L2 必须是当前会话（防串扰、故障隔离），跨会话连续性
  经 L3 检索达成。两次演化的教训："看似一样的，其实所代表的含义有
  差别"。
- **记忆手动创建/传递**（`Memory::new()` / `with_memory(&memory)` /
  `Memory::of(...)` 公开 API）：被否——记忆是交互的伴生产物，开发者
  最多配置策略，永不操作实体。

### Runtime 与工厂

- **隐式默认 Runtime**（独立 Agent / 独立 Conversation 各自挂默认）：
  被致命缺陷否决——多实体隐式解析导致环境分裂（会话在默认 InMemory、
  Agent 挂 Redis——同一交互分裂两环境，静默丢数据）。
- **enable 配置族**（`enable_in_memory_store()` 等）：被否——API 爆炸、
  不利长期迭代；由"启动注册表 + 缺省默认实现"取代。
- **runtime.agent(...) 工厂派生**：工厂倒置（Agent 身份不来自 Runtime）
  被否；演化为 with_runtime 挂载；最终连 with_runtime 也删除（环境经
  conv→context 流入，Agent 零环境知识——更纯）。
- **runtime.conversation(subject) 实例工厂**：动词涂抹到 Runtime 方法名
  上（conversation/conversation_of/ensure_conversation）+ 万能工厂膨胀；
  被 `Conversation::new/of/ensure(&runtime, &subject)` 静态工厂族取代。
- **store 显式入参**（`Conversation::of(&store, id)`）：被 Plugin 模型
  否决——显式传可插拔对象违背"不同实现达成不同效果"的初衷；Runtime
  环境句柄作为 Bootstrap 显式构造物不在此列（传环境不传插件）。
- **of 混合语义**（get-or-create）：被否——语义对必须纯粹，静默新建是
  行为不可推理的隐患；ensure 后置 S3。
- **for_subject 可选行为**：被否——主体必须强制，且 Subject 升为
  一等实体入参。

### 调用形态

- **绑定后简化 ask 参数**（`runtime.ask("x")` 直给输入）：被否——绑定
  会话是语义声明，每次传 turn_input 是轮次输入与串行化，互不替代；
  ask/run 唯一参数模型（TurnInput）是 API 优雅的根基。
- **显式在途 Turn + TurnRecord 两类型**（S2a 讨论遗留的 B 方案变体）：
  维持 ADR-0011 的结论不变。

### 触发与检索

- **ConversationEnd 作为 L2→L3 默认策略**：被否——不可控（依赖超时
  机制）；保底必须确定性资源约束。开发者可自行叠加 ConversationEnd。
- **L3Retrieval 独立策略契约**（框架定义 TopicRecetry/Hybrid 之争）：
  被否——整个 L3 检索与写入归入 MemoryStore 统一契约，检索逻辑是
  实现侧事务；原"默认 TopicRecency vs 立 Embedder 契约"的抉择随之
  消解。
- **裸路径**（无 Context 时 conv.messages() 直发）：被收编——
  ConversationHistory 成为内置组装策略之一，无特殊路径。
- **每轮摘要**：被否——触发式 + 缓存 + 增量。
- **L2 超限丢弃**：被否——蒸馏进 L3 向下沉淀。

## Consequences（后果）

### 正面

- 概念边界清晰：真相（Conversation）/ 背景（Context）/ 经验（Memory）
  三者行为模型互不越界，关系显式；
- 主体中心的记忆拓扑为 S3 多 Agent（用户与 Agent 各自的记忆、人与人式
  交互）预留了完整语义；
- 插件体系一致：三兄弟契约同一注册模式，编排在框架、存取在插件，
  扩展不越界；
- High/Low 双轨完整落地：触发策略（默认+叠加）、组装策略（内置+自
  定义）、显式 end / 自控流转；
- 故障隔离与串扰防护：L3 失效不影响当前会话；跨会话经验必须过检索
  闸门。

### 成本与义务

- 破坏性变更：Conversation::new 签名变更（pre-1.0 显式标注）；
- 空闲超时需要 Runtime 后台任务（会话活动跟踪 + 定时扫描）；
- 主题检测（默认 LLM 轻量分类）带来每 K 轮一次的额外模型调用（事件
  流可见）；
- 三契约 + Subject + SynonzRuntime 的实现量显著（独立里程碑）。

### 推迟项（触发条件显式记录）

| 项 | 触发条件 |
|---|---|
| `Conversation::ensure`（get-or-create） | S3 编排 |
| Agent 主体的交互记忆语义 | S3 多 Agent（`SubjectType::Agent` 变体自 v1 存在，仅交互语义后置） |
| Embedder 契约 | 永不——已消解为 MemoryStore 实现的内部依赖 |
| token 级总预算分配器 | 装配优化的真实需求 |
| 会话级配置覆盖（L1 窗口等） | 真实需求 |
| L2 摘要块数度量的 token 精确化 | 度量不均的实际证据 |
| 主题切换点回溯修正 | 误差窗口被证明有害 |

### 关联

- 依赖 ADR-0007（Agent 无状态——本 ADR 使其达到最纯形态）、
  ADR-0009（G3 激活并消解：组装时机=每次 ask；G4 消解：MemoryStore
  统一契约；G5 工具访问会话/记忆——S3 再议）、ADR-0010（High Level
  优先贯穿）、ADR-0011（Conversation 实体、TurnInput 模型）；
- 兑现 ADR-0003 的 CallPurpose::ContextManagement（装配期 LLM 调用
  首次有真实产生者）；
- 实施切片（M11+）：SynonzRuntime + Subject + 工厂族 → MemoryStore
  默认实现 + 触发引擎 → ContextAssembly + 装配管线 → 端到端。
