# Brickwave API deployment review

## Status

### QR scanner prefetch fix — 2026-09-25

Production `brickwave-api` is currently running version
`a992076b-b83d-4ede-b819-187b31b7aa43`. This version allows the real phone
browser to replace a provisional QR-preview cookie before OAuth starts, uses a
POST form for the explicit Continue action, and keeps the browser binding
strict after OAuth state has been created.

The exact pre-change production version is preserved by Cloudflare as
`870222a5-4db1-45d4-bb0b-a544105680d7` and remains the approved rollback
target. Rollback does not delete or migrate Durable Object storage and does not
change either production secret:

```powershell
cd D:\tele\soundcloud\cloudflare\brickwave-api
npx wrangler rollback 870222a5-4db1-45d4-bb0b-a544105680d7 `
  --name brickwave-api `
  --message "Rollback QR scanner prefetch fix" `
  --yes
```

Post-deployment checks passed for `/health`, a fresh pairing, a QR-preview
request followed by a separate real-browser request, POST `/auth/authorize`
redirecting to the official SoundCloud authorization host with PKCE/state, and
pairing cleanup. `/search?q=ambient` returned HTTP 200 on the single retry. No
binding, migration, secret, plan or paid service changed.

The authenticated AAC/MP3-HLS descriptor update in this directory was deployed
to the existing production Worker `brickwave-api` on 2026-09-23. The active
version is `e90572ec-02da-44c9-9b1d-65f5244494cd`; the AAC-only rollback is
`ac8a1cd3-f4c0-48ec-92fb-e9e329af08d8`; the first descriptor probe
version was `db9a56d3-e680-4822-9124-d51a36e66b6b`; the last pre-playback
version is `25844659-48a3-4973-a99e-e989d28c88f7`; the prior combined search
version is `404ae296-c52b-4246-ae07-16c340919d0a`; the prior authenticated
playlist-tracks version is `65e623db-5408-4c31-a8c4-af66d2132b7d`; the prior
playlist-tracks version is `f074bd66-51ea-48ad-802c-d179f41628c4`; the prior C4/URN version is
`c7bb3a81-9b35-4a7d-abd3-56787d214f7d`; the immediate pre-URN version is
`6e0688bd-e478-4d21-8b4a-01ba3821e6c7`, the earlier Likes/Playlists version is
`74f4b116-75f5-4961-89b5-b1cce89b02b1`, and the verified QR-only version is
`11a023ac-f82e-4aa5-bbb5-d4a51a27db95`. It passed TypeScript
checking, 36 offline Worker tests and `wrangler deploy --dry-run` before
deployment.

The production Worker exposes:

- `GET /health` - non-sensitive service status.
- `GET /search?q=...` - first public track and playlist metadata page, capped
  at 12 mapped items for each resource type and 512 KiB for the mapped response.
- `GET /search?q=...&cursor=...` - the next bounded page. The opaque cursor can
  contain only validated `https://api.soundcloud.com/tracks` and `/playlists`
  continuation URLs for the same query. It cannot become a general proxy.
- `GET /auth/tracks/{track_urn}/stream` - a no-store, authenticated descriptor
  for one validated `soundcloud:tracks:<digits>` resource. It returns a signed
  AAC-HLS or MP3-HLS media URL on its format-bound exact approved SoundCloud
  media host.

It also exposes the fixed-purpose QR OAuth routes listed below. It has no
general `/token` endpoint, arbitrary proxy, audio proxy or CORS credential
sharing.

## Authenticated stream descriptor

The descriptor route requires the same opaque Brickwave app session as
`/auth/me`; the SoundCloud access and refresh tokens remain in
`AuthSessionStore`. The Worker selects `hls_aac_160_url`, then
`hls_aac_96_url`, then `hls_mp3_128_url`. It never substitutes the 30-second
`preview_mp3_128_url`. OAuth is sent only to `api.soundcloud.com`. Redirect
handling is manual, so the OAuth header is never forwarded to the media CDN.

Returned media URLs must use HTTPS, contain no URL credentials and end in
`.m3u8`. AAC formats are bound to
`playback.media-streaming.soundcloud.cloud`; MP3-HLS is bound to
`cf-hls-media.sndcdn.com`. The response is `Cache-Control: no-store` and the
route uses the existing `RequestGuard`. If SoundCloud ever requires an
authenticated manifest or audio proxy, the Worker returns
`stream_proxy_required` and does not proxy bytes.

The production probe used the existing encrypted Brickwave session and one
playable URN obtained from live search. Results:

- saved session validation through `/auth/me`: HTTP 200;
- descriptor: HTTP 200, `hls_aac_160`, approved host and `.m3u8` path;
- direct client-to-CDN manifest request: HTTP 200, valid `#EXTM3U`, 12,718
  bytes;
- `/health`: HTTP 200;
- `/search?q=ambient`: HTTP 200 with 12 tracks and 12 playlists;
- pairing start/status/cancel smoke test: HTTP 200/200/200;
- unauthenticated descriptor request: HTTP 401.

The MP3 fallback production probe used track `soundcloud:tracks:876666484`,
which previously returned `stream_format_unavailable`. The updated route
returned HTTP 200 with `hls_mp3_128` on the exact approved MP3 CDN. The signed
manifest returned HTTP 200 and valid `#EXTM3U`. StockOS MPlayer under ARM64
QEMU detected HLS, selected `mpg123`, decoded 44.1 kHz stereo MP3 and advanced
past five seconds with `-ao null`. Speaker output remains a physical-device
test.

No audio data passed through the Worker, no secret value or signed media URL
was logged, and no Cloudflare binding, migration, paid service or plan was
added.

## C4 deployed scope

The deployed source adds these fixed-purpose routes:

- `POST /auth/device/start`
- `GET /auth/device/status?pairing_id=...`
- `POST /auth/device/complete`
- `POST /auth/device/cancel`
- `GET /auth/connect?pairing=...`
- `GET /auth/authorize`
- `GET /auth/callback`
- `GET /auth/me`
- `GET /auth/library`
- `GET /auth/playlists/{playlist_urn}/tracks`
- `GET /auth/tracks/{track_urn}/stream`
- `POST|DELETE /auth/tracks/{track_urn}/like`
- `POST /auth/playlists/{playlist_urn}/tracks`
- `POST /auth/logout`

It does not add a general token endpoint or arbitrary proxy. SoundCloud user
tokens remain inside the Worker. Device endpoints require an independent
device proof; `/auth/me`, `/auth/library`, the playlist-tracks route and
`/auth/logout` require an opaque app-session ID and secret. The playlist route
accepts only a validated `soundcloud:playlists:<digits>` URN, follows bounded
pagination on the same official SoundCloud resource and maps at most 100
tracks. `/auth/library` calls the documented account
collection/feed endpoints plus the related-tracks endpoint for one bounded
Discover seed, maps bounded metadata and never returns a SoundCloud token. QR
URLs contain neither proof nor any OAuth token.

The C4 config keeps the existing `TokenCoordinator` and `RequestGuard` bindings
and adds a separate SQLite Durable Object binding:

```text
AUTH_SESSION_STORE -> AuthSessionStore
migration tag: v2-user-auth-sessions
```

This object persists pairing state, PKCE verifier, user token rotation, profile
and app-session proofs. It does not share storage with the client-credentials
token coordinator. The user approved this persistent namespace and migration;
it was created without changing either production secret or any other resource.

The callback is supplied at deployment through the `AUTH_REDIRECT_URI` binding.
The repository contains no production Worker hostname. Its required shape is:

```text
https://your-worker.example/auth/callback
```

The deployed value must exactly match the redirect URI registered for the
SoundCloud app. Production endpoint tests verified that `/auth/authorize`
generates the configured URI exactly. The user completed the phone authorization and
confirmed that the real account loaded, so `OAUTH_LIVE_PASS` is confirmed. The
new account-library route still needs a manual run of the rebuilt UI.

## What deployment changes

`wrangler.jsonc` targets the existing Worker name `brickwave-api`. The deployed
C3.1 migration tag `v1` introduced two SQLite-backed Durable Object classes:

- `TokenCoordinator` stores and serializes the short-lived client-credentials
  token and its rotated refresh token.
- `RequestGuard` applies a fixed 20-search-per-minute limit for each hashed
  caller address.

The applied C4 `v2-user-auth-sessions` migration added only `AuthSessionStore`.
Neither migration changes or prints the existing `SOUNDCLOUD_CLIENT_ID` or
`SOUNDCLOUD_CLIENT_SECRET` values. Do not deploy to a different Cloudflare
account or Worker name by accident.

## C4 deployment checks completed

1. Wrangler selected the existing account that owns `brickwave-api`.
2. The user confirmed the exact SoundCloud callback URI is registered.
3. The user approved creation of `AuthSessionStore`; the existing
   `TokenCoordinator` and `RequestGuard` objects were retained.
4. Both production secret **names** still exist. Their values were not printed,
   replaced or written to this repository:

   ```powershell
   cd D:\tele\soundcloud\cloudflare\brickwave-api
   npx wrangler whoami
   npx wrangler secret list
   ```

   The expected names are `SOUNDCLOUD_CLIENT_ID`,
   `SOUNDCLOUD_CLIENT_SECRET` and `AUTH_REDIRECT_URI`. The redirect is not a
   credential, but keeping it in deployment configuration avoids publishing the
   production Worker hostname.

5. `new_sqlite_classes` is compatible with Workers Free and Paid plans. No plan
   change, KV namespace, R2 bucket, D1 database or paid add-on was created.

For a future public rollout, an account-level Cloudflare rate-limit or WAF rule
can protect `/search` as a second layer. That optional resource was outside C4
and was not enabled.

## Future redeployment commands

Run these only after reviewing the future source change:

```powershell
cd D:\tele\soundcloud\cloudflare\brickwave-api
npm ci
npm run check
npx wrangler deploy

$env:BRICKWAVE_WORKER_URL = "https://your-worker.example"
Invoke-WebRequest "$env:BRICKWAVE_WORKER_URL/health" |
  Select-Object -ExpandProperty Content
```

The expected health response is JSON containing `status: "ok"` and
`service: "brickwave-api"`. Then run the client validation without any
SoundCloud credential variables on the PC:

```powershell
cd D:\tele\soundcloud
cargo run -- --live-search "ambient"
```

Only `LIVE_SEARCH_OK tracks=N playlists=N` proves that the deployed Worker, its secrets,
the upstream token lease, the official SoundCloud search request, and the Rust
mapping all worked together. A successful `/health` response alone does not.

Validate QR OAuth manually with the same Windows component intended for TrimUI:

```powershell
cd D:\tele\soundcloud
$env:SOUNDCLOUD_MODE = "live"
cargo run
```

Open **Account -> Connect SoundCloud**, scan the QR with a phone, verify the
same six-digit code on both screens, continue to the official SoundCloud page,
then confirm the device in Brickwave. Success requires the real username/avatar
to load through `/auth/me`. Do not treat a rendered QR, a successful callback,
or unit tests alone as `OAUTH_LIVE_PASS`.

## Rollback

The immediate AAC-only rollback is
`ac8a1cd3-f4c0-48ec-92fb-e9e329af08d8`; the version before the stream
descriptor is `25844659-48a3-4973-a99e-e989d28c88f7`; the older combined-search rollback is
`404ae296-c52b-4246-ae07-16c340919d0a`;
the older QR-only version is `11a023ac-f82e-4aa5-bbb5-d4a51a27db95`.
If the account-data update fails, roll back in the Cloudflare dashboard. Do not
delete Durable Object storage merely to roll back code: it can contain the
cached service token, pending pairing sessions and rotated user refresh tokens.
Rolling back C4 code does not require deleting the new namespace.

## Brickwave 0.4.5 account mutations

The existing Worker was updated in place without adding a Durable Object,
migration, storage product or paid resource. Like/unlike uses the documented
track-like endpoint. Adding a track is allowed only for a playlist whose owner
matches the authenticated profile. Before sending SoundCloud's full playlist
replacement payload, the Worker obtains the complete current track list and
refuses an incomplete, oversized or ambiguous result. The device continues to
send only its opaque Brickwave session; SoundCloud tokens remain inside
`AuthSessionStore`.

Deployment `8f59413f-3c28-494d-b543-fbdc3f4b924f` passed `/health` and live
`/search?q=ambient`. Unauthenticated requests to both new mutation routes
returned HTTP 401. Unit tests cover the authenticated upstream mutations;
production account content was not changed as part of deployment validation.

## Configuration consumed by the Rust client

The normal application uses:

- `SOUNDCLOUD_MODE=live` to enable Worker-backed search.
- `SOUNDCLOUD_BACKEND_URL` is required at runtime or compile time and must be an
  HTTPS Worker base URL. There is no built-in production hostname in source.
- `SOUNDCLOUD_ARTWORK_HOSTS` optionally, as a comma-separated exact allowlist
  such as `i1.sndcdn.com`. It never accepts a host merely because an API result
  contains it.

No SoundCloud secret, access token, refresh token, or Cloudflare API token is
read by normal LIVE mode.

## Brickwave 0.4.6 playlist management

The existing Worker was updated in place without a new Durable Object,
migration, storage product or paid resource. `POST /auth/playlists` creates a
private playlist using SoundCloud's documented JSON request. Deletion uses
`DELETE /auth/playlists/{playlist_urn}` only after the Worker reads the
playlist and verifies that its owner matches the authenticated profile.

Deployment `0dbee0d1-3d45-4c17-8c63-d81b4134921d` passed `/health`, live
`/search?q=ambient` (12 tracks and 12 playlists), and unauthenticated guards
for both management routes (HTTP 401). Tests cover authenticated create,
ownership verification, delete, invalid names and token secrecy. Production
account content was not changed during deployment validation.

## Brickwave 0.4.7 playlist track removal

`DELETE /auth/playlists/{playlist_urn}/tracks` accepts an exact track URN and
zero-based playlist position through the opaque Brickwave session. The Worker
loads the complete owned playlist, rejects stale or mismatched positions, then
uses SoundCloud's documented playlist `PUT` operation with the remaining
ordered track list. Removing an entry does not delete the SoundCloud track.

The route reuses `AuthSessionStore`, `RequestGuard`, the existing secrets and
the existing playlist-size/pagination limits. It adds no Durable Object,
migration or paid Cloudflare resource.

Deployment `870222a5-4db1-45d4-bb0b-a544105680d7` passed `/health`, live
search (12 tracks and 12 playlists), and the unauthenticated removal guard
(HTTP 401). Production account content was not changed during smoke testing.
