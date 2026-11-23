//! Explicit visual-check artifacts, not application cache files.
//! cargo run --example map_preview_check -- [--online]
use std::{error::Error, fs, path::Path};

use image::{Rgba, RgbaImage, imageops::FilterType};
use photopinfix::{
    map::{self, MapPoint, MapRenderer, MapView},
    model::Coordinates,
};
use ratatui::{Terminal, backend::TestBackend, layout::Size, style::Color};
use ratatui_image::{
    Image, Resize,
    picker::Picker,
    protocol::{Protocol, halfblocks::Halfblocks},
};

fn buffer_pixels(protocol: &Protocol, size: Size) -> Result<RgbaImage, Box<dyn Error>> {
    let mut terminal = Terminal::new(TestBackend::new(size.width, size.height))?;
    terminal.draw(|frame| frame.render_widget(Image::new(protocol), frame.area()))?;
    let buffer = terminal.backend().buffer();
    let mut image = RgbaImage::new(size.width.into(), u32::from(size.height) * 2);
    for y in 0..size.height {
        for x in 0..size.width {
            let cell = &buffer[(x, y)];
            for (offset, color) in [(0, cell.fg), (1, cell.bg)] {
                let pixel = match color {
                    Color::Rgb(r, g, b) => Rgba([r, g, b, 255]),
                    _ => Rgba([0, 0, 0, 255]),
                };
                image.put_pixel(u32::from(x), u32::from(y) * 2 + offset, pixel);
            }
        }
    }
    Ok(image)
}

fn save_halfblocks(path: &Path, image: &RgbaImage) -> Result<(), Box<dyn Error>> {
    image::imageops::resize(
        image,
        image.width() * 10,
        image.height() * 10,
        FilterType::Nearest,
    )
    .save(path)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = Path::new("target/test-artifacts");
    fs::create_dir_all(output)?;
    let points: Vec<MapPoint> = [
        (48.8584, 2.2945, true, false),
        (48.8597, 2.3132, false, false),
        (48.8610, 2.3319, false, true),
        (48.8623, 2.3506, true, false),
    ]
    .into_iter()
    .map(|(lat, lon, existing, selected)| MapPoint {
        coordinates: Coordinates { lat, lon },
        selected,
        existing,
    })
    .collect();
    let view = MapView::fit(&points);
    let size = Size::new(60, 12);
    let mut renderer = MapRenderer::new();
    let full = renderer.render(&points, &view, 600, 240, false, 0);
    full.image.save(output.join("map-offline.png"))?;
    let picker = Picker::halfblocks();
    let former = picker.new_protocol(full.image, size, Resize::Fit(None))?;
    let former = buffer_pixels(&former, size)?;
    save_halfblocks(&output.join("map-halfblocks-downsampled.png"), &former)?;

    renderer.set_marker_radii(1, 2, 1);
    let native = renderer.render(&points, &view, 60, 24, false, 0);
    let native = Protocol::Halfblocks(Halfblocks::new(native.image, size)?);
    let native = buffer_pixels(&native, size)?;
    save_halfblocks(&output.join("map-halfblocks-native.png"), &native)?;
    let green = native
        .pixels()
        .filter(|p| p.0 == [76, 220, 128, 255])
        .count();
    let yellow = native
        .pixels()
        .filter(|p| p.0 == [255, 209, 76, 255])
        .count();
    assert!(
        green >= 5 && yellow >= 13,
        "native half-block markers lost their color or size"
    );
    println!(
        "Offline map and actual ratatui half-block buffer rendered: green pixels={green}, yellow pixels={yellow}"
    );

    if std::env::args().any(|arg| arg == "--online") {
        renderer.set_marker_radii(3, 7, 2);
        for name in ["OpenStreetMap", "NASA Blue Marble"] {
            let sources = map::sources();
            let source = sources
                .iter()
                .position(|item| item.contains(name))
                .ok_or("source not available")?;
            let result = renderer.render(&points, &view, 600, 240, true, source);
            println!("{} | {}", result.status, result.attribution);
            if result.attribution.is_empty() || !result.status.starts_with(name) {
                return Err(format!(
                    "Requested background {name} did not succeed: {}",
                    result.status
                )
                .into());
            }
            let label = if name == "OpenStreetMap" {
                "osm"
            } else {
                "nasa"
            };
            result.image.save(output.join(format!("map-{label}.png")))?;
        }
    }
    println!("Visual check artifacts: {}", output.display());
    Ok(())
}
