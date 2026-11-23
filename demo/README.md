# PhotoPinFix 演示照片

这些照片用于实际打开 TUI、移动光标、检查自动建议以及确认写回。照片来自公开可下载的 Picsum 样例；**所有拍摄时间和 GPS 都是人为模拟，与照片实际景物的地点、日期无关**。

生成命令：

```bash
python3 scripts/create_demo.py
```

脚本需要 Python 3、Pillow、pillow-heif、可通过 PATH 找到的 ExifTool 和 Perl（也可在本地 `tools/exiftool/exiftool` 放置开发副本）；会优先使用本地 `tools/preview-deps` 中的 Python 依赖。它会下载 6 张不同样例照片，生成 73 张真实 JPEG、PNG、TIFF、HEIF 文件，并回读验证写入的元数据。生成过程不读取或修改你的其他照片。

```bash
./bin/photopinfix ./demo/photos
# 桌面界面：
./bin/photopinfix --gui ./demo/photos
```

重复运行生成脚本会保留已有演示照片，包括你通过 TUI 写入的改动。要明确丢弃演示改动并恢复初始状态：

```bash
python3 scripts/create_demo.py --reset
```

`--output /path/to/new-demo` 可以生成一套独立数据。脚本只会替换带有本工具生成标记的演示目录；不会替换任意已有照片目录。

## 场景

默认设置：相邻拍摄间隔严格小于 60 秒，参考到目标的总时差不超过 600 秒；相邻照片间隔大于 1 小时才划分 session。

| Session | 照片数 | 测试内容 |
|---|---:|---|
| 1 | 31 | 两端 09:00 和 09:15 有 GPS，中间每 30 秒一张；前段复制前图、中段时间插值、后段复制后图 |
| 2 | 11 | 相邻 59 秒可传播，恰好 60 秒和 61 秒断链；遇到新绿图使用新参考，不越过绿图 |
| 3 | 16 | 每 50 秒连续拍摄；距唯一参考 600 秒可补，601 秒及之后不可自动补，避免候选反复延长传播期限 |
| 4 | 2 | 相隔恰好 1 小时仍属同一 session；默认自动补全仍因 60 秒阈值而断开 |
| 5 | 1 | 与前一张相隔 1 小时 1 秒，开启新 session |
| 6 | 3 | 前后参考时间相同、位置不同，中间缺 GPS 照片应标红，避免无法定义的时间插值 |
| 7 | 3 | 经度从 179.9°E 到 179.9°W，中间应落在日期线附近，而不是本初子午线 |
| 8 | 3 | 开头两张缺 GPS，使用后图复制；已有 GPS 的后图带 `PhotoPinFix:Mode=auto` 和 `PhotoPinFix:Method=time-interpolation` |
| 未知时间 | 3 | 两张无拍摄时间且无 GPS 的红图，一张无拍摄时间但已有 GPS 的绿图；不使用文件时间替代拍摄时间 |

每张文件的初始 GPS、预期颜色、候选坐标、策略名称、session 和来源 ID 都记录在 `expected.json`。写回后实际状态会变化；该文件始终描述生成时的初始预期。

## 时间与格式

生成结束后，所有演示文件的访问时间和修改时间固定为 `2020-01-02T03:04:05Z`，明显区别于模拟拍摄时间。清单只保存这些模拟基准值，不保存开发机器的设备号、inode 编号或真实创建时间。Unix `ctime` 表示 inode 修改时间，不是文件创建时间，无法通过 `os.utime` 设置。

PNG、TIFF 和 HEIF 由下载的 JPEG 正常解码转换而成；HEIF 由 libheif 编码，文件扩展名与真实格式一致。普通照片数据放在 `photos/`；真实相机 RAW 是下面独立的 `raw/` 数据集。

原始样例下载 URL、重定向 URL、SHA-256 和相对源文件路径保存在 `sources/*.json` 以及 `expected.json` 中。请保留来源信息；这些演示照片不代表用户真实拍摄记录。

## 真实相机 RAW

```bash
python3 scripts/create_raw_demo.py
cargo run -- ./demo/raw
```

脚本从 [raw.pixls.us](https://raw.pixls.us/) 下载完整、相机原生的 RAW，并按官方目录中的 SHA-256 校验。所选九个样本均为 [CC0](https://creativecommons.org/publicdomain/zero/1.0/)；没有把 JPEG 改扩展名，也没有使用截断的元数据测试文件。初次下载约 58.36 MiB，原始文件保存在 `raw-sources/`，不会写入模拟元数据。

| 厂商 / 相机 | 格式 | 原生拍摄模式 | 约 MiB |
|---|---|---|---:|
| Sony ILCE-7S | ARW | 相机压缩、APS-C 裁切 | 5.88 |
| Canon EOS 40D | CR2 | 相机 sRAW | 5.54 |
| Canon EOS R6 | CR3 | 相机 C-RAW、1.6× 裁切 | 5.03 |
| Leica M8 | DNG | 相机 DNG | 10.09 |
| Nikon D1X | NEF | 相机压缩 RAW | 3.71 |
| Fujifilm FinePix S5000 | RAF | 相机 RAW | 6.53 |
| Panasonic DMC-LX7 | RW2 | 相机智能变焦、3:2 | 3.24 |
| Olympus E-3 | ORF | 相机 RAW | 9.22 |
| Pentax K10D | PEF | 相机压缩 RAW | 9.12 |

具体下载 URL、校验值、相机模式和许可在 [raw-sources/catalog.json](raw-sources/catalog.json)。官方目录没有逐张摄影师字段；如果原文件的 `Artist` 或 `OwnerName` 有值，生成后的清单会记录，未知作者不会编造。原有版权和 MakerNotes 不会被整体清空。

测试副本放在 `raw/`，共 14 张：九种 RAW 各有一张缺 GPS，另有一张 Sony RAW 和四张 JPEG 已带 GPS。所有拍摄时间、位置均为模拟，JPEG 锚点取自真实 RAW 的内嵌图像。

| Session | 模拟拍摄时间 | 文件 | 初始预期 |
|---|---|---|---|
| 1 | 09:00–09:02 | 两端 JPEG，中间 ARW / CR2 / CR3 | 2 绿、3 黄；根据时间插值 |
| 2 | 11:00–11:01:30 | DNG / NEF / RAF，最后 JPEG | 1 绿、3 黄；复制后方参考 |
| 3 | 13:00–13:02 | 开头 JPEG，中间 RW2 / ORF / PEF，最后 ARW | 2 绿、3 黄；根据时间插值 |

相邻照片间隔 30 秒，默认合计 5 绿、9 黄、0 红。`raw/manifest.json` 和 `raw/expected.json` 记录每张的预期坐标与时间基准。生成器会检查真实文件类型、模拟元数据回读和预览可解码性；Leica M8 的预览是第一 TIFF IFD 中的 320×240 RGB 图像，不是 JPEG 数据块。

重复运行不覆盖已有输出；显式传入 `--reset` 才恢复生成器识别的 RAW 演示目录。所有文件的访问与修改时间固定为前述 2020 年基准。真实写回测试使用这些文件的临时副本，保留这套初始演示状态。九个型号只能证明相应样本，不能代表同品牌所有 RAW 编码变体。

这批样本已在 Linux 完成 GPS 与来源字段写回、拍摄参数保持、预览像素一致及文件时间/身份检查。ExifTool 为其中八种格式提供主图像数据哈希，写入前后均一致；该 ORF 样本没有可用的 `ImageDataHash`，仍强制比较解码预览、原有拍摄字段和备份字节。Windows/macOS 原生测试入口见项目 README 与 CI。
