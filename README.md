# rhônomètre

Modern water conditions dashboard for the Geneva Rhône area. The server is an Axum data hub backed by Postgres, and the client is a Dioxus app served as web assets and structured for native mobile clients using the same API.

The normal dashboard shows live discharge and temperature with five-day history. Pro mode is
server-authenticated and switches to a Rhône-only discharge view for Halle de l'Île. Its five-day
window contains yesterday, today, and the next three days. Measured discharge is blue; SIG programme
values are rendered as a translucent red forecast.

## Data Sources

Default stations:

- Arve - Genève, Bout du Monde (`2170`)
- Rhône - Genève, Halle de l'Île (`2606`, measured when Hydrodaten is online, estimated fallback otherwise)
- Lac Léman (`2028` level at Genève-Sécheron, paired in the non-focus dashboard with
  the `2606` lake-outflow temperature at Halle de l'Île)
- Rhône - Chancy, Aux Ripes (`2174`, downstream/post-Jonction Rhône reference)

The non-focus UI shows Arve, Halle de l'Île, Lac Léman, and Chancy tabs.

The server prefers measured Hydrodaten data for Rhône - Genève, Halle de l'Île (`2606`). If the station is unavailable or incomplete, the app derives `2606` from Arve (`2170`) and downstream Rhône at Chancy (`2174`):

```text
Q_2606 = Q_2174 - Q_2170
T_2606 = (Q_2174 * T_2174 - Q_2170 * T_2170) / Q_2606
```

The fallback temperature estimate is lagged before applying the heat balance: Chancy is downstream of the Jonction, so the server estimates travel time from discharge/current and samples Arve/Rhône terms at the corresponding upstream times. The displayed fallback `2606` temperature timestamp is therefore the estimated time when that water passed Halle de l'Île.

Pro forecasts use only stored SIG "Programme débit" points for `2606`; Hydrodaten forecasts are
not substituted. The parser reads hourly `Q Seujet` values and dates found in attachment names,
email subjects, title rows, or near the programme row. A `Seujet débit moyen journalier` value is
expanded into a horizontal daily estimate for each subsequent day. If several dated files arrive
together, as on Fridays, their hourly `Q Seujet` values are combined by date and take precedence
over daily-average fallback values.

The dashboard also fetches Geneva air temperature from Open-Meteo for the normal current-conditions
summary. It is not plotted in Pro mode.

Hydrodaten endpoints used by the server:

- Current discharge and water level: `https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_pq.geojson`
- Current water temperature: `https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_temperature.geojson`
- Historical discharge/water level: `https://www.hydrodaten.admin.ch/plots/p_q_7days/{station}_p_q_7days_de.json`
- Historical water temperature: `https://www.hydrodaten.admin.ch/plots/temperature_7days/{station}_temperature_7days_de.json`

## Server API

- `GET /healthz`
- `GET /api/v1/dashboard`
- `GET /api/v1/stations/:id/series?from=&to=&forecast=true`
- `POST /api/v1/auth/pro`
- `POST /api/admin/email-ingest`
- `GET /api/admin/imap-status`

`/api/v1/dashboard` strips forecasts unless the request includes a valid pro bearer token. `/api/v1/stations/:id/series?forecast=true` also requires pro authorization.

Pro auth validates `RHONOMETRE_PRO_CODE` server-side and returns a signed bearer token. The signing secret is `RHONOMETRE_TOKEN_SECRET`, falling back to the ingest token or pro code for local development.

Email ingest is protected by:

```http
Authorization: Bearer $RHONOMETRE_INGEST_TOKEN
```

The body may be raw RFC822 MIME, multipart uploaded `.eml`, or uploaded `.xls`/`.xlsx`. The
server extracts attached Excel workbooks, reads hourly `Q Seujet` and daily-average points, and
upserts them idempotently as `2606` discharge forecasts. PostgreSQL stores detailed dates as
`sig_programme_hourly` and fallback dates as `sig_programme_daily`, so an hourly curve always
wins regardless of email arrival order. Raw emails are not retained by default; Postgres stores
only message hash, received time, subject, attachment names, parsed point count, warnings, and
normalized series points.

Do not share the password for `debit@pontonniers-geneve.ch`. The direct Infomaniak IMAP setup
described below is the simplest production option. A webhook provider such as Mailgun, SendGrid
Inbound Parse, or a Cloudflare Email Worker remains an alternative; it must POST the raw RFC822
message to `https://<rhonometre-host>/api/admin/email-ingest` with:

```http
Authorization: Bearer $RHONOMETRE_INGEST_TOKEN
Content-Type: message/rfc822
```

The application also includes an optional IMAP poller for Infomaniak. Configure
`RHONOMETRE_IMAP_USERNAME=debit@pontonniers-geneve.ch` and
`RHONOMETRE_IMAP_PASSWORD=<generated mailbox password>`. It uses TLS on
`mail.infomaniak.com:993`, processes unseen messages, periodically checks the 50 most recent
messages so a manually opened email is not missed, and marks messages as seen only after
successful PostgreSQL ingestion. Content hashes make repeat scans idempotent. Never place a
mailbox password in Git or in the frontend.

The protected status endpoint uses the same ingest bearer token:

```sh
curl -H "Authorization: Bearer $RHONOMETRE_INGEST_TOKEN" \
  https://<rhonometre-host>/api/admin/imap-status
```

## Development

This repository includes a Nix development shell with Rust, the wasm and iOS targets on macOS, Dioxus CLI, Trunk, and Postgres client tools.

Run the local app:

```sh
nix run
```

Then open `http://127.0.0.1:3000`. Without `DATABASE_URL`, the server still serves the live dashboard from Hydrodaten but does not persist refreshes or accept stored programme forecasts.

Single-station iframe/embed URLs use the same app bundle:

```html
<iframe
  src="http://127.0.0.1:3000/?embed=1&station=2174"
  title="Rhonometre - Rhône Chancy"
  width="100%"
  height="760"
  loading="lazy"
></iframe>
```

`station` accepts either the Hydrodaten id (`2170`, `2606`, `2174`) or the station slug (`arve-bout-du-monde`, `rhone-halle-ile`, `rhone-chancy`). `?embed=2606` is a shorthand for the default French embed, and `lang=en` switches the labels to English.

For a local Postgres-backed run:

```sh
export DATABASE_URL=postgres://rhonometre:rhonometre@127.0.0.1:5432/rhonometre
export RHONOMETRE_INGEST_TOKEN=dev-ingest-token
export RHONOMETRE_PRO_CODE=dev-pro-code
export RHONOMETRE_TOKEN_SECRET=dev-token-secret
nix run
```

Manual checks:

```sh
nix develop -c cargo check -p nivrhone-server
nix develop -c cargo check -p nivrhone-frontend --target wasm32-unknown-unknown
nix develop -c env -u SDKROOT -u DEVELOPER_DIR sh -c 'export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH"; cargo check -p nivrhone-frontend --target aarch64-apple-ios-sim --no-default-features --features mobile'
nix build .#default
```

## iOS App

The native iOS app is the same Dioxus frontend built with the `mobile` feature. Its bundle metadata is in `frontend/Dioxus.toml`, and it calls the Axum API configured by `RHONOMETRE_API_BASE`.

Simulator:

```sh
# Terminal 1: start the API server
nix run

# Terminal 2: start the simulator app
open /Applications/Xcode.app/Contents/Developer/Applications/Simulator.app
xcrun simctl boot "iPhone 15 Pro Max"
nix run .#ios-simulator
```

For the simulator, the default API base is `http://127.0.0.1:3000`, which reaches the server running on the Mac.

Physical iPhone:

```sh
# Terminal 1: start the API server on the LAN
HOST=0.0.0.0 nix run

# Terminal 2: build/sign/deploy the app
RHONOMETRE_API_BASE=http://YOUR_MAC_LAN_IP:3000 \
IOS_DEVICE="Your iPhone Name" \
APPLE_TEAM_ID="Apple Development: Your Name (TEAMID)" \
nix run .#ios-device
```

For a real iPhone, keep the phone and Mac on the same network or use the production HTTPS URL. `APPLE_TEAM_ID` is the signing identity shown by `security find-identity -v -p codesigning`, and `IOS_DEVICE` can be the device name or UDID. The wrapper clears Nix's macOS SDK variables before calling Xcode tools so iOS builds use the real Xcode iPhone SDK.

## Production

The flake builds one package containing:

- `bin/rhonometre-server`
- `bin/rhonometre`
- Dioxus web assets under `share/rhonometre/dist`

Required production environment:

```sh
DATABASE_URL=postgres://rhonometre:...@127.0.0.1:5432/rhonometre
RHONOMETRE_INGEST_TOKEN=...
RHONOMETRE_PRO_CODE=...
RHONOMETRE_TOKEN_SECRET=...
```

For container orchestration where Postgres may start slightly after the app, the server retries
database connection and schema migration on startup. Tune this with:

```sh
RHONOMETRE_DATABASE_CONNECT_ATTEMPTS=30
RHONOMETRE_DATABASE_CONNECT_RETRY_SECONDS=2
```

Optional local programme import paths:

```sh
RHONOMETRE_PROGRAMME_PATH=/var/lib/rhonometre/programmes/latest.eml
RHONOMETRE_PROGRAMME_DIR=/var/lib/rhonometre/programmes
```

The included NixOS module exposes `services.rhonometre`. It creates `/var/lib/rhonometre`, `/var/lib/rhonometre/programmes`, and `/var/backups/rhonometre`, points `STATIC_DIR` at the packaged Dioxus build, and expects secrets in `/etc/rhonometre.env` by default.

Typical deployment shape:

1. Run Postgres on the VPS.
2. Enable the `services.rhonometre` NixOS module.
3. Put `DATABASE_URL`, `RHONOMETRE_INGEST_TOKEN`, `RHONOMETRE_PRO_CODE`, and `RHONOMETRE_TOKEN_SECRET` in the env file.
4. Put Caddy or nginx in front of the Axum service for HTTPS.
5. Configure the inbound email provider to forward SIG programme mail to `POST /api/admin/email-ingest`.

### Infomaniak Jelastic

For an Infomaniak Jelastic environment that deploys from GitHub, use the root `Dockerfile`.
The container builds the Axum server and Dioxus web assets, listens on `0.0.0.0:8080`,
and serves the frontend from `/app/dist`.

The repo also includes a Jelastic import package at
`deploy/jelastic/rhonometre.jps`. It creates one rhônomètre app container and one
PostgreSQL container, but it does not create resources until it is imported and installed
from the Jelastic dashboard.

Before importing the JPS package, publish a pullable image. The GitHub Actions workflow
in `.github/workflows/docker.yml` publishes:

```sh
ghcr.io/huberlabgit/rhonometre:latest
```

Import URL after pushing these files:

```text
https://raw.githubusercontent.com/huberlabgit/rhonometre/main/deploy/jelastic/rhonometre.jps
```

See `deploy/jelastic/README.md` for the full pre-install checklist.

Local Docker build test:

```sh
nix run .#docker-up
nix run .#docker-build
```

On macOS, `nix run .#docker-up` starts a Colima VM-backed Docker daemon. Stop it with
`nix run .#docker-down`. Override the local image tag with
`RHONOMETRE_DOCKER_TAG=registry.example/rhonometre:test nix run .#docker-build`.
If you need an amd64 image from Apple Silicon, set
`RHONOMETRE_DOCKER_PLATFORM=linux/amd64`.

Recommended Jelastic topology:

- One Docker/custom application node using the published rhônomètre image.
- One PostgreSQL node, either from the included JPS Docker PostgreSQL container or a managed Jelastic/Infomaniak PostgreSQL node wired through `DATABASE_URL`.
- Public HTTPS routing to the application node's HTTP port.

Application environment variables:

```sh
HOST=0.0.0.0
PORT=8080
STATIC_DIR=/app/dist
DATABASE_URL=postgres://USER:PASSWORD@POSTGRES_HOST:5432/DB_NAME
RHONOMETRE_INGEST_TOKEN=...
RHONOMETRE_PRO_CODE=...
RHONOMETRE_TOKEN_SECRET=...
RHONOMETRE_PROGRAMME_DIR=/data/programmes
```

The app can run without `DATABASE_URL`, but pro SIG programme forecasts require Postgres.
Do not store programme emails in the application container filesystem; use the email ingest
webhook at `/api/admin/email-ingest` so normalized points are stored in Postgres.
