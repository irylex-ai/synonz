# Synonz 0.10.0 实现计划

- 状态: 已批准（2026-11-05，irylex 人工评审通过）；M51-M53 已实施；
  发布待确认
- 日期: 2026-11-05
- 依据: ADR-0032（逐字近期窗口——读相位帧的近期连续性通道）
- 性质: 开发文档——0.10.0 的执行次序与验收基准；决策理由见
  ADR-0032，本文不重复论证
- 前置: 0.9.0 已发布（2026-10-27；289/289 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.9.0
- 上游场景: Valkyrie 0.6.0（长编码会话的近期连续性）

---

## 1. 总览

0.10.0 给读相位帧增加一个**可配置、默认关闭**的逐字近期通道：最近
K 轮成对消息（用户原文 + 最终回答）原样进入帧，召回继续负责更早的
L1/L2/L3 知识；核心只增加一个非破坏的 `recent` 输入材料，帧策略与
可观测都在组件内。工具消息与上下文压缩（`/compact`）不在本版本。

**范围**
- 核心：`MemoryContextAssembleInput.recent: &'a [Arc<Turn>]`（非破坏；
  复用 `Turn`、无新公开类型）+ `Conversation` 内部轮次改为 `Arc`
  共享（公开 API 与序列化行为不变）+ `MemoryProvider` 增默认方法
  `context_recent_turns()`（默认 20）+ Runtime 构建期按此构建材料；
  改写器仍取 3 轮尾部；
- 组件：`context_recent_turns`（默认 0）；生效窗口
  `min(K, l1_turns, 材料)`、帧布局、按序排除、改写跳过；
  `LayeredMemoryEvent::Assembled` 与观察者接入；
- 文档：组件 README、扩展指南、v8、CHANGELOG（无迁移）。

**非目标**
- 工具消息/完整转录（后续、数据驱动）；
- 上下文压缩命令与每会话 marker（解耦，后续另立决策）；
- 按会话/按任务类型的形态开关（记忆形态是应用级全局配置）；
- 不改写既有召回评分、去重、压实与维护语义。

## 2. 决策基线（ADR-0032）

- 应用级全局配置 `context_recent_turns`（0 = 现状）；
- 帧 = 最近 K 轮成对消息 + 末尾 `user(本轮输入 + 折叠补充)`；
- 逐字 = 最近 K 轮；召回 = 其余 L1 + L2 + L3；按序排除最近 K 条
  L1 候选；不做额外语义过滤；
- 改写仅在有补充时调用；无补充跳过；
- `Assembled { scope, verbatim_turns, recalled_l1, recalled_l2,
  recalled_l3, rewritten }` 每次组帧上报；失败仍走核心 `Failed`；
- 窗口实现为"最近 K 轮后缀"，为将来压缩 marker 预留同构扩展。

## 3. 里程碑明细

### M51 核心：最近轮次输入（复用 `Turn`）

- `Conversation`：内部轮次存储由 `Vec<Turn>` 改为
  `Vec<Arc<Turn>>`（`push_turn` 包装；`turns()` / `state()` /
  序列化行为不变——仍按需深拷贝对外）；新增 crate 内访问器
  `recent_turns(n) -> Vec<Arc<Turn>>`（只克隆句柄、立即释放锁）；
- `MemoryContextAssembleInput` 增 `recent: &'a [Arc<Turn>]`（最近
  成功轮、旧→新；文档写明内容与上界；`new` 增参，`#[non_exhaustive]`
  保证非破坏）；
- `MemoryProvider`：增默认方法 `context_recent_turns() -> usize`
  （默认 20；非破坏）；Runtime 构建时取一次，按此数量构建最近成功
  轮的 `Arc` 共享材料；内置默认 provider 覆写为 0（不使用该材料）；
  改写器仍取 3 轮尾部（与材料分开，避免重复深拷贝）；
- 测试：自定义 assembler 读取 `recent`；内容 = 最近成功轮、旧→新、
  失败轮不在其中；材料长度 = provider 声明的
  `context_recent_turns()`（默认 20）与实际轮数的较小者；内置默认
  provider 返回 0（不构建材料）；`turns()` / `state()` 公开行为不变；
  现有全量不变。

### M52 组件：逐字帧策略与可观测

- 配置：`LayeredMemoryConfig` 增 `context_recent_turns: usize`
  （默认 0；文档写明应用级全局、以逐字形式进入帧、推荐包络
  `K ≤ l1_turns`、0 = 现状）；组件 provider 的
  `context_recent_turns()` 返回 `min(K, l1_turns)`；
- assembler：
  - **生效窗口 = `min(配置 K, l1_turns, input.recent.len())`**——材料
    长度由注入 provider 的 `context_recent_turns()` 决定（组件为
    `min(K, l1_turns)`；K = 0 时为 0，不构建材料）；越界夹取、不
    报错；`K > 0` 时取最近 K 轮作为帧前缀（成对消息），末尾追加
    `user(query + 折叠补充)`；
  - 召回候选按序排除最近 `min(K, L1 长度)` 条 L1；其余 L1/L2/L3 的
    评分/去重/`recall_top_n` 不变；
  - 补充为空时跳过改写（帧 = 前缀 + 原始 query）；生效窗口为 0 走原
    路径；
  - 候选增加内部来源标记（L1/L2/L3）以支撑事件计数；
- 观察面：`LayeredMemoryEvent` 增 `Assembled`；assembler 接入观察者
  （provider 构建时传入）；
- 测试：帧布局与顺序；最近 K 轮无重复；排除逻辑；改写跳过；事件
  字段；生效窗口为 0 时逐字节等价（且 provider 返回 0、不构建材料）；
  夹取规则（K > l1_turns、K > 材料长度）；工具消息不存在（材料只含
  成对消息）。

### M53 文档、验证与发布准备

- 文档：组件 README（配置与观测量）、扩展指南（读相位 §3 帧构成、
  §5 配置）、v8（实施记录/里程碑）、CHANGELOG 0.10.0（加性、无迁移）；
- 验证：fmt / clippy 0 / doc 0 / 全量测试 / 离线示例；
- 发布准备：版本 bump、dry-run、发布说明草稿（发布仍需人工确认）。

## 4. 兼容与迁移

- 核心：`recent` 为纯增量（`#[non_exhaustive]` + `new` 增参；自定义
  assembler 读取新字段，编译不受影响）；`Conversation` 内部改为
  `Vec<Arc<Turn>>`，公开 API（`turns()` / `state()` / 序列化）行为
  不变；
- 组件：配置默认 0，既有应用行为与帧逐字节不变；无数据迁移；
- 消费方：显式设置 `context_recent_turns`（建议从 2–3 起步），并按
  需注册 `LayeredMemoryObserver` 消费 `Assembled`；
- 版本：0.10.0（0.x 次版本，含组件公开配置字段新增）。

## 5. 风险与开放项

- **工具级连续**：本轮不含工具消息；若 Valkyrie 数据表明必要，以同一
  输入口子扩展（完整转录视图）另立决策；
- **上下文压缩**：解耦；窗口为后缀，未来加"每会话压缩 marker"即可，
  不改帧布局；
- **K 的建议值**：生效窗口受 `min(K, l1_turns, provider 声明的材料)`
  约束；默认 0，消费方按需（推荐包络 `K ≤ l1_turns`）；
  Valkyrie 脱敏帧日志用于确定建议区间；
- **摘要重叠**：backlog 压实场景下逐字轮次可能与 L2 摘要同时出现；
  先记录边界，数据表明显著再考虑近邻过滤；
- **预算**：prompt 随 K 线性增长；每条消息不截断（保持"原样"），
  K 是唯一预算旋钮；
- **缓存**：历史前缀 append-only；尾部消息每轮变化，符合预期；
- **内部重构**：`Conversation` 轮次改为 `Arc` 存储属内部改动，风险
  集中在 `push_turn` / `turns()` / `state()` / 恢复路径，由既有全量
  用例与新增行为测试覆盖。

## 6. 验收

- `K = 0`：全量用例（289+）不变，帧与 0.9.0 等价；
- `K > 0`：帧布局、无重复、排除、改写跳过、事件字段全部符合 ADR；
- 生效窗口夹取规则与推荐包络行为正确；
- 自定义 assembler 编译通过（核心输入纯增量）；
- `Conversation` 的 `turns()` / `state()` / 序列化行为不变；
- 与重启恢复、冷启动门槛、压实/蒸馏互不影响；
- clippy/doc 零警告、fmt 干净、离线示例实跑通过。

## 7. 里程碑完成记录

### M51 核心：最近轮次输入 — 完成（2026-11-05）

- `Conversation` 内部轮次改为 `Vec<Arc<Turn>>`（`push_turn` 包装；
  `turns()` / `state()` / 序列化行为不变）；新增 crate 内
  `recent_turns(n)`（只克隆句柄）；
- `MemoryProvider::context_recent_turns()`（默认 20）+
  `MemoryContextAssembleInput.recent`（`new` 增参，
  `#[non_exhaustive]` 保持实现侧非破坏）；Runtime 构建期捕获；
  内置默认 provider 覆写为 0；
- 测试：材料内容/顺序/成功过滤、Runtime 捕获（默认 20 / 覆写 / 内置 0）。

### M52 组件：逐字帧策略与可观测 — 完成（2026-11-05）

- 配置 `context_recent_turns`（默认 0）；provider 声明
  `min(K, l1_turns)`；assembler 生效窗口
  `min(K, l1_turns, 材料)`、帧前缀、按序排除、补充为空跳过改写；
- 观察面 `Assembled`（逐字轮数、各层召回计数、改写状态）；assembler
  接入观察者；
- 测试：帧布局、回放轮次不重复、排除逻辑、改写跳过、事件字段、
  夹取（材料上界与 `l1_turns`）。

### M53 文档、验证与发布准备 — 完成（2026-11-05）

- README（根与五个 crate 副本同步、组件 README）、扩展指南 §3/§4.2、
  v8、CHANGELOG 已同步；
- 全量 294/294、clippy/doc 零警告、fmt 干净；
- 发布准备（版本 bump、dry-run、发布说明）待执行。

## 8. 发布记录

（待发布）
