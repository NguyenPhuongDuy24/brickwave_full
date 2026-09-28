# Brickwave 0.4.29

Brickwave is a Windows-first SoundCloud client UI being prepared for TrimUI
Brick Pro. Its default application mode is LIVE and requires a valid SoundCloud
login before the main application opens. It has
an isolated metadata worker, a Cloudflare Worker boundary for an official
public-metadata search, and an artwork-cache policy. It contains no SoundCloud
user token or bundled audio decoder. The TrimUI build supervises StockOS
MPlayer for direct AAC-HLS playback with an MP3-HLS fallback; the Windows build
remains metadata-only.
The C4 source adds a shared
Windows/TrimUI QR-pairing UI and a Worker-hosted SoundCloud Authorization Code
+ PKCE flow. The user has completed the real phone OAuth flow successfully.
The current Worker also exposes a fixed-purpose authenticated library route for
liked tracks, owned playlists and liked playlists. Playlist cards now load the
ordered track list through a fixed authenticated Worker route; SoundCloud
tokens remain in the Worker. In LIVE mode, the heart control now updates the
real SoundCloud like state. The save control can add a track to an existing
playlist owned by the connected account. Playlist replacement is refused when
Brickwave cannot verify ownership or obtain the complete current track list.
Library can also create a private playlist and delete a playlist owned by the
connected account. Creation reuses the TrimUI virtual keyboard; deletion is
guarded by an explicit confirmation dialog and a Worker-side ownership check.
Inside an owned playlist, **Remove Tracks** enables an explicit selection mode.
Each row then exposes a large Remove action. Brickwave confirms the selected
title, verifies the exact playlist position and URN on the Worker, and updates
the ordered track list without deleting the track from SoundCloud. Library tabs
and Refresh share one compact row. **New Playlist** is shown only inside the
Playlists view, and every owned playlist card has its own Delete action in the
top-right corner.
The global font scale is 118% for the Brick display.

Settings also provides a **Minimal interface** intended for D-pad use on the
Brick. It keeps the same account, catalog, player, queue, artwork and waveform
state as the full interface, with four large pages: Player, Search, Playlists
and Settings. D-pad moves focus, A selects, B returns to Player, L1/R1 changes
page, START resumes playback and Y pauses. Switching back to the full
interface does not create a second player or reload account data.

The information architecture is inspired by `zxcloli666/SoundCloud-Desktop`.
That external reference repository is not bundled with this source tree; its
license and attribution remain documented in `THIRD_PARTY_NOTICES.md`. This
Rust/egui implementation is new code. Its UI uses a fixed 1024×768 Retro Hi-Fi palette,
native vector icons and custom controls, and is designed to move later to the
existing SDL2 TrimUI host.

## Run on Windows

```powershell
git clone https://github.com/NguyenPhuongDuy24/brickwave_full.git
cd brickwave_full
cargo run --release
```

No mode variable is required: a missing or unknown `SOUNDCLOUD_MODE` selects
LIVE and can never expose fixture data accidentally. To enter the offline UI
development mode explicitly, set `$env:SOUNDCLOUD_MODE = "preview"`.

The window opens at 1024 x 768. The explicit Preview mode includes mock UI flows for Home, Discover,
Search, Library, Likes, playlist details, queue controls, Settings and a
dedicated Now Playing track page. Click the artwork in the persistent player
bar to open Now Playing.

The Now Playing page contains a track hero, a seekable SoundCloud waveform and
the shared queue. Playback controls remain in the persistent bottom player so
they are not duplicated inside the track card. Preview playback remains mock. In Windows
`LIVE` mode, selecting a result updates metadata without pretending to play
audio. On the ARM64 StockOS build, the same selection requests a short-lived
stream descriptor and launches supervised stock MPlayer. Opening a live
playlist keeps its real ordered tracks so Previous/Next and track-end advance
within that context.

On TrimUI, fully downloaded tracks are reused from a bounded session audio
cache. Returning to the same track and transcoding format in one app session
does not download its HLS segments again. The default limit is 256 MiB and 16
tracks. When the limit is reached, ordinary least-recently-used tracks are
removed before tracks in Likes; liked tracks remain bounded and are removed on
logout or app shutdown with the rest of the session cache. The app never
prefetches the full Likes library and never keeps playable audio across app
sessions. `BRICKWAVE_AUDIO_CACHE_MIB` may set a 64–1024 MiB session budget.

Settings includes a sleep timer with separate hours and minutes fields. **Set**
starts a visible countdown; when it expires Brickwave pauses the active player
without resetting the track, queue or position, so START can resume it. The
timer is intentionally session-only. Settings also controls a compact battery
indicator in the top bar. On StockOS it reads the kernel power-supply capacity
at a low polling rate and draws the icon and percentage with the same Retro
Hi-Fi surface, border, radius and orange accent used by Search and Account.
Windows displays an unavailable marker because it does not emulate the Brick
battery.

The StockOS build also provides **Always keep screen on**. It is off by
default. With it off, Brickwave stops the decoder at the configured StockOS
idle timeout, preserves track/queue/position and stops rendering so StockOS
can turn the display off. `KEY_POWER` remains delegated to StockOS and is
never grabbed by Brickwave. Brickwave owns `/tmp/stay_alive` only for its
lifetime so StockOS does not enter the deeper suspend path that can terminate
the app; it does not create `/tmp/stay_awake` in this mode. After StockOS wakes
the display, Brickwave clears the temporary LED effects left by `keymon`, so
the power/button LEDs return to off. START reloads the track and seeks back to
the preserved position. Cleanup also leaves those temporary LED effects off
when returning to MainUI. With the option on, Brickwave keeps playback active,
owns the stay-awake guard, dims through
StockOS brightness IPC and renders at 2 FPS after the same timeout. Both modes
reduce rendering after two seconds without input (10 FPS while playing, 4 FPS
otherwise).

After a StockOS wake, Brickwave keeps the existing catalog, queue, waveform
samples and artwork textures. It waits two seconds for Wi-Fi to settle, clears
only temporary waveform/artwork network failures, and retries account library
or the open playlist only when that data was incomplete. Wake diagnostics use
the URL-free `BRICKWAVE_WAKE_RECOVERY`, `BRICKWAVE_WAVEFORM_WAKE_RECOVERY`,
`BRICKWAVE_ARTWORK_WAKE_RECOVERY` and `BRICKWAVE_DATA_WAKE_RECOVERY` markers.
Backend requests open fresh HTTPS connections so a socket invalidated while
Wi-Fi was asleep is not reused. Transient overlay messages, including
`Playing <track>`, disappear automatically after five seconds.

The transport controls use icon buttons for shuffle, previous, play/pause,
next and repeat. Shuffle keeps one stable order. Repeat cycles through off,
repeat all and repeat one and is shared by preview and StockOS playback.

## Icon policy

`src/icons.rs` is the single icon registry. It embeds only the needed
Phosphor Icons glyphs at compile time through `egui-phosphor`'s subset feature;
the complete icon font is not shipped and there is no SVG runtime or per-frame
texture upload. The licenses and notices are in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

The header also contains an account menu. Preview mode shows Preview Listener;
after a verified LIVE QR login it shows the real SoundCloud profile. Likes and
Library/Playlists load account metadata through the same opaque app session.
Stations, Following and creator tools remain outside this preview.

## Track-page references

- [TrackPage.tsx](https://github.com/zxcloli666/SoundCloud-Desktop/blob/50a71b2da5549510f2e7b6faa5acd4875010849e/desktop/src/pages/TrackPage.tsx)
- [waveform.tsx](https://github.com/zxcloli666/SoundCloud-Desktop/blob/50a71b2da5549510f2e7b6faa5acd4875010849e/desktop/src/components/music/soundwave/waveform.tsx)
- [NowPlayingBar.tsx](https://github.com/zxcloli666/SoundCloud-Desktop/blob/50a71b2da5549510f2e7b6faa5acd4875010849e/desktop/src/components/layout/NowPlayingBar.tsx)

The preview reimplements this structure in Rust/egui. It does not copy the
reference project's React source, artwork or network code.

## Reusable TrimUI virtual keyboard

`crates/trimui-ui-kit` contains the reusable egui keyboard used by Brickwave.
The module owns layout, Shift/Symbol state, D-pad navigation and repeat timing;
Brickwave retains its Search text, submit action and backend. See the
[module README](crates/trimui-ui-kit/README.md) for the integration contract.

On the Brick, select Search or the New Playlist name field with analog + A,
navigate keys with D-pad, activate the highlighted key with A and close the
keyboard with B. While it is open, D-pad scrolling and clicks into the page
behind it are suppressed.

## Verify UI state

```powershell
cargo test
```

Windows and the TrimUI SDL2 host render the same egui views. Platform input is
kept outside `UiState` so the application and keyboard module remain reusable.

Current Brick controls are: analog stick for the virtual pointer, A for
click/drag, D-pad for scrolling and virtual-keyboard navigation, B for Back,
MENU for the exit confirmation, Y/SELECT for Pause, START for Play/Resume, and
L1/R1 for Previous/Next. Pause retains the decoder and current position so
START resumes without loading the track again. Confirming Exit performs the
true Stop before returning to MainUI. While the Exit dialog is open, the
virtual pointer is hidden and frozen: A exits immediately and B cancels.

## Build packages for TrimUI Brick Pro

Build the shared ARM64 binary first:

```bash
export SOUNDCLOUD_BACKEND_URL="https://your-worker.example"
./scripts/build-trimui.sh
```

Then create either package from the repository root:

```bash
python3 scripts/package-stockos.py --output artifacts
python3 scripts/package-nextui.py --output artifacts
```

The StockOS archive contains `Apps/Brickwave`. The NextUI archive contains the
separate `Tools/tg5040/Brickwave.pak` package. Build outputs under `target/` and
`artifacts/` are intentionally excluded from Git.

## NextUI package

The separate NextUI build is a standalone Tool Pak for TrimUI Brick Pro. It is
installed at `Tools/tg5040/Brickwave.pak` and requires NextUI's
`PLATFORM=tg5040`, `DEVICE=brickpro` runtime. The launcher sets
`BRICKWAVE_HOST=nextui`, which prevents Brickwave from calling StockOS
brightness APIs, creating StockOS power guards, or changing LED state. Account
data and artwork cache live under `.userdata`, outside the replaceable Pak.
The StockOS `Apps/Brickwave` package remains separate.

## LIVE Search through Cloudflare Worker

Normal `LIVE` mode calls only the HTTPS metadata Worker. It does **not** read a
SoundCloud client ID, client secret, token-service key, access token, or refresh
token on the PC or future Brick device. The worker source and deployment review
are in [`cloudflare/brickwave-api`](cloudflare/brickwave-api) and
[`WORKER_DEPLOYMENT.md`](cloudflare/brickwave-api/WORKER_DEPLOYMENT.md).

After the Worker deployment has been approved and completed, run one live
search or open the UI as follows:

```powershell
cd <path-to>\brickwave_full

# Required for LIVE mode. Official builds may embed this same value at compile
# time; the repository contains no production Worker hostname.
$env:SOUNDCLOUD_BACKEND_URL = "https://your-worker.example"

# i1.sndcdn.com is already the reviewed built-in artwork CDN host. Waveform
# samples are independently restricted to the exact wave.sndcdn.com host.
# Use this only for additional exact hosts approved later.
# $env:SOUNDCLOUD_ARTWORK_HOSTS = "another-reviewed-host.example"

cargo run -- --live-search "ambient"

# LIVE is the default mode.
cargo run --release
```

`--live-search` prints only `LIVE_SEARCH_OK tracks=N playlists=N` on success. LIVE failures
remain failures; the application never substitutes preview results. The C3.1
production Worker was verified through this command with a real metadata
response; repeat the command after any later Worker change.

In the LIVE window, enter a query and press **Enter** or the adjacent Search
button. The Worker searches the documented SoundCloud track and playlist
collections. Results use two balanced columns: tracks on the left and playlists
on the right. The first response contains at most 12 items per column. Scrolling
near the bottom requests the next opaque cursor page and appends deduplicated
items; there is no fixed 12-result total. Search uses the same `Catalog` and
playlist-opening flow as Library; it does not create a second UI data store.
The search surface shows distinct loading, results, empty and safe error states.
It sends one generation-tagged command at a time, so a late response from an
earlier query cannot overwrite the newer result list. A failed continuation
keeps already displayed results and can be retried by scrolling again. The
Worker returned `i1.sndcdn.com` for verified responses, so that exact host is
the built-in production artwork allowlist. Additional hosts remain disabled
unless independently reviewed and configured in `SOUNDCLOUD_ARTWORK_HOSTS`.
URLs returned by the API never alter this policy.

Artwork loads on a worker and is cached on disk before its texture reaches the
UI. The GPU cache keeps at most 12 textures and 2,500,000 decoded pixels
(about 10 MiB RGBA); decoded uploads are capped to 384 pixels on their longest
edge because the largest Brickwave artwork slot is 190 points. Visible slots
are pinned until the frame finishes, so an off-screen row cannot evict a cover
that is still being painted. For local cache diagnostics only, set
`BRICKWAVE_ARTWORK_DEBUG=1`; it logs event names and short URL hashes, never
the URL itself or credentials.

## C4 QR login, account library, Home and Discover (deployed)

Windows and the future TrimUI host use the same full-screen egui LoginScreen.
LIVE mode is account-gated: without a validated saved session, LoginScreen is
the only available route and the main shell is not rendered. The app asks the
Worker for a 15-minute pairing session and renders its HTTPS pairing URL as a
local black/white QR code. The QR contains only a random pairing ID. A separate
random device secret stays in application memory and authenticates status,
confirmation and cancel requests. Cancel revokes the active pairing but stays
on LoginScreen; there is no Back-to-app action until account access is valid.

The phone page shows a six-digit visual confirmation code and links only to the
official SoundCloud authorization page. The Worker binds the browser to the
pairing with Secure, HttpOnly, SameSite=Lax cookies, uses one-time OAuth state
and PKCE S256, exchanges the authorization code, and keeps SoundCloud access
and refresh tokens in a separate `AuthSessionStore` Durable Object. The client
receives an opaque Worker session proof, never a SoundCloud token. User token
refresh is single-flight per pairing and stores the rotated refresh token before
reuse. Logout invalidates the Worker session and makes a best-effort call to the
documented SoundCloud sign-out endpoint; public metadata Search remains
independent.

The deployed Worker must receive an exact callback URI through its
`AUTH_REDIRECT_URI` binding, and that same URI must be registered for the
SoundCloud application. For example:

```text
https://your-worker.example/auth/callback
```

SoundCloud requires the redirect in authorization and token exchange to match
the registered URI exactly. See the official
[SoundCloud API guide](https://developers.soundcloud.com/docs/api/guide).
Deployment details and rollback information are in
[`WORKER_DEPLOYMENT.md`](cloudflare/brickwave-api/WORKER_DEPLOYMENT.md).

After the first successful QR login, the client stores only the opaque Worker
app-session proof. Windows encrypts it with DPAPI under the current account.
The TrimUI package writes it atomically to `Apps/Brickwave/data/session.json`;
the file uses mode `0600` where the filesystem supports Unix permissions. On
the next launch, the app validates that proof through `/auth/me`, then loads
Home without requiring another QR login. During validation the app remains on
LoginScreen. With no session, or after logout/session expiry, the main shell
remains locked until QR login succeeds. SoundCloud access and refresh tokens
remain in `AuthSessionStore` in the Worker.

Home reads the authenticated `/me/feed/tracks` feed. Discover reads related
tracks for the first valid liked or Home track; when neither exists it shows a
real empty state instead of preview data.

Current C4 status:

- `AUTH_CODE_READY`: source and offline tests pass.
- `REDIRECT_CONFIGURED`: user-confirmed; generated authorization redirect was
  verified against the exact URI.
- `DEPLOYED`: production version `e90572ec-02da-44c9-9b1d-65f5244494cd`.
- `QR_ENDPOINT_PASS`: production pairing, proof, browser binding, PKCE redirect,
  cancel and cleanup checks pass.
- `QR_UI_PASS`: user completed the QR flow in the Windows UI.
- `OAUTH_LIVE_PASS`: user confirmed successful SoundCloud login.
- `ACCOUNT_DATA_CODE_PASS`: Home, Discover, Likes and Playlists mapping and UI
  state tests pass.
- `PLAYLIST_TRACKS_CODE_PASS`: an authenticated playlist opens its ordered
  track list; selecting track 5 then Next selects track 6 without claiming
  audio playback.
- `WINDOWS_SESSION_STORE_PASS`: DPAPI encrypt/decrypt unit test passes; no
  plaintext SoundCloud token is stored by the client.
- `TRIMUI_SESSION_STORE_BUILD_PASS`: atomic save/load, replacement, expiry and
  invalid-file tests pass in the ARM64 binary under QEMU with the StockOS
  rootfs. Physical restore on the Brick remains `DEVICE_NOT_TESTED`.
- `STREAM_DESCRIPTOR_LIVE_PASS`: the Worker returned AAC-HLS for a real
  playable track and the approved CDN returned a valid manifest.
- `MP3_HLS_FALLBACK_PASS`: track `soundcloud:tracks:876666484` returned a
  validated `hls_mp3_128` descriptor and StockOS MPlayer decoded it under
  ARM64 QEMU for five seconds.
- `TRIMUI_MPLAYER_BUILD_PASS`: the supervised player, exact-host validation
  and queue transition compile for ARM64/GLIBC 2.33 and pass under QEMU.
- `TRIMUI_AAC_AUDIO_DEVICE_PASS`: the user confirmed AAC tracks produce sound
  on the physical Brick and the captured log reached `TRACK_PLAYING`.
- `TRIMUI_MP3_FALLBACK_DEVICE_PASS`: the user confirmed that build 00.3.2 plays
  the previously failing MP3-HLS track through the physical Brick output.
- `TRIMUI_SOFTVOL_DEVICE_PASS`: the user verified that software volume in the
  00.3.4 device run increases in the correct direction. Audible stutter remains.
- `TRIMUI_PREBUFFER_DIAGNOSTICS_DEVICE_PARTIAL`: build 00.3.4 raises the MPlayer
  start threshold from 10% to 25% and records startup latency, applied volume
  percentage and playback-position stalls without logging signed media URLs.
  Playback still stuttered, while its two-second detector reported no stall.
- `TRIMUI_ALSA_MICROSTALL_DIAGNOSTICS_BUILD_PASS`: build 00.3.5 enables MPlayer
  diagnostics, records ALSA xrun/buffer/period evidence and reduces position
  polling/stall detection to 250/750 ms. Device evidence is `NOT_TESTED`.
- `ACCOUNT_DATA_LIVE_NOT_VERIFIED`: Home, Discover, Likes and Playlists still
  need one test with the user's real account in the new binary.

The saved app session has the Worker's seven-day TTL. Signing out clears it.
The FAT SD card cannot enforce Unix permissions, so anyone with physical access
to the card can copy the opaque proof until logout or expiry. The file contains
no SoundCloud token, refresh token, client secret or password.

## Explicit local development route

The older service is for local PC development only and rejects every bind
address except `127.0.0.1`. It is never selected by normal `LIVE` mode. It keeps
the SoundCloud client secret and token cache in its own memory; the UI gets a
short-lived lease over loopback and never receives the client secret.

Set these variables in a PowerShell session. Do not commit a `.env` file.

```powershell
$env:SOUNDCLOUD_CLIENT_ID = "your-client-id"
$env:SOUNDCLOUD_CLIENT_SECRET = "your-client-secret"
$env:SOUNDCLOUD_TOKEN_SERVICE_KEY = "a-long-random-local-key"
# Optional: append only additional reviewed CDN hosts.
# i1.sndcdn.com is already enabled by the production build.
# $env:SOUNDCLOUD_ARTWORK_HOSTS = "another-reviewed-host.example"
```

Start the service in one terminal:

```powershell
cargo run -- --token-service
```

Then run one end-to-end live search in another terminal:

```powershell
cargo run -- --local-live-search "ambient"
```

To open the UI through this local development service, keep the service running
and use:

```powershell
$env:SOUNDCLOUD_MODE = "local"
cargo run
```

`SOUNDCLOUD_TOKEN_SERVICE_URL` defaults to `http://127.0.0.1:8787`; it may be
set only to that loopback host for this build. `SOUNDCLOUD_REDIRECT_URI` is
optional and reserved for a future authorization-code flow. Artwork hosts are
an independent exact allowlist: API responses cannot add a host or enable a
download. Leaving `SOUNDCLOUD_ARTWORK_HOSTS` empty leaves artwork as placeholders.
