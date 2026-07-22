# MagicPaper (MP)

MagicPaper 是为 reMarkable Paper Pro Move 设计的纸面 AI 应用：用户直接用笔书写，墨迹在停笔后淡出，回答再以手写动画写回纸面。它没有键盘、聊天气泡或网页界面。

本项目由 Maxime Rivest 的 [`riddle`](https://github.com/MaximeRivest/riddle) 演进而来，并保留原项目历史和 MIT 署名。0.7.0 的正式运行方式是作为 Remagic Manager 托管的驻留应用；AppLoad、镇纸和旧独占脚本都不是其运行依赖。

## 与上游 riddle 的主要区别

| 方面 | 上游 | MagicPaper 0.7.0 |
|---|---|---|
| 设备与运行方式 | Paper Pro、AppLoad/独占模式 | Paper Pro Move，由 Remagic 提供 QTFB、笔/触摸与生命周期 |
| 定位 | Tom Riddle 日记 | 中文优先的纸面助手，简称 MP |
| OCR | 回答模型直接看整页 | 可提前 1 秒提交 PP-OCRv6，再由回答模型结合上下文纠错 |
| 回答 | 基础对话 | 计算直答、问答、长期对话、按需后台检索、纸面化整理，中文默认繁体 |
| 记忆 | 简短上下文 | 最近 20 轮、最多 400 页本地记忆及可删除历史 |
| 自动化 | 无 | 最多 9 个周期任务、TODO、智能心跳及后台 agent |
| 纸面 UI | 单一字体 | 固定方正屏显雅宋 UI；回答使用三种可切换、独立校准的手写体 |
| 阅读联动 | 无 | `read` 打开 KOReader 书库，`read 书名` 打开匹配书籍 |
| 性能 | 同步处理较多 | PNG、字形描边和回答排版移出 UI 线程；25 Hz 合并刷新，无等待圆点 |

## 系统边界

MagicPaper 只负责页面状态、笔迹解释、AI 请求、回答渲染和自己的持久数据。它不拥有物理面板、原始输入、前台切换、进程监督或系统恢复。

正式启动时，Remagic 必须提供：

- `REMAGIC_RUNTIME_PROFILE=qtfb_compat`；
- 稳定且唯一的 `QTFB_KEY` surface；
- v2 双向 lifecycle 通道；
- 经过 manifest 限定的 HOME/XDG、字体、证书和网络环境。

缺少任一托管契约时应用会在打开显示或输入前失败，不会退回到偷偷抢占设备的模式。`--legacy-takeover` 仍保留给明确的兼容实验，但不能在 Remagic 托管进程中启用。

```text
笔事件 ──► Remagic display host ──► QTFB surface ──► MagicPaper 输入状态机
                                                        │
                       停笔 1.0 s ─► 推测 OCR（可取消）  │
                       停笔 2.2/2.6 s ─► 提交当前回合   │
                                                        ▼
本地命令 ◄── 纠错文字 ◄── PP-OCRv6（可选） ──► 回答模型/记忆/搜索
    │                                                   │
    └──────── 本地页面                                  ▼
                                     离屏排版与字形描边 ─► 25 Hz 合并写回
```

任何新笔迹、前后台命令或关闭命令都会使旧回合失效；迟到的 OCR、网络流或后台排版结果不能覆盖新页面。进入后台前应用保存状态并报告 `state_saved`、`background_ready`；召回时沿用同一进程和页面；关闭时保存后报告 `shutdown_complete`。

## 纸面命令

| 写下 | 结果 |
|---|---|
| `任务`、`task` | 打开周期任务列表 |
| `任务 每五分钟讲一个黑暗冷笑话` | 新增周期任务 |
| `暂停任务 2` / `恢复任务 2` | 禁用或启用第 2 项 |
| `修改任务 2 每十分钟提醒喝水` | 修改第 2 项 |
| `删除任务 2` | 删除第 2 项 |
| `TODO 买牛奶` | 新增 TODO |
| `TODO` | 打开 TODO 列表 |
| `历史` | 打开最近对话历史 |
| `字体` | 打开字体与每字体字号校准页 |
| `设置`、`設定`、`settings` | 打开刷新、回答停留和字体设置页 |
| `帮助`、`help` 或大问号 | 打开内置说明 |
| `read` | 让 Remagic 打开 KOReader 书库 |
| `read 书名` | 打开唯一匹配书籍；歧义时显示候选列表 |
| `刷新`、`刷新屏幕`、`重新整理`、`refresh` | 执行一次完整刷新清除残影，不请求 API |

任务列表右侧方框在勾与叉之间切换启用状态。任务、TODO 或历史项目上划线可删除；划线可以从该行左右空白处起笔，但必须横穿文字。点击列表空白处退出。周期任务最多 9 个，最短间隔 5 分钟；TODO 不触发 API。智能心跳只在最近任务到期时请求回答，失败后按配置重试，不轮询消耗 API。

## 一次书写回合

1. 笔迹立即画入共享 surface；UI 线程只做必要的输入和轻量合成。
2. 停笔 1 秒后可推测性提交 OCR。继续书写会取消本地等待，并以新页面重新计时。
3. 完整、高置信输入在约 2.2 秒提交；模糊或未完成输入等待约 2.6 秒。
4. 14 段吸墨动画保留，每段 50 ms。等待时纸面保持空白，不显示闪烁圆点。
5. OCR 候选先按对话、任务和中文语境纠错；算式如 `122+456=?` 直接回答 `122+456=578`。
6. 普通知识问题直接作答；确需时模型可在后台检索，最终只输出整理后的纸面文字，不展示 URL、引用标记或搜索元数据。
7. 模型仍以流式协议接收，但回答文字先在内存中汇合；流结束后一次测量整段宽高，在安全内容区内同时做水平与垂直居中，再按所选字体生成描边，以最多 25 Hz 的节奏写回。这样首笔需要等待完整回答，却不会因后续文字到达而上下漂移或只做到左右居中。

## 字体、记忆和数据

固定界面文字使用 `FZPingXianYaSong.ttf`（方正屏显雅宋），不受手写字体选择或字号校准影响。回答文字内置辰宇落雁体；部署包另含黄油拾叁体与 851 远星夜行手写体，851 是默认选择。写下 `字体` 可直接进入字体页；写下 `设置` 后也可从总设置页进入。三种回答字体均可在 50%–180% 范围校准视觉大小，字体页只有专门的回答预览样例使用对应手写体。缺字由 `CoverageFallback.ttf` 中性完整字库逐字补齐。

设置页默认采用增强局部清理、16 px 清理边距和每 3 次回答一次全刷。局刷强度、边距、自动全刷间隔以及回答停留比例均即时生效并保存在 `preferences/settings.json`；API 密钥、模型和 URL 仍由外部 `oracle.env` 管理。

默认持久数据位于：

```text
/home/root/riddle-data/
├── memories/       对话、转写和原始笔迹
├── tasks/          周期任务
├── todos/          TODO
├── preferences/    刷新、回答停留、字体及每字体字号
└── agent/          前后台回答交接队列
```

配置默认位于 `/home/root/.config/riddle/oracle.env`。安装、升级和自动化测试不得覆盖真实记忆、任务、TODO、字体配置或 API 配置。

## OCR 与回答后端

复制示例配置并只在设备上填写密钥：

```sh
install -m 600 oracle.env.example /home/root/.config/riddle/oracle.env
```

OpenAI-compatible HTTP 后端的核心变量：

```sh
RIDDLE_OPENAI_KEY=...
RIDDLE_OPENAI_BASE=https://example.com/v1
RIDDLE_OPENAI_MODEL=your-model
RIDDLE_OPENAI_API=responses          # 或 chat_completions
RIDDLE_OPENAI_REASONING=low
RIDDLE_WEB_SEARCH=auto
RIDDLE_OPENAI_MAX_TOKENS=2000
```

可选 PaddleOCR：

```sh
RIDDLE_OCR_TOKEN=...
RIDDLE_OCR_URL=https://paddleocr.aistudio-app.com/api/v2/ocr/jobs
RIDDLE_OCR_MODEL=PP-OCRv6
RIDDLE_OCR_POLL_MS=250
RIDDLE_OCR_TIMEOUT_SECONDS=60
```

`RIDDLE_OCR_SPECULATIVE=off` 可关闭一秒预请求，避免停顿后继续书写造成已计费但弃用的远端任务。没有 HTTP 密钥时也可使用常驻 `pi --mode rpc` 后端；完整变量和注释见 `oracle.env.example`。

密钥不得提交到 Git。若密钥曾出现在终端日志、聊天或仓库历史中，应立即在提供商控制台撤销并重建。

无屏诊断：

```sh
riddle --ocr-test handwriting.png
riddle --oracle-test handwriting.png
```

## 确定性测试模式

设置精确值 `RIDDLE_TEST_MODE=1` 后，MagicPaper 使用确定性离线回答，并拒绝 HTTP、PaddleOCR、pi 和外部阅读器调用。`RIDDLE_DATA_DIR` 可把所有持久状态重定向到临时目录；也可按组件覆盖：

- `RIDDLE_AGENT_QUEUE_DIR`
- `RIDDLE_PI_DATA_DIR`、`RIDDLE_PI_HOME`
- `RIDDLE_MEMORY_DIR`
- `RIDDLE_TASKS_DIR`
- `RIDDLE_TODOS_DIR`
- `RIDDLE_PREFERENCES_DIR`
- `RIDDLE_REMARKABLE_LIBRARY`、`RIDDLE_KOREADER_LIBRARY`

测试模式在没有显式路径时也不会回退到 `/home/root`。Remagic 的设备验收通过临时 manifest 和 systemd runtime drop-in 使用生产二进制、生产显示栈和隔离数据，测试结束后比较真实数据指纹并恢复原会话。

## 构建与交付

提交前运行完整本地门禁：

```sh
./scripts/check.sh
```

它会执行架构检查、格式检查、全部 target 测试、Clippy `-D warnings` 和 release/all-features 编译检查。

正式设备包由同级 `remagic-manager` 统一构建和部署，它负责交叉编译、字体资源、QTFB shim、manifest、systemd 服务、校验和与事务安装：

```sh
cd ../remagic-manager
./scripts/build-bundle.sh
./scripts/deploy-usb.sh
```

旧独占构建仍可通过 `build-takeover.sh` 和 `scripts/make-bundle.sh` 生成；兼容包会校验并打包 `${MAGICPAPER_UI_FONT:-$HOME/Downloads/方正屏显雅宋.TTF}`，但它只作为显式兼容路径，不参与 Remagic 的应用切换、驻留、故障恢复和自动化验收。

## 模块划分

```text
src/app/          回合编排、生命周期、输入优先级、列表与回答控制
src/oracle/       HTTP/pi/Paddle、流解析、本地路由、提示词和确定性后端
src/storage/      记忆、任务与 TODO 领域模型
src/appearance/   字体、标定与手写描边
src/ui/           纸面列表、帮助和字体设置
src/qtfb/         共享 surface、输入与非阻塞提交适配
src/platform.rs   与设备无关的 token、refresh intent 等平台契约
```

默认生产文件以 400 行为目标、500 行为门禁，函数以 60/100 行为目标/门禁；测试文件默认上限 800 行。这些数字是审查预算，不是机械拆分规则。职责高度内聚、拆分会降低可读性的文件可在 `architecture-exceptions.tsv` 中登记精确路径、独立上限和理由，禁止通配符或整目录豁免。详见 `docs/ARCHITECTURE_STANDARDS.md`。
