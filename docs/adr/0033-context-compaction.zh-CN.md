# ADR-0033: 上下文压缩命令——组件的显式 compact（边界、marker 契约与执行语义）

- 状态: APPROVED（2026-11-06，irylex 人工评审通过）
- 日期: 2026-11-06
- 决策者: irylex（逐项决策；遵循 `.opencode/rules/architecture.md` §3–§9
  的渐进决策流程）
- 性质: 组件公开 API + L1 契约扩展 ADR——在 ADR-0032（逐字近期窗口）
  之上增加显式压缩命令；框架零改动（维持决策 #24）
- 关联: ADR-0032（逐字近期窗口）、ADR-0031（持久化压实游标）、
  决策 #24（框架不做主动压缩）；上游消费场景（Valkyrie 0.6.0，
  coding IDE 的 `/compact`）
- 版本承载: 0.11.0（L1 契约破坏性变更：两个必选方法）

## Context（背景）

ADR-0032 让组件第一次拥有一个**受控的上下文通道**：读相位可以把
最近 K 轮原样回放进模型可见帧（逐字近期窗口），召回继续负责更早的
材料。这使"显式压缩上下文"第一次在组件层有意义——决策 #24 的结论
是框架层不做主动压缩（框架没有自己的上下文通道），压缩归记忆实现
自决；现在实现有了通道。

应用场景（coding IDE）：用户在长会话中途执行 `/compact`，要求把当前
可见的近期上下文压缩掉、会话继续，而不是等轮次自然滚出窗口。ADR-0032
已为此预留了扩展：窗口实现为"最近 K 轮**后缀**"，因此加入一个
"每会话压缩边界"即可变为"边界之后的最近 K 轮"，不改帧布局。

现状：组件没有显式的压缩入口；窗口只能随新轮次推移、L1 淘汰或召回
排序自然收缩；被压缩材料没有"从上下文退出"的持久表示。

## Problem（问题）

应用需要一类显式动作：**把会话当前可见上下文压缩到某一轮为止**——
被压缩轮次先完成记忆整合（L2 摘要），随后退出模型可见上下文（逐字
窗口与 L1 召回），会话继续；动作失败必须可见且可重试；压缩状态必须
跨进程重启保持（重启不能让用户压缩掉的原文重新出现）；不增加每轮
I/O；对既有应用零影响（新增 API，不改变默认行为）。动作还必须与
后台维护按会话串行，避免边界与压实游标竞态。

## Decision（决策）

### 1. API 形状：挂在组件的应用侧富面上

```rust
impl LayeredMemoryActuator {
    /// Compresses the conversation's visible context, keeping the
    /// configured number of recent turns verbatim.
    pub async fn compact_context(
        &self,
        subject: &Subject,
        conversation_id: &str,
    ) -> Result<(), MemoryFailure>;
}
```

- **应用级、异步、轮次之间调用**：并发的在途轮次不与命令串行（其
  归档落在边界之后）；在途轮次未归档，不参与边界解析。
- **压缩幅度来自组件配置**（`context_compact_keep_turns`，默认 1），
  不是调用参数：压缩是组件行为，不做按次精细控制；配置是应用级
  全局的，与 `context_recent_turns` 同属记忆形态。
- **模型**：组件配置的上下文管理模型（与终结维护同源）；未配置且
  存在待整合内容 → 显式失败（沿决策 #13 口径）。
- **返回**：`Result<(), MemoryFailure>`；过程细节走组件观察面
  （`Compacted` 的创建/更新计数；`Assembled` 反映压缩后的窗口）。
  不新增专用事件（见 §7 开放项）。
- **句柄**：复用现有 `LayeredMemoryActuator`（类型化读 + 关系删除 +
  显式压缩）；不新增公开类型。若未来出现多个按需策展动作，再评估
  独立句柄。

### 2. 压缩边界：保留最近 R 轮

- **生效窗口 K** = `min(context_recent_turns, l1_turns, 框架材料)`
  （ADR-0032）；**K = 0 时命令无操作**（没有可见原文可压缩；且不推进
  marker，避免无理由关闭 L1 召回）。
- **R = `context_compact_keep_turns`**（组件配置，默认 1，应用级
  全局）：在组件可见的 L1 轮次（`l1.entries(.., l1_turns)`）内取
  第 R 个（旧→新计数）；R 按该列表长度截断，R ≥ 长度 → 无操作
  （等价"不压缩"）。R 不是调用参数——压缩是组件行为，不做按次
  精细控制。
- **边界轮** = 该列表倒数第 R+1 条（R = 0 时即最新一条）；命令
  成功后 `mark_context_boundary(边界轮 id)`。
- **K 与 R 正交**：K（窗口容量，配置）不因 R 改变；压缩瞬间可见窗口
  = `min(R, K)`（marker 之后只有 R 轮），随后随新轮增长到 K 封顶；
  R > K 允许（逐字只有 `min(R, K)`，多出的保留轮仍在 L1 可召回）。

逻辑示例（K = 7；R 为配置值）：

| 场景 | 可见逐字窗口 |
|---|---|
| 未压缩，10 轮 | 第 4–10 轮 |
| 10 轮时 `compact_context()`（R=2） | 第 9–10 轮（第 1–8 轮退出窗口与 L1 召回） |
| 之后又过 5 轮 | 第 9–15 轮（增长到 K 封顶） |
| 同上又过 3 轮 | 第 12–18 轮；第 9–11 轮在 marker 之后但在窗口外 → L1 可召回 |
| `compact_context()`（R=0） | 空窗口；后续帧 = 本轮输入 + 召回 |
| `compact_context()`（R=9，R>K） | 第 14–20 轮；第 12–13 轮保留但非逐字，L1 可召回 |
| 默认 R=1（K>0） | 保留最近 1 轮；其余退出窗口与 L1 召回 |
| K=0 或 R≥轮数 | 无操作 |

### 3. marker 的持久化：L1 契约第二 watermark

`L1MemoryStore` 增加两个**必选**方法（破坏性变更，0.11.0）：

```rust
/// The read-phase compression boundary: the last turn included in an
/// explicit context compression (`None` = never compressed).
fn context_boundary(&self, scope: &MemoryScope)
    -> Result<Option<String>, MemoryStoreError>;

/// Marks every turn up to and including `through` as compressed out of
/// the read phase. Idempotent and monotonic: a turn the store no longer
/// holds, or one already behind the boundary, is a no-op.
fn mark_context_boundary(&self, scope: &MemoryScope, through: &str)
    -> Result<(), MemoryStoreError>;
```

- 与压实游标同处一个存储、同一持久化/fsync 路径：marker 是"轮次日志上
  的处理位置"，需要按日志 id/顺序解析、并随淘汰保持语义。
- `clear(scope)` 同时清除两个位置；删除 marker 所指条目**不清除**边界
  （边界仍有效：剩余条目都在其后，排除自然为空）。
- 内置进程内默认实现按会话维护第二个位置；持久化实现与游标同批写入。
- 不引入第 5 个契约；不放进框架的 `ConversationState`（#24 边界：
  压缩语义归实现，框架零感知）。

### 4. 执行语义

- **压实范围**：待整合轮次（ADR-0031 的 `uncompacted` 集合）中
  **≤ 边界** 的那一段；沿用 ADR-0031 的分段上限（每段 ≤ `l1_turns`，
  游标逐段推进）——命令显式要求"压缩完成"，因此**循环排空**至
  边界处，而非留给后续触发。
- **蒸馏**：沿用正常压实路径的沉淀窗口规则（ADR-0032 的
  `l3_distill_min_age_seconds`），不为命令开特例。
- **推进时机**：全部成功后才 `mark_context_boundary(边界轮 id)`；
  任何失败返回 `Err`、不推进（部分写入的重复语义按 ADR-0031 的
  至少一次处理）。
- **串行**：命令与同会话的后台维护串行——命令**等待**在途维护结束
  后执行；后台触发保持"跳过、下轮重试"。
- 不压缩真相档案；不涉及工具消息/完整转录（ADR-0032 的边界）。

### 5. 对组帧的影响

- **窗口**：可见逐字窗口 = marker 之后的轮次，最多 K 轮。
- **召回排除**：L1 召回候选排除 ≤ marker 的轮次（否则被压缩的原文会
  从召回漏回上下文，压缩失效）；L2/L3 召回不变。
- **缓存**：marker 按会话缓存（命令成功后刷新；重启后首次组帧对存储
  读一次），不增加每轮 I/O。
- `Assembled` 事件如实反映压缩后的窗口与各层召回计数。

## Alternatives Considered（备选与否决）

1. **R 作为调用参数**（`compact_context(..., keep)`）——压缩是组件
   行为，市场产品不对 `/compact` 做按次精细控制；动作参数会让每个
   调用方重复决定幅度，且组件无从提供合理默认。改为组件配置
   （`context_compact_keep_turns`，默认 1），应用级全局。
2. **清空窗口（无 R）**——语义与 API 最简，但压缩后即时连续性完全
   依赖召回；R=0 配置值已覆盖该语义，故采用保留 R。
3. **边界 = 未压实集合末尾**——只压实 pending，窗口是否收缩取决于
   压实节拍，"压缩"名不副实；否决。
4. **marker 仅内存态**——重启后压缩回退，违背"重启是正常用法"；
   否决。
5. **新增第 5 个维护状态契约**——公开面 +1、跨存储一致性，而边界
   仍需轮次日志解析；否决。
6. **放进框架的 `ConversationState`**——把记忆读相位语义带进核心
   真相域，违反 #24；否决。
7. **新增专用策展句柄**（如 curator）——角色更清晰，但为一个动作
   新增公开类型 + 访问器；当前采用 actuator 扩展，动作增多时再评估。
8. **框架层统一 compact 能力**——#24 已否决（框架没有自己的上下文
   通道）；仅当"多个实现都需要共同契约"的真实需求出现时重开。
9. **命令无条件蒸馏**——蒸馏与上下文压缩正交；沿用沉淀窗口规则；
   否决特例。

## Consequences（后果）

**破坏项（0.11.0）**

- `L1MemoryStore` 新增两个必选方法；持久化 L1 实现需维护第二个每会话
  位置（与压实游标同一写入路径，无额外 I/O）；内置默认实现同步补齐。

**加性/行为项**

- `LayeredMemoryActuator::compact_context(subject, conversation_id)`
  （新 API，无 `keep` 参数，不影响既有行为）；
- 组件配置新增 `context_compact_keep_turns`（默认 1，应用级全局，
  与 `context_recent_turns` 同属记忆形态）；
- 压缩显式且持久：被压缩轮次退出逐字窗口与 L1 召回，材料先整合进
  L2；重启不回落；
- 边界解析与 R 的夹取、K=0/R≥轮数的无操作语义按 §2；
- 失败可见（`Err`），marker 不推进，重试为至少一次。

**已知边界**

- 不压缩真相档案、不涉及工具消息/完整转录；
- 压缩期间的并发在途轮次不与命令串行（其归档落在边界之后）；
- R > K 时"保留但不保证逐字"（文档化）；
- 压力回放场景下 L1 原文与 L2 摘要仍可能同时可召回（既有边界）。

## 验证要求

- R（配置值）矩阵：R=0、R=1（默认）、R<K、R=K、R>K、R≥轮数、
  K=0（无操作）；
- 压缩后窗口、窗口随新轮增长、连续两次压缩；
- L1 召回排除（被压缩轮次不再以原文进入帧）、L2/L3 召回不变；
- 失败路径：压实失败 → `Err`、marker 不推进、重试（至少一次）；
- 持久化：重建 provider 后 marker 保持（重启不回退）；
- 与后台维护串行（命令等待、后台跳过）；
- `Assembled`/`Compacted` 事件与既有全量用例（默认无命令时行为不变）；
- L1 契约纯增量的实现者视角：内置默认实现 + 契约一致性用例。
