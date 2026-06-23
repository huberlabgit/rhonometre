use std::rc::Rc;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use gloo_net::http::Request;
use gloo_timers::callback::Interval;
use leptos::{mount::mount_to_body, prelude::*};
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

const REFRESH_MS: u32 = 120_000;
const SVG_PLOT_WIDTH: f64 = 1000.0;
const SVG_PLOT_HEIGHT: f64 = 100.0;
const FORECAST_VISIBLE_SECONDS: f64 = 24.0 * 60.0 * 60.0;
const GENEVA_LATITUDE: f64 = 46.2044;
const GENEVA_LONGITUDE: f64 = 6.1432;
const SUNRISE_SUNSET_ZENITH_DEGREES: f64 = 90.833;
const PRO_ACCESS_CODE: &str = "rhonometre";

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
    notice_fr: Option<String>,
    notice_en: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WaterKind {
    River,
    Lake,
}

#[derive(Clone, Debug, Deserialize)]
struct CurrentMetric {
    kind: MetricKind,
    value: f64,
    unit: String,
    measured_at: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DischargeSafety {
    Safe,
    Risky,
    NoSwim,
}

#[derive(Clone, Debug)]
struct SegmentedPath {
    path: String,
    safety: DischargeSafety,
}

#[derive(Clone, Debug, PartialEq)]
struct HoverMetric {
    value: f64,
    unit: String,
    timestamp: String,
    cursor_ratio: f64,
    point_ratio: f64,
}

#[derive(Clone, Debug)]
struct AxisTick {
    label: String,
    position: f64,
    kind: AxisTickKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AxisTickKind {
    Day,
    Noon,
    Hour,
}

#[derive(Clone, Debug)]
struct ValueAxis {
    min: f64,
    max: f64,
    ticks: Vec<ValueTick>,
}

#[derive(Clone, Debug)]
struct ValueTick {
    label: String,
    position: f64,
}

#[derive(Clone, Debug)]
struct SunBand {
    x: f64,
    width: f64,
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
    let (focus_mode, set_focus_mode) = signal(initial_focus_mode());
    let (live_clock, set_live_clock) = signal(format_swiss_now_seconds());
    let (pro_mode, set_pro_mode) = signal(initial_pro_mode());
    let (pro_signin_open, set_pro_signin_open) = signal(false);
    let (pro_code, set_pro_code) = signal(String::new());
    let (pro_error, set_pro_error) = signal(None::<String>);

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

    let clock_interval = Interval::new(1_000, move || {
        set_live_clock.set(format_swiss_now_seconds());
    });
    clock_interval.forget();

    view! {
        <div class=move || if focus_mode.get() { "app-shell focus-mode" } else { "app-shell" }>
            <button
                class="focus-toggle-button icon-button"
                type="button"
                title=move || if focus_mode.get() {
                    tr(locale.get(), "Quitter le mode focus", "Exit focus mode")
                } else {
                    tr(locale.get(), "Mode focus", "Focus mode")
                }
                aria-label=move || if focus_mode.get() {
                    tr(locale.get(), "Quitter le mode focus", "Exit focus mode")
                } else {
                    tr(locale.get(), "Mode focus", "Focus mode")
                }
                on:click=move |_| set_focus_mode.update(|value| *value = !*value)
            >
                {move || if focus_mode.get() {
                    view! { <FocusExitIcon/> }.into_any()
                } else {
                    view! { <FocusEnterIcon/> }.into_any()
                }}
            </button>
            <header class="topbar">
                <div>
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
                    <button
                        class=move || if pro_mode.get() { "refresh-button active" } else { "refresh-button" }
                        type="button"
                        on:click=move |_| {
                            if pro_mode.get_untracked() {
                                set_pro_mode.set(false);
                                set_pro_signin_open.set(false);
                                set_pro_code.set(String::new());
                                set_pro_error.set(None);
                            } else {
                                set_pro_signin_open.set(true);
                            }
                        }
                    >
                        {move || if pro_mode.get() { tr(locale.get(), "Pro actif", "Pro on") } else { "Pro" }}
                    </button>
                </div>
            </header>

            {move || if pro_signin_open.get() && !pro_mode.get() && !focus_mode.get() {
                Some(view! {
                    <section class="pro-signin">
                        <strong>{tr(locale.get(), "Mode pro", "Pro mode")}</strong>
                        <input
                            type="password"
                            autocomplete="current-password"
                            placeholder=tr(locale.get(), "Code", "Code")
                            prop:value=move || pro_code.get()
                            on:input=move |event| {
                                if let Some(input) = event
                                    .target()
                                    .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
                                {
                                    set_pro_code.set(input.value());
                                    set_pro_error.set(None);
                                }
                            }
                        />
                        <button
                            class="refresh-button"
                            type="button"
                            on:click=move |_| {
                                if pro_code.get_untracked().trim() == PRO_ACCESS_CODE {
                                    set_pro_mode.set(true);
                                    set_pro_signin_open.set(false);
                                    set_pro_code.set(String::new());
                                    set_pro_error.set(None);
                                } else {
                                    set_pro_error.set(Some(tr(locale.get_untracked(), "Code invalide", "Invalid code").to_string()));
                                }
                            }
                        >
                            {tr(locale.get(), "Se connecter", "Sign in")}
                        </button>
                        <button
                            class="refresh-button ghost"
                            type="button"
                            on:click=move |_| {
                                set_pro_signin_open.set(false);
                                set_pro_code.set(String::new());
                                set_pro_error.set(None);
                            }
                        >
                            {tr(locale.get(), "Annuler", "Cancel")}
                        </button>
                        {move || pro_error.get().map(|message| view! { <small class="error-text">{message}</small> })}
                    </section>
                })
            } else {
                None
            }}

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
                        focus_mode.get(),
                        live_clock,
                        pro_mode.get(),
                    ).into_any(),
                    None => render_empty(locale_value, loading.get(), error.get()).into_any(),
                }
            }}
        </div>
    }
}

#[component]
fn FocusEnterIcon() -> impl IntoView {
    view! {
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M15 3h6v6"></path>
            <path d="M21 3l-7 7"></path>
            <path d="M9 21H3v-6"></path>
            <path d="M3 21l7-7"></path>
        </svg>
    }
}

#[component]
fn FocusExitIcon() -> impl IntoView {
    view! {
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M14 4v6h6"></path>
            <path d="M21 3l-7 7"></path>
            <path d="M10 20v-6H4"></path>
            <path d="M3 21l7-7"></path>
        </svg>
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

fn initial_focus_mode() -> bool {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .is_some_and(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .any(|part| matches!(part, "focus" | "focus=1" | "focus=true"))
        })
}

fn initial_pro_mode() -> bool {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .is_some_and(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .any(|part| matches!(part, "pro" | "pro=1" | "pro=true"))
        })
}

fn render_dashboard(
    data: DashboardData,
    selected_id: String,
    set_selected_id: WriteSignal<String>,
    locale: Locale,
    loading: bool,
    error: Option<String>,
    focus_mode: bool,
    live_clock: ReadSignal<String>,
    pro_mode: bool,
) -> impl IntoView {
    let stations = data.stations.clone();
    let selectable_stations = stations
        .iter()
        .filter(|station| station.kind != WaterKind::Lake)
        .cloned()
        .collect::<Vec<_>>();
    let warnings = data.warnings.clone();
    let cache_status = data.cache_status.clone();
    let source_label = data.source.label.clone();
    let source_url = data.source.url.clone();
    let generated_at = format_datetime(&data.generated_at);
    let selected = selectable_stations
        .iter()
        .find(|station| station.id == selected_id)
        .or_else(|| selectable_stations.first())
        .cloned();

    view! {
        <main class=if focus_mode { "dashboard dashboard-focus" } else { "dashboard" }>
            {if focus_mode {
                None
            } else {
                error.map(|message| view! { <section class="notice error-text">{message}</section> })
            }}
            {if focus_mode || warnings.is_empty() {
                None
            } else {
                Some(view! {
                    <section class="notice">
                        {warnings.iter().cloned().map(|warning| view! { <p>{warning}</p> }).collect_view()}
                    </section>
                })
            }}

            {selected.map(|station| view! { <StationPanel station=station locale=locale live_clock=live_clock pro_mode=pro_mode/> })}

            {if focus_mode {
                None
            } else {
                Some(view! {
                    <section class="station-tabs station-tabs-bottom" aria-label="Stations">
                        {selectable_stations.iter().map(|station| {
                            let id = station.id.clone();
                            let button_id = id.clone();
                            let is_active = id == selected_id;
                            view! {
                                <button
                                    type="button"
                                    class=if is_active { "station-tab active" } else { "station-tab" }
                                    data-station=station.slug.clone()
                                    on:click=move |_| set_selected_id.set(button_id.clone())
                                >
                                    <strong>{station_role(station, locale)}</strong>
                                    <small>{station_title(station, locale)}</small>
                                </button>
                            }
                        }).collect_view()}
                    </section>

                    <section class="source-strip source-strip-footer">
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
                })
            }}
        </main>
    }
}

#[component]
fn StationPanel(
    station: StationData,
    locale: Locale,
    live_clock: ReadSignal<String>,
    pro_mode: bool,
) -> impl IntoView {
    let mut history = station
        .history
        .iter()
        .filter(|series| is_visible_metric(series.kind))
        .cloned()
        .collect::<Vec<_>>();
    let forecast = if pro_mode {
        station
            .forecast
            .iter()
            .filter(|series| is_visible_metric(series.kind))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    history.sort_by_key(|series| metric_order(series.kind));
    let time_axis = station_time_axis(&history, &forecast);
    let x_range = time_axis.as_ref().map(|(start, end, _)| (*start, *end));
    let visible_x_range = x_range.map(|range| default_visible_x_range(&history, range));
    let scroll_width = scroll_content_width(x_range, visible_x_range);
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
    let show_error_band = station.id != "2606";
    let (hover_x, set_hover_x) = signal(None::<f64>);

    view! {
        <section class="station-panel">
            <div class="station-heading">
                <div>
                    <h2>{station_role(&station, locale)}</h2>
                    <p class="location-subtitle">{station_title(&station, locale)}</p>
                </div>
                <small class="page-live-clock">
                    {move || format!("{} {}", tr(locale, "Maintenant", "Now"), live_clock.get())}
                </small>
            </div>

            <div class=stack_class on:mouseleave=move |_| set_hover_x.set(None)>
                {if time_axis.is_some() {
                    Some(view! {
                        <TimeAxis
                            axis_ticks=axis_ticks.clone()
                            x_range=x_range
                            scroll_width=scroll_width.clone()
                            hover_x=hover_x
                            placement="time-axis-top"
                        />
                    })
                } else {
                    None
                }}
                {history.iter().enumerate().map(|(index, series)| {
                    let current = station.current.iter().find(|metric| metric.kind == series.kind).cloned();
                    let forecast = forecast.iter().find(|forecast| forecast.kind == series.kind).cloned();
                    let is_arve_discharge = station.id == "2170" && series.kind == MetricKind::Discharge;
                    let is_rhone_discharge = matches!(station.id.as_str(), "2606" | "2174")
                        && series.kind == MetricKind::Discharge;
                    let axis_floor = (is_arve_discharge || is_rhone_discharge).then_some(0.0);
                    let axis_min_ceiling = if is_arve_discharge {
                        Some(150.0)
                    } else if is_rhone_discharge {
                        Some(600.0)
                    } else {
                        None
                    };
                    view! {
                        <MetricChart
                            series=series.clone()
                            forecast=forecast
                            current=current
                            x_range=x_range
                            axis_ticks=axis_ticks.clone()
                            scroll_width=scroll_width.clone()
                            locale=locale
                            show_error_band=show_error_band
                            axis_floor=axis_floor
                            axis_min_ceiling=axis_min_ceiling
                            discharge_risk_reference=axis_min_ceiling
                            hover_x=hover_x
                            set_hover_x=set_hover_x
                        />
                        {if time_axis.is_some() && index + 1 < history.len() {
                            Some(view! {
                                <TimeAxis
                                    axis_ticks=axis_ticks.clone()
                                    x_range=x_range
                                    scroll_width=scroll_width.clone()
                                    hover_x=hover_x
                                    placement="time-axis-middle"
                                />
                            })
                        } else {
                            None
                        }}
                    }
                }).collect_view()}
                {if time_axis.is_some() {
                    Some(view! {
                        <TimeAxis
                            axis_ticks=axis_ticks.clone()
                            x_range=x_range
                            scroll_width=scroll_width.clone()
                            hover_x=hover_x
                            placement="time-axis-bottom"
                        />
                    })
                } else {
                    None
                }}
            </div>
            {notice.map(|message| view! {
                <p class="station-footnote">
                    <strong>{tr(locale, "Estimation", "Estimate")}</strong>
                    <span>{message}</span>
                </p>
            })}
        </section>
    }
}

#[component]
fn TimeAxis(
    axis_ticks: Vec<AxisTick>,
    x_range: Option<(f64, f64)>,
    scroll_width: String,
    hover_x: ReadSignal<Option<f64>>,
    placement: &'static str,
) -> impl IntoView {
    let class_name = format!("shared-time-axis {placement}");

    view! {
        <div class=class_name>
            <div class="shared-axis-frame">
                <div class="axis-scroll-viewport chart-scroll-sync" on:scroll=sync_chart_scroll>
                    <div class="axis-scroll-content" style=format!("width: {scroll_width};")>
                        <div class="axis-rule axis-rule-top" aria-hidden="true">
                            {axis_ticks.iter().map(|tick| {
                                let style = format!("left: {:.3}%;", tick.position);
                                view! {
                                    <span class=axis_tick_class(tick.kind) style=style></span>
                                }
                            }).collect_view()}
                            {move || axis_hover_marker(x_range, hover_x)}
                        </div>
                        <div class="axis-labels">
                            {axis_ticks.iter().filter(|tick| !tick.label.is_empty()).map(|tick| {
                                let style = format!("left: {:.3}%;", tick.position);
                                view! {
                                    <span class=axis_label_class(tick) style=style>{tick.label.clone()}</span>
                                }
                            }).collect_view()}
                        </div>
                        <div class="axis-rule axis-rule-bottom" aria-hidden="true">
                            {axis_ticks.iter().map(|tick| {
                                let style = format!("left: {:.3}%;", tick.position);
                                view! {
                                    <span class=axis_tick_class(tick.kind) style=style></span>
                                }
                            }).collect_view()}
                            {move || axis_hover_marker(x_range, hover_x)}
                        </div>
                    </div>
                </div>
            </div>
            <div class="axis-spacer"></div>
        </div>
    }
}

#[component]
fn MetricChart(
    series: MetricSeries,
    forecast: Option<MetricSeries>,
    current: Option<CurrentMetric>,
    x_range: Option<(f64, f64)>,
    axis_ticks: Vec<AxisTick>,
    scroll_width: String,
    locale: Locale,
    show_error_band: bool,
    axis_floor: Option<f64>,
    axis_min_ceiling: Option<f64>,
    discharge_risk_reference: Option<f64>,
    hover_x: ReadSignal<Option<f64>>,
    set_hover_x: WriteSignal<Option<f64>>,
) -> impl IntoView {
    let title = series_label(&series, locale);
    let unit = series.unit.clone();
    let class = match series.kind {
        MetricKind::Discharge => "chart-card discharge",
        MetricKind::WaterLevel => "chart-card level",
        MetricKind::Temperature => "chart-card temperature",
    };
    let has_forecast = forecast
        .as_ref()
        .is_some_and(|forecast| !forecast.points.is_empty());
    let points = chart_points(&series, forecast.as_ref());
    let hover_points = points.clone();
    let current_axis_value = current.as_ref().map(|metric| metric.value);
    let chart_x_range = x_range.or_else(|| points_x_range(&points));
    let value_axis = metric_value_axis(
        series.kind,
        &points,
        series.uncertainty.as_ref(),
        current_axis_value,
        axis_floor,
        axis_min_ceiling,
    );
    let chart_y_range = value_axis.as_ref().map(|axis| (axis.min, axis.max));
    let hover_y_range = chart_y_range;
    let y_ticks = value_axis
        .as_ref()
        .map(|axis| axis.ticks.clone())
        .unwrap_or_default();
    let band_path = show_error_band
        .then(|| error_band_path(&series, chart_x_range, chart_y_range))
        .flatten();
    let sun_bands = chart_x_range.map(sun_bands).unwrap_or_default();
    let history_area_segments = if series.kind == MetricKind::Discharge {
        segmented_area_paths(
            &points,
            |point| point.history_y,
            chart_x_range,
            chart_y_range,
            discharge_risk_reference,
        )
    } else {
        Vec::new()
    };
    let forecast_area_segments = if series.kind == MetricKind::Discharge && has_forecast {
        segmented_area_paths(
            &points,
            |point| point.forecast_y,
            chart_x_range,
            chart_y_range,
            discharge_risk_reference,
        )
    } else {
        Vec::new()
    };
    let history_segments = if series.kind == MetricKind::Discharge {
        segmented_line_paths(
            &points,
            |point| point.history_y,
            chart_x_range,
            chart_y_range,
            discharge_risk_reference,
        )
    } else {
        Vec::new()
    };
    let forecast_segments = if series.kind == MetricKind::Discharge && has_forecast {
        segmented_line_paths(
            &points,
            |point| point.forecast_y,
            chart_x_range,
            chart_y_range,
            discharge_risk_reference,
        )
    } else {
        Vec::new()
    };
    let history_path = (series.kind != MetricKind::Discharge)
        .then(|| {
            line_path(
                &points,
                |point| point.history_y,
                chart_x_range,
                chart_y_range,
            )
        })
        .flatten();
    let history_area_path = (series.kind != MetricKind::Discharge)
        .then(|| {
            area_path(
                &points,
                |point| point.history_y,
                chart_x_range,
                chart_y_range,
            )
        })
        .flatten();
    let forecast_path = if series.kind != MetricKind::Discharge && has_forecast {
        line_path(
            &points,
            |point| point.forecast_y,
            chart_x_range,
            chart_y_range,
        )
    } else {
        None
    };
    let forecast_area_path = if series.kind != MetricKind::Discharge && has_forecast {
        area_path(
            &points,
            |point| point.forecast_y,
            chart_x_range,
            chart_y_range,
        )
    } else {
        None
    };
    let axis_title = metric_axis_title(series.kind, &unit, locale);
    let chart_label = axis_title.clone();
    let plot_title = title.clone();
    let history_colour = metric_colour(series.kind).to_string();

    let current_value = current
        .as_ref()
        .map(|metric| format!("{} {}", format_value(metric.value), metric.unit))
        .unwrap_or_else(|| tr(locale, "n/d", "n/a").to_string());
    let current_numeric_value = current_axis_value;
    let current_at = current
        .as_ref()
        .map(|metric| format_datetime(&metric.measured_at))
        .unwrap_or_else(|| tr(locale, "non disponible", "unavailable").to_string());
    let hover_unit = unit.clone();
    let hover_x_range = x_range.or_else(|| points_x_range(&points));
    let update_hover = move |event: web_sys::MouseEvent| {
        let Some((start, end)) = hover_x_range else {
            set_hover_x.set(None);
            return;
        };
        let Some(target) = event
            .current_target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            set_hover_x.set(None);
            return;
        };

        let rect = target.get_bounding_client_rect();
        let scroll_width = (target.scroll_width() as f64).max(1.0);
        if rect.width() <= 0.0 || end <= start {
            set_hover_x.set(None);
            return;
        }

        let local_x = event.client_x() as f64 - rect.left();
        let ratio = ((local_x + target.scroll_left() as f64) / scroll_width).clamp(0.0, 1.0);
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
    let readout_value = move || {
        hovered
            .get()
            .map(|metric| format!("{} {}", format_value(metric.value), metric.unit))
            .unwrap_or_else(|| current_value_fallback.clone())
    };
    let readout_timestamp = move || {
        hovered
            .get()
            .map(|metric| metric.timestamp)
            .unwrap_or_else(|| current_at_fallback.clone())
    };
    let discharge_safety = Memo::new(move |_| {
        if series.kind != MetricKind::Discharge {
            return None;
        }

        hovered
            .get()
            .map(|metric| metric.value)
            .or(current_numeric_value)
            .map(|value| discharge_safety_for_value(value, discharge_risk_reference))
    });
    let readout_value_class = move || {
        discharge_safety
            .get()
            .map(|safety| format!("readout-value {}", discharge_safety_class(safety)))
            .unwrap_or_else(|| "readout-value".to_string())
    };
    let discharge_comment = move || {
        discharge_safety.get().map(|safety| {
            (
                discharge_safety_class(safety),
                discharge_safety_label(safety, locale),
            )
        })
    };

    view! {
        <article class=class>
            <div class="plot-row">
                <div class="chart-frame">
                    <div class="custom-chart" aria-label=chart_label>
                        <div class="custom-y-axis" aria-hidden="true">
                            <span class="custom-y-axis-title">{axis_title}</span>
                            {y_ticks.iter().map(|tick| {
                                view! {
                                    <span
                                        class="custom-y-tick"
                                        style=format!("top: {:.6}%;", tick.position)
                                    >
                                        {tick.label.clone()}
                                    </span>
                                }
                            }).collect_view()}
                        </div>
                        <div class="plot-scroll-viewport chart-scroll-sync" on:scroll=sync_chart_scroll on:mousemove=update_hover>
                            <div class="plot-scroll-content" style=format!("width: {scroll_width};")>
                                <div class="custom-plot-area">
                                    <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
                                        <g class="sun-shading">
                                            {sun_bands.iter().map(|band| {
                                                view! {
                                                    <rect
                                                        x=format!("{:.3}", band.x)
                                                        y="0"
                                                        width=format!("{:.3}", band.width)
                                                        height="100"
                                                    ></rect>
                                                }
                                            }).collect_view()}
                                        </g>
                                        <g class="custom-grid-x">
                                            {axis_ticks.iter().filter_map(|tick| {
                                                if tick.kind != AxisTickKind::Day || tick.position <= 0.0 || tick.position >= 100.0 {
                                                    return None;
                                                }
                                                let x = format!("{:.3}", tick.position / 100.0 * SVG_PLOT_WIDTH);
                                                Some(view! {
                                                    <line x1=x.clone() x2=x y1="0" y2="100"></line>
                                                })
                                            }).collect_view()}
                                        </g>
                                        <g class="custom-grid-y">
                                            {y_ticks.iter().enumerate().filter_map(|(index, tick)| {
                                                if index == 0 || index + 1 == y_ticks.len() {
                                                    return None;
                                                }
                                                let y = format!("{:.3}", tick.position);
                                                Some(view! {
                                                    <line x1="0" x2="1000" y1=y.clone() y2=y></line>
                                                })
                                            }).collect_view()}
                                        </g>
                                        {history_area_segments.iter().map(|segment| view! {
                                            <path
                                                class=format!("custom-area history {}", discharge_safety_class(segment.safety))
                                                d=segment.path.clone()
                                            ></path>
                                        }).collect_view()}
                                        {history_area_path.map(|path| {
                                            let fill = history_colour.clone();
                                            view! {
                                                <path class="custom-area history" fill=fill d=path></path>
                                            }
                                        })}
                                        {forecast_area_segments.iter().map(|segment| view! {
                                            <path
                                                class=format!("custom-area forecast {}", discharge_safety_class(segment.safety))
                                                d=segment.path.clone()
                                            ></path>
                                        }).collect_view()}
                                        {forecast_area_path.map(|path| view! {
                                            <path class="custom-area forecast" d=path></path>
                                        })}
                                        {band_path.map(|path| view! {
                                            <path class="uncertainty-band" d=path></path>
                                        })}
                                        {history_segments.iter().map(|segment| view! {
                                            <path
                                                class=format!("custom-line history {}", discharge_safety_class(segment.safety))
                                                d=segment.path.clone()
                                            ></path>
                                        }).collect_view()}
                                        {history_path.map(|path| {
                                            let stroke = history_colour.clone();
                                            view! {
                                                <path class="custom-line history" stroke=stroke d=path></path>
                                            }
                                        })}
                                        {forecast_segments.iter().map(|segment| view! {
                                            <path
                                                class=format!("custom-line forecast {}", discharge_safety_class(segment.safety))
                                                d=segment.path.clone()
                                            ></path>
                                        }).collect_view()}
                                        {forecast_path.map(|path| view! {
                                            <path class="custom-line forecast" d=path></path>
                                        })}
                                    </svg>
                                    {move || hovered.get().map(|metric| {
                                        let cursor_style = format!("left: {:.6}%;", metric.cursor_ratio * 100.0);
                                        let point_style = format!(
                                            "left: {:.6}%; top: {:.6}%;",
                                            metric.cursor_ratio * 100.0,
                                            metric.point_ratio * 100.0,
                                        );
                                        view! {
                                            <div class="hover-cursor" aria-hidden="true">
                                                <span class="hover-cursor-line" style=cursor_style></span>
                                                <span class="hover-cursor-point" style=point_style></span>
                                            </div>
                                        }
                                    })}
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
                <aside class="plot-current">
                    <div class="plot-current-heading">
                        <h3>{plot_title}</h3>
                    </div>
                    <div class="plot-readout">
                        <strong class=readout_value_class>{readout_value}</strong>
                        {move || discharge_comment().map(|(class, label)| view! {
                            <small class=format!("discharge-comment {class}")>{label}</small>
                        })}
                        <small class="readout-context">
                            {move || format!("{} {}", tr(locale, "Mesuré", "Measured"), readout_timestamp())}
                        </small>
                    </div>
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

fn is_visible_metric(kind: MetricKind) -> bool {
    !matches!(kind, MetricKind::WaterLevel)
}

fn metric_colour(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "#2563eb",
        MetricKind::WaterLevel => "#0f766e",
        MetricKind::Temperature => "#b7791f",
    }
}

fn discharge_safety_for_value(value: f64, reference_max: Option<f64>) -> DischargeSafety {
    let reference_max = reference_max
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(600.0);
    let safe_limit = reference_max / 3.0;
    let risk_limit = reference_max * 2.0 / 3.0;

    if value < safe_limit {
        DischargeSafety::Safe
    } else if value < risk_limit {
        DischargeSafety::Risky
    } else {
        DischargeSafety::NoSwim
    }
}

fn discharge_safety_class(safety: DischargeSafety) -> &'static str {
    match safety {
        DischargeSafety::Safe => "safety-safe",
        DischargeSafety::Risky => "safety-risky",
        DischargeSafety::NoSwim => "safety-noswim",
    }
}

fn discharge_safety_label(safety: DischargeSafety, locale: Locale) -> &'static str {
    match (locale, safety) {
        (Locale::Fr, DischargeSafety::Safe) => "Courant lent",
        (Locale::Fr, DischargeSafety::Risky) => "Attention courant fort",
        (Locale::Fr, DischargeSafety::NoSwim) => "Danger! Courant très fort!",
        (Locale::En, DischargeSafety::Safe) => "Slow current",
        (Locale::En, DischargeSafety::Risky) => "Strong current",
        (Locale::En, DischargeSafety::NoSwim) => "Danger! Very strong current!",
    }
}

fn axis_tick_class(kind: AxisTickKind) -> &'static str {
    match kind {
        AxisTickKind::Day => "axis-tick-mark day",
        AxisTickKind::Noon => "axis-tick-mark noon",
        AxisTickKind::Hour => "axis-tick-mark hour",
    }
}

fn axis_label_class(tick: &AxisTick) -> &'static str {
    match (tick.kind, tick.position < 4.0, tick.position > 96.0) {
        (AxisTickKind::Day, true, _) => "axis-label day start",
        (AxisTickKind::Noon, true, _) => "axis-label noon start",
        (AxisTickKind::Hour, true, _) => "axis-label hour start",
        (AxisTickKind::Day, _, true) => "axis-label day end",
        (AxisTickKind::Noon, _, true) => "axis-label noon end",
        (AxisTickKind::Hour, _, true) => "axis-label hour end",
        (AxisTickKind::Day, _, _) => "axis-label day",
        (AxisTickKind::Noon, _, _) => "axis-label noon",
        (AxisTickKind::Hour, _, _) => "axis-label hour",
    }
}

fn axis_hover_marker(
    x_range: Option<(f64, f64)>,
    hover_x: ReadSignal<Option<f64>>,
) -> Option<impl IntoView> {
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
}

fn metric_axis_title(kind: MetricKind, unit: &str, locale: Locale) -> String {
    let label = match (locale, kind) {
        (Locale::Fr, MetricKind::Discharge) => "Débit",
        (Locale::Fr, MetricKind::WaterLevel) => "Niveau",
        (Locale::Fr, MetricKind::Temperature) => "Température",
        (Locale::En, MetricKind::Discharge) => "Discharge",
        (Locale::En, MetricKind::WaterLevel) => "Water level",
        (Locale::En, MetricKind::Temperature) => "Temperature",
    };
    format!("{label} ({unit})")
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
                timestamp: format_swiss_timestamp(timestamp.timestamp()),
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
                timestamp: format_swiss_timestamp(timestamp.timestamp()),
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

fn metric_value_axis(
    kind: MetricKind,
    points: &[ChartPoint],
    uncertainty: Option<&MetricUncertainty>,
    current_value: Option<f64>,
    axis_floor: Option<f64>,
    axis_min_ceiling: Option<f64>,
) -> Option<ValueAxis> {
    if kind == MetricKind::Temperature {
        return Some(fixed_temperature_axis());
    }

    let (mut min, mut max) = points_y_range(points, uncertainty)?;
    if let Some(value) = current_value.filter(|value| value.is_finite()) {
        min = min.min(value);
        max = max.max(value);
    }
    if let Some(floor) = axis_floor {
        min = floor;
    }
    if let Some(min_ceiling) = axis_min_ceiling {
        max = max.max(min_ceiling);
    }

    Some(value_axis((min, max)))
}

fn fixed_temperature_axis() -> ValueAxis {
    let min = 5.0;
    let max = 30.0;
    let ticks = [5.0, 10.0, 15.0, 20.0, 25.0, 30.0]
        .into_iter()
        .map(|value| ValueTick {
            label: format_value(value),
            position: y_band_position(value, min, max),
        })
        .collect();

    ValueAxis { min, max, ticks }
}

fn value_axis((min, max): (f64, f64)) -> ValueAxis {
    let (mut min, mut max) = if min <= max { (min, max) } else { (max, min) };

    if (max - min).abs() < f64::EPSILON {
        let padding = (max.abs() * 0.05).max(1.0);
        min -= padding;
        max += padding;
    }

    let step = nice_step((max - min) / 4.0);
    let mut axis_min = (min / step).floor() * step;
    let mut axis_max = (max / step).ceil() * step;

    if (axis_max - axis_min).abs() < f64::EPSILON {
        axis_min -= step;
        axis_max += step;
    }

    let mut ticks = Vec::new();
    let mut value = axis_min;
    while value <= axis_max + step * 0.5 && ticks.len() < 8 {
        ticks.push(ValueTick {
            label: format_value(value),
            position: y_band_position(value, axis_min, axis_max),
        });
        value += step;
    }

    if ticks.len() < 2 {
        ticks = vec![
            ValueTick {
                label: format_value(axis_min),
                position: 100.0,
            },
            ValueTick {
                label: format_value(axis_max),
                position: 0.0,
            },
        ];
    }

    ValueAxis {
        min: axis_min,
        max: axis_max,
        ticks,
    }
}

fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }

    let exponent = 10_f64.powf(raw.log10().floor());
    let fraction = raw / exponent;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice_fraction * exponent
}

fn line_path<F>(
    points: &[ChartPoint],
    value_for: F,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
) -> Option<String>
where
    F: Fn(&ChartPoint) -> f64,
{
    let (start_x, end_x) = x_range?;
    let (min_y, max_y) = y_range?;
    if end_x <= start_x || max_y <= min_y {
        return None;
    }

    let mut path = String::new();
    let mut drawing = false;
    let mut drawn_points = 0;

    for point in points {
        let value = value_for(point);
        if !value.is_finite() || !(start_x..=end_x).contains(&point.x) {
            drawing = false;
            continue;
        }

        let x = ((point.x - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
        let y = y_band_position(value, min_y, max_y);
        if drawing {
            path.push_str(&format!(" L {:.3} {:.3}", x, y));
        } else {
            if !path.is_empty() {
                path.push(' ');
            }
            path.push_str(&format!("M {:.3} {:.3}", x, y));
            drawing = true;
        }
        drawn_points += 1;
    }

    (drawn_points >= 2).then_some(path)
}

fn area_path<F>(
    points: &[ChartPoint],
    value_for: F,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
) -> Option<String>
where
    F: Fn(&ChartPoint) -> f64,
{
    let (start_x, end_x) = x_range?;
    let (min_y, max_y) = y_range?;
    if end_x <= start_x || max_y <= min_y {
        return None;
    }

    let mut path = String::new();
    let mut drawn_points = 0;
    let mut run = Vec::<(f64, f64)>::new();

    for point in points {
        let value = value_for(point);
        if !value.is_finite() || !(start_x..=end_x).contains(&point.x) {
            append_area_run(&mut path, &mut run);
            continue;
        }

        let x = ((point.x - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
        let y = y_band_position(value, min_y, max_y);
        run.push((x, y));
        drawn_points += 1;
    }

    append_area_run(&mut path, &mut run);

    (drawn_points >= 2 && !path.is_empty()).then_some(path)
}

fn append_area_run(path: &mut String, run: &mut Vec<(f64, f64)>) {
    if run.len() < 2 {
        run.clear();
        return;
    }

    if !path.is_empty() {
        path.push(' ');
    }

    let (first_x, first_y) = run[0];
    path.push_str(&format!(
        "M {:.3} {:.3} L {:.3} {:.3}",
        first_x, SVG_PLOT_HEIGHT, first_x, first_y
    ));
    for (x, y) in run.iter().skip(1) {
        path.push_str(&format!(" L {:.3} {:.3}", x, y));
    }
    let last_x = run.last().map(|(x, _)| *x).unwrap_or(first_x);
    path.push_str(&format!(
        " L {:.3} {:.3} L {:.3} {:.3} Z",
        last_x, SVG_PLOT_HEIGHT, first_x, SVG_PLOT_HEIGHT
    ));
    run.clear();
}

fn segmented_line_paths<F>(
    points: &[ChartPoint],
    value_for: F,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
    risk_reference_max: Option<f64>,
) -> Vec<SegmentedPath>
where
    F: Fn(&ChartPoint) -> f64,
{
    let Some((start_x, end_x)) = x_range else {
        return Vec::new();
    };
    let Some((min_y, max_y)) = y_range else {
        return Vec::new();
    };
    if end_x <= start_x || max_y <= min_y {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut previous = None::<(f64, f64, f64)>;

    for point in points {
        let value = value_for(point);
        if !value.is_finite() || !(start_x..=end_x).contains(&point.x) {
            previous = None;
            continue;
        }

        let x = ((point.x - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
        let y = y_band_position(value, min_y, max_y);
        if let Some((previous_x, previous_y, previous_value)) = previous {
            let safety =
                discharge_safety_for_value((previous_value + value) / 2.0, risk_reference_max);
            segments.push(SegmentedPath {
                path: format!("M {:.3} {:.3} L {:.3} {:.3}", previous_x, previous_y, x, y),
                safety,
            });
        }
        previous = Some((x, y, value));
    }

    segments
}

fn segmented_area_paths<F>(
    points: &[ChartPoint],
    value_for: F,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
    risk_reference_max: Option<f64>,
) -> Vec<SegmentedPath>
where
    F: Fn(&ChartPoint) -> f64,
{
    let Some((start_x, end_x)) = x_range else {
        return Vec::new();
    };
    let Some((min_y, max_y)) = y_range else {
        return Vec::new();
    };
    if end_x <= start_x || max_y <= min_y {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut previous = None::<(f64, f64, f64)>;

    for point in points {
        let value = value_for(point);
        if !value.is_finite() || !(start_x..=end_x).contains(&point.x) {
            previous = None;
            continue;
        }

        let x = ((point.x - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
        let y = y_band_position(value, min_y, max_y);
        if let Some((previous_x, previous_y, previous_value)) = previous {
            let safety =
                discharge_safety_for_value((previous_value + value) / 2.0, risk_reference_max);
            segments.push(SegmentedPath {
                path: format!(
                    "M {:.3} {:.3} L {:.3} {:.3} L {:.3} {:.3} L {:.3} {:.3} Z",
                    previous_x, SVG_PLOT_HEIGHT, previous_x, previous_y, x, y, x, SVG_PLOT_HEIGHT
                ),
                safety,
            });
        }
        previous = Some((x, y, value));
    }

    segments
}

fn sun_bands((start_x, end_x): (f64, f64)) -> Vec<SunBand> {
    if end_x <= start_x {
        return Vec::new();
    }

    let Some(start_datetime) = DateTime::<Utc>::from_timestamp(start_x.floor() as i64, 0) else {
        return Vec::new();
    };
    let Some(end_datetime) = DateTime::<Utc>::from_timestamp(end_x.ceil() as i64, 0) else {
        return Vec::new();
    };
    let Some(mut date) = start_datetime
        .date_naive()
        .checked_sub_signed(Duration::days(1))
    else {
        return Vec::new();
    };
    let Some(last_date) = end_datetime
        .date_naive()
        .checked_add_signed(Duration::days(1))
    else {
        return Vec::new();
    };

    let mut bands = Vec::new();
    while date <= last_date {
        let Some(sunset) = solar_event_timestamp(date, false) else {
            date = match date.checked_add_signed(Duration::days(1)) {
                Some(next) => next,
                None => break,
            };
            continue;
        };
        let Some(next_date) = date.checked_add_signed(Duration::days(1)) else {
            break;
        };
        let Some(sunrise) = solar_event_timestamp(next_date, true) else {
            date = next_date;
            continue;
        };

        let band_start = sunset.max(start_x);
        let band_end = sunrise.min(end_x);
        if band_end > band_start {
            let x = ((band_start - start_x) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
            let width =
                ((band_end - band_start) / (end_x - start_x)).clamp(0.0, 1.0) * SVG_PLOT_WIDTH;
            bands.push(SunBand { x, width });
        }

        date = next_date;
    }

    bands
}

fn solar_event_timestamp(date: NaiveDate, sunrise: bool) -> Option<f64> {
    let day = date.ordinal() as f64;
    let longitude_hour = GENEVA_LONGITUDE / 15.0;
    let approximate_time = if sunrise {
        day + ((6.0 - longitude_hour) / 24.0)
    } else {
        day + ((18.0 - longitude_hour) / 24.0)
    };

    let mean_anomaly = 0.9856 * approximate_time - 3.289;
    let true_longitude = normalize_degrees(
        mean_anomaly
            + 1.916 * mean_anomaly.to_radians().sin()
            + 0.020 * (2.0 * mean_anomaly).to_radians().sin()
            + 282.634,
    );

    let mut right_ascension = (0.91764 * true_longitude.to_radians().tan())
        .atan()
        .to_degrees();
    right_ascension = normalize_degrees(right_ascension);
    let longitude_quadrant = (true_longitude / 90.0).floor() * 90.0;
    let ascension_quadrant = (right_ascension / 90.0).floor() * 90.0;
    right_ascension = (right_ascension + longitude_quadrant - ascension_quadrant) / 15.0;

    let sin_declination = 0.39782 * true_longitude.to_radians().sin();
    let cos_declination = sin_declination.asin().cos();
    let latitude = GENEVA_LATITUDE.to_radians();
    let cos_hour_angle = (SUNRISE_SUNSET_ZENITH_DEGREES.to_radians().cos()
        - sin_declination * latitude.sin())
        / (cos_declination * latitude.cos());

    if !(-1.0..=1.0).contains(&cos_hour_angle) {
        return None;
    }

    let hour_angle = if sunrise {
        360.0 - cos_hour_angle.acos().to_degrees()
    } else {
        cos_hour_angle.acos().to_degrees()
    } / 15.0;

    let local_mean_time = hour_angle + right_ascension - 0.06571 * approximate_time - 6.622;
    let utc_hour = normalize_hours(local_mean_time - longitude_hour);
    let seconds = (utc_hour * 3600.0).round() as i64;
    let midnight = date.and_hms_opt(0, 0, 0)?;
    Some(
        DateTime::<Utc>::from_naive_utc_and_offset(midnight, Utc).timestamp() as f64
            + seconds as f64,
    )
}

fn normalize_degrees(value: f64) -> f64 {
    value.rem_euclid(360.0)
}

fn normalize_hours(value: f64) -> f64 {
    value.rem_euclid(24.0)
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
    ((max_y - value) / (max_y - min_y)).clamp(0.0, 1.0) * SVG_PLOT_HEIGHT
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
            let value = if point.history_y.is_finite() {
                point.history_y
            } else if point.forecast_y.is_finite() {
                point.forecast_y
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

fn sync_chart_scroll(event: web_sys::Event) {
    let Some(source) = event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let scroll_left = source.scroll_left();
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(nodes) = document.query_selector_all(".chart-scroll-sync") else {
        return;
    };

    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            continue;
        };
        if element.scroll_left() != scroll_left {
            element.set_scroll_left(scroll_left);
        }
    }
}

fn default_visible_x_range(history: &[MetricSeries], full_range: (f64, f64)) -> (f64, f64) {
    let (start, end) = full_range;
    let latest_history = history
        .iter()
        .flat_map(|series| series.points.iter())
        .filter_map(|point| DateTime::parse_from_rfc3339(&point.timestamp).ok())
        .map(|timestamp| timestamp.timestamp() as f64)
        .fold(None, |latest: Option<f64>, timestamp| {
            Some(latest.map_or(timestamp, |latest| latest.max(timestamp)))
        });

    let visible_end = latest_history
        .map(|timestamp| (timestamp + FORECAST_VISIBLE_SECONDS).min(end))
        .unwrap_or(end)
        .max(start);

    if visible_end <= start {
        (start, end)
    } else {
        (start, visible_end)
    }
}

fn scroll_content_width(
    full_range: Option<(f64, f64)>,
    visible_range: Option<(f64, f64)>,
) -> String {
    let Some((full_start, full_end)) = full_range else {
        return "100%".to_string();
    };
    let Some((visible_start, visible_end)) = visible_range else {
        return "100%".to_string();
    };
    let full_span = full_end - full_start;
    let visible_span = visible_end - visible_start;
    if full_span <= 0.0 || visible_span <= 0.0 {
        return "100%".to_string();
    }

    let percent = (full_span / visible_span * 100.0).clamp(100.0, 320.0);
    format!("{percent:.3}%")
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
    if span <= 0 {
        return None;
    }
    let start_offset_seconds = swiss_offset_seconds(start_timestamp);
    let step_seconds = 6 * 60 * 60;
    let start_local = start_timestamp + start_offset_seconds;
    let end_local = end_timestamp + swiss_offset_seconds(end_timestamp);
    let mut tick_local = start_local - start_local.rem_euclid(step_seconds);
    if tick_local < start_local {
        tick_local += step_seconds;
    }
    let dense_labels = span <= 3 * 24 * 60 * 60;
    let mut ticks = Vec::new();

    while tick_local <= end_local {
        let timestamp = swiss_local_to_utc_timestamp(tick_local);
        let local_datetime = DateTime::<Utc>::from_timestamp(tick_local, 0)?;
        let hour = local_datetime.hour();
        let kind = if hour == 0 {
            AxisTickKind::Day
        } else if hour == 12 {
            AxisTickKind::Noon
        } else {
            AxisTickKind::Hour
        };
        let label = match hour {
            0 => local_datetime.format("%d.%m").to_string(),
            12 => local_datetime.format("%H").to_string(),
            6 | 18 if dense_labels => local_datetime.format("%H").to_string(),
            _ => String::new(),
        };
        let position = ((timestamp - start_timestamp) as f64 / span as f64).clamp(0.0, 1.0) * 100.0;
        ticks.push(AxisTick {
            label,
            position,
            kind,
        });
        tick_local += step_seconds;
    }

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
        .map(|datetime| format_swiss_timestamp(datetime.timestamp()))
        .unwrap_or_else(|_| value.to_string())
}

fn format_swiss_timestamp(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp + swiss_offset_seconds(timestamp), 0)
        .map(|datetime| datetime.format("%d.%m %H:%M").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn format_swiss_now_seconds() -> String {
    let timestamp = (js_sys::Date::new_0().get_time() / 1_000.0).floor() as i64;
    format_swiss_timestamp_seconds(timestamp)
}

fn format_swiss_timestamp_seconds(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp + swiss_offset_seconds(timestamp), 0)
        .map(|datetime| datetime.format("%d.%m %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn swiss_local_to_utc_timestamp(local_timestamp: i64) -> i64 {
    let mut timestamp = local_timestamp - swiss_offset_seconds(local_timestamp);

    for _ in 0..4 {
        let next = local_timestamp - swiss_offset_seconds(timestamp);
        if next == timestamp {
            break;
        }
        timestamp = next;
    }

    timestamp
}

fn swiss_offset_seconds(timestamp: i64) -> i64 {
    let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
        return 60 * 60;
    };
    let year = datetime.year();
    let Some(summer_start) = swiss_summer_time_transition(year, 3) else {
        return 60 * 60;
    };
    let Some(summer_end) = swiss_summer_time_transition(year, 10) else {
        return 60 * 60;
    };

    if timestamp >= summer_start && timestamp < summer_end {
        2 * 60 * 60
    } else {
        60 * 60
    }
}

fn swiss_summer_time_transition(year: i32, month: u32) -> Option<i64> {
    let last_day = NaiveDate::from_ymd_opt(year, month, 31)?;
    let last_sunday = last_day.checked_sub_signed(Duration::days(i64::from(
        last_day.weekday().num_days_from_sunday(),
    )))?;

    Some(
        DateTime::<Utc>::from_naive_utc_and_offset(last_sunday.and_hms_opt(1, 0, 0)?, Utc)
            .timestamp(),
    )
}

fn tr(locale: Locale, fr: &'static str, en: &'static str) -> &'static str {
    match locale {
        Locale::Fr => fr,
        Locale::En => en,
    }
}
