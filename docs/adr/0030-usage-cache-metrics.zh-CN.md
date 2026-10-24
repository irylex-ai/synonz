# ADR-0030: 模型用量缓存指标——TokenUsage 的缓存读写字段与总输入归一

- 状态: APPROVED（2026-10-24，irylex 批准实施）
- 日期: 2026-10-24
- 决策者: irylex
- 性质: 核心公开数据模型 ADR——扩展 ADR-0003（事件词汇）与 ADR-0006
  （模型契约）中的 `TokenUsage`
- 关联: 上游消费需求 R2（Valkyrie 0.6.0）
- 版本承载: 0.8.0（字段加性；Anthropic `input_tokens` 数值修正为行为
  变更）

## Context（背景）

`TokenUsage` 只有 `input_tokens` / `output_tokens`（`#[non_exhaustive]` +
`Copy`）。各 provider 的缓存指标被丢弃：OpenAI 兼容响应的
`prompt_tokens_details.cached_tokens`、DeepSeek 原生
`prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`、Anthropic
`cache_read_input_tokens` / `cache_creation_input_tokens`。消费侧无法
展示缓存命中率或按缓存计费。

同时存在语义不一致：OpenAI/DeepSeek 的 `prompt_tokens` 已含缓存命中；
Anthropic 的 `input_tokens` **不含**缓存读/写（总输入 = input + read +
creation）——当前适配器只报 `input_tokens`，启用缓存时少计。

## Problem（问题）

在不破坏既有消费方的前提下暴露缓存读写指标，并让 `input_tokens` 在所有
provider 上含义一致。

## Decision（决策）

### 1. 字段与语义

```rust
pub struct TokenUsage {
    /// Total input tokens consumed (cache read and write included).
    pub input_tokens: u64,
    /// Tokens produced by the response.
    pub output_tokens: u64,
    /// Input tokens served from the provider's prompt cache.
    /// `None` = not reported; `Some(0)` = reported as zero.
    pub cache_read_input_tokens: Option<u64>,
    /// Input tokens written into the provider's prompt cache.
    pub cache_write_input_tokens: Option<u64>,
}
```

- serde：新增字段 `#[serde(default)]` 且 None 时不序列化——旧数据反序列
  化为 `None`；
- `new(input, output)` 不变；新增以字段名词命名的链式修改器（取
  `Option`），供适配器区分"未上报"与"上报为 0"。

### 2. `input_tokens` 归一为总输入

| Provider | `input_tokens` | `cache_read` | `cache_write` |
|---|---|---|---|
| OpenAI 兼容 | `prompt_tokens`（已含缓存，不变） | `prompt_tokens_details.cached_tokens` | 无 |
| DeepSeek 原生 | `prompt_tokens`（= hit + miss，不变） | `prompt_cache_hit_tokens`（details 缺失时回退；miss ≠ write） | 无 |
| Anthropic | `input_tokens + cache_read + cache_creation`（修正少计） | `cache_read_input_tokens` | `cache_creation_input_tokens` |
| 无缓存体系 | 原值 | `None` | `None` |

### 3. 累加语义（agent 逐轮汇总）

逐字段：两侧均 `None` 保持 `None`；否则求和（`None` 视为 0）。
`AgentOutput.usage` 与 `ModelEvent::Responded` 自动携带，无结构改动。

### 4. 适配器范围

两个适配器都只走流式（`Model` 只有 `stream`，`complete` 折叠流），不存在
"非流式响应"路径；解析点在流式 usage chunk / `message_start` /
`message_delta`。

## Alternatives Considered（备选与否决）

1. `input_tokens` 保持 provider 原值——否决：同一框架字段跨 provider 含义
   不同，Anthropic 继续少计；
2. 嵌套 `cache: Option<CacheUsage>`——否决：为一个可选细节引入新类型，
   `Copy` 语义与消费方取值复杂化；
3. 独立 `CacheUsage` 契约——否决：同一用量记录的拆分没有真实需求；
4. 归一后不再提供缓存字段——否决：无法计算节省/溢价。

## Consequences（后果）

**加性项**

- 两个可选字段与字段名词修改器；既有构造与序列化数据全部兼容。

**行为变更**

- Anthropic 启用缓存时 `input_tokens` 增大为总输入（release notes 显式
  说明）；OpenAI/DeepSeek 数值不变。

**文档义务**

- `TokenUsage` rustdoc 写明总输入语义与 `None` / `Some(0)` 的区别；
  扩展指南与 CHANGELOG 补 per-provider 映射。

**验证要求**

- 三种 provider 的缓存解析与缺失回退；
- 累加的 `None` 语义；
- serde 向后兼容；
- `ModelEvent::Responded` 与 `AgentOutput.usage` 携带缓存字段。
