use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::Cursor,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use calamine::{open_workbook_auto_from_rs, Data, ExcelDateTime, ExcelDateTimeType, Range, Reader};
use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone,
};
use mailparse::parse_mail;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const HYDRO_PQ_URL: &str = "https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_pq.geojson";
const HYDRO_TEMPERATURE_URL: &str =
    "https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_temperature.geojson";
const HYDRO_PQ_FORECAST_URL: &str =
    "https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_pq_forecast.geojson";
const HYDRO_BASE_URL: &str = "https://www.hydrodaten.admin.ch";
const CACHE_TTL: Duration = Duration::from_secs(120);
const HISTORY_DAYS: i64 = 5;
const CALIBRATION_HISTORY_DAYS: i64 = 40;
const HEAT_CALIBRATION_HALF_LIFE_DAYS: f64 = 7.0;
const JONCTION_TO_CHANCY_M: f64 = 15_000.0;
const HALLE_TO_JONCTION_M: f64 = 2_500.0;
const ARVE_STATION_TO_JONCTION_M: f64 = 4_000.0;
const WATER_DENSITY_KG_M3: f64 = 1_000.0;
const WATER_SPECIFIC_HEAT_J_KG_C: f64 = 4_186.0;
const MIN_DERIVED_RHONE_DISCHARGE_M3S: f64 = 20.0;
const PROGRAMME_PATH_ENV: &str = "RHONOMETRE_PROGRAMME_PATH";
const PROGRAMME_DIR_ENV: &str = "RHONOMETRE_PROGRAMME_DIR";
const LOCAL_PROGRAMME_DIR: &str = "data/programmes";

const SOURCE_STATIONS: &[StationConfig] = &[
    StationConfig {
        id: "2170",
        slug: "arve-bout-du-monde",
        name_fr: "Arve - Genève, Bout du Monde",
        name_en: "Arve - Geneva, Bout du Monde",
        role_fr: "Arve",
        role_en: "Arve",
        kind: WaterKind::River,
    },
    StationConfig {
        id: "2028",
        slug: "leman-secheron",
        name_fr: "Lac Léman - Genève, Sécheron",
        name_en: "Lake Geneva - Geneva, Sécheron",
        role_fr: "Lac",
        role_en: "Lake",
        kind: WaterKind::Lake,
    },
    StationConfig {
        id: "2174",
        slug: "rhone-chancy",
        name_fr: "Rhône - Chancy, Aux Ripes",
        name_en: "Rhône - Chancy, Aux Ripes",
        role_fr: "Rhône aval de la Jonction",
        role_en: "Post-Jonction Rhône",
        kind: WaterKind::River,
    },
];

const DERIVED_HALLE_ILE_STATION: StationConfig = StationConfig {
    id: "2606",
    slug: "rhone-halle-ile",
    name_fr: "Rhône - Genève, Halle de l'Ile",
    name_en: "Rhône - Geneva, Halle de l'Ile",
    role_fr: "Rhône avant la Jonction (calculé)",
    role_en: "Rhône before Jonction (derived)",
    kind: WaterKind::River,
};

#[derive(Clone)]
struct AppState {
    client: Client,
    cache: Arc<RwLock<Option<CachedDashboard>>>,
}

#[derive(Clone)]
struct CachedDashboard {
    fetched_at: Instant,
    data: DashboardData,
}

#[derive(Clone, Copy)]
struct StationConfig {
    id: &'static str,
    slug: &'static str,
    name_fr: &'static str,
    name_en: &'static str,
    role_fr: &'static str,
    role_en: &'static str,
    kind: WaterKind,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum WaterKind {
    River,
    Lake,
}

#[derive(Clone, Debug, Serialize)]
struct DashboardData {
    generated_at: String,
    cache_status: CacheStatus,
    source: SourceInfo,
    stations: Vec<StationData>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum CacheStatus {
    Fresh,
    Stale,
}

#[derive(Clone, Debug, Serialize)]
struct SourceInfo {
    label: String,
    url: String,
}

#[derive(Clone, Debug, Serialize)]
struct StationData {
    id: &'static str,
    slug: &'static str,
    name_fr: &'static str,
    name_en: &'static str,
    role_fr: &'static str,
    role_en: &'static str,
    kind: WaterKind,
    current: Vec<CurrentMetric>,
    history: Vec<MetricSeries>,
    forecast: Vec<MetricSeries>,
    status: StationStatus,
    notice_fr: Option<&'static str>,
    notice_en: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum StationStatus {
    Complete,
    Partial,
    Missing,
}

#[derive(Clone, Debug, Serialize)]
struct CurrentMetric {
    kind: MetricKind,
    label_fr: &'static str,
    label_en: &'static str,
    value: f64,
    unit: String,
    measured_at: String,
    range_24h: Option<MetricRange>,
}

#[derive(Clone, Debug, Serialize)]
struct MetricRange {
    min: f64,
    max: f64,
    mean: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MetricKind {
    Discharge,
    WaterLevel,
    Temperature,
}

#[derive(Clone, Debug, Serialize)]
struct MetricSeries {
    kind: MetricKind,
    label_fr: &'static str,
    label_en: &'static str,
    unit: String,
    points: Vec<HistoryPoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uncertainty: Option<MetricUncertainty>,
}

#[derive(Clone, Debug, Serialize)]
struct MetricUncertainty {
    lower: f64,
    upper: f64,
    confidence: f64,
}

#[derive(Clone, Debug, Serialize)]
struct HistoryPoint {
    timestamp: String,
    value: f64,
}

#[derive(Clone, Debug)]
struct TimedPoint {
    timestamp: DateTime<FixedOffset>,
    value: f64,
}

#[derive(Clone, Debug)]
struct TemperatureCalibration {
    heat_power_w: f64,
    regression: TemperatureRegression,
    sample_count: usize,
    median_absolute_error_c: Option<f64>,
    error_band_c: Option<f64>,
}

#[derive(Clone, Debug)]
struct TemperatureRegression {
    intercept: f64,
    chancy_temperature: f64,
    arve_temperature: f64,
    arve_flow_fraction: f64,
}

impl TemperatureRegression {
    fn predict(&self, downstream_t: f64, arve_t: f64, arve_fraction: f64) -> f64 {
        self.intercept
            + self.chancy_temperature * downstream_t
            + self.arve_temperature * arve_t
            + self.arve_flow_fraction * arve_fraction
    }
}

#[derive(Clone, Debug)]
struct TemperatureCalibrationSample {
    timestamp: DateTime<FixedOffset>,
    downstream_q: f64,
    downstream_t: f64,
    arve_q: f64,
    arve_t: f64,
    observed_upstream_t: f64,
    heat_power_w: f64,
}

#[derive(Clone, Debug, Default)]
struct ProgrammeForecasts {
    seujet: Option<MetricSeries>,
}

impl ProgrammeForecasts {
    fn has_data(&self) -> bool {
        self.seujet
            .as_ref()
            .is_some_and(|series| !series.points.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct FeatureCollection {
    features: Vec<Feature>,
}

#[derive(Debug, Deserialize)]
struct Feature {
    properties: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct PlotEnvelope {
    plot: PlotData,
}

#[derive(Debug, Deserialize)]
struct PlotData {
    data: Vec<PlotTrace>,
}

#[derive(Debug, Deserialize)]
struct PlotTrace {
    name: String,
    x: Vec<String>,
    y: Vec<Option<f64>>,
    meta: Option<PlotMeta>,
}

#[derive(Debug, Deserialize)]
struct PlotMeta {
    unit: Option<String>,
}

#[derive(Debug, Error)]
enum FetchError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("station {0} not found in {1}")]
    StationNotFound(&'static str, &'static str),
    #[error("history for station {0} did not contain usable data")]
    EmptyHistory(&'static str),
    #[error("forecast for station {0} did not contain usable discharge data")]
    EmptyForecast(&'static str),
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nivrhone_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let client = Client::builder()
        .user_agent("rhonometre/0.1 (+https://github.com/huberlabgit/rhoneplot-master)")
        .timeout(Duration::from_secs(20))
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        client,
        cache: Arc::new(RwLock::new(None)),
    };

    let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "frontend/dist".to_string());
    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let static_service =
        ServeDir::new(&static_dir).fallback(ServeFile::new(format!("{static_dir}/index.html")));

    let app = Router::new()
        .route("/api/dashboard", get(dashboard_handler))
        .route("/healthz", get(healthz))
        .fallback_service(static_service)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));
    info!(%addr, %static_dir, "serving rhonometre");

    axum::serve(listener, app).await.expect("server failed");
}

async fn healthz() -> &'static str {
    "ok"
}

async fn dashboard_handler(State(state): State<AppState>) -> impl IntoResponse {
    match dashboard_data(&state).await {
        Ok(data) => (StatusCode::OK, Json(data)).into_response(),
        Err(err) => {
            error!(error = %err, "dashboard fetch failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": err.to_string() })),
            )
                .into_response()
        }
    }
}

async fn dashboard_data(state: &AppState) -> Result<DashboardData, FetchError> {
    if let Some(cached) = fresh_cache(state).await {
        return Ok(cached);
    }

    match fetch_dashboard(&state.client).await {
        Ok(mut data) => {
            data.cache_status = CacheStatus::Fresh;
            let mut cache = state.cache.write().await;
            *cache = Some(CachedDashboard {
                fetched_at: Instant::now(),
                data: data.clone(),
            });
            Ok(data)
        }
        Err(err) => {
            let cache = state.cache.read().await;
            if let Some(cached) = cache.as_ref() {
                warn!(error = %err, "serving stale dashboard cache after upstream failure");
                let mut data = cached.data.clone();
                data.cache_status = CacheStatus::Stale;
                data.warnings.push(format!(
                    "Hydrodaten refresh failed; serving stale data: {err}"
                ));
                return Ok(data);
            }
            Err(err)
        }
    }
}

async fn fresh_cache(state: &AppState) -> Option<DashboardData> {
    let cache = state.cache.read().await;
    let cached = cache.as_ref()?;
    if cached.fetched_at.elapsed() < CACHE_TTL {
        Some(cached.data.clone())
    } else {
        None
    }
}

async fn fetch_dashboard(client: &Client) -> Result<DashboardData, FetchError> {
    let pq = fetch_features(client, HYDRO_PQ_URL).await?;
    let temperature = fetch_features(client, HYDRO_TEMPERATURE_URL).await?;
    let pq_by_station = features_by_station(pq);
    let temperature_by_station = features_by_station(temperature);

    let mut stations = Vec::with_capacity(SOURCE_STATIONS.len() + 1);
    let mut warnings = Vec::new();
    let programme_forecasts = load_programme_forecasts(&mut warnings);
    let forecast_by_station = match fetch_features(client, HYDRO_PQ_FORECAST_URL).await {
        Ok(features) => Some(features_by_station(features)),
        Err(err) => {
            warn!(error = %err, "failed to fetch forecast station overview");
            warnings.push(format!(
                "Could not refresh Hydrodaten discharge forecast overview: {err}"
            ));
            None
        }
    };

    for station in SOURCE_STATIONS {
        let pq_feature = pq_by_station
            .get(station.id)
            .ok_or(FetchError::StationNotFound(station.id, HYDRO_PQ_URL))?;
        let temperature_feature = temperature_by_station.get(station.id);

        let mut current = parse_pq_current(pq_feature);
        if let Some(feature) = temperature_feature {
            if let Some(metric) = parse_temperature_current(feature) {
                current.push(metric);
            }
        }

        let mut history = Vec::new();
        match fetch_pq_history(client, station).await {
            Ok(mut series) => history.append(&mut series),
            Err(err) => {
                warn!(station = station.id, error = %err, "failed to fetch pq history");
                warnings.push(format!(
                    "Could not refresh discharge/water-level history for {}: {err}",
                    station.id
                ));
            }
        }

        if temperature_feature.is_some() {
            match fetch_temperature_history(client, station).await {
                Ok(mut series) => history.append(&mut series),
                Err(err) => {
                    warn!(station = station.id, error = %err, "failed to fetch temperature history");
                    warnings.push(format!(
                        "Could not refresh temperature history for {}: {err}",
                        station.id
                    ));
                }
            }
        }

        let mut forecast = Vec::new();
        if forecast_by_station
            .as_ref()
            .is_some_and(|features| features.contains_key(station.id))
        {
            match fetch_discharge_forecast(client, station).await {
                Ok(series) => forecast.push(series),
                Err(err) => {
                    warn!(station = station.id, error = %err, "failed to fetch discharge forecast");
                    warnings.push(format!(
                        "Could not refresh discharge forecast for {}: {err}",
                        station.id
                    ));
                }
            }
        }

        let status = match (current.is_empty(), history.is_empty()) {
            (false, false) => StationStatus::Complete,
            (false, true) | (true, false) => StationStatus::Partial,
            (true, true) => StationStatus::Missing,
        };

        stations.push(StationData {
            id: station.id,
            slug: station.slug,
            name_fr: station.name_fr,
            name_en: station.name_en,
            role_fr: station.role_fr,
            role_en: station.role_en,
            kind: station.kind,
            current,
            history,
            forecast,
            status,
            notice_fr: None,
            notice_en: None,
        });
    }

    let temperature_calibration = match fetch_temperature_calibration(client).await {
        Ok(calibration) => {
            info!(
                heat_power_mw = calibration.heat_power_w / 1_000_000.0,
                sample_count = calibration.sample_count,
                median_absolute_error_c = ?calibration.median_absolute_error_c,
                error_band_c = ?calibration.error_band_c,
                "calibrated derived 2606 temperature"
            );
            Some(calibration)
        }
        Err(err) => {
            warn!(error = %err, "failed to calibrate derived 2606 temperature");
            warnings.push(format!(
                "Could not calibrate the Rhône - Genève, Halle de l'Ile temperature estimate: {err}"
            ));
            None
        }
    };

    match derive_halle_ile_station(
        &stations,
        temperature_calibration.as_ref(),
        programme_forecasts.seujet.as_ref(),
    ) {
        Some(station) => stations.insert(1, station),
        None => warnings.push(
            "Could not derive Rhône - Genève, Halle de l'Ile from Arve and Chancy data".to_string(),
        ),
    }

    Ok(DashboardData {
        generated_at: chrono::Utc::now().to_rfc3339(),
        cache_status: CacheStatus::Fresh,
        source: SourceInfo {
            label: if programme_forecasts.has_data() {
                "Swiss Hydrodaten + SIG discharge programme"
            } else {
                "Swiss Hydrodaten"
            }
            .to_string(),
            url: "https://www.hydrodaten.admin.ch/de/seen-und-fluesse/messstationen-zustand"
                .to_string(),
        },
        stations,
        warnings,
    })
}

fn derive_halle_ile_station(
    source_stations: &[StationData],
    temperature_calibration: Option<&TemperatureCalibration>,
    seujet_programme_forecast: Option<&MetricSeries>,
) -> Option<StationData> {
    let arve = station_by_id(source_stations, "2170")?;
    let chancy = station_by_id(source_stations, "2174")?;

    let mut current = Vec::new();
    if let Some(metric) = derive_current_discharge(arve, chancy) {
        current.push(metric);
    }
    let mut history = Vec::new();
    if let Some(series) = derive_discharge_history(arve, chancy) {
        history.push(series);
    }
    let derived_temperature_history = temperature_calibration
        .and_then(|calibration| derive_temperature_history(arve, chancy, calibration));
    if let Some(metric) = derive_current_temperature(
        arve,
        chancy,
        derived_temperature_history.as_ref(),
        temperature_calibration,
    ) {
        current.push(metric);
    }
    if let Some(series) = derived_temperature_history {
        history.push(series);
    }

    let mut forecast = Vec::new();
    if let Some(series) = seujet_programme_forecast {
        forecast.push(series.clone());
    } else if let Some(series) = derive_discharge_forecast(arve, chancy) {
        forecast.push(series);
    }

    if current.is_empty() && history.is_empty() {
        return None;
    }

    let status = match (current.is_empty(), history.is_empty()) {
        (false, false) => StationStatus::Complete,
        (false, true) | (true, false) => StationStatus::Partial,
        (true, true) => StationStatus::Missing,
    };

    Some(StationData {
        id: DERIVED_HALLE_ILE_STATION.id,
        slug: DERIVED_HALLE_ILE_STATION.slug,
        name_fr: DERIVED_HALLE_ILE_STATION.name_fr,
        name_en: DERIVED_HALLE_ILE_STATION.name_en,
        role_fr: DERIVED_HALLE_ILE_STATION.role_fr,
        role_en: DERIVED_HALLE_ILE_STATION.role_en,
        kind: DERIVED_HALLE_ILE_STATION.kind,
        current,
        history,
        forecast,
        status,
        notice_fr: Some(
            "Estimation par bilan de chaleur calibré sur l'historique Arve/Chancy/2606: la station 2606 est hors ligne.",
        ),
        notice_en: Some(
            "Estimated from a heat balance calibrated on Arve/Chancy/2606 history while station 2606 is offline.",
        ),
    })
}

fn load_programme_forecasts(warnings: &mut Vec<String>) -> ProgrammeForecasts {
    let (roots, configured) = programme_source_roots();
    if roots.is_empty() {
        return ProgrammeForecasts::default();
    }

    let mut files = Vec::new();
    for root in &roots {
        collect_programme_files(root, &mut files, warnings);
    }
    files.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());

    let mut seujet_points = BTreeMap::new();
    let mut parsed_file_count = 0usize;
    for path in &files {
        match parse_programme_source_path(path, &mut seujet_points) {
            Ok(points) => {
                if points > 0 {
                    parsed_file_count += 1;
                }
            }
            Err(err) => {
                warn!(path = %path.display(), error = %err, "failed to parse SIG discharge programme");
                warnings.push(format!(
                    "Could not read SIG discharge programme {}: {err}",
                    path.display()
                ));
            }
        }
    }

    if seujet_points.is_empty() {
        if configured && !files.is_empty() {
            warnings.push(
                "Configured SIG discharge programme did not contain usable Q Seujet hourly points"
                    .to_string(),
            );
        }
        return ProgrammeForecasts::default();
    }

    info!(
        file_count = parsed_file_count,
        point_count = seujet_points.len(),
        "loaded SIG discharge programme"
    );

    ProgrammeForecasts {
        seujet: Some(MetricSeries {
            kind: MetricKind::Discharge,
            label_fr: "Programme SIG Seujet",
            label_en: "SIG Seujet programme",
            unit: "m³/s".to_string(),
            points: seujet_points
                .into_iter()
                .map(|(timestamp, value)| HistoryPoint {
                    timestamp: timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
                    value,
                })
                .collect(),
            uncertainty: None,
        }),
    }
}

fn programme_source_roots() -> (Vec<PathBuf>, bool) {
    let mut roots = Vec::new();
    let mut configured = false;

    if let Some(paths) = env::var_os(PROGRAMME_PATH_ENV) {
        roots.extend(env::split_paths(&paths));
        configured = true;
    }

    if let Some(path) = env::var_os(PROGRAMME_DIR_ENV) {
        roots.push(PathBuf::from(path));
        configured = true;
    }

    let local_dir = PathBuf::from(LOCAL_PROGRAMME_DIR);
    if local_dir.exists() {
        roots.push(local_dir);
        configured = true;
    }

    if roots.is_empty() {
        if let Some(path) = newest_download_programme_source() {
            roots.push(path);
        }
    }

    (roots, configured)
}

fn newest_download_programme_source() -> Option<PathBuf> {
    let downloads = PathBuf::from(env::var_os("HOME")?).join("Downloads");
    let entries = fs::read_dir(downloads).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_likely_programme_source(path))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn is_likely_programme_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let normalized = name
        .to_lowercase()
        .replace('é', "e")
        .replace('è', "e")
        .replace('ê', "e")
        .replace('ë', "e");
    let ascii_name = normalized
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();

    ascii_name.contains("programme")
        && ascii_name.contains("debit")
        && (path.is_file() || path.is_dir())
}

fn collect_programme_files(path: &Path, files: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    if path.is_file() {
        if should_attempt_programme_file(path) {
            files.push(path.to_path_buf());
        }
        return;
    }

    if path.is_dir() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(err) => {
                warnings.push(format!(
                    "Could not open SIG discharge programme directory {}: {err}",
                    path.display()
                ));
                return;
            }
        };

        for entry in entries.filter_map(Result::ok) {
            let child = entry.path();
            if child.is_file() && should_attempt_programme_file(&child) {
                files.push(child);
            }
        }
        return;
    }

    warnings.push(format!(
        "Configured SIG discharge programme path does not exist: {}",
        path.display()
    ));
}

fn should_attempt_programme_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("xls" | "xlsx" | "xlsm" | "xlsb" | "eml" | "txt" | "rtfd")
    )
}

fn parse_programme_source_path(
    path: &Path,
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> Result<usize, String> {
    let bytes = fs::read(path).map_err(|err| format!("failed to read programme source: {err}"))?;

    if is_workbook_path(path) {
        return parse_programme_workbook_bytes(&path.display().to_string(), &bytes, seujet_points);
    }

    parse_programme_mail_bytes(&path.display().to_string(), &bytes, seujet_points).or_else(
        |mail_err| {
            parse_programme_workbook_bytes(&path.display().to_string(), &bytes, seujet_points)
                .map_err(|workbook_err| {
                    format!(
                        "not a readable programme email ({mail_err}) or workbook ({workbook_err})"
                    )
                })
        },
    )
}

fn is_workbook_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("xls" | "xlsx" | "xlsm" | "xlsb")
    )
}

fn parse_programme_mail_bytes(
    name: &str,
    bytes: &[u8],
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> Result<usize, String> {
    let mail = parse_mail(bytes).map_err(|err| err.to_string())?;
    let mut point_count = 0usize;
    let mut attachment_errors = Vec::new();

    for part in mail.parts() {
        let disposition = part.get_content_disposition();
        let filename = disposition
            .params
            .get("filename")
            .cloned()
            .or_else(|| part.ctype.params.get("name").cloned());
        let mimetype = part.ctype.mimetype.to_ascii_lowercase();

        if !is_excel_attachment(filename.as_deref(), &mimetype) {
            continue;
        }

        let attachment_name = filename.unwrap_or_else(|| format!("{name} attachment"));
        let attachment = part
            .get_body_raw()
            .map_err(|err| format!("failed to decode attachment {attachment_name}: {err}"))?;
        match parse_programme_workbook_bytes(&attachment_name, &attachment, seujet_points) {
            Ok(points) => point_count += points,
            Err(err) => attachment_errors.push(format!("{attachment_name}: {err}")),
        }
    }

    if point_count == 0 && !attachment_errors.is_empty() {
        return Err(attachment_errors.join("; "));
    }

    Ok(point_count)
}

fn is_excel_attachment(filename: Option<&str>, mimetype: &str) -> bool {
    filename
        .map(|name| {
            matches!(
                Path::new(name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_ascii_lowercase())
                    .as_deref(),
                Some("xls" | "xlsx" | "xlsm" | "xlsb")
            )
        })
        .unwrap_or(false)
        || mimetype.contains("excel")
        || mimetype.contains("spreadsheet")
}

fn parse_programme_workbook_bytes(
    name: &str,
    bytes: &[u8],
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> Result<usize, String> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook =
        open_workbook_auto_from_rs(cursor).map_err(|err| format!("open workbook: {err}"))?;
    let mut point_count = 0usize;

    for sheet_name in workbook.sheet_names().to_owned() {
        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(|err| format!("read worksheet {sheet_name}: {err}"))?;
        point_count += collect_hourly_programme_points(&range, "Q Seujet", seujet_points);
    }

    if point_count == 0 {
        return Err(format!(
            "{name} did not contain Q Seujet hourly programme points"
        ));
    }

    Ok(point_count)
}

fn collect_hourly_programme_points(
    range: &Range<Data>,
    series_label: &str,
    points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> usize {
    let rows = range.rows().collect::<Vec<_>>();
    let mut point_count = 0usize;

    for (row_idx, row) in rows.iter().enumerate() {
        if !row_contains_label(row, series_label) {
            continue;
        }

        let Some(date) = row.iter().find_map(cell_date) else {
            continue;
        };
        let Some(headers) = find_time_headers(&rows, row_idx) else {
            continue;
        };
        let Some(midnight_idx) = headers.iter().position(|(_, hour)| *hour == 0) else {
            continue;
        };

        for (column_idx, hour) in headers.iter().skip(midnight_idx).take(24) {
            let Some(value) = row.get(*column_idx).and_then(cell_number) else {
                continue;
            };
            let Some(timestamp) = geneva_datetime(date, *hour) else {
                continue;
            };
            points.insert(timestamp, value);
            point_count += 1;
        }
    }

    point_count
}

fn find_time_headers(rows: &[&[Data]], row_idx: usize) -> Option<Vec<(usize, u32)>> {
    let start_idx = row_idx.saturating_sub(12);
    for header_idx in (start_idx..row_idx).rev() {
        let headers = rows[header_idx]
            .iter()
            .enumerate()
            .filter_map(|(column_idx, cell)| {
                Some((column_idx, parse_hour_label(cell_text(cell)?)?))
            })
            .collect::<Vec<_>>();

        if headers.len() >= 12 && headers.iter().any(|(_, hour)| *hour == 0) {
            return Some(headers);
        }
    }

    None
}

fn row_contains_label(row: &[Data], label: &str) -> bool {
    row.iter().any(|cell| {
        cell_text(cell)
            .as_deref()
            .map(str::trim)
            .is_some_and(|text| text.eq_ignore_ascii_case(label))
    })
}

fn cell_text(cell: &Data) -> Option<String> {
    match cell {
        Data::String(value) => Some(value.clone()),
        Data::Int(value) => Some(value.to_string()),
        Data::Float(value) => Some(value.to_string()),
        _ => None,
    }
}

fn cell_number(cell: &Data) -> Option<f64> {
    match cell {
        Data::Int(value) => Some(*value as f64),
        Data::Float(value) => Some(*value),
        Data::String(value) => value.trim().replace(',', ".").parse::<f64>().ok(),
        _ => None,
    }
}

fn cell_date(cell: &Data) -> Option<NaiveDate> {
    match cell {
        Data::DateTime(value) => excel_datetime_date(*value),
        Data::Float(value) => excel_datetime_date(ExcelDateTime::new(
            *value,
            ExcelDateTimeType::DateTime,
            false,
        )),
        Data::Int(value) => excel_datetime_date(ExcelDateTime::new(
            *value as f64,
            ExcelDateTimeType::DateTime,
            false,
        )),
        Data::String(value) => NaiveDate::parse_from_str(value.trim(), "%d.%m.%Y")
            .or_else(|_| NaiveDate::parse_from_str(value.trim(), "%d/%m/%Y"))
            .ok(),
        _ => None,
    }
}

fn excel_datetime_date(value: ExcelDateTime) -> Option<NaiveDate> {
    let (year, month, day, _, _, _, _) = value.to_ymd_hms_milli();
    NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
}

fn parse_hour_label(value: String) -> Option<u32> {
    let normalized = value.trim().to_ascii_lowercase().replace(' ', "");
    let (hour, _) = normalized.split_once('h')?;
    let hour = hour.parse::<u32>().ok()?;
    (hour < 24).then_some(hour)
}

fn geneva_datetime(date: NaiveDate, hour: u32) -> Option<DateTime<FixedOffset>> {
    let time = NaiveTime::from_hms_opt(hour, 0, 0)?;
    let offset = geneva_offset(date);
    offset
        .from_local_datetime(&NaiveDateTime::new(date, time))
        .single()
}

fn geneva_offset(date: NaiveDate) -> FixedOffset {
    let year = date.year();
    let dst_start = last_sunday(year, 3);
    let dst_end = last_sunday(year, 10);
    let seconds = if date >= dst_start && date < dst_end {
        2 * 60 * 60
    } else {
        60 * 60
    };
    FixedOffset::east_opt(seconds).expect("valid Geneva UTC offset")
}

fn last_sunday(year: i32, month: u32) -> NaiveDate {
    let mut date = NaiveDate::from_ymd_opt(year, month, 31).expect("valid month end");
    while date.weekday().num_days_from_sunday() != 0 {
        date = date.pred_opt().expect("previous day exists");
    }
    date
}

async fn fetch_temperature_calibration(
    client: &Client,
) -> Result<TemperatureCalibration, FetchError> {
    let arve = &SOURCE_STATIONS[0];
    let chancy = &SOURCE_STATIONS[2];

    let arve_pq = fetch_pq_history_window(client, arve, "40days", CALIBRATION_HISTORY_DAYS).await?;
    let chancy_pq =
        fetch_pq_history_window(client, chancy, "40days", CALIBRATION_HISTORY_DAYS).await?;
    let arve_temperature =
        fetch_temperature_history_window(client, arve, "40days", CALIBRATION_HISTORY_DAYS).await?;
    let chancy_temperature =
        fetch_temperature_history_window(client, chancy, "40days", CALIBRATION_HISTORY_DAYS)
            .await?;
    let observed_temperature = fetch_temperature_history_window(
        client,
        &DERIVED_HALLE_ILE_STATION,
        "40days",
        CALIBRATION_HISTORY_DAYS,
    )
    .await?;

    let arve_discharge = arve_pq
        .iter()
        .find(|series| series.kind == MetricKind::Discharge)
        .ok_or(FetchError::EmptyHistory(arve.id))?;
    let chancy_discharge = chancy_pq
        .iter()
        .find(|series| series.kind == MetricKind::Discharge)
        .ok_or(FetchError::EmptyHistory(chancy.id))?;
    let arve_temperature = arve_temperature
        .iter()
        .find(|series| series.kind == MetricKind::Temperature)
        .ok_or(FetchError::EmptyHistory(arve.id))?;
    let chancy_temperature = chancy_temperature
        .iter()
        .find(|series| series.kind == MetricKind::Temperature)
        .ok_or(FetchError::EmptyHistory(chancy.id))?;
    let observed_temperature = observed_temperature
        .iter()
        .find(|series| series.kind == MetricKind::Temperature)
        .ok_or(FetchError::EmptyHistory(DERIVED_HALLE_ILE_STATION.id))?;

    derive_temperature_calibration(
        arve_discharge,
        chancy_discharge,
        arve_temperature,
        chancy_temperature,
        observed_temperature,
    )
    .ok_or(FetchError::EmptyHistory(DERIVED_HALLE_ILE_STATION.id))
}

fn station_by_id<'a>(stations: &'a [StationData], id: &str) -> Option<&'a StationData> {
    stations.iter().find(|station| station.id == id)
}

fn current_metric(station: &StationData, kind: MetricKind) -> Option<&CurrentMetric> {
    station.current.iter().find(|metric| metric.kind == kind)
}

fn history_series(station: &StationData, kind: MetricKind) -> Option<&MetricSeries> {
    station.history.iter().find(|series| series.kind == kind)
}

fn forecast_series(station: &StationData, kind: MetricKind) -> Option<&MetricSeries> {
    station.forecast.iter().find(|series| series.kind == kind)
}

fn derive_current_discharge(arve: &StationData, chancy: &StationData) -> Option<CurrentMetric> {
    let arve_discharge = current_metric(arve, MetricKind::Discharge)?;
    let chancy_discharge = current_metric(chancy, MetricKind::Discharge)?;
    let value = chancy_discharge.value - arve_discharge.value;
    if value <= 0.0 {
        return None;
    }

    Some(CurrentMetric {
        kind: MetricKind::Discharge,
        label_fr: "Débit calculé",
        label_en: "Derived discharge",
        value,
        unit: "m³/s".to_string(),
        measured_at: oldest_timestamp(&arve_discharge.measured_at, &chancy_discharge.measured_at),
        range_24h: derive_discharge_range(arve_discharge, chancy_discharge),
    })
}

fn derive_current_temperature(
    arve: &StationData,
    chancy: &StationData,
    derived_history: Option<&MetricSeries>,
    calibration: Option<&TemperatureCalibration>,
) -> Option<CurrentMetric> {
    if let Some(point) = derived_history.and_then(|series| series.points.last()) {
        return Some(CurrentMetric {
            kind: MetricKind::Temperature,
            label_fr: "Température calculée",
            label_en: "Derived temperature",
            value: point.value,
            unit: "°C".to_string(),
            measured_at: point.timestamp.clone(),
            range_24h: None,
        });
    }

    let calibration = calibration?;
    let arve_discharge = current_metric(arve, MetricKind::Discharge)?;
    let chancy_discharge = current_metric(chancy, MetricKind::Discharge)?;
    let arve_temperature = current_metric(arve, MetricKind::Temperature)?;
    let chancy_temperature = current_metric(chancy, MetricKind::Temperature)?;
    let value = calibrated_upstream_temperature(
        chancy_discharge.value,
        chancy_temperature.value,
        arve_discharge.value,
        arve_temperature.value,
        calibration,
    )?;

    let measured_at = [
        arve_discharge.measured_at.as_str(),
        chancy_discharge.measured_at.as_str(),
        arve_temperature.measured_at.as_str(),
        chancy_temperature.measured_at.as_str(),
    ]
    .into_iter()
    .reduce(oldest_timestamp_str)
    .unwrap_or_default()
    .to_string();

    Some(CurrentMetric {
        kind: MetricKind::Temperature,
        label_fr: "Température calculée",
        label_en: "Derived temperature",
        value,
        unit: "°C".to_string(),
        measured_at,
        range_24h: None,
    })
}

fn derive_discharge_range(
    arve_discharge: &CurrentMetric,
    chancy_discharge: &CurrentMetric,
) -> Option<MetricRange> {
    let arve = arve_discharge.range_24h.as_ref()?;
    let chancy = chancy_discharge.range_24h.as_ref()?;
    Some(MetricRange {
        min: (chancy.min - arve.max).max(0.0),
        max: (chancy.max - arve.min).max(0.0),
        mean: chancy.mean.zip(arve.mean).map(|(down, arve)| down - arve),
    })
}

fn derive_discharge_history(arve: &StationData, chancy: &StationData) -> Option<MetricSeries> {
    let arve_discharge = history_series(arve, MetricKind::Discharge)?;
    let chancy_discharge = history_series(chancy, MetricKind::Discharge)?;
    let arve_by_time = series_map(arve_discharge);
    let points = chancy_discharge
        .points
        .iter()
        .filter_map(|point| {
            let arve_value = arve_by_time.get(&point.timestamp)?;
            let value = point.value - arve_value;
            (value > 0.0).then(|| HistoryPoint {
                timestamp: point.timestamp.clone(),
                value,
            })
        })
        .collect::<Vec<_>>();

    (!points.is_empty()).then(|| MetricSeries {
        kind: MetricKind::Discharge,
        label_fr: "Débit calculé",
        label_en: "Derived discharge",
        unit: "m³/s".to_string(),
        points,
        uncertainty: None,
    })
}

fn derive_discharge_forecast(arve: &StationData, chancy: &StationData) -> Option<MetricSeries> {
    let arve_discharge = forecast_series(arve, MetricKind::Discharge)?;
    let chancy_discharge = forecast_series(chancy, MetricKind::Discharge)?;
    let arve_by_time = series_map(arve_discharge);
    let points = chancy_discharge
        .points
        .iter()
        .filter_map(|point| {
            let arve_value = arve_by_time.get(&point.timestamp)?;
            let value = point.value - arve_value;
            (value > 0.0).then(|| HistoryPoint {
                timestamp: point.timestamp.clone(),
                value,
            })
        })
        .collect::<Vec<_>>();

    (!points.is_empty()).then(|| MetricSeries {
        kind: MetricKind::Discharge,
        label_fr: "Prévision du débit calculé",
        label_en: "Derived discharge forecast",
        unit: "m³/s".to_string(),
        points,
        uncertainty: None,
    })
}

fn derive_temperature_history(
    arve: &StationData,
    chancy: &StationData,
    calibration: &TemperatureCalibration,
) -> Option<MetricSeries> {
    let arve_discharge = history_series(arve, MetricKind::Discharge)?;
    let chancy_discharge = history_series(chancy, MetricKind::Discharge)?;
    let arve_temperature = history_series(arve, MetricKind::Temperature)?;
    let chancy_temperature = history_series(chancy, MetricKind::Temperature)?;

    let arve_discharge_points = timed_points(arve_discharge);
    let chancy_discharge_points = timed_points(chancy_discharge);
    let arve_temperature_points = timed_points(arve_temperature);

    let points = chancy_temperature
        .points
        .iter()
        .filter_map(|point| {
            derive_lagged_temperature_point(
                point,
                &chancy_discharge_points,
                &arve_discharge_points,
                &arve_temperature_points,
                calibration,
            )
        })
        .collect::<Vec<_>>();

    (!points.is_empty()).then(|| MetricSeries {
        kind: MetricKind::Temperature,
        label_fr: "Température calculée",
        label_en: "Derived temperature",
        unit: "°C".to_string(),
        points,
        uncertainty: calibration.error_band_c.map(|error| MetricUncertainty {
            lower: error,
            upper: error,
            confidence: 0.90,
        }),
    })
}

fn derive_lagged_temperature_point(
    chancy_temperature: &HistoryPoint,
    chancy_discharge: &[TimedPoint],
    arve_discharge: &[TimedPoint],
    arve_temperature: &[TimedPoint],
    calibration: &TemperatureCalibration,
) -> Option<HistoryPoint> {
    let chancy_time = parse_timestamp(&chancy_temperature.timestamp)?;
    let chancy_q = interpolate_at(chancy_discharge, chancy_time)?;
    let downstream_lag = travel_duration(JONCTION_TO_CHANCY_M, chancy_q)?;
    let confluence_time = chancy_time - downstream_lag;

    let arve_q_for_lag = interpolate_at(arve_discharge, confluence_time)?;
    let rhone_q_for_lag = chancy_q - arve_q_for_lag;
    if rhone_q_for_lag <= 0.0 {
        return None;
    }

    let arve_lag = travel_duration(ARVE_STATION_TO_JONCTION_M, arve_q_for_lag)?;
    let rhone_lag = travel_duration(HALLE_TO_JONCTION_M, rhone_q_for_lag)?;
    let arve_time = confluence_time - arve_lag;
    let rhone_time = confluence_time - rhone_lag;

    let arve_q = interpolate_at(arve_discharge, arve_time)?;
    let arve_t = interpolate_at(arve_temperature, arve_time)?;
    let value = calibrated_upstream_temperature(
        chancy_q,
        chancy_temperature.value,
        arve_q,
        arve_t,
        calibration,
    )?;

    Some(HistoryPoint {
        timestamp: rhone_time.to_rfc3339_opts(SecondsFormat::Millis, true),
        value,
    })
}

fn travel_duration(distance_m: f64, discharge_m3s: f64) -> Option<chrono::Duration> {
    if !(distance_m.is_finite() && discharge_m3s.is_finite()) || discharge_m3s <= 0.0 {
        return None;
    }

    let velocity_mps = estimated_velocity_mps(discharge_m3s);
    let seconds = (distance_m / velocity_mps).round() as i64;
    Some(chrono::Duration::seconds(seconds))
}

fn estimated_velocity_mps(discharge_m3s: f64) -> f64 {
    (0.30 + 0.0029 * discharge_m3s).clamp(0.40, 2.20)
}

fn calibrated_upstream_temperature(
    downstream_q: f64,
    downstream_t: f64,
    arve_q: f64,
    arve_t: f64,
    calibration: &TemperatureCalibration,
) -> Option<f64> {
    let upstream_q = downstream_q - arve_q;
    if upstream_q <= MIN_DERIVED_RHONE_DISCHARGE_M3S || downstream_q <= 0.0 {
        return None;
    }

    let arve_fraction = arve_q / downstream_q;
    let upstream_t = calibration
        .regression
        .predict(downstream_t, arve_t, arve_fraction);
    (-2.0..=32.0).contains(&upstream_t).then_some(upstream_t)
}

fn downstream_heat_power(
    downstream_q: f64,
    downstream_t: f64,
    arve_q: f64,
    arve_t: f64,
    observed_upstream_t: f64,
) -> Option<f64> {
    let upstream_q = downstream_q - arve_q;
    if upstream_q <= MIN_DERIVED_RHONE_DISCHARGE_M3S {
        return None;
    }

    let heat_capacity = WATER_DENSITY_KG_M3 * WATER_SPECIFIC_HEAT_J_KG_C;
    let heat_power = heat_capacity
        * ((downstream_q * downstream_t) - (arve_q * arve_t) - (upstream_q * observed_upstream_t));
    heat_power.is_finite().then_some(heat_power)
}

fn derive_temperature_calibration(
    arve_discharge: &MetricSeries,
    chancy_discharge: &MetricSeries,
    arve_temperature: &MetricSeries,
    chancy_temperature: &MetricSeries,
    observed_temperature: &MetricSeries,
) -> Option<TemperatureCalibration> {
    let arve_discharge_points = timed_points(arve_discharge);
    let chancy_discharge_points = timed_points(chancy_discharge);
    let arve_temperature_points = timed_points(arve_temperature);
    let observed_temperature_points = timed_points(observed_temperature);

    let mut samples = chancy_temperature
        .points
        .iter()
        .filter_map(|point| {
            derive_temperature_calibration_sample(
                point,
                &chancy_discharge_points,
                &arve_discharge_points,
                &arve_temperature_points,
                &observed_temperature_points,
            )
        })
        .collect::<Vec<_>>();

    if samples.len() < 24 {
        return None;
    }

    let mut heat_values = samples
        .iter()
        .map(|sample| sample.heat_power_w)
        .collect::<Vec<_>>();
    heat_values.sort_by(f64_total_cmp);
    let lower = percentile_sorted(&heat_values, 0.05)?;
    let upper = percentile_sorted(&heat_values, 0.95)?;
    samples.retain(|sample| (lower..=upper).contains(&sample.heat_power_w));

    let latest = samples.iter().map(|sample| sample.timestamp).max()?;
    let mut weighted_heat_sum = 0.0;
    let mut weight_sum = 0.0;
    for sample in &samples {
        let age_days = latest
            .signed_duration_since(sample.timestamp)
            .num_seconds()
            .max(0) as f64
            / 86_400.0;
        let weight = 0.5_f64.powf(age_days / HEAT_CALIBRATION_HALF_LIFE_DAYS);
        weighted_heat_sum += weight * sample.heat_power_w;
        weight_sum += weight;
    }

    if weight_sum <= 0.0 {
        return None;
    }

    let heat_power_w = weighted_heat_sum / weight_sum;
    let regression = fit_temperature_regression(&samples)?;
    let mut absolute_errors = samples
        .iter()
        .filter_map(|sample| {
            let arve_fraction = sample.arve_q / sample.downstream_q;
            let predicted = regression.predict(sample.downstream_t, sample.arve_t, arve_fraction);
            if !(-2.0..=32.0).contains(&predicted) {
                return None;
            }
            Some((predicted - sample.observed_upstream_t).abs())
        })
        .collect::<Vec<_>>();
    absolute_errors.sort_by(f64_total_cmp);

    Some(TemperatureCalibration {
        heat_power_w,
        regression,
        sample_count: samples.len(),
        median_absolute_error_c: percentile_sorted(&absolute_errors, 0.50),
        error_band_c: percentile_sorted(&absolute_errors, 0.90),
    })
}

fn derive_temperature_calibration_sample(
    chancy_temperature: &HistoryPoint,
    chancy_discharge: &[TimedPoint],
    arve_discharge: &[TimedPoint],
    arve_temperature: &[TimedPoint],
    observed_temperature: &[TimedPoint],
) -> Option<TemperatureCalibrationSample> {
    let chancy_time = parse_timestamp(&chancy_temperature.timestamp)?;
    let chancy_q = interpolate_at(chancy_discharge, chancy_time)?;
    let downstream_lag = travel_duration(JONCTION_TO_CHANCY_M, chancy_q)?;
    let confluence_time = chancy_time - downstream_lag;

    let arve_q_for_lag = interpolate_at(arve_discharge, confluence_time)?;
    let rhone_q_for_lag = chancy_q - arve_q_for_lag;
    if rhone_q_for_lag <= MIN_DERIVED_RHONE_DISCHARGE_M3S {
        return None;
    }

    let arve_lag = travel_duration(ARVE_STATION_TO_JONCTION_M, arve_q_for_lag)?;
    let rhone_lag = travel_duration(HALLE_TO_JONCTION_M, rhone_q_for_lag)?;
    let arve_time = confluence_time - arve_lag;
    let rhone_time = confluence_time - rhone_lag;

    let arve_q = interpolate_at(arve_discharge, arve_time)?;
    let arve_t = interpolate_at(arve_temperature, arve_time)?;
    let observed_upstream_t = interpolate_at(observed_temperature, rhone_time)?;
    let heat_power_w = downstream_heat_power(
        chancy_q,
        chancy_temperature.value,
        arve_q,
        arve_t,
        observed_upstream_t,
    )?;

    Some(TemperatureCalibrationSample {
        timestamp: rhone_time,
        downstream_q: chancy_q,
        downstream_t: chancy_temperature.value,
        arve_q,
        arve_t,
        observed_upstream_t,
        heat_power_w,
    })
}

fn fit_temperature_regression(
    samples: &[TemperatureCalibrationSample],
) -> Option<TemperatureRegression> {
    let mut matrix = [[0.0; 4]; 4];
    let mut target = [0.0; 4];
    let mut rows = 0usize;

    for sample in samples {
        if sample.downstream_q <= 0.0 {
            continue;
        }

        let upstream_q = sample.downstream_q - sample.arve_q;
        if upstream_q <= MIN_DERIVED_RHONE_DISCHARGE_M3S {
            continue;
        }

        let arve_fraction = sample.arve_q / sample.downstream_q;
        let features = [1.0, sample.downstream_t, sample.arve_t, arve_fraction];
        if !features.iter().all(|value| value.is_finite())
            || !sample.observed_upstream_t.is_finite()
        {
            continue;
        }

        for row in 0..4 {
            target[row] += features[row] * sample.observed_upstream_t;
            for col in 0..4 {
                matrix[row][col] += features[row] * features[col];
            }
        }
        rows += 1;
    }

    if rows < 24 {
        return None;
    }

    let coefficients = solve_4x4(matrix, target)?;
    Some(TemperatureRegression {
        intercept: coefficients[0],
        chancy_temperature: coefficients[1],
        arve_temperature: coefficients[2],
        arve_flow_fraction: coefficients[3],
    })
}

fn solve_4x4(mut matrix: [[f64; 4]; 4], mut target: [f64; 4]) -> Option<[f64; 4]> {
    for pivot in 0..4 {
        let mut best_row = pivot;
        for row in (pivot + 1)..4 {
            if matrix[row][pivot].abs() > matrix[best_row][pivot].abs() {
                best_row = row;
            }
        }

        if best_row != pivot {
            matrix.swap(pivot, best_row);
            target.swap(pivot, best_row);
        }

        let divisor = matrix[pivot][pivot];
        if !divisor.is_finite() || divisor.abs() < 1e-12 {
            return None;
        }

        for col in pivot..4 {
            matrix[pivot][col] /= divisor;
        }
        target[pivot] /= divisor;

        for row in 0..4 {
            if row == pivot {
                continue;
            }

            let factor = matrix[row][pivot];
            for col in pivot..4 {
                matrix[row][col] -= factor * matrix[pivot][col];
            }
            target[row] -= factor * target[pivot];
        }
    }

    target
        .iter()
        .all(|value| value.is_finite())
        .then_some(target)
}

fn f64_total_cmp(left: &f64, right: &f64) -> std::cmp::Ordering {
    left.total_cmp(right)
}

fn percentile_sorted(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }

    let index = ((values.len() - 1) as f64 * percentile.clamp(0.0, 1.0)).round() as usize;
    values.get(index).copied()
}

fn series_map(series: &MetricSeries) -> HashMap<String, f64> {
    series
        .points
        .iter()
        .map(|point| (point.timestamp.clone(), point.value))
        .collect()
}

fn timed_points(series: &MetricSeries) -> Vec<TimedPoint> {
    let mut points = series
        .points
        .iter()
        .filter_map(|point| {
            Some(TimedPoint {
                timestamp: parse_timestamp(&point.timestamp)?,
                value: point.value,
            })
        })
        .collect::<Vec<_>>();
    points.sort_by_key(|point| point.timestamp);
    points
}

fn interpolate_at(points: &[TimedPoint], target: DateTime<FixedOffset>) -> Option<f64> {
    if points.is_empty() {
        return None;
    }

    let index = points.partition_point(|point| point.timestamp < target);
    if index < points.len() && points[index].timestamp == target {
        return Some(points[index].value);
    }
    if index == 0 || index == points.len() {
        return None;
    }

    let before = &points[index - 1];
    let after = &points[index];
    let before_ms = before.timestamp.timestamp_millis();
    let after_ms = after.timestamp.timestamp_millis();
    let target_ms = target.timestamp_millis();
    if after_ms <= before_ms {
        return Some(before.value);
    }

    let ratio = (target_ms - before_ms) as f64 / (after_ms - before_ms) as f64;
    Some(before.value + ratio * (after.value - before.value))
}

fn oldest_timestamp(left: &str, right: &str) -> String {
    oldest_timestamp_str(left, right).to_string()
}

fn oldest_timestamp_str<'a>(left: &'a str, right: &'a str) -> &'a str {
    match (parse_timestamp(left), parse_timestamp(right)) {
        (Some(left_time), Some(right_time)) if right_time < left_time => right,
        _ => left,
    }
}

async fn fetch_features(client: &Client, url: &'static str) -> Result<Vec<Feature>, FetchError> {
    let collection = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<FeatureCollection>()
        .await?;
    Ok(collection.features)
}

fn features_by_station(features: Vec<Feature>) -> HashMap<String, Feature> {
    features
        .into_iter()
        .filter_map(|feature| {
            let key = prop_string(&feature.properties, "key")?;
            Some((key, feature))
        })
        .collect()
}

fn parse_pq_current(feature: &Feature) -> Vec<CurrentMetric> {
    let mut current = Vec::new();
    let props = &feature.properties;

    if let Some(value) = prop_measurement(props, "sensor_discharge_last_value")
        .or_else(|| generic_current_value(props, "discharge_ms"))
    {
        current.push(CurrentMetric {
            kind: MetricKind::Discharge,
            label_fr: "Débit",
            label_en: "Discharge",
            value,
            unit: "m³/s".to_string(),
            measured_at: prop_string(props, "sensor_discharge_measured_at")
                .or_else(|| prop_string(props, "last_measured_at"))
                .unwrap_or_default(),
            range_24h: Some(MetricRange {
                min: prop_measurement(props, "sensor_discharge_min_24h")
                    .or_else(|| prop_numberish(props, "min_24h"))
                    .unwrap_or(value),
                max: prop_measurement(props, "sensor_discharge_max_24h")
                    .or_else(|| prop_numberish(props, "max_24h"))
                    .unwrap_or(value),
                mean: prop_measurement(props, "sensor_discharge_mean_24h")
                    .or_else(|| prop_numberish(props, "mean_24h")),
            }),
        });
    }

    if let Some(value) = prop_measurement(props, "sensor_waterlevel_last_value")
        .or_else(|| generic_current_value(props, "masl"))
    {
        current.push(CurrentMetric {
            kind: MetricKind::WaterLevel,
            label_fr: "Niveau",
            label_en: "Water level",
            value,
            unit: "m ü.M.".to_string(),
            measured_at: prop_string(props, "sensor_waterlevel_measured_at")
                .or_else(|| prop_string(props, "last_measured_at"))
                .unwrap_or_default(),
            range_24h: Some(MetricRange {
                min: prop_measurement(props, "sensor_waterlevel_min_24h")
                    .or_else(|| prop_numberish(props, "min_24h"))
                    .unwrap_or(value),
                max: prop_measurement(props, "sensor_waterlevel_max_24h")
                    .or_else(|| prop_numberish(props, "max_24h"))
                    .unwrap_or(value),
                mean: prop_measurement(props, "sensor_waterlevel_mean_24h")
                    .or_else(|| prop_numberish(props, "mean_24h")),
            }),
        });
    }

    current
}

fn parse_temperature_current(feature: &Feature) -> Option<CurrentMetric> {
    let props = &feature.properties;
    let value = prop_numberish(props, "last_value")?;
    Some(CurrentMetric {
        kind: MetricKind::Temperature,
        label_fr: "Température",
        label_en: "Temperature",
        value,
        unit: prop_string(props, "unit").unwrap_or_else(|| "°C".to_string()),
        measured_at: prop_string(props, "last_measured_at").unwrap_or_default(),
        range_24h: Some(MetricRange {
            min: prop_numberish(props, "min_24h").unwrap_or(value),
            max: prop_numberish(props, "max_24h").unwrap_or(value),
            mean: prop_numberish(props, "mean_24h"),
        }),
    })
}

fn generic_current_value(props: &HashMap<String, Value>, expected_metric: &str) -> Option<f64> {
    match prop_string(props, "metric").as_deref() {
        Some(metric) if metric == expected_metric => prop_numberish(props, "last_value"),
        _ => None,
    }
}

async fn fetch_pq_history(
    client: &Client,
    station: &StationConfig,
) -> Result<Vec<MetricSeries>, FetchError> {
    fetch_pq_history_window(client, station, "7days", HISTORY_DAYS).await
}

async fn fetch_pq_history_window(
    client: &Client,
    station: &StationConfig,
    window: &str,
    days: i64,
) -> Result<Vec<MetricSeries>, FetchError> {
    let url = format!(
        "{HYDRO_BASE_URL}/plots/p_q_{window}/{id}_p_q_{window}_de.json",
        id = station.id,
    );
    fetch_history(client, station, &url, days).await
}

async fn fetch_temperature_history(
    client: &Client,
    station: &StationConfig,
) -> Result<Vec<MetricSeries>, FetchError> {
    fetch_temperature_history_window(client, station, "7days", HISTORY_DAYS).await
}

async fn fetch_temperature_history_window(
    client: &Client,
    station: &StationConfig,
    window: &str,
    days: i64,
) -> Result<Vec<MetricSeries>, FetchError> {
    let url = format!(
        "{HYDRO_BASE_URL}/plots/temperature_{window}/{id}_temperature_{window}_de.json",
        id = station.id,
    );
    fetch_history(client, station, &url, days).await
}

async fn fetch_discharge_forecast(
    client: &Client,
    station: &StationConfig,
) -> Result<MetricSeries, FetchError> {
    let url = format!(
        "{HYDRO_BASE_URL}/plots/q_forecast/{id}_q_forecast_de.json",
        id = station.id
    );
    let envelope = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<PlotEnvelope>()
        .await?;

    envelope
        .plot
        .data
        .into_iter()
        .find_map(parse_discharge_forecast_trace)
        .ok_or(FetchError::EmptyForecast(station.id))
}

async fn fetch_history(
    client: &Client,
    station: &StationConfig,
    url: &str,
    days: i64,
) -> Result<Vec<MetricSeries>, FetchError> {
    let envelope = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<PlotEnvelope>()
        .await?;

    let mut series = envelope
        .plot
        .data
        .into_iter()
        .filter_map(parse_trace)
        .collect::<Vec<_>>();

    for item in &mut series {
        trim_to_latest_days(&mut item.points, days);
    }

    series.retain(|item| !item.points.is_empty());

    if series.is_empty() {
        return Err(FetchError::EmptyHistory(station.id));
    }

    Ok(series)
}

fn parse_discharge_forecast_trace(trace: PlotTrace) -> Option<MetricSeries> {
    if !trace.name.to_lowercase().contains("median") {
        return None;
    }

    let unit = trace
        .meta
        .and_then(|meta| meta.unit)
        .unwrap_or_else(|| default_unit(&MetricKind::Discharge).to_string());
    let points = trace
        .x
        .into_iter()
        .zip(trace.y)
        .filter_map(|(timestamp, value)| {
            Some(HistoryPoint {
                timestamp,
                value: value?,
            })
        })
        .collect::<Vec<_>>();

    (!points.is_empty()).then(|| MetricSeries {
        kind: MetricKind::Discharge,
        label_fr: "Prévision du débit",
        label_en: "Discharge forecast",
        unit,
        points,
        uncertainty: None,
    })
}

fn parse_trace(trace: PlotTrace) -> Option<MetricSeries> {
    let kind = metric_kind_from_trace(&trace.name)?;
    let unit = trace
        .meta
        .and_then(|meta| meta.unit)
        .unwrap_or_else(|| default_unit(&kind).to_string());

    let points = trace
        .x
        .into_iter()
        .zip(trace.y)
        .filter_map(|(timestamp, value)| {
            Some(HistoryPoint {
                timestamp,
                value: value?,
            })
        })
        .collect::<Vec<_>>();

    Some(MetricSeries {
        label_fr: metric_label_fr(&kind),
        label_en: metric_label_en(&kind),
        kind,
        unit,
        points,
        uncertainty: None,
    })
}

fn metric_kind_from_trace(name: &str) -> Option<MetricKind> {
    let normalized = name.to_lowercase();
    if normalized.contains("abfluss") || normalized.contains("débit") {
        Some(MetricKind::Discharge)
    } else if normalized.contains("wasserstand") || normalized.contains("niveau") {
        Some(MetricKind::WaterLevel)
    } else if normalized.contains("temperatur") || normalized.contains("temp") {
        Some(MetricKind::Temperature)
    } else {
        None
    }
}

fn metric_label_fr(kind: &MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "Débit",
        MetricKind::WaterLevel => "Niveau",
        MetricKind::Temperature => "Température",
    }
}

fn metric_label_en(kind: &MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "Discharge",
        MetricKind::WaterLevel => "Water level",
        MetricKind::Temperature => "Temperature",
    }
}

fn default_unit(kind: &MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "m³/s",
        MetricKind::WaterLevel => "m ü.M.",
        MetricKind::Temperature => "°C",
    }
}

fn trim_to_latest_days(points: &mut Vec<HistoryPoint>, days: i64) {
    let Some(latest) = points
        .iter()
        .filter_map(|point| parse_timestamp(&point.timestamp))
        .max()
    else {
        return;
    };
    let cutoff = latest - chrono::Duration::days(days);
    points.retain(|point| {
        parse_timestamp(&point.timestamp)
            .map(|timestamp| timestamp >= cutoff)
            .unwrap_or(false)
    });
}

fn parse_timestamp(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

fn prop_string(props: &HashMap<String, Value>, key: &str) -> Option<String> {
    match props.get(key)? {
        Value::String(value) => Some(value.clone()),
        value => Some(value.to_string()),
    }
}

fn prop_numberish(props: &HashMap<String, Value>, key: &str) -> Option<f64> {
    match props.get(key)? {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => parse_first_number(value),
        _ => None,
    }
}

fn prop_measurement(props: &HashMap<String, Value>, key: &str) -> Option<f64> {
    prop_string(props, key).and_then(|value| parse_first_number(&value))
}

fn parse_first_number(value: &str) -> Option<f64> {
    let candidate = value.trim().split_whitespace().next()?.replace(',', ".");
    candidate.parse::<f64>().ok()
}
