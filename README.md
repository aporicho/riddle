# MagicPaper

MagicPaper（简称 MP）是正式产品名，仓库与发布标识为 `magicpaper`。它是为 reMarkable Paper Pro 与 Paper Pro Move 设计的纸面 AI 应用：用户直接用笔书写，墨迹在停笔后淡出，回答再以手写动画写回纸面。它没有键盘、聊天气泡或网页界面。

本项目由 Maxime Rivest 的 [`riddle`](https://github.com/MaximeRivest/riddle) 演进而来，并保留原项目历史和 MIT 署名。0.8.1 的正式运行方式是作为 ReMagic 托管的驻留应用；AppLoad、镇纸和旧独占脚本都不是其运行依赖。

## 与上游 riddle 的主要区别

| 方面 | 上游 | MagicPaper 0.8.1 |
|---|---|---|
| 设备与运行方式 | Paper Pro、AppLoad/独占模式 | Paper Pro 与 Paper Pro Move，由 ReMagic 自动适配 QTFB、笔/触摸与生命周期 |
| 定位 | Tom Riddle 日记 | 中文优先的纸面助手，简称 MP |
| OCR | 回答模型直接看整页 | 可提前 1 秒提交 PP-OCRv6，再由回答模型结合上下文纠错 |
| 回答 | 基础对话 | 所有模型回答统一经 ReMagic 托管的 Pi Agent；计算直答、问答、长期对话及纸面化整理，中文默认繁体 |
| 记忆 | 简短上下文 | 最近 20 轮、最多 400 页本地记忆及可删除历史 |
| 自动化 | 无 | 最多 9 个周期任务、TODO、智能心跳及后台 agent |
| 纸面 UI | 单一字体 | 固定方正屏显雅宋 UI；回答使用三种可切换、独立校准的手写体 |
| 阅读联动 | 无 | `read` 打开 KOReader 书库，`read 书名` 打开匹配书籍 |
| 性能 | 同步处理较多 | PNG、字形描边和回答排版移出 UI 线程；25 Hz 合并刷新，无等待圆点 |

## 系统边界

MagicPaper 只负责页面状态、笔迹解释、AI 请求、回答渲染和自己的持久数据。它不拥有物理面板、原始输入、前台切换、进程监督或系统恢复。

正式启动时，ReMagic 必须提供：

- `REMAGIC_RUNTIME_PROFILE=qtfb_compat`；
- 稳定且唯一的 `QTFB_KEY` surface；
- 版本化的 `REMAGIC_DEVICE_PROFILE`；
- v2 双向 lifecycle 通道；
- 私有 `agent:pi-v1` socket、应用令牌与前后台独立身份；
- 经过 manifest 限定的 HOME/XDG、字体、证书和网络环境。

缺少任一托管契约时应用会在打开显示或输入前失败，不会退回到偷偷抢占设备的模式。`--legacy-takeover` 仍保留给明确的兼容实验，但不能在 ReMagic 托管进程中启用。

QTFB v1 的初始化回包只包含共享内存 key 与字节数，不包含逻辑尺寸或像素格式。因此 MagicPaper 不从内存大小、主机名或屏幕比例猜设备，而是严格验证 ReMagic 注入的 profile。当前契约如下；用户不需要也不能手工选择设备：

```json
{
  "schema_version": 1,
  "product": "paper_pro_move",
  "codename": "chiappa",
  "os_version": "3.27.0",
  "display": {
    "logical_width": 954,
    "logical_height": 1696,
    "qtfb_format": 6,
    "pixel_format": "rgb565",
    "stride": 1908
  },
  "capabilities": ["display:qtfb-v1", "input:pen-v1", "ink:direct-v1", "lifecycle:v2"]
}
```

Paper Pro 使用 `ferrari`、`1620×2160`、QTFB format 3、stride 3240；Paper Pro Move 使用 `chiappa`、`954×1696`、QTFB format 6、stride 1908。产品、代号、几何、格式或能力互相矛盾时启动会 fail closed。两款设备随后进入完全相同的共享 RGB565 surface 和低延迟笔迹提交路径；页面布局只读取实际 surface 尺寸。

```text
笔事件 ──► ReMagic display host ──► QTFB surface ──► MagicPaper 输入状态机
                                                        │
                       停笔 1.0 s ─► 推测 OCR（可取消）  │
                       停笔 2.2/2.6 s ─► 提交当前回合   │
                                                        ▼
本地命令 ◄── 纠错文字 ◄── PP-OCRv6（可选） ──► ReMagic Pi Agent
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
| `read` | 让 ReMagic 打开 KOReader 书库 |
| `read 书名` | 打开唯一匹配书籍；歧义时显示候选列表 |
| `刷新`、`刷新屏幕`、`重新整理`、`refresh` | 执行一次完整刷新清除残影，不请求 API |

任务列表右侧方框在勾与叉之间切换启用状态。任务、TODO 或历史项目上划线可删除；划线可以从该行左右空白处起笔，但必须横穿文字。点击列表空白处退出。周期任务最多 9 个，最短间隔 5 分钟；TODO 不触发 API。智能心跳只在最近任务到期时请求回答，失败后按配置重试，不轮询消耗 API。

## 一次书写回合

1. 笔迹立即画入共享 surface；UI 线程只做必要的输入和轻量合成。
2. 停笔 1 秒后可推测性提交 OCR。继续书写会取消本地等待，并以新页面重新计时。
3. 完整、高置信输入在约 2.2 秒提交；模糊或未完成输入等待约 2.6 秒。
4. 14 段吸墨动画保留，每段 50 ms。等待时纸面保持空白，不显示闪烁圆点。
5. OCR 候选先按对话、任务和中文语境纠错；算式如 `122+456=?` 直接回答 `122+456=578`。
6. 普通知识问题直接作答；Pi Agent 只得到 ReMagic 明确开放的安全工具，绝不继承 shell、任意文件读写或默认编码工具。最终回答由模型整理成纸面文字，不展示 URL、引用标记或搜索元数据。
7. 模型仍以流式协议接收，但回答文字先在内存中汇合；流结束后一次测量整段宽高，在安全内容区内同时做水平与垂直居中，再按所选字体生成描边，以最多 25 Hz 的节奏写回。这样首笔需要等待完整回答，却不会因后续文字到达而上下漂移或只做到左右居中。

## 字体、记忆和数据

固定界面文字使用 `FZPingXianYaSong.ttf`（方正屏显雅宋），不受手写字体选择或字号校准影响。回答文字内置辰宇落雁体；部署包另含黄油拾叁体与 851 远星夜行手写体，851 是默认选择。写下 `字体` 可直接进入字体页；写下 `设置` 后也可从总设置页进入。三种回答字体均可在 50%–180% 范围校准视觉大小，字体页只有专门的回答预览样例使用对应手写体。缺字由 `CoverageFallback.ttf` 中性完整字库逐字补齐。

设置页默认采用增强局部清理、16 px 清理边距和每 3 次回答一次全刷。局刷强度、边距、自动全刷间隔以及回答停留比例均即时生效并保存在 `preferences/settings.json`。Pi 智能体页保存供应商、Flash/Pro、思考等级与安全工具开关；默认是 DeepSeek、`deepseek-v4-flash`、关闭思考、开启安全工具。密钥不在设备屏幕上输入，由 ReMagic 单独保管。“新建会话”会同时重置驻留 Agent 并写入持久的本地对话边界；旧页面仍留在历史与召回目录中，但 ReMagic 日后重启 Pi 时不会再把它们自动灌入新上下文。

默认持久数据位于：

```text
/home/root/.local/share/magicpaper/
├── memories/       对话、转写和原始笔迹
├── tasks/          周期任务
├── todos/          TODO
├── preferences/    刷新、回答停留、字体、每字体字号及 Pi 非敏感偏好
└── agent/          前后台回答交接队列
```

PaddleOCR 配置默认位于 `/home/root/.config/magicpaper/oracle.env`；模型供应商密钥位于 ReMagic 的独立密钥目录，不进入 MagicPaper 数据区。安装、升级和自动化测试不得覆盖真实记忆、任务、TODO、字体配置、OCR 配置或供应商密钥。

## OCR 与 Pi Agent

MagicPaper 不再包含直连 OpenAI-compatible HTTP 或自行拉起 Pi 的生产后端。所有模型回合（包括心跳回答）都经过 ReMagic 的常驻 Pi RPC 进程；切换供应商、模型或思考等级会让 ReMagic 原子重载该应用的 Agent profile。交互回合优先于推测 OCR 和定时任务，断开连接也会取消其拥有的远端回合。

`oracle.env` 只保留 PaddleOCR 等 MagicPaper 输入侧配置：

```sh
install -m 600 oracle.env.example /home/root/.config/magicpaper/oracle.env
```

PaddleOCR 变量：

```sh
MAGICPAPER_OCR_TOKEN=...
MAGICPAPER_OCR_URL=https://paddleocr.aistudio-app.com/api/v2/ocr/jobs
MAGICPAPER_OCR_MODEL=PP-OCRv6
MAGICPAPER_OCR_POLL_MS=250
MAGICPAPER_OCR_TIMEOUT_SECONDS=60
```

`MAGICPAPER_OCR_SPECULATIVE=off` 可关闭一秒预请求，避免停顿后继续书写造成已计费但弃用的远端任务。完整变量和注释见 `oracle.env.example`。DeepSeek/OpenAI 等模型密钥使用 ReMagic 的电脑端配置命令写入权限为 `0600` 的供应商文件，不写进 `oracle.env`，也不会被传给 MagicPaper 进程。

密钥不得提交到 Git。若密钥曾出现在终端日志、聊天或仓库历史中，应立即在提供商控制台撤销并重建。

无屏诊断：

```sh
magicpaper --ocr-test handwriting.png
magicpaper --oracle-test handwriting.png
```

## 确定性测试模式

设置精确值 `MAGICPAPER_TEST_MODE=1` 后，MagicPaper 使用确定性离线回答，并拒绝 Pi Agent、PaddleOCR 和外部阅读器调用。`MAGICPAPER_DATA_DIR` 可把所有持久状态重定向到临时目录；也可按组件覆盖：

- `MAGICPAPER_AGENT_QUEUE_DIR`
- `MAGICPAPER_MEMORY_DIR`
- `MAGICPAPER_TASKS_DIR`
- `MAGICPAPER_TODOS_DIR`
- `MAGICPAPER_PREFERENCES_DIR`
- `MAGICPAPER_REMARKABLE_LIBRARY`、`MAGICPAPER_KOREADER_LIBRARY`

测试模式在没有显式路径时也不会回退到 `/home/root`。ReMagic 的设备验收通过临时 manifest 和托管 runtime override 使用生产二进制、生产显示栈和隔离数据，测试结束后比较真实数据指纹并恢复原会话。

## 安装、构建与交付

正式用户只通过 **ReMagic Store** 安装、更新、回滚或卸载 MagicPaper。MagicPaper 是独立应用包，不随 ReMagic 系统核心捆绑，也不要求用户安装 Rust、reMarkable SDK、AppLoad、镇纸或 Quill。Store 在安装前自动检查设备 profile、系统版本、ReMagic API 与所需能力。

提交前运行完整本地门禁：

```sh
./scripts/check.sh
```

它会执行架构检查、格式检查、全部 target 测试、Clippy `-D warnings` 和 release/all-features 编译检查。

仓库发布流程生成一个通用 aarch64 应用包；Ferrari 与 Chiappa 的设备差异由 ReMagic 的运行时 profile 和显示 host 解决，而不是发布两个由用户选择的 MagicPaper 安装包。应用包包含 MagicPaper 二进制、字体、manifest、配置模板、迁移器和托管后台 agent 声明，但不包含 ReMagic、设备显示库或用户 API 密钥。

发布维护者先设置 `RM_SDK` 并运行 `scripts/build-remagic.sh`。该脚本会覆盖设备 SDK 自带的 `-mcpu`，以通用 ARMv8-A 指令集编译，并拒绝 libc/libgcc 之外的设备专属动态库；因此同一应用二进制可由 Ferrari 与 Chiappa 的 ReMagic 运行环境承载。随后通过 `scripts/make-remagic-package.sh` 生成 Store bundle（构建机需要 Python 3、GNU tar、gzip 与 sha256sum）。四个字体路径必须显式传入 `MAGICPAPER_UI_FONT`、`MAGICPAPER_851_FONT`、`MAGICPAPER_BUTTER_FONT` 和 `MAGICPAPER_COVERAGE_FONT`；打包脚本不会从用户目录猜测资源，也不会读取 `oracle.env`。bundle 顶层包含 `bundle.json`、`manifest.toml` 和 `payload/`，版本化 payload 安装到 `/home/root/apps/magicpaper/releases/<content-id>`，Store 原子维护 `current` 链接。

`bundle.json` 为每个普通文件记录路径、四位八进制权限、大小和 SHA-256，并拒绝链接或特殊文件。`payload_sha256` 对按 UTF-8 路径排序的 payload 文件连续计算 `path\0mode\0size\0sha256\n`；`content_id` 在域分隔符 `remagic-bundle-content-v1\0`、应用标识/包名/版本之后，对同样格式的全部文件记录计算 SHA-256。`scripts/remagic-bundle.py verify` 在安装事务前复算整个清单。

旧独占构建仍可通过 `build-takeover.sh` 和 `scripts/make-bundle.sh` 生成。该目录包含显式的设备/Quill 假设，只作为实验兼容路径；它不属于 Store 包，不参与 ReMagic 的应用切换、驻留、故障恢复、双设备承诺或正式验收。

## 模块划分

```text
src/app/          回合编排、生命周期、输入优先级、列表与回答控制
src/device_profile.rs  ReMagic 注入的双设备显示契约与 fail-closed 校验
src/oracle/       ReMagic Pi Agent、PaddleOCR、流解析、本地路由、提示词与确定性后端
src/storage/      记忆、任务与 TODO 领域模型
src/appearance/   字体、标定与手写描边
src/ui/           纸面列表、帮助和字体设置
src/qtfb/         共享 surface、输入与非阻塞提交适配
src/platform.rs   与设备无关的 token、refresh intent 等平台契约
```

默认生产文件以 400 行为目标、500 行为门禁，函数以 60/100 行为目标/门禁；测试文件默认上限 800 行。这些数字是审查预算，不是机械拆分规则。职责高度内聚、拆分会降低可读性的文件可在 `architecture-exceptions.tsv` 中登记精确路径、独立上限和理由，禁止通配符或整目录豁免。详见 `docs/ARCHITECTURE_STANDARDS.md`。
