# Container Monitoring — Design Spec

Date: 2026-07-08
Status: Approved (design), pending spec review
Feature branch: `feat/container-monitoring`

## Goal

Replace the `extras.containers: []` stub with a real Docker container monitor: a
dashboard panel showing every running container's name, image, state, health,
CPU%, memory, network/block I/O, PIDs, restart count, ports, uptime, and whether
a newer image is available upstream. This is roadmap item #1 from the README.

Consistent with the rest of aetheris: **no fabricated data** — when Docker or a
registry can't be reached, report an explicit empty/unknown state, never a guess.

## Non-goals

- Container control (start/stop/exec) — read-only telemetry only.
- Kubernetes / non-Docker runtimes.
- Historical container metrics / RUL for containers.
- macOS/Windows-specific container backends (Docker Desktop exposes the same
  API/socket, so it works, but is not separately tested here).

## Verified facts (2026-07-08)

- **bollard 0.21.0** (released 2026-05-04) requires **tokio ^1.47**; the project
  pins tokio 1.52.3, so it is compatible.
- **Docker CLI JSON** on this host (`docker ps --format '{{json .}}'`) includes:
  `Names, Image, ID, State, Status, HealthStatus, RunningFor, CreatedAt, Ports,
  Labels, Networks, Size`. `docker stats --no-stream --format '{{json .}}'`
  includes: `Name, ID, CPUPerc, MemUsage, MemPerc, NetIO, BlockIO, PIDs`.
  Restart count is NOT in `ps`/`stats` — it is `State.RestartCount` from
  `docker inspect` / the Engine API.
- **CPU% correctness**: `docker stats --no-stream` (CLI) and bollard with
  `stream:false` (NOT `one_shot:true`) return a frame with both `cpu_stats` and
  `precpu_stats` populated (~1s sample). `one_shot:true` returns immediately but
  with empty `precpu_stats`, making CPU% uncomputable — so we do not use it.
  Formula: `cpu_delta = cpu.total_usage - precpu.total_usage`;
  `system_delta = cpu.system_cpu_usage - precpu.system_cpu_usage`;
  `cpu% = (cpu_delta / system_delta) * online_cpus * 100` when both deltas > 0.
- **Registry v2 update check**: `HEAD https://{registry}/v2/{repo}/manifests/{tag}`
  with `Accept` listing the manifest-list + OCI-index + manifest v2 media types;
  the `Docker-Content-Digest` response header is the tag's current digest. On
  `401`, parse `WWW-Authenticate: Bearer realm=…,service=…[,scope=…]`, GET
  `{realm}?service={service}&scope=repository:{repo}:pull` anonymously (public
  images) to obtain a token, then retry with `Authorization: Bearer {token}`.
  Compare the returned digest to the container image's local `RepoDigests`.
- **Docker Hub anonymous pull limit**: 100 pulls / 6h (the April-2025 reduction
  was postponed). Manifest HEADs count toward it, so per-image checks are cached
  30–60 min — ~26 images ≪ 100/6h.
- This host: Docker socket present at `/var/run/docker.sock`, 26 running
  containers, images from ghcr.io / docker.io / etc. (public → anonymous token).

## Architecture

Because bollard is async and registry checks are slow/network-bound, container
collection runs in a **background task feeding a global cache** — the same
pattern already used by `cloud_ranges` and `external_baselines`. `get_stats`
reads the cache; it never blocks on Docker or the network.

```
main.rs setup: spawn periodic containers::refresh()  ──writes──▶  global CACHE
get_stats (sync, cached read) ──reads──▶ extras.containers  ──▶ frontend panel
```

### Module layout

- `monitors/containers.rs` — orchestration + backends + cache.
  - `pub fn get_container_stats() -> Value` — synchronous cache read for
    `get_stats` (returns `{status, containers:[...]}`; `status` ∈
    `ok | unavailable`).
  - `pub async fn refresh()` — spawned periodically; collects containers via the
    first working backend, merges cached image-update flags, writes the cache.
  - `mod cli` — CLI backend (primary).
  - `mod api` — bollard backend (fallback).
- `monitors/registry.rs` — `pub async fn check_update(image, local_digests) ->
  Option<bool>` plus the bearer-token + manifest-digest logic, with its own
  per-image cache (30–60 min TTL) and rate-limit friendliness.

### Backend selection (auto-detect)

`refresh()` tries, in order, and uses the first that yields containers:
1. **CLI backend** — `tokio::process::Command` (or `spawn_blocking` over
   `std::process`) running `docker ps`, `docker stats --no-stream`, and one
   batched `docker inspect $(docker ps -q --no-trunc)` (RestartCount + repo
   digests + health), joined by container ID. Pure parsers over the JSON.
2. **bollard backend** — `Docker::connect_with_local_defaults()` →
   `list_containers`, per-container `stats(stream:false)` (fetched concurrently
   so 26 containers ≈ one sample interval, not 26×), `inspect_container`
   (RestartCount, Health), `inspect_image` (RepoDigests). CPU% via the formula
   above.
3. Neither → cache `{status:"unavailable", reason, containers:[]}`.

Both backends produce the **same normalized `Container` shape**, so downstream
code and tests are backend-agnostic.

## Data model (`Container`)

| field | source | notes |
|---|---|---|
| `name` | ps Names / summary | |
| `image` | ps Image | |
| `state` | ps State | running/exited/paused/… |
| `status` | ps Status | e.g. "Up 20 hours (healthy)" |
| `health` | HealthStatus / State.Health.Status | healthy/unhealthy/starting/none |
| `cpuPercent` | stats | f64 or null |
| `memUsed`,`memLimit`,`memPercent` | stats MemUsage/MemPerc | bytes; parse "23.43MiB / 23.41GiB" |
| `netRx`,`netTx` | stats NetIO | bytes |
| `blockRead`,`blockWrite` | stats BlockIO | bytes |
| `pids` | stats PIDs | u64 |
| `restartCount` | inspect State.RestartCount | u64 |
| `ports` | ps Ports | string/list |
| `createdAt`,`uptime` | ps CreatedAt/RunningFor | |
| `imageUpdateAvailable` | registry.rs | `true`/`false`/`null` (unknown) |

Any unparseable/missing metric is `null`, never a fabricated 0.

## Image-update detection (`registry.rs`)

For each container: resolve `(registry, repository, tag)` from the image ref and
the local digest from the image's `RepoDigests`. If the image has no RepoDigest
(locally built, `:latest` never pulled by digest) → `null` (unknown). Else HEAD
the registry manifest (with token dance for 401), read `Docker-Content-Digest`,
and set `imageUpdateAvailable = (remote_digest != local_digest)`.

- Multi-arch: pulled multi-arch images store the **manifest-list/index** digest
  as their RepoDigest, and the manifest HEAD (with the list/index media types in
  `Accept`) returns that same index digest — so a top-level digest comparison is
  correct without descending into per-arch manifests.
- Registries: docker.io (→ `registry-1.docker.io`, `library/` prefix for
  official images), ghcr.io, quay.io, generic v2. Anonymous token for public;
  private images without creds → `null`.
- Caching: per-image result cached 30–60 min; failures/rate-limits → `null` +
  keep last-known, never block the container refresh.
- Honesty: `imageUpdateAvailable` is only `true`/`false` when a real digest
  comparison succeeded; every uncertain case is `null`.

## Caching cadences

- Container stats refresh: every ~15–30 s (CPU%/mem freshness vs. the ~1s sample
  cost). This is the `refresh()` loop interval.
- Registry update checks: per-image 30–60 min (rate-limit + updates are
  infrequent); the container refresh reuses cached flags between checks.

## Frontend

New `col-span-2` "Containers" panel in `index.html`, rendered by `main.js`:
a table — Name/Image · State+Health badge · CPU% · Mem (used/limit) · Net I/O ·
Restarts · Uptime · Update (⬆ when `true`, blank when `false`, "?" when `null`).
Color cues: unhealthy/exited → danger, restartCount high → warning. All
interpolated strings pass through `esc()`. When `status:"unavailable"` or empty →
"Docker not available" empty-state row. Reads `stats.dynamic.extras.containers`.

## Error / empty states

- No docker binary AND no socket / no permission → `unavailable` + reason.
- Individual container stat/inspect failure → that container still listed with
  the failed metrics `null`.
- Registry unreachable / rate-limited / private → `imageUpdateAvailable: null`.
- No panics on malformed CLI/API output — parse defensively.

## Testing

- **Pure parsers unit-tested** against captured samples (from this host):
  - CLI: `docker ps`/`stats` JSON incl. running, exited, unhealthy, high-restart.
  - Memory/NetIO/BlockIO human-string parsing ("23.43MiB / 23.41GiB", "1.11MB / 51.8MB").
  - bollard CPU% formula from synthetic cpu/precpu counters (incl. zero-delta guard).
  - registry: `WWW-Authenticate` challenge parse; image-ref → (registry, repo,
    tag) incl. docker.io `library/` defaulting; digest-compare decision.
- **Live `#[ignore]` probe** exercising the 26 real containers via both backends
  and a real anonymous registry digest check against a public image (ghcr.io).
- `cargo clippy` clean; whole suite green.

## Dependencies

- Add `bollard = "0.21"`.
- Reuse existing `reqwest` (json feature) for the registry HTTP calls, `serde_json`,
  `tokio`.

## Risks / open items

- bollard 0.21 moved options to builder-style (`StatsOptionsBuilder`); exact API
  names confirmed at implementation via `cargo doc`/compiler (compiler is ground
  truth — the crate is newer than training data).
- Registry auth only covers anonymous/public; private-registry creds are out of
  scope (→ `null`).
- `docker stats` sampling cost is bounded by fetching containers concurrently.
