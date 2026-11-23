# Embedded Chinese UI font

`NotoSansSC-Regular.otf` is the unmodified Simplified Chinese regional subset of
Noto Sans CJK, release **Sans2.004**, distributed by Google/Adobe's Noto project.
It includes the region's character repertoire for general Chinese filenames;
it is not a subset cut down to PhotoPinFix's current UI labels.

- [Official format guide](https://github.com/notofonts/noto-cjk/blob/Sans2.004/Sans/README.md)
- [Original font](https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/SubsetOTF/SC/NotoSansSC-Regular.otf)
- [Original license](https://raw.githubusercontent.com/notofonts/noto-cjk/main/Sans/LICENSE)

The font remains licensed under the SIL Open Font License 1.1. `OFL.txt` contains
the original license; copyright and font naming information are also retained
inside the unmodified font. `SHA256SUMS` records the downloaded bytes. This license
does not change the license of the application's code. Include the font notice
and license with any package embedding this font.

Development fetch/setup: `bash scripts/prepare_gui_test.sh`. The font is embedded
in the executable; users do not need to install it or have system Chinese fonts.
