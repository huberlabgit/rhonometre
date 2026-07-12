use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
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
    #[serde(default)]
    sources: Vec<SourceInfo>,
    #[serde(default)]
    air_temperature: Option<AirTemperatureData>,
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
struct AirTemperatureData {
    source: SourceInfo,
    current: Option<CurrentMetric>,
    history: MetricSeries,
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
    source: StationDataSource,
    #[serde(default)]
    notice_fr: Option<String>,
    #[serde(default)]
    notice_en: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StationDataSource {
    #[default]
    Hydrodaten,
    Derived,
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
    Midnight,
    SixHour,
    Noon,
    EighteenHour,
}

#[derive(Clone, Debug, PartialEq)]
struct AxisTick {
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct EmbedConfig {
    station: String,
}

#[component]
fn App() -> Element {
    let embed_config = initial_embed_config();
    let embed_mode = embed_config.is_some();
    let initial_station = initial_station_id(embed_config.as_ref());
    let mut locale = use_signal(initial_locale);
    let selected_station = use_signal(move || initial_station.clone());
    let mut focus_mode = use_signal(move || !embed_mode && initial_focus_mode());
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
    let app_class = if embed_mode {
        "app-shell embed-mode"
    } else if focus_mode() {
        "app-shell focus-mode"
    } else {
        "app-shell"
    };

    rsx! {
        document::Style { "{APP_CSS}" }

        div { class: "{app_class}",
            if !embed_mode {
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
            }

            if focus_mode() && !embed_mode {
                span {
                    class: "focus-corner-logo",
                    role: "img",
                    aria_label: "Pontonnier·ère·s de Genève",
                    ""
                }
            }

            if !embed_mode {
                div { class: "topbar",
                    div { class: "brand-lockup",
                        h1 { "{app_title(locale())}" }
                        div { class: "partner-mark",
                            span { class: "station-brand-copy",
                                "des"
                                br {}
                                "Pontonnier·ère·s"
                                br {}
                                "de Genève"
                            }
                            span { class: "partner-logo-icon", "" }
                        }
                    }
                    div { class: "topbar-actions",
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
                }
            }
            }

            if !embed_mode && pro_signin_open() && !pro_enabled {
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
                        embed_mode,
                        pro_enabled,
                        live_clock: live_clock(),
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
    embed_mode: bool,
    pro_enabled: bool,
    live_clock: String,
) -> Element {
    let river_stations = data
        .stations
        .iter()
        .filter(|station| station.kind == WaterKind::River)
        .cloned()
        .collect::<Vec<_>>();
    let selected = river_stations
        .iter()
        .find(|station| station_matches(station, &selected_station()))
        .or_else(|| river_stations.first())
        .cloned();
    let air_temperature = data.air_temperature.clone();
    let air_temperature_label = data
        .air_temperature
        .as_ref()
        .and_then(|air| air.current.as_ref())
        .map(|metric| format_metric_value(metric.value, &metric.unit, MetricKind::Temperature));

    rsx! {
        div { class: if focus_mode { "dashboard dashboard-focus" } else if embed_mode { "dashboard dashboard-embed" } else { "dashboard" },
            if let Some(station) = selected {
                StationPanel {
                    station,
                    locale,
                    focus_mode,
                    embed_mode,
                    pro_enabled,
                    air_temperature: air_temperature.clone(),
                    air_temperature_label: air_temperature_label.clone(),
                    live_clock: live_clock.clone(),
                }
            }

            if !focus_mode && !embed_mode {
                div { class: "station-tabs station-tabs-bottom",
                    for station in river_stations {
                        button {
                            class: if station.id == selected_station() { "station-tab active" } else { "station-tab" },
                            r#type: "button",
                            onclick: move |_| selected_station.set(station.id.clone()),
                            strong { "{station_title(&station, locale)}" }
                            small { "{station_tab_detail(locale)}" }
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
    let sources = if data.sources.is_empty() {
        vec![data.source.clone()]
    } else {
        data.sources.clone()
    };

    rsx! {
        div { class: "source-strip source-strip-footer",
            div { class: "source-list",
                span { class: "label", "{tr(locale, \"Sources\", \"Sources\")}" }
                div { class: "source-links",
                    for source in sources {
                        if source.url.is_empty() {
                            span { "{source.label}" }
                        } else {
                            a { href: "{source.url}", target: "_blank", rel: "noreferrer", "{source.label}" }
                        }
                    }
                }
            }
            if !data.warnings.is_empty() {
                span { class: "status-line", "{data.warnings.len()} {tr(locale, \"avert.\", \"warn.\")}" }
            }
        }
    }
}

#[component]
fn StationPanel(
    station: StationData,
    locale: Locale,
    focus_mode: bool,
    embed_mode: bool,
    pro_enabled: bool,
    air_temperature: Option<AirTemperatureData>,
    air_temperature_label: Option<String>,
    live_clock: String,
) -> Element {
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
    let air_temperature_overlay = if pro_enabled {
        air_temperature.as_ref().map(|air| &air.history)
    } else {
        None
    };
    let station_notice = if station.source == StationDataSource::Derived {
        match locale {
            Locale::Fr => station.notice_fr.clone(),
            Locale::En => station.notice_en.clone(),
        }
    } else {
        None
    };
    let latest_measurement = latest_station_measurement(&station, locale);
    let measurement_readout = hover_state()
        .map(|hover| {
            (
                tr(locale, "Mesure à:", "Measurement at:").to_string(),
                format_timestamp_from_seconds(hover.timestamp, locale),
            )
        })
        .or_else(|| {
            latest_measurement.clone().map(|timestamp| {
                (
                    tr(locale, "Dernière mesure", "Latest measurement").to_string(),
                    timestamp,
                )
            })
        });
    let idle_hover_state = latest_station_timestamp(&station)
        .and_then(|timestamp| hover_state_for_timestamp(timestamp, &domain));
    let heading_subtitle = station_metric_context(&station, locale);
    let measurement_station = station_measurement_station(&station, locale);

    rsx! {
        article { class: "station-panel",
            div { class: "station-heading",
                div { class: "station-heading-title",
                    if focus_mode {
                        h2 { class: "station-focus-title",
                            span { class: "station-app-word", "{app_title(locale)}" }
                            span { class: "station-focus-brand",
                                span { class: "station-brand-copy",
                                    "des"
                                    br {}
                                    "Pontonnier·ère·s"
                                    br {}
                                    "de Genève"
                                }
                                span { class: "partner-logo-icon station-brand-logo", "" }
                            }
                        }
                    } else if embed_mode {
                        h2 { class: "station-embed-title",
                            span { class: "station-app-word", "rhonoscope" }
                            span { class: "station-embed-brand",
                                span { class: "station-brand-copy",
                                    "par les"
                                    br {}
                                    "Pontonnier·ère·s"
                                    br {}
                                    "de Genève"
                                }
                                span { class: "partner-logo-icon station-embed-logo", "" }
                            }
                        }
                    } else {
                        h2 { "{station_title(&station, locale)}" }
                    }
                }
                div { class: "station-heading-side",
                    div { class: "page-live-clock station-live-clock",
                        span { class: "live-clock-label", "{tr(locale, \"Maintenant\", \"Now\")}" }
                        time { class: "live-clock-time", "{live_clock}" }
                        if let Some(air_temperature_label) = air_temperature_label {
                            span { class: "live-clock-air",
                                span { "{tr(locale, \"Air\", \"Air\")}" }
                                strong { "{air_temperature_label}" }
                            }
                        }
                    }
                    span {
                        class: "station-header-qr",
                        role: "img",
                        aria_label: "QR code Pontonnier·ère·s de Genève",
                        ""
                    }
                }
            }

            div { class: "chart-stack",
                div { class: "chart-measure-row",
                    p { class: "chart-context-label", "{heading_subtitle}" }
                }

                {metric_chart(
                    &station,
                    MetricKind::Temperature,
                    Some("top"),
                    temperature_history,
                    temperature_forecast,
                    &domain,
                    locale,
                    hover_state,
                    air_temperature_overlay,
                    idle_hover_state,
                )}

                {metric_readout(
                    &station,
                    MetricKind::Temperature,
                    temperature_history,
                    temperature_forecast,
                    &domain,
                    locale,
                    hover_state,
                    None,
                    "plot-current plot-current-stacked",
                )}

                {metric_chart(
                    &station,
                    MetricKind::Discharge,
                    Some("bottom"),
                    discharge_history,
                    discharge_forecast,
                    &domain,
                    locale,
                    hover_state,
                    None,
                    idle_hover_state,
                )}

                {metric_readout(
                    &station,
                    MetricKind::Discharge,
                    discharge_history,
                    discharge_forecast,
                    &domain,
                    locale,
                    hover_state,
                    None,
                    "plot-current plot-current-stacked",
                )}

                if let Some((measurement_label, measurement_time)) = measurement_readout {
                    div { class: "chart-measure-footer",
                        p { class: "plot-last-measure plot-last-measure-chart",
                            span { "{measurement_label}" }
                            time { "{measurement_time}" }
                        }
                    }
                }
            }

            div { class: "station-footnotes",
                p { class: "station-footnote",
                    strong { "{tr(locale, \"Station\", \"Station\")}" }
                    span { "{measurement_station}" }
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
}

#[component]
fn TimeAxis(
    domain: TimeDomain,
    placement: String,
    locale: Locale,
    hover_state: Signal<Option<HoverState>>,
    idle_hover_state: Option<HoverState>,
) -> Element {
    let axis_class = format!("shared-time-axis time-axis-{placement}");
    let width_style = format!(
        "width: {:.3}%; min-width: var(--chart-content-min-width, 100%);",
        domain.content_width_percent
    );
    let hover_position = hover_state()
        .or(idle_hover_state)
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
                                if tick.kind == AxisTickKind::Noon {
                                    span {
                                        class: "{axis_label_class(tick)}",
                                        style: format!("left: {:.3}%;", tick.position),
                                        span { class: "axis-label-full", "{axis_label_full(tick, locale)}" }
                                        span { class: "axis-label-wide", "{axis_label_wide(tick, locale)}" }
                                        span { class: "axis-label-medium", "{axis_label_medium(tick, locale)}" }
                                        span { class: "axis-label-short", "{axis_label_short(tick, locale)}" }
                                    }
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
    time_axis_placement: Option<&str>,
    history: Option<&MetricSeries>,
    forecast: Option<&MetricSeries>,
    domain: &TimeDomain,
    locale: Locale,
    mut hover_state: Signal<Option<HoverState>>,
    air_overlay: Option<&MetricSeries>,
    idle_hover_state: Option<HoverState>,
) -> Element {
    let history_points = history
        .map(|series| timed_points(&series.points))
        .unwrap_or_default();
    let forecast_points = forecast
        .map(|series| timed_points(&series.points))
        .unwrap_or_default();
    let history_points = points_in_domain(&history_points, domain);
    let forecast_points = points_in_domain(&forecast_points, domain);
    let air_overlay_points = if kind == MetricKind::Temperature {
        air_overlay
            .map(|series| points_in_domain(&timed_points(&series.points), domain))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
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
    let discharge_reference = if kind == MetricKind::Discharge {
        Some(discharge_reference_max(station))
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
    let chart_class = format!("chart-card {}", metric_kind_class(kind));
    let cursor_hover = hover_state().or(idle_hover_state);
    let hover_position = cursor_hover
        .map(|hover| hover.position)
        .filter(|position| (0.0..=100.0).contains(position));
    let hover_point = cursor_hover.and_then(|hover| {
        sample_point_at(hover.timestamp, &history_points, &forecast_points)
            .map(|point| (hover.position, value_position(point.value, &axis)))
    });
    let domain_min = domain.min;
    let domain_span = (domain.max - domain.min).max(1.0);
    let history_path = line_path(&history_points, domain, &axis);
    let history_area = area_path(&history_points, domain, &axis);
    let forecast_path = line_path(&forecast_points, domain, &axis);
    let forecast_area = area_path(&forecast_points, domain, &axis);
    let air_overlay_path = line_path(&air_overlay_points, domain, &axis);
    let show_uncertainty = station.id != "2606";
    let uncertainty_path = history
        .and_then(|series| series.uncertainty.as_ref())
        .filter(|_| show_uncertainty)
        .and_then(|uncertainty| uncertainty_band_path(&history_points, uncertainty, domain, &axis));

    rsx! {
        div { class: "{chart_class}",
            if time_axis_placement == Some("top") {
                TimeAxis {
                    domain: domain.clone(),
                    placement: "top".to_string(),
                    locale,
                    hover_state,
                    idle_hover_state,
                }
            }
            div { class: "plot-row",
                div { class: "chart-frame",
                    div { class: "custom-chart",
                        div { class: "{y_axis_class}",
                            {axis_title_view(kind, locale, &unit)}
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
                            {axis_title_view(kind, locale, &unit)}
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
                                                    class: tick_kind_class(tick.kind),
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
                                        if let Some(path) = air_overlay_path {
                                            path {
                                                class: "custom-line air-temperature",
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

                {metric_readout(
                    station,
                    kind,
                    history,
                    forecast,
                    domain,
                    locale,
                    hover_state,
                    None,
                    "plot-current plot-current-inline",
                )}
            }
            if time_axis_placement == Some("bottom") {
                TimeAxis {
                    domain: domain.clone(),
                    placement: "bottom".to_string(),
                    locale,
                    hover_state,
                    idle_hover_state,
                }
            }
        }
    }
}

fn metric_readout(
    station: &StationData,
    kind: MetricKind,
    history: Option<&MetricSeries>,
    forecast: Option<&MetricSeries>,
    domain: &TimeDomain,
    locale: Locale,
    hover_state: Signal<Option<HoverState>>,
    measurement_readout: Option<(String, String)>,
    class_name: &str,
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
    let current = current_for_kind(station, kind);
    let hovered = hover_state()
        .and_then(|hover| sample_point_at(hover.timestamp, &history_points, &forecast_points));
    let readout_value = hovered
        .as_ref()
        .map(|point| point.value)
        .or_else(|| current.map(|metric| metric.value));
    let formatted_value = readout_value
        .map(|value| format_metric_number(value, kind))
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
    let readout_class = safety
        .map(|safety| {
            format!(
                "readout-value {} {}",
                metric_kind_class(kind),
                safety_class(safety)
            )
        })
        .unwrap_or_else(|| format!("readout-value {}", metric_kind_class(kind)));

    rsx! {
        div { class: "{class_name}",
            div { class: "plot-current-heading",
                h3 { "{metric_title(kind, locale)}" }
            }
            div { class: "plot-readout",
                {metric_value(readout_class, formatted_value, unit.clone(), kind)}
                if let Some(safety) = safety {
                    span {
                        class: format!("discharge-comment {}", safety_class(safety)),
                        "{safety_label(safety, locale)}"
                    }
                }
                if let Some((measurement_label, measurement_time)) = measurement_readout {
                    p { class: "plot-last-measure plot-last-measure-inline",
                        span { "{measurement_label}" }
                        time { "{measurement_time}" }
                    }
                }
            }
        }
    }
}

fn metric_value(class_name: String, value: String, unit: String, _kind: MetricKind) -> Element {
    rsx! {
        strong { class: "{class_name}",
            span { class: "metric-number", "{value}" }
            span { class: "metric-unit", "{unit}" }
        }
    }
}

fn axis_title_view(kind: MetricKind, locale: Locale, unit: &str) -> Element {
    let title = metric_title(kind, locale);

    rsx! {
        div { class: "custom-y-axis-title",
            span { class: "axis-title-label", "{title}" }
            span { class: "axis-title-unit",
                " ({unit})"
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
    latest_station_timestamp_with_label(station)
        .map(|(_, timestamp)| format_timestamp(timestamp, locale))
}

fn latest_station_timestamp(station: &StationData) -> Option<f64> {
    latest_station_timestamp_with_label(station).map(|(timestamp, _)| timestamp)
}

fn latest_station_timestamp_with_label(station: &StationData) -> Option<(f64, &str)> {
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
}

fn hover_state_for_timestamp(timestamp: f64, domain: &TimeDomain) -> Option<HoverState> {
    if !timestamp.is_finite() || timestamp < domain.min || timestamp > domain.max {
        return None;
    }
    let span = (domain.max - domain.min).max(1.0);
    Some(HoverState {
        timestamp,
        position: ((timestamp - domain.min) / span * 100.0).clamp(0.0, 100.0),
    })
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
        MetricKind::Temperature => {
            let axis = ValueAxis::bare(5.0, 30.0);
            ValueAxis {
                min: axis.min,
                max: axis.max,
                ticks: [30.0, 25.0, 20.0, 15.0, 10.0, 5.0]
                    .into_iter()
                    .map(|value| ValueTick {
                        label: format!("{value:.0}"),
                        value,
                        position: value_position(value, &axis),
                    })
                    .collect(),
            }
        }
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
    let mut classes = vec!["custom-y-tick"];
    if tick.position <= 0.5 {
        classes.push("edge-top");
    } else if tick.position >= 99.5 {
        classes.push("edge-bottom");
    }
    if let Some(reference) = discharge_reference {
        if kind == MetricKind::Discharge {
            classes.push(safety_class(discharge_safety(tick.value, reference)));
        }
    }
    classes.join(" ")
}

fn time_ticks(min: f64, max: f64) -> Vec<AxisTick> {
    let Some(start_utc) = DateTime::<Utc>::from_timestamp(min as i64, 0) else {
        return Vec::new();
    };
    let Some(end_utc) = DateTime::<Utc>::from_timestamp(max as i64, 0) else {
        return Vec::new();
    };
    let mut date = start_utc.with_timezone(&Local).date_naive();
    let end_date = end_utc.with_timezone(&Local).date_naive();

    let mut ticks = Vec::new();
    while date <= end_date {
        for hour in [0, 6, 12, 18] {
            let Some(cursor) = local_datetime(date, hour, 0, 0) else {
                continue;
            };
            let timestamp = cursor.timestamp() as f64;
            if timestamp < min || timestamp > max + 1.0 {
                continue;
            }
            let kind = if hour == 0 {
                AxisTickKind::Midnight
            } else if hour == 6 {
                AxisTickKind::SixHour
            } else if hour == 12 {
                AxisTickKind::Noon
            } else {
                AxisTickKind::EighteenHour
            };
            ticks.push(AxisTick {
                timestamp,
                position: ((timestamp - min) / (max - min).max(1.0) * 100.0).clamp(0.0, 100.0),
                kind,
            });
        }
        date += Duration::days(1);
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
        let ratio_source = target
            .closest(".plot-scroll-content")
            .ok()
            .flatten()
            .or_else(|| target.closest(".hover-capture").ok().flatten())
            .unwrap_or(target);
        let rect = ratio_source.get_bounding_client_rect();
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

fn initial_locale() -> Locale {
    match query_param("lang")
        .or_else(|| query_param("locale"))
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("en") | Some("eng") | Some("english") => Locale::En,
        _ => Locale::Fr,
    }
}

fn initial_embed_config() -> Option<EmbedConfig> {
    let embed = query_param("embed")?;
    let embed = embed.trim();
    if matches!(
        embed.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    ) {
        return None;
    }

    let station = query_param("station")
        .or_else(|| {
            if embed.is_empty()
                || matches!(
                    embed.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            {
                None
            } else {
                Some(embed.to_string())
            }
        })
        .map(|station| station.trim().to_string())
        .filter(|station| !station.is_empty())
        .unwrap_or_else(|| "2606".to_string());

    Some(EmbedConfig { station })
}

fn initial_station_id(embed: Option<&EmbedConfig>) -> String {
    embed
        .map(|config| config.station.clone())
        .or_else(|| query_param("station"))
        .map(|station| station.trim().to_string())
        .filter(|station| !station.is_empty())
        .unwrap_or_else(|| "2606".to_string())
}

#[cfg(target_arch = "wasm32")]
fn query_param(name: &str) -> Option<String> {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .and_then(|search| parse_query_param(&search, name))
}

#[cfg(not(target_arch = "wasm32"))]
fn query_param(name: &str) -> Option<String> {
    let _ = name;
    None
}

fn parse_query_param(query: &str, name: &str) -> Option<String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter(|part| !part.is_empty())
        .find_map(|part| {
            let mut pair = part.splitn(2, '=');
            let key = pair.next()?;
            if key != name {
                return None;
            }
            Some(pair.next().unwrap_or_default().replace('+', " "))
        })
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

fn app_title(locale: Locale) -> &'static str {
    tr(locale, "rhonometre", "rhonometer")
}

fn station_matches(station: &StationData, requested: &str) -> bool {
    station.id == requested || station.slug == requested
}

fn station_title(station: &StationData, locale: Locale) -> String {
    let title = match locale {
        Locale::Fr => station.role_fr.clone(),
        Locale::En => station.role_en.clone(),
    };
    station_display_text(station, title)
}

fn station_measurement_station(station: &StationData, locale: Locale) -> String {
    let station_name = match locale {
        Locale::Fr => station.name_fr.clone(),
        Locale::En => station.name_en.clone(),
    };
    station_display_text(station, station_name)
}

fn station_metric_context(station: &StationData, locale: Locale) -> String {
    let title = station_title(station, locale);
    match locale {
        Locale::Fr => {
            if title.starts_with("Arve") {
                format!("Température et débit de l'{title}")
            } else if title.starts_with("Lac") {
                format!("Température du {title}")
            } else {
                format!("Température et débit du {title}")
            }
        }
        Locale::En => format!("Temperature and discharge: {title}"),
    }
}

fn station_tab_detail(locale: Locale) -> &'static str {
    tr(locale, "Température et débit", "Temperature and discharge")
}

fn station_display_text(station: &StationData, text: String) -> String {
    if station.source == StationDataSource::Derived {
        text
    } else {
        text.replace(" (calculé)", "")
            .replace(" (calcule)", "")
            .replace(" (derived)", "")
            .replace(" (estimated)", "")
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
        AxisTickKind::Midnight => "midnight",
        AxisTickKind::SixHour => "six-hour",
        AxisTickKind::Noon => "noon",
        AxisTickKind::EighteenHour => "eighteen-hour",
    }
}

fn axis_label_class(tick: &AxisTick) -> String {
    let mut class = format!("axis-label {}", tick_kind_class(tick.kind));
    if tick.position <= 0.5 {
        class.push_str(" start");
    } else if tick.position >= 99.5 {
        class.push_str(" end");
    }
    class
}

fn axis_label_full(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Noon => date_label_full(tick.timestamp, locale),
        _ => String::new(),
    }
}

fn axis_label_wide(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Noon => date_label_full(tick.timestamp, locale),
        _ => String::new(),
    }
}

fn axis_label_medium(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Noon => date_label_compact(tick.timestamp, locale, false),
        _ => String::new(),
    }
}

fn axis_label_short(tick: &AxisTick, locale: Locale) -> String {
    match tick.kind {
        AxisTickKind::Noon => date_label_compact(tick.timestamp, locale, true),
        _ => String::new(),
    }
}

fn date_label_full(timestamp: f64, locale: Locale) -> String {
    let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp as i64, 0) else {
        return String::new();
    };
    let local = datetime.with_timezone(&Local);
    match locale {
        Locale::Fr => format!(
            "{} {:02}.{:02}",
            weekday_medium_label(timestamp, locale),
            local.day(),
            local.month()
        ),
        Locale::En => format!(
            "{} {} {}",
            weekday_medium_label(timestamp, locale),
            month_name_en(local.month()),
            local.day()
        ),
    }
}

fn date_label_compact(timestamp: f64, locale: Locale, short_weekday: bool) -> String {
    let Some(datetime) = DateTime::<Utc>::from_timestamp(timestamp as i64, 0) else {
        return String::new();
    };
    let weekday = if short_weekday {
        weekday_short_label(timestamp, locale)
    } else {
        weekday_medium_label(timestamp, locale)
    };
    format!("{} {}", weekday, datetime.with_timezone(&Local).day())
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

fn format_metric_number(value: f64, kind: MetricKind) -> String {
    match kind {
        MetricKind::Temperature => format!("{value:.1}"),
        MetricKind::Discharge => format!("{value:.0}"),
        MetricKind::WaterLevel => format!("{value:.2}"),
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
