use std::rc::Rc;

use chrono::{DateTime, Utc};
use gloo_net::http::Request;
use gloo_timers::callback::Interval;
use leptos::{mount::mount_to_body, prelude::*};
use leptos_chartistry::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

const REFRESH_MS: u32 = 120_000;
const Y_AXIS_MIN_CHARS: usize = 7;
const PLOT_LEFT_INSET_PX: f64 = 90.0;
const PLOT_TOP_INSET_PX: f64 = 0.0;
const PLOT_BOTTOM_INSET_PX: f64 = 0.0;

#[derive(Clone, Debug, Deserialize)]
struct DashboardData {
    generated_at: String,
    cache_status: CacheStatus,
    source: SourceInfo,
    stations: Vec<StationData>,
    warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CacheStatus {
    Fresh,
    Stale,
}

#[derive(Clone, Debug, Deserialize)]
struct SourceInfo {
    label: String,
    url: String,
}

#[derive(Clone, Debug, Deserialize)]
struct StationData {
    id: String,
    slug: String,
    name_fr: String,
    name_en: String,
    role_fr: String,
    role_en: String,
    kind: WaterKind,
    current: Vec<CurrentMetric>,
    history: Vec<MetricSeries>,
    forecast: Vec<MetricSeries>,
    status: StationStatus,
    notice_fr: Option<String>,
    notice_en: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WaterKind {
    River,
    Lake,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StationStatus {
    Complete,
    Partial,
    Missing,
}

#[derive(Clone, Debug, Deserialize)]
struct CurrentMetric {
    kind: MetricKind,
    value: f64,
    unit: String,
    measured_at: String,
    range_24h: Option<MetricRange>,
}

#[derive(Clone, Debug, Deserialize)]
struct MetricRange {
    min: f64,
    max: f64,
    mean: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MetricKind {
    Discharge,
    WaterLevel,
    Temperature,
}

#[derive(Clone, Debug, Deserialize)]
struct MetricSeries {
    kind: MetricKind,
    label_fr: String,
    label_en: String,
    unit: String,
    points: Vec<HistoryPoint>,
    #[serde(default)]
    uncertainty: Option<MetricUncertainty>,
}

#[derive(Clone, Debug, Deserialize)]
struct MetricUncertainty {
    lower: f64,
    upper: f64,
    confidence: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoryPoint {
    timestamp: String,
    value: f64,
}

#[derive(Clone, Debug)]
struct ChartPoint {
    x: f64,
    history_y: f64,
    forecast_y: f64,
    timestamp: String,
}

#[derive(Clone, Debug, PartialEq)]
struct HoverMetric {
    value: f64,
    unit: String,
    timestamp: String,
    is_forecast: bool,
    cursor_ratio: f64,
    point_ratio: f64,
}

#[derive(Clone, Debug)]
struct AxisTick {
    label: String,
    position: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locale {
    Fr,
    En,
}

fn main() {
    console_error_panic_hook::set_once();
    register_service_worker();

    mount_to_body(|| view! { <App/> });
}

#[component]
fn App() -> impl IntoView {
    let (locale, set_locale) = signal(Locale::Fr);
    let (selected_station, set_selected_station) = signal("2606".to_string());
    let (dashboard, set_dashboard) = signal(None::<DashboardData>);
    let (loading, set_loading) = signal(false);
    let (error, set_error) = signal(None::<String>);

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        set_loading.set(true);
        set_error.set(None);
        spawn_local(async move {
            match load_dashboard().await {
                Ok(data) => {
                    if data
                        .stations
                        .iter()
                        .all(|station| station.id != selected_station.get())
                    {
                        if let Some(first) = data.stations.first() {
                            set_selected_station.set(first.id.clone());
                        }
                    }
                    set_dashboard.set(Some(data));
                    set_loading.set(false);
                }
                Err(err) => {
                    set_error.set(Some(err));
                    set_loading.set(false);
                }
            }
        });
    });

    refresh();

    let interval = {
        let refresh = Rc::clone(&refresh);
        Interval::new(REFRESH_MS, move || refresh())
    };
    interval.forget();

    view! {
        <div class="app-shell">
            <header class="topbar">
                <div>
                    <p class="eyebrow">{move || tr(locale.get(), "Conditions hydrologiques", "Water conditions")}</p>
                    <h1>"rhonometre"</h1>
                </div>
                <div class="topbar-actions">
                    <div class="segmented" aria-label="Language">
                        <button
                            type="button"
                            class=move || if locale.get() == Locale::Fr { "active" } else { "" }
                            on:click=move |_| set_locale.set(Locale::Fr)
                        >
                            "FR"
                        </button>
                        <button
                            type="button"
                            class=move || if locale.get() == Locale::En { "active" } else { "" }
                            on:click=move |_| set_locale.set(Locale::En)
                        >
                            "EN"
                        </button>
                    </div>
                    <button class="refresh-button" type="button" on:click=move |_| refresh()>
                        {move || tr(locale.get(), "Actualiser", "Refresh")}
                    </button>
                </div>
            </header>

            {move || {
                let locale_value = locale.get();
                match dashboard.get() {
                    Some(data) => render_dashboard(
                        data,
                        selected_station.get(),
                        set_selected_station,
                        locale_value,
                        loading.get(),
                        error.get(),
                    ).into_any(),
                    None => render_empty(locale_value, loading.get(), error.get()).into_any(),
                }
            }}
        </div>
    }
}

fn render_empty(locale: Locale, loading: bool, error: Option<String>) -> impl IntoView {
    view! {
        <main class="dashboard">
            <section class="empty-state">
                <div class="loading-mark"></div>
                <h2>{tr(locale, "Chargement des données", "Loading data")}</h2>
                <p>{tr(locale, "Connexion à Hydrodaten.", "Connecting to Hydrodaten.")}</p>
                {error.map(|message| view! { <p class="error-text">{message}</p> })}
                <p class="status-line">
                    {if loading {
                        tr(locale, "Requête en cours", "Request in progress")
                    } else {
                        tr(locale, "En attente", "Waiting")
                    }}
                </p>
            </section>
        </main>
    }
}

fn render_dashboard(
    data: DashboardData,
    selected_id: String,
    set_selected_id: WriteSignal<String>,
    locale: Locale,
    loading: bool,
    error: Option<String>,
) -> impl IntoView {
    let stations = data.stations.clone();
    let warnings = data.warnings.clone();
    let cache_status = data.cache_status.clone();
    let source_label = data.source.label.clone();
    let source_url = data.source.url.clone();
    let generated_at = format_datetime(&data.generated_at);
    let selected = stations
        .iter()
        .find(|station| station.id == selected_id)
        .or_else(|| stations.first())
        .cloned();

    view! {
        <main class="dashboard">
            <section class="source-strip">
                <div>
                    <span class="label">{tr(locale, "Source", "Source")}</span>
                    <a href=source_url target="_blank" rel="noreferrer">{source_label}</a>
                </div>
                <div>
                    <span class="label">{tr(locale, "Dernière mise à jour", "Updated")}</span>
                    <span>{generated_at}</span>
                </div>
                <div>
                    <span class=if cache_status == CacheStatus::Fresh { "cache fresh" } else { "cache stale" }>
                        {match cache_status {
                            CacheStatus::Fresh => tr(locale, "Données fraîches", "Fresh data"),
                            CacheStatus::Stale => tr(locale, "Cache ancien", "Stale cache"),
                        }}
                    </span>
                </div>
                {if loading {
                    Some(view! { <span class="loading-text">{tr(locale, "Actualisation...", "Refreshing...")}</span> })
                } else {
                    None
                }}
            </section>

            {error.map(|message| view! { <section class="notice error-text">{message}</section> })}
            {if warnings.is_empty() {
                None
            } else {
                Some(view! {
                    <section class="notice">
                        {warnings.iter().cloned().map(|warning| view! { <p>{warning}</p> }).collect_view()}
                    </section>
                })
            }}

            <section class="station-tabs" aria-label="Stations">
                {stations.iter().map(|station| {
                    let id = station.id.clone();
                    let button_id = id.clone();
                    let is_active = id == selected_id;
                    let kind = station.kind.clone();
                    view! {
                        <button
                            type="button"
                            class=if is_active { "station-tab active" } else { "station-tab" }
                            data-station=station.slug.clone()
                            on:click=move |_| set_selected_id.set(button_id.clone())
                        >
                            <span class=if kind == WaterKind::Lake { "kind lake" } else { "kind river" }>
                                {kind_label(locale, &kind)}
                            </span>
                            <strong>{station_title(station, locale)}</strong>
                            <small>{station_role(station, locale)}</small>
                        </button>
                    }
                }).collect_view()}
            </section>

            {selected.map(|station| view! { <StationPanel station=station locale=locale/> })}
        </main>
    }
}

#[component]
fn StationPanel(station: StationData, locale: Locale) -> impl IntoView {
    let status_class = match &station.status {
        StationStatus::Complete => "status-pill complete",
        StationStatus::Partial => "status-pill partial",
        StationStatus::Missing => "status-pill missing",
    };
    let mut history = station.history.clone();
    history.sort_by_key(|series| metric_order(series.kind));
    let time_axis = station_time_axis(&history, &station.forecast);
    let x_range = time_axis.as_ref().map(|(start, end, _)| (*start, *end));
    let axis_ticks = time_axis
        .as_ref()
        .map(|(_, _, ticks)| ticks.clone())
        .unwrap_or_default();
    let notice = station_notice(&station, locale);
    let stack_class = if history.len() <= 1 {
        "chart-stack single"
    } else {
        "chart-stack multi"
    };
    let (hover_x, set_hover_x) = signal(None::<f64>);

    view! {
        <section class="station-panel">
            <div class="station-heading">
                <div>
                    <p class="eyebrow">{station_role(&station, locale)}</p>
                    <h2>{station_title(&station, locale)}</h2>
                </div>
                <span class=status_class>{status_label(locale, &station.status)}</span>
            </div>
            {notice.map(|message| view! {
                <div class="station-notice">
                    <strong>{tr(locale, "Estimation", "Estimate")}</strong>
                    <span>{message}</span>
                </div>
            })}

            <div class=stack_class on:mouseleave=move |_| set_hover_x.set(None)>
                {if axis_ticks.is_empty() {
                    None
                } else {
                    Some(view! {
                        <div class="stack-time-grid" aria-hidden="true">
                            <div class="stack-time-grid-plot">
                                {axis_ticks.iter().map(|tick| {
                                    view! {
                                        <span
                                            class="stack-grid-line"
                                            style=format!("left: {:.3}%;", tick.position)
                                        ></span>
                                    }
                                }).collect_view()}
                                {move || {
                                    let (start, end) = x_range?;
                                    let x = hover_x.get()?;
                                    let ratio = if end > start {
                                        ((x - start) / (end - start)).clamp(0.0, 1.0)
                                    } else {
                                        0.0
                                    };
                                    Some(view! {
                                        <span
                                            class="stack-hover-grid-line"
                                            style=format!("left: {:.6}%;", ratio * 100.0)
                                        ></span>
                                    })
                                }}
                            </div>
                            <div></div>
                        </div>
                    })
                }}
                {history.iter().map(|series| {
                    let current = station.current.iter().find(|metric| metric.kind == series.kind).cloned();
                    let forecast = station.forecast.iter().find(|forecast| forecast.kind == series.kind).cloned();
                    view! {
                        <MetricChart
                            series=series.clone()
                            forecast=forecast
                            current=current
                            x_range=x_range
                            locale=locale
                            hover_x=hover_x
                            set_hover_x=set_hover_x
                        />
                    }
                }).collect_view()}
                {if time_axis.is_some() {
                    Some(view! {
                        <div class="shared-time-axis">
                            <div class="axis-content">
                                <div class="axis-line" aria-hidden="true">
                                    {axis_ticks.iter().map(|tick| {
                                        let style = format!("left: {:.3}%;", tick.position);
                                        view! {
                                            <span class="axis-tick-mark" style=style></span>
                                        }
                                    }).collect_view()}
                                    {move || {
                                        let (start, end) = x_range?;
                                        let x = hover_x.get()?;
                                        let ratio = if end > start {
                                            ((x - start) / (end - start)).clamp(0.0, 1.0)
                                        } else {
                                            0.0
                                        };
                                        Some(view! {
                                            <span
                                                class="axis-hover-tick"
                                                style=format!("left: {:.6}%;", ratio * 100.0)
                                            ></span>
                                        })
                                    }}
                                </div>
                                <div class="axis-labels">
                                    {axis_ticks.iter().enumerate().map(|(index, tick)| {
                                        let style = format!("left: {:.3}%;", tick.position);
                                        let class = if index == 0 {
                                            "axis-label start"
                                        } else if index + 1 == axis_ticks.len() {
                                            "axis-label end"
                                        } else {
                                            "axis-label"
                                        };
                                        view! {
                                            <span class=class style=style>{tick.label.clone()}</span>
                                        }
                                    }).collect_view()}
                                </div>
                            </div>
                            <div class="axis-spacer"></div>
                        </div>
                    })
                } else {
                    None
                }}
            </div>
        </section>
    }
}

#[component]
fn MetricChart(
    series: MetricSeries,
    forecast: Option<MetricSeries>,
    current: Option<CurrentMetric>,
    x_range: Option<(f64, f64)>,
    locale: Locale,
    hover_x: ReadSignal<Option<f64>>,
    set_hover_x: WriteSignal<Option<f64>>,
) -> impl IntoView {
    let title = series_label(&series, locale);
    let unit = series.unit.clone();
    let summary = history_summary(&series.points, locale);
    let class = match series.kind {
        MetricKind::Discharge => "chart-card discharge",
        MetricKind::WaterLevel => "chart-card level",
        MetricKind::Temperature => "chart-card temperature",
    };
    let has_forecast = forecast
        .as_ref()
        .is_some_and(|forecast| !forecast.points.is_empty());
    let forecast_summary = forecast
        .as_ref()
        .and_then(|forecast| time_span_summary(&forecast.points));
    let points = chart_points(&series, forecast.as_ref());
    let chart_points = points.clone();
    let hover_points = points.clone();
    let data = Signal::derive(move || chart_points.clone());
    let chart_y_range = points_y_range(&points, series.uncertainty.as_ref());
    let hover_y_range = chart_y_range;
    let band_path = error_band_path(
        &series,
        x_range.or_else(|| points_x_range(&points)),
        chart_y_range,
    );
    let uncertainty_text = series.uncertainty.as_ref().map(|uncertainty| {
        let error = uncertainty.upper.max(uncertainty.lower);
        format!("±{} {}", format_value(error), unit)
    });
    let uncertainty_label = series.uncertainty.as_ref().map(|uncertainty| {
        format!(
            "{} {:.0}%",
            tr(locale, "Bande", "Band"),
            uncertainty.confidence * 100.0
        )
    });
    let chart_title = title.clone();
    let chart_unit = unit.clone();
    let mut series_def = Series::new(|point: &ChartPoint| point.x).line(
        Line::new(|point: &ChartPoint| point.history_y)
            .with_name(chart_title.clone())
            .with_colour(metric_colour(series.kind))
            .with_width(2.0)
            .with_interpolation(Interpolation::Linear),
    );
    if has_forecast {
        let forecast_name = forecast
            .as_ref()
            .map(|forecast| series_label(forecast, locale))
            .unwrap_or_else(|| tr(locale, "Prévision", "Forecast").to_string());
        series_def = series_def.line(
            Line::new(|point: &ChartPoint| point.forecast_y)
                .with_name(forecast_name)
                .with_colour(Colour::from_rgb(180, 83, 9))
                .with_width(2.0)
                .with_interpolation(Interpolation::Linear),
        );
    }
    if let Some((start, end)) = x_range {
        series_def = series_def.with_x_range(start, end);
    }
    if let Some((min, max)) = chart_y_range {
        series_def = series_def.with_y_range(min, max);
    }

    let current_value = current
        .as_ref()
        .map(|metric| format!("{} {}", format_value(metric.value), metric.unit))
        .unwrap_or_else(|| tr(locale, "n/d", "n/a").to_string());
    let current_at = current
        .as_ref()
        .map(|metric| format_datetime(&metric.measured_at))
        .unwrap_or_else(|| tr(locale, "non disponible", "unavailable").to_string());
    let current_range = current.as_ref().and_then(|metric| {
        let range = metric.range_24h.as_ref()?;
        Some(format!(
            "{} - {} {}",
            format_value(range.min),
            format_value(range.max),
            metric.unit
        ))
    });
    let current_mean = current.as_ref().and_then(|metric| {
        metric
            .range_24h
            .as_ref()
            .and_then(|range| range.mean)
            .map(|mean| (mean, metric.unit.clone()))
    });
    let hover_unit = unit.clone();
    let hover_x_range = x_range.or_else(|| points_x_range(&points));
    let update_hover = move |event: web_sys::MouseEvent| {
        let Some((start, end)) = hover_x_range else {
            set_hover_x.set(None);
            return;
        };
        let Some(target) = event
            .current_target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            set_hover_x.set(None);
            return;
        };

        let rect = target.get_bounding_client_rect();
        if rect.width() <= 0.0 || end <= start {
            set_hover_x.set(None);
            return;
        }

        let plot_left = rect.left() + PLOT_LEFT_INSET_PX;
        let plot_width = (rect.width() - PLOT_LEFT_INSET_PX).max(1.0);
        let ratio = ((event.client_x() as f64 - plot_left) / plot_width).clamp(0.0, 1.0);
        let x = start + ratio * (end - start);
        set_hover_x.set(Some(x));
    };
    let hovered = Memo::new(move |_| {
        let x = hover_x.get()?;
        let x_range = hover_x_range?;
        let y_range = hover_y_range?;
        nearest_hover_metric(&hover_points, x, &hover_unit, x_range, y_range)
    });
    let current_value_fallback = current_value.clone();
    let current_at_fallback = current_at.clone();
    let readout_label = move || {
        hovered
            .get()
            .map(|metric| metric.timestamp)
            .unwrap_or_else(|| tr(locale, "Actuel", "Current").to_string())
    };
    let readout_value = move || {
        hovered
            .get()
            .map(|metric| format!("{} {}", format_value(metric.value), metric.unit))
            .unwrap_or_else(|| current_value_fallback.clone())
    };
    let readout_time = move || {
        hovered
            .get()
            .map(|metric| {
                if metric.is_forecast {
                    tr(locale, "Prévision", "Forecast").to_string()
                } else {
                    tr(locale, "Historique", "History").to_string()
                }
            })
            .unwrap_or_else(|| {
                format!(
                    "{} {}",
                    tr(locale, "Mesuré", "Measured"),
                    current_at_fallback
                )
            })
    };

    view! {
        <article class=class>
            <div class="chart-heading">
                <div>
                    <h3>{title}</h3>
                    <small>{summary}</small>
                    {forecast_summary.map(|summary| view! {
                        <small class="forecast-summary">
                            {tr(locale, "Prévision", "Forecast")} ": " {summary}
                        </small>
                    })}
                </div>
                <span>{chart_unit}</span>
            </div>
            <div class="plot-row">
                <div class="chart-frame" on:mousemove=update_hover>
                    <Chart
                        aspect_ratio=AspectRatio::from_env()
                        series=series_def
                        data=data
                        left=TickLabels::aligned_floats().with_min_chars(Y_AXIS_MIN_CHARS)
                        inner=[
                            AxisMarker::left_edge().into_inner(),
                            YGridLine::default().into_inner(),
                        ]
                    />
                    {band_path.map(|path| view! {
                        <div class="uncertainty-band-plot" aria-hidden="true">
                            <svg viewBox="0 0 1000 100" preserveAspectRatio="none">
                                <path class="uncertainty-band" d=path></path>
                            </svg>
                        </div>
                    })}
                    {move || hovered.get().map(|metric| {
                        let cursor_left = plot_left_calc(metric.cursor_ratio);
                        let point_top = plot_top_calc(metric.point_ratio);
                        let point_style = format!(
                            "left: {cursor_left}; top: {point_top};",
                        );
                        view! {
                            <div class="hover-cursor" aria-hidden="true">
                                <span class="hover-cursor-point" style=point_style></span>
                            </div>
                        }
                    })}
                </div>
                <aside class="plot-current">
                    <span>{readout_label}</span>
                    <strong>{readout_value}</strong>
                    <small>{readout_time}</small>
                    {current_range.map(|range| view! {
                        <div class="range-row">
                            <span>{tr(locale, "24 h", "24 h")}</span>
                            <span>{range}</span>
                        </div>
                    })}
                    {current_mean.map(|(mean, unit)| view! {
                        <div class="range-row subtle">
                            <span>{tr(locale, "Moyenne", "Mean")}</span>
                            <span>{format!("{} {}", format_value(mean), unit)}</span>
                        </div>
                    })}
                    {uncertainty_text.zip(uncertainty_label).map(|(text, label)| view! {
                        <div class="range-row subtle">
                            <span>{label}</span>
                            <span>{text}</span>
                        </div>
                    })}
                </aside>
            </div>
        </article>
    }
}

async fn load_dashboard() -> Result<DashboardData, String> {
    let response = Request::get("/api/dashboard")
        .send()
        .await
        .map_err(|err| format!("Network error: {err}"))?;

    if !response.ok() {
        return Err(format!(
            "Hydrodaten API returned HTTP {}",
            response.status()
        ));
    }

    response
        .json::<DashboardData>()
        .await
        .map_err(|err| format!("Invalid API payload: {err}"))
}

fn register_service_worker() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let navigator = window.navigator();
    let container = navigator.service_worker();
    let _ = container.register("sw.js");
}

fn station_title(station: &StationData, locale: Locale) -> String {
    match locale {
        Locale::Fr => station.name_fr.clone(),
        Locale::En => station.name_en.clone(),
    }
}

fn station_role(station: &StationData, locale: Locale) -> String {
    match locale {
        Locale::Fr => station.role_fr.clone(),
        Locale::En => station.role_en.clone(),
    }
}

fn station_notice(station: &StationData, locale: Locale) -> Option<String> {
    match locale {
        Locale::Fr => station.notice_fr.clone(),
        Locale::En => station.notice_en.clone(),
    }
}

fn kind_label(locale: Locale, kind: &WaterKind) -> &'static str {
    match (locale, kind) {
        (Locale::Fr, WaterKind::River) => "Rivière",
        (Locale::Fr, WaterKind::Lake) => "Lac",
        (Locale::En, WaterKind::River) => "River",
        (Locale::En, WaterKind::Lake) => "Lake",
    }
}

fn status_label(locale: Locale, status: &StationStatus) -> &'static str {
    match (locale, status) {
        (Locale::Fr, StationStatus::Complete) => "Complet",
        (Locale::Fr, StationStatus::Partial) => "Partiel",
        (Locale::Fr, StationStatus::Missing) => "Manquant",
        (Locale::En, StationStatus::Complete) => "Complete",
        (Locale::En, StationStatus::Partial) => "Partial",
        (Locale::En, StationStatus::Missing) => "Missing",
    }
}

fn series_label(series: &MetricSeries, locale: Locale) -> String {
    match locale {
        Locale::Fr => series.label_fr.clone(),
        Locale::En => series.label_en.clone(),
    }
}

fn metric_order(kind: MetricKind) -> u8 {
    match kind {
        MetricKind::Discharge => 0,
        MetricKind::Temperature => 1,
        MetricKind::WaterLevel => 2,
    }
}

fn metric_colour(kind: MetricKind) -> Colour {
    match kind {
        MetricKind::Discharge => Colour::from_rgb(37, 99, 235),
        MetricKind::WaterLevel => Colour::from_rgb(15, 118, 110),
        MetricKind::Temperature => Colour::from_rgb(183, 121, 31),
    }
}

fn chart_points(series: &MetricSeries, forecast: Option<&MetricSeries>) -> Vec<ChartPoint> {
    let mut points = series
        .points
        .iter()
        .filter_map(|point| {
            let timestamp = DateTime::parse_from_rfc3339(&point.timestamp).ok()?;
            Some(ChartPoint {
                x: timestamp.timestamp() as f64,
                history_y: point.value,
                forecast_y: f64::NAN,
                timestamp: timestamp.format("%d.%m %H:%M").to_string(),
            })
        })
        .collect::<Vec<_>>();

    if let Some(forecast) = forecast {
        points.extend(forecast.points.iter().filter_map(|point| {
            let timestamp = DateTime::parse_from_rfc3339(&point.timestamp).ok()?;
            Some(ChartPoint {
                x: timestamp.timestamp() as f64,
                history_y: f64::NAN,
                forecast_y: point.value,
                timestamp: timestamp.format("%d.%m %H:%M").to_string(),
            })
        }));
    }

    points.sort_by(|left, right| {
        left.x
            .partial_cmp(&right.x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    points
}

fn points_x_range(points: &[ChartPoint]) -> Option<(f64, f64)> {
    let start = points.first()?.x;
    let end = points.last()?.x;
    Some((start, end))
}

fn points_y_range(
    points: &[ChartPoint],
    uncertainty: Option<&MetricUncertainty>,
) -> Option<(f64, f64)> {
    let mut values = points.iter().flat_map(|point| {
        if point.history_y.is_finite() {
            let mut values = vec![point.history_y];
            if let Some(uncertainty) = uncertainty {
                values.push(point.history_y - uncertainty.lower);
                values.push(point.history_y + uncertainty.upper);
            }
            values
        } else if point.forecast_y.is_finite() {
            vec![point.forecast_y]
        } else {
            Vec::new()
        }
    });
    let first = values.next()?;
    let (min, max) = values.fold((first, first), |(min, max), value| {
        (min.min(value), max.max(value))
    });
    Some((min, max))
}

fn error_band_path(
    series: &MetricSeries,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
) -> Option<String> {
    let uncertainty = series.uncertainty.as_ref()?;
    let (start_x, end_x) = x_range?;
    let (min_y, max_y) = y_range?;
    if end_x <= start_x || max_y <= min_y {
        return None;
    }

    let mut points = series
        .points
        .iter()
        .filter_map(|point| {
            let timestamp = DateTime::parse_from_rfc3339(&point.timestamp).ok()?;
            let x = timestamp.timestamp() as f64;
            if !(start_x..=end_x).contains(&x) || !point.value.is_finite() {
                return None;
            }

            let x_ratio = ((x - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * 1000.0;
            let upper = y_band_position(point.value + uncertainty.upper, min_y, max_y);
            let lower = y_band_position(point.value - uncertainty.lower, min_y, max_y);
            Some((x_ratio, upper, lower))
        })
        .collect::<Vec<_>>();

    points.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if points.len() < 2 {
        return None;
    }

    let mut path = String::new();
    for (index, (x, upper, _)) in points.iter().enumerate() {
        if index == 0 {
            path.push_str(&format!("M {:.3} {:.3}", x, upper));
        } else {
            path.push_str(&format!(" L {:.3} {:.3}", x, upper));
        }
    }
    for (x, _, lower) in points.iter().rev() {
        path.push_str(&format!(" L {:.3} {:.3}", x, lower));
    }
    path.push_str(" Z");
    Some(path)
}

fn y_band_position(value: f64, min_y: f64, max_y: f64) -> f64 {
    ((max_y - value) / (max_y - min_y)).clamp(0.0, 1.0) * 100.0
}

fn nearest_hover_metric(
    points: &[ChartPoint],
    x: f64,
    unit: &str,
    x_range: (f64, f64),
    y_range: (f64, f64),
) -> Option<HoverMetric> {
    let (start_x, end_x) = x_range;
    let (min_y, max_y) = y_range;
    points
        .iter()
        .filter_map(|point| {
            let (value, is_forecast) = if point.history_y.is_finite() {
                (point.history_y, false)
            } else if point.forecast_y.is_finite() {
                (point.forecast_y, true)
            } else {
                return None;
            };
            let x_ratio = if end_x > start_x {
                ((point.x - start_x) / (end_x - start_x)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let y_ratio = if max_y > min_y {
                ((max_y - value) / (max_y - min_y)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            Some((
                (point.x - x).abs(),
                HoverMetric {
                    value,
                    unit: unit.to_string(),
                    timestamp: point.timestamp.clone(),
                    is_forecast,
                    cursor_ratio: x_ratio,
                    point_ratio: y_ratio,
                },
            ))
        })
        .min_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, metric)| metric)
}

fn plot_left_calc(ratio: f64) -> String {
    format!(
        "calc(var(--plot-left-inset) + {:.6}% - {:.3}px)",
        ratio * 100.0,
        ratio * PLOT_LEFT_INSET_PX,
    )
}

fn plot_top_calc(ratio: f64) -> String {
    format!(
        "calc(var(--plot-top-inset) + {:.6}% - {:.3}px)",
        ratio * 100.0,
        ratio * (PLOT_TOP_INSET_PX + PLOT_BOTTOM_INSET_PX),
    )
}

fn station_time_axis(
    history: &[MetricSeries],
    forecast: &[MetricSeries],
) -> Option<(f64, f64, Vec<AxisTick>)> {
    let mut times = history
        .iter()
        .chain(forecast.iter())
        .flat_map(|series| series.points.iter())
        .filter_map(|point| DateTime::parse_from_rfc3339(&point.timestamp).ok())
        .collect::<Vec<_>>();
    times.sort();
    let start = times.first()?;
    let end = times.last()?;
    let start_timestamp = start.timestamp();
    let end_timestamp = end.timestamp();
    let span = end_timestamp - start_timestamp;
    let offset = *start.offset();
    let ticks = (0..=4)
        .filter_map(|index| {
            let position = index as f64 * 25.0;
            let timestamp = if span <= 0 {
                start_timestamp
            } else {
                start_timestamp + ((span as f64) * (index as f64 / 4.0)).round() as i64
            };
            let datetime = DateTime::<Utc>::from_timestamp(timestamp, 0)?.with_timezone(&offset);
            Some(AxisTick {
                label: datetime.format("%d.%m %H:%M").to_string(),
                position,
            })
        })
        .collect();

    Some((start_timestamp as f64, end_timestamp as f64, ticks))
}

fn format_value(value: f64) -> String {
    let absolute = value.abs();
    if absolute >= 100.0 {
        format!("{value:.0}")
    } else if absolute >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn format_datetime(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|datetime| datetime.format("%d.%m %H:%M").to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn history_summary(points: &[HistoryPoint], locale: Locale) -> String {
    time_span_summary(points)
        .map(|span| format!("{}: {span}", tr(locale, "Historique", "History")))
        .unwrap_or_else(|| tr(locale, "Aucun point disponible", "No points available").to_string())
}

fn time_span_summary(points: &[HistoryPoint]) -> Option<String> {
    Some(format!(
        "{} - {}",
        format_datetime(&points.first()?.timestamp),
        format_datetime(&points.last()?.timestamp)
    ))
}

fn tr(locale: Locale, fr: &'static str, en: &'static str) -> &'static str {
    match locale {
        Locale::Fr => fr,
        Locale::En => en,
    }
}
