# Spike: image vulnerability scanning (S1)

**Decision: Syft + Grype, run as containers on the managed server, SBOM in the middle.** No Trivy fallback was needed: both tools work in rootless Podman with the plain `oci-archive` flow. Trivy was not measured.

## Setup

- Host: NixOS, rootless Podman 6.1.4 (overlay), 31 GB RAM.
- Tools, pinned: `docker.io/anchore/syft:v1.57.2` (92 MB image), `docker.io/anchore/grype:v0.122.1` (96 MB image). Both Apache-2.0.
- Every scanner container ran with `--cpus 1 --memory 1g --user 0` (root inside a rootless container is the host user), the work dir bind-mounted at `/in` with `:Z`, and the Grype DB in a named volume at `/db` (`GRYPE_DB_CACHE_DIR=/db`).
- Flow, per image:
  1. `podman save --format oci-archive -o img.tar <image>`
  2. `syft oci-archive:/in/img.tar -o cyclonedx-json=/in/sbom.json -q`
  3. `grype sbom:/in/sbom.json -o json --file /in/grype.json -q`
- `docker-archive` also works (tested on `traefik/whoami`, same SBOM size). `oci-archive` stays the default.
- Piping `podman save` into Syft over stdin was **not** used: a temp file is simpler, makes the cleanup explicit, and is what the card asks for.
- Peak memory is the highest `podman stats` sample of the container's cgroup, polled every 0.3 s. It can miss short spikes, so read it as a lower bound.

## Measurements

Grype DB warm and `GRYPE_DB_AUTO_UPDATE=false`, except where noted.

| Image | Archive | Save | Syft time | Syft peak | SBOM (json / gzip) | Packages | Grype time | Grype peak | Matches |
|---|---|---|---|---|---|---|---|---|---|
| `traefik/whoami:v1.10` | 3 MiB | 0.1 s | 0.9 s | 144 MB | 4.4 KB / 1.2 KB | 4 | 1.7 s | 25 MB | 68 |
| `postgres:18-alpine` | 119 MiB | 1.2 s | 2.6 s | 147 MB | 346 KB / 76 KB | 889 | 3.4 s | 54 MB | 74 |
| `mongo:9.0` (larger, 1.1 GB) | 389 MiB | 2.7 s | **25.8 s** | **718 MB** | 2.3 MB / 474 KB | 6513 | 3.9 s | 55 MB | 328 |
| `alpine:3.14` (known-vulnerable) | 3 MiB | 0.1 s | 0.8 s | 78 MB | 50 KB / 11 KB | 92 | 2.7 s | 56 MB | 89 (8 critical, 38 high) |

- Severity mix is from Grype's own `vulnerability.severity`: `alpine:3.14` = 8 Critical, 38 High, 35 Medium, 8 Low; `mongo:9.0` also has `Negligible` and `Unknown`, so the parser must map both.
- **Memory headroom is thin for big images.** Syft on `mongo:9.0` peaked at 718 MB under the 1 GB cap; an image with tens of thousands of files would hit the cap. The cap stays configurable and a cgroup OOM kill must surface as a clear `error` on the scan ("scanner ran out of memory, raise the limit"), never as a failed deploy.
- Grype is cheap once the SBOM exists (under 4 s, under 60 MB), which is why the daily rescan reads the stored SBOM only.
- First ever Grype run (cold DB, download included): 234 MB peak. `grype db update` into an empty volume: **32 s** at 1 CPU.

### Grype DB

- Download: **204 MB** (`vulnerability-db_v6.1.10_….tar.zst`), unpacked `vulnerability.db` is **3.5 GB** (schema v6.1.10, built 2026-11-19). That is about three times the 1.2 GB Coolify quotes; budget 4 GB per server for the `podkit-scanner-db` volume.
- The pointer is `https://grype.anchore.io/databases/v6/latest.json` (status, schema, built, path, checksum).
- Grype refuses to scan when the DB is older than 5 days (`db.max-allowed-built-age`). We set `GRYPE_DB_MAX_ALLOWED_BUILT_AGE=336h` so a missed update warns instead of failing scans.
- Update policy: Grype auto-updates on every launch. We pass `GRYPE_DB_AUTO_UPDATE=false` on normal runs and run `grype db update` first only when the last update is older than 24 h (the agent keeps the timestamp). Both tools also get `*_CHECK_FOR_APP_UPDATE=false`.

## JSON fields we read

CycloneDX (Syft): `bomFormat`, `specVersion` (1.7), `metadata.component` (`type: container`, `name`, `version: sha256:…` = image id), `components[]` (`type: library`, `name`, `version`, `purl`, `licenses`). We store it as is, gzip-compressed, and hand it back as the download.

Grype `matches[]`:

| Need | Field | Notes |
|---|---|---|
| id | `vulnerability.id` | `CVE-…`, `GHSA-…`, `GO-…`. Go-module matches are often `GHSA`/`GO` with the CVE under `relatedVulnerabilities[].id`. |
| severity | `vulnerability.severity` | `Critical`, `High`, `Medium`, `Low`, `Negligible`, `Unknown` |
| fix | `vulnerability.fix.versions[]`, `vulnerability.fix.state` | `fixed`, `not-fixed`, `wont-fix`, `unknown` |
| CVSS | `vulnerability.cvss[].metrics.baseScore` (+ `version`, `vector`, `source`) | can be empty (236 of 559 matches, mostly Go and Debian-sourced) → store null; read the max over `vulnerability` and `relatedVulnerabilities` |
| EPSS | `vulnerability.epss[].epss`, `.percentile`, `.date` | present on every `CVE-` match we saw; empty on 39 of 559 matches → null |
| KEV | `vulnerability.knownExploited[]` (`cve`, `dateAdded`, `dueDate`, `knownRansomwareCampaignUse`) | `omitempty`: **absent when not listed**. None of the four images had a KEV hit, so the KEV path is covered by fixtures only (the field set is taken from Grype's `presenter/models/vulnerability.go`). |
| link | `vulnerability.dataSource`, `vulnerability.urls[]` | `dataSource` is the canonical link |
| package | `artifact.name`, `artifact.version`, `artifact.type`, `artifact.purl` | |
| matcher | `matchDetails[].matcher` | not needed |

`grype.json` is large (0.3 MB for 68 matches, 1.2 MB for 328) because of descriptions and URL lists, so the agent returns a **slim finding list**, not Grype's raw output, and only the SBOM is stored whole.

## Cost and shape of a scan

- Typical small/medium image: 5 to 10 s end to end. Large image: 30 to 40 s, dominated by Syft.
- Two containers run one after the other, never in parallel, so the 1 CPU / 1 GB cap is per scan, not summed.
- Disk: the OCI archive is about the image size, so it lives in a per-scan temp dir that is removed on every path (success, failure, cancel).

## DB terms of use (read before a hosted commercial podkit)

- Grype's DB is built by Anchore's open-source pipeline (`vunnel` + `grype-db`, Apache-2.0) and "published by Anchore at no cost to users" from `grype.anchore.io`. We found **no separate terms of use, licence or SLA for the published DB file**, and no statement either way about commercial or high-volume use.
- The content is a merge of public feeds (NVD, GitHub advisories, distro trackers such as Alpine secdb, Debian, Red Hat, Ubuntu; CISA KEV; FIRST EPSS). Their own terms apply: GHSA is CC-BY-4.0, EPSS is free with attribution, KEV is US-government data. NVD asks for an acknowledgement.
- What matters for a hosted podkit:
  1. **Availability and fair use are unguaranteed.** Every managed server pulls 204 MB per update. At fleet scale, run a mirror (`GRYPE_DB_UPDATE_URL` points Grype at your own `latest.json`) instead of hitting Anchore's CDN from every server. This also gives one place to pin and test DB versions.
  2. **Ask Anchore, or have counsel confirm, before reselling scan results as a product feature.** Using the tool is clearly fine; redistribution of the DB itself (a mirror) is the grey area.
  3. **Attribution.** Keep a credit line for NVD, GitHub advisories, FIRST EPSS and CISA KEV in the docs or UI footer.
  4. **Privacy.** Nothing leaves the server except the DB download. Syft and Grype have no telemetry; the app-update check is disabled.
  5. **Staleness is a product risk.** If the DB cannot update for more than 14 days the scan result says so instead of showing a false "clean".
- Trivy was not measured. For the record: its DB comes from `ghcr.io/aquasecurity/trivy-db` and is subject to GHCR rate limits, which is another reason a mirror would be needed there too.

## Consequences for the implementation

- Agent `POST /v1/scan` takes `{image, db_volume?, cpus, memory_mb, syft_image, grype_image, update_db}` and returns `{image_id, sbom, findings[], db_built, tool_versions}`; the rescan variant takes the SBOM and returns findings only.
- Tool images are pinned by the agent (constants), pulled on first use, and configurable via the request.
- Severity mapping: `Negligible` → `low`, `Unknown` → `unknown` (counted, shown as low-key).
- A finding is **Act now** when it is KEV-listed, or EPSS ≥ 0.10 and CVSS ≥ 7.
