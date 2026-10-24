# Synonz 0.8.0 实现计划

- 状态: 已批准（2026-10-24，人工补审通过）；M45-M47 已实施并通过
  全量回归；**发布待 irylex 确认**
- 日期: 2026-10-24
- 依据: ADR-0029（Embedding 端口角色化）、ADR-0030（模型用量缓存指标）；
  上游消费需求 R1–R6（Valkyrie 0.6.0）
- 性质: 开发文档——0.8.0 增量波的范围、执行记录与验收基准；架构
  决策理由见 ADR-0029/0030 与 v8，本文不重复论证
- 前置: 0.7.0 已发布（2026-09-24；268/268 全绿、clippy/doc 零警告、
  fmt 干净）；代码基线 = 0.7.0

---

## 1. 上游需求与判定

| 项 | 判定 | 处置 |
|---|---|---|
| R1 embedding 端口角色化（P0） | 真实 | 0.8.0 实施（ADR-0029） |
| R2 TokenUsage 缓存指标（P1） | 真实 | 0.8.0 实施（ADR-0030，含 Anthropic 总输入归一行为修正） |
| R3 空 content delta 缺陷（P1） | 真实缺陷 | 0.8.0 修复 |
| R4 组件完整性审计（P1） | 无缺项（2 个发现） | 发现 1（内联嵌入失败静默）0.8.0 修复；发现 2（L2 淘汰无观察事件）记开放项 |
| R5 reasoning 持久化（P2） | 需求成立，设计级改动 | 延后（开放项 §5） |
| R6 会话部分加载（P2） | 需求成立，设计级改动 | 延后（开放项 §5） |

## 2. 决策基线

- **R1**：`embed_query` / `embed_passage` 与 `embed_inline_query` /
  `embed_inline_passage`（默认委托 `embed` / `embed_inline`；`embed`
  保持必选对称基线）。角色是语义而非文本：组件绝不拼前缀，前缀由
  实现侧按模型约定添加。组件 11 处调用点映射：召回查询、蒸馏已知
  实体检索、别名候选检索 = `query`；L1 两侧、L3 锚点/走图实体重嵌入、
  L2 摘要、实体写入、向量修复、管理编辑 = `passage`；主题检测两侧同
  为 `passage`。
- **R2**：`input_tokens` 归一为总输入；`cache_read_input_tokens` /
  `cache_write_input_tokens: Option<u64>`（`None` = 未上报、`Some(0)` =
  显式为 0；serde 默认值、None 不序列化）；以字段名词命名的链式
  修改器；逐轮累加（两侧均 `None` 保持 `None`）。适配器只有流式路径。
- **R4**：管理编辑的内联刷新失败不再静默——发
  `MemoryEvent::Failed { stage: "embed", moment: OnDemand }`，编辑继续
  生效、向量保持旧值；新增 `MemoryFailedMoment::OnDemand`；修正
  `embed_inline` 文档中"下次后台点刷新"的不实口径。

## 3. 执行记录（M45-M47）

### M45 核心与适配器

- `TokenUsage` 缓存字段、`accumulate` 累加、serde 向后兼容
  （`crates/synonz/src/event.rs`）；`MemoryFailedMoment::OnDemand`
  （`crates/synonz/src/bus.rs`）。
- `synonz-openai`：usage 缓存解析（`prompt_tokens_details.cached_tokens`
  → 回退 DeepSeek 原生 `prompt_cache_hit_tokens`）；空 content delta
  守卫 + reasoning/空 content 的 SSE 回归测试。
- `synonz-anthropic`：解析 `cache_read_input_tokens` /
  `cache_creation_input_tokens`（`message_start`，`message_delta` 有则
  更新）；`input_tokens` 归一为总输入。

### M46 组件

- `Embedding` 四个角色方法（`embedding.rs`）；11 处调用点迁移。
- `edit_entry` / `edit_entity` 上抛内联刷新失败；管理面发 `Failed`
  事实；`embed_inline` 文档改为准确口径。
- 测试 `tests/embedding.rs`：默认委托等价、记录型实现的调用点角色
  断言（含主题检测两侧）、内联失败事实与编辑仍生效。

### M47 文档与验证

- ADR-0029/0030；扩展指南（§5.1 角色与非对称模型、§6.2 用量与缓存）；
  组件 README；v8（实施记录、里程碑、端口与事件词汇）；CHANGELOG。

## 4. R4 审计结论

对照 Valkyrie 0.6.0 所列能力逐项核对 0.7.0：

- 存储端口：L1 / L2 / L3 图 / L3 向量 4 个契约齐全；
- 三策略：批量压实 / 实体提取 / 上下文改写（另有 embedding 端口）；
- 观察事件：`Compacted` / `Distilled` / `Normalized` / `Skipped` 全部
  有发射点；失败经框架 `Failed` 事实全链路可见（轮末 / 后台 / 会话
  终结）；
- 管理动词：`Memory` 五动词 + actuator 四方法（含关系删除）；
- scope 隔离、配置项齐全。

发现：

1. 管理编辑的内联嵌入失败被静默吞掉（已修复，见 M46）；
2. L2 容量淘汰无观察事件（开放项，不修）。

## 5. 开放项

- **R5 reasoning 持久化**：`ContentBlock` 无 reasoning 块（reasoning
  是叙述-only），需新增块类型 + 适配器忽略/回放 + 持久化设计；
- **R6 会话部分加载**：`ConversationStore::load` 返回全量 state，
  部分加载牵动 `Conversation` 实体语义与存储契约扩展；
- **内联刷新无排队机制**：`None`（纯异步实现）时向量保持到该记录
  下次被管线写入（L2 压实更新 / L3 蒸馏更新）；
- **L2 容量淘汰观察**（R4 发现 2）：淘汰无事件，消费侧不可见。

## 6. 验收

- 全量回归 **280/280**（含新增 12 项）；clippy / doc 零警告；fmt
  干净；离线示例全部实跑通过。
- 现有 `Embedding` 实现零改动可编译；对称模型行为不变；非对称模型
  能拿到角色化文本。
- 消费侧能拿到真实缓存命中数据；无缓存字段时行为不变。
- 兼容 provider 的流式序列不再产生空 `Text` delta。

## 7. 发布记录

（待发布）
