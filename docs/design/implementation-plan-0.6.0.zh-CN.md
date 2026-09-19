# Synonz 0.6.0 实现计划

- 状态: RELEASED（2026-09-19，五个 crate 0.6.0 已发布；tag `v0.6.0`
  + GitHub Release；发布记录见 §6）
- 日期: 2026-09-19
- 依据: ADR-0020（APPROVED——记忆管理面与条目语义）、架构设计文档
  v7（APPROVED）
- 性质: 开发文档——0.6.0 破坏性单波发布的执行次序、迁移映射与验收
  基准；架构决策理由见 ADR-0020 与 v7，本文不重复论证
- 前置: 0.5.0 已发布（2026-09-14；221/221 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.5.0；文档基线 = ADR-0020 + v7（均
  APPROVED，v6 已 SUPERSEDED）
- 关联延期项（已记录边界）: 应用日常新增（`add`）、框架外导入的事实/
  有序保证、物理删除/副本/备份/加密擦除、L1 留存与会话级清理、严格
  全局时间序（跨类型持续归并）、语义搜索、事实聚合/矛盾消解（策略
  层）、错误覆盖的内容质量（策略层）

---

## 1. 总览

0.6.0 是**记忆管理面落地波**：把 ADR-0020 与 v7 的全部决策实施为
代码。六个里程碑按依赖链排序：

```
M33 条目数据模型与新鲜度（id / updated_at / L3 upsert 保 id / 召回排序）
  ↓
M34 存储契约扩展（L2/L3 必选 get/update/remove/list + 内置实现 + 存储层 keyset）
  ↓
M35 边界重切（MemoryLayerStore 内部对象 / Memory 应用面骨架 / reader 内部化 / 引擎接线）
  ↓
M36 管理面（类型 + list 合并分页 + get/edit/forget/forget_matching + 事实 + test-util）
  ↓
M37 并发纪律（维护写入前按 id 重校验 + 按 id 删源 + 丢弃产物 + 事实）
  ↓
M38 收尾与验证（扩展指南、迁移指南、CHANGELOG、README、全量回归、发布决策留后）
```

依赖理由：M33 先落条目数据模型（id/updated_at）——后面所有操作都
以它为地址与新鲜度；M34 把存储契约扩到"按 id 可读改删 + 可枚举"，
是管理面与并发纪律的基座；M35 完成"应用面 / 内部分离"的公开边界
重切；M36 在此之上实现管理面（含合并分页与事实）；M37 落地"遗忘
不回写"的并发纪律；M38 收尾扫全局。

**每一波的完成定义**：子项全部落地 + 新增/迁移测试绿 + 全量回归绿
（`cargo fmt --check` / `clippy --workspace --all-targets --all-features`
零警告 / `cargo test --workspace --all-features` 全通过 /
`cargo doc --workspace --no-deps` 零警告）+ 与 v7 决策落点核对 +
**里程碑完成记录**（含 B 类决议与 A 类请示结果）写入本计划。

---

## 2. 里程碑明细

### M33 条目数据模型与新鲜度

| # | 子项 | 要点 |
|---|---|---|
| 1 | 条目加 `id` | `L2Entry` / `L3Entry` 增 `id`（框架生成 **UUID v4**；`new(...)` 自动生成；`#[serde(default = ...)]` 兼容旧数据（缺失即生成） |
| 2 | 条目加 `updated_at` | `L2Entry` / `L3Entry` 增 `updated_at`（创建 = `created_at`；任何更新刷新）；旧数据缺失 → 归一为 `created_at` |
| 3 | L3 `upsert` 保 id | 同身份替换时**保留原 id**（行为契约更新，内置实现按此改） |
| 4 | 召回排序 | `l3_query` 由 `created_at desc` 改为 **`(updated_at desc, id asc)`**（内置实现 + rustdoc 同步） |
| 5 | 验证 | 迁移缺省（旧序列化数据）、排序、保 id；全量回归 |

### M34 存储契约扩展

| # | 子项 | 要点 |
|---|---|---|
| 1 | L2 契约 | 必选新增：`get(subject, id) -> Result<Option<L2Entry>>`；`update(subject, entry) -> Result<bool>`（false=不存在）；`remove(subject, id) -> Result<bool>`（**幂等**：不存在返回 false，不报错）；`list(subject, MemoryStoreQuery) -> Result<MemoryPage<L2Entry>>` |
| 2 | L3 契约 | 同上四项 + `upsert` 保 id 语义 |
| 3 | 存储层 keyset | `MemoryStoreQuery`（单源：`after: Option<MemoryCursor>` + `limit` + 过滤集；`memory_type` 由存储层隐含）；排序 `(updated_at desc, id asc)`；`next=None` 即末页 |
| 4 | 内置实现 | `InProcessMemoryL2Store` / `InProcessMemoryL3Store` 全部补齐（含按 id 操作与分页）；**L1 契约不动** |
| 5 | 错误语义 | `MemoryStoreError` 加性变体 `EntryNotFound`（供 `edit`/`update` 缺失场景；remove 保持幂等 bool） |
| 6 | 验证 | 契约行为测试：按 id 增删改查、保 id、幂等删除、分页在增删下稳定（keyset） |

### M35 公开边界重切

| # | 子项 | 要点 |
|---|---|---|
| 1 | 内部对象 | 新建 crate 内部 `MemoryLayerStore`：三存储槽聚合 + 全部分层原语（append/pop/upsert/query/len/window/read + 新增 by-id）+ `MemoryReader` 构造 + 维护用辅助；Runtime `build()` 组装并唯一持有 |
| 2 | 应用面 | `Memory` 收敛为应用面（内部持有 `MemoryLayerStore`）；`runtime.memory()` 返回应用面；新增 `pub(crate)` 内部访问器（供引擎/Runtime/后台任务） |
| 3 | reader 内部化 | `Memory::reader` 构造收为 `pub(crate)`；`MemoryReader` **类型保持公开**；引擎经内部对象构造 reader（载荷形态不变） |
| 4 | 引擎/Runtime 接线 | `Context::on_turn_completed` 的 `memory` 参数改为 `&MemoryLayerStore`；agent 调用点、后台维护任务、终结促进（`finalize_conversation`）全部改经内部对象 |
| 5 | 公开面下线 | 分层原语从公开面移除（编译期核对）；`lib.rs` 导出面更新（新增管理类型、移除旧方法可见性） |
| 6 | 测试迁移 | 集成测试的"按层种子/直读"改为 `test-util`（M36 提供）或端到端观测；crate 内部测试用内部对象 |
| 7 | 验证 | 全量回归；公开面无分层原语；除新增能力外行为等价 |

### M36 管理面

| # | 子项 | 要点 |
|---|---|---|
| 1 | 类型 | `MemoryItem` / `MemoryType` / `MemorySource` / `MemoryQuery` / `MemoryCursor` / `MemoryListCursor` / `MemoryPage<T, C>` / `MemoryForgetResult`（公开；`non_exhaustive` 按需） |
| 2 | `list` | 结构序（先 `Summary` 块后 `Knowledge` 块）；块内 `(updated_at desc, id asc)`；**不足跨源补拉且跨界一次**；阶段游标；`memory_type=Some` 退化为单源 |
| 3 | `get` / `edit` | `get(subject, id) -> Option<MemoryItem>`；`edit(subject, id, content)` 原地更新（id / memory_type / source / created_at 不变，`updated_at` 刷新） |
| 4 | `forget` / `forget_matching` | 单条（id）+ 批量（复用 `MemoryQuery` 过滤）；**逐条尽力**（单条失败不中断整批）；返回 `MemoryForgetResult`（removed + 失败清单）；操作返回后立即不可见 |
| 5 | 事实 | `MemoryEvent::Updated { subject_id, memory_type, id }` 与 `Removed { subject_id, memory_type, ids }`（**无内容**；ids 受 limit 有界） |
| 6 | `test-util` 入口 | 外部槽作者/下游测试：种子入口 + `MemoryReader` 构造入口（与 `MockModel` 同模式） |
| 7 | 验证 | 分页（块序/补拉/跨界一次/游标在删除下不跳不重/过滤）；编辑保 id 且刷新新鲜度；删除立即不可见；事实无内容；memory_type 过滤 |

### M37 并发纪律（遗忘不回写）

| # | 子项 | 要点 |
|---|---|---|
| 1 | 写入前重校验 | 蒸馏 / 终结促进：**按 id 重读来源** → 只把幸存者交给策略 → **写前再校验**来源集合 → 一致则写，不一致**丢弃本次产物** |
| 2 | 按 id 删源 | 蒸馏与终结促进的"弹出"改为按 id `remove` 幸存者集合；压实（L1）保持按位置弹出（L1 不可管理，无竞态面） |
| 3 | 事实 | 跳过/丢弃经 `FlowFailed { stage, moment: Background | AtConversationEnd }` 可见（不新增词汇） |
| 4 | 验证 | 确定性竞态测试（可控策略/模型）：①删除来源后不回写（无 L3 残留）；②部分来源被删时只处理幸存者；③编辑与系统写入同权（后到者生效、id 不变） |

### M38 收尾与验证

| # | 子项 | 要点 |
|---|---|---|
| 1 | 测试全量 | 全量回归（fmt / clippy / doc / test --all-features）；新增测试按 §4；新引入 flaky 即缺陷 |
| 2 | **示例验证**（契约变更必做） | `cargo build -p synonz-examples --bins` 全部通过；**离线示例实跑**（`custom_tool` / `events` / `cancellation` / `scheduler` / `mcp_tools`）exit 0；**网络示例**（`openai_chat` / `anthropic_chat`）无 key 时提示退出（exit 0）；`chat_tui` 随新契约编译通过，并由人工在交互终端实跑（无头环境无法应答终端查询）；**触及变更面**（`Memory` facade / `MemoryReader` / 条目类型 / 事件词汇）的示例同步改写并记录 |
| 3 | 文档 | **扩展指南更新**（管理面用法、导入边界、自定义存储迁移清单、"错误覆盖归策略质量"的规则）；**迁移指南**（自定义存储必补方法、L3 upsert 保 id、公开面移除、id/updated_at 迁移）；CHANGELOG 0.6.0（破坏项 + 迁移表 + 边界）；README 指针（随 v7 批准已更新）；v7 落点核对与实施记录 |
| 4 | 命名终稿 | 按 coding.md §5 核对：`MemoryType` / `MemorySource` / `MemoryQuery` / `MemoryCursor` / `MemoryListCursor` / `MemoryPage` / `MemoryForgetResult` / `MemoryStoreQuery` / `MemoryLayerStore` |
| 5 | 发布决策 | bump 0.5.0→0.6.0、五份 crate README 同步、dry-run、依序 publish、tag `v0.6.0`、GitHub Release——**等 irylex 确认后执行** |

---

## 3. 迁移映射表（旧 → 新）

| 旧（0.5.0 代码） | 新（0.6.0） |
|---|---|
| `Memory` 分层 facade（`l1_append`/`l1_pop_oldest`/`l1_len`/`l1_window`/`l2_*`/`l3_*`） | `Memory` = 应用面（`list`/`get`/`edit`/`forget`/`forget_matching`）；分层原语迁入 crate 内部 `MemoryLayerStore`（应用不可见） |
| `Memory::reader(&subject)`（公开构造） | 构造内部化；策略槽经载荷获得 `MemoryReader`；外部单测走 `test-util` |
| `MemoryL2Store` / `MemoryL3Store`（append/read/len/pop_oldest、upsert/query/len） | **必选新增** `get` / `update` / `remove` / `list`（自定义实现必须补齐） |
| `MemoryL3Store::upsert`（可整体替换） | 同身份替换**保留原 id**（行为契约更新） |
| `L2Entry` / `L3Entry`（无 id / updated_at） | 增 `id` 与 `updated_at`（旧序列化数据缺失时迁移缺省） |
| 召回排序 `created_at desc` | `(updated_at desc, id asc)` |
| 应用经分层原语注入/直读记忆 | 无公开写路径；测试走 `test-util`；批量导入/迁移由应用自持存储句柄在框架外完成（边界） |
| `MemoryEvent`（TurnArchived/Compacted/Distilled/Promoted/FlowFailed） | 追加 `Updated` / `Removed`（无内容） |
| 无条目级查看/纠正/遗忘 | `list` / `get` / `edit` / `forget` / `forget_matching`（keyset 分页、结构序、不足补拉） |

---

## 4. 验收总标准

1. **六里程碑逐项完成**，每波全量回归绿（fmt / clippy / doc / test）；
2. **存储契约**：L2/L3 具备 `get`/`update`/`remove`/`list`；按 id
   更新/删除；`upsert` 同身份替换保 id；L1 契约不变；
3. **分页**：块内 `updated_at` 倒序；结构序（Summary 块 → Knowledge
   块）；不足跨源补拉且**跨界一次**；keyset 游标在增删下稳定（不跳
   不重）；`memory_type` 过滤退化为单源；
4. **管理语义**：`edit` 原地更新（id 不变、`updated_at` 刷新）；
   `forget`/`forget_matching` 逐条尽力、返回 `MemoryForgetResult`；
   操作后所有读路径立即不可见；
5. **并发纪律**：删除与在途维护竞争时不回写（确定性测试）；被删
   来源不进入变换；部分来源被删只处理幸存者；跳过/丢弃经
   `FlowFailed` 可见；
6. **新鲜度**：列表块内排序、分页游标、召回排序统一 `(updated_at
   desc, id asc)`；编辑刷新并在列表与召回同时生效；迁移缺省
   `updated_at = created_at`；
7. **公开面**：`Memory` 不再暴露分层原语（编译期核对）；`reader`
   构造不可见（编译期）；`MemoryReader` 类型仍公开；应用经管理动词
   完成闭环；不提供日常新增（边界）；
8. **事实**：`Updated`/`Removed` 发出且不含内容；
9. **文档一致**：扩展指南与迁移指南落地；CHANGELOG 0.6.0 完整；
   v7 落点核对通过；
10. **示例验证（契约变更必做）**：`cargo build -p synonz-examples
    --bins` 全部通过；五个离线示例实跑 exit 0；两个网络示例无 key
    时提示退出；`chat_tui` 编译通过并人工交互实跑；触及变更面的示例
    已同步改写并记录；
11. **延期项记录在案**：`add`/框架外导入保证/物理擦除/L1 清理/严格
    时间序/语义搜索/事实聚合/内容质量。

---

## 5. 风险与开放项

**已决项**（预先定案——不留到实施期逐段处理）：

| 项 | 决议 |
|---|---|
| `id` 生成 | 框架生成 **UUID v4**（`uuid` 依赖，MIT/Apache-2.0 双许可；字符串形态对调用方不透明）；`new(...)` 自动生成 |
| 旧数据迁移 | `id` 缺失 → 反序列化时生成；`updated_at` 缺失 → 归一为 `created_at` |
| `update`/`remove` 返回 | `bool`（存在性）；`remove` 幂等；`MemoryStoreError` 加 `EntryNotFound`（加性）用于 `edit`/`update` 缺失 |
| `edit` 字段范围 | 仅 `content`；`id`/`memory_type`/`source`/`created_at` 不变；`updated_at` 刷新 |
| `MemoryQuery` 过滤集 | `memory_type` / 会话 / 时间范围 / 关键字 / `after` / `limit`（`non_exhaustive` 加字段） |
| `MemoryStoreQuery` | 存储层**单源**版本（`after: MemoryCursor` + limit + 同过滤集；`memory_type` 隐含） |
| 合并列表算法 | 每次从当前阶段源拉一页；不足补拉下一阶段；**跨界一次**；阶段游标 `MemoryListCursor` |
| 遗忘结果 | 单条与批量共用 `MemoryForgetResult`（removed + 逐条失败清单） |
| 维护重校验 | 两次（变换前 + 写入前）；不一致**丢弃产物**并发事实 |
| 弹出路径 | 蒸馏/促进按 id 删源；压实（L1）保持按位置弹出 |
| 可见性 | 立即生效（无缓存层）；在途读可能已观察旧值（正常线性化语义） |
| `test-util` 入口 | 种子 + `MemoryReader` 构造（命名实施时定，与 `MockModel` 同模式） |
| L1 | 不参与条目管理；契约与行为不变 |

**实施期决策协议**（对涌现事项）：

- **A 类（用户门控）**：影响公开 API 契约、可观察行为语义或不可逆
  承诺的事项——带选项与推荐向 irylex 请示，确认后实施；
- **B 类（代理可决，按既定规则）**：内部结构、命名（coding.md §5
  命名立法）、有界影响的具体值与测试策略——由实施代理按既定规则
  定案：开工时明确、完工时记录；
- **记录义务**：A 类进 ADR/v7（涉及语义时）；B 类进本计划的
  "里程碑完成记录"与提交信息——决议不得只存在于聊天或代码中。

**执行期约定**：

| 项 | 约定 |
|---|---|
| 发布节奏 | M38 发布执行等 irylex 单独确认 |
| 版本号 | 工作标签 0.6.0（MINOR：加性新能力 + 破坏项同波）；发布准备时按纳入范围定 |
| 单波策略 | 破坏项集中本波发布，不拆分（与 ADR-0020 一致） |

---

## 6. 里程碑完成记录

### M33 条目数据模型与新鲜度 ✅（2026-09-19）

- `L2Entry` / `L3Entry` 增 `id`（框架生成 **UUID v4**——评审修订：
  由自建"epoch+pid+计数"方案改为标准 UUID；`uuid` 依赖已在依赖树中）
  与 `updated_at`（创建 = `created_at`，更新刷新）；`new(...)` 自动
  生成 id 并置时间；
- **旧数据迁移缺省**：`id` 缺失 → 反序列化时生成
  （`#[serde(default = ...)]`）；`updated_at` 缺失 → 0，排序按
  `max(updated_at, created_at)` 归一（crate 内部 `freshness()`）；
- L3 `upsert` 同身份替换**保留原 id**（内置实现）；
- 召回排序：`l3_query` 改为 `(freshness desc, id asc)`；
- 测试：构造置 id/时间、旧序列化数据缺省回退（2 项）。

### M34 存储契约扩展 ✅（2026-09-19）

- 公开类型：`MemoryCursor`（单源位置 `updated_at + id`）、
  `MemoryPage<T, C = MemoryCursor>`（`items` + `next`，带 `new`）、
  `MemoryStoreQuery`（会话 / 时间范围 / 关键字 / `after` / `limit`
  + `with_*` 构造链）；`MemoryStoreError` 加性变体 `EntryNotFound`；
- 契约：`MemoryL2Store` / `MemoryL3Store` **必选新增** `get` /
  `update`（bool=存在性）/ `remove`（幂等 bool）/ `list`（keyset）；
  L3 `upsert` 文档更新为"保 id"；L1 契约不变；
- 内置实现全部补齐（`page_entries` 归并 + `contains_ci`）；
- **B 类决议**：游标为值（`(freshness, id)` 严格后位），对跨界删除
  鲁棒（不跳不重）；`limit=0` 返回空末页；`update` 由调用方提供完整
  条目（并负责刷新 `updated_at`）；
- 测试：L2 增删改查往返、分页在删除下稳定、组合过滤；L3 upsert 保
  id + 增删改查（4 项）；`BrokenL2/L3` 测试桩补齐新方法；
- 验证：全量回归 **227/227** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净（M33+M34 合并验证）。

### M35 公开边界重切 ✅（2026-09-19）

- 新增 crate 内部 `MemoryLayerStore`：三存储槽聚合 + 全部分层原语 +
  按 id 操作（get/update/remove/list）+ `MemoryReader` 构造；`Clone`
  廉价（三个 `Arc`）；
- `Memory` 收敛为应用面（持有内部对象与事实出口 `EventBus`）；
  `runtime.memory()` 返回应用面；新增 `Runtime::memory_layers()` 与
  `Memory::layers()`（均 `pub(crate)`）；
- `Memory::reader` 构造收为 `pub(crate)`；**`MemoryReader` 类型保持
  公开**，改为包装内部对象；
- 接线：`Context::on_turn_completed(…, memory: &MemoryLayerStore, …)`；
  `BackgroundMaintenanceTask.memory: MemoryLayerStore`；agent 装配与
  轮末调用、`finalize_conversation` 全部改经内部对象；
- 公开面下线：分层原语从 `Memory` 移除（编译期核对）；
- 测试迁移：集成测试种子改 `test-util`（`seed_l1/l2/l3` +
  `l1/l2/l3_len_for_tests` + `reader_for_tests`），断言走公开管理面或
  test-util 读数。

### M36 管理面 ✅（2026-09-19）

- 公开类型：`MemoryType`（Summary/Knowledge）、`MemorySource`、
  `MemoryItem`、`MemoryQuery`、`MemoryListCursor`、`MemoryPage<T, C>`、
  `MemoryForgetFailure`、`MemoryForgetResult`；
- 动词：`list`（结构序 + 块内 `(freshness desc, id asc)` + 不足跨源
  补拉且**跨界一次** + 阶段游标 + `memory_type` 单源）、`get`、
  `edit`（原地更新、id 不变、`updated_at` 刷新）、`forget`、
  `forget_matching`（逐条尽力、`limit` 为批量上限、返回
  `MemoryForgetResult`）；
- 事实：`MemoryEvent::Updated` / `Removed`（无内容）；`Memory` 持
  `EventBus`（runtime build 先建总线再注入），管理操作经总线发事实；
- 测试：合并分页（块序/补拉/续页/单源）、编辑与遗忘（保 id、
  `EntryNotFound`、幂等）、批量过滤与上限、管理事实（4 项）；
- **B 类决议**：①`MemoryListCursor.position` 取
  `Option<MemoryCursor>`（`None` = 阶段起点；v7 草图记为
  `MemoryCursor`，实施细化为可选）；②`L2Entry` 增 `topic` 字段
  （`with_topic`；压实写入当前主题；旧数据缺省空串）——`MemoryItem`
  的来源需要它；③新增测试辅助类型名 `MemoryForgetFailure`；
  ④`test-util` 除种子/reader 外补三个读数方法（`*_len_for_tests`）；
- 验证：全量回归 **231/231** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净（M35+M36 合并验证）。

### M37 并发纪律（遗忘不回写）✅（2026-09-19）

- **蒸馏**（`BackgroundMaintenanceTask::distill`）：peek → **变换前
  按 id 重校验**（只把幸存者交给策略；被提前遗忘的来源跳过并发
  `FlowFailed{Distill}`）→ 策略纯变换 → **写前逐条按 id 认领
  （remove）幸存者**：认领失败（期间被删）或出错即**丢弃本次产物**
  + `FlowFailed{Distill}`；全部认领成功才写 L3；
- **终结促进**（`Runtime::finalize_conversation`）：改为 `l2_read` +
  逐条按 id `remove` 认领（`false` 跳过，不复活）；upsert 失败发事实；
- 压实（L1）保持按位置弹出（L1 不可管理、无竞态面）；
  `MemoryLayerStore::l2_pop_oldest` 包装退役（存储 trait 方法保留）；
- **确定性竞态测试**：`GatedDistiller`（首个调用阻塞并暴露源
  id/内容）+ 三回合触发蒸馏 → 在途遗忘来源 → 释放 → 断言知识中不含
  被遗忘内容、`FlowFailed{Distill}` 可见；
- **B 类决议**：预变换跳过仅发事实；写前认领失败一律丢弃产物
  （宁可本次不蒸馏，也不回写遗忘内容）；"部分来源被提前遗忘"的
  预变换路径由代码保证（公开 API 无法精确打点，未单独测试）；
- 验证：全量回归 **232/232** 全绿、clippy 零警告、`cargo doc
  --workspace --no-deps` 零警告、fmt 干净。

### M38 收尾与验证 ✅（2026-09-19）

- **示例验证（契约变更必做）**：`cargo build -p synonz-examples
  --bins` 全部通过；**五个离线示例实跑 exit 0**（`custom_tool` /
  `events` / `cancellation` / `mcp_tools` / `scheduler`）；**两个网络
  示例**无 key 时提示退出（exit 0）；`chat_tui` 编译通过 + 非 TTY
  友好提示（交互实跑由人工另行确认）；示例未触及变更面（无需改写）；
- **文档**：扩展指南更新（新增 §2.1 管理面配方、扩展点表与边界行、
  迁移指引、"常见坑"8-11）；**迁移指南新增**
  `docs/design/migration-0.6.0.zh-CN.md`（应用代码 / 自定义存储 /
  数据缺省 / 行为变化 / 检查清单）；CHANGELOG 0.6.0 完整
  （Highlights / Added / Changed / Breaking / Migration / Notes）；
  v7 落点核对通过（实施记录 + 状态表更新）；README 六份同步
  （`Memory` 描述改为"管理面 over 三存储槽"）；
- **命名终稿**：按 coding.md §5 核对通过（referential nouns、无
  碰撞；`MemorySource` 与 `EventSource`、`MemoryStoreQuery` 与
  `MemoryQuery` 的区分已在指南/文档写明）；
- **全量验证**：fmt 干净、clippy 零警告、`cargo doc --workspace
  --no-deps` 零警告、`cargo test --workspace --all-features`
  **232/232** 全绿；
- **发布决策**：**待 irylex 确认**——版本号 0.6.0（工作标签）、五份
  crate README 同步（已随仓库同步）、依序 publish、tag `v0.6.0`、
  GitHub Release；本波止于"等待发布"。

### 发布记录（2026-09-19）✅

- **批准**：irylex（2026-09-19，"review 完了，你可以发布了"）；
- **发布前**：版本 bump 0.5.0 → 0.6.0（workspace + 5 path 依赖）；
  CHANGELOG 定稿 `[0.6.0] - 2026-09-19`；六份 README 逐字节一致；
  全量回归 **232/232**（clippy/doc 零警告、fmt 干净）；提交
  `6674ef5 chore(release): prepare 0.6.0` 推送 origin；
- **发布**：依序 publish `synonz-derive` → `synonz` → `synonz-openai`
  → `synonz-anthropic` → `synonz-mcp`，五者 0.6.0 全部上传成功；
  tag `v0.6.0` 推送；GitHub Release：
  https://github.com/irylex-ai/synonz/releases/tag/v0.6.0 ；
- **发布后核验**（实测非假设）：crates.io 五 crate
  `max_stable_version = 0.6.0`；从 static.crates.io 下载五份 `.crate`
  解包，包内 README 与仓库 README **逐字节一致**（v7 指针在位）；
- **状态**：RELEASED。
