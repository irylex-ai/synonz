# ADR-0029: Embedding 端口角色化——query/passage 双角色与非对称检索模型支持

- 状态: APPROVED（2026-10-24，irylex 批准实施）
- 日期: 2026-10-24
- 决策者: irylex
- 性质: 组件契约 ADR——扩展 ADR-0026 §5（embedding 组件侧端口），
  其余决策不变
- 关联: 上游消费需求 R1（Valkyrie 0.6.0）；ADR-0028 未涉及本端口
- 版本承载: 0.8.0（纯增量；现有实现零改动可编译）

## Context（背景）

组件的 `Embedding` 端口只有一个向量化入口 `embed(text)` 与一个可选同步
快路径 `embed_inline(text)`。组件内部存在两种语义不同的调用：

1. **检索查询**：召回查询、蒸馏时检索已知实体、别名归一候选检索——它们
   作为"查询"去和已存向量比较；
2. **存储文本**：L1 轮次、L2 摘要、L3 实体文本、向量修复——向量化后
   **存为**候选（对照物）。

非对称检索模型（如 E5 系列，训练约定 `query: ` / `passage: ` 前缀）要求
两侧使用不同编码；单方法端口使其无法同时表达两种语义，组件也无法在
内部分辨。

## Problem（问题）

端口必须能让实现方区分 query 与 passage，组件不得内置任何模型前缀约定，
扩展必须非破坏。

## Decision（决策）

### 1. 端口形态：双角色方法 + 默认委托

```rust
pub trait Embedding: Send + Sync + 'static {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>>;

    fn embed_query<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        self.embed(text)
    }
    fn embed_passage<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, MemoryFailure>> {
        self.embed(text)
    }

    fn embed_inline(&self, _text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        None
    }
    fn embed_inline_query(&self, text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        self.embed_inline(text)
    }
    fn embed_inline_passage(&self, text: &str) -> Option<Result<Vec<f32>, MemoryFailure>> {
        self.embed_inline(text)
    }
}
```

- `embed` 保持必选——对称实现的唯一实现点；`HashEmbedding` 不变；
- 角色是**语义角色**而非文本前缀：组件绝不拼接前缀，前缀由实现方在
  自己的覆写中按模型约定添加。

### 2. 组件调用点映射（锁死）

| # | 调用点 | 角色 |
|---|---|---|
| 1 | assembler：召回查询嵌入 | query |
| 2 | l3_memory：蒸馏批量检索向量（已知实体检索） | query |
| 3 | l3_memory：别名候选检索名 | query |
| 4 | assembler：L1 轮次 input/response | passage |
| 5 | assembler：L3 锚点/走图实体文本重嵌入（去重比较） | passage |
| 6 | l2_memory：L2 摘要内容（写前） | passage |
| 7 | l2_memory：L2 编辑内容（内联快路径） | passage |
| 8 | l3_memory：实体文本（写前） | passage |
| 9 | l3_memory：向量修复 | passage |
| 10 | l3_memory：实体编辑文本（内联快路径） | passage |
| 11 | topic_detector：当前主题与候选主题两侧 | passage（两侧同角色） |

### 3. 语义与边界

- 非对称实现覆写 `embed_query` / `embed_passage`（及内联变体）；对称
  实现只覆写 `embed` / `embed_inline`——两种实现都零成本兼容；
- 主题检测是"同域比较"，两侧同用 passage，不构成检索方向，写入文档；
- 管理编辑的内联刷新失败不属本 ADR 的端口语义：由框架 `Failed` 事实
  暴露（0.8.0 实施记录）。

## Alternatives Considered（备选与否决）

1. `embed(text, role)` 单方法带角色参数——否决：破坏现有实现者；调用点
   可读性下降；当前无第三种角色；
2. 只给异步路径加角色——否决：本地非对称模型的管理编辑内联刷新需要
   passage 语义；
3. 组件侧拼前缀——否决：把组件焊死到某一家模型约定；
4. 拆成两个 trait——否决：一个概念两个端口，实现者必须都实现。

## Consequences（后果）

**加性项**

- 4 个带默认实现的方法；现有实现与 `HashEmbedding` 零改动；对称模型
  行为不变。

**文档义务**

- 扩展指南补非对称模型指引、角色映射表与示例；trait rustdoc 更新；
  组件 README 提及角色。

**验证要求**

- 默认委托等价：只实现 `embed` 的实现，`embed_query` / `embed_passage`
  的结果与 `embed` 一致；
- 记录型实现断言 11 处调用点的角色与文本；
- 非对称实现示例可编译，且能观察到两侧编码差异。
