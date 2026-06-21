# rhonometre

Modern water conditions dashboard for the Geneva Rhône area. The app displays live discharge, water level, and water temperature where available, with five-day history plots and discharge forecasts sourced from the Swiss Hydrodaten service.

## Data Sources

The default station set is:

- Arve - Genève, Bout du Monde (`2170`)
- Rhône - Genève, Halle de l'Ile (`2606`, estimated while the station is offline)
- Lac Léman - Genève, Sécheron (`2028`)
- Rhône - Chancy, Aux Ripes (`2174`, downstream/post-Jonction Rhône reference)

Hydrodaten currently publishes Rhône - Genève, Halle de l'Ile (`2606`) as missing and does not expose its seven-day JSON history. The app therefore derives `2606` from Arve (`2170`) and downstream Rhône at Chancy (`2174`):

- `Q_2606 = Q_2174 - Q_2170`
- `T_2606 = (Q_2174 * T_2174 - Q_2170 * T_2170) / Q_2606`

The temperature estimate is lagged before applying that heat balance: Chancy is downstream of the Jonction, so the app estimates travel time from discharge/current and samples Arve/Rhône terms at the corresponding upstream times. The displayed timestamp for `2606` temperature is therefore the estimated time when that water passed Halle de l'Ile, not the Chancy measurement time.

The UI shows a station-level warning for `2606` while this estimate is used.

Where Hydrodaten publishes discharge forecasts, the app overlays the forecast median on the discharge chart. For estimated `2606`, the app derives the forecast with the same flow balance at matching timestamps:

- `Q_forecast_2606 = Q_forecast_2174 - Q_forecast_2170`

Hydrodaten endpoints used by the server:

- Current discharge and water level: `https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_pq.geojson`
- Current water temperature: `https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_temperature.geojson`
- Forecast station overview: `https://www.hydrodaten.admin.ch/web-hydro-maps/hydro_sensor_pq_forecast.geojson`
- Historical discharge/water level: `https://www.hydrodaten.admin.ch/plots/p_q_7days/{station}_p_q_7days_de.json`
- Historical water temperature: `https://www.hydrodaten.admin.ch/plots/temperature_7days/{station}_temperature_7days_de.json`
- Discharge forecast: `https://www.hydrodaten.admin.ch/plots/q_forecast/{station}_q_forecast_de.json`

The server caches upstream Hydrodaten responses for two minutes and trims history series to the latest five days.

## Development

This repository includes a Nix development shell with Rust, the `wasm32-unknown-unknown` target, and Trunk.

Nix flakes only see files tracked by Git. In a brand-new checkout, add the new project files to Git before `nix develop` if Nix reports that `flake.nix` is untracked.

```sh
nix run
```

Then open `http://127.0.0.1:3000`. The default flake app builds the Leptos frontend into `frontend/dist` and starts the Axum server.

For manual development steps:

```sh
nix develop
cd frontend
trunk build index.html --dist dist
cd ..
cargo run -p nivrhone-server
```

For API-only work:

```sh
cargo run -p nivrhone-server
curl http://127.0.0.1:3000/api/dashboard
```

## Build

```sh
nix develop -c trunk build frontend/index.html --dist frontend/dist --release
nix develop -c cargo build -p nivrhone-server --release
```

The Axum server serves the built frontend from `frontend/dist` by default. Override with `STATIC_DIR=/path/to/dist`.
