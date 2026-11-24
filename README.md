# PhotoPinFix

给缺失 GPS 的照片补上位置。PhotoPinFix 根据同批照片的拍摄时间和已有定位生成建议，你可以预览、调整，再确认写入。

支持 JPEG、PNG、HEIF / HEIC 和 Sony、Canon、Nikon、Leica 等相机的 RAW。同一个程序提供桌面 GUI 和终端 TUI，运行无需安装 Rust、Python、Perl 或 ExifTool。

![PhotoPinFix 主界面：照片列表、图片预览和位置地图](docs/images/overview.png)

截图中的拍摄时间和 GPS 为演示数据，不代表景物的真实位置。

## 下载与启动

登录 GitHub，打开 [Actions](https://github.com/Animadverto-mundum/PhotoPinFix/actions/workflows/ci.yml)，选择最近一次成功的运行，在 **Artifacts** 下载对应平台的 **app** 包：

| 平台 | 下载名称 |
| --- | --- |
| Windows x64 | `photopinfix-x86_64-pc-windows-msvc-app` |
| macOS Apple Silicon（M 系列） | `photopinfix-aarch64-apple-darwin-app` |
| Linux x64 | `photopinfix-x86_64-unknown-linux-gnu-app` |

先解开下载的 ZIP，再解开里面的 `.tar.gz`，进入 `photopinfix-<平台>-app` 文件夹。程序就在其中；不需要下载 **source** 包或编译源码。测试包保留 7 天，目前不自动发布 GitHub Release。

**Linux / macOS：**

```bash
./photopinfix --gui
```

**Windows PowerShell：**

```powershell
.\photopinfix.exe --gui
```

打开后点击 **选择照片文件夹**。也可以直接指定目录，例如 `./photopinfix --gui "照片目录"`。窗口启动成功后，终端会返回命令提示符；关闭终端也能继续使用 GUI。

去掉 `--gui` 就是终端界面；TUI 不传目录时打开当前目录。下载包不包含演示照片。

GUI 需要图形桌面。Linux 使用 X11 / Wayland；Debian / Ubuntu 的 X11 环境如果提示缺少 `libxkbcommon-x11.so.0`，安装 `libxkbcommon-x11-0` 即可。通过 SSH 使用时可选择 TUI，它会自动检测终端图片协议，不支持时使用字符预览。

## 使用方法

宽窗口中，左侧是照片库，中间预览图片，右侧编辑坐标和查看地图；窄窗口通过页签切换预览与地图。文件列表用颜色表示状态：

| 颜色 | 含义 |
| --- | --- |
| 🟢 绿色 | 已有 GPS，会保留并跳过 |
| 🟡 黄色 | 有待写入的位置建议，可以检查或调整 |
| 🔴 红色 | 暂时无法自动补全，需要手动选择位置 |

默认将拍摄间隔超过 1 小时的照片分为不同 session。自动补全沿相邻间隔小于 1 分钟的照片寻找参考，每侧最多追溯 10 分钟：两侧都有位置时按时间插值，只有一侧时复制该位置。阈值可在设置中调整，仅本次运行生效。

坐标可以直接编辑，支持小数、度分秒，以及正负号或 N / S / E / W。地图默认只显示照片位置，无需联网；需要时可开启底图。

![GPS 编辑框：独立纬度、经度输入及坐标格式选择](docs/images/gps-editor.png)

编辑只暂存建议。按 **Enter** 写入当前照片，**Shift+Enter**（或 **W**）写入当前 session / 全部视图中的候选；两者都会再次确认。写入前保留 `.photopinfix-original` 备份，正常完成时保持照片的创建、修改和访问时间。已有 GPS 不会被覆盖。

桌面界面可直接使用按钮。

## 终端界面

不加 `--gui` 即可在终端使用：

```bash
./photopinfix "照片目录"
```

![PhotoPinFix 真实终端界面：照片文件列表、Sixel 图片预览和位置地图](docs/images/tui.png)

终端会自动选择图片显示方式，定位策略和写入确认与桌面界面一致。

TUI 常用按键：

| 按键 | 功能 |
| --- | --- |
| ↑ / ↓ | 选择照片 |
| ← / →、a | 切换 session；切换显示全部 |
| e | 编辑 GPS |
| p / n / c / i | 复制前方 / 后方 / 最近位置，或按时间插值 |
| s、?、q | 设置、帮助、退出 |

源码构建、测试、完整快捷键和文件时间说明见 [开发文档](docs/DEVELOPMENT.md)。第三方组件见 [许可说明](THIRD_PARTY_NOTICES.md)。
