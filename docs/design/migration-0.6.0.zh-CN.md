# Synonz 0.6.0 迁移指南

- 状态: VERIFIED（2026-09-19，随 0.6.0 波次实施；内容与代码行为一致）
- 日期: 2026-09-19
- 依据: ADR-0020（APPROVED）、架构设计文档 v7（APPROVED）
- 性质: 开发文档——0.5.0 → 0.6.0 的破坏项清单与迁移动作；
  决策理由见 ADR-0020 与 v7

---

## 1. 应用代码

| 0.5.0 | 0.6.0 |
|---|---|
| `memory.l1_append(...)` / `l2_append` / `l3_upsert`（按层写） | **公开写路径移除**：新记忆来自对话沉淀；测试/fixtures 用 `test-util` 的 `seed_l1` / `seed_l2` / `seed_l3`；批量导入/迁移由应用自持存储句柄在框架外完成（无事实/无有序保证——边界） |
| `memory.l1_len` / `l2_len` / `l3_len` / `l2_read`（按层读） | 管理面 `list` / `get`（`MemoryItem`）；测试可用 `test-util` 的 `l1_len_for_tests` / `l2_len_for_tests` / `l3_len_for_tests` |
| `memory.reader(&subject)`（公开构造） | **构造内部化**；策略槽经载荷获得 `MemoryReader`；测试槽用 `test-util` 的 `reader_for_tests` |
| —— | 新增管理面：`list` / `get` / `edit` / `forget` / `forget_matching`（见扩展指南 §2.1） |

`Context` 配置面与策略槽签名不变（`ContextAssembler` /
`TurnInputRewriter` / `MemorySummarizer` / `MemoryDistiller` /
`ConversationTopicDetector` 的入参与语义保持）。

## 2. 自定义存储实现（必选补齐）

`MemoryL2Store` / `MemoryL3Store` 新增四个**必选方法**（编译期强制，
不是运行期报错）：

```rust
fn get(&self, subject: &Subject, id: &str) -> Result<Option<Entry>, MemoryStoreError>;
fn update(&self, subject: &Subject, entry: Entry) -> Result<bool, MemoryStoreError>; // false = 不存在
fn remove(&self, subject: &Subject, id: &str) -> Result<bool, MemoryStoreError>;     // 幂等
fn list(&self, subject: &Subject, query: MemoryStoreQuery)
    -> Result<MemoryPage<Entry>, MemoryStoreError>;
```

契约要点：

- **`list` 排序**：`(freshness desc, id asc)`，其中
  `freshness = max(updated_at, created_at)`；
- **游标是值**（`(freshness, id)` 严格后位）：跨界删除时不得跳条或
  重复；
- **`limit` 有界**（`0` 返回空末页）；`next=None` 表示末页；
- **`update` 接收完整条目**（id 即地址；调用方负责刷新
  `updated_at`）；
- **`MemoryL3Store::upsert` 行为更新**：同身份
  `(subject, conversation, topic)` 替换时**保留原 id**；
- `MemoryL1Store` **不变**（L1 不参与条目管理）。

## 3. 数据迁移（旧序列化数据）

| 字段 | 缺失时的规则 |
|---|---|
| `L2Entry.id` / `L3Entry.id` | 反序列化时自动生成（框架 id） |
| `L2Entry.updated_at` / `L3Entry.updated_at` | 0；排序按 `max(updated_at, created_at)` 归一 |
| `L2Entry.topic` | 空串（旧摘要无主题标签） |

新构造：`L2Entry::new(conversation_id, content, index).with_topic(topic)`；
`L3Entry::new(identity, content)`；`id` 与时间戳自动生成。下游若用
结构体字面量构造条目，需改用构造器（`non_exhaustive` 结构体新增
字段）。

## 4. 行为变化

- **召回排序**：由 `created_at desc` 改为 `(updated_at desc, id asc)`
  ——编辑/更新过的条目会前置；结果顺序可能变化；
- **蒸馏/终结促进**：由"按位置弹出"改为"**按 id 认领**"——在途被
  遗忘的来源被跳过；若来源集合在变换期间变化，本次产物被丢弃并发
  `FlowFailed { stage: Distill }`（`L2` 保留给后续轮次）；
- **同权更新**：用户 `edit` 与系统蒸馏写同一身份槽，后到者生效；
  要"编辑优先"应在策略层解决（内核不设优先级）；
- **事实词汇**：新增 `MemoryEvent::Updated` / `Removed`（无内容）；
  `non_exhaustive` 枚举的匹配需要通配分支。

## 5. 迁移检查清单

- [ ] 应用不再调用分层原语（编译期会报错）；改用管理面 / `test-util`；
- [ ] 自定义 `MemoryL2Store` / `MemoryL3Store` 补齐四个按 id 方法；
- [ ] `upsert` 同身份替换保留原 id；
- [ ] 持久化数据按 §3 处理缺省字段；
- [ ] 召回排序变化的回归确认；
- [ ] 事件匹配补通配（`Updated` / `Removed`）；
- [ ] 测试 fixtures 改用 `seed_*` 与 `*_for_tests`。
