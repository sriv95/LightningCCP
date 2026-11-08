//! Port of the plot tests of `tests/test_cogs.py` (`test_plot_group_commands`, `test_rating_plot_renders_png`) and the
//! data-level parity checks of `src/ui/plots.py`. Expected values come from running the Python `rating_plot`
//! with `Figure.savefig` wrapped to read the axes back (matplotlib 3.11.2); x limits are matplotlib days times 86400.
use lightningccp::ui::plots::{self, CF_BANDS, Line, SIZE, plan, rating_plot};

fn lines(spec: &[(&str, &[(i64, i64)])]) -> Vec<(String, Vec<(i64, i64)>)> {
    spec.iter().map(|(l, p)| (l.to_string(), p.to_vec())).collect()
}

fn line(label: &str, points: &[(i64, i64)], colour: &str) -> Line {
    Line { label: label.into(), points: points.to_vec(), colour: colour.into() }
}

#[test]
fn single_line_over_bands_is_black_without_legend() {
    let pts = [(1_700_000_000, 1500), (1_700_100_000, 1450), (1_700_300_000, 1620)];
    let p = plan("Codeforces rating - tourist", &lines(&[("tourist", &pts)]), &CF_BANDS);
    assert_eq!(p.title, "Codeforces rating - tourist");
    assert_eq!(p.lines, [line("tourist", &pts, "#000000")]);
    assert_eq!(p.legend, None);
    assert_eq!((p.x_range, p.y_range), ((1699985000.0, 1700315000.0), (1350, 1720)));
    assert_eq!(p.bands.len(), 10); // axhspan order, from Python's patches
    assert_eq!(p.bands[0], (0, 1200, "#cccccc".to_string()));
    assert_eq!(p.bands[9], (3400, 4000, "#aa0000".to_string()));
    assert_eq!((plots::BAND_ALPHA, plots::GRID_ALPHA, plots::MARKER_SIZE, plots::LINE_WIDTH), (0.45, 0.3, 4, 1.5));
}

#[test]
fn several_lines_use_the_default_cycle_and_a_legend() {
    let (a, b) = ([(1_700_000_000, 800), (1_700_100_000, 900)], [(1_700_050_000, 750)]);
    let p = plan("Match rating", &lines(&[("a", &a), ("b", &b)]), &[]);
    assert_eq!(p.lines, [line("a", &a, "#1f77b4"), line("b", &b, "#ff7f0e")]);
    assert_eq!(p.legend, Some(vec!["a".to_string(), "b".to_string()]));
    assert!(p.bands.is_empty());
    assert_eq!((p.x_range, p.y_range), ((1699995000.0, 1700105000.0), (650, 1000)));
}

#[test]
fn one_point_widens_the_x_axis_like_matplotlib() {
    let pts = [(1_700_000_000, 800)];
    let p = plan("T", &lines(&[("a", &pts)]), &[]);
    assert_eq!((p.x_range, p.y_range), ((1630620800.0, 1769379200.0), (700, 900)));
    assert_eq!(p.lines[0].colour, "#000000");
}

#[test]
fn render_is_a_png_of_the_figure_size() {
    assert_eq!(SIZE, (900, 500)); // figsize (9, 5) at dpi 100
    let a = lines(&[("a", &[(1_700_000_000, 1500), (1_700_100_000, 1450)])]);
    let two = lines(&[("a", &[(1_700_000_000, 800), (1_700_100_000, 900)]), ("b", &[(1_700_050_000, 750)])]);
    let one = lines(&[("a", &[(1_700_000_000, 800)])]);
    for (l, bands) in [(&a, &CF_BANDS[..]), (&two, &[]), (&one, &[])] {
        let png = rating_plot("T", l, bands).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(png.as_slice());
        let mut reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (900, 500));
        let mut pixels = vec![0; reader.output_buffer_size()];
        reader.next_frame(&mut pixels).unwrap();
        assert!(pixels.iter().any(|&b| b != 255)); // something is drawn
    }
}
