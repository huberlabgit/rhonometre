use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc};
use dioxus::events::{MouseEvent, ScrollEvent};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
use gloo_net::http::Request;
#[cfg(target_arch = "wasm32")]
use gloo_timers::future::TimeoutFuture;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

const REFRESH_MS: u32 = 120_000;
const SVG_PLOT_WIDTH: f64 = 1000.0;
const SVG_PLOT_HEIGHT: f64 = 100.0;
const GENEVA_LATITUDE: f64 = 46.2044;
const GENEVA_LONGITUDE: f64 = 6.1432;
const SUNRISE_SUNSET_ZENITH_DEGREES: f64 = 90.833;
#[cfg(target_arch = "wasm32")]
const PRO_TOKEN_STORAGE_KEY: &str = "rhonometre_pro_token";
const APP_CSS: &str = include_str!("styles.css");

fn main() {
    #[cfg(target_arch = "wasm32")]
    {
        console_error_panic_hook::set_once();
        register_service_worker();
        dioxus::LaunchBuilder::web()
            .with_cfg(dioxus::web::Config::new().rootname("app"))
            .launch(App);
    }

    #[cfg(not(target_arch = "wasm32"))]
    dioxus::launch(App);
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct SourceInfo {
    label: String,
    url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
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
    #[serde(default)]
    notice_fr: Option<String>,
    #[serde(default)]
    notice_en: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WaterKind {
    River,
    Lake,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct CurrentMetric {
    kind: MetricKind,
    #[serde(default)]
    label_fr: Option<String>,
    #[serde(default)]
    label_en: Option<String>,
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct MetricSeries {
    kind: MetricKind,
    label_fr: String,
    label_en: String,
    unit: String,
    points: Vec<HistoryPoint>,
    #[serde(default)]
    uncertainty: Option<MetricUncertainty>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct MetricUncertainty {
    lower: f64,
    upper: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct HistoryPoint {
    timestamp: String,
    value: f64,
}

#[derive(Clone, Debug, Serialize)]
struct ProAuthRequest {
    code: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ProAuthResponse {
    token: String,
    expires_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Locale {
    Fr,
    En,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DischargeSafety {
    Safe,
    Risky,
    NoSwim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AxisTickKind {
    Day,
    Noon,
    Hour,
}

#[derive(Clone, Debug, PartialEq)]
struct AxisTick {
    label: String,
    timestamp: f64,
    position: f64,
    kind: AxisTickKind,
}

#[derive(Clone, Debug, PartialEq)]
struct ValueAxis {
    min: f64,
    max: f64,
    ticks: Vec<ValueTick>,
}

#[derive(Clone, Debug, PartialEq)]
struct ValueTick {
    label: String,
    value: f64,
    position: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct RiskAxisSegment {
    class_name: &'static str,
    top: f64,
    height: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct TimedPoint {
    timestamp: f64,
    value: f64,
    label: String,
}

#[derive(Clone, Debug, PartialEq)]
struct TimeDomain {
    min: f64,
    max: f64,
    visible_max: f64,
    ticks: Vec<AxisTick>,
    sun_bands: Vec<SunBand>,
    content_width_percent: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct SunBand {
    x: f64,
    width: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HoverState {
    timestamp: f64,
    position: f64,
}

#[component]
fn App() -> Element {
    let mut locale = use_signal(|| Locale::Fr);
    let selected_station = use_signal(|| "2606".to_string());
    let mut focus_mode = use_signal(initial_focus_mode);
    #[allow(unused_mut)]
    let mut live_clock = use_signal(format_swiss_now_seconds);
    let mut refresh_version = use_signal(|| 0_u64);
    let mut pro_token = use_signal(read_stored_pro_token);
    let mut pro_signin_open = use_signal(|| false);
    let mut pro_code = use_signal(String::new);
    let mut pro_error = use_signal(|| None::<String>);
    let mut pro_pending = use_signal(|| false);

    #[cfg(target_arch = "wasm32")]
    use_future(move || async move {
        loop {
            TimeoutFuture::new(1_000).await;
            live_clock.set(format_swiss_now_seconds());
        }
    });

    #[cfg(not(target_arch = "wasm32"))]
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            live_clock.set(format_swiss_now_seconds());
        }
    });

    #[cfg(not(target_arch = "wasm32"))]
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(u64::from(REFRESH_MS))).await;
            *refresh_version.write() += 1;
        }
    });

    #[cfg(target_arch = "wasm32")]
    use_future(move || async move {
        loop {
            TimeoutFuture::new(REFRESH_MS).await;
            *refresh_version.write() += 1;
        }
    });

    let dashboard = use_resource(move || {
        let token = pro_token();
        let _refresh = refresh_version();
        async move { load_dashboard(token).await }
    });

    let dashboard_result = dashboard.read().clone();
    let pro_enabled = pro_token().is_some();
    let app_class = if focus_mode() {
        "app-shell focus-mode"
    } else {
        "app-shell"
    };

    rsx! {
        document::Style { "{APP_CSS}" }

        div { class: "{app_class}",
            button {
                class: "focus-toggle-button icon-button",
                r#type: "button",
                title: if focus_mode() {
                    tr(locale(), "Quitter le mode focus", "Exit focus mode")
                } else {
                    tr(locale(), "Mode focus", "Focus mode")
                },
                onclick: move |_| {
                    let next = !focus_mode();
                    focus_mode.set(next);
                    store_focus_mode(next);
                },
                FocusIcon { active: focus_mode() }
            }

                div { class: "topbar",
                    div { class: "brand-lockup",
                        h1 { "rhonometre" }
                        div { class: "partner-mark",
                            span { class: "partner-logo-icon", "" }
                            span { "Pontonniers de Genève" }
                        }
                    }
                    div { class: "topbar-actions",
                    div { class: "page-live-clock", "{live_clock}" }
                    div { class: "segmented",
                        button {
                            class: if locale() == Locale::Fr { "active" } else { "" },
                            r#type: "button",
                            onclick: move |_| locale.set(Locale::Fr),
                            "FR"
                        }
                        button {
                            class: if locale() == Locale::En { "active" } else { "" },
                            r#type: "button",
                            onclick: move |_| locale.set(Locale::En),
                            "EN"
                        }
                    }
                    button {
                        class: if pro_enabled { "refresh-button active" } else { "refresh-button ghost" },
                        r#type: "button",
                        onclick: move |_| {
                            if pro_token().is_some() {
                                pro_token.set(None);
                                store_pro_token(None);
                                pro_signin_open.set(false);
                            } else {
                                pro_signin_open.set(!pro_signin_open());
                            }
                        },
                        if pro_enabled {
                            "PRO"
                        } else {
                            "Pro"
                        }
                    }
                    button {
                        class: "refresh-button icon-button",
                        r#type: "button",
                        title: tr(locale(), "Actualiser", "Refresh"),
                        disabled: dashboard.pending(),
                        onclick: move |_| *refresh_version.write() += 1,
                        RefreshIcon {}
                    }
                }
            }

            if pro_signin_open() && !pro_enabled {
                div { class: "pro-signin",
                    strong { "Pro" }
                    input {
                        r#type: "password",
                        placeholder: tr(locale(), "code d'accès", "access code"),
                        value: "{pro_code}",
                        oninput: move |event| {
                            pro_code.set(event.value());
                            pro_error.set(None);
                        },
                    }
                    button {
                        class: "refresh-button",
                        r#type: "button",
                        disabled: pro_pending(),
                        onclick: move |_| {
                            let code = pro_code().trim().to_string();
                            if code.is_empty() {
                                pro_error.set(Some(tr(locale(), "Code requis", "Code required").to_string()));
                                return;
                            }
                            pro_pending.set(true);
                            pro_error.set(None);
                            spawn(async move {
                                match authenticate_pro(code).await {
                                    Ok(response) => {
                                        let _expires_at = response.expires_at;
                                        store_pro_token(Some(&response.token));
                                        pro_token.set(Some(response.token));
                                        pro_code.set(String::new());
                                        pro_signin_open.set(false);
                                    }
                                    Err(err) => pro_error.set(Some(err)),
                                }
                                pro_pending.set(false);
                            });
                        },
                        if pro_pending() {
                            {tr(locale(), "Connexion...", "Signing in...")}
                        } else {
                            {tr(locale(), "Connexion", "Sign in")}
                        }
                    }
                    if let Some(error) = pro_error() {
                        span { class: "error-text", "{error}" }
                    }
                }
            }

            match dashboard_result {
                Some(Ok(data)) => rsx! {
                    DashboardView {
                        data,
                        locale: locale(),
                        selected_station,
                        focus_mode: focus_mode(),
                        pro_enabled,
                    }
                },
                Some(Err(error)) => rsx! {
                    div { class: "empty-state",
                        h2 { "{tr(locale(), \"Données indisponibles\", \"Data unavailable\")}" }
                        p { class: "error-text", "{error}" }
                    }
                },
                None => rsx! {
                    div { class: "empty-state",
                        div { class: "loading-mark" }
                        p { class: "status-line", "{tr(locale(), \"Chargement des mesures\", \"Loading measurements\")}" }
                    }
                },
            }
        }
    }
}

#[component]
fn DashboardView(
    data: DashboardData,
    locale: Locale,
    mut selected_station: Signal<String>,
    focus_mode: bool,
    pro_enabled: bool,
) -> Element {
    let river_stations = data
        .stations
        .iter()
        .filter(|station| station.kind == WaterKind::River)
        .cloned()
        .collect::<Vec<_>>();
    let selected = river_stations
        .iter()
        .find(|station| station.id == selected_station())
        .or_else(|| river_stations.first())
        .cloned();

    rsx! {
        div { class: if focus_mode { "dashboard dashboard-focus" } else { "dashboard" },
            if let Some(station) = selected {
                StationPanel {
                    station,
                    locale,
                    pro_enabled,
                }
            }

            if !focus_mode {
                div { class: "station-tabs station-tabs-bottom",
                    for station in river_stations {
                        button {
                            class: if station.id == selected_station() { "station-tab active" } else { "station-tab" },
                            r#type: "button",
                            onclick: move |_| selected_station.set(station.id.clone()),
                            strong { "{station_title(&station, locale)}" }
                            small { "{station_subtitle(&station, locale)}" }
                        }
                    }
                }

                SourceStrip {
                    data,
                    locale,
                }
            }
        }
    }
}

#[component]
fn SourceStrip(data: DashboardData, locale: Locale) -> Element {
    let updated = format_timestamp(&data.generated_at, locale);
    let cache_class = match data.cache_status {
        CacheStatus::Fresh => "cache fresh",
        CacheStatus::Stale => "cache stale",
    };
    let cache_label = match data.cache_status {
        CacheStatus::Fresh => tr(locale, "frais", "fresh"),
        CacheStatus::Stale => tr(locale, "périmé", "stale"),
    };

    rsx! {
        div { class: "source-strip source-strip-footer",
            div {
                span { class: "label", "{tr(locale, \"Sources\", \"Sources\")}" }
                a { href: "{data.source.url}", target: "_blank", rel: "noreferrer", "{data.source.label}" }
            }
            div {
                span { class: "label", "{tr(locale, \"Dernière mise à jour\", \"Last updated\")}" }
                span { "{updated}" }
            }
            span { class: "{cache_class}", "{cache_label}" }
            if !data.warnings.is_empty() {
                span { class: "status-line", "{data.warnings.len()} {tr(locale, \"avert.\", \"warn.\")}" }
            }
        }
    }
}

#[component]
fn StationPanel(station: StationData, locale: Locale, pro_enabled: bool) -> Element {
    let hover_state = use_signal(|| None::<HoverState>);
    let domain = station_time_domain(&station, pro_enabled);
    let discharge_history = series_for_kind(&station.history, MetricKind::Discharge);
    let discharge_forecast = if pro_enabled {
        series_for_kind(&station.forecast, MetricKind::Discharge)
    } else {
        None
    };
    let temperature_history = series_for_kind(&station.history, MetricKind::Temperature);
    let temperature_forecast = if pro_enabled {
        series_for_kind(&station.forecast, MetricKind::Temperature)
    } else {
        None
    };
    let station_notice = match locale {
        Locale::Fr => station.notice_fr.clone(),
        Locale::En => station.notice_en.clone(),
    };
    let latest_measurement = latest_station_measurement(&station, locale);

    rsx! {
        article { class: "station-panel",
            div { class: "station-heading",
                div {
                    h2 { "{station_title(&station, locale)}" }
                    p { class: "location-subtitle", "{station_subtitle(&station, locale)}" }
                    if let Some(latest_measurement) = latest_measurement {
                        p { class: "station-last-measure",
                            span { "{tr(locale, \"Dernière mesure\", \"Latest measurement\")}" }
                            time { "{latest_measurement}" }
                        }
                    }
                }
            }

            div { class: "chart-stack",
                TimeAxis {
                    domain: domain.clone(),
                    placement: "top".to_string(),
                    locale,
                    hover_state,
                }

                {metric_chart(
                    &station,
                    MetricKind::Temperature,
                    temperature_history,
                    temperature_forecast,
                    &domain,
                    locale,
                    hover_state,
                )}

                TimeAxis {
                    domain: domain.clone(),
                    placement: "middle".to_string(),
                    locale,
                    hover_state,
                }

                {metric_chart(
                    &station,
                    MetricKind::Discharge,
                    discharge_history,
                    discharge_forecast,
                    &domain,
                    locale,
                    hover_state,
                )}

                TimeAxis {
                    domain,
                    placement: "bottom".to_string(),
                    locale,
                    hover_state,
                }
            }

            if let Some(notice) = station_notice {
                p { class: "station-footnote",
                    strong { "{tr(locale, \"Note\", \"Note\")}" }
                    span { "{notice}" }
                }
            }
        }
    }
}

#[component]
fn TimeAxis(
    domain: TimeDomain,
    placement: String,
    locale: Locale,
    hover_state: Signal<Option<HoverState>>,
) -> Element {
    let axis_class = format!("shared-time-axis time-axis-{placement}");
    let width_style = format!(
        "width: {:.3}%; min-width: var(--chart-content-min-width, 100%);",
        domain.content_width_percent
    );
    let hover_position = hover_state()
        .map(|hover| hover.position)
        .filter(|position| (0.0..=100.0).contains(position));

    rsx! {
        div { class: "{axis_class}",
            div { class: "shared-axis-frame",
                if placement == "top" {
                    div { class: "axis-range-label", "{time_axis_range_label(&domain, locale)}" }
                }
                div {
                    class: "axis-scroll-viewport scroll-sync",
                    onscroll: move |event| sync_chart_scroll(event),
                    div { class: "axis-scroll-content", style: "{width_style}",
                        div { class: "axis-rule axis-rule-top",
                            for tick in domain.ticks.iter() {
                                span {
                                    class: format!("axis-tick-mark {}", tick_kind_class(tick.kind)),
                                    style: format!("left: {:.3}%;", tick.position),
                                }
                            }
                            if let Some(position) = hover_position {
                                span {
                                    class: "axis-hover-tick",
                                    style: format!("left: {:.3}%;", position),
                                }
                            }
                        }
                        div { class: "axis-labels",
                            for tick in domain.ticks.iter() {
                                span {
                                    class: "{axis_label_class(tick)}",
                                    style: format!("left: {:.3}%;", tick.position),
                                    span { class: "axis-label-full", "{axis_label_full(tick)}" }
                                    span { class: "axis-label-wide", "{axis_label_wide(tick, locale)}" }
                                    span { class: "axis-label-medium", "{axis_label_medium(tick, locale)}" }
                                    span { class: "axis-label-short", "{axis_label_short(tick, locale)}" }
                                }
                            }
                        }
                        div { class: "axis-rule axis-rule-bottom",
                            for tick in domain.ticks.iter() {
                                span {
                                    class: format!("axis-tick-mark {}", tick_kind_class(tick.kind)),
                                    style: format!("left: {:.3}%;", tick.position),
                                }
                            }
                            if let Some(position) = hover_position {
                                span {
                                    class: "axis-hover-tick",
                                    style: format!("left: {:.3}%;", position),
                                }
                            }
                        }
                    }
                }
            }
            div { class: "axis-spacer" }
        }
    }
}

fn metric_chart(
    station: &StationData,
    kind: MetricKind,
    history: Option<&MetricSeries>,
    forecast: Option<&MetricSeries>,
    domain: &TimeDomain,
    locale: Locale,
    mut hover_state: Signal<Option<HoverState>>,
) -> Element {
    let history_points = history
        .map(|series| timed_points(&series.points))
        .unwrap_or_default();
    let forecast_points = forecast
        .map(|series| timed_points(&series.points))
        .unwrap_or_default();
    let history_points = points_in_domain(&history_points, domain);
    let forecast_points = points_in_domain(&forecast_points, domain);
    let unit = history
        .or(forecast)
        .map(|series| series.unit.clone())
        .or_else(|| current_for_kind(station, kind).map(|metric| metric.unit.clone()))
        .unwrap_or_else(|| default_unit(kind).to_string());
    let axis = value_axis(station, kind, &history_points, &forecast_points);
    let width_style = format!(
        "width: {:.3}%; min-width: var(--chart-content-min-width, 100%);",
        domain.content_width_percent
    );
    let current = current_for_kind(station, kind);
    let hovered = hover_state()
        .and_then(|hover| sample_point_at(hover.timestamp, &history_points, &forecast_points));
    let readout_value = hovered
        .as_ref()
        .map(|point| point.value)
        .or_else(|| current.map(|metric| metric.value));
    let formatted_value = readout_value
        .map(|value| format_metric_value(value, &unit, kind))
        .unwrap_or_else(|| "—".to_string());
    let discharge_reference = if kind == MetricKind::Discharge {
        Some(discharge_reference_max(station))
    } else {
        None
    };
    let safety = if kind == MetricKind::Discharge {
        discharge_reference
            .and_then(|reference| readout_value.map(|value| discharge_safety(value, reference)))
    } else {
        None
    };
    let risk_axis_segments = discharge_reference
        .map(|reference| discharge_risk_axis_segments(&axis, reference))
        .unwrap_or_default();
    let y_axis_class = if risk_axis_segments.is_empty() {
        "custom-y-axis"
    } else {
        "custom-y-axis has-risk-scale"
    };
    let y_axis_right_class = if risk_axis_segments.is_empty() {
        "custom-y-axis custom-y-axis-right"
    } else {
        "custom-y-axis custom-y-axis-right has-risk-scale"
    };
    let chart_class = safety
        .map(|safety| {
            format!(
                "chart-card {} {}",
                metric_kind_class(kind),
                safety_class(safety)
            )
        })
        .unwrap_or_else(|| format!("chart-card {}", metric_kind_class(kind)));
    let readout_class = safety
        .map(|safety| format!("readout-value {}", safety_class(safety)))
        .unwrap_or_else(|| "readout-value".to_string());
    let hover_time = hovered
        .as_ref()
        .map(|point| format_timestamp_from_seconds(point.timestamp, locale));
    let hover_position = hover_state()
        .map(|hover| hover.position)
        .filter(|position| (0.0..=100.0).contains(position));
    let hover_point = hover_state().and_then(|hover| {
        sample_point_at(hover.timestamp, &history_points, &forecast_points)
            .map(|point| (hover.position, value_position(point.value, &axis)))
    });
    let domain_min = domain.min;
    let domain_span = (domain.max - domain.min).max(1.0);
    let history_path = line_path(&history_points, domain, &axis);
    let history_area = area_path(&history_points, domain, &axis);
    let forecast_path = line_path(&forecast_points, domain, &axis);
    let forecast_area = area_path(&forecast_points, domain, &axis);
    let show_uncertainty = station.id != "2606";
    let uncertainty_path = history
        .and_then(|series| series.uncertainty.as_ref())
        .filter(|_| show_uncertainty)
        .and_then(|uncertainty| uncertainty_band_path(&history_points, uncertainty, domain, &axis));

    rsx! {
        div { class: "{chart_class}",
            div { class: "plot-row",
                div { class: "chart-frame",
                    div { class: "custom-chart",
                        div { class: "{y_axis_class}",
                            div { class: "custom-y-axis-title", "{axis_title(kind, locale, &unit)}" }
                            if !risk_axis_segments.is_empty() {
                                div { class: "risk-axis-scale", aria_hidden: "true",
                                    for segment in risk_axis_segments.iter() {
                                        div {
                                            class: format!("risk-axis-segment {}", segment.class_name),
                                            style: format!("top: {:.3}%; height: {:.3}%;", segment.top, segment.height),
                                        }
                                    }
                                }
                            }
                            for tick in axis.ticks.iter() {
                                span {
                                    class: value_tick_class(kind, tick, discharge_reference),
                                    style: format!("top: {:.3}%;", tick.position),
                                    "{tick.label}"
                                }
                            }
                        }
                        div { class: "{y_axis_right_class}",
                            div { class: "custom-y-axis-title", "{axis_title(kind, locale, &unit)}" }
                            if !risk_axis_segments.is_empty() {
                                div { class: "risk-axis-scale", aria_hidden: "true",
                                    for segment in risk_axis_segments.iter() {
                                        div {
                                            class: format!("risk-axis-segment {}", segment.class_name),
                                            style: format!("top: {:.3}%; height: {:.3}%;", segment.top, segment.height),
                                        }
                                    }
                                }
                            }
                            for tick in axis.ticks.iter() {
                                span {
                                    class: value_tick_class(kind, tick, discharge_reference),
                                    style: format!("top: {:.3}%;", tick.position),
                                    "{tick.label}"
                                }
                            }
                        }

                        div { class: "custom-plot-area",
                            div {
                                class: "plot-scroll-viewport scroll-sync",
                                onscroll: move |event| sync_chart_scroll(event),
                                div {
                                    class: "plot-scroll-content",
                                    style: "{width_style}",
                                    svg {
                                        view_box: "0 0 1000 100",
                                        preserve_aspect_ratio: "none",
                                        g { class: "sun-shading",
                                            for band in domain.sun_bands.iter() {
                                                rect {
                                                    x: format!("{:.3}", band.x * SVG_PLOT_WIDTH / 100.0),
                                                    y: "0",
                                                    width: format!("{:.3}", band.width * SVG_PLOT_WIDTH / 100.0),
                                                    height: "{SVG_PLOT_HEIGHT}",
                                                }
                                            }
                                        }
                                        g { class: "custom-grid-y",
                                            for tick in axis.ticks.iter() {
                                                line {
                                                    x1: "0",
                                                    x2: "{SVG_PLOT_WIDTH}",
                                                    y1: format!("{:.3}", tick.position),
                                                    y2: format!("{:.3}", tick.position),
                                                }
                                            }
                                        }
                                        g { class: "custom-grid-x",
                                            for tick in domain.ticks.iter() {
                                                line {
                                                    x1: format!("{:.3}", tick.position * SVG_PLOT_WIDTH / 100.0),
                                                    x2: format!("{:.3}", tick.position * SVG_PLOT_WIDTH / 100.0),
                                                    y1: "0",
                                                    y2: "{SVG_PLOT_HEIGHT}",
                                                }
                                            }
                                        }
                                        if let Some(path) = uncertainty_path {
                                            path {
                                                class: "uncertainty-band",
                                                d: "{path}",
                                            }
                                        }
                                        if kind == MetricKind::Discharge {
                                            if let Some(area) = history_area {
                                                path {
                                                    class: "custom-area discharge",
                                                    d: "{area}",
                                                }
                                            }
                                            if let Some(path) = history_path {
                                                path {
                                                    class: "custom-line discharge",
                                                    d: "{path}",
                                                }
                                            }
                                        } else {
                                            if let Some(path) = history_path {
                                                path {
                                                    class: "custom-line temperature",
                                                    d: "{path}",
                                                }
                                            }
                                        }
                                        if kind != MetricKind::Temperature {
                                            if let Some(area) = forecast_area {
                                                path {
                                                    class: "custom-area forecast",
                                                    d: "{area}",
                                                }
                                            }
                                        }
                                        if let Some(path) = forecast_path {
                                            path {
                                                class: format!("custom-line forecast {}", metric_kind_class(kind)),
                                                d: "{path}",
                                            }
                                        }
                                    }

                                    div {
                                        class: "hover-capture",
                                        onmousemove: move |event: MouseEvent| {
                                            if let Some(ratio) = mouse_ratio(&event) {
                                                let timestamp = domain_min + ratio * domain_span;
                                                hover_state.set(Some(HoverState {
                                                    timestamp,
                                                    position: ratio * 100.0,
                                                }));
                                            }
                                        },
                                        onmouseleave: move |_| hover_state.set(None),
                                    }

                                    if let Some(position) = hover_position {
                                        div { class: "hover-cursor",
                                            div {
                                                class: "hover-cursor-line",
                                                style: format!("left: {:.3}%;", position),
                                            }
                                            if let Some((x, y)) = hover_point {
                                                div {
                                                    class: "hover-cursor-point",
                                                    style: format!("left: {:.3}%; top: {:.3}%;", x, y),
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "plot-current",
                    div { class: "plot-current-heading",
                        h3 { "{metric_title(kind, locale)}" }
                        if let Some(hover_time) = hover_time.clone() {
                            small { "{hover_time}" }
                        }
                    }
                    div { class: "plot-readout",
                        strong { class: "{readout_class}", "{formatted_value}" }
                        if let Some(safety) = safety {
                            span {
                                class: format!("discharge-comment {}", safety_class(safety)),
                                "{safety_label(safety, locale)}"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn station_time_domain(_station: &StationData, _pro_enabled: bool) -> TimeDomain {
    let today = Local::now().date_naive();
    let end_date = today + Duration::days(1);
    let start_date = end_date - Duration::days(5);
    let fallback_end = Local::now() + Duration::hours(12);
    let end = local_datetime(end_date, 0, 0, 0).unwrap_or(fallback_end);
    let start = local_datetime(start_date, 0, 0, 0).unwrap_or(end - Duration::days(5));
    let min = start.timestamp() as f64;
    let max = end.timestamp() as f64;

    TimeDomain {
        min,
        max,
        visible_max: max,
        ticks: time_ticks(min, max),
        sun_bands: sun_bands(min, max),
        content_width_percent: 100.0,
    }
}

fn series_for_kind(series: &[MetricSeries], kind: MetricKind) -> Option<&MetricSeries> {
    series.iter().find(|series| series.kind == kind)
}

fn current_for_kind(station: &StationData, kind: MetricKind) -> Option<&CurrentMetric> {
    station.current.iter().find(|metric| metric.kind == kind)
}

fn latest_station_measurement(station: &StationData, locale: Locale) -> Option<String> {
    station
        .current
        .iter()
        .filter_map(|metric| {
            timestamp_seconds(&metric.measured_at)
                .map(|timestamp| (timestamp, metric.measured_at.as_str()))
        })
        .chain(station.history.iter().flat_map(|series| {
            series.points.iter().filter_map(|point| {
                timestamp_seconds(&point.timestamp)
                    .map(|timestamp| (timestamp, point.timestamp.as_str()))
            })
        }))
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, timestamp)| format_timestamp(timestamp, locale))
}

fn timed_points(points: &[HistoryPoint]) -> Vec<TimedPoint> {
    points
        .iter()
        .filter_map(|point| {
            let timestamp = timestamp_seconds(&point.timestamp)?;
            Some(TimedPoint {
                timestamp,
                value: point.value,
                label: point.timestamp.clone(),
            })
        })
        .collect()
}

fn points_in_domain(points: &[TimedPoint], domain: &TimeDomain) -> Vec<TimedPoint> {
    points
        .iter()
        .filter(|point| point.timestamp >= domain.min && point.timestamp <= domain.max)
        .cloned()
        .collect()
}

fn timestamp_seconds(value: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis() as f64 / 1000.0)
}

fn value_axis(
    station: &StationData,
    kind: MetricKind,
    history: &[TimedPoint],
    forecast: &[TimedPoint],
) -> ValueAxis {
    match kind {
        MetricKind::Temperature => ValueAxis {
            min: 5.0,
            max: 30.0,
            ticks: vec![
                ValueTick {
                    label: "30".to_string(),
                    value: 30.0,
                    position: 0.0,
                },
                ValueTick {
                    label: "20".to_string(),
                    value: 20.0,
                    position: value_position(20.0, &ValueAxis::bare(5.0, 30.0)),
                },
                ValueTick {
                    label: "10".to_string(),
                    value: 10.0,
                    position: value_position(10.0, &ValueAxis::bare(5.0, 30.0)),
                },
                ValueTick {
                    label: "5".to_string(),
                    value: 5.0,
                    position: 100.0,
                },
            ],
        },
        MetricKind::Discharge => {
            let observed_max = history
                .iter()
                .chain(forecast.iter())
                .map(|point| point.value)
                .chain(current_for_kind(station, kind).map(|metric| metric.value))
                .reduce(f64::max)
                .unwrap_or(0.0);
            let base_max: f64 = if station.id == "2170" { 150.0 } else { 600.0 };
            let max = base_max.max((observed_max * 1.08).ceil());
            let mut ticks = Vec::new();
            for fraction in [1.0, 0.75, 0.5, 0.25, 0.0] {
                let value = max * fraction;
                ticks.push(ValueTick {
                    label: format!("{value:.0}"),
                    value,
                    position: value_position(value, &ValueAxis::bare(0.0, max)),
                });
            }
            ValueAxis {
                min: 0.0,
                max,
                ticks,
            }
        }
        MetricKind::WaterLevel => ValueAxis::bare(0.0, 1.0),
    }
}

impl ValueAxis {
    fn bare(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            ticks: Vec::new(),
        }
    }
}

fn line_path(points: &[TimedPoint], domain: &TimeDomain, axis: &ValueAxis) -> Option<String> {
    if points.len() < 2 {
        return None;
    }

    Some(
        points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let prefix = if index == 0 { "M" } else { "L" };
                format!(
                    "{prefix} {:.3} {:.3}",
                    x_coordinate(point.timestamp, domain),
                    y_coordinate(point.value, axis)
                )
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn area_path(points: &[TimedPoint], domain: &TimeDomain, axis: &ValueAxis) -> Option<String> {
    if points.len() < 2 {
        return None;
    }
    let first = points.first()?;
    let last = points.last()?;
    let mut path = format!(
        "M {:.3} {:.3}",
        x_coordinate(first.timestamp, domain),
        SVG_PLOT_HEIGHT
    );
    for point in points {
        path.push_str(&format!(
            " L {:.3} {:.3}",
            x_coordinate(point.timestamp, domain),
            y_coordinate(point.value, axis)
        ));
    }
    path.push_str(&format!(
        " L {:.3} {:.3} Z",
        x_coordinate(last.timestamp, domain),
        SVG_PLOT_HEIGHT
    ));
    Some(path)
}

fn uncertainty_band_path(
    points: &[TimedPoint],
    uncertainty: &MetricUncertainty,
    domain: &TimeDomain,
    axis: &ValueAxis,
) -> Option<String> {
    if points.len() < 2 {
        return None;
    }

    let mut upper = Vec::new();
    let mut lower = Vec::new();
    for point in points {
        upper.push(format!(
            "{:.3} {:.3}",
            x_coordinate(point.timestamp, domain),
            y_coordinate(point.value + uncertainty.upper, axis)
        ));
        lower.push(format!(
            "{:.3} {:.3}",
            x_coordinate(point.timestamp, domain),
            y_coordinate(point.value - uncertainty.lower, axis)
        ));
    }
    lower.reverse();
    Some(format!("M {} L {} Z", upper.join(" L "), lower.join(" L ")))
}

fn sample_point_at(
    timestamp: f64,
    history: &[TimedPoint],
    forecast: &[TimedPoint],
) -> Option<TimedPoint> {
    let mut points = history
        .iter()
        .chain(forecast.iter())
        .filter(|point| point.timestamp.is_finite() && point.value.is_finite())
        .cloned()
        .collect::<Vec<_>>();
    points.sort_by(|a, b| {
        a.timestamp
            .partial_cmp(&b.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    points.dedup_by(|a, b| (a.timestamp - b.timestamp).abs() < 0.001);

    let first = points.first()?.clone();
    let last = points.last()?.clone();
    if timestamp <= first.timestamp {
        return Some(first);
    }
    if timestamp >= last.timestamp {
        return Some(last);
    }

    let upper_index = points
        .partition_point(|point| point.timestamp < timestamp)
        .min(points.len() - 1);
    let lower = &points[upper_index - 1];
    let upper = &points[upper_index];
    let span = upper.timestamp - lower.timestamp;
    if span <= 0.0 {
        return Some(lower.clone());
    }

    let ratio = ((timestamp - lower.timestamp) / span).clamp(0.0, 1.0);
    Some(TimedPoint {
        timestamp,
        value: lower.value + (upper.value - lower.value) * ratio,
        label: String::new(),
    })
}

fn x_coordinate(timestamp: f64, domain: &TimeDomain) -> f64 {
    position_for_timestamp(timestamp, domain) * SVG_PLOT_WIDTH / 100.0
}

fn y_coordinate(value: f64, axis: &ValueAxis) -> f64 {
    value_position(value, axis) * SVG_PLOT_HEIGHT / 100.0
}

fn value_position(value: f64, axis: &ValueAxis) -> f64 {
    let span = (axis.max - axis.min).max(0.0001);
    (100.0 - ((value - axis.min) / span * 100.0)).clamp(0.0, 100.0)
}

fn position_for_timestamp(timestamp: f64, domain: &TimeDomain) -> f64 {
    let span = (domain.max - domain.min).max(1.0);
    ((timestamp - domain.min) / span * 100.0).clamp(0.0, 100.0)
}

fn discharge_reference_max(station: &StationData) -> f64 {
    let observed = station
        .history
        .iter()
        .chain(station.forecast.iter())
        .filter(|series| series.kind == MetricKind::Discharge)
        .flat_map(|series| series.points.iter().map(|point| point.value))
        .chain(
            station
                .current
                .iter()
                .filter(|metric| metric.kind == MetricKind::Discharge)
                .map(|metric| metric.value),
        )
        .reduce(f64::max)
        .unwrap_or(0.0);
    let baseline: f64 = if station.id == "2170" { 150.0 } else { 600.0 };
    observed.max(baseline)
}

fn discharge_safety(value: f64, reference_max: f64) -> DischargeSafety {
    if value < reference_max / 3.0 {
        DischargeSafety::Safe
    } else if value < reference_max * 2.0 / 3.0 {
        DischargeSafety::Risky
    } else {
        DischargeSafety::NoSwim
    }
}

fn discharge_risk_axis_segments(axis: &ValueAxis, reference_max: f64) -> Vec<RiskAxisSegment> {
    let safe_limit = reference_max / 3.0;
    let risky_limit = reference_max * 2.0 / 3.0;
    [
        (DischargeSafety::Safe, axis.min, safe_limit),
        (DischargeSafety::Risky, safe_limit, risky_limit),
        (DischargeSafety::NoSwim, risky_limit, axis.max),
    ]
    .into_iter()
    .filter_map(|(safety, lower, upper)| risk_axis_segment(axis, safety, lower, upper))
    .collect()
}

fn risk_axis_segment(
    axis: &ValueAxis,
    safety: DischargeSafety,
    lower: f64,
    upper: f64,
) -> Option<RiskAxisSegment> {
    let lower = lower.clamp(axis.min, axis.max);
    let upper = upper.clamp(axis.min, axis.max);
    if upper <= lower {
        return None;
    }

    let top = value_position(upper, axis);
    let bottom = value_position(lower, axis);
    Some(RiskAxisSegment {
        class_name: safety_class(safety),
        top,
        height: (bottom - top).max(0.0),
    })
}

fn value_tick_class(
    kind: MetricKind,
    tick: &ValueTick,
    discharge_reference: Option<f64>,
) -> String {
    let Some(reference) = discharge_reference else {
        return "custom-y-tick".to_string();
    };
    if kind != MetricKind::Discharge {
        return "custom-y-tick".to_string();
    }
    format!(
        "custom-y-tick {}",
        safety_class(discharge_safety(tick.value, reference))
    )
}

fn time_ticks(min: f64, max: f64) -> Vec<AxisTick> {
    let Some(start_utc) = DateTime::<Utc>::from_timestamp(min as i64, 0) else {
        return Vec::new();
    };
    let start_local = start_utc.with_timezone(&Local);
    let rounded_hour = (start_local.hour() / 6) * 6;
    let Some(mut cursor) = local_datetime(start_local.date_naive(), rounded_hour, 0, 0) else {
        return Vec::new();
    };
    while (cursor.timestamp() as f64) < min {
        cursor += Duration::hours(6);
    }

    let mut ticks = Vec::new();
    while cursor.timestamp() as f64 <= max + 1.0 {
        let timestamp = cursor.timestamp() as f64;
        let hour = cursor.hour();
        let kind = if hour == 0 {
            AxisTickKind::Day
        } else if hour == 12 {
            AxisTickKind::Noon
        } else {
            AxisTickKind::Hour
        };
        let label = match kind {
            AxisTickKind::Day => cursor.format("%d.%m").to_string(),
            AxisTickKind::Noon => "12".to_string(),
            AxisTickKind::Hour => format!("{hour:02}"),
        };
        ticks.push(AxisTick {
            label,
            timestamp,
            position: ((timestamp - min) / (max - min).max(1.0) * 100.0).clamp(0.0, 100.0),
            kind,
        });
        cursor += Duration::hours(6);
    }

    ticks
}

fn sun_bands(min: f64, max: f64) -> Vec<SunBand> {
    let Some(start) = DateTime::<Utc>::from_timestamp(min as i64, 0) else {
        return Vec::new();
    };
    let Some(end) = DateTime::<Utc>::from_timestamp(max as i64, 0) else {
        return Vec::new();
    };
    let mut date = start.with_timezone(&Local).date_naive() - Duration::days(1);
    let end_date = end.with_timezone(&Local).date_naive() + Duration::days(1);
    let mut bands = Vec::new();

    while date <= end_date {
        if let (Some((_, sunset)), Some((next_sunrise, _))) = (
            sunrise_sunset(date),
            sunrise_sunset(date + Duration::days(1)),
        ) {
            let band_start = sunset.timestamp() as f64;
            let band_end = next_sunrise.timestamp() as f64;
            let clipped_start = band_start.max(min);
            let clipped_end = band_end.min(max);
            if clipped_end > clipped_start {
                bands.push(SunBand {
                    x: ((clipped_start - min) / (max - min).max(1.0) * 100.0).clamp(0.0, 100.0),
                    width: ((clipped_end - clipped_start) / (max - min).max(1.0) * 100.0)
                        .clamp(0.0, 100.0),
                });
            }
        }
        date += Duration::days(1);
    }

    bands
}

fn sunrise_sunset(date: NaiveDate) -> Option<(DateTime<Local>, DateTime<Local>)> {
    let sunrise = solar_event_utc_hour(date, true)?;
    let sunset = solar_event_utc_hour(date, false)?;
    Some((
        utc_hour_to_local(date, sunrise)?,
        utc_hour_to_local(date, sunset)?,
    ))
}

fn solar_event_utc_hour(date: NaiveDate, sunrise: bool) -> Option<f64> {
    let day = date.ordinal() as f64;
    let lng_hour = GENEVA_LONGITUDE / 15.0;
    let base_time = if sunrise { 6.0 } else { 18.0 };
    let t = day + ((base_time - lng_hour) / 24.0);
    let mean_anomaly = (0.9856 * t) - 3.289;
    let true_longitude = normalize_degrees(
        mean_anomaly
            + (1.916 * deg_sin(mean_anomaly))
            + (0.020 * deg_sin(2.0 * mean_anomaly))
            + 282.634,
    );
    let mut right_ascension =
        normalize_degrees((0.91764 * deg_tan(true_longitude)).atan().to_degrees());
    let l_quadrant = (true_longitude / 90.0).floor() * 90.0;
    let ra_quadrant = (right_ascension / 90.0).floor() * 90.0;
    right_ascension = (right_ascension + l_quadrant - ra_quadrant) / 15.0;
    let sin_declination = 0.39782 * deg_sin(true_longitude);
    let cos_declination = (1.0 - sin_declination * sin_declination).sqrt();
    let cos_hour_angle = (deg_cos(SUNRISE_SUNSET_ZENITH_DEGREES)
        - (sin_declination * deg_sin(GENEVA_LATITUDE)))
        / (cos_declination * deg_cos(GENEVA_LATITUDE));
    if !(-1.0..=1.0).contains(&cos_hour_angle) {
        return None;
    }
    let hour_angle = if sunrise {
        360.0 - cos_hour_angle.acos().to_degrees()
    } else {
        cos_hour_angle.acos().to_degrees()
    } / 15.0;
    let local_mean_time = hour_angle + right_ascension - (0.06571 * t) - 6.622;
    Some(normalize_hours(local_mean_time - lng_hour))
}

fn utc_hour_to_local(date: NaiveDate, hour: f64) -> Option<DateTime<Local>> {
    let seconds = (hour * 3600.0).round() as i64;
    let midnight = Utc
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
        .single()?;
    Some((midnight + Duration::seconds(seconds)).with_timezone(&Local))
}

fn local_datetime(date: NaiveDate, hour: u32, minute: u32, second: u32) -> Option<DateTime<Local>> {
    let naive = date.and_hms_opt(hour, minute, second)?;
    Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Local.from_local_datetime(&naive).earliest())
}

fn deg_sin(value: f64) -> f64 {
    value.to_radians().sin()
}

fn deg_cos(value: f64) -> f64 {
    value.to_radians().cos()
}

fn deg_tan(value: f64) -> f64 {
    value.to_radians().tan()
}

fn normalize_degrees(value: f64) -> f64 {
    value.rem_euclid(360.0)
}

fn normalize_hours(value: f64) -> f64 {
    value.rem_euclid(24.0)
}

fn mouse_ratio(event: &MouseEvent) -> Option<f64> {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt;

        let web_event = event.data().as_web_event();
        let target = web_event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())?;
        let capture = target
            .closest(".hover-capture")
            .ok()
            .flatten()
            .unwrap_or(target);
        let rect = capture.get_bounding_client_rect();
        if rect.width() <= 0.0 {
            return None;
        }
        let client_x = web_event.client_x() as f64;
        Some(((client_x - rect.left()) / rect.width()).clamp(0.0, 1.0))
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let point = event.data().element_coordinates();
        Some((point.x / SVG_PLOT_WIDTH).clamp(0.0, 1.0))
    }
}

#[cfg(target_arch = "wasm32")]
fn sync_chart_scroll(event: ScrollEvent) {
    use dioxus::web::WebEventExt;

    let web_event = event.data().as_web_event();
    let Some(source) = web_event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let scroll_left = source.scroll_left();
    let Ok(Some(stack)) = source.closest(".chart-stack") else {
        return;
    };
    let Ok(nodes) = stack.query_selector_all(".scroll-sync") else {
        return;
    };

    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<web_sys::Element>() else {
            continue;
        };
        if element.scroll_left() != scroll_left {
            element.set_scroll_left(scroll_left);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_chart_scroll(_event: ScrollEvent) {}

async fn load_dashboard(token: Option<String>) -> Result<DashboardData, String> {
    let url = api_url("/api/v1/dashboard");

    #[cfg(target_arch = "wasm32")]
    {
        let mut request = Request::get(&url);
        if let Some(token) = token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        let response = request.send().await.map_err(|err| err.to_string())?;
        if !response.ok() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "dashboard request failed".to_string()));
        }
        let content_type = response.headers().get("content-type").unwrap_or_default();
        if !is_json_content_type(&content_type) {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "dashboard API returned non-JSON response ({status}, {}): {}",
                content_type_label(&content_type),
                response_excerpt(&body)
            ));
        }
        response.json().await.map_err(|err| err.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let client = reqwest::Client::new();
        let mut request = client.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|err| err.to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "dashboard request failed".to_string()));
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !is_json_content_type(&content_type) {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "dashboard API returned non-JSON response ({status}, {}): {}",
                content_type_label(&content_type),
                response_excerpt(&body)
            ));
        }
        response.json().await.map_err(|err| err.to_string())
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("application/json") || content_type.contains("+json")
}

fn content_type_label(content_type: &str) -> String {
    if content_type.trim().is_empty() {
        "missing content-type".to_string()
    } else {
        content_type.to_string()
    }
}

fn response_excerpt(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = compact.chars().take(180).collect::<String>();
    if compact.chars().count() > excerpt.chars().count() {
        excerpt.push_str("...");
    }
    excerpt
}

async fn authenticate_pro(code: String) -> Result<ProAuthResponse, String> {
    let url = api_url("/api/v1/auth/pro");
    let request = ProAuthRequest { code };

    #[cfg(target_arch = "wasm32")]
    {
        let response = Request::post(&url)
            .json(&request)
            .map_err(|err| err.to_string())?
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if !response.ok() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "invalid pro code".to_string()));
        }
        response.json().await.map_err(|err| err.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let response = reqwest::Client::new()
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if !response.status().is_success() {
            return Err(response
                .text()
                .await
                .unwrap_or_else(|_| "invalid pro code".to_string()));
        }
        response.json().await.map_err(|err| err.to_string())
    }
}

fn api_url(path: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        path.to_string()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let base = option_env!("RHONOMETRE_API_BASE").unwrap_or("http://127.0.0.1:3000");
        format!("{}{}", base.trim_end_matches('/'), path)
    }
}

#[cfg(target_arch = "wasm32")]
fn register_service_worker() {
    let Some(window) = web_sys::window() else {
        return;
    };
    if can_register_service_worker(&window) {
        let _ = window.navigator().service_worker().register("/sw.js");
    }
}

#[cfg(target_arch = "wasm32")]
fn can_register_service_worker(window: &web_sys::Window) -> bool {
    if window.is_secure_context() {
        return true;
    }

    let location = window.location();
    let protocol = location.protocol().unwrap_or_default();
    let hostname = location.hostname().unwrap_or_default();
    protocol == "https:" || hostname == "localhost" || hostname == "127.0.0.1" || hostname == "::1"
}

fn read_stored_pro_token() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(PRO_TOKEN_STORAGE_KEY).ok().flatten())
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

fn store_pro_token(token: Option<&str>) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        match token {
            Some(token) => {
                let _ = storage.set_item(PRO_TOKEN_STORAGE_KEY, token);
            }
            None => {
                let _ = storage.remove_item(PRO_TOKEN_STORAGE_KEY);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _ = token;
}

fn initial_focus_mode() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item("rhonometre_focus").ok().flatten())
            .as_deref()
            == Some("1")
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

fn store_focus_mode(enabled: bool) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item("rhonometre_focus", if enabled { "1" } else { "0" });
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _ = enabled;
}

fn tr<'a>(locale: Locale, fr: &'a str, en: &'a str) -> &'a str {
    match locale {
        Locale::Fr => fr,
        Locale::En => en,
    }
}

fn station_title(station: &StationData, locale: Locale) -> String {
    match locale {
        Locale::Fr => station.role_fr.clone(),
        Locale::En => station.role_en.clone(),
    }
}

fn station_subtitle(station: &StationData, locale: Locale) -> String {
    match locale {
        Locale::Fr => station.name_fr.clone(),
        Locale::En => station.name_en.clone(),
    }
}

fn metric_title(kind: MetricKind, locale: Locale) -> &'static str {
    match (kind, locale) {
        (MetricKind::Discharge, Locale::Fr) => "Débit",
        (MetricKind::Discharge, Locale::En) => "Discharge",
        (MetricKind::Temperature, Locale::Fr) => "Température",
        (MetricKind::Temperature, Locale::En) => "Temperature",
        (MetricKind::WaterLevel, Locale::Fr) => "Niveau",
        (MetricKind::WaterLevel, Locale::En) => "Water level",
    }
}

fn axis_title(kind: MetricKind, locale: Locale, unit: &str) -> String {
    format!("{} ({unit})", metric_title(kind, locale))
}

fn default_unit(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "m³/s",
        MetricKind::WaterLevel => "m",
        MetricKind::Temperature => "°C",
    }
}

fn metric_kind_class(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "discharge",
        MetricKind::WaterLevel => "level",
        MetricKind::Temperature => "temperature",
    }
}

fn tick_kind_class(kind: AxisTickKind) -> &'static str {
    match kind {
        AxisTickKind::Day => "day",
        AxisTickKind::Noon => "noon",
        AxisTickKind::Hour => "hour",
    }
}

fn axis_label_class(tick: &AxisTick) -> String {
    let mut class = format!("axis-label {}", tick_kind_class(tick.kind));
    if tick.position <= 0.5 {
        class.push_str(" start");
    } else if tick.position >= 99.5 {
        class.push_str(" end");
    }
    if tick.kind == AxisTickKind::Hour && (tick.position <= 5.5 || tick.position >= 94.5) {
        class.push_str(" edge-hour");
    }
    if tick.kind == AxisTickKind::Noon && (tick.position <= 10.5 || tick.position >= 89.5) {
        class.push_str(" edge-noon");
    }
    class
}

fn axis_label_full(tick: &AxisTick) -> String {
    match tick.kind {
        AxisTickKind::Day | AxisTickKind::Hour => tick.label.clone(),
        AxisTickKind::Noon => "12:00".to_string(),
    }
}

fn axis_label_wide(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Day => {
            let Some(datetime) = DateTime::<Utc>::from_timestamp(tick.timestamp as i64, 0) else {
                return axis_label_short(tick, locale);
            };
            format!(
                "{} {}",
                weekday_medium_label(tick.timestamp, locale),
                datetime.with_timezone(&Local).day()
            )
        }
        AxisTickKind::Noon => "12:00".to_string(),
        AxisTickKind::Hour => tick.label.clone(),
    }
}

fn axis_label_medium(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Day => weekday_medium_label(tick.timestamp, locale).to_string(),
        AxisTickKind::Noon => "12:00".to_string(),
        AxisTickKind::Hour => tick.label.clone(),
    }
}

fn axis_label_short(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Day => weekday_short_label(tick.timestamp, locale).to_string(),
        AxisTickKind::Noon => "12:00".to_string(),
        AxisTickKind::Hour => tick.label.clone(),
    }
}

fn weekday_short_label(timestamp: f64, locale: Locale) -> &'static str {
    let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp as i64, 0) else {
        return "";
    };
    match (locale, datetime.with_timezone(&Local).weekday()) {
        (Locale::Fr, chrono::Weekday::Mon) => "L",
        (Locale::Fr, chrono::Weekday::Tue) => "Ma",
        (Locale::Fr, chrono::Weekday::Wed) => "Me",
        (Locale::Fr, chrono::Weekday::Thu) => "J",
        (Locale::Fr, chrono::Weekday::Fri) => "V",
        (Locale::Fr, chrono::Weekday::Sat) => "S",
        (Locale::Fr, chrono::Weekday::Sun) => "D",
        (Locale::En, chrono::Weekday::Mon) => "M",
        (Locale::En, chrono::Weekday::Tue) => "T",
        (Locale::En, chrono::Weekday::Wed) => "W",
        (Locale::En, chrono::Weekday::Thu) => "Th",
        (Locale::En, chrono::Weekday::Fri) => "F",
        (Locale::En, chrono::Weekday::Sat) => "Sa",
        (Locale::En, chrono::Weekday::Sun) => "Su",
    }
}

fn weekday_medium_label(timestamp: f64, locale: Locale) -> &'static str {
    let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp as i64, 0) else {
        return "";
    };
    match (locale, datetime.with_timezone(&Local).weekday()) {
        (Locale::Fr, chrono::Weekday::Mon) => "Lun",
        (Locale::Fr, chrono::Weekday::Tue) => "Mar",
        (Locale::Fr, chrono::Weekday::Wed) => "Mer",
        (Locale::Fr, chrono::Weekday::Thu) => "Jeu",
        (Locale::Fr, chrono::Weekday::Fri) => "Ven",
        (Locale::Fr, chrono::Weekday::Sat) => "Sam",
        (Locale::Fr, chrono::Weekday::Sun) => "Dim",
        (Locale::En, chrono::Weekday::Mon) => "Mon",
        (Locale::En, chrono::Weekday::Tue) => "Tue",
        (Locale::En, chrono::Weekday::Wed) => "Wed",
        (Locale::En, chrono::Weekday::Thu) => "Thu",
        (Locale::En, chrono::Weekday::Fri) => "Fri",
        (Locale::En, chrono::Weekday::Sat) => "Sat",
        (Locale::En, chrono::Weekday::Sun) => "Sun",
    }
}

fn time_axis_range_label(domain: &TimeDomain, locale: Locale) -> String {
    let Some(start) = DateTime::<Utc>::from_timestamp(domain.min as i64, 0) else {
        return String::new();
    };
    let Some(end) = DateTime::<Utc>::from_timestamp(domain.max as i64, 0) else {
        return String::new();
    };
    let start = start.with_timezone(&Local).date_naive();
    let end = end.with_timezone(&Local).date_naive();

    if start.year() == end.year() && start.month() == end.month() {
        match locale {
            Locale::Fr => format!(
                "{}-{} {} {}",
                start.day(),
                end.day(),
                month_name_fr(start.month()),
                start.year()
            ),
            Locale::En => format!(
                "{} {}-{}, {}",
                month_name_en(start.month()),
                start.day(),
                end.day(),
                start.year()
            ),
        }
    } else if start.year() == end.year() {
        match locale {
            Locale::Fr => format!(
                "{} {}-{} {} {}",
                start.day(),
                month_name_fr(start.month()),
                end.day(),
                month_name_fr(end.month()),
                start.year()
            ),
            Locale::En => format!(
                "{} {}-{} {}, {}",
                month_name_en(start.month()),
                start.day(),
                month_name_en(end.month()),
                end.day(),
                start.year()
            ),
        }
    } else {
        match locale {
            Locale::Fr => format!(
                "{} {} {}-{} {} {}",
                start.day(),
                month_name_fr(start.month()),
                start.year(),
                end.day(),
                month_name_fr(end.month()),
                end.year()
            ),
            Locale::En => format!(
                "{} {}, {}-{} {}, {}",
                month_name_en(start.month()),
                start.day(),
                start.year(),
                month_name_en(end.month()),
                end.day(),
                end.year()
            ),
        }
    }
}

fn month_name_fr(month: u32) -> &'static str {
    match month {
        1 => "janv.",
        2 => "févr.",
        3 => "mars",
        4 => "avr.",
        5 => "mai",
        6 => "juin",
        7 => "juil.",
        8 => "août",
        9 => "sept.",
        10 => "oct.",
        11 => "nov.",
        12 => "déc.",
        _ => "",
    }
}

fn month_name_en(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

fn safety_class(safety: DischargeSafety) -> &'static str {
    match safety {
        DischargeSafety::Safe => "safety-safe",
        DischargeSafety::Risky => "safety-risky",
        DischargeSafety::NoSwim => "safety-noswim",
    }
}

fn safety_label(safety: DischargeSafety, locale: Locale) -> &'static str {
    match (safety, locale) {
        (DischargeSafety::Safe, Locale::Fr) => "Courant lent",
        (DischargeSafety::Safe, Locale::En) => "Slow current",
        (DischargeSafety::Risky, Locale::Fr) => "Attention courant fort",
        (DischargeSafety::Risky, Locale::En) => "Strong current",
        (DischargeSafety::NoSwim, Locale::Fr) => "Danger! Courant très fort!",
        (DischargeSafety::NoSwim, Locale::En) => "Danger! Very strong current!",
    }
}

fn format_metric_value(value: f64, unit: &str, kind: MetricKind) -> String {
    match kind {
        MetricKind::Temperature => format!("{value:.1} {unit}"),
        MetricKind::Discharge => format!("{value:.0} {unit}"),
        MetricKind::WaterLevel => format!("{value:.2} {unit}"),
    }
}

fn format_timestamp(value: &str, locale: Locale) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            let local = timestamp.with_timezone(&Local);
            match locale {
                Locale::Fr => local.format("%d.%m.%Y %H:%M").to_string(),
                Locale::En => local.format("%Y-%m-%d %H:%M").to_string(),
            }
        })
        .unwrap_or_else(|_| value.to_string())
}

fn format_timestamp_from_seconds(seconds: f64, locale: Locale) -> String {
    DateTime::<Utc>::from_timestamp(seconds as i64, 0)
        .map(|timestamp| format_timestamp(&timestamp.to_rfc3339(), locale))
        .unwrap_or_else(|| "—".to_string())
}

fn format_swiss_now_seconds() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

#[component]
fn FocusIcon(active: bool) -> Element {
    if active {
        rsx! {
            svg { class: "button-icon", view_box: "0 0 24 24",
                path { d: "M8 3v3a2 2 0 0 1-2 2H3" }
                path { d: "M21 8h-3a2 2 0 0 1-2-2V3" }
                path { d: "M3 16h3a2 2 0 0 1 2 2v3" }
                path { d: "M16 21v-3a2 2 0 0 1 2-2h3" }
            }
        }
    } else {
        rsx! {
            svg { class: "button-icon", view_box: "0 0 24 24",
                path { d: "M15 3h6v6" }
                path { d: "M9 21H3v-6" }
                path { d: "M21 3l-7 7" }
                path { d: "M3 21l7-7" }
            }
        }
    }
}

#[component]
fn RefreshIcon() -> Element {
    rsx! {
        svg { class: "button-icon", view_box: "0 0 24 24",
            path { d: "M21 12a9 9 0 0 1-15.5 6.3" }
            path { d: "M3 12A9 9 0 0 1 18.5 5.7" }
            path { d: "M3 18v-6h6" }
            path { d: "M21 6v6h-6" }
        }
    }
}
