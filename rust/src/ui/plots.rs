//! Port of src/ui/plots.py: PNG line charts of rating histories. `plan` is the pure data step (series, axes,
//! title, legend, colours); `render` draws it with plotters and encodes a PNG. Compared with matplotlib by
//! data, not pixels.
use std::sync::OnceLock;

use plotters::prelude::*;

/// (lowest rating, highest rating, colour) of the Codeforces rank bands.
pub const CF_BANDS: [(i64, i64, &str); 10] = [
    (0, 1200, "#cccccc"),
    (1200, 1400, "#77ff77"),
    (1400, 1600, "#77ddbb"),
    (1600, 1900, "#aaaaff"),
    (1900, 2100, "#ff88ff"),
    (2100, 2400, "#ffcc88"),
    (2400, 2600, "#ffbb55"),
    (2600, 3000, "#ff7777"),
    (3000, 3400, "#ff3333"),
    (3400, 4000, "#aa0000"),
];

/// `Figure(figsize=(9, 5), dpi=100)`
pub const SIZE: (u32, u32) = (900, 500);
pub const BAND_ALPHA: f64 = 0.45;
pub const GRID_ALPHA: f64 = 0.3;
pub const MARKER_SIZE: u32 = 4; // matplotlib markersize 4 (marker "o")
pub const LINE_WIDTH: f64 = 1.5;
const SINGLE_COLOUR: &str = "#000000";
/// matplotlib's default colour cycle (tab10), used when there are several lines.
const CYCLE: [&str; 10] = [
    "#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#7f7f7f", "#bcbd22", "#17becf",
];
/// matplotlib 3.11 `DAYS_PER_YEAR`; a single-point x axis spans two of them each side before the margin.
const SINGULAR_HALF_SPAN: f64 = 2.0 * 365.0 * 86400.0;
const MARGIN: f64 = 0.05; // matplotlib's default axes margin

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub label: String,
    /// (unix seconds, rating) in the given order; matplotlib plots datetimes, here the x value is unix seconds.
    pub points: Vec<(i64, i64)>,
    pub colour: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub title: String,
    /// Drawn first, in order, as horizontal spans over the whole x range at `BAND_ALPHA`.
    pub bands: Vec<(i64, i64, String)>,
    pub lines: Vec<Line>,
    /// x axis limits in unix seconds (data range plus the 5% margin).
    pub x_range: (f64, f64),
    pub y_range: (i64, i64),
    /// Legend entries in order; only with more than one line.
    pub legend: Option<Vec<String>>,
}

/// The data step of `rating_plot`. `lines` must hold at least one point overall (Python `min()` raises on none).
pub fn plan(title: &str, lines: &[(String, Vec<(i64, i64)>)], bands: &[(i64, i64, &str)]) -> Plan {
    let ratings = lines.iter().flat_map(|(_, p)| p.iter().map(|&(_, r)| r));
    let (low, high) = (ratings.clone().min().unwrap() - 100, ratings.max().unwrap() + 100);
    let times = lines.iter().flat_map(|(_, p)| p.iter().map(|&(t, _)| t as f64));
    let (mut lo, mut hi) = times.fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(t), b.max(t)));
    if lo == hi {
        (lo, hi) = (lo - SINGULAR_HALF_SPAN, hi + SINGULAR_HALF_SPAN);
    }
    let pad = (hi - lo) * MARGIN;
    let several = lines.len() > 1;
    Plan {
        title: title.to_string(),
        bands: bands.iter().map(|&(a, b, c)| (a, b, c.to_string())).collect(),
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, (label, points))| Line {
                label: label.clone(),
                points: points.clone(),
                colour: if several { CYCLE[i % CYCLE.len()] } else { SINGLE_COLOUR }.to_string(),
            })
            .collect(),
        x_range: (lo - pad, hi + pad),
        y_range: (low, high),
        legend: several.then(|| lines.iter().map(|(l, _)| l.clone()).collect()),
    }
}

const FONT: &str = "dejavu-sans";

fn register_font() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let bytes: &'static [u8] = include_bytes!("../../assets/DejaVuSans.ttf");
        assert!(plotters::style::register_font(FONT, FontStyle::Normal, bytes).is_ok(), "embedded font is valid");
    });
}

fn rgb(hex: &str) -> RGBColor {
    let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("hex colour");
    RGBColor((n >> 16) as u8, (n >> 8) as u8, n as u8)
}

fn date(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// The render step: a PNG of `SIZE` pixels.
pub fn render(plan: &Plan) -> anyhow::Result<Vec<u8>> {
    register_font();
    let (w, h) = SIZE;
    let mut buf = vec![0u8; (w * h * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buf, SIZE).into_drawing_area();
        root.fill(&WHITE)?;
        let (x0, x1) = plan.x_range;
        let (y0, y1) = (plan.y_range.0 as f64, plan.y_range.1 as f64);
        let mut chart = ChartBuilder::on(&root)
            .caption(&plan.title, (FONT, 17).into_font())
            .margin(12)
            .x_label_area_size(36)
            .y_label_area_size(56)
            .build_cartesian_2d(x0..x1, y0..y1)?;
        chart
            .configure_mesh()
            .light_line_style(BLACK.mix(GRID_ALPHA))
            .bold_line_style(BLACK.mix(GRID_ALPHA))
            .x_labels(6)
            .x_label_formatter(&|t| date(*t))
            .y_label_formatter(&|r| format!("{r:.0}"))
            .label_style((FONT, 14))
            .draw()?;
        for (low, high, colour) in &plan.bands {
            let (a, b) = ((*low as f64).max(y0), (*high as f64).min(y1)); // clip to the axes
            if a < b {
                chart.draw_series([Rectangle::new([(x0, a), (x1, b)], rgb(colour).mix(BAND_ALPHA).filled())])?;
            }
        }
        for line in &plan.lines {
            let colour = rgb(&line.colour);
            let points: Vec<(f64, f64)> = line.points.iter().map(|&(t, r)| (t as f64, r as f64)).collect();
            let series = chart.draw_series(LineSeries::new(points, colour.stroke_width(2)).point_size(MARKER_SIZE))?;
            if plan.legend.is_some() {
                series.label(&line.label).legend(move |(x, y)| PathElement::new([(x, y), (x + 20, y)], colour.stroke_width(2)));
            }
        }
        if plan.legend.is_some() {
            chart
                .configure_series_labels()
                .position(SeriesLabelPosition::UpperLeft)
                .background_style(WHITE.mix(0.8))
                .border_style(BLACK.mix(0.5))
                .label_font((FONT, 14))
                .draw()?;
        }
        root.present()?;
    }
    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&buf)?;
    Ok(png)
}

/// Python `rating_plot(title, lines, bands)`.
pub fn rating_plot(title: &str, lines: &[(String, Vec<(i64, i64)>)], bands: &[(i64, i64, &str)]) -> anyhow::Result<Vec<u8>> {
    render(&plan(title, lines, bands))
}
