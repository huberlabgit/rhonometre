# Infomaniak Jelastic deployment

This directory contains the import package for the production Jelastic setup. It creates:

- `cp`: the rhonometre app container from a Docker image.
- `db`: a PostgreSQL 16 container with persistent `/var/lib/postgresql/data`.

Nothing in this directory creates resources by itself. Resource usage starts only when the JPS file is imported and installed from the Jelastic dashboard.

## 1. Publish the app image

Push `main` to GitHub and let `.github/workflows/docker.yml` publish:

```text
ghcr.io/lcnbr/rhonometre:latest
```

If Jelastic cannot pull the image, make the GHCR package public in GitHub or replace the image field during import with another public registry image.

## 2. Prepare secrets

Generate these before import:

- PostgreSQL password: 16 or more URL-safe characters.
- Email ingest bearer token: 24 or more URL-safe characters.
- Pro access code: the code users enter in the app.
- Pro session signing secret: 32 or more URL-safe characters.

The database password is embedded into `DATABASE_URL`, so avoid spaces and URL-reserved characters.

## 3. Import the package

In Infomaniak Jelastic:

1. Open **Import**.
2. Use the **URL** tab with the raw GitHub URL after this file is pushed:

   ```text
   https://raw.githubusercontent.com/lcnbr/rhonometre/main/deploy/jelastic/rhonometre.jps
   ```

3. Fill the image and secret fields.
4. Review pricing/resources.
5. Click install only after you are ready to create the environment.

## 4. Configure email ingest

After installation, configure the inbound email provider to POST raw programme emails or uploaded `.eml`/`.xls` payloads to:

```text
https://<jelastic-env-url>/api/admin/email-ingest
```

with:

```http
Authorization: Bearer <email ingest bearer token>
```

The server stores normalized forecast points and ingest metadata, not raw programme emails.
