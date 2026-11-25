"""Geometry checks on unmodified native GUI captures; standard library only.

Frames are (width, height, packed RGB bytes). These checks find painted widgets
and image bounds, rather than assuming a particular header height or OS frame.
They do not render, crop, recolor, or replace any saved screenshot.
"""

TAB_FILL = bytes((230, 241, 233))
PHOTO_BACKGROUND = bytes((28, 35, 32))
MAP_BACKGROUND = bytes((241, 246, 242))


def _color_components(frame, color, bounds, scale):
    width, height, pixels = frame
    left, top, right, bottom = bounds
    step = max(1, round(scale))
    remaining = {(x, y)
                 for y in range(max(0, top), min(height, bottom), step)
                 for x in range(max(0, left), min(width, right), step)
                 if pixels[(y * width + x) * 3:(y * width + x) * 3 + 3] == color}
    components = []
    while remaining:
        start = remaining.pop()
        pending = [start]
        x1 = x2 = start[0]
        y1 = y2 = start[1]
        count = 0
        while pending:
            x, y = pending.pop()
            x1, x2 = min(x1, x), max(x2, x)
            y1, y2 = min(y1, y), max(y2, y)
            count += 1
            for point in ((x - step, y), (x + step, y), (x, y - step), (x, y + step)):
                if point in remaining:
                    remaining.remove(point)
                    pending.append(point)
        components.append(((x1, y1, x2 + step - 1, y2 + step - 1), count * step * step))
    return components


def compact_selected_tab(frame, scale=1.0):
    """Find the sole selected compact tab, excluding narrow stats/sidebar rows."""
    width, height, _ = frame
    bounds = (max(round(170 * scale), round(width * 0.18)),
              round(48 * scale), round(width * 0.94), round(height * 0.76))
    candidates = []
    for box, area in _color_components(frame, TAB_FILL, bounds, scale):
        x1, y1, x2, y2 = box
        w, h = x2 - x1 + 1, y2 - y1 + 1
        if (x1 > bounds[0] and y1 > bounds[1] and x2 < bounds[2] - scale and y2 < bounds[3] - scale
                and 65 * scale <= w <= 110 * scale and 28 * scale <= h <= 44 * scale
                and area >= w * h * 0.65):
            candidates.append(box)
    if len(candidates) != 1:
        raise RuntimeError(f'Expected one visibly selected compact tab; found {len(candidates)}')
    return candidates[0]


def compact_tab_points(frame, scale=1.0):
    """Photo must initially be selected; return all three real mouse targets."""
    x1, y1, x2, y2 = compact_selected_tab(frame, scale)
    if (x1 + x2) / 2 > frame[0] * 0.48:
        raise RuntimeError('Compact photo tab is not the selected starting view')
    # Four-glyph labels share padding. Painted fill excludes the 1pt stroke;
    # the three adjacent controls have the UI's documented 10pt item spacing.
    stride = (x2 - x1 + 1) + 12 * scale
    points = [((x1 + x2) / 2 + index * stride, (y1 + y2) / 2) for index in range(3)]
    if points[-1][0] >= frame[0] - 12 * scale:
        raise RuntimeError('Compact tab group would extend outside the window')
    return points


def assert_selected_tab(frame, point, scale=1.0):
    x1, y1, x2, y2 = compact_selected_tab(frame, scale)
    if not (x1 <= point[0] <= x2 and y1 <= point[1] <= y2):
        raise RuntimeError('The clicked compact tab did not become visibly selected')


def assert_map_canvas(frame, photo_tab, scale=1.0):
    width, height, _ = frame
    bounds = (round(photo_tab[0] - 60 * scale), round(photo_tab[1] + 22 * scale),
              width, height)
    # Grid lines split the same canvas into several same-color components.
    components = [(box, area) for box, area in _color_components(frame, MAP_BACKGROUND, bounds, scale)
                  if area >= 100 * scale * scale]
    if not components:
        raise RuntimeError('The compact map did not render a visible map canvas')
    span_width = max(box[2] for box, _ in components) - min(box[0] for box, _ in components) + 1
    span_height = max(box[3] for box, _ in components) - min(box[1] for box, _ in components) + 1
    if (span_width < 300 * scale or span_height < 60 * scale
            or sum(area for _, area in components) < 20_000 * scale * scale):
        raise RuntimeError('The compact map did not render a visible map canvas')


def photo_geometry(frame, scale=1.0):
    """Measure actual photo pixels inside the dark preview canvas.

    Minimums accommodate a 330x220 landscape in the 880x640 layout, but reject
    a tiny thumbnail, text-only loading/error state, or a clipped preview.
    """
    width, height, pixels = frame
    components = _color_components(
        frame, PHOTO_BACKGROUND, (round(width * 0.18), round(48 * scale), width, height), scale)
    if not components:
        raise RuntimeError('No photo preview canvas is visible')
    box, _ = max(components, key=lambda item: (item[0][2] - item[0][0]) * (item[0][3] - item[0][1]))
    left, top, right, bottom = box
    canvas = ((right - left + 1) / scale, (bottom - top + 1) / scale)
    if canvas[0] < 400 or canvas[1] < 230:
        raise RuntimeError(f'Photo canvas is too small: {canvas[0]:.0f}x{canvas[1]:.0f} points')
    step = max(1, round(2 * scale))
    inset = max(2, round(3 * scale))
    xs, ys, colors = [], [], set()
    for y in range(top + inset, bottom - inset, step):
        for x in range(left + inset, right - inset, step):
            rgb = pixels[(y * width + x) * 3:(y * width + x) * 3 + 3]
            if max(abs(value - background) for value, background in zip(rgb, PHOTO_BACKGROUND)) > 12:
                xs.append(x)
                ys.append(y)
                colors.add(rgb)
    if not xs:
        raise RuntimeError('The photo canvas contains no image')
    photo = ((max(xs) - min(xs) + step) / scale, (max(ys) - min(ys) + step) / scale)
    coverage = len(xs) * step * step / ((max(xs) - min(xs) + step) * (max(ys) - min(ys) + step))
    if photo[0] < 300 or photo[1] < 190 or photo[0] * photo[1] < 60_000 or coverage < 0.70 or len(colors) < 200:
        raise RuntimeError(f'Actual photo is too small or incomplete: {photo[0]:.0f}x{photo[1]:.0f} points, '
                           f'coverage={coverage:.2f}, colors={len(colors)}')
    return {'canvas_points': [round(value, 1) for value in canvas],
            'photo_points': [round(value, 1) for value in photo],
            'photo_coverage': round(coverage, 3), 'sampled_colors': len(colors)}
