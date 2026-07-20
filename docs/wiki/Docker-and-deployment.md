# Docker and deployment

The repository includes a multi-stage Alpine/musl image. The large Rust build environment is
discarded; the final image contains only Alpine, CA certificates, `tini`, the HTTP server, and the
admin CLI. The process runs as non-root UID/GID `10001`.

## Build locally

```bash
docker build -t frankensteindb:local .
```

Run it with a named volume:

```bash
docker volume create frankensteindb-data

docker run --rm --name frankensteindb \
  -p 8080:8080 \
  -e FRANKENSTEINDB_API_KEY='replace-with-a-long-random-secret' \
  -v frankensteindb-data:/data \
  frankensteindb:local
```

The image default command is:

```text
frankensteindb-server /data --listen 0.0.0.0:8080
```

Use a different host port without changing the container:

```bash
docker run -p 9000:8080 ... frankensteindb:local
```

## Docker Compose

The included `compose.yaml` works without editing:

```bash
export FRANKENSTEINDB_API_KEY='replace-with-a-long-random-secret'
docker compose up --build -d
docker compose ps
curl http://localhost:8080/health
```

Configuration variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `FRANKENSTEINDB_API_KEY` | `change-me-before-production` | Development bearer token |
| `FRANKENSTEINDB_PORT` | `8080` | Published host port |
| `FRANKENSTEINDB_IMAGE` | `ghcr.io/your-org/frankensteindb:latest` | Image used by Compose |

For production, never accept the placeholder key. Prefer a mounted scoped-key configuration file
over a plaintext environment variable.

The service root filesystem is read-only. `/data` is the persistent named volume and `/tmp` is a
small tmpfs. The container healthcheck uses BusyBox `wget`, so no separate curl package is included.

## Use the admin CLI from the image

Stop the server before an offline restore or direct CLI operation, then mount the same volume:

```bash
docker compose stop frankensteindb

docker run --rm \
  --entrypoint frankensteindb \
  -v frankensteindb-data:/data \
  frankensteindb:local \
  /data tables
```

For restore, mount the backup file read-only as well.

## GHCR image tags

`.github/workflows/container.yml` builds `linux/amd64` and `linux/arm64` images. Pull requests build
without publishing. Every pushed commit publishes an immutable SHA tag:

```text
ghcr.io/<repository-owner>/frankensteindb:sha-<full-commit-sha>
```

It also publishes convenient aliases:

| Git event | Additional tags |
| --- | --- |
| Branch push | Sanitized branch name |
| Default branch push | `latest` |
| Git tag `v1.2.3` | `1.2.3` and `1.2` |

The workflow attaches OCI metadata, an SBOM, BuildKit provenance, and a GitHub build attestation.
Deploy the SHA tag or digest when reproducibility matters; treat `latest` as a convenience alias.

```bash
docker pull ghcr.io/<owner>/frankensteindb:sha-<commit>
```

GitHub Actions authenticates with `GITHUB_TOKEN`; the repository workflow needs `packages: write`.
The published package may need to be made public in the repository owner's Packages settings.

## Production recommendations

- Put HTTPS and request-size policy in a reverse proxy or service mesh.
- Mount `/data` on local persistent storage with enough room for SQLite, Tantivy generations,
  merges, backups, and temporary rebuilds.
- Allow at least one extra index generation during reindex or schema migration.
- Back up outside the container volume.
- Use health checks for replacement decisions, not as a substitute for query and outbox monitoring.
- Send `SIGTERM` and allow the configured grace period; `tini` forwards signals to the server.
- Run one FrankensteinDB server per database directory.
- Pin production deployments to a SHA tag or image digest.

## Kubernetes notes

FrankensteinDB is stateful. Use a `StatefulSet` with one replica per volume, a persistent volume
mounted at `/data`, a `ReadWriteOnce` access mode, and an HTTP probe on `/health`. Do not point
multiple replicas at the same filesystem. Horizontal sharding must use separate database
directories; distributed aggregation endpoints can merge aggregation fruits at the application
layer.
