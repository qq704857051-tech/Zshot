<div align="center">

<img src="assets/logo/LOGO.png" width="120" alt="Zshot Logo">

# Zshot

**常驻托盘的 Windows 截图工具（Rust 版）**

标注编辑 · 本地 OCR · 自带 API 翻译 · 长截图 · 贴图钉住

[![Release](https://img.shields.io/github/v/release/Zyl0812/Zshot?style=flat-square)](../../releases)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](https://github.com/Zyl0812/Zshot?tab=MIT-1-ov-file)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6?style=flat-square&logo=windows)](../../releases)

[下载](../../releases) · [快速上手](#快速上手) · [功能](#功能) · [从源码构建](#从源码构建)

</div>

---

## 这是什么

按下热键 → 框选 → 标注 / 取字 / 翻译 / 接着往下滚 → 保存并复制 → 回到托盘。

全程不出现主窗口。没有启动器，没有欢迎页，没有图库。设置是从托盘单独打开的一个窗口，关掉它进程不退出。

本仓库是 Zshot 的 **Rust 重写版**：GPUI 界面、单 HWND 冻结覆盖层、DirectComposition 合成，目标是更小的体积与更快的冷启动。功能上对齐 C# 版的主干流程，尚未补齐的部分见[与 C# 版的差距](#与-c-版的差距)。

Zshot 最初基于 [Starshot](https://github.com/loliri/Starshot) 二次开发。

## 为什么

主流截图工具二选一：要么交互顺手但在 HDR 屏幕上发灰过曝，要么色彩正确却没有标注和取字。

- **HDR 屏幕上截图是对的。** 直接从 DXGI 拿 `R16G16B16A16Float` scRGB 帧缓冲，在 GPU 上做 tone mapping，不经过系统合成器的 8bit 压缩，高光不削顶。
- **取字不出机器。** OCR 用本地 PP-OCRv6 模型推理，截图不上传任何服务器。
- **翻译用你自己的 API。** 填自己的 Base URL 和 Key，不经过任何中间服务，密钥存 Windows 凭据管理器。

## 快速上手

装好后程序静默进托盘，直接按热键即可：

| 热键 | 动作 |
| --- | --- |
| `PrtSc` | 区域截图（最常用） |
| `Ctrl` + `Alt` + `W` | 全屏截图 |
| `Ctrl` + `Alt` + `A` | 区域截图，只进剪贴板不存文件 |

三个热键都能在「设置 → 快捷键」里点击方框重新录制。托盘**左键单击 = 区域截图**；右键菜单列出三个动作及当前绑定的热键，外加设置和退出。

框选完成后覆盖层不会关闭，直接在选区上标注、取字、翻译或继续长截图，按 `Enter` 或点「完成」结束。

默认保存 PNG 到 `图片\Zshot`，文件名带时间戳并自动防重名，保存目录可在设置里更改。

## 功能

### 区域截图覆盖层

冻结帧渲染，框选期间画面不会变。窗口自动探测（悬停高亮、可吸附）、15×15 像素放大镜、实时坐标、多显示器跨屏框选，独占全屏的游戏窗口也能抓。

`Esc` 或右键取消，`Enter` 确认。

### 标注编辑

在选区内直接画，不弹独立编辑窗：

矩形 · 箭头 · 画笔 · 文字 · 马赛克

支持撤销 / 重做 / 清空，颜色、线宽、马赛克格子的默认值都能在设置里改。

**工具栏可自定义**：17 个按钮都能在「设置 → 截图」里显示或隐藏，完成和取消始终显示。

### 本地 OCR

选区里的文字一键提取，识别结果可整段复制。

- 模型是 **PP-OCRv6 Small**，随发布包一起分发，装完即用，不需要联网下载
- 多语言单模型：简体中文、繁体中文、英文、日文及拉丁语系
- **全程本地推理，截图不上传**
- 首次调用加载模型约 160 ms，之后会话保留 60 秒，空闲超时自动释放，不长期占内存

### AI 翻译

OCR 取出文字后一键翻译，用你自己的 API：

- 兼容 OpenAI 格式的任意服务，填 Base URL / API Key / 模型名
- 目标语言、系统提示词、超时（5~300 秒）都可配
- API Key 存进 Windows 凭据管理器，不写进配置文件
- 对返回格式做了容错：流式 `delta`、缺字段都不会让程序崩

### 长截图

固定选区，手动滚动页面，自动检测新内容并拼接。

- 每 450 ms 采一帧，按重叠区域做灰度 SAD 匹配对齐，避免重复条带
- 上限 16384px（D3D11 纹理尺寸上限）
- 拼接期间选区点击穿透，下方的应用正常接收滚轮
- 完成时同时保存 PNG 并复制到剪贴板

### 贴图钉住

把标注后的选区作为无边框置顶窗口钉在屏幕上（PixPin 式）：拖动移动、滚轮缩放，`Esc` 或双击关闭，可同时钉多张。

### 更新检查

「设置 → 通用 → 关于」里可手动检查更新，对照 GitHub Releases 的最新版本；发现新版本后一键跳到发布页下载。

## 与 C# 版的差距

Rust 版仍在补齐以下 C# 版已有的能力：

- HDR 输出格式（AVIF / JPEG XL / PNGv3）：当前只输出 SDR PNG，HDR 屏幕下做正确的 tone mapping
- 椭圆 / 直线 / 序号标注（占位已留，尚未开放）
- 文件名模板、自动保存 / 自动复制的独立开关
- 应用内流式更新与差分补丁（当前为手动检查 + 浏览器下载）

## 已知限制

- OCR 典型选区约 1 秒出结果（CPU 推理的固有开销），识别中会显示状态提示
- 长截图需要手动滚动，不会自动滚屏；单次滚动超过选区高度的帧会被丢弃
- 翻译依赖你自己提供的 API，程序不内置任何翻译服务

## 系统要求

- Windows 10 / 11，x64
- HDR 捕获需要 HDR 显示器，SDR 屏幕自动走 8bit 路径

## 架构

单 crate，按职责分模块：

```text
crates/zshot/src/
├── capture/      WGC 优先、DXGI DuplicateOutput1 兜底的屏幕捕获
├── overlay/      GPUI 冻结覆盖层：框选、标注、工具栏、放大镜
├── settings/     设置窗口（GPUI）
├── platform/     托盘、全局热键、Win32 杂项
├── ocr/          PP-OCRv6 推理，60 秒空闲自动卸载
├── codec/        WIC PNG 编码
├── translate.rs  WinHTTP 直连 OpenAI 兼容接口
├── update.rs     GitHub Releases 更新检查
└── pin.rs / longcapture.rs / hdr.rs / exclusive.rs / perf.rs …
```

UI 用 vendored GPUI 0.2.2（`third_party/gpui`），本地补丁是 `patches/gpui-0.2.2-zshot-overlay.patch`，升级 GPUI 前必读 `patches/README.md`。

| 层 | 技术 |
| --- | --- |
| UI | GPUI 0.2.2（vendored） |
| 图形 | D3D11 + DirectComposition + Direct2D |
| 捕获 | Windows.Graphics.Capture / DXGI Output Duplication |
| 编码 | WIC（PNG） |
| OCR | oar-ocr（ONNX Runtime）+ PP-OCRv6 Small |
| 网络 | WinHTTP（翻译、更新检查） |
| 密钥 | Windows 凭据管理器 |

## 从源码构建

### 前置要求

- Rust stable（MSVC 工具链）
- Windows 10/11 SDK

### 步骤

```bash
cargo build --release    # 产出 target/release/Zshot.exe
cargo test --workspace   # 单元测试
cargo clippy --workspace --all-targets --locked -- -D warnings   # CI 同款检查
```

OCR 模型已入库（`models/v6/`），发布包会把它们放在 exe 旁边的 `models\v6\`。本地直接跑 `target\` 里的构建时，用 `ZSHOT_MODELS` 环境变量指向模型目录：

```text
set ZSHOT_MODELS=F:\own\Zshot-rust\models\v6
```

### 调试与性能 flags

| Flag | 作用 |
| --- | --- |
| `--once-capture [path]` | 抓当前显示器存 PNG 后退出 |
| `--self-test` | 初始化托盘与热键，打印结果后退出 |
| `--bench-overlay --mode first\|warm --n N` | 覆盖层 T2→T7 延迟采样 |
| `--stress-overlay N` | 覆盖层反复显隐，采样工作集内存 |

设置存 `%LOCALAPPDATA%\Zshot\settings.json`（每个键都可缺省），运行日志在同目录 `logs\`。

## 常见问题

**截图发灰 / 过曝？**
HDR 屏幕下捕获走 scRGB + GPU tone mapping，理论上不会发灰。若遇到请提 issue 并附显示器型号。

**OCR 第一次特别慢？**
首次调用要加载 ONNX 模型（约 160~400 ms），之后 60 秒内复用同一会话。稳定后典型选区约 1 秒。

**OCR 认不出某种语言？**
PP-OCRv6 Small 覆盖中日英与拉丁语系。韩文、阿拉伯文等不在支持范围内。

**翻译报错？**
先确认 Base URL 填的是兼容 OpenAI 格式的地址（通常以 `/v1` 结尾即可，程序会自动补 `/chat/completions`）。错误信息会直接显示 API 返回的状态码。

**长截图拼接错位？**
滚动慢一点。每次滚动的距离若超过选区高度，相邻两帧就没有重叠区域可供对齐，该帧会被丢弃。

**关掉设置窗口程序就退出了？**
不会。设置是独立窗口，关掉后程序继续留在托盘。要退出请用托盘右键菜单的「退出」。

## 致谢

- [Starshot](https://github.com/loliri/Starshot) — Zshot 最初的底座，作者 [@loliri](https://github.com/loliri)（MIT）
- [GPUI](https://github.com/zed-industries/zed) — Zed 编辑器的 GPU 加速 UI 框架
- [oar-ocr](https://github.com/GreatV/oar-ocr) — Rust OCR 推理封装
- [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) — PP-OCRv6 模型（Apache-2.0）
- [ONNX Runtime](https://github.com/microsoft/onnxruntime)（MIT）

## Code signing policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

### Roles

- Committer and Reviewer: [@Zyl0812](https://github.com/Zyl0812)
- Approver: [@Zyl0812](https://github.com/Zyl0812)

### Privacy

Zshot 不会把截图、OCR 结果、API 密钥或任何用户数据发送到用户未显式配置或请求的系统。网络访问只发生在两种情况：用户主动请求的翻译（发往用户自己配置的 API 端点）和针对 GitHub Releases 的更新检查。

Zshot does not transfer screenshots, OCR results, API keys, or other user data to systems not explicitly configured or requested by the user. Network access only occurs for user-requested translation through the configured API endpoint and update checks against GitHub Releases.

## 许可证

MIT，见 [LICENSE](LICENSE)。

Zshot 最初基于 Starshot 二次开发；本仓库是独立的 Rust 实现，不是 Starshot 官方版本。
