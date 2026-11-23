gradient.heic is an original synthetic 32x24 RGB gradient generated for
PhotoPinFix's native HEIC decoder regression test. It contains no real photo,
location, person, or camera metadata. Generated with Pillow/pillow-heif at HEVC
quality 85. No Python or image encoder is needed to run the test.

Pixel source: RGB(x * 8, y * 10, (x * 3 + y * 5) % 256),
for x in 0..32 and y in 0..24.

This generated fixture is dedicated to the public domain under CC0-1.0.
