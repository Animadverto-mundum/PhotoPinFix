use crate::{
    app::{App, BrowserRow, Modal, editor_coordinates},
    input::TextInput,
};
use photopinfix::{
    coordinates::{CoordinateFormat, CoordinateNotation, format_coordinate, parse_coordinate},
    map,
    model::{Mode, Photo},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use ratatui_image::Image;

const MUTED: Color = Color::Rgb(132, 145, 164);
const ACCENT: Color = Color::Rgb(83, 204, 203);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < 45 || area.height < 14 {
        app.update_areas(Rect::default(), Rect::default(), Rect::default());
        frame.render_widget(
            Paragraph::new("PhotoPinFix\n请将终端放大至至少 45 列 × 14 行。\nq 退出")
                .wrap(Wrap { trim: false }),
            area,
        );
        if let Some(modal) = &app.modal {
            draw_modal(frame, app, modal);
        }
        return;
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(app.directory.display().to_string())
            .style(Style::default().fg(MUTED))
            .block(
                Block::bordered()
                    .title(app.title())
                    .border_style(Style::default().fg(ACCENT)),
            ),
        header,
    );
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    let [files, details] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(7)]).areas(left);
    let [photo, map_area] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(right);
    let photo_block = Block::bordered().title(" 照片预览 ");
    let photo_inner = photo_block.inner(photo);
    let sources = map::sources();
    let mode = if app.background {
        sources
            .get(app.source)
            .map(String::as_str)
            .unwrap_or("自动地图源")
    } else {
        "无底图"
    };
    let map_block = Block::bordered().title(format!(" 地图 · {mode} "));
    let map_inner = map_block.inner(map_area);
    let footer_height = if app.attribution.is_empty() { 3 } else { 5 };
    let [map_image, map_info] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(map_inner);
    app.update_areas(photo_inner, map_image, files);
    draw_files(frame, app, files);
    draw_details(frame, app, details);
    frame.render_widget(photo_block, photo);
    frame.render_widget(map_block, map_area);
    if app.modal.is_none() {
        if let Some(image) = &app.photo_image {
            let size = image.size();
            let actual = Rect::new(
                photo_inner.x + photo_inner.width.saturating_sub(size.width) / 2,
                photo_inner.y + photo_inner.height.saturating_sub(size.height) / 2,
                photo_inner.width.min(size.width),
                photo_inner.height.min(size.height),
            );
            frame.render_widget(Image::new(image), actual);
        } else {
            frame.render_widget(
                Paragraph::new(app.photo_status.as_str())
                    .style(Style::default().fg(MUTED))
                    .wrap(Wrap { trim: false }),
                photo_inner,
            );
        }
        if let Some(image) = &app.map_image {
            frame.render_widget(Image::new(image), map_image);
        }
    }
    let count = app.points().len();
    let selected = app
        .selected_photo()
        .and_then(Photo::effective_coordinates)
        .map(|c| format!("{:.6}, {:.6}", c.lat, c.lon))
        .unwrap_or_else(|| "当前照片暂无定位".into());
    let mut info = Vec::new();
    if let Some(scale) = &app.map_scale {
        let bar = format!(
            "├{}┤ {}",
            "─".repeat(scale.cells.saturating_sub(1) as usize),
            scale.label
        );
        info.push(Line::from(bar).style(Style::default().fg(Color::White)));
    } else {
        info.push(Line::from("比例尺计算中…").style(Style::default().fg(MUTED)));
    }
    info.push(Line::from(format!("{count} 个位置 · {selected}")));
    info.push(Line::from(app.map_status.as_str()).style(Style::default().fg(MUTED)));
    if !app.attribution.is_empty() {
        info.push(Line::from(app.attribution.as_str()));
    }
    frame.render_widget(Paragraph::new(info).wrap(Wrap { trim: false }), map_info);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(app.status.as_str()).style(Style::default().fg(ACCENT)),
            Line::from(
                "↑↓文件  ←→拍摄  a全部  e编辑  Enter写当前  W写全部  s设置  m底图  ?帮助  q退出",
            )
            .style(Style::default().fg(MUTED)),
        ]),
        footer,
    );
    if let Some(modal) = &app.modal {
        draw_modal(frame, app, modal);
    }
}

fn coordinate_pair(photo: &Photo, format: CoordinateFormat) -> Option<(String, String)> {
    photo.effective_coordinates().map(|c| {
        let notation = if format == CoordinateFormat::Dms {
            CoordinateNotation::Hemispheres
        } else {
            CoordinateNotation::Signed
        };
        (
            format_coordinate(c.lat, true, format, notation),
            format_coordinate(c.lon, false, format, notation),
        )
    })
}

fn photo_lines(photo: &Photo, format: CoordinateFormat, width: u16) -> (Vec<Line<'static>>, Color) {
    let (status, color) = if photo.gps.is_some() {
        ("GPS", Color::Green)
    } else if photo
        .candidate
        .is_some_and(|c| c.provenance.mode == Mode::Manual)
    {
        ("MAN *", Color::Yellow)
    } else if photo.candidate.is_some() {
        ("AUTO *", Color::Yellow)
    } else {
        ("缺失", Color::Red)
    };
    let method = photo
        .candidate
        .map(|c| c.provenance)
        .or(photo.provenance)
        .map(|p| p.method.label())
        .unwrap_or(if photo.gps.is_some() {
            "已有定位"
        } else {
            "无法自动补全"
        });
    let name = photo
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{status} · {method}  "), Style::default().bold()),
        Span::raw(name),
    ])];
    // Table cells do not wrap automatically. Keep the timezone and fractional
    // seconds visible even when the browser occupies half of an 80-column TUI.
    let mut time_line = String::new();
    for word in photo.capture_time_label().split_whitespace() {
        let next = if time_line.is_empty() {
            word.to_owned()
        } else {
            format!("{time_line} {word}")
        };
        if !time_line.is_empty() && Line::from(next.clone()).width() > width as usize {
            lines.push(Line::from(time_line).style(Style::default().fg(MUTED)));
            time_line = word.to_owned();
        } else {
            time_line = next;
        }
    }
    lines.push(Line::from(time_line).style(Style::default().fg(MUTED)));
    if let Some((lat, lon)) = coordinate_pair(photo, format) {
        let line = format!("纬 {lat}  经 {lon}");
        if Line::from(line.clone()).width() > width as usize {
            lines.push(Line::from(format!("纬 {lat}")));
            lines.push(Line::from(format!("经 {lon}")));
        } else {
            lines.push(Line::from(line));
        }
    } else {
        lines.push(Line::from("纬 —  经 —").style(Style::default().fg(MUTED)));
    }
    (lines, color)
}

fn draw_files(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_indices();
    let green = visible
        .iter()
        .filter(|i| app.photos[**i].gps.is_some())
        .count();
    let yellow = visible
        .iter()
        .filter(|i| app.photos[**i].gps.is_none() && app.photos[**i].candidate.is_some())
        .count();
    let red = visible.len() - green - yellow;
    let rows = app
        .rows
        .iter()
        .map(|entry| match entry {
            BrowserRow::Parent(_) => {
                Row::new([Cell::from("DIR  ../")]).style(Style::default().fg(ACCENT))
            }
            BrowserRow::Directory(path) => Row::new([Cell::from(format!(
                "DIR  {}/",
                path.file_name().unwrap_or_default().to_string_lossy()
            ))])
            .style(Style::default().fg(ACCENT)),
            BrowserRow::Photo(i) => {
                let (lines, color) = photo_lines(
                    &app.photos[*i],
                    app.coordinate_format,
                    area.width.saturating_sub(4),
                );
                let height = lines.len() as u16;
                Row::new([Cell::from(ratatui::text::Text::from(lines))])
                    .height(height)
                    .bottom_margin(1)
                    .style(Style::default().fg(color))
            }
        })
        .collect::<Vec<_>>();
    let table = Table::new(rows, [Constraint::Percentage(100)])
        .block(Block::bordered().title(format!(" 文件 · 绿{green} 黄{yellow} 红{red} ")))
        .row_highlight_style(Style::default().bg(Color::Rgb(32, 48, 65)))
        .highlight_symbol("› ");
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let lines = if let Some(photo) = app.selected_photo() {
        let source = if photo.gps.is_some() {
            "已有 GPS"
        } else if photo.candidate.is_some() {
            "待写入 *"
        } else {
            "缺少 GPS"
        };
        let gps = coordinate_pair(photo, app.coordinate_format)
            .map(|(lat, lon)| format!("{lat}  {lon}"))
            .unwrap_or_else(|| "无坐标；e 输入，p/n/c/i 选择参考".into());
        let provenance = photo
            .candidate
            .as_ref()
            .map(|c| &c.provenance)
            .or(photo.provenance.as_ref())
            .map(|p| {
                format!(
                    "PhotoPinFix: {} / {}",
                    if p.mode == Mode::Auto {
                        "auto"
                    } else {
                        "manual"
                    },
                    p.method.as_str()
                )
            })
            .unwrap_or_else(|| "来源未标注".into());
        vec![
            Line::from(source).bold(),
            Line::from(gps),
            Line::from(provenance),
            Line::from(
                photo
                    .error
                    .as_deref()
                    .unwrap_or("已有 GPS 会跳过；* 表示尚未写入"),
            )
            .style(Style::default().fg(MUTED)),
        ]
    } else {
        vec![
            Line::from("Enter / l 打开目录；Backspace 返回上级"),
            Line::from(format!("显示协议：{}", app.graphics_name)),
        ]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" 定位详情 ")),
        area,
    );
}

fn draw_modal(frame: &mut Frame, app: &App, modal: &Modal) {
    match modal {
        Modal::Edit {
            index,
            fields,
            focus,
            format,
            notation,
            error,
        } => {
            draw_editor(
                frame,
                &app.photos[*index],
                fields,
                *focus,
                *format,
                *notation,
                error,
            );
            return;
        }
        Modal::Settings {
            fields,
            focus,
            format,
            error,
        } => {
            draw_settings(frame, fields, *focus, *format, error);
            return;
        }
        _ => {}
    }
    let area = center(frame.area(), 86, 23);
    let (title, lines) = match modal {
        Modal::Help => (
            "快捷键",
            vec![
                Line::from("↑↓ / j k：移动光标；PageUp/Down：翻页；Home/End：首末行"),
                Line::from("← / →：上一次 / 下一次拍摄；a：当前拍摄 / 所有照片"),
                Line::from("Enter：确认写当前照片；Shift+Enter / W：确认写当前范围全部候选"),
                Line::from("e：手动编辑 GPS；F2 小数/度分秒，F3 正负号/NSEW"),
                Line::from("p：采用前一张绿图；n：后一张；c：时间最近；i：按时间插值"),
                Line::from("手动快捷键不受 1 分钟 / 10 分钟阈值限制；u：恢复自动建议"),
                Line::from("s：设置自动补全阈值与经纬度显示格式，仅本次运行有效"),
                Line::from("m：无底图 / 图片地图；b：切换地图源；+/-：缩放；f：全部点入镜"),
                Line::from("H/J/K/L：平移地图；鼠标在地图上滚动缩放"),
                Line::from("l / Enter：打开目录；Backspace：上级；r：重新扫描；v：错误详情"),
                Line::from("q / Ctrl+C：退出；对话框 Esc：取消；确认框 y：确认 / n：取消"),
                Line::from(""),
                Line::from(format!(
                    "当前图像协议：{}；增强键盘协议：{}",
                    app.graphics_name,
                    if app.keyboard_enhanced {
                        "已启用"
                    } else {
                        "未启用，写全部请用 W"
                    }
                )),
                Line::from("绿：文件已有 GPS；黄：可写候选；红：需要手动定位。* 表示未保存。"),
                Line::from("照片与地图预览不生成临时图片；地图缓存仅保留在内存。"),
                Line::from("任意键关闭").style(Style::default().fg(ACCENT)),
            ],
        ),
        Modal::Info(text) => ("错误与提示", text.lines().map(Line::from).collect()),
        Modal::Settings { .. } | Modal::Edit { .. } => unreachable!(),
        Modal::Confirm { indices, .. } => {
            let mut lines = vec![
                Line::from(format!(
                    "将写入 {} 张照片（{}）。",
                    indices.len(),
                    app.scope_label()
                ))
                .bold(),
                Line::from("仅写入缺少 GPS 且已有候选的文件；已有 GPS 全部跳过。"),
                Line::from("写入 GPS 与 PhotoPinFix:Mode / Method，保留原图备份。"),
                Line::from("保持创建、修改和访问时间；写完回读验证。"),
                Line::from(""),
            ];
            for i in indices.iter().take(7) {
                let p = &app.photos[*i];
                if let Some(c) = &p.candidate {
                    lines.push(Line::from(format!(
                        "{} → {:.6}, {:.6} [{}]",
                        p.path.file_name().unwrap_or_default().to_string_lossy(),
                        c.coordinates.lat,
                        c.coordinates.lon,
                        c.provenance.method.as_str()
                    )));
                }
            }
            if indices.len() > 7 {
                lines.push(Line::from(format!("…另 {} 张", indices.len() - 7)));
            }
            ("确认写入", lines)
        }
        Modal::Leave { .. } => (
            "尚有手动修改未写入",
            vec![Line::from("继续将丢弃本目录尚未写入的手动坐标。")],
        ),
    };
    let footer = match modal {
        Modal::Confirm { confirm, .. } | Modal::Leave { confirm, .. } => {
            confirmation_footer(*confirm)
        }
        _ => Vec::new(),
    };
    draw_dialog(frame, area, title, lines, footer);
}

fn choice(label: &str, selected: bool) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        if selected {
            Style::default().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::default().fg(MUTED).bg(Color::Rgb(30, 40, 54))
        },
    )
}

fn field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    input: &TextInput,
    active: bool,
    invalid: bool,
) {
    if area.is_empty() {
        return;
    }
    let color = if invalid {
        Color::Red
    } else if active {
        ACCENT
    } else {
        MUTED
    };
    let block = Block::bordered()
        .title(format!(" {label} "))
        .border_style(Style::default().fg(color));
    let inner = block.inner(area);
    let (value, cursor) = input.viewport(inner.width as usize);
    frame.render_widget(
        Paragraph::new(value)
            .style(Style::default().fg(Color::White).bg(Color::Rgb(24, 34, 48)))
            .block(block),
        area,
    );
    if active && inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((inner.x + cursor.min(inner.width - 1), inner.y));
    }
}

fn form_block(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(Color::Rgb(18, 24, 34)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn form_buttons(
    frame: &mut Frame,
    area: Rect,
    focus: usize,
    save: usize,
    cancel: usize,
    save_label: &str,
) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            choice(save_label, focus == save),
            Span::raw("  "),
            choice("取消 · Esc", focus == cancel),
        ])),
        area,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_editor(
    frame: &mut Frame,
    photo: &Photo,
    fields: &[TextInput; 2],
    focus: usize,
    format: CoordinateFormat,
    notation: CoordinateNotation,
    error: &str,
) {
    let area = center(frame.area(), 84, 23);
    let inner = form_block(frame, area, "编辑 GPS · 坐标输入");
    let compact = inner.height < 15;
    let [
        heading,
        selectors,
        latitude,
        longitude,
        preview,
        buttons,
        hints,
    ] = Layout::vertical([
        Constraint::Length(if compact { 0 } else { 2 }),
        Constraint::Length(if compact { 1 } else { 3 }),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(if compact { 2 } else { 3 }),
    ])
    .areas(inner);
    if !compact {
        frame.render_widget(
            Paragraph::new(
                photo
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
            .style(Style::default().fg(MUTED)),
            heading,
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::raw("格式  "),
                    choice("小数度", format == CoordinateFormat::Decimal),
                    Span::raw(" "),
                    choice("度分秒", format == CoordinateFormat::Dms),
                    Span::styled("  F2 切换", Style::default().fg(MUTED)),
                ]),
                Line::from(vec![
                    Span::raw("方向  "),
                    choice("+ / −", notation == CoordinateNotation::Signed),
                    Span::raw(" "),
                    choice("N / S · E / W", notation == CoordinateNotation::Hemispheres),
                    Span::styled("  F3 切换", Style::default().fg(MUTED)),
                ]),
            ]),
            selectors,
        );
    } else {
        frame.render_widget(
            Paragraph::new(format!(
                "F2 {}   F3 {}",
                format.label(),
                if notation == CoordinateNotation::Signed {
                    "+/−"
                } else {
                    "N/S E/W"
                }
            )),
            selectors,
        );
    }
    field(
        frame,
        latitude,
        "纬度 Latitude · −90…+90",
        &fields[0],
        focus == 0,
        !fields[0].value.is_empty() && parse_coordinate(&fields[0].value, true).is_err(),
    );
    field(
        frame,
        longitude,
        "经度 Longitude · −180…+180",
        &fields[1],
        focus == 1,
        !fields[1].value.is_empty() && parse_coordinate(&fields[1].value, false).is_err(),
    );
    let feedback = if !error.is_empty() {
        Line::from(error.to_string()).red()
    } else {
        match editor_coordinates(fields) {
            Ok(c) => Line::from(format!("✓  有效坐标  {:+.7}, {:+.7}", c.lat, c.lon)).green(),
            Err(message) => Line::from(message).style(Style::default().fg(MUTED)),
        }
    };
    frame.render_widget(
        Paragraph::new(vec![
            feedback,
            Line::from("支持 −33.5、33.5S、S33°30′00″；正负号须与方向一致。")
                .style(Style::default().fg(MUTED)),
        ])
        .wrap(Wrap { trim: false }),
        preview,
    );
    form_buttons(frame, buttons, focus, 2, 3, "暂存坐标 · Enter");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Tab 切换控件 · ←→ 移动光标 · Home/End · Ctrl+U 清空"),
            Line::from("F2 小数/度分秒 · F3 ±/方位 · Enter 暂存 · Esc 取消"),
            Line::from("暂存后返回主界面，按 Enter 确认写入文件。"),
        ])
        .style(Style::default().fg(MUTED)),
        hints,
    );
}

fn draw_settings(
    frame: &mut Frame,
    fields: &[TextInput; 2],
    focus: usize,
    format: CoordinateFormat,
    error: &str,
) {
    let area = center(frame.area(), 78, 23);
    let inner = form_block(frame, area, "Settings · 本次运行");
    let compact = inner.height < 15;
    let [description, gap, window, display, feedback, buttons, hints] = Layout::vertical([
        Constraint::Length(if compact { 0 } else { 2 }),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(if compact { 1 } else { 3 }),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(if compact { 2 } else { 3 }),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new("修改自动补全范围和经纬度显示方式，退出后恢复默认。")
            .style(Style::default().fg(MUTED)),
        description,
    );
    field(
        frame,
        gap,
        "相邻间隔 · 秒 · 严格小于",
        &fields[0],
        focus == 0,
        false,
    );
    field(
        frame,
        window,
        "每侧参考上限 · 秒 · 不超过",
        &fields[1],
        focus == 1,
        false,
    );
    let line = Line::from(vec![
        Span::raw(if focus == 2 {
            "› 坐标 "
        } else {
            "  坐标 "
        }),
        choice("小数度", format == CoordinateFormat::Decimal),
        Span::raw(" "),
        choice("度分秒", format == CoordinateFormat::Dms),
        Span::styled("  F2", Style::default().fg(MUTED)),
    ]);
    if compact {
        frame.render_widget(Paragraph::new(line), display);
    } else {
        frame.render_widget(
            Paragraph::new(line).block(
                Block::bordered()
                    .title(" 文件列表与定位详情 ")
                    .border_style(Style::default().fg(if focus == 2 { ACCENT } else { MUTED })),
            ),
            display,
        );
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(error.to_string()).red(),
            Line::from("默认 60 / 600 秒；保留已有手动候选。").style(Style::default().fg(MUTED)),
        ])
        .wrap(Wrap { trim: false }),
        feedback,
    );
    form_buttons(frame, buttons, focus, 3, 4, "应用设置 · Enter");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Tab 切换控件 · Ctrl+U 清空 · F2 切换坐标格式"),
            Line::from("选中坐标格式后 ←/→ 或空格切换 · Enter 应用"),
            Line::from("仅本次运行有效，不写入配置文件。"),
        ])
        .style(Style::default().fg(MUTED)),
        hints,
    );
}

fn draw_dialog<'a>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'a>>,
    footer: Vec<Line<'a>>,
) {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // File names may wrap onto many lines. Reserve controls independently of
    // the scrolling/clipped body so neither batch size nor terminal width can
    // push the confirmation buttons off screen.
    let [body, controls] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer.len() as u16)])
            .areas(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    frame.render_widget(Paragraph::new(footer), controls);
}

fn confirmation_footer(confirm: bool) -> Vec<Line<'static>> {
    vec![
        confirm_line(confirm),
        Line::from("Tab/←→ 选择 · Enter 执行"),
        Line::from("y 确认 · Esc / n 取消"),
    ]
}

fn confirm_line(confirm: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " [取消] ",
            if !confirm {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default()
            },
        ),
        Span::styled(
            " [确认] ",
            if confirm {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default()
            },
        ),
    ])
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn sample_photo() -> Photo {
        Photo {
            path: "photo.heic".into(),
            captured_at: chrono::NaiveDateTime::parse_from_str(
                "2026-03-01 09:02:03.050",
                "%Y-%m-%d %H:%M:%S%.f",
            )
            .ok(),
            capture_time_info: photopinfix::model::CaptureTimeInfo {
                offset: chrono::FixedOffset::east_opt(8 * 3600),
                subsecond_digits: Some("050".into()),
            },
            gps: None,
            provenance: None,
            candidate: Some(photopinfix::model::Candidate {
                coordinates: photopinfix::model::Coordinates {
                    lat: -33.5,
                    lon: -70.25,
                },
                provenance: photopinfix::model::Provenance {
                    mode: Mode::Auto,
                    method: photopinfix::model::Method::TimeInterpolation,
                },
            }),
            error: None,
        }
    }

    #[test]
    fn browser_rows_show_strategy_full_capture_time_and_both_coordinate_formats() {
        let photo = sample_photo();
        for format in [CoordinateFormat::Decimal, CoordinateFormat::Dms] {
            let (lines, color) = photo_lines(&photo, format, 68);
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(color, Color::Yellow);
            assert!(text.contains("AUTO * · 时间插值"));
            assert!(text.contains("2026-03-01 09:02:03.050 +08:00"), "{text}");
            assert!(text.contains("纬 ") && text.contains("经 "));
            assert!(if format == CoordinateFormat::Decimal {
                text.contains("-33.5000000")
            } else {
                text.contains("33°30′00.000″S")
            });
        }
        let (lines, _) = photo_lines(&photo, CoordinateFormat::Decimal, 25);
        let time_lines: Vec<_> = lines.iter().skip(1).take(2).collect();
        assert!(time_lines.iter().all(|line| line.width() <= 25));
        assert_eq!(
            time_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(" "),
            "2026-03-01 09:02:03.050 +08:00"
        );
    }

    #[test]
    fn coordinate_form_has_fields_mode_controls_and_visible_save_cancel_buttons() {
        let photo = sample_photo();
        let fields = [
            TextInput::from("-33.5".to_owned()),
            TextInput::from("70.25W".to_owned()),
        ];
        for (width, height) in [(100, 30), (80, 24), (45, 14)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_editor(
                        frame,
                        &photo,
                        &fields,
                        0,
                        CoordinateFormat::Decimal,
                        CoordinateNotation::Signed,
                        "",
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = (0..height)
                .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
                .collect::<String>()
                .replace(' ', "");
            assert!(
                text.contains("纬度Latitude") && text.contains("经度Longitude"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("暂存坐标") && text.contains("取消"), "{text}");
            assert!(text.contains("F2") && text.contains("F3"));
        }
    }

    #[test]
    fn confirmation_controls_survive_long_file_names_and_small_terminals() {
        for (width, height) in [(80, 24), (45, 14)] {
            for count in [1, 7] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        let mut lines = vec![Line::from(format!("将写入 {count} 张照片"))];
                        lines.extend((0..count).map(|_| {
                            Line::from(format!(
                                "{} → 31.230400, 121.473700 [time-interpolation]",
                                "very_long_file_name_".repeat(12)
                            ))
                        }));
                        draw_dialog(
                            frame,
                            center(frame.area(), 86, 23),
                            "确认写入",
                            lines,
                            confirmation_footer(false),
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let rows: Vec<String> = (0..height)
                    .map(|y| {
                        (0..width)
                            .map(|x| buffer[(x, y)].symbol())
                            .collect::<String>()
                            .replace(' ', "")
                    })
                    .collect();
                let controls = rows
                    .iter()
                    .position(|row| row.contains("[取消]") && row.contains("[确认]"))
                    .expect("both confirmation buttons must stay visible");
                assert!(rows[controls + 1].contains("Enter"));
                assert!(rows[controls + 2].contains("Esc/n取消"));
                assert!(
                    rows.iter()
                        .any(|row| row.contains(&format!("将写入{count}张照片")))
                );
            }
        }
    }
}
