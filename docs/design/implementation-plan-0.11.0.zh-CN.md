# Synonz 0.11.0 实现计划

- 状态: APPROVED（2026-11-06，irylex 人工评审通过；ADR-0033 已批准）
- 日期: 2026-11-06
- 依据: ADR-0033（上下文压缩命令——组件的显式 compact：边界、marker
  契约与执行语义）
- 性质: 开发文档——0.11.0 的执行次序与验收基准；决策理由见
  ADR-0033，本文不重复论证
- 前置: 0.10.0 已发布（2026-11-05；294/294 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.10.0
- 上游场景: Valkyrie 0.6.0（coding IDE 的 `/compact`）

---

## 1. 总览

0.11.0 给分层组件增加**显式的上下文压缩命令**：应用在轮次之间调用，
组件把会话当前可见的近期上下文压缩掉——被压缩轮次先完成记忆整合
（L2 摘要），随后退出逐字窗口与 L1 召回；压缩边界持久化，重启不回退；
会话继续。压缩幅度由组件配置决定（默认保留最近 1 轮），不做按次精细
控制。框架零改动（维持 #24）；L1 契约破坏性变更（第二 watermark）。

**范围**
- L1 契约：`context_boundary(scope)` / `mark_context_boundary(scope,
  through)`（必选、幂等、单调）+ 内置默认实现；
- 组件配置：`context_compact_keep_turns`（默认 1，应用级全局）；
- `LayeredMemoryActuator::compact_context(subject, conversation_id)`
  （async，组件上下文管理模型；与后台维护串行——命令等待、后台跳过）；
- 组帧：窗口 = marker 之后且 ≤ K；L1 召回排除 ≤ marker；marker 按
  会话缓存，无每轮 I/O；
- 文档：组件 README、扩展指南、v8、CHANGELOG（含迁移）、计划记录。

**非目标**
- 不改框架（#24）；不压缩真相档案；不涉及工具消息/完整转录；
- 不做按次 keep 参数、不新增专用策展句柄（动作增多时再评估）；
- 命令不无条件蒸馏（沿用沉淀窗口规则）；不新增专用观察事件。

## 2. 决策基线（ADR-0033）

- R = `context_compact_keep_turns`（默认 1）；K = `min(context_recent_turns,
  l1_turns, 材料)`；K=0 或 R ≥ 可见轮数 → 无操作；
- 边界轮 = 组件可见 L1 列表倒数第 R+1 条；成功后
  `mark_context_boundary(边界轮 id)`；
- 压实范围 = 待整合轮次中 ≤ 边界的部分（ADR-0031 分段、循环排空）；
  蒸馏走沉淀窗口；失败 `Err`、不推进（至少一次）；
- 效果：窗口 = marker 之后 ≤ K；L1 召回排除 ≤ marker；L2/L3 不变；
- L1 契约：两个必选方法，与压实游标同存储同 fsync；`clear` 清两个
  位置；删除 marker 所指条目不清边界。

## 3. 里程碑明细

### M54 L1 契约：压缩边界 watermark

- `L1MemoryStore` 增 `context_boundary` / `mark_context_boundary`
  （文档写明幂等、单调、未知 id no-op）；
- `InProcessL1MemoryStore`：每 scope 第二个位置（内存），`clear` 一并
  清除；`remove` 删除 marker 条目不清边界；
- `L1Memory` 层封装访问器；
- 单测：初始为空、幂等推进、未知 id no-op、clear 重置、删除边界条目
  后边界仍有效（剩余条目都在其后）。

### M55 组件：压缩命令与组帧排除

- 配置：`LayeredMemoryConfig` 增 `context_compact_keep_turns: usize`
  （默认 1；文档写明应用级全局、R 与 K 的关系）；
- 维护串行：把后台的 busy 守卫升级为可按会话**等待**的同步位
  （后台触发保持 try/跳过；命令 await 等待在途维护结束），实现方式
  在计划内自定（不引入新的公开契约）；
- actuator：增加 `l1` / `config` / 维护串行位；实现
  `compact_context`：
  - 解析生效窗口 K 与配置 R；K=0 / R ≥ 可见轮数 → `Ok`（无操作）；
  - 取边界轮 = 可见 L1 列表倒数第 R+1 条；
  - 对待整合轮次中 ≤ 边界的部分循环压实（每段 ≤ `l1_turns`），
    游标逐段推进；全部成功后 `mark_context_boundary`；
  - 任一步失败返回 `Err`、不推进边界；
  - 组件模型缺失且有待整合内容 → 显式失败（沿 #13 口径）；
- assembler：
  - 读取 marker（按会话缓存；命令成功后刷新；重启后首次组帧读一次
    存储）；
  - 计算 marker 之后的 L1 轮次数 A；逐字窗口 = 最近 `min(K, A)` 轮；
  - L1 召回候选排除 ≤ marker 的轮次；L2/L3 召回不变；
- 测试：R 矩阵（0/1/K/>K/≥轮数、K=0）、压缩后窗口与增长、连续压缩、
  L1 召回排除、失败不推进、重启后 marker 保持、与后台维护串行、
  `Compacted`/`Assembled` 事件；默认（不调用命令）时既有 294 用例
  行为不变。

### M56 文档、验证与发布准备

- 文档：组件 README（命令、R 配置、marker 语义）、扩展指南（读相位
  压缩命令与边界规则）、v8（实施记录/里程碑/组件摘要）、CHANGELOG
  0.11.0（Breaking + 迁移）；
- 验证：fmt / clippy 0 / doc 0 / 全量测试 / 离线示例；
- 发布准备：版本 bump、dry-run、发布说明草稿（发布仍需人工确认）。

## 4. 兼容与迁移

- **破坏项**：`L1MemoryStore` 新增两个必选方法；持久化 L1 实现需在同一
  写入路径持久化第二个每会话位置；内置默认实现同步补齐。
- **加性项**：配置 `context_compact_keep_turns`（默认 1，仅命令使用，
  既有应用行为不变）；actuator 新方法；框架零改动。
- **迁移**：实现两个方法（可复用压实游标的存储模式：id + 快照/追加
  日志位置）；`clear` 同时清两个位置；删除边界条目不清边界。
- **消费方**：调用 `provider.actuator().compact_context(&subject, id)`
  即可（应用级配置把 `context_compact_keep_turns` 设为期望值）。
- **版本**：0.11.0（L1 契约破坏性变更 + 组件配置/API 新增）。

## 5. 风险与开放项

- **marker 缓存**：命令后刷新、重启后首次读；缓存失效若遗漏会导致
  窗口/召回未排除——用"重启恢复"与"命令后立即组帧"测试锁定；
- **并发**：命令等待在途维护；在途轮次不参与边界解析（其归档落在
  边界之后）；测试覆盖"维护进行中调用命令"；
- **至少一次**：压实部分失败后重试可能重复 L2 条目（ADR-0031 既有
  语义）；
- **R 的边界用例**：R>K、R≥轮数、K=0 的无操作语义需逐一测试；
- **不新增专用事件**：`Compacted`/`Assembled` + 返回结果已覆盖；若
  消费方需要"压缩完成"的专门事实，后续按同一观察面扩展；
- **actuator 角色变宽**：动作增多时再评估独立句柄。

## 6. 验收

- ADR-0033 的验证矩阵全部通过；
- 默认不调用命令时既有全量用例（294+）行为不变；
- 重启后压缩保持（marker 持久化）；
- 失败路径可见且不推进边界；
- 与后台维护串行；
- clippy/doc 零警告、fmt 干净、离线示例实跑通过。

## 7. 里程碑完成记录

（实施后填写）

- **M54（L1 契约）完成**：`L1MemoryStore` 增
  `context_boundary` / `mark_context_boundary`（必选、幂等、单调；
  内置进程实现与压实游标同存储；`clear` 重置两个位置、删除边界
  条目不清边界）+ `L1Memory` 读穿缓存；单测 3 个（幂等/单调与未知
  id、删除边界条目、clear 重置）。
- **M55（压缩命令与组帧排除）完成**：配置
  `context_compact_keep_turns`（默认 1）；`MaintenanceState` 提升为
  provider 共享（后台 try 占位、命令按会话等待并在释放时唤醒）；
  `LayeredMemoryActuator::compact_context`（K=0 / R≥轮数无操作；边界
  = 可见 L1 倒数第 R+1 条；≤ 边界待整合轮次分段压实并排空；沉淀窗口
  照常；成功后才推进；缺模型且有待整合 → 显式失败）；组帧窗口以
  边界收缩（`min(K, 材料, 边界之后轮数)`），L1 召回池 = 边界之后且
  回放之外的轮次。
- **M56（文档/验证/发布准备）完成**：组件 README、根 README 与 5 个
  crate 副本、扩展指南 §4.2/§5、v8（依据/实施记录/里程碑/组件行）、
  CHANGELOG 0.11.0（Breaking + 迁移）；版本 0.11.0（workspace + 6
  crate + examples，Cargo.lock 刷新）。
- **验证**：`cargo test --workspace --all-features` **306/306**
  （组件 60，含新增压缩用例 9、L1 边界用例 3）；clippy 0 告警、
  doc 0 告警、fmt 干净；6 个离线示例实跑通过；
  `cargo publish --workspace --dry-run --allow-dirty` 六包打包/验证
  通过（上传中止）。
- 发布：待人工确认（发布记录见 §8）。

## 8. 发布记录

（待发布）
