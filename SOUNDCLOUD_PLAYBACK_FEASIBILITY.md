# SoundCloud playback feasibility on TrimUI Brick Pro

## Status

The supervised MPlayer path is implemented and packaged. AAC playback has been
confirmed on the physical Brick; MP3-HLS fallback has been decoded with the
StockOS MPlayer under ARM64 QEMU but is still **not device-tested** through the
Brick speaker/headphone output. Windows LIVE remains metadata-only. The ARM64
StockOS build enables playback only when `/usr/trimui/bin/mplayer` is executable.

## Verified SoundCloud path

The official OpenAPI specification exposes
`GET /tracks/{track_urn}/streams` and says that authentication must continue to
be used. SoundCloud's current migration notice says clients should prefer
`hls_aac_160_url`, falling back to `hls_aac_96_url`. SoundCloud later confirmed
that some older uploads remain MP3-only during the migration, so Brickwave
also accepts `hls_mp3_128_url` from its exact CDN host. Progressive MP3 and
Opus remain unsupported. The returned value may itself be an authenticated
SoundCloud API URL before it redirects to a short-lived media URL.

Sources:

- [SoundCloud OpenAPI specification](https://github.com/soundcloud/api/blob/master/openapi/api.yaml)
- [AAC HLS migration announcement](https://github.com/soundcloud/api/issues/441)
- [SoundCloud API explorer](https://developers.soundcloud.com/docs/api/explorer/)
- [SoundCloud API rate limits](https://developers.soundcloud.com/docs/api/rate-limits)
- [SoundCloud API terms](https://developers.soundcloud.com/docs/api/terms-of-use)

Stream requests count against a documented limit of 15,000 play requests per
24 hours per client ID. Playback must preserve uploader/SoundCloud attribution,
must honor the track's availability, and must never save audio as an offline
copy.

## Existing security boundary

The Brick holds only `session_id` and `session_secret`. The Worker keeps the
SoundCloud access/refresh token in its Durable Object (`worker.ts:940`),
refreshes it there (`worker.ts:1068-1095`), and exposes authenticated metadata
through the existing `/auth/*` routes (`worker.ts:1552-1596`). This boundary
must remain intact: a SoundCloud token must not be returned to the Brick or
placed in an MPlayer command line.

## Verified StockOS playback runtime

Static execution under QEMU with the extracted StockOS rootfs confirmed:

- `/usr/trimui/bin/mplayer` is MPlayer 1.5 with internal libavformat.
- `https://` and GnuTLS symbols are present.
- the HLS/Apple HTTP demuxer is present.
- FFmpeg AAC and AAC-LATM decoders are marked working.
- the ALSA output driver is available.

Evidence: `artifacts/stockos-mplayer-capabilities.log`.

The smaller stock `/usr/bin/ffmpeg` is unsuitable as the direct player: it has
HLS and AAC, but its build exposes neither HTTPS/TLS nor an ALSA output device.
Evidence: `artifacts/stockos-playback-capabilities.log`.

## Proposed data and process flow

```text
track click
  -> PlayerController enters Loading
  -> Brick requests a stream descriptor using its opaque Worker session
  -> Worker validates session and playback availability
  -> Worker refreshes OAuth token if necessary
  -> Worker calls /tracks/{urn}/streams and selects AAC 160/96 HLS
  -> Worker follows the authenticated SoundCloud hop
  -> Brick receives only a short-lived, approved HTTPS media URL
  -> supervised stock MPlayer process opens HLS and ALSA
  -> parsed process events update Playing/Paused/Position/Ended/Error
  -> PlayerController advances the existing queue on Ended
```

The client must launch MPlayer with `std::process::Command`, never a shell, and
must validate the URL scheme and exact host before launch. Its stdin slave
channel can carry pause, seek and volume commands after each command is
verified against this MPlayer build. Track changes and app exit must terminate
and reap the child process. Logs must omit the signed media URL and all session
proofs.

## Completed production observation

The existing `brickwave-api` Worker now exposes one authenticated, no-store
descriptor endpoint for a validated track URN. Production version
`ac8a1cd3-f4c0-48ec-92fb-e9e329af08d8` preserves all existing Durable Objects
and secrets and adds no media proxy or paid Cloudflare resource.

The live probe established:

- the saved opaque Brickwave session remains valid through `/auth/me`;
- one playable track returned `hls_aac_160`;
- the final URL used HTTPS, the exact approved host
  `playback.media-streaming.soundcloud.cloud`, and an `.m3u8` path;
- a direct client-to-CDN request returned HTTP 200 and a valid `#EXTM3U`
  manifest (12,718 bytes) without a SoundCloud OAuth header;
- no token, session proof, or signed URL was printed or logged.

This removes the need for a Worker media proxy in the verified flow. A valid
descriptor and manifest are still not evidence that ALSA produced sound.

## Implemented client path

`src/playback_engine.rs` requests no credentials and accepts only a validated
signed `https://playback.media-streaming.soundcloud.cloud/*.m3u8` URL. It
passes that value as one argv item to `/usr/trimui/bin/mplayer`, controls the
child through slave stdin, maps process output to the existing
`PlayerController`, and stops/reaps the child on track change, error, logout or
app exit. Signed URLs and session proofs are never logged.

The ARM64 executable builds with GLIBC 2.33. QEMU tests pass for URL policy,
MPlayer output parsing and the descriptor-to-queue state transition. The
remaining gate is a real Brick test of ALSA output, pause/resume,
next/previous, automatic track-end advance and clean MainUI return.
