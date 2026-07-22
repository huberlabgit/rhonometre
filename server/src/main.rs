use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    io::Cursor,
    net::{IpAddr, SocketAddr},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{any, get, get_service, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use calamine::{open_workbook_auto_from_rs, Data, ExcelDateTime, ExcelDateTimeType, Range, Reader};
use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone,
    Utc,
};
use futures_util::stream;
use hmac::{Hmac, KeyInit, Mac};
use imap_rs::{
    client::{
        flags::{Flag, StoreAction},
        search::{SearchKey, SearchQuery},
    },
    connect_tls,
    credentials::Password,
};
use mailparse::parse_mail;
use mailparse::MailHeaderMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use thiserror::Error;
use tokio::{net::TcpListener, sync::RwLock, time::sleep};
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
const HYDRO_BASE_URL: &str = "https://www.hydrodaten.admin.ch";
const OPEN_METEO_URL: &str = "https://api.open-meteo.com/v1/forecast";
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
const DATABASE_URL_ENV: &str = "DATABASE_URL";
const DATABASE_CONNECT_ATTEMPTS_ENV: &str = "RHONOMETRE_DATABASE_CONNECT_ATTEMPTS";
const DATABASE_CONNECT_RETRY_SECONDS_ENV: &str = "RHONOMETRE_DATABASE_CONNECT_RETRY_SECONDS";
const INGEST_TOKEN_ENV: &str = "RHONOMETRE_INGEST_TOKEN";
const PRO_CODE_ENV: &str = "RHONOMETRE_PRO_CODE";
const TOKEN_SECRET_ENV: &str = "RHONOMETRE_TOKEN_SECRET";
const IMAP_HOST_ENV: &str = "RHONOMETRE_IMAP_HOST";
const IMAP_PORT_ENV: &str = "RHONOMETRE_IMAP_PORT";
const IMAP_USERNAME_ENV: &str = "RHONOMETRE_IMAP_USERNAME";
const IMAP_PASSWORD_ENV: &str = "RHONOMETRE_IMAP_PASSWORD";
const IMAP_MAILBOX_ENV: &str = "RHONOMETRE_IMAP_MAILBOX";
const IMAP_POLL_SECONDS_ENV: &str = "RHONOMETRE_IMAP_POLL_SECONDS";
const HOST_ENV: &str = "HOST";
const DEFAULT_PRO_CODE: &str = "rhonometre";
const PRO_TOKEN_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

type HmacSha256 = Hmac<Sha256>;

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
        role_fr: "Rhône après la Jonction",
        role_en: "Post-Jonction Rhône",
        kind: WaterKind::River,
    },
];

const HALLE_ILE_STATION: StationConfig = StationConfig {
    id: "2606",
    slug: "rhone-halle-ile",
    name_fr: "Rhône - Genève, Halle de l'Île",
    name_en: "Rhône - Geneva, Halle de l'Île",
    role_fr: "Rhône avant la Jonction",
    role_en: "Rhône before Jonction",
    kind: WaterKind::River,
};

const DERIVED_HALLE_ILE_STATION: StationConfig = StationConfig {
    id: "2606",
    slug: "rhone-halle-ile",
    name_fr: "Rhône - Genève, Halle de l'Île",
    name_en: "Rhône - Geneva, Halle de l'Île",
    role_fr: "Rhône avant la Jonction",
    role_en: "Rhône before Jonction",
    kind: WaterKind::River,
};

#[derive(Clone)]
struct AppState {
    client: Client,
    cache: Arc<RwLock<Option<CachedDashboard>>>,
    db: Option<PgPool>,
    ingest_token: Option<String>,
    pro_code: String,
    token_secret: String,
    imap_status: Arc<RwLock<ImapStatus>>,
}

struct ImapConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    mailbox: String,
    poll_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImapStatus {
    configured: bool,
    running: bool,
    host: Option<String>,
    username: Option<String>,
    mailbox: Option<String>,
    last_poll_at: Option<String>,
    last_success_at: Option<String>,
    last_error: Option<String>,
    messages_ingested: u64,
}

impl Default for ImapStatus {
    fn default() -> Self {
        Self {
            configured: false,
            running: false,
            host: None,
            username: None,
            mailbox: None,
            last_poll_at: None,
            last_success_at: None,
            last_error: None,
            messages_ingested: 0,
        }
    }
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
    sources: Vec<SourceInfo>,
    air_temperature: Option<AirTemperatureData>,
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
    source: StationDataSource,
    notice_fr: Option<&'static str>,
    notice_en: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
struct AirTemperatureData {
    source: SourceInfo,
    current: Option<CurrentMetric>,
    history: MetricSeries,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StationDataSource {
    Hydrodaten,
    Derived,
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

#[derive(Debug, Deserialize)]
struct ProAuthRequest {
    code: String,
}

#[derive(Debug, Serialize)]
struct ProAuthResponse {
    token: String,
    expires_at: String,
}

#[derive(Debug, Serialize)]
struct IngestResponse {
    id: String,
    parsed_points: usize,
    duplicate: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SeriesQuery {
    from: Option<String>,
    to: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    forecast: bool,
}

#[derive(Debug, Serialize)]
struct StationSeriesResponse {
    station_id: String,
    series: Vec<MetricSeries>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProTokenPayload {
    sub: String,
    exp: i64,
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

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    current: Option<OpenMeteoCurrent>,
    hourly: OpenMeteoHourly,
    hourly_units: Option<HashMap<String, String>>,
    current_units: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoCurrent {
    time: String,
    temperature_2m: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoHourly {
    time: Vec<String>,
    temperature_2m: Vec<Option<f64>>,
}

#[derive(Debug, Error)]
enum FetchError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("database failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("station {0} not found in {1}")]
    StationNotFound(&'static str, &'static str),
    #[error("history for station {0} did not contain usable data")]
    EmptyHistory(&'static str),
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

    let db = init_database().await;
    let ingest_token = env::var(INGEST_TOKEN_ENV).ok();
    let pro_code = env::var(PRO_CODE_ENV).unwrap_or_else(|_| DEFAULT_PRO_CODE.to_string());
    let token_secret = env::var(TOKEN_SECRET_ENV)
        .or_else(|_| env::var(INGEST_TOKEN_ENV))
        .unwrap_or_else(|_| pro_code.clone());

    let state = AppState {
        client,
        cache: Arc::new(RwLock::new(None)),
        db,
        ingest_token,
        pro_code,
        token_secret,
        imap_status: Arc::new(RwLock::new(ImapStatus::default())),
    };

    let imap_config = match imap_config_from_env() {
        Ok(config) => config,
        Err(err) => {
            warn!(error = %err, "IMAP programme ingestion is disabled");
            None
        }
    };

    if let Some(db) = state.db.as_ref() {
        if let Err(err) = ingest_configured_programme_sources(db).await {
            warn!(error = %err, "failed to ingest configured SIG programme files");
        }
    }

    let static_dir = env::var("STATIC_DIR").unwrap_or_else(|_| "frontend/dist".to_string());
    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3000);
    let host = env::var(HOST_ENV).unwrap_or_else(|_| "127.0.0.1".to_string());
    let host = host
        .parse::<IpAddr>()
        .unwrap_or_else(|err| panic!("failed to parse HOST as an IP address: {err}"));
    let addr = SocketAddr::from((host, port));
    let index_file = format!("{static_dir}/index.html");
    let sw_file = format!("{static_dir}/public/sw.js");
    let manifest_file = format!("{static_dir}/public/manifest.webmanifest");
    let icon_file = format!("{static_dir}/public/icon.svg");
    let static_service = ServeDir::new(&static_dir);

    let app = Router::new()
        .route("/", get_service(ServeFile::new(index_file)))
        .route("/sw.js", get_service(ServeFile::new(sw_file)))
        .route(
            "/manifest.webmanifest",
            get_service(ServeFile::new(manifest_file)),
        )
        .route("/icon.svg", get_service(ServeFile::new(icon_file)))
        .route("/api/dashboard", get(dashboard_handler))
        .route("/api/v1/dashboard", get(dashboard_v1_handler))
        .route("/api/v1/stations/{id}/series", get(station_series_handler))
        .route("/api/v1/auth/pro", post(pro_auth_handler))
        .route("/api/admin/email-ingest", post(email_ingest_handler))
        .route("/api/admin/imap-status", get(imap_status_handler))
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .route("/healthz", get(healthz))
        .fallback_service(static_service)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));
    info!(%addr, %static_dir, "serving rhonometre");

    tokio::spawn(refresh_loop(state.clone()));
    if let Some(config) = imap_config {
        if state.db.is_some() {
            tokio::spawn(imap_ingest_loop(state.clone(), config));
        } else {
            warn!("IMAP programme ingestion requires DATABASE_URL and is disabled");
        }
    }

    axum::serve(listener, app).await.expect("server failed");
}

async fn init_database() -> Option<PgPool> {
    let database_url = match env::var(DATABASE_URL_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            info!("DATABASE_URL is not set; running without persistent Postgres storage");
            return None;
        }
    };

    let attempts = env::var(DATABASE_CONNECT_ATTEMPTS_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(30)
        .max(1);
    let retry_delay = Duration::from_secs(
        env::var(DATABASE_CONNECT_RETRY_SECONDS_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2)
            .max(1),
    );

    for attempt in 1..=attempts {
        match PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
        {
            Ok(pool) => match migrate_database(&pool).await {
                Ok(()) => return Some(pool),
                Err(err) if attempt == attempts => {
                    panic!("failed to migrate Postgres schema after {attempts} attempts: {err}");
                }
                Err(err) => {
                    warn!(
                        attempt,
                        attempts,
                        retry_seconds = retry_delay.as_secs(),
                        error = %err,
                        "Postgres migration failed; retrying"
                    );
                }
            },
            Err(err) if attempt == attempts => {
                panic!(
                    "failed to connect to Postgres DATABASE_URL after {attempts} attempts: {err}"
                );
            }
            Err(err) => {
                warn!(
                    attempt,
                    attempts,
                    retry_seconds = retry_delay.as_secs(),
                    error = %err,
                    "Postgres connection failed; retrying"
                );
            }
        }

        sleep(retry_delay).await;
    }

    unreachable!("database connection loop always returns or panics")
}

async fn migrate_database(db: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS station_series (
            station_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            series_role TEXT NOT NULL,
            source TEXT NOT NULL,
            timestamp TIMESTAMPTZ NOT NULL,
            value DOUBLE PRECISION NOT NULL,
            unit TEXT NOT NULL,
            label_fr TEXT,
            label_en TEXT,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            PRIMARY KEY (station_id, kind, series_role, source, timestamp)
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ingest_events (
            id TEXT PRIMARY KEY,
            received_at TIMESTAMPTZ NOT NULL,
            subject TEXT,
            attachment_names JSONB NOT NULL DEFAULT '[]'::jsonb,
            parsed_points INTEGER NOT NULL,
            warnings JSONB NOT NULL DEFAULT '[]'::jsonb,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS dashboard_warnings (
            id BIGSERIAL PRIMARY KEY,
            generated_at TIMESTAMPTZ NOT NULL,
            message TEXT NOT NULL
        )
        "#,
    )
    .execute(db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS station_series_lookup ON station_series (station_id, kind, series_role, timestamp)",
    )
    .execute(db)
    .await?;

    Ok(())
}

async fn refresh_loop(state: AppState) {
    loop {
        if let Err(err) = refresh_dashboard_cache(&state).await {
            warn!(error = %err, "scheduled dashboard refresh failed");
        }
        sleep(CACHE_TTL).await;
    }
}

fn imap_config_from_env() -> Result<Option<ImapConfig>, String> {
    let username = env::var(IMAP_USERNAME_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let password = env::var(IMAP_PASSWORD_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    match (username, password) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(format!("{IMAP_PASSWORD_ENV} is required")),
        (None, Some(_)) => Err(format!("{IMAP_USERNAME_ENV} is required")),
        (Some(username), Some(password)) => {
            let host =
                env::var(IMAP_HOST_ENV).unwrap_or_else(|_| "mail.infomaniak.com".to_string());
            let port = env::var(IMAP_PORT_ENV)
                .ok()
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(993);
            let mailbox = env::var(IMAP_MAILBOX_ENV).unwrap_or_else(|_| "INBOX".to_string());
            let poll_seconds = env::var(IMAP_POLL_SECONDS_ENV)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(300)
                .max(30);
            Ok(Some(ImapConfig {
                host,
                port,
                username,
                password,
                mailbox,
                poll_interval: Duration::from_secs(poll_seconds),
            }))
        }
    }
}

async fn imap_ingest_loop(state: AppState, config: ImapConfig) {
    {
        let mut status = state.imap_status.write().await;
        status.configured = true;
        status.running = true;
        status.host = Some(config.host.clone());
        status.username = Some(config.username.clone());
        status.mailbox = Some(config.mailbox.clone());
    }
    info!(
        host = %config.host,
        port = config.port,
        username = %config.username,
        mailbox = %config.mailbox,
        poll_seconds = config.poll_interval.as_secs(),
        "starting secure IMAP programme ingestion"
    );
    let mut poll_count = 0_u64;
    loop {
        let scan_all = poll_count % 12 == 0;
        state.imap_status.write().await.last_poll_at =
            Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
        let result = poll_imap_programmes(&state, &config, scan_all).await;
        match &result {
            Ok(processed) if *processed > 0 => {
                info!(processed, "ingested SIG programme email messages");
                *state.cache.write().await = None;
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "IMAP programme poll failed");
            }
        }
        {
            let mut status = state.imap_status.write().await;
            apply_imap_poll_result(
                &mut status,
                &result,
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            );
        }
        poll_count = poll_count.wrapping_add(1);
        sleep(config.poll_interval).await;
    }
}

fn apply_imap_poll_result(
    status: &mut ImapStatus,
    result: &Result<usize, String>,
    completed_at: String,
) {
    match result {
        Ok(processed) => {
            status.last_success_at = Some(completed_at);
            status.last_error = None;
            status.messages_ingested = status
                .messages_ingested
                .saturating_add(u64::try_from(*processed).unwrap_or_default());
        }
        Err(err) => status.last_error = Some(err.clone()),
    }
}

async fn poll_imap_programmes(
    state: &AppState,
    config: &ImapConfig,
    scan_all: bool,
) -> Result<usize, String> {
    let db = state
        .db
        .as_ref()
        .ok_or_else(|| "Postgres storage is not configured".to_string())?;
    let session = connect_tls(&config.host, config.port)
        .await
        .map_err(|err| format!("connect to {}:{}: {err}", config.host, config.port))?;
    let authenticated = session
        .login(&config.username, Password::new(&config.password))
        .await
        .map_err(|err| format!("authenticate {}: {err}", config.username))?;
    let mut inbox = authenticated
        .select(&config.mailbox)
        .await
        .map_err(|err| format!("select mailbox {}: {err}", config.mailbox))?;
    let search_key = if scan_all {
        SearchKey::And(vec![SearchKey::All, SearchKey::Undeleted])
    } else {
        SearchKey::And(vec![SearchKey::Unseen, SearchKey::Undeleted])
    };
    let message_ids = inbox
        .search(SearchQuery::new(search_key))
        .await
        .map_err(|err| format!("search unread messages: {err}"))?;
    let mut processed = 0usize;

    for sequence in message_ids
        .into_iter()
        .rev()
        .take(50)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let sequence_set = sequence.to_string();
        let fetched = inbox
            .fetch(&sequence_set, "BODY.PEEK[]")
            .await
            .map_err(|err| format!("fetch message {sequence}: {err}"))?;
        let Some(body) = fetched.first().and_then(|message| message.body.as_ref()) else {
            warn!(sequence, "IMAP message did not include an RFC822 body");
            continue;
        };

        match ingest_request_bytes(db, &format!("imap-message-{sequence}.eml"), body.clone()).await
        {
            Ok(response) => {
                inbox
                    .store(&sequence_set, StoreAction::Add, &[Flag::Seen])
                    .await
                    .map_err(|err| format!("mark message {sequence} as seen: {err}"))?;
                if !response.duplicate {
                    info!(
                        sequence,
                        id = %response.id,
                        parsed_points = response.parsed_points,
                        "ingested IMAP programme message"
                    );
                    processed += 1;
                }
            }
            Err(err) => {
                warn!(
                    sequence,
                    error = %err,
                    "unread IMAP message was not a usable SIG programme and remains unread"
                );
            }
        }
    }

    inbox
        .logout()
        .await
        .map_err(|err| format!("logout from IMAP: {err}"))?;
    Ok(processed)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn imap_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(expected_token) = state.ingest_token.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "RHONOMETRE_INGEST_TOKEN is not configured" })),
        )
            .into_response();
    };
    if bearer_token(&headers) != Some(expected_token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid ingest token" })),
        )
            .into_response();
    }
    (StatusCode::OK, Json(state.imap_status.read().await.clone())).into_response()
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "API route not found" })),
    )
}

async fn dashboard_handler(State(state): State<AppState>) -> impl IntoResponse {
    match dashboard_data(&state).await {
        Ok(mut data) => {
            strip_forecasts(&mut data);
            (StatusCode::OK, Json(data)).into_response()
        }
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

async fn dashboard_v1_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match dashboard_data(&state).await {
        Ok(mut data) => {
            if is_authorized_pro(&headers, &state) {
                retain_pro_forecasts(&mut data);
            } else {
                strip_forecasts(&mut data);
            }
            (StatusCode::OK, Json(data)).into_response()
        }
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

async fn station_series_handler(
    State(state): State<AppState>,
    AxumPath(station_id): AxumPath<String>,
    Query(query): Query<SeriesQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Postgres storage is not configured" })),
        )
            .into_response();
    };

    if query.forecast && !is_authorized_pro(&headers, &state) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "pro authorization required for forecast series" })),
        )
            .into_response();
    }

    let include_forecast = query.forecast;
    match load_station_series(db, &station_id, &query, include_forecast).await {
        Ok(series) => (
            StatusCode::OK,
            Json(StationSeriesResponse { station_id, series }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn pro_auth_handler(
    State(state): State<AppState>,
    Json(request): Json<ProAuthRequest>,
) -> impl IntoResponse {
    if request.code.trim() != state.pro_code {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid pro code" })),
        )
            .into_response();
    }

    let expires_at = Utc::now() + chrono::Duration::seconds(PRO_TOKEN_TTL_SECONDS);
    match sign_pro_token(&state, expires_at) {
        Ok(token) => (
            StatusCode::OK,
            Json(ProAuthResponse {
                token,
                expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response(),
    }
}

async fn email_ingest_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(expected_token) = state.ingest_token.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "RHONOMETRE_INGEST_TOKEN is not configured" })),
        )
            .into_response();
    };

    if bearer_token(&headers) != Some(expected_token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid ingest token" })),
        )
            .into_response();
    }

    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Postgres storage is not configured" })),
        )
            .into_response();
    };

    match ingest_request_body(db, &headers, body).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response(),
    }
}

async fn load_station_series(
    db: &PgPool,
    station_id: &str,
    query: &SeriesQuery,
    include_forecast: bool,
) -> Result<Vec<MetricSeries>, sqlx::Error> {
    let role = if include_forecast {
        "forecast"
    } else {
        "history"
    };
    let kind_filter = query.kind.as_deref();
    let from = query
        .from
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(HISTORY_DAYS));
    let to = query
        .to
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() + chrono::Duration::days(14));

    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
            f64,
        ),
    >(
        r#"
        SELECT kind, unit, source, label_fr, label_en, timestamp, value
        FROM station_series
        WHERE station_id = $1
          AND series_role = $2
          AND timestamp >= $3
          AND timestamp <= $4
          AND ($5::text IS NULL OR kind = $5)
          AND (
              $6::boolean = false
              OR source IN (
                  'sig_programme_hourly',
                  'sig_programme_daily',
                  'sig_programme'
              )
          )
        ORDER BY kind, source, timestamp
        "#,
    )
    .bind(station_id)
    .bind(role)
    .bind(from)
    .bind(to)
    .bind(kind_filter)
    .bind(include_forecast)
    .fetch_all(db)
    .await?;

    let mut grouped: BTreeMap<(String, String), MetricSeries> = BTreeMap::new();
    for (kind_key, unit, source, label_fr, label_en, timestamp, value) in rows {
        let Some(kind) = metric_kind_from_key(&kind_key) else {
            continue;
        };
        grouped
            .entry((kind_key, source))
            .or_insert_with(|| MetricSeries {
                kind,
                label_fr: Box::leak(
                    label_fr
                        .unwrap_or_else(|| metric_label_fr(&kind).to_string())
                        .into_boxed_str(),
                ),
                label_en: Box::leak(
                    label_en
                        .unwrap_or_else(|| metric_label_en(&kind).to_string())
                        .into_boxed_str(),
                ),
                unit: unit.clone(),
                points: Vec::new(),
                uncertainty: None,
            })
            .points
            .push(HistoryPoint {
                timestamp: timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
                value,
            });
    }

    Ok(grouped.into_values().collect())
}

fn strip_forecasts(data: &mut DashboardData) {
    for station in &mut data.stations {
        station.forecast.clear();
    }
}

fn retain_pro_forecasts(data: &mut DashboardData) {
    for station in &mut data.stations {
        if station.id == DERIVED_HALLE_ILE_STATION.id {
            station
                .forecast
                .retain(|series| series.kind == MetricKind::Discharge);
        } else {
            station.forecast.clear();
        }
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn sign_pro_token(state: &AppState, expires_at: DateTime<Utc>) -> Result<String, String> {
    let payload = ProTokenPayload {
        sub: "pro".to_string(),
        exp: expires_at.timestamp(),
    };
    let payload_json = serde_json::to_vec(&payload).map_err(|err| err.to_string())?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
    let signature = sign_bytes(&state.token_secret, payload_b64.as_bytes())?;
    Ok(format!(
        "{payload_b64}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

fn is_authorized_pro(headers: &HeaderMap, state: &AppState) -> bool {
    let Some(token) = bearer_token(headers) else {
        return false;
    };
    let Some((payload_b64, signature_b64)) = token.split_once('.') else {
        return false;
    };
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature_b64) else {
        return false;
    };
    let Ok(expected) = sign_bytes(&state.token_secret, payload_b64.as_bytes()) else {
        return false;
    };
    if signature != expected {
        return false;
    }
    let Ok(payload_json) = URL_SAFE_NO_PAD.decode(payload_b64) else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<ProTokenPayload>(&payload_json) else {
        return false;
    };
    payload.sub == "pro" && payload.exp > Utc::now().timestamp()
}

fn sign_bytes(secret: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|err| err.to_string())?;
    mac.update(bytes);
    Ok(mac.finalize().into_bytes().to_vec())
}

async fn dashboard_data(state: &AppState) -> Result<DashboardData, FetchError> {
    if let Some(cached) = fresh_cache(state).await {
        return Ok(cached);
    }

    match refresh_dashboard_cache(state).await {
        Ok(data) => Ok(data),
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

async fn refresh_dashboard_cache(state: &AppState) -> Result<DashboardData, FetchError> {
    let mut data = fetch_dashboard(&state.client, state.db.as_ref()).await?;
    data.cache_status = CacheStatus::Fresh;

    if let Some(db) = state.db.as_ref() {
        persist_dashboard(db, &data).await?;
    }

    let mut cache = state.cache.write().await;
    *cache = Some(CachedDashboard {
        fetched_at: Instant::now(),
        data: data.clone(),
    });

    Ok(data)
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

async fn fetch_dashboard(
    client: &Client,
    db: Option<&PgPool>,
) -> Result<DashboardData, FetchError> {
    let pq = fetch_features(client, HYDRO_PQ_URL).await?;
    let temperature = fetch_features(client, HYDRO_TEMPERATURE_URL).await?;
    let pq_by_station = features_by_station(pq);
    let temperature_by_station = features_by_station(temperature);

    let mut stations = Vec::with_capacity(SOURCE_STATIONS.len() + 1);
    let mut warnings = Vec::new();
    let programme_forecasts = match db {
        Some(db) => match load_programme_forecasts_from_db(db).await {
            Ok(forecasts) if forecasts.has_data() => forecasts,
            Ok(_) => load_programme_forecasts(&mut warnings),
            Err(err) => {
                warn!(error = %err, "failed to load SIG programme forecast from Postgres");
                warnings.push(format!(
                    "Could not load SIG discharge programme from Postgres: {err}"
                ));
                load_programme_forecasts(&mut warnings)
            }
        },
        None => load_programme_forecasts(&mut warnings),
    };
    for station in SOURCE_STATIONS {
        stations.push(
            fetch_hydrodaten_station_data(
                client,
                station,
                &pq_by_station,
                &temperature_by_station,
                &mut warnings,
            )
            .await?,
        );
    }

    let measured_halle_ile = match fetch_hydrodaten_station_data(
        client,
        &HALLE_ILE_STATION,
        &pq_by_station,
        &temperature_by_station,
        &mut warnings,
    )
    .await
    {
        Ok(station) if has_complete_temperature_and_discharge(&station) => Some(station),
        Ok(station) => {
            warn!(
                station = station.id,
                "Hydrodaten 2606 is present but incomplete; using derived fallback"
            );
            None
        }
        Err(err) => {
            warn!(error = %err, "Hydrodaten 2606 is unavailable; using derived fallback");
            None
        }
    };

    if let Some(mut station) = measured_halle_ile {
        apply_seujet_programme_forecast(&mut station, programme_forecasts.seujet.as_ref());
        stations.insert(1, station);
    } else {
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
                    "Could not calibrate the Rhône - Genève, Halle de l'Île temperature estimate: {err}"
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
                "Could not derive Rhône - Genève, Halle de l'Île from Arve and Chancy data"
                    .to_string(),
            ),
        }
    }

    let air_temperature = match fetch_air_temperature(client).await {
        Ok(air_temperature) => Some(air_temperature),
        Err(err) => {
            warn!(error = %err, "failed to fetch Open-Meteo air temperature");
            warnings.push(format!("Could not refresh Geneva air temperature: {err}"));
            None
        }
    };

    let mut sources = vec![SourceInfo {
        label: "Swiss Hydrodaten".to_string(),
        url: "https://www.hydrodaten.admin.ch/de/seen-und-fluesse/messstationen-zustand"
            .to_string(),
    }];
    if let Some(air_temperature) = air_temperature.as_ref() {
        sources.push(air_temperature.source.clone());
    }
    let source_label = sources
        .iter()
        .map(|source| source.label.as_str())
        .collect::<Vec<_>>()
        .join(" + ");

    Ok(DashboardData {
        generated_at: chrono::Utc::now().to_rfc3339(),
        cache_status: CacheStatus::Fresh,
        source: SourceInfo {
            label: source_label,
            url: "https://www.hydrodaten.admin.ch/de/seen-und-fluesse/messstationen-zustand"
                .to_string(),
        },
        sources,
        air_temperature,
        stations,
        warnings,
    })
}

async fn fetch_hydrodaten_station_data(
    client: &Client,
    station: &StationConfig,
    pq_by_station: &HashMap<String, Feature>,
    temperature_by_station: &HashMap<String, Feature>,
    warnings: &mut Vec<String>,
) -> Result<StationData, FetchError> {
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

    let status = match (current.is_empty(), history.is_empty()) {
        (false, false) => StationStatus::Complete,
        (false, true) | (true, false) => StationStatus::Partial,
        (true, true) => StationStatus::Missing,
    };

    Ok(StationData {
        id: station.id,
        slug: station.slug,
        name_fr: station.name_fr,
        name_en: station.name_en,
        role_fr: station.role_fr,
        role_en: station.role_en,
        kind: station.kind,
        current,
        history,
        forecast: Vec::new(),
        status,
        source: StationDataSource::Hydrodaten,
        notice_fr: None,
        notice_en: None,
    })
}

fn has_complete_temperature_and_discharge(station: &StationData) -> bool {
    current_metric(station, MetricKind::Discharge).is_some()
        && current_metric(station, MetricKind::Temperature).is_some()
        && history_series(station, MetricKind::Discharge).is_some()
        && history_series(station, MetricKind::Temperature).is_some()
}

fn apply_seujet_programme_forecast(
    station: &mut StationData,
    seujet_programme_forecast: Option<&MetricSeries>,
) {
    station
        .forecast
        .retain(|forecast| forecast.kind != MetricKind::Discharge);
    if let Some(series) = seujet_programme_forecast {
        station.forecast.push(series.clone());
    }
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
    let mut derived_temperature_history = temperature_calibration
        .and_then(|calibration| derive_temperature_history(arve, chancy, calibration));
    if let Some(metric) = derive_current_temperature(
        arve,
        chancy,
        derived_temperature_history.as_ref(),
        temperature_calibration,
    ) {
        if let Some(series) = derived_temperature_history.as_mut() {
            append_current_point_if_newer(series, &metric);
        }
        current.push(metric);
    }
    if let Some(series) = derived_temperature_history {
        history.push(series);
    }

    let mut forecast = Vec::new();
    if let Some(series) = seujet_programme_forecast {
        forecast.push(series.clone());
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
        source: StationDataSource::Derived,
        notice_fr: Some(
            "Estimation par bilan de chaleur calibré sur l'historique Arve/Chancy/2606: la station 2606 est hors ligne.",
        ),
        notice_en: Some(
            "Estimated from a heat balance calibrated on Arve/Chancy/2606 history while station 2606 is offline.",
        ),
    })
}

async fn ingest_request_body(
    db: &PgPool,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<IngestResponse, String> {
    let blobs = if let Some(boundary) = multipart_boundary(headers) {
        multipart_blobs(boundary, body.clone()).await?
    } else {
        vec![("request-body".to_string(), body.to_vec())]
    };

    let request_hash = content_hash(&body);
    if let Some(response) = existing_ingest_response(db, &request_hash).await? {
        return Ok(response);
    }
    let mut total_points = 0usize;
    let mut all_warnings = Vec::new();
    let mut all_attachments = Vec::new();
    let mut subject = None;
    let mut all_points = BTreeMap::new();
    let mut hourly_dates = HashSet::new();

    for (name, bytes) in blobs {
        let metadata = programme_payload_metadata(&name, &bytes);
        if subject.is_none() {
            subject = metadata.subject;
        }
        all_attachments.extend(metadata.attachments);

        match parse_programme_source_bytes_detailed(
            &name,
            &bytes,
            &mut all_points,
            &mut hourly_dates,
        ) {
            Ok(points) => total_points += points,
            Err(err) => all_warnings.push(format!("{name}: {err}")),
        }
    }

    if all_points.is_empty() {
        return Err(format!(
            "programme payload did not contain usable Q Seujet points: {}",
            all_warnings.join("; ")
        ));
    }

    upsert_sig_programme_points(db, &all_points, &hourly_dates).await?;
    let duplicate = store_ingest_event(
        db,
        &request_hash,
        subject.as_deref(),
        &all_attachments,
        total_points,
        &all_warnings,
    )
    .await?;

    Ok(IngestResponse {
        id: request_hash,
        parsed_points: total_points,
        duplicate,
        warnings: all_warnings,
    })
}

async fn multipart_blobs(boundary: String, body: Bytes) -> Result<Vec<(String, Vec<u8>)>, String> {
    let stream = stream::once(async move { Ok::<Bytes, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut blobs = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| format!("invalid multipart payload: {err}"))?
    {
        let name = field
            .file_name()
            .map(ToOwned::to_owned)
            .or_else(|| field.name().map(ToOwned::to_owned))
            .unwrap_or_else(|| "upload".to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|err| format!("invalid multipart field {name}: {err}"))?;
        blobs.push((name, bytes.to_vec()));
    }

    Ok(blobs)
}

fn multipart_boundary(headers: &HeaderMap) -> Option<String> {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    content_type
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("boundary="))
        .map(|boundary| boundary.trim_matches('"').to_string())
}

fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

struct ProgrammePayloadMetadata {
    subject: Option<String>,
    attachments: Vec<String>,
}

fn programme_payload_metadata(name: &str, bytes: &[u8]) -> ProgrammePayloadMetadata {
    let Ok(mail) = parse_mail(bytes) else {
        return ProgrammePayloadMetadata {
            subject: None,
            attachments: vec![name.to_string()],
        };
    };

    let subject = mail.headers.get_first_value("Subject");
    let attachments = mail
        .parts()
        .filter_map(|part| {
            let disposition = part.get_content_disposition();
            disposition
                .params
                .get("filename")
                .cloned()
                .or_else(|| part.ctype.params.get("name").cloned())
        })
        .collect::<Vec<_>>();

    ProgrammePayloadMetadata {
        subject,
        attachments,
    }
}

async fn upsert_sig_programme_points(
    db: &PgPool,
    points: &BTreeMap<DateTime<FixedOffset>, f64>,
    hourly_dates: &HashSet<NaiveDate>,
) -> Result<(), String> {
    for (timestamp, value) in points {
        let source = sig_programme_source(timestamp.date_naive(), hourly_dates);
        upsert_station_point(
            db,
            DERIVED_HALLE_ILE_STATION.id,
            MetricKind::Discharge,
            "forecast",
            source,
            timestamp.with_timezone(&Utc),
            *value,
            "m³/s",
            "Programme SIG Seujet",
            "SIG Seujet programme",
        )
        .await
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

async fn store_ingest_event(
    db: &PgPool,
    id: &str,
    subject: Option<&str>,
    attachment_names: &[String],
    parsed_points: usize,
    warnings: &[String],
) -> Result<bool, String> {
    let result = sqlx::query(
        r#"
        INSERT INTO ingest_events
            (id, received_at, subject, attachment_names, parsed_points, warnings)
        VALUES ($1, now(), $2, $3, $4, $5)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(id)
    .bind(subject)
    .bind(serde_json::to_value(attachment_names).map_err(|err| err.to_string())?)
    .bind(parsed_points as i32)
    .bind(serde_json::to_value(warnings).map_err(|err| err.to_string())?)
    .execute(db)
    .await
    .map_err(|err| err.to_string())?;

    Ok(result.rows_affected() == 0)
}

async fn ingest_configured_programme_sources(db: &PgPool) -> Result<(), String> {
    let (roots, _) = programme_source_roots();
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for root in &roots {
        collect_programme_files(root, &mut files, &mut warnings);
    }

    for warning in warnings {
        warn!(%warning, "configured programme source warning");
    }

    for path in files {
        let bytes = fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let response = ingest_request_bytes(db, &path.display().to_string(), bytes).await?;
        info!(
            id = %response.id,
            parsed_points = response.parsed_points,
            duplicate = response.duplicate,
            "ingested configured SIG programme source"
        );
    }

    Ok(())
}

async fn ingest_request_bytes(
    db: &PgPool,
    name: &str,
    bytes: Vec<u8>,
) -> Result<IngestResponse, String> {
    let request_hash = content_hash(&bytes);
    if let Some(response) = existing_ingest_response(db, &request_hash).await? {
        return Ok(response);
    }
    let metadata = programme_payload_metadata(name, &bytes);
    let mut points = BTreeMap::new();
    let mut hourly_dates = HashSet::new();
    let mut warnings = Vec::new();
    let parsed_points =
        match parse_programme_source_bytes_detailed(name, &bytes, &mut points, &mut hourly_dates) {
            Ok(points) => points,
            Err(err) => {
                warnings.push(err);
                0
            }
        };

    if points.is_empty() {
        return Err(format!("{name} did not contain usable Q Seujet points"));
    }

    upsert_sig_programme_points(db, &points, &hourly_dates).await?;
    let duplicate = store_ingest_event(
        db,
        &request_hash,
        metadata.subject.as_deref(),
        &metadata.attachments,
        parsed_points,
        &warnings,
    )
    .await?;

    Ok(IngestResponse {
        id: request_hash,
        parsed_points,
        duplicate,
        warnings,
    })
}

async fn existing_ingest_response(db: &PgPool, id: &str) -> Result<Option<IngestResponse>, String> {
    let existing = sqlx::query_as::<_, (i32, Value)>(
        "SELECT parsed_points, warnings FROM ingest_events WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|err| err.to_string())?;
    let Some((parsed_points, warnings)) = existing else {
        return Ok(None);
    };
    Ok(Some(IngestResponse {
        id: id.to_string(),
        parsed_points: usize::try_from(parsed_points).unwrap_or_default(),
        duplicate: true,
        warnings: serde_json::from_value(warnings).unwrap_or_default(),
    }))
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
    let today = geneva_today();
    seujet_points.retain(|timestamp, _| is_visible_forecast_date(today, timestamp.date_naive()));

    if seujet_points.is_empty() {
        return ProgrammeForecasts::default();
    }

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

async fn load_programme_forecasts_from_db(db: &PgPool) -> Result<ProgrammeForecasts, sqlx::Error> {
    let rows = sqlx::query_as::<_, (DateTime<Utc>, f64)>(
        r#"
        SELECT timestamp, value
        FROM (
            SELECT DISTINCT ON (timestamp)
                timestamp,
                value
            FROM station_series
            WHERE station_id = '2606'
              AND kind = 'discharge'
              AND series_role = 'forecast'
              AND source IN (
                  'sig_programme_hourly',
                  'sig_programme_daily',
                  'sig_programme'
              )
              AND timestamp >= (
                  date_trunc('day', now() AT TIME ZONE 'Europe/Zurich')
                  - interval '1 day'
              ) AT TIME ZONE 'Europe/Zurich'
              AND timestamp < (
                  date_trunc('day', now() AT TIME ZONE 'Europe/Zurich')
                  + interval '4 days'
              ) AT TIME ZONE 'Europe/Zurich'
            ORDER BY
                timestamp,
                CASE source
                    WHEN 'sig_programme_hourly' THEN 0
                    WHEN 'sig_programme_daily' THEN 1
                    ELSE 2
                END,
                updated_at DESC
        ) preferred
        ORDER BY timestamp
        "#,
    )
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        return Ok(ProgrammeForecasts::default());
    }

    Ok(ProgrammeForecasts {
        seujet: Some(MetricSeries {
            kind: MetricKind::Discharge,
            label_fr: "Programme SIG Seujet",
            label_en: "SIG Seujet programme",
            unit: "m³/s".to_string(),
            points: rows
                .into_iter()
                .map(|(timestamp, value)| HistoryPoint {
                    timestamp: timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
                    value,
                })
                .collect(),
            uncertainty: None,
        }),
    })
}

async fn persist_dashboard(db: &PgPool, data: &DashboardData) -> Result<(), sqlx::Error> {
    let generated_at = DateTime::parse_from_rfc3339(&data.generated_at)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    for station in &data.stations {
        for metric in &station.current {
            if let Ok(timestamp) = DateTime::parse_from_rfc3339(&metric.measured_at) {
                upsert_station_point(
                    db,
                    station.id,
                    metric.kind,
                    "current",
                    "hydrodaten",
                    timestamp.with_timezone(&Utc),
                    metric.value,
                    &metric.unit,
                    metric.label_fr,
                    metric.label_en,
                )
                .await?;
            }
        }

        for series in &station.history {
            let source = station_data_source_key(station.source);
            persist_metric_series(db, station.id, "history", source, series).await?;
        }
    }

    if !data.warnings.is_empty() {
        for warning in &data.warnings {
            sqlx::query("INSERT INTO dashboard_warnings (generated_at, message) VALUES ($1, $2)")
                .bind(generated_at)
                .bind(warning)
                .execute(db)
                .await?;
        }
    }

    Ok(())
}

fn station_data_source_key(source: StationDataSource) -> &'static str {
    match source {
        StationDataSource::Hydrodaten => "hydrodaten",
        StationDataSource::Derived => "derived",
    }
}

async fn persist_metric_series(
    db: &PgPool,
    station_id: &str,
    series_role: &str,
    source: &str,
    series: &MetricSeries,
) -> Result<(), sqlx::Error> {
    for point in &series.points {
        let Ok(timestamp) = DateTime::parse_from_rfc3339(&point.timestamp) else {
            continue;
        };
        upsert_station_point(
            db,
            station_id,
            series.kind,
            series_role,
            source,
            timestamp.with_timezone(&Utc),
            point.value,
            &series.unit,
            &series.label_fr,
            &series.label_en,
        )
        .await?;
    }
    Ok(())
}

async fn upsert_station_point(
    db: &PgPool,
    station_id: &str,
    kind: MetricKind,
    series_role: &str,
    source: &str,
    timestamp: DateTime<Utc>,
    value: f64,
    unit: &str,
    label_fr: &str,
    label_en: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO station_series
            (station_id, kind, series_role, source, timestamp, value, unit, label_fr, label_en, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
        ON CONFLICT (station_id, kind, series_role, source, timestamp)
        DO UPDATE SET
            value = EXCLUDED.value,
            unit = EXCLUDED.unit,
            label_fr = EXCLUDED.label_fr,
            label_en = EXCLUDED.label_en,
            updated_at = now()
        "#,
    )
    .bind(station_id)
    .bind(metric_kind_key(kind))
    .bind(series_role)
    .bind(source)
    .bind(timestamp)
    .bind(value)
    .bind(unit)
    .bind(label_fr)
    .bind(label_en)
    .execute(db)
    .await?;

    Ok(())
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

fn is_likely_programme_source(path: &FsPath) -> bool {
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

fn collect_programme_files(path: &FsPath, files: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
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

fn should_attempt_programme_file(path: &FsPath) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("xls" | "xlsx" | "xlsm" | "xlsb" | "eml" | "txt" | "rtfd")
    )
}

fn parse_programme_source_path(
    path: &FsPath,
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> Result<usize, String> {
    let bytes = fs::read(path).map_err(|err| format!("failed to read programme source: {err}"))?;

    parse_programme_source_bytes(&path.display().to_string(), &bytes, seujet_points)
}

fn parse_programme_source_bytes(
    name: &str,
    bytes: &[u8],
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
) -> Result<usize, String> {
    parse_programme_source_bytes_detailed(name, bytes, seujet_points, &mut HashSet::new())
}

fn parse_programme_source_bytes_detailed(
    name: &str,
    bytes: &[u8],
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
    hourly_dates: &mut HashSet<NaiveDate>,
) -> Result<usize, String> {
    if is_workbook_name(name) {
        return parse_programme_workbook_bytes(
            name,
            bytes,
            date_from_text(name),
            seujet_points,
            hourly_dates,
        );
    }

    parse_programme_mail_bytes(name, bytes, seujet_points, hourly_dates).or_else(|mail_err| {
        parse_programme_workbook_bytes(
            name,
            bytes,
            date_from_text(name),
            seujet_points,
            hourly_dates,
        )
        .map_err(|workbook_err| {
            format!("not a readable programme email ({mail_err}) or workbook ({workbook_err})")
        })
    })
}

fn is_workbook_name(name: &str) -> bool {
    matches!(
        FsPath::new(name)
            .extension()
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
    hourly_dates: &mut HashSet<NaiveDate>,
) -> Result<usize, String> {
    let mail = parse_mail(bytes).map_err(|err| err.to_string())?;
    let message_date = mail
        .headers
        .get_first_value("Subject")
        .as_deref()
        .and_then(date_from_text);
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
        let attachment_date = date_from_text(&attachment_name).or(message_date);
        match parse_programme_workbook_bytes(
            &attachment_name,
            &attachment,
            attachment_date,
            seujet_points,
            hourly_dates,
        ) {
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
                FsPath::new(name)
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
    fallback_date: Option<NaiveDate>,
    seujet_points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
    hourly_dates: &mut HashSet<NaiveDate>,
) -> Result<usize, String> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook =
        open_workbook_auto_from_rs(cursor).map_err(|err| format!("open workbook: {err}"))?;
    let mut point_count = 0usize;

    for sheet_name in workbook.sheet_names().to_owned() {
        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(|err| format!("read worksheet {sheet_name}: {err}"))?;
        point_count += collect_programme_points(&range, fallback_date, seujet_points, hourly_dates);
    }

    if point_count == 0 {
        return Err(format!(
            "{name} did not contain Q Seujet or Seujet daily-average programme points"
        ));
    }

    Ok(point_count)
}

fn collect_programme_points(
    range: &Range<Data>,
    fallback_date: Option<NaiveDate>,
    points: &mut BTreeMap<DateTime<FixedOffset>, f64>,
    hourly_dates: &mut HashSet<NaiveDate>,
) -> usize {
    let rows = range.rows().collect::<Vec<_>>();
    let workbook_date = fallback_date.or_else(|| {
        rows.iter()
            .take(30)
            .flat_map(|row| row.iter())
            .find_map(cell_date)
    });
    let mut point_count = 0usize;
    let mut daily_index = 0_i64;

    for row in &rows {
        if !row_contains_label(row, "Seujet débit moyen journalier") {
            continue;
        }

        daily_index += 1;
        let explicit_date = row.iter().find_map(cell_date);
        let date = explicit_date
            .or_else(|| workbook_date?.checked_add_signed(chrono::Duration::days(daily_index)));
        let Some(date) = date else {
            continue;
        };
        let Some(label_index) = row.iter().position(|cell| {
            cell_text(cell)
                .as_deref()
                .map(str::trim)
                .is_some_and(|text| text.eq_ignore_ascii_case("Seujet débit moyen journalier"))
        }) else {
            continue;
        };
        let Some(value) = row
            .iter()
            .skip(label_index + 1)
            .filter_map(cell_number)
            .find(|value| (0.0..=5_000.0).contains(value))
        else {
            continue;
        };

        for hour in 0..24 {
            if let Some(timestamp) = geneva_datetime(date, hour) {
                points.entry(timestamp).or_insert(value);
                point_count += 1;
            }
        }
    }

    for (row_idx, row) in rows.iter().enumerate() {
        if !row_contains_label(row, "Q Seujet") {
            continue;
        }

        let Some(date) = row
            .iter()
            .find_map(cell_date)
            .or_else(|| find_nearby_date(&rows, row_idx))
            .or(workbook_date)
        else {
            continue;
        };
        let Some(headers) = find_time_headers(&rows, row_idx) else {
            continue;
        };
        let Some(midnight_idx) = headers.iter().position(|(_, hour)| *hour == 0) else {
            continue;
        };
        hourly_dates.insert(date);

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

fn find_nearby_date(rows: &[&[Data]], row_idx: usize) -> Option<NaiveDate> {
    for distance in 1..=20 {
        if let Some(date) = row_idx
            .checked_sub(distance)
            .and_then(|index| rows.get(index))
            .and_then(|row| row.iter().find_map(cell_date))
        {
            return Some(date);
        }
        if let Some(date) = rows
            .get(row_idx + distance)
            .and_then(|row| row.iter().find_map(cell_date))
        {
            return Some(date);
        }
    }
    None
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
        Data::Float(value) if *value >= 20_000.0 => excel_datetime_date(ExcelDateTime::new(
            *value,
            ExcelDateTimeType::DateTime,
            false,
        )),
        Data::Int(value) if *value >= 20_000 => excel_datetime_date(ExcelDateTime::new(
            *value as f64,
            ExcelDateTimeType::DateTime,
            false,
        )),
        Data::String(value) => date_from_text(value),
        _ => None,
    }
}

fn date_from_text(value: &str) -> Option<NaiveDate> {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_digit() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let parts = normalized
        .split_whitespace()
        .filter_map(|part| part.parse::<i32>().ok())
        .collect::<Vec<_>>();

    for window in parts.windows(3) {
        let (first, second, third) = (window[0], window[1], window[2]);
        if first >= 1_900 {
            if let Some(date) = NaiveDate::from_ymd_opt(
                first,
                u32::try_from(second).ok()?,
                u32::try_from(third).ok()?,
            ) {
                return Some(date);
            }
        } else if third >= 1_900 {
            if let Some(date) = NaiveDate::from_ymd_opt(
                third,
                u32::try_from(second).ok()?,
                u32::try_from(first).ok()?,
            ) {
                return Some(date);
            }
        }
    }
    None
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

fn geneva_today() -> NaiveDate {
    let utc_now = Utc::now();
    let first_pass = utc_now.with_timezone(&geneva_offset(utc_now.date_naive()));
    first_pass
        .with_timezone(&geneva_offset(first_pass.date_naive()))
        .date_naive()
}

fn is_visible_forecast_date(today: NaiveDate, date: NaiveDate) -> bool {
    date >= today - chrono::Duration::days(1) && date <= today + chrono::Duration::days(3)
}

fn sig_programme_source(date: NaiveDate, hourly_dates: &HashSet<NaiveDate>) -> &'static str {
    if hourly_dates.contains(&date) {
        "sig_programme_hourly"
    } else {
        "sig_programme_daily"
    }
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
    if let Some(metric) = calibration.and_then(|calibration| {
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
    }) {
        return Some(metric);
    }

    derived_history
        .and_then(|series| series.points.last())
        .map(|point| CurrentMetric {
            kind: MetricKind::Temperature,
            label_fr: "Température calculée",
            label_en: "Derived temperature",
            value: point.value,
            unit: "°C".to_string(),
            measured_at: point.timestamp.clone(),
            range_24h: None,
        })
}

fn append_current_point_if_newer(series: &mut MetricSeries, metric: &CurrentMetric) {
    if series.kind != metric.kind {
        return;
    }

    let Some(metric_timestamp) = parse_timestamp(&metric.measured_at) else {
        return;
    };
    let should_append = series
        .points
        .last()
        .and_then(|point| parse_timestamp(&point.timestamp))
        .map(|last_timestamp| metric_timestamp > last_timestamp)
        .unwrap_or(true);

    if should_append {
        series.points.push(HistoryPoint {
            timestamp: metric.measured_at.clone(),
            value: metric.value,
        });
    }
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

async fn fetch_air_temperature(client: &Client) -> Result<AirTemperatureData, FetchError> {
    let response = client
        .get(OPEN_METEO_URL)
        .query(&[
            ("latitude", "46.2044"),
            ("longitude", "6.1432"),
            ("current", "temperature_2m"),
            ("hourly", "temperature_2m"),
            ("past_days", "5"),
            ("forecast_days", "1"),
            ("timezone", "Europe/Zurich"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<OpenMeteoResponse>()
        .await?;

    let unit = response
        .hourly_units
        .as_ref()
        .and_then(|units| units.get("temperature_2m"))
        .or_else(|| {
            response
                .current_units
                .as_ref()
                .and_then(|units| units.get("temperature_2m"))
        })
        .cloned()
        .unwrap_or_else(|| "°C".to_string());
    let points = response
        .hourly
        .time
        .into_iter()
        .zip(response.hourly.temperature_2m)
        .filter_map(|(timestamp, value)| {
            Some(HistoryPoint {
                timestamp: open_meteo_timestamp(&timestamp)?,
                value: value?,
            })
        })
        .collect::<Vec<_>>();

    if points.is_empty() {
        return Err(FetchError::EmptyHistory("open_meteo_air_temperature"));
    }

    let current = response.current.and_then(|current| {
        Some(CurrentMetric {
            kind: MetricKind::Temperature,
            label_fr: "Température de l'air",
            label_en: "Air temperature",
            value: current.temperature_2m?,
            unit: unit.clone(),
            measured_at: open_meteo_timestamp(&current.time).unwrap_or(current.time),
            range_24h: None,
        })
    });

    Ok(AirTemperatureData {
        source: SourceInfo {
            label: "Open-Meteo".to_string(),
            url: "https://open-meteo.com/".to_string(),
        },
        current,
        history: MetricSeries {
            kind: MetricKind::Temperature,
            label_fr: "Température de l'air",
            label_en: "Air temperature",
            unit,
            points,
            uncertainty: None,
        },
    })
}

fn open_meteo_timestamp(value: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").ok()?;
    let offset = geneva_offset(naive.date());
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Secs, true))
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

fn metric_kind_key(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Discharge => "discharge",
        MetricKind::WaterLevel => "water_level",
        MetricKind::Temperature => "temperature",
    }
}

fn metric_kind_from_key(key: &str) -> Option<MetricKind> {
    match key {
        "discharge" => Some(MetricKind::Discharge),
        "water_level" => Some(MetricKind::WaterLevel),
        "temperature" => Some(MetricKind::Temperature),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use std::io::{Cursor as IoCursor, Write};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    #[test]
    fn parses_sig_programme_workbook_q_seujet_points() {
        let workbook = synthetic_programme_workbook();
        let mut points = BTreeMap::new();

        let count = parse_programme_source_bytes("programme.xlsx", &workbook, &mut points)
            .expect("synthetic workbook should parse");

        assert_eq!(count, 24);
        assert_eq!(points.len(), 24);
        let offset = FixedOffset::east_opt(2 * 60 * 60).expect("valid CEST offset");
        let first = offset
            .with_ymd_and_hms(2026, 6, 20, 0, 0, 0)
            .single()
            .expect("valid timestamp");
        let last = offset
            .with_ymd_and_hms(2026, 6, 20, 23, 0, 0)
            .single()
            .expect("valid timestamp");
        assert_eq!(points.get(&first), Some(&100.0));
        assert_eq!(points.get(&last), Some(&123.0));
    }

    #[test]
    fn parses_sig_programme_from_raw_mime_attachment() {
        let workbook = synthetic_programme_workbook();
        let mail = synthetic_programme_mail(&workbook);
        let mut points = BTreeMap::new();

        let count = parse_programme_source_bytes("programme.eml", mail.as_bytes(), &mut points)
            .expect("synthetic MIME email should parse");

        assert_eq!(count, 24);
        assert_eq!(points.len(), 24);
    }

    #[test]
    fn duplicate_programme_points_collapse_by_timestamp() {
        let workbook = synthetic_programme_workbook();
        let mut points = BTreeMap::new();

        parse_programme_source_bytes("programme.xlsx", &workbook, &mut points)
            .expect("first parse should succeed");
        parse_programme_source_bytes("programme.xlsx", &workbook, &mut points)
            .expect("second parse should succeed");

        assert_eq!(points.len(), 24);
    }

    #[test]
    fn parses_title_date_and_daily_average_fallbacks() {
        let workbook = synthetic_programme_workbook_with_sheet(&programme_sheet_xml(
            "20.06.2026",
            100,
            &[230, 240],
        ));
        let mut points = BTreeMap::new();

        let count = parse_programme_source_bytes("programme.xlsx", &workbook, &mut points)
            .expect("programme with title date and daily averages should parse");

        assert_eq!(count, 72);
        assert_eq!(points.len(), 72);
        let offset = FixedOffset::east_opt(2 * 60 * 60).expect("valid CEST offset");
        let hourly = offset
            .with_ymd_and_hms(2026, 6, 20, 12, 0, 0)
            .single()
            .expect("valid hourly timestamp");
        let first_average = offset
            .with_ymd_and_hms(2026, 6, 21, 12, 0, 0)
            .single()
            .expect("valid first average timestamp");
        let second_average = offset
            .with_ymd_and_hms(2026, 6, 22, 12, 0, 0)
            .single()
            .expect("valid second average timestamp");
        assert_eq!(points.get(&hourly), Some(&112.0));
        assert_eq!(points.get(&first_average), Some(&230.0));
        assert_eq!(points.get(&second_average), Some(&240.0));
    }

    #[test]
    fn combines_three_friday_programmes_by_forecast_date() {
        let mut points = BTreeMap::new();
        for (date, base) in [
            ("17.07.2026", 100),
            ("18.07.2026", 200),
            ("19.07.2026", 300),
        ] {
            let workbook =
                synthetic_programme_workbook_with_sheet(&programme_sheet_xml(date, base, &[]));
            parse_programme_source_bytes(&format!("Programme {date}.xlsx"), &workbook, &mut points)
                .expect("Friday programme should parse");
        }

        assert_eq!(points.len(), 72);
        let offset = FixedOffset::east_opt(2 * 60 * 60).expect("valid CEST offset");
        let saturday = offset
            .with_ymd_and_hms(2026, 7, 18, 12, 0, 0)
            .single()
            .expect("valid Saturday timestamp");
        let sunday = offset
            .with_ymd_and_hms(2026, 7, 19, 12, 0, 0)
            .single()
            .expect("valid Sunday timestamp");
        assert_eq!(points.get(&saturday), Some(&212.0));
        assert_eq!(points.get(&sunday), Some(&312.0));
    }

    #[test]
    fn detailed_file_has_database_priority_over_an_earlier_daily_fallback() {
        let friday = synthetic_programme_workbook_with_sheet(&programme_sheet_xml(
            "17.07.2026",
            100,
            &[230],
        ));
        let saturday =
            synthetic_programme_workbook_with_sheet(&programme_sheet_xml("18.07.2026", 200, &[]));
        let mut points = BTreeMap::new();
        let mut hourly_dates = HashSet::new();

        parse_programme_source_bytes_detailed(
            "17.07.2026.xls",
            &friday,
            &mut points,
            &mut hourly_dates,
        )
        .expect("Friday workbook should parse");
        parse_programme_source_bytes_detailed(
            "18.07.2026.xls",
            &saturday,
            &mut points,
            &mut hourly_dates,
        )
        .expect("Saturday workbook should parse");

        let saturday_date = NaiveDate::from_ymd_opt(2026, 7, 18).expect("valid date");
        assert_eq!(
            sig_programme_source(saturday_date, &hourly_dates),
            "sig_programme_hourly"
        );
        let timestamp = geneva_datetime(saturday_date, 12).expect("valid timestamp");
        assert_eq!(points.get(&timestamp), Some(&212.0));
    }

    #[test]
    fn pro_forecast_dates_are_yesterday_through_day_three() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 16).expect("valid date");
        assert!(!is_visible_forecast_date(
            today,
            NaiveDate::from_ymd_opt(2026, 7, 14).expect("valid date")
        ));
        assert!(is_visible_forecast_date(
            today,
            NaiveDate::from_ymd_opt(2026, 7, 15).expect("valid date")
        ));
        assert!(is_visible_forecast_date(today, today));
        assert!(is_visible_forecast_date(
            today,
            NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid date")
        ));
        assert!(is_visible_forecast_date(
            today,
            NaiveDate::from_ymd_opt(2026, 7, 19).expect("valid date")
        ));
        assert!(!is_visible_forecast_date(
            today,
            NaiveDate::from_ymd_opt(2026, 7, 20).expect("valid date")
        ));
    }

    #[test]
    #[ignore = "set RHONOMETRE_TEST_PROGRAMME_FILES to inspect real SIG workbooks"]
    fn inspects_real_programme_files_from_env() {
        let paths = env::var("RHONOMETRE_TEST_PROGRAMME_FILES")
            .expect("RHONOMETRE_TEST_PROGRAMME_FILES must contain semicolon-separated paths");
        let mut combined = BTreeMap::new();

        for path in paths.split(';').filter(|path| !path.trim().is_empty()) {
            let bytes = fs::read(path).expect("real programme workbook should be readable");
            let mut points = BTreeMap::new();
            let count = parse_programme_source_bytes(path, &bytes, &mut points)
                .expect("real programme workbook should parse");
            assert_eq!(count, points.len());
            let first = points
                .keys()
                .next()
                .expect("workbook should have a first point");
            let last = points
                .keys()
                .next_back()
                .expect("workbook should have a last point");
            let mut distinct_values = points.values().copied().collect::<Vec<_>>();
            distinct_values.sort_by(f64::total_cmp);
            distinct_values.dedup_by(|left, right| left.total_cmp(right).is_eq());
            eprintln!(
                "{path}: {count} points, {first} to {last}, {} distinct values",
                distinct_values.len()
            );
            let programme_date = first.date_naive();
            let hourly_values = points
                .iter()
                .filter(|(timestamp, _)| timestamp.date_naive() == programme_date)
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            eprintln!("  {programme_date} Q Seujet 00h..23h: {hourly_values:?}");
            let mut by_date = BTreeMap::<NaiveDate, Vec<f64>>::new();
            for (timestamp, value) in &points {
                by_date
                    .entry(timestamp.date_naive())
                    .or_default()
                    .push(*value);
            }
            for (date, mut values) in by_date {
                values.sort_by(f64::total_cmp);
                let minimum = values.first().copied().expect("day has a minimum");
                let maximum = values.last().copied().expect("day has a maximum");
                values.dedup_by(|left, right| left.total_cmp(right).is_eq());
                eprintln!(
                    "  {date}: {} hourly points, {} distinct, {minimum:.1}..{maximum:.1} m³/s",
                    points
                        .keys()
                        .filter(|timestamp| timestamp.date_naive() == date)
                        .count(),
                    values.len()
                );
            }
            combined.extend(points);
        }

        assert!(!combined.is_empty());
        eprintln!("combined: {} unique timestamped points", combined.len());
    }

    #[test]
    fn pro_tokens_require_valid_signature_and_expiry() {
        let state = test_state();
        let token = sign_pro_token(&state, Utc::now() + chrono::Duration::hours(1))
            .expect("token should sign");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("valid header"),
        );
        assert!(is_authorized_pro(&headers, &state));

        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}x").parse().expect("valid header"),
        );
        assert!(!is_authorized_pro(&headers, &state));

        let expired = sign_pro_token(&state, Utc::now() - chrono::Duration::hours(1))
            .expect("expired token should still sign");
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {expired}").parse().expect("valid header"),
        );
        assert!(!is_authorized_pro(&headers, &state));
    }

    #[tokio::test]
    async fn email_ingest_rejects_invalid_bearer_before_database_check() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer wrong-token".parse().expect("valid header"),
        );

        let response = email_ingest_handler(State(state), headers, Bytes::from_static(b"ignored"))
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn imap_status_requires_ingest_token() {
        let state = test_state();
        let response = imap_status_handler(State(state), HeaderMap::new())
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn imap_status_reports_disabled_without_credentials() {
        let state = test_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer ingest-token".parse().expect("valid header"),
        );
        let response = imap_status_handler(State(state), headers)
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("status body should be readable");
        let status: ImapStatus = serde_json::from_slice(&bytes).expect("status should deserialize");
        assert!(!status.configured);
        assert!(!status.running);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn imap_status_tracks_success_and_error_without_exposing_credentials() {
        let mut status = ImapStatus {
            configured: true,
            running: true,
            host: Some("mail.infomaniak.com".to_string()),
            username: Some("debit@example.test".to_string()),
            mailbox: Some("INBOX".to_string()),
            ..ImapStatus::default()
        };
        apply_imap_poll_result(&mut status, &Ok(3), "2026-07-16T20:00:00Z".to_string());
        assert_eq!(status.messages_ingested, 3);
        assert_eq!(
            status.last_success_at.as_deref(),
            Some("2026-07-16T20:00:00Z")
        );
        assert!(status.last_error.is_none());

        apply_imap_poll_result(
            &mut status,
            &Err("authentication failed".to_string()),
            "2026-07-16T20:05:00Z".to_string(),
        );
        assert_eq!(status.messages_ingested, 3);
        assert_eq!(
            status.last_success_at.as_deref(),
            Some("2026-07-16T20:00:00Z")
        );
        assert_eq!(status.last_error.as_deref(), Some("authentication failed"));

        let json = serde_json::to_string(&status).expect("status should serialize");
        assert!(!json.contains("password"));
        assert!(!json.contains("secret"));
    }

    fn test_state() -> AppState {
        AppState {
            client: Client::new(),
            cache: Arc::new(RwLock::new(None)),
            db: None,
            ingest_token: Some("ingest-token".to_string()),
            pro_code: "pro-code".to_string(),
            token_secret: "token-secret".to_string(),
            imap_status: Arc::new(RwLock::new(ImapStatus::default())),
        }
    }

    fn synthetic_programme_mail(workbook: &[u8]) -> String {
        let encoded = base64::Engine::encode(&STANDARD, workbook);
        let wrapped = encoded
            .as_bytes()
            .chunks(76)
            .map(|chunk| std::str::from_utf8(chunk).expect("base64 is utf8"))
            .collect::<Vec<_>>()
            .join("\r\n");

        format!(
            concat!(
                "From: sig@example.test\r\n",
                "Subject: Programme débit\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"rhonometre-test\"\r\n",
                "\r\n",
                "--rhonometre-test\r\n",
                "Content-Type: text/plain; charset=utf-8\r\n",
                "\r\n",
                "Programme attached.\r\n",
                "--rhonometre-test\r\n",
                "Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet; name=\"programme.xlsx\"\r\n",
                "Content-Disposition: attachment; filename=\"programme.xlsx\"\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "\r\n",
                "{}\r\n",
                "--rhonometre-test--\r\n"
            ),
            wrapped
        )
    }

    fn synthetic_programme_workbook() -> Vec<u8> {
        synthetic_programme_workbook_with_sheet(&sheet_xml())
    }

    fn synthetic_programme_workbook_with_sheet(sheet: &str) -> Vec<u8> {
        let cursor = IoCursor::new(Vec::new());
        let mut zip = ZipWriter::new(cursor);

        add_zip_file(
            &mut zip,
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
        );
        add_zip_file(
            &mut zip,
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        );
        add_zip_file(
            &mut zip,
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Programme" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#,
        );
        add_zip_file(
            &mut zip,
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        );
        add_zip_file(&mut zip, "xl/worksheets/sheet1.xml", sheet);

        zip.finish().expect("zip should finish").into_inner()
    }

    fn sheet_xml() -> String {
        let headers = (0..24)
            .map(|hour| {
                let cell = format!("{}1", column_name(hour + 2));
                inline_string_cell(&cell, &format!("{hour}h"))
            })
            .collect::<String>();
        let values = (0..24)
            .map(|hour| {
                let cell = format!("{}2", column_name(hour + 2));
                format!(r#"<c r="{cell}"><v>{}</v></c>"#, 100 + hour)
            })
            .collect::<String>();

        let label_cell = inline_string_cell("A2", "Q Seujet");
        let date_cell = inline_string_cell("B2", "20.06.2026");

        format!(
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
                r#"<sheetData>"#,
                r#"<row r="1">{headers}</row>"#,
                r#"<row r="2">"#,
                "{label_cell}",
                "{date_cell}",
                "{values}",
                r#"</row>"#,
                r#"</sheetData>"#,
                r#"</worksheet>"#
            ),
            headers = headers,
            label_cell = label_cell,
            date_cell = date_cell,
            values = values
        )
    }

    fn programme_sheet_xml(date: &str, base: i32, daily_averages: &[i32]) -> String {
        let headers = (0..24)
            .map(|hour| {
                let cell = format!("{}19", column_name(hour + 1));
                inline_string_cell(&cell, &format!("{hour}h"))
            })
            .collect::<String>();
        let values = (0..24)
            .map(|hour| {
                let cell = format!("{}20", column_name(hour + 1));
                format!(
                    r#"<c r="{cell}"><v>{}</v></c>"#,
                    base + i32::try_from(hour).expect("hour fits i32")
                )
            })
            .collect::<String>();
        let daily_rows = daily_averages
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let row = 21 + index;
                format!(
                    r#"<row r="{row}">{}<c r="B{row}"><v>{value}</v></c>{}</row>"#,
                    inline_string_cell(&format!("A{row}"), "Seujet débit moyen journalier"),
                    inline_string_cell(&format!("C{row}"), "m³/s")
                )
            })
            .collect::<String>();

        format!(
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
                r#"<sheetData>"#,
                r#"<row r="17">{title}</row>"#,
                r#"<row r="19">{headers}</row>"#,
                r#"<row r="20">{label}{values}</row>"#,
                "{daily_rows}",
                r#"</sheetData>"#,
                r#"</worksheet>"#
            ),
            headers = headers,
            title = inline_string_cell("A17", &format!("Programme du {date}")),
            label = inline_string_cell("A20", "Q Seujet"),
            values = values,
            daily_rows = daily_rows,
        )
    }

    fn add_zip_file(zip: &mut ZipWriter<IoCursor<Vec<u8>>>, name: &str, contents: &str) {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file(name, options)
            .expect("zip file should start");
        zip.write_all(contents.as_bytes())
            .expect("zip file should write");
    }

    fn inline_string_cell(cell: &str, value: &str) -> String {
        format!(r#"<c r="{cell}" t="inlineStr"><is><t>{value}</t></is></c>"#)
    }

    fn column_name(mut index: usize) -> String {
        let mut name = String::new();
        loop {
            let remainder = index % 26;
            name.insert(0, char::from(b'A' + remainder as u8));
            if index < 26 {
                break;
            }
            index = (index / 26) - 1;
        }
        name
    }
}
