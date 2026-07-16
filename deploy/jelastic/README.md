# Infomaniak Jelastic deployment

This directory contains the import package for the production Jelastic setup. It creates:

- `cp`: the rhonometre app container from a Docker image.
- `db`: a PostgreSQL 16 container with persistent `/var/lib/postgresql/data`.

Nothing in this directory creates resources by itself. Resource usage starts only when the JPS file is imported and installed from the Jelastic dashboard.

## 1. Publish the app image

Push `main` to GitHub and let `.github/workflows/docker.yml` publish:

```text
ghcr.io/huberlabgit/rhonometre:latest
```

If Jelastic cannot pull the image, make the GHCR package public in GitHub or replace the image field during import with another public registry image.

## 2. Prepare secrets

Generate these before import:

- PostgreSQL password: 16 or more URL-safe characters.
- Email ingest bearer token: 24 or more URL-safe characters.
- Pro access code: the code users enter in the app.
- Pro session signing secret: 32 or more URL-safe characters.
- Infomaniak IMAP address: optional; use `debit@pontonniers-geneve.ch` to ingest directly.
- Infomaniak generated mailbox password: optional; create a dedicated password for this
  application rather than reusing a personal Infomaniak login password.

The database password is embedded into `DATABASE_URL`, so avoid spaces and URL-reserved characters.

## 3. Import the package

In Infomaniak Jelastic:

1. Open **Import**.
2. Use the **URL** tab with the raw GitHub URL after this file is pushed:

   ```text
   https://raw.githubusercontent.com/huberlabgit/rhonometre/main/deploy/jelastic/rhonometre.jps
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

### Direct Infomaniak mailbox access

As an alternative to a third-party inbound-email provider, configure the two optional IMAP
settings during installation. The application connects with TLS to
`mail.infomaniak.com:993`, reads unseen messages from `INBOX`, periodically checks the 50
most recent messages, runs the same RFC822/Excel ingest path, and marks a message as seen only
after a successful database write. Content hashes prevent duplicate ingestion.

For an existing Jelastic environment, add these secret environment variables to the application
container and redeploy it:

```text
RHONOMETRE_IMAP_USERNAME=debit@pontonniers-geneve.ch
RHONOMETRE_IMAP_PASSWORD=<generated mailbox password>
```

Optional overrides are `RHONOMETRE_IMAP_HOST`, `RHONOMETRE_IMAP_PORT`,
`RHONOMETRE_IMAP_MAILBOX`, and `RHONOMETRE_IMAP_POLL_SECONDS`. Leave both username and
password unset or empty to disable polling.

Check the live poller state with:

```sh
curl -H "Authorization: Bearer <email ingest bearer token>" \
  https://<jelastic-env-url>/api/admin/imap-status
```

The response reports whether IMAP is configured and running, the last poll and success times,
the number of newly ingested messages, and the latest connection or parsing error. It never
returns the mailbox password.
