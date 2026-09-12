# Synonz Chat TUI 示例实现计划

- 状态: VERIFIED（2026-09-12，M1-M4 完成；189/189 全绿、clippy/doc
  零警告、fmt 干净；实际运行反馈待 irylex）
- 日期: 2026-09-12
- 依据: ADR-0005（工具契约）、ADR-0006 / ADR-0015（model 契约与
  参数归位）、ADR-0017（事件总线）、0.4.0 实施计划 M27（适配器运行
  时可调选项）
- 性质: 开发文档——`examples/chat_tui` 示例的目标、交互规格、结构与
  验收基准；**不引入框架契约变更**（仅使用公开 API），示例不随
  crate 发布（`examples` 为 `publish = false`）
- 目的: 用真实可聊天的 TUI 程序验证公开 API 对真实开发场景的支撑，
  并作为开发者扩展工具 / 接入模型 / 协作事件流的可复制范例

---

## 1. 目标与验收

### 1.1 验证目的（本示例存在的理由）

- **公开 API 支撑真实场景**：多轮会话、流式增量、工具调用与结果、
  取消、运行期调参（`set_model` / `set_options` 零重建）、显式收尾；
- **开发者范例**：只读原语工具的 `#[derive(Tool)]` 扩展方式、
  OpenAI-compatible 接入、TUI 事件循环与 `Execution` 流的协作方式。

### 1.2 功能验收

1. **启动向导**：①API 配置（base_url / api_key，env 预填、key 掩码）
   → ②模型与思考档位选择 → 进入聊天；
2. **聊天**：多轮对话（会话保留）、助手流式渲染、工具调用与结果
   展示、状态栏（模型 / 档位 / 运行状态）、输入行编辑与历史；
3. **运行期调参**：`/model <name>` 与 `/effort <level>` 立即对下一条
   消息生效（零重建——M27 能力的真实消费者）；
4. **取消**：`Esc` 取消当前轮（`Execution::cancel`），界面回到可输入；
5. **退出**：`Ctrl+C`；终端状态（raw mode / alternate screen / 光标）
   在正常退出与 panic 时都恢复；
6. **工具**：三个只读原语（`read_file` / `list_dir` / `file_info`），
   沙箱在启动目录，越界拒绝、大小 / 数量有上限。

### 1.3 非目标

- 配置不落盘（会话内存 + env 预填；"保存设置"另议）；
- 不支持 Anthropic（仅 OpenAI-compatible；Anthropic 运行期调参由
  M27 单元测试覆盖，不进本示例）；
- 不做多会话管理 / 历史持久化 / 附件 / Markdown 渲染 / 鼠标交互。

## 2. 公开 API 使用面（验证清单）

| 场景 | 公开 API |
|---|---|
| 运行环境 | `SynonzRuntime::builder().build()`；退出 `runtime.shutdown().await` |
| Agent | `Agent::builder().runtime(..).model(..).system_prompt(..).tool(..).build()` |
| 模型接入 | `synonz_openai::Client::new(base_url, key, model)`；`options()` / `set_options()` / `set_model()` |
| 多轮会话 | `Conversation::new(&runtime, &subject)`；`conv.turn_input(&mut self)` |
| 流式消费 | `Execution::next()` → `ExecutionEvent::{Delta, ToolRequested, ToolCompleted, Completed, Failed, Cancelled}` |
| 取消 | `Execution::cancel()`（drop 亦为 RAII 取消） |
| 工具扩展 | `#[derive(Tool, Deserialize, JsonSchema)]` + `async fn run(&self)` |
| 记忆 / 主题 | 默认 `DefaultContext`（引擎在后台维护；本示例不直接触碰） |

## 3. 架构

### 3.1 事件循环与借用结构

`Execution<'a>` 经 `TurnInput` 借用 `&Agent` 与 `&mut Conversation`，
不能与二者同存于一个 App 结构（自引用）；采用内外双层循环：

```
外层循环（向导 / 空闲输入）
  select! { crossterm EventStream, 定时重绘 }
    └─ 发送消息：agent.run(conv.turn_input(text)) → 进入内层
内层循环（运行中）
  select! { crossterm EventStream, execution.next() }
    ├─ 事件 → 更新 transcript / 状态
    └─ 终态事件（Completed / Failed / Cancelled）→ 回到外层
```

- 运行中 `Esc` → `execution.cancel()`，继续消费至终态后退出内层；
- 输入事件与模型事件共用一棵状态更新路径（`apply_event`）。

### 3.2 状态机

```
Setup(Api) --Enter--> Setup(Model) --Enter--> Chat(Idle)
Chat(Idle) --输入+Enter--> Chat(Running) --终态--> Chat(Idle)
Chat(Running) --Esc--> Chat(Idle)（取消后）
任意状态 --Ctrl+C--> 退出（终端恢复）
```

### 3.3 文件布局

```
examples/chat_tui/
  main.rs    终端生命周期（RAII + panic hook）、事件循环、借用编排
  setup.rs   向导状态与校验、env 预填、模型与档位选择
  app.rs     会话状态：transcript、输入行编辑、滚动、运行状态
  ui.rs      ratatui 渲染（向导 / 聊天 / 状态栏 / 错误弹层）
  tools.rs   只读原语工具（#[derive(Tool)]）与其单元测试
```

`examples/Cargo.toml` 注册 `[[bin]] name = "chat_tui"`。

### 3.4 终端生命周期

- 进入：`enable_raw_mode` + `EnterAlternateScreen` + 隐藏光标；
- 恢复：RAII guard（Drop）+ panic hook（两者都恢复终端）；
- resize 事件触发重绘；无 TTY（管道 / CI）时给出清晰错误退出。

## 4. 交互规格

### 4.1 向导

- 字段与默认：
  - Base URL：默认 `https://api.openai.com/v1`（env `SYNONZ_OPENAI_BASE_URL`）；
  - API Key：掩码显示（仅尾部 4 位），env `SYNONZ_OPENAI_API_KEY` 预填；
  - Model：env `SYNONZ_OPENAI_MODEL` 或内置预填（如 `gpt-4o-mini`）；
  - Thinking：`Off / Minimal / Low / Medium / High / ExtraHigh / Max`
    （映射 `ReasoningEffort`，默认 `Off`）；
- 校验：URL 非空且形如 `http(s)://`；Key 非空；Model 非空；错误内联
  展示、不阻塞重试；
- 按键：`Tab` / `Shift+Tab` 切换字段，`←/→`（或 `空格`）选择档位，
  `Enter` 确认，`Esc` 退出。

### 4.2 聊天

- transcript 条目：User / Assistant（流式拼接）/ Tool（调用名 + 参数
  摘要 → 结果状态）/ Error（终态或软失败）；
- 输入行：字符编辑（`←/→/Home/End/Backspace/Delete`）、`↑/↓` 召回
  历史输入；
- 快捷键：`Enter` 发送（运行中禁用并提示）、`Esc` 取消、`Ctrl+C`
  退出、`PgUp/PgDn/Home/End` 滚动；运行中自动滚底，用户上滚时暂停
  跟随（新输出出现时标记"有新内容"）；
- 命令（输入行以 `/` 开头）：
  - `/model <name>`：`client.set_model(name)`；
  - `/effort <level>`：`client.options()` 读改写后 `set_options`；
  - 非法参数内联报错，不发送给模型。

### 4.3 工具与安全

| 工具 | 参数 | 行为 | 上限 |
|---|---|---|---|
| `read_file` | `path` | UTF-8 文本读取（非 UTF-8 报错） | 64 KiB 截断并标注 |
| `list_dir` | `path` | 目录项（类型 / 名称，排序） | 200 项截断 |
| `file_info` | `path` | JSON：大小 / 是否目录 / 修改时间（Unix 秒） | — |

- 沙箱：启动目录 `canonicalize` 为根；解析后的目标路径必须在根内
  （含符号链接处理后复核），越界返回 `ToolResult::Err`（模型可读）；
- 工具失败一律软失败（`ToolResult::Err`），不中断 run。

## 5. 依赖

- 新增（仅 examples crate，进 workspace catalog）：
  `ratatui 0.30`、`crossterm 0.29`（feature `event-stream`）；版本以
  cargo 解析统一为准（ratatui 的 crossterm 复用）。
- 复用：`synonz`（features `test-util`）、`synonz-openai`、`tokio`
  （`full`）、`futures`、`serde` / `serde_json`。

## 6. 里程碑

| # | 里程碑 | 内容 | 完成定义 |
|---|---|---|---|
| M1 | 骨架与向导 | 终端 RAII + 事件循环骨架 + 向导（校验 / 预填）+ 聊天静态布局 | 编译 + clippy 零警告 + setup 校验单测绿 |
| M2 | 流式聊天 | 多轮会话 + Delta 渲染 + 工具事件展示 + 状态栏 + 取消 + 历史 | 装配测试（MockModel）绿；手工冒烟可聊 |
| M3 | 工具 | 三只读原语 + 沙箱校验 + 事件展示完善 | 工具单测（越界 / 截断 / 形状）绿 |
| M4 | 运行期调参与收尾 | `/model`、`/effort`；错误路径（模型失败 / 断流）；README；全量回归 | 全量回归绿 + README 更新 |

每波完成定义：`cargo fmt --check` / `clippy --workspace
--all-targets --all-features` 零警告 / 相关测试绿；M4 后跑全量。

## 7. 测试策略

- **工具单测**（`tools.rs`）：越界路径拒绝、大小 / 数量截断、结果
  形状（JSON / 文本）；不依赖终端与网络；
- **向导单测**（`setup.rs`）：URL / Key / Model 校验与档位映射；
- **装配测试**：`MockModel`（`test-util`）+ 真实 `Conversation` 跑一轮，
  断言 transcript 组装与终态（不依赖 TUI 渲染）；
- **手工验证**（irylex 本机，带 key）：向导 → 对话 → 工具调用 →
  `/model` / `/effort` 生效 → `Esc` 取消 → `Ctrl+C` 退出后终端恢复；
- TUI 渲染与按键不做自动化（成本高、价值低）；不引入 flaky。

## 8. 风险与边界

- **借用编排**：`Execution` 借用会话 → 双层循环（3.1）；若后续需要
  更大重构（如多会话），另立设计；
- **事件与按键竞态**：统一终态收口（终态事件后必须退出内层），
  取消后继续消费直到终态，避免流悬挂；
- **无 TTY 环境**：CI 只保证编译 / 单测；TUI 实跑靠手工验证；
- **沙箱边界**：符号链接可能指向根外——canonicalize 后复核；不承诺
  防御恶意并发文件系统变更（示例级）；
- **流式渲染性能**：按条目缓存 + 有界重绘节流，避免每 delta 全量渲染。

## 9. 验证与文档

- 验证：`cargo fmt --check`、`cargo clippy --workspace --all-targets
  --all-features`、`cargo test --workspace --all-features`、`cargo doc
  --workspace --no-deps` 零警告；
- 文档：README（根 + 五份 crate 副本，保持同步）示例表新增 `chat_tui`
  行（标注"交互式 TUI；首次启动向导配置 endpoint"）；
- 发布面：示例不随 crate 发布，无 CHANGELOG 条目。

## 10. 里程碑完成记录（2026-09-12）

- **M1 骨架与向导**：终端 RAII（Drop 恢复 + panic hook）+ `EventStream`
  事件循环 + 向导（env 预填、URL / Key / Model 校验、key 尾 4 位掩码、
  档位选择）；`setup.rs` 校验单测 2 项；
- **M2 流式聊天**：`app.rs`（`TextInput` 字符安全编辑、transcript、
  历史召回、`apply_event` 终态收口）；双层循环借用编排（外层空闲 /
  向导，内层运行 × `execution.next()`）；`MockModel` 装配测试
  （真实会话一轮 → transcript 与终态断言）；
- **M3 工具**：`tools.rs` 三只读原语（`read_file` 64 KiB 截断 /
  `list_dir` 200 项上限 / `file_info` JSON）；cwd 沙箱 canonicalize
  越界拒绝；单测 5 项（越界、截断、非 UTF-8、排序与上限、元数据）；
  工具调用与结果入 transcript；
- **M4 运行期调参与收尾**：`/model` / `/effort`（读改写 `set_options`，
  零重建，下一条消息生效）；取消（Esc）、退出（Ctrl+C）、resize、
  错误路径（模型失败 / 流断开）；README（根 + 5 份 crate 副本）示例表
  同步；
- **与计划的偏差（B 类，实施记录）**：
  1. 向导档位首项用 **`Default`（不发送参数）** 而非计划中的 `Off`
     ——`Off` 是显式 `"none"`、`Default` 是省略，语义不同；档位表为
     `Default/Off/Minimal/Low/Medium/High/ExtraHigh/Max`；
  2. 额外提供 `/quit`、`/exit` 便利命令；
  3. `ChatState` 持 `model` / `effort`（状态栏与命令共享），运行期
     切换在状态栏实时反映；
- **验证**：示例单测 16 项 + 装配测试 1 项；全量回归 **189/189** 全绿、
  clippy 零警告、`cargo doc --workspace` 零警告、fmt 干净；TUI 实跑由
  irylex 本机带 key 验证（无 TTY 环境不可自动化）。

## 11. 运行反馈迭代（2026-09-12）

irylex 首轮实跑反馈四项与处置：

1. **版本显示**：实跑输出的 `… 0.3.1` 是工作区 manifest 版本号
   （0.4.0 尚未 bump；发布检查单第一步才更新）——非缺陷，无需改动；
2. **光标不闪**：原因是实现隐藏了终端光标、改用反色假光标；改为
   **真实终端光标**（`Frame::set_cursor_position`，聊天输入行与向导
   聚焦字段均定位真实光标），是否闪烁交由终端自身配置；
3. **thinking 不显示**：新增核心 `ModelDelta::Reasoning`（narration
   only，不进 canonical 消息），OpenAI-compatible 适配器透传
   `reasoning_content` / `reasoning` / `reasoning_text`，Anthropic
   适配器映射 `thinking_delta`；TUI 新增 `Entry::Reasoning`（品红
   斜体）与 `/think [on|off]` 显示开关（默认显示，切换即时生效）；
4. **斜杠无提示**：输入 `/` 前缀时在输入框上方显示命令候选（用法 +
   一行描述），`Tab` 补全第一个匹配命令。

验证：示例测试 **20/20**（含 1 项 `MockModel` 装配）；适配器新增测试
（OpenAI reasoning 透传且不入 canonical 消息；Anthropic
`thinking_delta` → reasoning delta）；全量回归 **193/193** 全绿、
clippy 零警告、`cargo doc --workspace` 零警告、fmt 干净。

## 12. 第二轮运行反馈（2026-09-12）

irylex 第二轮实跑反馈与处置：

1. **模型列表弹窗**：新增 `synonz-openai::Client::list_models()`
   （标准 `GET /models`；排序 + 去重）；向导第二步在模型字段按 Enter
   拉取并弹出列表（输入过滤、↑↓ 选择、Enter 确认）；拉取失败内联
   提示，仍可手输模型；
2. **档位弹窗**：思考档位改为弹窗列表（Default / Minimal / Low /
   Medium / High / ExtraHigh / Max），←/→ 仍可快速循环；
3. **reasoning 开关**：新增独立 `Reasoning: On/Off` 行——Off 发送显式
   `"none"`，On 时选档位（Default = 省略参数）；新增 `Start` 行，
   Enter 开始聊天（与弹窗交互解耦）；
4. **reasoning 视觉区分**：思考内容品红斜体 + `thinking` 标签，正常
   回复绿色 + `assistant` 标签（默认显示，`/think` 可隐藏）。

**同时修复首轮遗留实现缺口**：`transform_sse`（真实流路径）此前未接入
`translate::ResponseAccumulator::apply_chunk`，首轮 reasoning 透传只改
了镜像函数（实际不生效）——现统一为单一解析路径（`apply_chunk`），并
补真实流路径集成测试（`SSE → reasoning/text/finish`）。

验证：示例测试 **23/23**；全量回归 **198/198** 全绿、clippy 零警告、
`cargo doc --workspace` 零警告、fmt 干净。

## 13. 第三轮运行反馈（2026-09-12）

irylex 第三轮实跑反馈与处置：

1. **输入光标位置偏移（中文场景）**：光标/滚动窗口/正文换行此前按
   「字符数」计算，CJK 双宽字符导致偏移；改为按**显示宽度**计算
   （示例依赖 `unicode-width`，与 ratatui 复用同一版本）：`input_window`
   与转录 `wrap` 均以显示列为准，补 CJK 光标/换行测试；
2. **工具调用空名报错**：根因两层——① `transform_sse` 旧实现从未解析
   `tool_calls`（工具调用在 OpenAI 适配器的流式路径上从未生效；本轮
   统一解析路径后才首次出现）；② 部分 provider 在后续分片重复发送
   `"name": ""`，覆盖了已累积的名字。修复：`id`/`name` 仅**首次非空**
   写入；参数为空串按 `{}` 处理；无名工具调用在 `into_message` 显式
   报错（不再静默执行 "unknown tool" 并把畸形历史回传 provider）；
   `finish_item` 改为 `Result` 以传播该错误；
3. 回归测试：空分片保名、空参数 `{}`、CJK 光标与换行；示例测试
   **24/24**。

验证：全量回归 **200/200** 全绿、clippy 零警告、`cargo doc --workspace`
零警告、fmt 干净。
