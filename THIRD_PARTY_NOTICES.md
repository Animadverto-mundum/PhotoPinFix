# Third-party components

PhotoPinFix embeds the following components. This notice records their terms;
it does not assign a license to PhotoPinFix's original application code.

| Component | Role | Upstream terms and source |
| --- | --- | --- |
| exiftool-rs 1.0.0 | In-process metadata reader/writer and its embedded ExifTool implementation | GPL-3.0-or-later; [upstream](https://github.com/Le-Syl21/exiftool-rs), `LICENSE` and source notices in the vendored crate |
| libheif | HEIF container and pixel decoding | LGPL v3 terms; [upstream](https://github.com/strukturag/libheif), complete LGPL/GPL texts in `COPYING` |
| libde265 | HEVC pixel decoding, linked statically | LGPL v3 terms; [upstream](https://github.com/strukturag/libde265), `COPYING` and `libde265/COPYING`; additional platform notices remain in source |
| libheif-rs and libheif-sys | Rust HEIF bindings | MIT; [upstream](https://github.com/Cykooz/libheif-rs), each crate's license files |
| PCRE2 and pcre2-sys | Regular expressions used by the metadata engine | PCRE2 BSD-style terms and Rust wrapper MIT terms; included in the vendored `pcre2-sys` source and license files |
| eframe / egui 0.37 | Native desktop window, controls and texture rendering | MIT OR Apache-2.0; [upstream](https://github.com/emilk/egui), complete notices in the vendored crates |
| Noto Sans SC Regular, Sans2.004 | Embedded Simplified Chinese GUI font | SIL Open Font License 1.1; [upstream](https://github.com/notofonts/noto-cjk/tree/Sans2.004/Sans), original font, `OFL.txt` and source information in `assets/fonts/` |
| Other Rust crates | Terminal UI, window systems, image formats, HTTP and support code | Exact versions, declared terms and source URLs are retained in `Cargo.lock` and the vendored crate directories |

The landscape photograph used in GUI tests and README screenshots is by
[Paul Jarvis](https://unsplash.com/photos/Cm7oKel-X2Q), obtained as Picsum image 11
under the [Unsplash License](https://unsplash.com/license). The source package
includes its provenance in `tests/fixtures/gui/SOURCES.md`. Screenshot capture
times and GPS locations are simulated and do not describe the photograph's
actual location. This photograph is a test/documentation asset, not embedded
in the application executable.

The Unix native build pins libheif 1.23.7 and libde265 1.1.3 using upstream archive
SHA-512 hashes. It applies one local compatibility patch that expresses two
defaulted C++20 equality operators as the same four scalar-member comparisons
for GCC 9. The original archives, modified source and separate patch are kept.
The Windows build pins vcpkg 2026.07.29; its port metadata and applied source
patches are copied into the source artifact and identify the exact native versions.
Only HEVC decoding is enabled; x265 encoding is not linked.

Static use of libheif and libde265 is covered by the LGPL. Its combined-work
terms include notices, the GNU GPL/LGPL texts and materials permitting a recipient
to modify the library and recombine or relink it with the application. The
paired source package therefore carries the application source, locked and
vendored Rust dependencies, native source and patches, static library files,
build settings and `RELINKING.md`. The app package carries notices and a
`SOURCE.txt` identifying the matching source archive and its SHA-256. Keep that
source package available with the corresponding app package when redistributing.
The complete license texts take precedence
over this description. See [LGPL section 4](https://www.gnu.org/licenses/lgpl-3.0.html#section4).

The GPL-3.0-or-later metadata dependency also affects distribution of a linked
application; keeping the application source private is not a substitute for
the GPL's corresponding-source requirements when conveying a covered binary.
No application license or public distribution decision is made by these scripts.
The workflow creates development/test artifacts only, with no release publishing
step or repository write permission. A public release must establish compatible
terms for the application and carry the required source and notices alongside
the binary. See the [GNU GPL v3](https://www.gnu.org/licenses/gpl-3.0.html).

Python, Perl, standalone ExifTool, CMake and vcpkg may be used to build dependencies
or prepare independent test fixtures. They are not runtime requirements of the
packaged PhotoPinFix executable. Standard operating-system libraries remain
runtime dependencies and are listed in the source artifact's `native-dependencies.txt`.
The GUI additionally uses the host's window system and OpenGL implementation;
Linux supports X11 and Wayland. No browser, WebView or separate GUI runtime is
bundled or launched. The Noto font and its OFL notices are included in the source
materials copied by the packaging script; the font bytes are embedded at build
time and are not downloaded by the application.
