# PhotoPinFix 开发说明

用户下载和基本操作见 [README](../README.md)。本页保留源码构建、测试、打包和实现约定。命令均在项目根目录执行。

## 源码构建

源码构建需要 Rust 1.95 或更新的 stable、C / C++ 编译器、CMake 和相应构建工具；这些属于开发环境，用户运行编译产物时不需要安装。GUI 使用 eframe / egui 0.37，已包含在同一构建中。Linux 构建还需要窗口系统开发包；Debian / Ubuntu 可安装 `pkg-config libxkbcommon-dev libxkbcommon-x11-0 libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev`。Linux / macOS 先构建静态 HEIC 依赖：

```bash
bash scripts/build_native_heif.sh
source tools/native-heif/env.sh
PCRE2_SYS_STATIC=1 cargo build --release --locked
```

`Cargo.toml` 的 release profile 启用 ThinLTO、单个 codegen unit，并移除调试信息和符号。运行功能保持不变，实际文件大小以对应平台构建产物为准。

Windows 在 Visual Studio C++ 开发环境的 PowerShell 中运行：

```powershell
./scripts/build_native_heif.ps1
. ./tools/native-heif/env.ps1
$env:PCRE2_SYS_STATIC = '1'
cargo build --release --locked
```

元数据使用 `exiftool-rs` 的进程内引擎：由 Rust 实现的解释器执行其内嵌的 ExifTool 代码，保留相机 RAW 格式的兼容性；不调用系统 Perl，也不需要 `exiftool` 可执行文件。它并非重新手写一套 RAW 元数据解析器。第三方组件的版本和分发条件见 [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)；目前仓库尚未选定项目自身的发布许可证。

本机在完成构建和测试后可运行：

```bash
bash scripts/package_release.sh x86_64-unknown-linux-gnu target/release/photopinfix
```

输出位于 `target/artifacts/`，每个平台分成两个包，并各附 `.sha256`：

- `photopinfix-<target>-app.tar.gz`：可执行文件、运行说明、第三方许可，以及 `SOURCE.txt` 中对应源码包的文件名和 SHA-256。
- `photopinfix-<target>-source.tar.gz`：`source/` 中的项目源码和 vendored Rust 依赖、原生库源码与静态库、`RELINKING.md`、构建与依赖记录；不包含应用二进制。

普通用户只需 app 包。重新分发时应保持同批 source 包可获取，并保留许可和源码对应信息；具体要求见第三方说明及原始许可证。源码包根目录的 `native-dependencies.txt` 记录系统动态库依赖，`PROVENANCE.txt` 与 `SHA256SUMS` 用于核对构建和文件。中文字体及 OFL 许可位于 `assets/fonts/`，字体在编译时嵌入程序。打包脚本不发布到外部服务。

修改打包脚本后可运行 `python scripts/check_package.py`，离线核对两类包的内容边界、许可证和校验和。该检查使用模拟编译工具，不替代各平台的真实构建；Linux CI 也会运行它。

Linux / macOS 如需隐藏本机构建路径，在加载原生依赖环境后运行 `bash scripts/build_portable.sh`（依赖已缓存时可加 `--offline`）。脚本将 Rust 源码、依赖和构建目录映射为通用路径，移除调试符号，生成 `bin/photopinfix`。分享前可用 `python3 -B scripts/check_privacy.py .` 检查项目文件中的个人路径和常见凭据；它也检查被 Git 忽略的文件，但不会解包压缩文件或识别截图文字。Python 只用于这项开发检查。

## 本地演示工作区

如果本地工作区已经生成了 `bin/photopinfix` 和演示照片，也可以在项目目录中运行以下命令。这些生成文件不随 Git 仓库或下载包提供：

```bash
./bin/photopinfix ./demo/photos
./bin/photopinfix ./demo/raw
./bin/photopinfix --gui ./demo/photos
./bin/photopinfix --gui ./demo/raw
```

从源码运行需要先安装 Rust 和构建工具；原生依赖首次构建见上方说明。Rust 工具链不随项目目录分发。在保留了相邻本地工具链的工作区，也可先执行 `source ../rust/env.sh`，其他环境直接使用已安装的 `cargo`：

```bash
bash scripts/build_native_heif.sh
source ./tools/native-heif/env.sh
export PCRE2_SYS_STATIC=1
cargo run -- ./demo/photos
cargo run -- --gui ./demo/photos
```

真实相机 RAW 的演示目录：

```bash
cargo run -- ./demo/raw
```

## 测试与 CI

[demo/README.md](../demo/README.md) 解释 73 张普通照片的 9 个 session；`demo/expected.json` 逐张列出初始预期。`scripts/create_demo.py` 和 `scripts/create_raw_demo.py` 下载公开样本并生成模拟拍摄时间、GPS。真实 RAW 保留来源及 SHA-256，测试只改副本。重复生成默认保留现有数据，显式 `--reset` 可恢复演示初态。

```bash
source tools/native-heif/env.sh
export PCRE2_SYS_STATIC=1
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
PHOTOPINFIX_REQUIRE_DEMO=1 PHOTOPINFIX_REQUIRE_RAW=1 cargo test --locked -- --nocapture
```

内置的小型 HEIC 样本始终参与测试；独立进程清空 PATH、离开项目目录，验证程序无需外部工具。完整演示测试在未下载样本时会明确打印跳过；设置 REQUIRE 环境变量后缺数据直接失败。真实 RAW 写回测试额外使用开发环境中的原版 ExifTool 作独立核验，检查定位来源、原始图像数据、解码预览、拍摄参数及文件时间。样本生成和终端自动化脚本使用 Python，属于开发测试工具，不属于应用的运行依赖。

[原生 CI 工作流](../.github/workflows/ci.yml) 在每次 push 和 pull request 时运行格式检查、Clippy、单元测试、HEIC 预览及普通图片和 8 家相机厂商的 RAW 写回测试，并构建发布优化版。Linux / macOS 额外运行清空 PATH 的 TUI 键盘测试；Linux 还通过 Xvfb 验证真实 GUI 窗口和确认写入。平台如下，均使用 [GitHub 标准托管 runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)：

| 系统 | Runner | 产物架构 |
| --- | --- | --- |
| Linux | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` |
| Windows | `windows-2022` | `x86_64-pc-windows-msvc` |
| macOS · Apple Silicon | `macos-15` | `aarch64-apple-darwin` |

成功运行后，该次 Actions 的 **Artifacts** 提供各平台的 `photopinfix-<target>-app` 和 `photopinfix-<target>-source`，分别包含上文对应的 `.tar.gz` 和 `.sha256`，保留 7 天。工作流不创建 GitHub Release。手动 **Run workflow** 可分别关闭完整样本测试或产物上传；push / pull request 默认全部启用。Windows 跑原生构建、图片与 RAW 测试；macOS 额外跑 TUI 键盘测试；Linux 还跑真实 GUI 验收。平台兼容性以对应原生 CI 的实际结果为准。

创建时间只在操作系统和文件系统提供该值时进行精确比较；不把 Unix `ctime` 当作创建时间。本轮 Linux 原生 GUI 测试通过 `statx` 读取测试文件的 birthtime，已验证创建、访问、修改时间及 inode 均保持不变；这不替代 Windows / macOS 的原生验证。

Linux/macOS 还可运行真实终端键盘操作测试（需要 `pexpect`、`pyte`）：

```bash
python scripts/check_tui.py
PHOTOPINFIX_TEST_STANDALONE=1 python scripts/check_tui.py ./target/release/photopinfix
```

它在临时副本上检查 session、手动快捷键、度分秒编辑、设置、确认与取消、单张/批量写回、终端尺寸变化和读取中退出。终端记录保存在 `target/test-artifacts/`。地图的显式可视化验证命令为 `cargo run --example map_preview_check -- --online`；仅这个测试例子将对比图片输出到该目录，应用本身仍在内存中绘制地图。

Linux 的原生 GUI 验收使用 `python scripts/check_gui.py ./target/release/photopinfix`。它在 Xvfb 中启动真实窗口，把可执行文件复制到项目外、清空运行 PATH，在新生成的照片上执行预览、编辑、确认、单张与批量写入，并核验文件时间和备份。测试截图位于 `target/test-artifacts/gui-*.png`；环境准备见 [scripts/GUI_TESTING.md](../scripts/GUI_TESTING.md)。这些桌面测试工具不属于应用运行依赖。

更新 README 的两张实际窗口截图，可在本机构建完成后运行：

```bash
python3 scripts/check_gui.py bin/photopinfix --screenshots docs/images
```

只有验收成功才导出 `overview.png` 和 `gps-editor.png`。使用公开风景测试图，拍摄日期与 GPS 均为模拟值；默认无底图，不依赖地图网络服务。CI 将截图写到 `target/readme-screenshots` 并作为 Artifact 上传，不自动修改仓库文档。

## 界面与高级操作

不传目录时处理当前目录。可以在文件列表中进入子目录，不递归扫描。`--gui` 可以放在目录前或后；以 `-` 开头的目录放到 `--` 后面，例如 `photopinfix --gui -- -photos`。

GUI 需要本机图形桌面会话，以及系统提供的窗口和 OpenGL 支持；Linux 使用 X11 或 Wayland。在 Debian / Ubuntu 的 X11 环境中还需安装 `libxkbcommon-x11-0`，它不由 `libxkbcommon-dev` 自动提供。GUI 直接创建桌面窗口，不要求标准输入输出连接终端，不启动浏览器。纯 SSH / 无显示环境可以继续使用默认 TUI。图形窗口内的中文字体随程序嵌入，无需另装字体。

TUI 若未正确识别终端图像协议，使用 `--graphics halfblocks`；也可指定 `sixel`、`kitty` 或 `iterm2`。默认自动检测，无法使用图像协议时回退到 Unicode 半块字符。`--graphics` 只作用于 TUI，GUI 直接使用内存纹理显示图像。

左半屏是文件列表和定位详情；右上显示照片，右下显示地图。绿表示文件已有 GPS，黄表示有待写入的候选，红表示需要手动定位。已有 GPS 始终跳过，包括之前由 PhotoPinFix 写入的 GPS。

每张照片显示具体策略、完整拍摄日期时间和经纬度。保留文件中原有的小数秒位数和 UTC 偏移；没有记录时区时显示“时区未知”，不根据运行电脑猜测。设置页面可切换小数度 / 度分秒，列表与定位详情同步改变。

### 桌面 GUI

使用鼠标选择照片、切换 session / 全部范围、修改设置与编辑坐标。`Ctrl+L` 打开目录输入，`Ctrl+Q` 退出。GUI 的快捷键以界面提示为准，下面的完整按键表用于 TUI。

GUI 中 `Enter` 写当前照片，`Shift+Enter` / `W` 写当前范围。写入确认框默认取消，按 `Y` 或点击“确认写入”才执行；编辑框的 `Enter` 只暂存候选。坐标编辑支持 `F2` 切换小数 / 度分秒、`F3` 切换正负号 / 方位字母。关闭窗口、换目录或重扫前，如有未写入的手动候选，会询问是否丢弃。macOS 上目录与退出快捷键使用 Command。

GUI 与 TUI 写入后都保留其他已查看的候选，不会立即根据新增 GPS 自动重算；每批写入使用确认框中的候选快照。需要重新计算时，可按 `U` 恢复自动建议、应用设置或重扫目录。

地图默认无底图，可用按钮开启图片底图并选择来源。滚轮缩放，拖动平移，“适配全部点”恢复当前范围的全览；比例尺和来源署名与实际显示的地图一致。地图下载、照片解码和元数据读写在后台执行。编辑坐标只产生待写入候选，单张和批量写入仍需确认。

### TUI 完整快捷键

| 按键 | 功能 |
|---|---|
| ↑ / ↓、j / k | 移动光标 |
| ← / → | 上一个 / 下一个拍摄 session |
| a | 当前 session / 全部照片 |
| e | 编辑经纬度，接受小数或度分秒 |
| p / n / c | 使用前一张 / 后一张 / 时间最近的已有 GPS 照片 |
| i | 用前后参考照片按拍摄时间插值 |
| u | 撤销当前手动候选，恢复自动建议 |
| Enter | 确认写入当前照片 |
| Shift+Enter / W | 确认写入当前显示范围的全部候选 |
| s | 修改自动补全阈值、经纬度显示格式，仅本次运行有效 |
| m / b | 开关底图 / 切换地图源 |
| + / -、H / J / K / L、f | 地图缩放、平移、适配全部点 |
| l / Enter、Backspace | 打开目录、上级目录 |
| r / v / ? / q | 重新扫描、错误详情、帮助、退出 |

编辑框包含独立纬度、经度输入框、格式选择、方向表示、有效性提示和暂存 / 取消按钮。Tab / Shift+Tab 切换控件，←→ / Home / End 移动输入光标，Ctrl+U 清空，F2 转换小数度 / 度分秒，F3 转换正负号 / N、S、E、W。接受 `-33.5`、`33.5S`、`S33°30′00″` 等写法，拒绝正负号与方位矛盾的输入。格式转换保持坐标位置。

编辑框 Enter 只暂存坐标，回到主界面再次按 Enter 才出现写入确认。确认框默认取消，按 y 确认，Esc 或 n 取消。部分终端无法区分 Shift+Enter，使用 W 即可。

要写整个目录，先按 a 切到全部照片，再按 W；当前 session 视图下只写该 session。

## 自动补全规则

按 EXIF 拍摄时间排序，相邻照片间隔 **大于 1 小时**时划分 session；没有拍摄时间的照片单列，不拿文件修改时间替代。

默认仅允许相邻拍摄间隔 **严格小于 60 秒**的链传递参考，目标到每一侧参考照片的总时差 **不超过 600 秒**。每侧只看最近的已有 GPS 照片，不越过它；候选不会继续作为新参考延长这 600 秒。自动补全不跨 session。

两侧可用时按时间比例插值，一侧可用时直接复制，都不可用则标红。经度跨日期线时走较短方向。时间相同但位置不同的两侧参考无法确定插值，保留为红色。手动 p/n/c/i 不受自动阈值限制。

例如 09:00 和 09:15 两端有 GPS，中间每 30 秒一张：开头复制前方参考，中间在两端之间插值，末尾复制后方参考。600 秒分别针对目标到每一侧的距离。

## 元数据写入与文件时间

确认后写入标准 EXIF / XMP GPS，并写入自定义 XMP 命名空间 `urn:photopinfix:metadata:1.0/` 下两个字段：

| 字段 | 内容 |
|---|---|
| `PhotoPinFix:Mode` | `auto` 或 `manual` |
| `PhotoPinFix:Method` | `time-interpolation`、`copy-previous`、`copy-next`、`copy-nearest`、`coordinate-input` |

自动建议经确认写入仍为 auto；使用编辑框或手动快捷键则标 manual。不写工具版本或操作日期。自定义字段定义来自 `assets/photopinfix-exiftool.config`，编译时嵌入程序，运行时不需要查找此文件。

写入先在副本上完成并回读验证，保留 `<原文件名>.photopinfix-original` 备份，再通过原文件句柄写回，保持原文件身份及创建时间；恢复并校验修改时间、访问时间。扫描和预览同样恢复访问时间。发生错误时显示具体原因，写入失败会尝试从备份恢复。备份已存在时拒绝覆盖。

这里的创建时间是 birth/creation time。Unix 的 `ctime` 是 inode 状态变更时间，修改内容必然改变它，普通应用不能将它恢复；它不是创建时间。不支持 birthtime 的文件系统无法读回校验该值，但写回仍保留原 inode。断电、进程被强制结束和其他程序同时修改同一文件不在正常恢复流程内。保留原文件身份也意味着原地提交无法同时具备 rename 的原子替换语义。

## 地图与预览实现

默认无底图，不发地图网络请求。当前范围全部有效坐标显示为小点，选中照片为大点；包括待写入候选。切换 session 时适配全部点，移动光标时保持视野稳定。有无底图均显示公制比例尺（m / km），随纬度、缩放与窗口尺寸更新。

可切换 OpenStreetMap、CyclOSM、EOX 和 NASA 公开瓦片，也提供自动故障切换。每个服务有独立覆盖范围和访问条件；网络不可达时仍显示照片点。底图开启后会向相应服务请求当前视野瓦片。显示来源署名，遵循响应缓存指令；只下载当前视野，缓存限制在内存中。地图和照片预览均不生成临时图片文件。

可选环境变量：

| 变量 | 用途 |
|---|---|
| `PHOTOPINFIX_MAPTILER_KEY` | 启用自己的 MapTiler 账户 |
| `PHOTOPINFIX_STADIA_KEY` | 启用自己的 Stadia Maps 账户 |
| `PHOTOPINFIX_CARTO_KEY` | 启用自己的 CARTO 账户 |
| `PHOTOPINFIX_TILE_URL` | 自定义 XYZ 模板，含 `{z}`、`{x}`、`{y}` |
| `PHOTOPINFIX_TILE_ATTRIBUTION` | 自定义来源署名 |

RAW 预览优先读取相机嵌入的 JPEG，也支持 Leica M8 等 DNG 的第一 TIFF IFD 中已渲染的 RGB 缩略图；不进行完整 RAW 显影。没有可解码预览时显示原因，仍可处理元数据。HEIC / HEIF 使用静态链接的 libheif + libde265 在进程内解码，支持 8 / 10 / 12 位 HEVC 内容，不启动 Python 或外部解码程序。
