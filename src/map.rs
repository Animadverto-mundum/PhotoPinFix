//! Map images are assembled in memory; this module never writes a cache or image file.
//! Only tiles intersecting the current viewport are requested, serially.
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io::{Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use image::{DynamicImage, ImageReader, Rgba, RgbaImage};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{
        CACHE_CONTROL, ETAG, EXPIRES, HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
    },
};

use crate::model::Coordinates;

const TILE_SIZE: f64 = 256.0;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LATITUDE: f64 = 85.051_128_78;
// EPSG:3857 uses the WGS84 equatorial radius for its spherical projection.
const MERCATOR_RADIUS_METERS: f64 = 6_378_137.0;
const BACKGROUND: Rgba<u8> = Rgba([18, 24, 34, 255]);

#[derive(Debug, Clone, Copy)]
pub struct MapPoint {
    pub coordinates: Coordinates,
    pub selected: bool,
    pub existing: bool,
}

/// Background identity deliberately excludes markers. Selection, GPS edits and
/// write completion must not restart a slow tile request for the same view.
pub fn background_key(view: &MapView, size: [u32; 2], source: usize, scope: u64) -> u64 {
    let mut state = DefaultHasher::new();
    for value in [
        view.center_x,
        view.center_y,
        view.span_x,
        view.span_y,
        view.zoom,
    ] {
        value.to_bits().hash(&mut state);
    }
    size.hash(&mut state);
    source.hash(&mut state);
    scope.hash(&mut state);
    state.finish()
}

/// Pure, synchronous compositing: a cached background never contains markers.
/// Calling with new points immediately replaces old-session/old-selection dots.
pub fn compose_points(
    background: Option<&DynamicImage>,
    points: &[MapPoint],
    view: &MapView,
    size: [u32; 2],
    radii: (u32, u32, u32),
) -> DynamicImage {
    let width = size[0].clamp(1, 2048);
    let height = size[1].clamp(1, 1536);
    let mut image = background
        .filter(|image| image.width() == width && image.height() == height)
        .map_or_else(
            || RgbaImage::from_pixel(width, height, BACKGROUND),
            DynamicImage::to_rgba8,
        );
    let normal = radii.0.clamp(1, 32);
    let selected = radii.1.clamp(1, 64).max(normal);
    let outline = radii.2.min(8);
    paint_points(
        &mut image,
        points,
        view,
        (normal as i32, selected as i32, outline as i32),
    );
    DynamicImage::ImageRgba8(image)
}

fn paint_points(
    image: &mut RgbaImage,
    points: &[MapPoint],
    view: &MapView,
    radii: (i32, i32, i32),
) {
    let (width, height) = image.dimensions();
    for selected in [false, true] {
        for point in points.iter().filter(|point| point.selected == selected) {
            if !point.coordinates.lat.is_finite() || !point.coordinates.lon.is_finite() {
                continue;
            }
            let (x, y) = view.pixel_position(point.coordinates, width, height);
            if x < -12.0 || y < -12.0 || x > width as f64 + 12.0 || y > height as f64 + 12.0 {
                continue;
            }
            let color = if point.existing {
                Rgba([76, 220, 128, 255])
            } else {
                Rgba([255, 209, 76, 255])
            };
            let (normal, selected_radius, outline) = radii;
            let radius = if selected { selected_radius } else { normal };
            circle(
                image,
                x.round() as i32,
                y.round() as i32,
                radius + outline,
                if selected {
                    Rgba([247, 250, 255, 255])
                } else {
                    Rgba([5, 10, 17, 255])
                },
            );
            circle(image, x.round() as i32, y.round() as i32, radius, color);
        }
    }
}

/// A horizontal metric ruler at the map's center latitude. `cells` is the
/// distance between its endpoint columns: `├───┤` spans four cells. Terminal
/// rounding introduces at most half a cell of error in the represented length.
#[derive(Debug, Clone, PartialEq)]
pub struct MapScale {
    pub cells: u16,
    pub meters: f64,
    pub label: String,
}

/// Coordinates are normalized Web Mercator world coordinates, independent of pixels.
#[derive(Debug, Clone)]
pub struct MapView {
    pub center_x: f64,
    pub center_y: f64,
    pub span_x: f64,
    pub span_y: f64,
    pub zoom: f64,
}

impl Default for MapView {
    fn default() -> Self {
        Self {
            center_x: 0.5,
            center_y: 0.5,
            span_x: 1.0,
            span_y: 1.0,
            zoom: 0.0,
        }
    }
}

impl MapView {
    pub fn fit(points: &[MapPoint]) -> Self {
        let projected: Vec<(f64, f64)> = points
            .iter()
            .filter(|point| point.coordinates.lat.is_finite() && point.coordinates.lon.is_finite())
            .map(|point| project(point.coordinates))
            .collect();
        if projected.is_empty() {
            return Self::default();
        }
        // The complement of the largest gap gives the smallest longitude arc.
        // In particular 179.9 E and 179.9 W stay next to each other.
        let mut xs: Vec<f64> = projected.iter().map(|point| point.0).collect();
        xs.sort_by(f64::total_cmp);
        let mut largest_gap = -1.0;
        let mut arc_start = xs[0];
        for index in 0..xs.len() {
            let next = if index + 1 == xs.len() {
                xs[0] + 1.0
            } else {
                xs[index + 1]
            };
            let gap = next - xs[index];
            if gap > largest_gap {
                largest_gap = gap;
                arc_start = next.rem_euclid(1.0);
            }
        }
        let span_x = (1.0 - largest_gap).max(0.000_02);
        let min_y = projected
            .iter()
            .map(|point| point.1)
            .fold(f64::INFINITY, f64::min);
        let max_y = projected
            .iter()
            .map(|point| point.1)
            .fold(f64::NEG_INFINITY, f64::max);
        Self {
            center_x: (arc_start + (1.0 - largest_gap) / 2.0).rem_euclid(1.0),
            center_y: (min_y + max_y) / 2.0,
            span_x: (span_x * 1.25).min(1.0),
            span_y: ((max_y - min_y).max(0.000_02) * 1.25).min(1.0),
            zoom: 0.0,
        }
    }

    pub fn zoom_by(&mut self, delta: f64) {
        if delta.is_finite() {
            self.zoom = (self.zoom + delta).clamp(-16.0, 18.0);
        }
    }

    /// Pan by a fraction of the fitted view; positive y moves south.
    pub fn pan(&mut self, dx_fraction: f64, dy_fraction: f64) {
        if dx_fraction.is_finite() && dy_fraction.is_finite() {
            let scale = 2_f64.powf(-self.zoom);
            self.center_x = (self.center_x + dx_fraction * self.span_x * scale).rem_euclid(1.0);
            self.center_y = (self.center_y + dy_fraction * self.span_y * scale).clamp(0.0, 1.0);
        }
    }

    pub fn pixel_position(&self, coordinates: Coordinates, width: u32, height: u32) -> (f64, f64) {
        let geometry = self.geometry(width.max(1), height.max(1));
        let (x, y) = project(coordinates);
        let dx = wrap_delta(x - self.center_x);
        (
            width as f64 / 2.0 + dx / geometry.units_per_pixel,
            height as f64 / 2.0 + (y - self.center_y) / geometry.units_per_pixel,
        )
    }

    /// Use the rendered image dimensions and the encoded protocol's column
    /// count, which can differ from the requested area after image-size limits.
    /// Longitude is deliberately absent from the metric conversion: crossing
    /// the date line must not produce a world-sized jump in the ruler.
    pub fn scale(&self, width: u32, height: u32, columns: u16) -> Option<MapScale> {
        if width == 0
            || height == 0
            || columns < 12
            || !self.center_y.is_finite()
            || !self.span_x.is_finite()
            || !self.span_y.is_finite()
            || !self.zoom.is_finite()
            || self.span_x <= 0.0
            || self.span_y <= 0.0
        {
            return None;
        }
        let geometry = self.geometry(width, height);
        // Inverse Mercator: latitude = atan(sinh(pi * (1 - 2*y))). Hence
        // cos(latitude) = 1/cosh(pi * (1 - 2*y)), without a pole singularity.
        let mercator_y = std::f64::consts::PI * (1.0 - 2.0 * self.center_y.clamp(0.0, 1.0));
        let meters_per_cell =
            geometry.units_per_pixel * std::f64::consts::TAU * MERCATOR_RADIUS_METERS
                / mercator_y.cosh()
                * f64::from(width)
                / f64::from(columns);
        if !meters_per_cell.is_finite() || meters_per_cell <= 0.0 {
            return None;
        }

        let mut maximum_cells = (columns / 3).min(24);
        while maximum_cells > 0 {
            let target_meters = meters_per_cell * f64::from(maximum_cells);
            let magnitude = 10_f64.powf(target_meters.log10().floor());
            let multiple = target_meters / magnitude;
            let meters = magnitude
                * if multiple >= 5.0 {
                    5.0
                } else if multiple >= 2.0 {
                    2.0
                } else {
                    1.0
                };
            let cells = (meters / meters_per_cell)
                .round()
                .clamp(1.0, f64::from(maximum_cells)) as u16;
            let (number, unit) = if meters >= 1000.0 {
                (meters / 1000.0, "km")
            } else {
                (meters, "m")
            };
            let decimals = (-number.log10().floor()).clamp(0.0, 12.0) as usize;
            let label = format!("{number:.decimals$} {unit}");
            // Account for both endpoints, one space, and the complete label.
            if usize::from(cells) + 2 + label.len() <= usize::from(columns) {
                return Some(MapScale {
                    cells,
                    meters,
                    label,
                });
            }
            maximum_cells -= 1;
        }
        None
    }

    fn geometry(&self, width: u32, height: u32) -> Geometry {
        let base = (self.span_x / width as f64).max(self.span_y / height as f64);
        let units_per_pixel = (base * 2_f64.powf(-self.zoom)).clamp(
            1.0 / (TILE_SIZE * 2_f64.powi(23)),
            1.0 / width.max(height) as f64,
        );
        Geometry {
            units_per_pixel,
            left: self.center_x - width as f64 * units_per_pixel / 2.0,
            top: self.center_y - height as f64 * units_per_pixel / 2.0,
        }
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    units_per_pixel: f64,
    left: f64,
    top: f64,
}

pub struct MapResult {
    pub image: DynamicImage,
    pub status: String,
    pub attribution: String,
}

struct Source {
    name: String,
    template: String,
    attribution: String,
    max_zoom: u8,
}

/// Sources requiring personal keys are offered only when their environment variable is set.
/// No keys, images or preferences are persisted by the application.
pub fn sources() -> Vec<String> {
    let mut names = vec!["自动选择／故障切换".to_string()];
    names.extend(map_sources().into_iter().map(|source| source.name));
    names
}

fn map_sources() -> Vec<Source> {
    // Sources and policies:
    // https://operations.osmfoundation.org/policies/tiles/
    // https://www.cyclosm.org/ (standard style, not the restricted live style)
    // https://maps.eox.at/ (public application use with visible attribution)
    // https://nasa-gibs.github.io/gibs-api-docs/access-basics/
    let mut sources = vec![
        Source {
            name: "OpenStreetMap".into(),
            template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
            attribution: "© OpenStreetMap contributors · openstreetmap.org/copyright".into(),
            max_zoom: 19,
        },
        Source {
            name: "CyclOSM".into(),
            template: "https://a.tile-cyclosm.openstreetmap.fr/cyclosm/{z}/{x}/{y}.png".into(),
            attribution: "© OpenStreetMap contributors · CyclOSM / OSM France".into(),
            max_zoom: 20,
        },
        Source {
            name: "EOX Terrain Light".into(),
            template: "https://tiles.maps.eox.at/wmts/1.0.0/terrain-light_3857/default/g/{z}/{y}/{x}.jpg".into(),
            attribution: "© EOX · maps.eox.at · © OpenStreetMap contributors".into(),
            max_zoom: 14,
        },
        Source {
            name: "NASA Blue Marble（全球低分辨率）".into(),
            template: "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/BlueMarble_NextGeneration/default/GoogleMapsCompatible_Level8/{z}/{y}/{x}.jpeg".into(),
            attribution: "NASA Blue Marble · NASA EOSDIS GIBS".into(),
            max_zoom: 8,
        },
    ];
    for (variable, name, template, attribution, max_zoom) in [
        (
            "PHOTOPINFIX_MAPTILER_KEY",
            "MapTiler Streets",
            "https://api.maptiler.com/maps/streets-v4/256/{z}/{x}/{y}.png",
            "© MapTiler · © OpenStreetMap contributors",
            20,
        ),
        (
            "PHOTOPINFIX_STADIA_KEY",
            "Stadia Alidade Smooth",
            "https://tiles.stadiamaps.com/tiles/alidade_smooth/{z}/{x}/{y}.png",
            "© Stadia Maps · © OpenMapTiles · © OpenStreetMap contributors",
            20,
        ),
        (
            "PHOTOPINFIX_CARTO_KEY",
            "CARTO Voyager",
            "https://basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}.png",
            "© CARTO · © OpenStreetMap contributors",
            20,
        ),
    ] {
        if let Ok(key) = std::env::var(variable)
            && !key.trim().is_empty()
        {
            let parameter = if variable == "PHOTOPINFIX_STADIA_KEY" {
                "api_key"
            } else {
                "key"
            };
            let query: String = reqwest::Url::parse_with_params(
                "https://example.invalid/",
                [(parameter, key.trim())],
            )
            .ok()
            .and_then(|url| url.query().map(str::to_owned))
            .unwrap_or_default();
            sources.push(Source {
                name: name.into(),
                template: format!("{template}?{query}"),
                attribution: attribution.into(),
                max_zoom,
            });
        }
    }
    if let Ok(template) = std::env::var("PHOTOPINFIX_TILE_URL")
        && template.starts_with("https://")
        && ["{z}", "{x}", "{y}"]
            .iter()
            .all(|part| template.contains(part))
    {
        sources.push(Source {
            name: "自定义 HTTPS XYZ".into(),
            template,
            attribution: std::env::var("PHOTOPINFIX_TILE_ATTRIBUTION")
                .unwrap_or_else(|_| "自定义地图源".into()),
            max_zoom: 20,
        });
    }
    sources
}

#[derive(Clone)]
struct CachedTile {
    image: Arc<RgbaImage>,
    expires: Instant,
    etag: Option<String>,
    modified: Option<String>,
    used: u64,
}

pub struct MapRenderer {
    client: Option<Client>,
    sources: Vec<Source>,
    cache: HashMap<(usize, u8, u32, u32), CachedTile>,
    cache_bytes: usize,
    tick: u64,
    unavailable_until: HashMap<usize, Instant>,
    last_good_source: Option<usize>,
    marker_radii: (i32, i32, i32),
}

impl Default for MapRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MapRenderer {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent(concat!(
                "PhotoPinFix/",
                env!("CARGO_PKG_VERSION"),
                " (interactive photo geotag review)"
            ))
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .ok();
        Self {
            client,
            sources: map_sources(),
            cache: HashMap::new(),
            cache_bytes: 0,
            tick: 0,
            unavailable_until: HashMap::new(),
            last_good_source: None,
            marker_radii: (3, 7, 2),
        }
    }

    /// Half-block maps render at one pixel per column and two per row. Smaller
    /// radii keep their markers legible without downsampling a full-size image.
    pub fn set_marker_radii(&mut self, normal: u32, selected: u32, outline: u32) {
        let normal = normal.clamp(1, 32) as i32;
        self.marker_radii = (
            normal,
            (selected.clamp(1, 64) as i32).max(normal),
            outline.min(8) as i32,
        );
    }

    /// Blocking worker API. The UI must call this on its map worker thread.
    pub fn render(
        &mut self,
        points: &[MapPoint],
        view: &MapView,
        width: u32,
        height: u32,
        background: bool,
        source_index: usize,
    ) -> MapResult {
        self.render_cancellable(
            points,
            view,
            width,
            height,
            background,
            source_index,
            &AtomicBool::new(false),
        )
    }

    /// A replaced UI request should set its cancel flag, then discard its eventual result.
    #[allow(clippy::too_many_arguments)] // View, output size, source, and cancellation are independent inputs.
    pub fn render_cancellable(
        &mut self,
        points: &[MapPoint],
        view: &MapView,
        width: u32,
        height: u32,
        background: bool,
        source_index: usize,
        cancel: &AtomicBool,
    ) -> MapResult {
        let width = width.clamp(1, 2048);
        let height = height.clamp(1, 1536);
        let mut result = if background && !points.is_empty() {
            self.render_background_cancellable(view, width, height, source_index, cancel)
        } else {
            MapResult {
                image: DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, BACKGROUND)),
                status: "无底图".into(),
                attribution: String::new(),
            }
        };
        if let Some(image) = result.image.as_mut_rgba8() {
            paint_points(image, points, view, self.marker_radii);
        }
        result
            .status
            .push_str(&format!(" · {} 个位置", points.len()));
        result
    }

    /// Fetch and assemble only the background. UI layers composite their current
    /// points separately, even while this blocking, cancellable request runs.
    pub fn render_background_cancellable(
        &mut self,
        view: &MapView,
        width: u32,
        height: u32,
        source_index: usize,
        cancel: &AtomicBool,
    ) -> MapResult {
        let width = width.clamp(1, 2048);
        let height = height.clamp(1, 1536);
        let mut image = RgbaImage::from_pixel(width, height, BACKGROUND);
        let mut attribution = String::new();
        let mut status = "底图暂不可用，保留位置点".to_owned();
        if !cancel.load(Ordering::Relaxed) {
            let mut failures = Vec::new();
            let start = if source_index == 0 {
                self.last_good_source.unwrap_or(0)
            } else {
                (source_index - 1).min(self.sources.len().saturating_sub(1))
            };
            let deadline = Instant::now() + Duration::from_secs(20);
            for offset in 0..self.sources.len() {
                if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    break;
                }
                let index = (start + offset) % self.sources.len();
                if self
                    .unavailable_until
                    .get(&index)
                    .is_some_and(|until| *until > Instant::now())
                {
                    continue;
                }
                match self.draw_background(index, view, &mut image, cancel, deadline) {
                    Ok(()) => {
                        self.last_good_source = Some(index);
                        attribution = self.sources[index].attribution.clone();
                        let fallback = if offset > 0 {
                            " · 已切换备用源"
                        } else {
                            ""
                        };
                        status = format!("{}{}", self.sources[index].name, fallback);
                        break;
                    }
                    Err(error) => {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        failures.push(format!("{}：{}", self.sources[index].name, error));
                        self.unavailable_until
                            .insert(index, Instant::now() + Duration::from_secs(30));
                    }
                }
            }
            if attribution.is_empty() {
                status = format!(
                    "底图暂不可用，保留位置点{}",
                    failures
                        .first()
                        .map(|error| format!(" · {error}"))
                        .unwrap_or_default()
                );
            }
        }
        MapResult {
            image: DynamicImage::ImageRgba8(image),
            status,
            attribution,
        }
    }

    fn draw_background(
        &mut self,
        source: usize,
        view: &MapView,
        image: &mut RgbaImage,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<(), String> {
        let geometry = view.geometry(image.width(), image.height());
        let zoom = ((1.0 / (TILE_SIZE * geometry.units_per_pixel))
            .log2()
            .floor() as i32)
            .clamp(0, self.sources[source].max_zoom as i32) as u8;
        let tiles_across = 1_u32 << zoom;
        let n = tiles_across as f64;
        let min_x = (geometry.left * n).floor() as i64;
        let max_x =
            ((geometry.left + image.width() as f64 * geometry.units_per_pixel) * n).floor() as i64;
        let min_y = ((geometry.top * n).floor() as i64).max(0);
        let max_y =
            (((geometry.top + image.height() as f64 * geometry.units_per_pixel) * n).floor()
                as i64)
                .min(tiles_across as i64 - 1);
        let mut tiles: HashMap<(u32, u32), Arc<RgbaImage>> = HashMap::new();
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if cancel.load(Ordering::Relaxed) {
                    return Err("已取消".into());
                }
                if Instant::now() >= deadline {
                    return Err("加载超时".into());
                }
                let wrapped_x = x.rem_euclid(tiles_across as i64) as u32;
                let key = (wrapped_x, y as u32);
                if tiles.contains_key(&key) {
                    continue;
                }
                if tiles.len() >= 80 {
                    return Err("视口瓦片数量过多，请缩小窗口".into());
                }
                let tile = self.tile(source, zoom, wrapped_x, y as u32)?;
                tiles.insert(key, tile);
            }
        }
        if tiles.is_empty() {
            return Err("当前位置超出地图投影范围".into());
        }
        // Sampling directly avoids huge temporary images when upscaling coarse sources.
        for y in 0..image.height() {
            if y % 32 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("已取消".into());
            }
            let world_y = geometry.top + (y as f64 + 0.5) * geometry.units_per_pixel;
            if !(0.0..1.0).contains(&world_y) {
                continue;
            }
            let tile_y = world_y * n;
            for x in 0..image.width() {
                let world_x =
                    (geometry.left + (x as f64 + 0.5) * geometry.units_per_pixel).rem_euclid(1.0);
                let tile_x = world_x * n;
                if let Some(tile) = tiles.get(&(tile_x.floor() as u32, tile_y.floor() as u32)) {
                    let px = ((tile_x.fract() * tile.width() as f64) as u32).min(tile.width() - 1);
                    let py =
                        ((tile_y.fract() * tile.height() as f64) as u32).min(tile.height() - 1);
                    image.put_pixel(x, y, *tile.get_pixel(px, py));
                }
            }
        }
        Ok(())
    }

    fn tile(&mut self, source: usize, zoom: u8, x: u32, y: u32) -> Result<Arc<RgbaImage>, String> {
        let key = (source, zoom, x, y);
        self.tick = self.tick.wrapping_add(1);
        if let Some(tile) = self.cache.get_mut(&key) {
            tile.used = self.tick;
            if tile.expires > Instant::now() {
                return Ok(Arc::clone(&tile.image));
            }
        }
        let stale = self.cache.get(&key).cloned();
        let url = self.sources[source]
            .template
            .replace("{z}", &zoom.to_string())
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string());
        let client = self.client.as_ref().ok_or("网络组件初始化失败")?;
        let mut request = client.get(url);
        if let Some(tile) = &stale {
            if let Some(etag) = &tile.etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(modified) = &tile.modified {
                request = request.header(IF_MODIFIED_SINCE, modified);
            }
        }
        // Do not include reqwest errors: their URLs may contain a user's API key.
        let mut response = request.send().map_err(|error| {
            if error.is_timeout() {
                "连接超时"
            } else {
                "网络连接失败"
            }
            .to_string()
        })?;
        if response.status() == StatusCode::NOT_MODIFIED
            && let Some(mut tile) = stale
        {
            let (ttl, cacheable) = cache_lifetime(response.headers());
            tile.expires = Instant::now() + ttl;
            tile.used = self.tick;
            let image = Arc::clone(&tile.image);
            if cacheable {
                self.cache.insert(key, tile);
            } else if let Some(removed) = self.cache.remove(&key) {
                self.cache_bytes = self
                    .cache_bytes
                    .saturating_sub(removed.image.as_raw().len());
            }
            return Ok(image);
        }
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES)
        {
            return Err("瓦片响应过大".into());
        }
        let (ttl, cacheable) = cache_lifetime(response.headers());
        let etag = header_text(response.headers(), ETAG);
        let modified = header_text(response.headers(), LAST_MODIFIED);
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "读取瓦片失败".to_string())?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("瓦片响应过大".into());
        }
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| "无法识别瓦片图片".to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(1024);
        limits.max_image_height = Some(1024);
        limits.max_alloc = Some(16 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader
            .decode()
            .map_err(|_| "地图服务未返回可解码图片".to_string())?
            .to_rgba8();
        if decoded.width() == 0 || decoded.height() == 0 {
            return Err("地图服务返回空图片".into());
        }
        let image = Arc::new(decoded);
        if let Some(previous) = self.cache.remove(&key) {
            self.cache_bytes = self
                .cache_bytes
                .saturating_sub(previous.image.as_raw().len());
        }
        if cacheable {
            self.cache_bytes += image.as_raw().len();
            self.cache.insert(
                key,
                CachedTile {
                    image: Arc::clone(&image),
                    expires: Instant::now() + ttl,
                    etag,
                    modified,
                    used: self.tick,
                },
            );
            while self.cache_bytes > MAX_CACHE_BYTES && self.cache.len() > 1 {
                let victim = self
                    .cache
                    .iter()
                    .min_by_key(|(_, tile)| tile.used)
                    .map(|(key, _)| *key);
                if let Some(victim) = victim
                    && let Some(removed) = self.cache.remove(&victim)
                {
                    self.cache_bytes = self
                        .cache_bytes
                        .saturating_sub(removed.image.as_raw().len());
                }
            }
        }
        Ok(image)
    }
}

fn header_text(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn cache_lifetime(headers: &HeaderMap) -> (Duration, bool) {
    let control = headers
        .get(CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let directives: Vec<_> = control
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();
    if directives.iter().any(|part| part == "no-store") {
        return (Duration::ZERO, false);
    }
    if directives.iter().any(|part| part == "no-cache") {
        return (Duration::ZERO, true);
    }
    let age = headers
        .get("age")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if let Some(max_age) = directives.iter().find_map(|part| {
        part.strip_prefix("max-age=")
            .and_then(|value| value.trim_matches('"').parse::<u64>().ok())
    }) {
        return (
            Duration::from_secs(max_age.saturating_sub(age).min(366 * 86400)),
            true,
        );
    }
    if let Some(expires) = headers
        .get(EXPIRES)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| chrono::DateTime::parse_from_rfc2822(value).ok())
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        return (
            Duration::from_secs((expires.timestamp() - now).clamp(0, 366 * 86400) as u64),
            true,
        );
    }
    // OSM's documented fallback when no usable freshness header is available.
    (Duration::from_secs(7 * 86400), true)
}

fn project(coordinates: Coordinates) -> (f64, f64) {
    let latitude = coordinates
        .lat
        .clamp(-MAX_LATITUDE, MAX_LATITUDE)
        .to_radians();
    let x = ((coordinates.lon + 180.0) / 360.0).rem_euclid(1.0);
    let y = (0.5
        - (std::f64::consts::FRAC_PI_4 + latitude / 2.0).tan().ln() / (2.0 * std::f64::consts::PI))
        .clamp(0.0, 1.0);
    (x, y)
}

fn wrap_delta(delta: f64) -> f64 {
    (delta + 0.5).rem_euclid(1.0) - 0.5
}

fn circle(image: &mut RgbaImage, x: i32, y: i32, radius: i32, color: Rgba<u8>) {
    for py in (y - radius).max(0)..=(y + radius).min(image.height() as i32 - 1) {
        for px in (x - radius).max(0)..=(x + radius).min(image.width() as i32 - 1) {
            if (px - x).pow(2) + (py - y).pow(2) <= radius.pow(2) {
                image.put_pixel(px as u32, py as u32, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(lat: f64, lon: f64, selected: bool, existing: bool) -> MapPoint {
        MapPoint {
            coordinates: Coordinates { lat, lon },
            selected,
            existing,
        }
    }

    #[test]
    fn current_markers_replace_old_points_without_mutating_cached_background() {
        let previous = point(30.0, 120.0, true, true);
        let current = point(30.02, 120.02, true, false);
        let view = MapView::fit(&[previous, current]);
        let size = [240, 160];
        let background = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            size[0],
            size[1],
            Rgba([41, 52, 63, 255]),
        ));
        let original = background.clone();
        let before = compose_points(Some(&background), &[previous], &view, size, (3, 7, 2));
        let after = compose_points(Some(&background), &[current], &view, size, (3, 7, 2));
        let old_pixel = view.pixel_position(previous.coordinates, size[0], size[1]);
        let new_pixel = view.pixel_position(current.coordinates, size[0], size[1]);
        let (old_x, old_y) = (old_pixel.0.round() as u32, old_pixel.1.round() as u32);
        let (new_x, new_y) = (new_pixel.0.round() as u32, new_pixel.1.round() as u32);
        assert_eq!(
            before.to_rgba8().get_pixel(old_x, old_y).0,
            [76, 220, 128, 255]
        );
        assert_eq!(
            after.to_rgba8().get_pixel(old_x, old_y).0,
            [41, 52, 63, 255]
        );
        assert_eq!(
            after.to_rgba8().get_pixel(new_x, new_y).0,
            [255, 209, 76, 255]
        );
        assert_eq!(background, original);
        let awaiting_tiles = compose_points(None, &[current], &view, size, (3, 7, 2));
        assert_eq!(
            awaiting_tiles.to_rgba8().get_pixel(new_x, new_y).0,
            [255, 209, 76, 255]
        );
    }

    #[test]
    fn background_identity_tracks_view_source_size_and_session() {
        let mut view = MapView::fit(&[point(30.0, 120.0, false, true)]);
        let key = background_key(&view, [600, 400], 0, 1);
        assert_ne!(key, background_key(&view, [600, 400], 0, 2));
        assert_ne!(key, background_key(&view, [600, 400], 1, 1));
        assert_ne!(key, background_key(&view, [601, 400], 0, 1));
        view.zoom_by(0.5);
        assert_ne!(key, background_key(&view, [600, 400], 0, 1));
    }

    #[test]
    fn dateline_fit_uses_the_short_arc() {
        let points = [
            point(10.0, 179.9, false, true),
            point(10.0, -179.9, true, false),
        ];
        let view = MapView::fit(&points);
        assert!(view.span_x < 0.01);
        for point in points {
            let (x, y) = view.pixel_position(point.coordinates, 600, 300);
            assert!((0.0..600.0).contains(&x));
            assert!((0.0..300.0).contains(&y));
        }
    }

    #[test]
    fn selected_point_is_drawn_above_coincident_points() {
        let points = [
            point(30.0, 120.0, true, false),
            point(30.0, 120.0, false, true),
        ];
        let result = MapRenderer::new().render(&points, &MapView::fit(&points), 120, 80, false, 0);
        let image = result.image.to_rgba8();
        assert_eq!(*image.get_pixel(60, 40), Rgba([255, 209, 76, 255]));
        assert_eq!(*image.get_pixel(66, 40), Rgba([255, 209, 76, 255]));
        assert!(result.attribution.is_empty());
    }

    #[test]
    fn mercator_handles_poles_without_non_finite_values() {
        for lat in [-90.0, 0.0, 90.0] {
            let (x, y) = project(Coordinates { lat, lon: 180.0 });
            assert!(x.is_finite() && y.is_finite());
            assert!((0.0..=1.0).contains(&y));
        }
    }

    fn scale_view(latitude: f64) -> MapView {
        MapView {
            center_x: 0.5,
            center_y: project(Coordinates {
                lat: latitude,
                lon: 0.0,
            })
            .1,
            span_x: 0.01,
            span_y: 0.001,
            zoom: 0.0,
        }
    }

    #[test]
    fn metric_scale_tracks_latitude_zoom_and_viewport_height() {
        let mut equator = scale_view(0.0);
        let normal = equator.scale(1000, 200, 100).unwrap();
        assert_eq!(
            (normal.cells, normal.meters, normal.label.as_str()),
            (12, 50_000.0, "50 km")
        );

        // The same Mercator pixels cover half as much ground at latitude 60°.
        let north = scale_view(60.0).scale(1000, 200, 100).unwrap();
        assert_eq!((north.cells, north.meters), (10, 20_000.0));
        equator.zoom_by(1.0);
        assert_eq!(equator.scale(1000, 200, 100).unwrap(), north);

        // A short viewport makes the north/south extent control the fit.
        let short = scale_view(0.0).scale(1000, 50, 100).unwrap();
        assert_eq!((short.cells, short.meters), (12, 100_000.0));
    }

    #[test]
    fn metric_scale_matches_native_halfblocks_and_graphics_pixels() {
        let view = scale_view(35.0);
        assert_eq!(view.scale(600, 240, 60), view.scale(60, 24, 60));
        assert!(view.scale(0, 24, 60).is_none());
        assert!(view.scale(60, 0, 60).is_none());
        assert!(view.scale(10, 24, 10).is_none());
    }

    #[test]
    fn metric_scale_uses_small_dateline_span_and_finite_polar_distance() {
        let date_line = MapView::fit(&[
            point(10.0, 179.9, false, true),
            point(10.0, -179.9, true, false),
        ]);
        let prime_meridian = MapView::fit(&[
            point(10.0, -0.1, false, true),
            point(10.0, 0.1, true, false),
        ]);
        assert_eq!(
            date_line.scale(600, 240, 60),
            prime_meridian.scale(600, 240, 60)
        );
        for latitude in [-90.0, 90.0] {
            let scale = scale_view(latitude).scale(600, 240, 60).unwrap();
            assert!(scale.meters.is_finite() && scale.meters > 0.0);
        }
    }

    #[test]
    fn metric_scale_has_readable_small_units_and_fits_its_line() {
        let mut view = scale_view(0.0);
        view.zoom_by(16.0);
        let scale = view.scale(1000, 200, 100).unwrap();
        assert_eq!(scale.label, "2 m");
        view.center_y = 0.0;
        let scale = view.scale(1000, 200, 100).unwrap();
        assert_eq!(scale.label, "0.2 m");
        for columns in 12..80 {
            let scale = view.scale(u32::from(columns) * 10, 200, columns).unwrap();
            assert!(usize::from(scale.cells) + 2 + scale.label.len() <= usize::from(columns));
        }
    }

    #[test]
    fn cache_headers_do_not_force_redownloads_or_ignore_no_store() {
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, "public, max-age=3600".parse().unwrap());
        headers.insert("age", "120".parse().unwrap());
        assert_eq!(cache_lifetime(&headers), (Duration::from_secs(3480), true));
        headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
        assert_eq!(cache_lifetime(&headers), (Duration::ZERO, false));
    }
}
