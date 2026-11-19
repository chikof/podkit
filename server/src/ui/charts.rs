//! Server-rendered SVG charts. No chart library and no script: a chart is a pure function
//! of its data (`line_chart`, `sparkline`), so the same input always gives the same
//! markup and every figure is escaped by maud.
//!
//! A chart is a `<figure>` holding
//!
//! - a caption with the title and the latest value of each series,
//! - an `<svg role="img">` with axis ticks, one path per run of readings (a missing
//!   reading, or a hole longer than three times the usual step, breaks the line instead
//!   of being bridged), and one invisible column per reading whose `<title>` is the hover
//!   value,
//! - a visually hidden summary the SVG points at with `aria-describedby`,
//! - a table of the readings, shown by the `_tables` signal that
//!   `components::metrics_section` declares (the "data table" toggle).
//!
//! Colors are the `chart-1` … `chart-5` tokens, so both color schemes are covered.

use std::fmt::Write as _;

use maud::{Markup, html};

use super::components::table;

// The plot fills the whole viewBox and is stretched to its box (`preserveAspectRatio="none"`);
// strokes are `non-scaling-stroke` and the axis labels are HTML, so nothing is distorted.
const WIDTH: f64 = 600.0;
const HEIGHT: f64 = 160.0;
const LEFT: f64 = 0.0;
const RIGHT: f64 = 0.0;
const TOP: f64 = 0.0;
const BOTTOM: f64 = 0.0;
/// Rows in the data table; the readings are thinned evenly to this many.
const TABLE_ROWS: usize = 30;
/// A hole wider than this many usual steps breaks the line.
const GAP_STEPS: i64 = 3;

/// What a series measures, which decides how its numbers read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
	/// `23.4%`.
	Percent,
	/// `1.5 GB`, binary multiples.
	Bytes,
	/// `1.5 MB/s`.
	BytesPerSec,
}

/// One line. `values` has one entry per timestamp of the chart; `None` is a missing
/// reading. `color` is the chart token, 1 to 5.
#[derive(Debug, Clone)]
pub struct Series<'a> {
	pub label: &'a str,
	pub color: u8,
	pub values: Vec<Option<f64>>,
}

/// Everything a chart needs. `from` and `to` are the window in unix seconds (the x axis);
/// `max` fixes the top of the y axis (100 for a percentage of a whole), else it follows the
/// data.
#[derive(Debug, Clone)]
pub struct Chart<'a> {
	/// A unique, id-safe name: `cpu`. The figure is `chart-<id>`.
	pub id: &'a str,
	pub title: &'a str,
	/// "last hour", for the summary.
	pub range: &'a str,
	pub unit: Unit,
	pub max: Option<f64>,
	pub from: i64,
	pub to: i64,
	pub xs: &'a [i64],
	pub series: Vec<Series<'a>>,
}

// ---- Numbers -------------------------------------------------------------------------

const SCALES: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

fn trim(value: f64, digits: usize) -> String {
	let s = format!("{value:.digits$}");
	if s.contains('.') {
		s.trim_end_matches('0').trim_end_matches('.').to_owned()
	} else {
		s
	}
}

/// A value as people read it: `23.4%`, `1.5 GB`, `3 MB/s`.
#[must_use]
pub fn format_value(unit: Unit, value: f64) -> String {
	match unit {
		Unit::Percent => format!("{}%", trim(value, 1)),
		Unit::Bytes => format_bytes(value),
		Unit::BytesPerSec => format!("{}/s", format_bytes(value)),
	}
}

fn format_bytes(value: f64) -> String {
	let mut v = value.max(0.0);
	let mut i = 0;
	while v >= 1024.0 && i + 1 < SCALES.len() {
		v /= 1024.0;
		i += 1;
	}
	let digits = usize::from(!(i == 0 || v >= 100.0));
	format!("{} {}", trim(v, digits), SCALES[i])
}

/// A round step at or above `raw`: 1, 2, 2.5, 5 or 10 times a power of ten.
fn round_step(raw: f64) -> f64 {
	if raw <= 0.0 || !raw.is_finite() {
		return 1.0;
	}
	let exp = raw.log10().floor();
	let base = 10f64.powf(exp);
	let m = raw / base;
	let nice = if m <= 1.0 {
		1.0
	} else if m <= 2.0 {
		2.0
	} else if m <= 2.5 {
		2.5
	} else if m <= 5.0 {
		5.0
	} else {
		10.0
	};
	nice * base
}

/// The top of the axis and the step between ticks, for data up to `max`.
fn axis(unit: Unit, max: f64, fixed: Option<f64>) -> (f64, f64) {
	if let Some(top) = fixed.filter(|t| *t > 0.0) {
		return (top, top / 4.0);
	}
	let max = if max.is_finite() && max > 0.0 {
		max
	} else {
		1.0
	};
	let step = match unit {
		Unit::Percent => round_step(max.max(10.0) / 4.0),
		// Bytes tick at powers of two, so the labels are 256 MB, 512 MB, 768 MB, 1 GB.
		Unit::Bytes | Unit::BytesPerSec => 2f64.powf((max / 4.0).max(1.0).log2().ceil()),
	};
	let floor = if unit == Unit::Percent { 10.0 } else { 0.0 };
	let top = (max.max(floor) / step).ceil().max(1.0) * step;
	(top, step)
}

fn num(v: f64) -> String {
	trim(v, 1)
}

// ---- Time ----------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
	"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn datetime(t: i64) -> Option<time::OffsetDateTime> {
	time::OffsetDateTime::from_unix_timestamp(t).ok()
}

/// `14:05`, or `Nov 18` for a window of more than a day and a half.
fn tick_label(t: i64, span: i64) -> String {
	let Some(d) = datetime(t) else {
		return String::new();
	};
	if span > 36 * 3600 {
		format!(
			"{} {}",
			MONTHS[usize::from(u8::from(d.month())) - 1],
			d.day()
		)
	} else {
		format!("{:02}:{:02}", d.hour(), d.minute())
	}
}

/// `2026-11-18 14:05 UTC`: hover values and table rows.
fn full_time(t: i64) -> String {
	datetime(t).map_or_else(String::new, |d| {
		format!(
			"{}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
			d.year(),
			u8::from(d.month()),
			d.day(),
			d.hour(),
			d.minute(),
			d.second()
		)
	})
}

// ---- Geometry ------------------------------------------------------------------------

struct Plot {
	from: i64,
	to: i64,
	top: f64,
}

impl Plot {
	fn x(&self, t: i64) -> f64 {
		let span = (self.to - self.from).max(1) as f64;
		let f = ((t - self.from) as f64 / span).clamp(0.0, 1.0);
		LEFT + f * (WIDTH - LEFT - RIGHT)
	}

	fn y(&self, v: f64) -> f64 {
		let f = (v / self.top).clamp(0.0, 1.0);
		TOP + (1.0 - f) * (HEIGHT - TOP - BOTTOM)
	}
}

/// The usual distance between two readings: the median step.
fn usual_step(xs: &[i64]) -> Option<i64> {
	let mut steps: Vec<i64> = xs
		.windows(2)
		.map(|w| w[1] - w[0])
		.filter(|d| *d > 0)
		.collect();
	if steps.is_empty() {
		return None;
	}
	steps.sort_unstable();
	Some(steps[steps.len() / 2])
}

/// Runs of consecutive readings: a missing value or a hole breaks the run. Each point is
/// `(time, value)`.
fn segments(xs: &[i64], values: &[Option<f64>]) -> Vec<Vec<(i64, f64)>> {
	let limit = usual_step(xs).map(|s| s * GAP_STEPS);
	let mut out: Vec<Vec<(i64, f64)>> = Vec::new();
	let mut open = false;
	let mut last_t = 0;
	for (t, v) in xs.iter().zip(values) {
		let Some(v) = v.filter(|v| v.is_finite()) else {
			open = false;
			continue;
		};
		let hole = limit.is_some_and(|l| *t - last_t > l);
		if !open || hole {
			out.push(Vec::new());
			open = true;
		}
		if let Some(run) = out.last_mut() {
			run.push((*t, v));
		}
		last_t = *t;
	}
	out
}

fn line_path(plot: &Plot, run: &[(i64, f64)]) -> String {
	let mut d = String::new();
	for (i, (t, v)) in run.iter().enumerate() {
		d.push_str(if i == 0 { "M" } else { "L" });
		let _ = write!(d, "{:.1} {:.1}", plot.x(*t), plot.y(*v));
		d.push(' ');
	}
	d.trim_end().to_owned()
}

fn area_path(plot: &Plot, run: &[(i64, f64)]) -> String {
	let base = HEIGHT - BOTTOM;
	let (first, last) = (run[0].0, run[run.len() - 1].0);
	format!(
		"{} L{:.1} {base:.1} L{:.1} {base:.1} Z",
		line_path(plot, run),
		plot.x(last),
		plot.x(first)
	)
}

// ---- Classes -------------------------------------------------------------------------

fn stroke_class(color: u8) -> &'static str {
	match color {
		1 => "stroke-chart-1",
		2 => "stroke-chart-2",
		3 => "stroke-chart-3",
		4 => "stroke-chart-4",
		_ => "stroke-chart-5",
	}
}

fn area_class(color: u8) -> &'static str {
	match color {
		1 => "fill-chart-1/15",
		2 => "fill-chart-2/15",
		3 => "fill-chart-3/15",
		4 => "fill-chart-4/15",
		_ => "fill-chart-5/15",
	}
}

fn dot_class(color: u8) -> &'static str {
	match color {
		1 => "fill-chart-1",
		2 => "fill-chart-2",
		3 => "fill-chart-3",
		4 => "fill-chart-4",
		_ => "fill-chart-5",
	}
}

fn swatch_class(color: u8) -> &'static str {
	match color {
		1 => "inline-block h-2 w-2 rounded-full bg-chart-1",
		2 => "inline-block h-2 w-2 rounded-full bg-chart-2",
		3 => "inline-block h-2 w-2 rounded-full bg-chart-3",
		4 => "inline-block h-2 w-2 rounded-full bg-chart-4",
		_ => "inline-block h-2 w-2 rounded-full bg-chart-5",
	}
}

// ---- Summary -------------------------------------------------------------------------

struct Stats {
	latest: f64,
	min: f64,
	max: f64,
	avg: f64,
	count: usize,
}

fn stats(values: &[Option<f64>]) -> Option<Stats> {
	let present: Vec<f64> = values
		.iter()
		.flatten()
		.copied()
		.filter(|v| v.is_finite())
		.collect();
	let latest = *present.last()?;
	let sum: f64 = present.iter().sum();
	Some(Stats {
		latest,
		min: present.iter().copied().fold(f64::INFINITY, f64::min),
		max: present.iter().copied().fold(f64::NEG_INFINITY, f64::max),
		avg: sum / present.len() as f64,
		count: present.len(),
	})
}

/// The sentence a screen reader gets in place of the picture.
fn summary(chart: &Chart<'_>) -> String {
	let mut out = format!("{}, {}.", chart.title, chart.range);
	for s in &chart.series {
		let name = if chart.series.len() > 1 { s.label } else { "" };
		match stats(&s.values) {
			None => {
				out.push_str(&format!(" {name} No readings.").replace("  ", " "));
			}
			Some(st) => {
				let f = |v: f64| format_value(chart.unit, v);
				out.push_str(
					&format!(
						" {name} {} readings. Latest {}, average {}, minimum {}, maximum {}.",
						st.count,
						f(st.latest),
						f(st.avg),
						f(st.min),
						f(st.max)
					)
					.replace("  ", " "),
				);
			}
		}
	}
	out
}

// ---- Rendering -----------------------------------------------------------------------

fn data_max(chart: &Chart<'_>) -> f64 {
	chart
		.series
		.iter()
		.flat_map(|s| s.values.iter().flatten().copied())
		.filter(|v| v.is_finite())
		.fold(0.0, f64::max)
}

fn has_data(chart: &Chart<'_>) -> bool {
	chart.series.iter().any(|s| stats(&s.values).is_some())
}

/// Indices of at most `TABLE_ROWS` readings spread over the window, the newest last.
fn table_indices(len: usize) -> Vec<usize> {
	if len <= TABLE_ROWS {
		return (0..len).collect();
	}
	let mut out: Vec<usize> = (0..TABLE_ROWS)
		.map(|i| i * (len - 1) / (TABLE_ROWS - 1))
		.collect();
	out.dedup();
	out
}

fn data_table(chart: &Chart<'_>, figure_id: &str) -> Markup {
	let idx = table_indices(chart.xs.len());
	let mut headers: Vec<&str> = vec!["Time"];
	headers.extend(chart.series.iter().map(|s| s.label));
	let rows: Vec<Vec<Markup>> = idx
		.iter()
		.rev()
		.map(|&i| {
			let mut row = vec![html! { (full_time(chart.xs[i])) }];
			for s in &chart.series {
				let cell = s
					.values
					.get(i)
					.copied()
					.flatten()
					.map_or_else(|| "–".to_owned(), |v| format_value(chart.unit, v));
				row.push(html! { (cell) });
			}
			row
		})
		.collect();
	let note = if chart.xs.len() > idx.len() {
		format!("Showing {} of {} readings.", idx.len(), chart.xs.len())
	} else {
		format!("{} readings.", chart.xs.len())
	};
	html! {
		div class="mt-3 flex flex-col gap-2" "data-show"="$_tables" style="display: none" {
			(table(&format!("{figure_id}-data"), &format!("{} readings", chart.title), &headers, &rows))
			p class="text-xs text-muted-foreground" { (note) }
		}
	}
}

fn svg(chart: &Chart<'_>, figure_id: &str) -> Markup {
	let (top, step) = axis(chart.unit, data_max(chart), chart.max);
	let plot = Plot {
		from: chart.from,
		to: chart.to,
		top,
	};
	// The axis ends at its last tick: four steps for a fixed top, as many as the data needs
	// otherwise.
	#[allow(
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "clamped to 1..=8"
	)]
	let tick_count = (top / step).round().clamp(1.0, 8.0) as u8;
	let ticks: Vec<f64> = (0..=tick_count)
		.map(|i| top * f64::from(i) / f64::from(tick_count))
		.collect();
	let span = chart.to - chart.from;
	let x_ticks: Vec<i64> = (0..=4i64).map(|i| chart.from + span * i / 4).collect();
	let base = HEIGHT - BOTTOM;
	let plot_width = WIDTH - LEFT - RIGHT;
	// A hover column is one usual step wide (or the plot split evenly when that is unknown).
	let col = usual_step(chart.xs)
		.map_or(plot_width / chart.xs.len().max(1) as f64, |s| {
			s as f64 / span.max(1) as f64 * plot_width
		})
		.clamp(1.0, plot_width);
	html! {
		div class="ui-chart" {
			div class="ui-chart-plot" {
				svg viewBox={ "0 0 " (WIDTH) " " (HEIGHT) } preserveAspectRatio="none" role="img"
					aria-labelledby={ (figure_id) "-title" } aria-describedby={ (figure_id) "-summary" } {
					@for tick in &ticks {
						line x1=(LEFT) x2=(WIDTH - RIGHT) y1=(num(plot.y(*tick))) y2=(num(plot.y(*tick))) class="ui-chart-grid" {}
					}
					line x1=(LEFT) x2=(WIDTH - RIGHT) y1=(base) y2=(base) class="ui-chart-grid" {}
					@for s in &chart.series {
						@for run in segments(chart.xs, &s.values) {
							@if run.len() > 1 {
								path d=(area_path(&plot, &run)) class={ "ui-chart-area " (area_class(s.color)) } stroke="none" {}
								path d=(line_path(&plot, &run)) class=(stroke_class(s.color)) fill="none" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" {}
							} @else {
								circle cx=(num(plot.x(run[0].0))) cy=(num(plot.y(run[0].1))) r="2" class=(dot_class(s.color)) {}
							}
						}
					}
					@for (i, t) in chart.xs.iter().enumerate() {
						@let left = (plot.x(*t) - col / 2.0).max(LEFT);
						@let right = (plot.x(*t) + col / 2.0).min(WIDTH - RIGHT);
						rect x=(num(left)) y=(TOP) width=(num((right - left).max(1.0))) height=(base - TOP)
							class="fill-transparent hover:fill-foreground/10" {
							title { (hover_text(chart, i)) }
						}
					}
				}
				@for tick in &ticks {
					span class="ui-chart-yl" style={ "--p:" (num((1.0 - tick / top) * 100.0)) } aria-hidden="true" { (format_value(chart.unit, *tick)) }
				}
			}
			div class="ui-chart-xl" aria-hidden="true" {
				@for t in &x_ticks {
					span { (tick_label(*t, span)) }
				}
			}
		}
	}
}

fn hover_text(chart: &Chart<'_>, i: usize) -> String {
	let mut out = full_time(chart.xs[i]);
	for s in &chart.series {
		let v = s
			.values
			.get(i)
			.copied()
			.flatten()
			.map_or_else(|| "no reading".to_owned(), |v| format_value(chart.unit, v));
		let _ = write!(out, " · {}: {v}", s.label);
	}
	out
}

/// One chart as a `<figure id="chart-<id>">`. See the module docs.
#[must_use]
pub fn line_chart(chart: &Chart<'_>) -> Markup {
	let figure_id = format!("chart-{}", chart.id);
	html! {
		figure id=(figure_id) class="flex flex-col gap-2" {
			figcaption class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1" {
				h3 id={ (figure_id) "-title" } class="text-sm font-medium" { (chart.title) }
				div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground" {
					@for s in &chart.series {
						span class="inline-flex items-center gap-1.5" {
							span class=(swatch_class(s.color)) aria-hidden="true" {}
							(s.label)
							@if let Some(st) = stats(&s.values) {
								span class="font-medium text-foreground" { (format_value(chart.unit, st.latest)) }
							}
						}
					}
				}
			}
			@if has_data(chart) {
				(svg(chart, &figure_id))
				p id={ (figure_id) "-summary" } class="ui-visually-hidden" { (summary(chart)) }
				(data_table(chart, &figure_id))
			} @else {
				p id={ (figure_id) "-summary" } class="flex h-32 items-center justify-center rounded-lg border border-dashed border-border px-4 text-center text-sm text-muted-foreground" {
					"No readings in this range yet. podkit samples every 30 seconds."
				}
			}
		}
	}
}

/// A small decorative line for a card: no axes, hidden from assistive technology (the card
/// carries the number). `max` fixes the top, as `Chart::max` does.
#[must_use]
pub fn sparkline(
	xs: &[i64],
	values: &[Option<f64>],
	from: i64,
	to: i64,
	max: Option<f64>,
) -> Markup {
	const W: f64 = 120.0;
	const H: f64 = 32.0;
	let top = max.filter(|m| *m > 0.0).unwrap_or_else(|| {
		let m = values.iter().flatten().copied().fold(0.0, f64::max);
		if m > 0.0 { m } else { 1.0 }
	});
	let span = (to - from).max(1) as f64;
	let point = |t: i64, v: f64| {
		let x = ((t - from) as f64 / span).clamp(0.0, 1.0) * W;
		let y = 2.0 + (1.0 - (v / top).clamp(0.0, 1.0)) * (H - 4.0);
		format!("{x:.1} {y:.1}")
	};
	let runs = segments(xs, values);
	html! {
		svg viewBox={ "0 0 " (W) " " (H) } class="h-8 w-full" aria-hidden="true" focusable="false" preserveAspectRatio="none" {
			@for run in &runs {
				@if run.len() > 1 {
					path d=(run.iter().enumerate().map(|(i, (t, v))| format!("{}{}", if i == 0 { "M" } else { "L" }, point(*t, *v))).collect::<Vec<_>>().join(" "))
						class="stroke-chart-2" fill="none" stroke-width="1.5" stroke-linejoin="round" vector-effect="non-scaling-stroke" {}
				}
			}
		}
	}
}

/// The `/_ui` demo: a line with a gap, two lines, and the empty state, with the data-table
/// toggle. `prefix` keeps the figure ids unique across the gallery's two panels.
pub fn demo(prefix: &str) -> Markup {
	const FROM: i64 = 1_700_000_000;
	let xs: Vec<i64> = (0..60).map(|i| FROM + i * 60).collect();
	// Readings stop for ten minutes in the middle: the line breaks instead of bridging.
	let cpu: Vec<Option<f64>> = (0..60)
		.map(|i| (!(25..35).contains(&i)).then(|| 30.0 + 25.0 * (f64::from(i) / 6.0).sin()))
		.collect();
	let rx: Vec<Option<f64>> = (0..60)
		.map(|i| Some(f64::from(i % 20) * 90_000.0))
		.collect();
	let tx: Vec<Option<f64>> = (0..60).map(|i| Some(f64::from(i % 7) * 20_000.0)).collect();
	let ids = (
		format!("{prefix}-cpu"),
		format!("{prefix}-net"),
		format!("{prefix}-none"),
	);
	let cpu_chart = Chart {
		id: &ids.0,
		title: "CPU",
		range: "last hour",
		unit: Unit::Percent,
		max: Some(100.0),
		from: FROM,
		to: FROM + 3600,
		xs: &xs,
		series: vec![Series {
			label: "CPU",
			color: 2,
			values: cpu,
		}],
	};
	let net_chart = Chart {
		id: &ids.1,
		title: "Network",
		unit: Unit::BytesPerSec,
		max: None,
		series: vec![
			Series {
				label: "Received",
				color: 1,
				values: rx,
			},
			Series {
				label: "Sent",
				color: 5,
				values: tx,
			},
		],
		..cpu_chart.clone()
	};
	let empty_chart = Chart {
		id: &ids.2,
		title: "GPU",
		xs: &[],
		series: vec![Series {
			label: "Utilisation",
			color: 2,
			values: Vec::new(),
		}],
		..cpu_chart.clone()
	};
	html! {
		div class="flex flex-col gap-6" "data-signals"="{_tables: false}" {
			div {
				button type="button" class="ui-button inline-flex h-8 cursor-pointer items-center justify-center rounded-md border border-input bg-background px-3 text-sm font-medium text-foreground shadow-xs hover:bg-accent" aria-pressed="false"
					"data-on:click"="$_tables = !$_tables" "data-attr:aria-pressed"="$_tables ? 'true' : 'false'" { "Data tables" }
			}
			(line_chart(&cpu_chart))
			(line_chart(&net_chart))
			(line_chart(&empty_chart))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const FROM: i64 = 1_700_000_000;

	fn one(xs: &[i64], values: Vec<Option<f64>>, unit: Unit, max: Option<f64>) -> Chart<'_> {
		Chart {
			id: "cpu",
			title: "CPU",
			range: "last hour",
			unit,
			max,
			from: FROM,
			to: FROM + 3600,
			xs,
			series: vec![Series {
				label: "CPU",
				color: 2,
				values,
			}],
		}
	}

	fn render(chart: &Chart<'_>) -> String {
		line_chart(chart).into_string()
	}

	fn minutes(n: i64) -> Vec<i64> {
		(0..n).map(|i| FROM + i * 60).collect()
	}

	#[test]
	fn charts_values_are_formatted_by_unit() {
		assert_eq!(format_value(Unit::Percent, 23.44), "23.4%");
		assert_eq!(format_value(Unit::Percent, 50.0), "50%");
		assert_eq!(format_value(Unit::Bytes, 0.0), "0 B");
		assert_eq!(format_value(Unit::Bytes, 1536.0), "1.5 KB");
		assert_eq!(
			format_value(Unit::Bytes, 8.0 * 1024.0 * 1024.0 * 1024.0),
			"8 GB"
		);
		assert_eq!(format_value(Unit::BytesPerSec, 2048.0), "2 KB/s");
	}

	#[test]
	fn charts_scale_a_percentage_to_a_fixed_axis_and_other_units_to_round_ticks() {
		assert_eq!(axis(Unit::Percent, 37.0, Some(100.0)), (100.0, 25.0));
		// Follows the data, in round steps, never below 10 for a percentage.
		assert_eq!(axis(Unit::Percent, 0.0, None), (10.0, 2.5));
		assert_eq!(axis(Unit::Percent, 180.0, None), (200.0, 50.0));
		// Bytes tick at binary multiples: 0, 256, 512, 768, 1024 MB for 900 MB.
		let (top, step) = axis(Unit::Bytes, 900.0 * 1024.0 * 1024.0, None);
		assert_eq!(top, 1024.0 * 1024.0 * 1024.0);
		assert_eq!(step, 256.0 * 1024.0 * 1024.0);
		// All zero or garbage never divides by zero.
		assert!(axis(Unit::Bytes, 0.0, None).0 > 0.0);
		assert!(axis(Unit::Bytes, f64::NAN, None).0 > 0.0);
	}

	#[test]
	fn charts_place_points_inside_the_plot_and_put_larger_values_higher() {
		let plot = Plot {
			from: FROM,
			to: FROM + 100,
			top: 100.0,
		};
		assert!((plot.x(FROM) - LEFT).abs() < 1e-9);
		assert!((plot.x(FROM + 100) - (WIDTH - RIGHT)).abs() < 1e-9);
		// Outside the window is clamped, not drawn off the canvas.
		assert!((plot.x(FROM - 50) - LEFT).abs() < 1e-9);
		assert!((plot.y(0.0) - (HEIGHT - BOTTOM)).abs() < 1e-9);
		assert!((plot.y(100.0) - TOP).abs() < 1e-9);
		assert!((plot.y(500.0) - TOP).abs() < 1e-9);
		assert!(plot.y(80.0) < plot.y(20.0));
	}

	#[test]
	fn charts_with_no_readings_say_so_instead_of_drawing_an_empty_plot() {
		let xs: Vec<i64> = Vec::new();
		let html = render(&one(&xs, vec![], Unit::Percent, Some(100.0)));
		assert!(html.contains("No readings in this range yet"), "{html}");
		assert!(!html.contains("<svg"), "{html}");
		assert!(html.contains("id=\"chart-cpu\""), "{html}");

		// Readings that are all missing are empty too.
		let xs = minutes(3);
		let html = render(&one(
			&xs,
			vec![None, None, None],
			Unit::Percent,
			Some(100.0),
		));
		assert!(!html.contains("<svg"), "{html}");
	}

	#[test]
	fn charts_with_a_single_reading_draw_a_dot_not_a_line() {
		let xs = minutes(1);
		let html = render(&one(&xs, vec![Some(40.0)], Unit::Percent, Some(100.0)));
		assert!(html.contains("<circle"), "{html}");
		assert!(!html.contains("<path"), "{html}");
	}

	#[test]
	fn charts_break_the_line_at_a_missing_reading_and_at_a_long_hole() {
		let xs = minutes(6);
		let vals = vec![Some(1.0), Some(2.0), None, Some(3.0), Some(4.0), Some(5.0)];
		let runs = segments(&xs, &vals);
		assert_eq!(runs.len(), 2);
		assert_eq!(runs[0].len(), 2);
		assert_eq!(runs[1].len(), 3);

		// Readings every minute, then nothing for ten minutes: two runs, not a bridge.
		let mut xs = minutes(5);
		xs.extend((15..20).map(|i| FROM + i * 60));
		let vals = vec![Some(1.0); 10];
		let runs = segments(&xs, &vals);
		assert_eq!(runs.len(), 2, "{runs:?}");

		// Every run is one `M`; a bridged line would have a single `M` for all ten.
		let html = render(&one(&xs, vals, Unit::Percent, Some(100.0)));
		let lines = html.matches("stroke-linejoin").count();
		assert_eq!(lines, 2, "{html}");
	}

	#[test]
	fn charts_with_300_points_stay_complete_and_bounded() {
		let xs: Vec<i64> = (0..300).map(|i| FROM + i * 12).collect();
		let vals: Vec<Option<f64>> = (0..300).map(|i| Some(f64::from(i % 100))).collect();
		let chart = Chart {
			to: FROM + 3600,
			..one(&xs, vals, Unit::Percent, Some(100.0))
		};
		let html = render(&chart);
		// One hover column per reading, each with its own value.
		assert_eq!(html.matches("<rect").count(), 300);
		assert_eq!(html.matches("<title>").count(), 300);
		// The table is thinned, and says so.
		assert_eq!(html.matches("<tr>").count(), TABLE_ROWS + 1);
		assert!(html.contains("Showing 30 of 300 readings."), "{html}");
		// Five y ticks, five x ticks, plus the baseline: a fixed number of lines.
		assert_eq!(html.matches("<line").count(), 6);
		assert!(html.len() < 120_000, "{}", html.len());
		assert_eq!(table_indices(300).len(), 30);
		assert_eq!(table_indices(300)[29], 299);
		assert_eq!(table_indices(5), vec![0, 1, 2, 3, 4]);
	}

	#[test]
	fn charts_are_described_for_assistive_technology() {
		let xs = minutes(4);
		let vals = vec![Some(10.0), Some(20.0), Some(30.0), Some(40.0)];
		let html = render(&one(&xs, vals, Unit::Percent, Some(100.0)));
		assert!(html.contains("role=\"img\""), "{html}");
		assert!(
			html.contains("aria-labelledby=\"chart-cpu-title\""),
			"{html}"
		);
		assert!(
			html.contains("aria-describedby=\"chart-cpu-summary\""),
			"{html}"
		);
		assert!(html.contains("id=\"chart-cpu-summary\""), "{html}");
		assert!(
			html.contains("4 readings. Latest 40%, average 25%, minimum 10%, maximum 40%."),
			"{html}"
		);
		// The numbers are reachable without the picture: a table behind the toggle.
		assert!(html.contains("data-show=\"$_tables\""), "{html}");
		assert!(html.contains("<th scope=\"col\">Time</th>"), "{html}");
		assert!(html.contains("2023-11-14 22:13:20 UTC"), "{html}");
		// Hover values.
		assert!(
			html.contains("<title>2023-11-14 22:13:20 UTC · CPU: 10%</title>"),
			"{html}"
		);
	}

	#[test]
	fn charts_escape_every_label_and_carry_no_script() {
		let xs = minutes(3);
		let chart = Chart {
			id: "x",
			title: "<script>alert(1)</script>",
			range: "<img src=x onerror=alert(1)>",
			series: vec![
				Series {
					label: "<b onmouseover=1>rx</b>",
					color: 1,
					values: vec![Some(1.0), Some(2.0), Some(3.0)],
				},
				Series {
					label: "\"><svg onload=1>",
					color: 9,
					values: vec![Some(3.0), None, Some(1.0)],
				},
			],
			..one(&xs, vec![], Unit::BytesPerSec, None)
		};
		let html = render(&chart);
		assert!(!html.contains("<script"), "{html}");
		assert!(!html.contains("<img"), "{html}");
		assert!(!html.contains("<b "), "{html}");
		assert!(!html.contains("<svg onload"), "{html}");
		assert!(!html.contains("javascript:"), "{html}");
		assert!(
			html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
			"{html}"
		);
		// A color outside 1 to 5 falls back to a token, never to anything else.
		assert!(html.contains("fill-chart-5"), "{html}");
		let spark = sparkline(
			&xs,
			&[Some(1.0), Some(2.0), Some(3.0)],
			FROM,
			FROM + 120,
			None,
		)
		.into_string();
		assert!(!spark.contains("<script"), "{spark}");
		assert!(spark.contains("aria-hidden=\"true\""), "{spark}");
	}

	#[test]
	fn charts_label_multiple_series_in_the_legend_and_the_table() {
		let xs = minutes(3);
		let chart = Chart {
			series: vec![
				Series {
					label: "Received",
					color: 1,
					values: vec![Some(1024.0), Some(2048.0), Some(4096.0)],
				},
				Series {
					label: "Sent",
					color: 3,
					values: vec![Some(10.0), None, Some(30.0)],
				},
			],
			..one(&xs, vec![], Unit::BytesPerSec, None)
		};
		let html = render(&chart);
		assert!(html.contains("Received") && html.contains("Sent"), "{html}");
		assert!(html.contains("4 KB/s"), "{html}");
		assert!(html.contains("no reading"), "{html}");
		assert!(html.contains("–"), "{html}");
	}

	#[test]
	fn charts_ticks_read_as_clock_times_for_a_day_and_dates_for_a_week() {
		assert_eq!(tick_label(FROM, 3600), "22:13");
		assert_eq!(tick_label(FROM, 7 * 86_400), "Nov 14");
		assert_eq!(full_time(FROM), "2023-11-14 22:13:20 UTC");
	}

	#[test]
	fn charts_sparklines_are_decoration_with_no_axes() {
		let xs = minutes(4);
		let html = sparkline(
			&xs,
			&[Some(1.0), Some(5.0), None, Some(2.0)],
			FROM,
			FROM + 240,
			Some(100.0),
		)
		.into_string();
		assert!(html.contains("aria-hidden=\"true\""), "{html}");
		assert!(!html.contains("<text"), "{html}");
		assert!(!html.contains("<title"), "{html}");
	}
}
