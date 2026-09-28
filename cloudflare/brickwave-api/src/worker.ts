export interface Env {
  SOUNDCLOUD_CLIENT_ID: string;
  SOUNDCLOUD_CLIENT_SECRET: string;
  AUTH_REDIRECT_URI: string;
  TOKEN_COORDINATOR: DurableObjectNamespace;
  REQUEST_GUARD: DurableObjectNamespace;
  /** A separate DO namespace for user OAuth sessions. It never shares data
   * with the Client Credentials TokenCoordinator. */
  AUTH_SESSION_STORE: DurableObjectNamespace;
}

type StoredToken = {
  accessToken: string;
  refreshToken?: string;
  expiresAtMs: number;
};

type IssuedToken = {
  accessToken: string;
  refreshToken?: string;
  expiresInSeconds: number;
};

type UserProfile = {
  id?: number;
  urn?: string;
  username: string;
  displayName?: string;
  avatarUrl?: string;
};

type UserToken = {
  accessToken: string;
  refreshToken?: string;
  expiresAtMs: number;
};

type DevicePhase =
  | "waiting_for_scan"
  | "waiting_for_authorization"
  | "authorization_complete"
  | "authorized"
  | "cancelled";

type PairingSession = {
  pairingId: string;
  callerKey: string;
  deviceSecretHash: string;
  confirmationCode: string;
  phase: DevicePhase;
  createdAtMs: number;
  expiresAtMs: number;
  nextPollAtMs: number;
  browserNonceHash?: string;
  oauthState?: string;
  oauthConsumed?: boolean;
  pkceVerifier?: string;
  userToken?: UserToken;
  profile?: UserProfile;
  sessionId?: string;
  sessionSecretHash?: string;
  sessionExpiresAtMs?: number;
  confirmationAttempts: number;
};

type WorkerTrack = {
  id?: number;
  urn?: string;
  title: string;
  metadata_artist?: string;
  user?: { id?: number; username?: string };
  duration?: number;
  artwork_url?: string | null;
  waveform_url?: string | null;
  access?: "playable" | "preview" | "blocked" | string;
  genre?: string | null;
};

type WorkerPlaylist = {
  id?: number;
  urn?: string;
  title: string;
  description?: string | null;
  artwork_url?: string | null;
  track_count?: number;
  user?: { id?: number; urn?: string };
};

export type PublicTrack = {
  id?: number;
  urn?: string;
  title: string;
  metadata_artist: string;
  user: { id: number; username: string };
  duration: number;
  artwork_url?: string;
  waveform_url?: string;
  access: "playable" | "preview" | "blocked";
  genre?: string;
};

export type PublicPlaylist = {
  id?: number;
  urn?: string;
  title: string;
  description?: string;
  artwork_url?: string;
  track_count: number;
  editable?: boolean;
};

type PublicStreamDescriptor = {
  track_urn: string;
  format: StreamFormat;
  media_url: string;
};

type StreamFormat = "hls_aac_160" | "hls_aac_96" | "hls_mp3_128";

const SOUNDCLOUD_TRACKS_API = "https://api.soundcloud.com/tracks";
const SOUNDCLOUD_PLAYLISTS_API = "https://api.soundcloud.com/playlists";
const SOUNDCLOUD_TOKEN = "https://secure.soundcloud.com/oauth/token";
const MAX_QUERY_LENGTH = 80;
const MAX_RESULTS = 12;
const SEARCH_PAGE_SIZE = 12;
const MAX_SEARCH_CURSOR_LENGTH = 4096;
const MAX_LIBRARY_TRACKS = 50;
const MAX_LIBRARY_PLAYLISTS = 30;
const MAX_PLAYLIST_TRACKS = 100;
const MAX_PLAYLIST_PAGES = 4;
const MAX_EDITABLE_PLAYLIST_TRACKS = 500;
const MAX_EDITABLE_PLAYLIST_PAGES = 10;
const MAX_PLAYLIST_TITLE_LENGTH = 100;
const MAX_UPSTREAM_BYTES = 1_000_000;
const MAX_RESPONSE_BYTES = 512_000;
const MAX_STREAM_DESCRIPTOR_BYTES = 32_000;
const TOKEN_SKEW_MS = 60_000;
const UPSTREAM_TIMEOUT_MS = 8_000;
const RATE_WINDOW_MS = 60_000;
const RATE_MAX_REQUESTS = 20;
const AUTH_PENDING_TTL_MS = 15 * 60_000;
const AUTH_SESSION_TTL_MS = 7 * 24 * 60 * 60_000;
const AUTH_POLL_INTERVAL_MS = 5_000;
const AUTH_MAX_PENDING_SESSIONS = 64;
const AUTH_MAX_PENDING_PER_CALLER = 3;
const AUTH_MAX_CONFIRMATION_ATTEMPTS = 5;
const APPROVED_STREAM_HOSTS: Readonly<Record<StreamFormat, string>> = {
  hls_aac_160: "playback.media-streaming.soundcloud.cloud",
  hls_aac_96: "playback.media-streaming.soundcloud.cloud",
  hls_mp3_128: "cf-hls-media.sndcdn.com",
};

class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

function authRedirectUri(env: Env): string {
  const configured = env.AUTH_REDIRECT_URI?.trim();
  if (!configured) {
    throw new ApiError(500, "worker_misconfigured", "OAuth redirect is not configured");
  }
  try {
    const url = new URL(configured);
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      url.port ||
      url.pathname !== "/auth/callback" ||
      url.search ||
      url.hash
    ) {
      throw new Error("invalid redirect");
    }
    return url.toString();
  } catch {
    throw new ApiError(500, "worker_misconfigured", "OAuth redirect configuration is invalid");
  }
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

function errorResponse(error: unknown): Response {
  if (error instanceof ApiError) {
    return json({ error: { code: error.code, message: error.message } }, error.status);
  }
  return json(
    { error: { code: "backend_unavailable", message: "Metadata backend is unavailable" } },
    502,
  );
}

export function validateSearchQuery(value: string | null): string {
  const query = value?.trim() ?? "";
  if (!query) {
    throw new ApiError(400, "invalid_query", "Query is required");
  }
  if (Array.from(query).length > MAX_QUERY_LENGTH) {
    throw new ApiError(400, "invalid_query", "Query is too long");
  }
  return query;
}

function clampText(value: unknown, maxLength: number): string {
  return typeof value === "string" ? Array.from(value).slice(0, maxLength).join("") : "";
}

function validArtworkUrl(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.port ? url.toString() : undefined;
  } catch {
    return undefined;
  }
}

function validWaveformUrl(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      url.hostname === "wave.sndcdn.com" &&
      !url.username && !url.password && !url.port
      ? url.toString()
      : undefined;
  } catch {
    return undefined;
  }
}

function validResourceUrn(value: unknown, kind: "tracks" | "playlists" | "users"): string | undefined {
  if (typeof value !== "string" || value.length > 96) return undefined;
  return new RegExp(`^soundcloud:${kind}:[1-9][0-9]*$`).test(value) ? value : undefined;
}

function validatedStreamMediaUrl(raw: unknown, format: StreamFormat): URL | undefined {
  if (typeof raw !== "string" || raw.length > 8192) return undefined;
  try {
    const url = new URL(raw);
    if (
      url.protocol !== "https:" ||
      url.hostname !== APPROVED_STREAM_HOSTS[format] ||
      url.port ||
      url.username ||
      url.password ||
      url.hash ||
      !url.pathname.endsWith(".m3u8")
    ) return undefined;
    return url;
  } catch {
    return undefined;
  }
}

function validatedStreamApiHop(raw: unknown, trackUrn: string): URL | undefined {
  if (typeof raw !== "string" || raw.length > 4096) return undefined;
  try {
    const url = new URL(raw);
    const path = url.pathname.split("/").filter(Boolean);
    const pathUrn = path.length === 5 ? decodeURIComponent(path[1]) : undefined;
    if (
      url.protocol !== "https:" ||
      url.hostname !== "api.soundcloud.com" ||
      url.port ||
      url.username ||
      url.password ||
      url.hash ||
      url.search ||
      path[0] !== "tracks" ||
      pathUrn !== trackUrn ||
      path[2] !== "streams" ||
      !/^[A-Za-z0-9_-]{8,200}$/.test(path[3] ?? "") ||
      path[4] !== "hls"
    ) return undefined;
    return url;
  } catch {
    return undefined;
  }
}

function validatedMetadataRedirect(raw: string | null, original: URL): URL | undefined {
  if (!raw) return undefined;
  try {
    const url = new URL(raw, original);
    const normalizedOriginalPath = original.pathname.replace(/\/$/, "");
    const normalizedRedirectPath = url.pathname.replace(/\/$/, "");
    if (
      url.protocol !== "https:" ||
      url.hostname !== "api.soundcloud.com" ||
      url.port ||
      url.username ||
      url.password ||
      url.hash ||
      url.search ||
      normalizedRedirectPath !== normalizedOriginalPath
    ) return undefined;
    return url;
  } catch {
    return undefined;
  }
}

function streamUpstreamError(response: Response): ApiError {
  const status = response.status === 401 || response.status === 403 || response.status === 404 || response.status === 429
    ? response.status
    : 502;
  const code = status === 401
    ? "upstream_unauthorized"
    : status === 403
      ? "stream_forbidden"
      : status === 404
        ? "stream_not_found"
        : status === 429
          ? "upstream_rate_limited"
          : "stream_unavailable";
  return new ApiError(status, code, "SoundCloud stream is unavailable");
}

async function fetchStreamDescriptor(accessToken: string, trackUrn: string): Promise<PublicStreamDescriptor> {
  if (!validResourceUrn(trackUrn, "tracks")) {
    throw new ApiError(400, "invalid_track", "Track URN is invalid");
  }
  const streamsUrl = new URL(`https://api.soundcloud.com/tracks/${encodeURIComponent(trackUrn)}/streams`);
  const headers = {
    accept: "application/json; charset=utf-8",
    authorization: `OAuth ${accessToken}`,
  };
  let response = await fetchWithTimeout(streamsUrl.toString(), { headers, redirect: "manual" });
  if (response.status >= 300 && response.status <= 399) {
    const redirect = validatedMetadataRedirect(response.headers.get("location"), streamsUrl);
    if (!redirect) {
      throw new ApiError(502, "stream_metadata_redirect_rejected", "SoundCloud redirected stream metadata outside the approved API resource");
    }
    response = await fetchWithTimeout(redirect.toString(), { headers, redirect: "error" });
  }
  if (!response.ok) throw streamUpstreamError(response);
  const bytes = await readBoundedBody(response, MAX_STREAM_DESCRIPTOR_BYTES);
  let payload: Record<string, unknown>;
  try {
    payload = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
  } catch {
    throw new ApiError(502, "stream_response_invalid", "SoundCloud returned invalid stream metadata");
  }
  const format: StreamFormat | undefined = typeof payload.hls_aac_160_url === "string"
    ? "hls_aac_160"
    : typeof payload.hls_aac_96_url === "string"
      ? "hls_aac_96"
      : typeof payload.hls_mp3_128_url === "string"
        ? "hls_mp3_128"
        : undefined;
  if (!format) {
    throw new ApiError(409, "stream_format_unavailable", "No supported HLS stream is available for this track");
  }
  const candidate = payload[`${format}_url`];
  const direct = validatedStreamMediaUrl(candidate, format);
  if (direct) return { track_urn: trackUrn, format, media_url: direct.toString() };

  const apiHop = validatedStreamApiHop(candidate, trackUrn);
  if (!apiHop) {
    throw new ApiError(502, "stream_url_rejected", "SoundCloud returned an unapproved stream location");
  }
  // Keep OAuth on api.soundcloud.com. Manual redirect handling prevents the
  // authorization header from ever being forwarded to a media CDN.
  const hop = await fetchWithTimeout(apiHop.toString(), { headers, redirect: "manual" });
  if (hop.status >= 300 && hop.status <= 399) {
    const location = hop.headers.get("location");
    let resolved: string | undefined;
    try {
      resolved = location ? new URL(location, apiHop).toString() : undefined;
    } catch {
      resolved = undefined;
    }
    const media = validatedStreamMediaUrl(resolved, format);
    if (!media) {
      throw new ApiError(502, "stream_url_rejected", "SoundCloud redirected to an unapproved stream location");
    }
    return { track_urn: trackUrn, format, media_url: media.toString() };
  }
  if (!hop.ok) throw streamUpstreamError(hop);
  // A successful body that still needs OAuth would require Brickwave to proxy
  // manifests or audio. That is deliberately outside this endpoint.
  throw new ApiError(409, "stream_proxy_required", "This stream cannot be handed off without an authenticated proxy");
}

function safeResourceId(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

export function mapTrack(track: WorkerTrack): PublicTrack | undefined {
  const id = safeResourceId(track.id);
  const urn = validResourceUrn(track.urn, "tracks");
  if (!id && !urn) return undefined;
  const title = clampText(track.title, 240);
  if (!title) return undefined;
  const access = track.access === "playable" || track.access === "preview" ? track.access : "blocked";
  const artist = clampText(track.metadata_artist, 160);
  const username = clampText(track.user?.username, 160);
  const userId = Number.isSafeInteger(track.user?.id) && track.user!.id! > 0 ? track.user!.id! : 0;
  const artworkUrl = validArtworkUrl(track.artwork_url);
  const waveformUrl = validWaveformUrl(track.waveform_url);
  const genre = clampText(track.genre, 100);
  const result: PublicTrack = {
    title,
    metadata_artist: artist,
    user: { id: userId, username },
    duration: Number.isFinite(track.duration) && track.duration! > 0 ? Math.floor(track.duration!) : 0,
    access,
  };
  if (id) result.id = id;
  if (urn) result.urn = urn;
  if (artworkUrl) result.artwork_url = artworkUrl;
  if (waveformUrl) result.waveform_url = waveformUrl;
  if (genre) result.genre = genre;
  return result;
}

export function mapPlaylist(playlist: WorkerPlaylist, editable = false): PublicPlaylist | undefined {
  const id = safeResourceId(playlist.id);
  const urn = validResourceUrn(playlist.urn, "playlists");
  if (!id && !urn) return undefined;
  const title = clampText(playlist.title, 240);
  if (!title) return undefined;
  const description = clampText(playlist.description, 600);
  const artworkUrl = validArtworkUrl(playlist.artwork_url);
  const result: PublicPlaylist = {
    title,
    track_count:
      Number.isFinite(playlist.track_count) && playlist.track_count! >= 0
        ? Math.floor(playlist.track_count!)
        : 0,
  };
  if (id) result.id = id;
  if (urn) result.urn = urn;
  if (description) result.description = description;
  if (artworkUrl) result.artwork_url = artworkUrl;
  if (editable) result.editable = true;
  return result;
}

function tokenIsFresh(token: StoredToken): boolean {
  return token.expiresAtMs - Date.now() > TOKEN_SKEW_MS;
}

function basicAuthorization(clientId: string, clientSecret: string): string {
  return `Basic ${btoa(`${clientId}:${clientSecret}`)}`;
}

function base64UrlRandom(byteLength: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(byteLength));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

async function sha256Base64Url(value: string): Promise<string> {
  const bytes = new TextEncoder().encode(value);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  let binary = "";
  for (const byte of new Uint8Array(digest)) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

/** PKCE S256 challenge. The verifier stays in Durable Object storage only. */
export async function pkceChallenge(verifier: string): Promise<string> {
  return sha256Base64Url(verifier);
}

function validOpaqueId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{20,128}$/.test(value);
}

function parseBearer(request: Request): string | undefined {
  const value = request.headers.get("authorization");
  if (!value?.startsWith("Bearer ")) return undefined;
  const secret = value.slice("Bearer ".length);
  return validOpaqueId(secret) ? secret : undefined;
}

function safeEqual(left: string | undefined, right: string | undefined): boolean {
  // Values compared here are SHA-256 digests of 256-bit random secrets.
  // A constant-length XOR avoids an accidental early-exit comparison.
  if (!left || !right || left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) difference |= left.charCodeAt(index) ^ right.charCodeAt(index);
  return difference === 0;
}

function authNoStoreHeaders(extra: HeadersInit = {}): Headers {
  const headers = new Headers(extra);
  headers.set("cache-control", "no-store");
  headers.set("x-content-type-options", "nosniff");
  headers.set("referrer-policy", "no-referrer");
  return headers;
}

async function fetchWithTimeout(input: RequestInfo, init: RequestInit): Promise<Response> {
  try {
    return await fetch(input, { ...init, signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS) });
  } catch (error) {
    if (error instanceof DOMException && error.name === "TimeoutError") {
      throw new ApiError(504, "upstream_timeout", "SoundCloud did not respond in time");
    }
    throw new ApiError(502, "upstream_unavailable", "SoundCloud metadata is unavailable");
  }
}

async function parseToken(response: Response): Promise<IssuedToken> {
  if (!response.ok) {
    throw new ApiError(response.status === 429 ? 429 : 502, "token_exchange_failed", "Token service rejected the request");
  }
  const payload = (await response.json()) as Record<string, unknown>;
  const accessToken = clampText(payload.access_token, 4096);
  const refreshToken = clampText(payload.refresh_token, 4096) || undefined;
  const expiresIn = Number(payload.expires_in);
  if (!accessToken || !Number.isFinite(expiresIn) || expiresIn <= 0) {
    throw new ApiError(502, "token_exchange_failed", "Token service returned an invalid response");
  }
  return { accessToken, refreshToken, expiresInSeconds: Math.floor(expiresIn) };
}

async function exchangeClientCredentials(env: Env): Promise<IssuedToken> {
  const body = new URLSearchParams({ grant_type: "client_credentials" });
  const response = await fetchWithTimeout(SOUNDCLOUD_TOKEN, {
    method: "POST",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/x-www-form-urlencoded",
      authorization: basicAuthorization(env.SOUNDCLOUD_CLIENT_ID, env.SOUNDCLOUD_CLIENT_SECRET),
    },
    body,
  });
  return parseToken(response);
}

async function exchangeRefresh(env: Env, refreshToken: string): Promise<IssuedToken> {
  const body = new URLSearchParams({
    grant_type: "refresh_token",
    // SoundCloud documents refresh credentials in the form body. This is an
    // internal Worker-to-SoundCloud request only; it is never logged or sent
    // to the public client.
    client_id: env.SOUNDCLOUD_CLIENT_ID,
    client_secret: env.SOUNDCLOUD_CLIENT_SECRET,
    refresh_token: refreshToken,
  });
  const response = await fetchWithTimeout(SOUNDCLOUD_TOKEN, {
    method: "POST",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
  return parseToken(response);
}

async function exchangeAuthorizationCode(env: Env, code: string, verifier: string): Promise<IssuedToken> {
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    client_id: env.SOUNDCLOUD_CLIENT_ID,
    client_secret: env.SOUNDCLOUD_CLIENT_SECRET,
    redirect_uri: authRedirectUri(env),
    code_verifier: verifier,
    code,
  });
  const response = await fetchWithTimeout(SOUNDCLOUD_TOKEN, {
    method: "POST",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
  return parseToken(response);
}

function asUserProfile(value: unknown): UserProfile {
  const payload = value as Record<string, unknown>;
  const id = safeResourceId(payload.id);
  const urn = validResourceUrn(payload.urn, "users");
  const username = clampText(payload.username, 160);
  if ((!id && !urn) || !username) {
    throw new ApiError(502, "profile_invalid", "SoundCloud returned an invalid account profile");
  }
  const displayName = clampText(payload.full_name, 160) || undefined;
  const avatarUrl = validArtworkUrl(payload.avatar_url);
  return { id, urn, username, displayName, avatarUrl };
}

async function fetchCurrentUser(accessToken: string): Promise<UserProfile> {
  const response = await fetchWithTimeout("https://api.soundcloud.com/me", {
    headers: {
      accept: "application/json; charset=utf-8",
      authorization: `OAuth ${accessToken}`,
    },
  });
  if (!response.ok) {
    const status = response.status === 401 || response.status === 403 ? response.status : 502;
    throw new ApiError(status, "profile_unavailable", "SoundCloud account profile is unavailable");
  }
  return asUserProfile(await response.json());
}

function collectionItems<T>(payload: unknown): T[] {
  if (Array.isArray(payload)) return payload as T[];
  const collection = (payload as { collection?: unknown })?.collection;
  if (Array.isArray(collection)) return collection as T[];
  throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned an invalid collection");
}

async function readUserCollection<T>(response: Response): Promise<T[]> {
  if (!response.ok) {
    const status = response.status === 401 || response.status === 403 || response.status === 429
      ? response.status
      : 502;
    const code = status === 429
      ? "upstream_rate_limited"
      : status === 401
        ? "upstream_unauthorized"
        : status === 403
          ? "upstream_forbidden"
          : "upstream_error";
    throw new ApiError(status, code, "SoundCloud account collection is unavailable");
  }
  const bytes = await readBoundedBody(response, MAX_UPSTREAM_BYTES);
  try {
    return collectionItems<T>(JSON.parse(new TextDecoder().decode(bytes)));
  } catch (error) {
    if (error instanceof ApiError) throw error;
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid account metadata");
  }
}

type SoundCloudCollectionPage<T> = {
  collection: T[];
  next_href?: string;
};

async function readUserCollectionPage<T>(response: Response): Promise<SoundCloudCollectionPage<T>> {
  if (!response.ok) {
    const status = response.status === 401 || response.status === 403 || response.status === 404 || response.status === 429
      ? response.status
      : 502;
    const code = status === 429
      ? "upstream_rate_limited"
      : status === 401
        ? "upstream_unauthorized"
        : status === 403
          ? "upstream_forbidden"
          : status === 404
            ? "upstream_not_found"
            : "upstream_error";
    throw new ApiError(status, code, "SoundCloud playlist tracks are unavailable");
  }
  const bytes = await readBoundedBody(response, MAX_UPSTREAM_BYTES);
  try {
    const payload = JSON.parse(new TextDecoder().decode(bytes)) as Record<string, unknown>;
    if (!Array.isArray(payload.collection)) {
      throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned an invalid playlist page");
    }
    return {
      collection: payload.collection as T[],
      next_href: typeof payload.next_href === "string" ? payload.next_href : undefined,
    };
  } catch (error) {
    if (error instanceof ApiError) throw error;
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid playlist metadata");
  }
}

function validatedPlaylistPageUrl(raw: string, playlistUrn: string): URL | undefined {
  try {
    const url = new URL(raw);
    const path = url.pathname.split("/").filter(Boolean);
    const pathUrn = path.length === 3 ? decodeURIComponent(path[1]) : undefined;
    if (
      url.protocol !== "https:" ||
      url.hostname !== "api.soundcloud.com" ||
      url.port ||
      url.username ||
      url.password ||
      path[0] !== "playlists" ||
      pathUrn !== playlistUrn ||
      path[2] !== "tracks" ||
      url.hash
    ) return undefined;
    return url;
  } catch {
    return undefined;
  }
}

async function fetchPlaylistTracks(accessToken: string, playlistUrn: string): Promise<PublicTrack[]> {
  if (!validResourceUrn(playlistUrn, "playlists")) {
    throw new ApiError(400, "invalid_playlist", "Playlist URN is invalid");
  }
  const first = new URL(`https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}/tracks`);
  first.search = new URLSearchParams({
    access: "playable,preview,blocked",
    linked_partitioning: "true",
  }).toString();
  const headers = {
    accept: "application/json; charset=utf-8",
    authorization: `OAuth ${accessToken}`,
  };
  const mapped: PublicTrack[] = [];
  let next: URL | undefined = first;
  for (let pageIndex = 0; next && pageIndex < MAX_PLAYLIST_PAGES && mapped.length < MAX_PLAYLIST_TRACKS; pageIndex += 1) {
    const page: SoundCloudCollectionPage<WorkerTrack> = await readUserCollectionPage<WorkerTrack>(
      await fetchWithTimeout(next.toString(), { headers }),
    );
    for (const track of page.collection) {
      const publicTrack = mapTrack(track);
      if (publicTrack) mapped.push(publicTrack);
      if (mapped.length >= MAX_PLAYLIST_TRACKS) break;
    }
    next = page.next_href
      ? validatedPlaylistPageUrl(page.next_href, playlistUrn)
      : undefined;
    if (page.next_href && !next) {
      throw new ApiError(502, "upstream_invalid_pagination", "SoundCloud returned an invalid playlist cursor");
    }
  }
  return mapped;
}

function accountOwnsPlaylist(profile: UserProfile | undefined, playlist: WorkerPlaylist): boolean {
  if (!profile || !playlist.user) return false;
  const ownerUrn = validResourceUrn(playlist.user.urn, "users");
  const profileUrn = validResourceUrn(profile.urn, "users");
  if (ownerUrn && profileUrn) return ownerUrn === profileUrn;
  const ownerId = safeResourceId(playlist.user.id);
  const profileId = safeResourceId(profile.id);
  return ownerId !== undefined && profileId !== undefined && ownerId === profileId;
}

function accountMutationError(response: Response, action: string): ApiError {
  const status = response.status === 400 || response.status === 401 || response.status === 403 ||
    response.status === 404 || response.status === 422 || response.status === 429
    ? response.status
    : 502;
  const code = status === 401
    ? "upstream_unauthorized"
    : status === 403
      ? "upstream_forbidden"
      : status === 404
        ? "upstream_not_found"
        : status === 429
          ? "upstream_rate_limited"
          : status === 422
            ? "upstream_rejected"
            : "upstream_error";
  return new ApiError(status, code, `SoundCloud could not ${action}`);
}

async function setTrackLiked(accessToken: string, trackUrn: string, liked: boolean): Promise<void> {
  if (!validResourceUrn(trackUrn, "tracks")) {
    throw new ApiError(400, "invalid_track", "Track URN is invalid");
  }
  const endpoint = `https://api.soundcloud.com/likes/tracks/${encodeURIComponent(trackUrn)}`;
  const response = await fetchWithTimeout(endpoint, {
    method: liked ? "POST" : "DELETE",
    headers: {
      accept: "application/json; charset=utf-8",
      authorization: `OAuth ${accessToken}`,
    },
  });
  if (!response.ok) throw accountMutationError(response, liked ? "like this track" : "unlike this track");
}

function validatePlaylistTitle(value: unknown): string {
  if (typeof value !== "string") {
    throw new ApiError(400, "invalid_playlist_title", "Playlist title is required");
  }
  const title = value.trim();
  if (!title || [...title].length > MAX_PLAYLIST_TITLE_LENGTH) {
    throw new ApiError(400, "invalid_playlist_title", "Playlist title must contain 1 to 100 characters");
  }
  return title;
}

async function fetchOwnedPlaylistMetadata(
  accessToken: string,
  profile: UserProfile | undefined,
  playlistUrn: string,
): Promise<WorkerPlaylist> {
  if (!validResourceUrn(playlistUrn, "playlists")) {
    throw new ApiError(400, "invalid_playlist", "Playlist URN is invalid");
  }
  const metadataUrl = new URL(`https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}`);
  metadataUrl.searchParams.set("show_tracks", "false");
  const metadataResponse = await fetchWithTimeout(metadataUrl.toString(), {
    headers: {
      accept: "application/json; charset=utf-8",
      authorization: `OAuth ${accessToken}`,
    },
  });
  if (!metadataResponse.ok) throw accountMutationError(metadataResponse, "read this playlist");
  const metadataBytes = await readBoundedBody(metadataResponse, MAX_UPSTREAM_BYTES);
  let metadata: WorkerPlaylist;
  try {
    metadata = JSON.parse(new TextDecoder().decode(metadataBytes)) as WorkerPlaylist;
  } catch {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid playlist metadata");
  }
  if (!accountOwnsPlaylist(profile, metadata)) {
    throw new ApiError(403, "playlist_not_owned", "Only playlists owned by this account can be edited");
  }
  return metadata;
}

async function createPrivatePlaylist(accessToken: string, title: string): Promise<PublicPlaylist> {
  const endpoint = "https://api.soundcloud.com/playlists";
  const response = await fetchWithTimeout(endpoint, {
    method: "POST",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/json",
      authorization: `OAuth ${accessToken}`,
    },
    body: JSON.stringify({ playlist: { title, sharing: "private", tracks: [] } }),
  });
  if (!response.ok) throw accountMutationError(response, "create this playlist");
  const bytes = await readBoundedBody(response, MAX_UPSTREAM_BYTES);
  let playlist: WorkerPlaylist;
  try {
    playlist = JSON.parse(new TextDecoder().decode(bytes)) as WorkerPlaylist;
  } catch {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid playlist metadata");
  }
  const mapped = mapPlaylist(playlist, true);
  if (!mapped) {
    throw new ApiError(502, "upstream_invalid_playlist", "SoundCloud returned incomplete playlist metadata");
  }
  return mapped;
}

async function deleteOwnedPlaylist(
  accessToken: string,
  profile: UserProfile | undefined,
  playlistUrn: string,
): Promise<void> {
  await fetchOwnedPlaylistMetadata(accessToken, profile, playlistUrn);
  const endpoint = `https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}`;
  const response = await fetchWithTimeout(endpoint, {
    method: "DELETE",
    headers: {
      accept: "application/json; charset=utf-8",
      authorization: `OAuth ${accessToken}`,
    },
  });
  if (!response.ok) throw accountMutationError(response, "delete this playlist");
}

async function fetchOwnedPlaylistTrackUrns(
  accessToken: string,
  profile: UserProfile | undefined,
  playlistUrn: string,
): Promise<string[]> {
  const metadata = await fetchOwnedPlaylistMetadata(accessToken, profile, playlistUrn);
  const headers = {
    accept: "application/json; charset=utf-8",
    authorization: `OAuth ${accessToken}`,
  };
  const expectedCount = Number.isFinite(metadata.track_count) && metadata.track_count! >= 0
    ? Math.floor(metadata.track_count!)
    : undefined;
  if (expectedCount === undefined || expectedCount >= MAX_EDITABLE_PLAYLIST_TRACKS) {
    throw new ApiError(409, "playlist_too_large", "This playlist is too large to edit safely on Brickwave");
  }

  const first = new URL(`https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}/tracks`);
  first.search = new URLSearchParams({
    access: "playable,preview,blocked",
    linked_partitioning: "true",
    limit: "200",
  }).toString();
  const urns: string[] = [];
  let next: URL | undefined = first;
  for (let pageIndex = 0; next && pageIndex < MAX_EDITABLE_PLAYLIST_PAGES; pageIndex += 1) {
    const page: SoundCloudCollectionPage<WorkerTrack> = await readUserCollectionPage<WorkerTrack>(
      await fetchWithTimeout(next.toString(), { headers }),
    );
    for (const track of page.collection) {
      const urn = validResourceUrn(track.urn, "tracks");
      if (!urn) {
        throw new ApiError(409, "playlist_incomplete", "A playlist track is missing its SoundCloud URN");
      }
      urns.push(urn);
      if (urns.length >= MAX_EDITABLE_PLAYLIST_TRACKS) {
        throw new ApiError(409, "playlist_too_large", "This playlist is too large to edit safely on Brickwave");
      }
    }
    next = page.next_href ? validatedPlaylistPageUrl(page.next_href, playlistUrn) : undefined;
    if (page.next_href && !next) {
      throw new ApiError(502, "upstream_invalid_pagination", "SoundCloud returned an invalid playlist cursor");
    }
  }
  if (next || urns.length !== expectedCount) {
    throw new ApiError(409, "playlist_incomplete", "Brickwave could not verify the complete playlist before editing");
  }
  return urns;
}

async function addTrackToOwnedPlaylist(
  accessToken: string,
  profile: UserProfile | undefined,
  playlistUrn: string,
  trackUrn: string,
): Promise<number> {
  if (!validResourceUrn(trackUrn, "tracks")) {
    throw new ApiError(400, "invalid_track", "Track URN is invalid");
  }
  const trackUrns = await fetchOwnedPlaylistTrackUrns(accessToken, profile, playlistUrn);
  if (trackUrns.includes(trackUrn)) return trackUrns.length;
  trackUrns.push(trackUrn);
  const endpoint = `https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}`;
  const response = await fetchWithTimeout(endpoint, {
    method: "PUT",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/json",
      authorization: `OAuth ${accessToken}`,
    },
    body: JSON.stringify({ playlist: { tracks: trackUrns.map((urn) => ({ urn })) } }),
  });
  if (!response.ok) throw accountMutationError(response, "update this playlist");
  return trackUrns.length;
}

async function removeTrackFromOwnedPlaylist(
  accessToken: string,
  profile: UserProfile | undefined,
  playlistUrn: string,
  trackUrn: string,
  trackIndex: number,
): Promise<number> {
  if (!validResourceUrn(trackUrn, "tracks") || !Number.isSafeInteger(trackIndex) || trackIndex < 0) {
    throw new ApiError(400, "invalid_track", "Track selection is invalid");
  }
  const trackUrns = await fetchOwnedPlaylistTrackUrns(accessToken, profile, playlistUrn);
  if (trackIndex >= trackUrns.length || trackUrns[trackIndex] !== trackUrn) {
    throw new ApiError(409, "playlist_changed", "The playlist changed; reopen it before removing a track");
  }
  trackUrns.splice(trackIndex, 1);
  const endpoint = `https://api.soundcloud.com/playlists/${encodeURIComponent(playlistUrn)}`;
  const response = await fetchWithTimeout(endpoint, {
    method: "PUT",
    headers: {
      accept: "application/json; charset=utf-8",
      "content-type": "application/json",
      authorization: `OAuth ${accessToken}`,
    },
    body: JSON.stringify({ playlist: { tracks: trackUrns.map((urn) => ({ urn })) } }),
  });
  if (!response.ok) throw accountMutationError(response, "update this playlist");
  return trackUrns.length;
}

async function fetchUserLibrary(accessToken: string): Promise<{
  liked_tracks: PublicTrack[];
  playlists: PublicPlaylist[];
  home_tracks: PublicTrack[];
  discover_tracks: PublicTrack[];
}> {
  const likedUrl = new URL("https://api.soundcloud.com/me/likes/tracks");
  likedUrl.search = new URLSearchParams({
    access: "playable,preview,blocked",
    linked_partitioning: "true",
    limit: String(MAX_LIBRARY_TRACKS),
  }).toString();
  const playlistsUrl = new URL("https://api.soundcloud.com/me/playlists");
  playlistsUrl.search = new URLSearchParams({
    show_tracks: "false",
    linked_partitioning: "true",
    limit: String(MAX_LIBRARY_PLAYLISTS),
  }).toString();
  const likedPlaylistsUrl = new URL("https://api.soundcloud.com/me/likes/playlists");
  likedPlaylistsUrl.search = new URLSearchParams({
    linked_partitioning: "true",
    limit: String(MAX_LIBRARY_PLAYLISTS),
  }).toString();
  const homeUrl = new URL("https://api.soundcloud.com/me/feed/tracks");
  homeUrl.search = new URLSearchParams({
    access: "playable,preview,blocked",
    limit: String(MAX_LIBRARY_TRACKS),
  }).toString();
  const headers = {
    accept: "application/json; charset=utf-8",
    authorization: `OAuth ${accessToken}`,
  };
  const [likedResponse, playlistsResponse, likedPlaylistsResponse, homeResponse] = await Promise.all([
    fetchWithTimeout(likedUrl.toString(), { headers }),
    fetchWithTimeout(playlistsUrl.toString(), { headers }),
    fetchWithTimeout(likedPlaylistsUrl.toString(), { headers }),
    fetchWithTimeout(homeUrl.toString(), { headers }),
  ]);
  const [liked, playlists, likedPlaylists, activities] = await Promise.all([
    readUserCollection<WorkerTrack>(likedResponse),
    readUserCollection<WorkerPlaylist>(playlistsResponse),
    readUserCollection<WorkerPlaylist>(likedPlaylistsResponse),
    readUserCollection<{ type?: string; origin?: WorkerTrack }>(homeResponse),
  ]);
  const homeTracks = activities
    .filter((activity) => activity.type === "track" || activity.type === "track:repost")
    .map((activity) => activity.origin)
    .filter((track): track is WorkerTrack => Boolean(track));
  const seed = liked.find((track) => validResourceUrn(track.urn, "tracks") || safeResourceId(track.id))
    ?? homeTracks.find((track) => validResourceUrn(track.urn, "tracks") || safeResourceId(track.id));
  let discover: WorkerTrack[] = [];
  if (seed) {
    const seedUrn = validResourceUrn(seed.urn, "tracks") ?? `soundcloud:tracks:${safeResourceId(seed.id)}`;
    const relatedUrl = new URL(
      `https://api.soundcloud.com/tracks/${encodeURIComponent(seedUrn)}/related`,
    );
    relatedUrl.search = new URLSearchParams({
      access: "playable,preview,blocked",
      linked_partitioning: "true",
      limit: String(MAX_RESULTS),
    }).toString();
    discover = await readUserCollection<WorkerTrack>(
      await fetchWithTimeout(relatedUrl.toString(), { headers }),
    );
  }
  const uniquePlaylists = new Map<string, PublicPlaylist>();
  for (const playlist of playlists) {
    const mapped = mapPlaylist(playlist, true);
    const key = mapped?.urn ?? (mapped?.id === undefined ? undefined : String(mapped.id));
    if (mapped && key && !uniquePlaylists.has(key)) uniquePlaylists.set(key, mapped);
    if (uniquePlaylists.size >= MAX_LIBRARY_PLAYLISTS) break;
  }
  for (const playlist of likedPlaylists) {
    const mapped = mapPlaylist(playlist, false);
    const key = mapped?.urn ?? (mapped?.id === undefined ? undefined : String(mapped.id));
    if (mapped && key && !uniquePlaylists.has(key)) uniquePlaylists.set(key, mapped);
    if (uniquePlaylists.size >= MAX_LIBRARY_PLAYLISTS) break;
  }
  return {
    liked_tracks: liked
      .slice(0, MAX_LIBRARY_TRACKS)
      .map(mapTrack)
      .filter((track): track is PublicTrack => Boolean(track)),
    playlists: [...uniquePlaylists.values()],
    home_tracks: homeTracks
      .slice(0, MAX_LIBRARY_TRACKS)
      .map(mapTrack)
      .filter((track): track is PublicTrack => Boolean(track)),
    discover_tracks: discover
      .slice(0, MAX_RESULTS)
      .map(mapTrack)
      .filter((track): track is PublicTrack => Boolean(track)),
  };
}

/// A singleton Durable Object serializes token renewal and persists the rotated
/// refresh token. Its `fetch` method is reachable only through the Worker
/// service binding; the public Worker exposes no token route.
export class TokenCoordinator implements DurableObject {
  private refreshInFlight: Promise<StoredToken> | undefined;

  constructor(
    private readonly state: DurableObjectState,
    private readonly env: Env,
  ) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== "/lease") {
      return json({ error: { code: "not_found", message: "Not found" } }, 404);
    }
    try {
      const force = url.searchParams.get("force") === "1";
      const token = await this.lease(force);
      return json({ access_token: token.accessToken, expires_at_ms: token.expiresAtMs });
    } catch (error) {
      return errorResponse(error);
    }
  }

  private async lease(force: boolean): Promise<StoredToken> {
    const cached = await this.state.storage.get<StoredToken>("soundcloud-token");
    if (!force && cached && tokenIsFresh(cached)) return cached;
    if (!this.refreshInFlight) {
      this.refreshInFlight = this.renew(cached).finally(() => {
        this.refreshInFlight = undefined;
      });
    }
    return this.refreshInFlight;
  }

  private async renew(previous: StoredToken | undefined): Promise<StoredToken> {
    let issued: IssuedToken;
    if (previous?.refreshToken) {
      try {
        issued = await exchangeRefresh(this.env, previous.refreshToken);
      } catch (error) {
        // A rejected single-use refresh token is replaced with exactly one new
        // Client Credentials grant. Rate-limit and temporary failures are not
        // retried by this path.
        if (!(error instanceof ApiError) || ![400, 401, 403].includes(error.status)) throw error;
        issued = await exchangeClientCredentials(this.env);
      }
    } else {
      issued = await exchangeClientCredentials(this.env);
    }
    const token: StoredToken = {
      accessToken: issued.accessToken,
      refreshToken: issued.refreshToken,
      expiresAtMs: Date.now() + issued.expiresInSeconds * 1000,
    };
    // Store the newly rotated refresh token before it is ever considered live.
    await this.state.storage.put("soundcloud-token", token);
    return token;
  }
}

type RateRecord = { windowStartMs: number; count: number };

/// One Durable Object per hashed caller stores a fixed-window counter. It
/// avoids relying on per-isolate globals or eventually-consistent KV for abuse
/// control. The Worker never stores the caller's raw IP address.
export class RequestGuard implements DurableObject {
  constructor(private readonly state: DurableObjectState) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== "/check") {
      return json({ allowed: false }, 404);
    }
    const now = Date.now();
    const windowStartMs = Math.floor(now / RATE_WINDOW_MS) * RATE_WINDOW_MS;
    const previous = await this.state.storage.get<RateRecord>("counter");
    const record = previous?.windowStartMs === windowStartMs ? previous : { windowStartMs, count: 0 };
    record.count += 1;
    await this.state.storage.put("counter", record);
    // Bound retained per-caller state. A later request can safely schedule a
    // new alarm, and an object with no counter has no rate-limit history.
    await this.state.storage.setAlarm(windowStartMs + RATE_WINDOW_MS + 1);
    if (record.count > RATE_MAX_REQUESTS) {
      return json({ allowed: false, retry_after_seconds: Math.ceil((windowStartMs + RATE_WINDOW_MS - now) / 1000) }, 429);
    }
    return json({ allowed: true });
  }

  async alarm(): Promise<void> {
    await this.state.storage.deleteAll();
  }
}

/**
 * Stores user-authorization pairing sessions. It is deliberately a different
 * namespace from TokenCoordinator: client-credentials tokens can never be
 * returned by, refreshed by, or invalidated through this object.
 *
 * Public requests never reach this object's URL. The Worker is its sole
 * caller through the AUTH_SESSION_STORE service binding.
 */
export class AuthSessionStore implements DurableObject {
  private readonly userRefreshInFlight = new Map<string, Promise<UserToken>>();

  constructor(
    private readonly state: DurableObjectState,
    private readonly env: Env,
  ) {}

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST") return json({ error: { code: "not_found", message: "Not found" } }, 404);
    try {
      switch (url.pathname) {
        case "/start":
          return json(await this.start(await request.json()));
        case "/browser-open":
          return json(await this.browserOpen(await request.json()));
        case "/authorize":
          return json(await this.authorize(await request.json()));
        case "/callback-begin":
          return json(await this.callbackBegin(await request.json()));
        case "/callback-success":
          return json(await this.callbackSuccess(await request.json()));
        case "/status":
          return json(await this.status(await request.json()));
        case "/complete":
          return json(await this.complete(await request.json()));
        case "/cancel":
          return json(await this.cancel(await request.json()));
        case "/me":
          return json(await this.currentUser(await request.json()));
        case "/library":
          return json(await this.userLibrary(await request.json()));
        case "/track-like":
          return json(await this.trackLike(await request.json()));
        case "/playlist-add-track":
          return json(await this.playlistAddTrack(await request.json()));
        case "/playlist-remove-track":
          return json(await this.playlistRemoveTrack(await request.json()));
        case "/playlist-create":
          return json(await this.playlistCreate(await request.json()), 201);
        case "/playlist-delete":
          return json(await this.playlistDelete(await request.json()));
        case "/playlist-tracks":
          return json(await this.playlistTracks(await request.json()));
        case "/stream-descriptor":
          return json(await this.streamDescriptor(await request.json()));
        case "/logout":
          return json(await this.logout(await request.json()));
        default:
          return json({ error: { code: "not_found", message: "Not found" } }, 404);
      }
    } catch (error) {
      return errorResponse(error);
    }
  }

  private sessionKey(pairingId: string): string {
    return `pair:${pairingId}`;
  }

  private oauthKey(oauthState: string): string {
    return `oauth:${oauthState}`;
  }

  private sessionIdKey(sessionId: string): string {
    return `session:${sessionId}`;
  }

  private async loadPairing(pairingId: unknown): Promise<PairingSession> {
    if (!validOpaqueId(pairingId)) throw new ApiError(400, "invalid_pairing", "Pairing request is invalid");
    const session = await this.state.storage.get<PairingSession>(this.sessionKey(pairingId));
    const expired = session?.phase === "authorized"
      ? !session.sessionExpiresAtMs || session.sessionExpiresAtMs <= Date.now()
      : !session || session.expiresAtMs <= Date.now();
    if (!session || expired || session.phase === "cancelled") {
      if (session) {
        if (session.oauthState) await this.state.storage.delete(this.oauthKey(session.oauthState));
        if (session.sessionId) await this.state.storage.delete(this.sessionIdKey(session.sessionId));
        await this.state.storage.delete(this.sessionKey(session.pairingId));
        await this.scheduleNextAlarm();
      }
      throw new ApiError(410, "pairing_expired", "This pairing request has expired");
    }
    return session;
  }

  private async requireDevice(payload: unknown): Promise<PairingSession> {
    const body = payload as Record<string, unknown>;
    const session = await this.loadPairing(body.pairing_id);
    const secret = typeof body.device_secret === "string" ? body.device_secret : "";
    if (!validOpaqueId(secret) || !safeEqual(session.deviceSecretHash, await sha256Base64Url(secret))) {
      throw new ApiError(401, "device_proof_invalid", "This device cannot access the pairing request");
    }
    return session;
  }

  private async requireAppSession(payload: unknown): Promise<PairingSession> {
    const body = payload as Record<string, unknown>;
    const sessionId = body.session_id;
    const sessionSecret = body.session_secret;
    if (!validOpaqueId(sessionId) || !validOpaqueId(sessionSecret)) {
      throw new ApiError(401, "session_invalid", "Account session is invalid");
    }
    const pairingId = await this.state.storage.get<string>(this.sessionIdKey(sessionId));
    if (!pairingId) throw new ApiError(401, "session_invalid", "Account session is invalid");
    const session = await this.loadPairing(pairingId);
    if (
      session.phase !== "authorized" ||
      !session.sessionExpiresAtMs ||
      session.sessionExpiresAtMs <= Date.now() ||
      !safeEqual(session.sessionSecretHash, await sha256Base64Url(sessionSecret))
    ) {
      throw new ApiError(401, "session_invalid", "Account session is invalid");
    }
    return session;
  }

  private async start(payload: unknown): Promise<Record<string, unknown>> {
    const callerKey = (payload as Record<string, unknown>).caller_key;
    if (typeof callerKey !== "string" || !/^[0-9a-f]{64}$/.test(callerKey)) {
      throw new ApiError(400, "caller_invalid", "Pairing request is invalid");
    }
    const entries = await this.state.storage.list<PairingSession>({ prefix: "pair:" });
    const pending = [...entries.values()].filter(
      (session) => session.expiresAtMs > Date.now() && session.phase !== "authorized" && session.phase !== "cancelled",
    );
    if (pending.length >= AUTH_MAX_PENDING_SESSIONS) {
      throw new ApiError(429, "too_many_pending_pairings", "Too many pending pairing requests; try again shortly");
    }
    if (pending.filter((session) => session.callerKey === callerKey).length >= AUTH_MAX_PENDING_PER_CALLER) {
      throw new ApiError(429, "too_many_pending_pairings", "Too many pending pairing requests; try again shortly");
    }
    const pairingId = base64UrlRandom(24);
    const deviceSecret = base64UrlRandom(32);
    const confirmationCode = String(100000 + (crypto.getRandomValues(new Uint32Array(1))[0] % 900000));
    const now = Date.now();
    const session: PairingSession = {
      pairingId,
      callerKey,
      deviceSecretHash: await sha256Base64Url(deviceSecret),
      confirmationCode,
      phase: "waiting_for_scan",
      createdAtMs: now,
      expiresAtMs: now + AUTH_PENDING_TTL_MS,
      nextPollAtMs: 0,
      confirmationAttempts: 0,
    };
    await this.state.storage.put(this.sessionKey(pairingId), session);
    await this.scheduleNextAlarm();
    // The device secret is returned exactly once to the initiating application.
    // It is never included in the pairing URL, a browser page, or later status.
    return {
      pairing_id: pairingId,
      device_secret: deviceSecret,
      confirmation_code: confirmationCode,
      expires_at_ms: session.expiresAtMs,
    };
  }

  private async browserOpen(payload: unknown): Promise<Record<string, unknown>> {
    const body = payload as Record<string, unknown>;
    const session = await this.loadPairing(body.pairing_id);
    const browserNonce = typeof body.browser_nonce === "string" ? body.browser_nonce : "";
    if (!validOpaqueId(browserNonce)) throw new ApiError(400, "browser_invalid", "Browser pairing request is invalid");
    const browserNonceHash = await sha256Base64Url(browserNonce);
    if (session.browserNonceHash && !safeEqual(session.browserNonceHash, browserNonceHash)) {
      // Phone cameras and QR scanners commonly fetch a link once to build a
      // preview, then open it in the user's browser without carrying the
      // preview request's cookie. Before OAuth starts there is no credential
      // or authorization state to protect, so let the real browser replace
      // that provisional nonce. The six-digit device confirmation remains the
      // user-visible proof. Once OAuth has started, keep the binding strict.
      if (session.oauthState) {
        throw new ApiError(409, "pairing_claimed", "This pairing request is already authorizing in another browser");
      }
    }
    session.browserNonceHash = browserNonceHash;
    if (session.phase === "waiting_for_scan") session.phase = "waiting_for_authorization";
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    return { confirmation_code: session.confirmationCode, expires_at_ms: session.expiresAtMs };
  }

  private async authorize(payload: unknown): Promise<Record<string, unknown>> {
    const body = payload as Record<string, unknown>;
    const session = await this.loadPairing(body.pairing_id);
    const browserNonce = typeof body.browser_nonce === "string" ? body.browser_nonce : "";
    if (!validOpaqueId(browserNonce) || !safeEqual(session.browserNonceHash, await sha256Base64Url(browserNonce))) {
      throw new ApiError(401, "browser_invalid", "Open the pairing page again and retry");
    }
    if (session.oauthState) throw new ApiError(409, "authorization_started", "Authorization is already in progress");
    const oauthState = base64UrlRandom(32);
    const verifier = base64UrlRandom(48);
    session.oauthState = oauthState;
    session.pkceVerifier = verifier;
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    await this.state.storage.put(this.oauthKey(oauthState), session.pairingId);
    return { oauth_state: oauthState, code_challenge: await pkceChallenge(verifier) };
  }

  private async callbackBegin(payload: unknown): Promise<Record<string, unknown>> {
    const body = payload as Record<string, unknown>;
    const oauthState = body.oauth_state;
    const browserNonce = typeof body.browser_nonce === "string" ? body.browser_nonce : "";
    if (!validOpaqueId(oauthState) || !validOpaqueId(browserNonce)) {
      throw new ApiError(400, "oauth_invalid", "Authorization callback is invalid");
    }
    const pairingId = await this.state.storage.get<string>(this.oauthKey(oauthState));
    if (!pairingId) throw new ApiError(400, "oauth_state_invalid", "Authorization state is invalid or already used");
    const session = await this.loadPairing(pairingId);
    if (
      session.oauthConsumed ||
      !safeEqual(session.oauthState, oauthState) ||
      !safeEqual(session.browserNonceHash, await sha256Base64Url(browserNonce)) ||
      !session.pkceVerifier
    ) {
      throw new ApiError(400, "oauth_state_invalid", "Authorization state is invalid or already used");
    }
    session.oauthConsumed = true;
    await this.state.storage.put(this.sessionKey(pairingId), session);
    await this.state.storage.delete(this.oauthKey(oauthState));
    return { pairing_id: pairingId, pkce_verifier: session.pkceVerifier };
  }

  private async callbackSuccess(payload: unknown): Promise<Record<string, unknown>> {
    const body = payload as Record<string, unknown>;
    const session = await this.loadPairing(body.pairing_id);
    const token = body.user_token as UserToken | undefined;
    const profile = body.profile as UserProfile | undefined;
    if (
      !session.oauthConsumed ||
      !token?.accessToken ||
      !profile?.username ||
      (!safeResourceId(profile.id) && !validResourceUrn(profile.urn, "users"))
    ) {
      throw new ApiError(400, "oauth_invalid", "Authorization callback is invalid");
    }
    session.userToken = token;
    session.profile = profile;
    session.phase = "authorization_complete";
    session.pkceVerifier = undefined;
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    return { confirmation_code: session.confirmationCode, expires_at_ms: session.expiresAtMs };
  }

  private async status(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireDevice(payload);
    const now = Date.now();
    if (now < session.nextPollAtMs) {
      throw new ApiError(429, "poll_too_soon", "Wait before checking pairing status again");
    }
    session.nextPollAtMs = now + AUTH_POLL_INTERVAL_MS;
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    return {
      phase: session.phase,
      expires_at_ms: session.expiresAtMs,
      confirmation_code: session.confirmationCode,
    };
  }

  private async complete(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireDevice(payload);
    const body = payload as Record<string, unknown>;
    if (session.phase !== "authorization_complete" || !session.profile) {
      throw new ApiError(409, "confirmation_required", "Confirm the matching code after authorization completes");
    }
    if (body.confirmation_code !== session.confirmationCode) {
      session.confirmationAttempts += 1;
      await this.state.storage.put(this.sessionKey(session.pairingId), session);
      if (session.confirmationAttempts >= AUTH_MAX_CONFIRMATION_ATTEMPTS) {
        await this.state.storage.delete(this.sessionKey(session.pairingId));
        throw new ApiError(429, "confirmation_locked", "Too many confirmation attempts; create a new QR code");
      }
      throw new ApiError(409, "confirmation_required", "Confirm the matching code after authorization completes");
    }
    const sessionId = base64UrlRandom(24);
    const sessionSecret = base64UrlRandom(32);
    session.sessionId = sessionId;
    session.sessionSecretHash = await sha256Base64Url(sessionSecret);
    session.sessionExpiresAtMs = Date.now() + AUTH_SESSION_TTL_MS;
    session.phase = "authorized";
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    await this.state.storage.put(this.sessionIdKey(sessionId), session.pairingId);
    await this.scheduleNextAlarm();
    // This is the only response containing the opaque client session secret.
    // It is not a SoundCloud token and is never embedded in a URL or browser page.
    return {
      session_id: sessionId,
      session_secret: sessionSecret,
      profile: session.profile,
      expires_at_ms: session.sessionExpiresAtMs,
    };
  }

  private async cancel(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireDevice(payload);
    session.phase = "cancelled";
    session.userToken = undefined;
    session.pkceVerifier = undefined;
    if (session.oauthState) await this.state.storage.delete(this.oauthKey(session.oauthState));
    if (session.sessionId) await this.state.storage.delete(this.sessionIdKey(session.sessionId));
    await this.state.storage.delete(this.sessionKey(session.pairingId));
    await this.scheduleNextAlarm();
    return { cancelled: true };
  }

  private async currentUser(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const profile = await this.withUserToken(session, fetchCurrentUser);
    session.profile = profile;
    await this.state.storage.put(this.sessionKey(session.pairingId), session);
    return { profile };
  }

  private async userLibrary(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const library = await this.withUserToken(session, fetchUserLibrary);
    const encoded = JSON.stringify(library);
    if (new TextEncoder().encode(encoded).byteLength > MAX_RESPONSE_BYTES) {
      throw new ApiError(502, "response_too_large", "Mapped account collection exceeded the limit");
    }
    return library;
  }

  private async playlistTracks(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const playlistUrn = validResourceUrn(
      (payload as Record<string, unknown>).playlist_urn,
      "playlists",
    );
    if (!playlistUrn) throw new ApiError(400, "invalid_playlist", "Playlist URN is invalid");
    const tracks = await this.withUserToken(
      session,
      (accessToken) => fetchPlaylistTracks(accessToken, playlistUrn),
    );
    const result = { playlist_urn: playlistUrn, tracks };
    const encoded = JSON.stringify(result);
    if (new TextEncoder().encode(encoded).byteLength > MAX_RESPONSE_BYTES) {
      throw new ApiError(502, "response_too_large", "Mapped playlist exceeded the limit");
    }
    return result;
  }

  private async trackLike(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const body = payload as Record<string, unknown>;
    const trackUrn = validResourceUrn(body.track_urn, "tracks");
    if (!trackUrn || typeof body.liked !== "boolean") {
      throw new ApiError(400, "invalid_track", "Track like request is invalid");
    }
    await this.withUserToken(
      session,
      (accessToken) => setTrackLiked(accessToken, trackUrn, body.liked as boolean),
    );
    return { track_urn: trackUrn, liked: body.liked };
  }

  private async playlistAddTrack(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const body = payload as Record<string, unknown>;
    const playlistUrn = validResourceUrn(body.playlist_urn, "playlists");
    const trackUrn = validResourceUrn(body.track_urn, "tracks");
    if (!playlistUrn || !trackUrn) {
      throw new ApiError(400, "invalid_resource", "Playlist or track URN is invalid");
    }
    const trackCount = await this.withUserToken(
      session,
      (accessToken) => addTrackToOwnedPlaylist(
        accessToken,
        session.profile,
        playlistUrn,
        trackUrn,
      ),
    );
    return { playlist_urn: playlistUrn, track_urn: trackUrn, track_count: trackCount };
  }

  private async playlistRemoveTrack(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const body = payload as Record<string, unknown>;
    const playlistUrn = validResourceUrn(body.playlist_urn, "playlists");
    const trackUrn = validResourceUrn(body.track_urn, "tracks");
    const trackIndex = typeof body.track_index === "number" && Number.isSafeInteger(body.track_index)
      ? body.track_index
      : -1;
    if (!playlistUrn || !trackUrn || trackIndex < 0) {
      throw new ApiError(400, "invalid_resource", "Playlist or track selection is invalid");
    }
    const trackCount = await this.withUserToken(
      session,
      (accessToken) => removeTrackFromOwnedPlaylist(
        accessToken,
        session.profile,
        playlistUrn,
        trackUrn,
        trackIndex,
      ),
    );
    return { playlist_urn: playlistUrn, track_urn: trackUrn, track_index: trackIndex, track_count: trackCount };
  }

  private async playlistCreate(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const title = validatePlaylistTitle((payload as Record<string, unknown>).title);
    const playlist = await this.withUserToken(
      session,
      (accessToken) => createPrivatePlaylist(accessToken, title),
    );
    return { playlist };
  }

  private async playlistDelete(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    const playlistUrn = validResourceUrn(
      (payload as Record<string, unknown>).playlist_urn,
      "playlists",
    );
    if (!playlistUrn) throw new ApiError(400, "invalid_playlist", "Playlist URN is invalid");
    await this.withUserToken(
      session,
      (accessToken) => deleteOwnedPlaylist(accessToken, session.profile, playlistUrn),
    );
    return { playlist_urn: playlistUrn, deleted: true };
  }

  private async streamDescriptor(payload: unknown): Promise<PublicStreamDescriptor> {
    const session = await this.requireAppSession(payload);
    const trackUrn = validResourceUrn(
      (payload as Record<string, unknown>).track_urn,
      "tracks",
    );
    if (!trackUrn) throw new ApiError(400, "invalid_track", "Track URN is invalid");
    return this.withUserToken(
      session,
      (accessToken) => fetchStreamDescriptor(accessToken, trackUrn),
    );
  }

  private async logout(payload: unknown): Promise<Record<string, unknown>> {
    const session = await this.requireAppSession(payload);
    if (session.userToken?.accessToken) {
      // The documented sign-out call is best effort. Regardless of its result,
      // this app session is invalidated locally in the DO exactly once.
      await fetchWithTimeout("https://secure.soundcloud.com/sign-out", {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify({ access_token: session.userToken.accessToken }),
      }).catch(() => undefined);
    }
    if (session.oauthState) await this.state.storage.delete(this.oauthKey(session.oauthState));
    if (session.sessionId) await this.state.storage.delete(this.sessionIdKey(session.sessionId));
    await this.state.storage.delete(this.sessionKey(session.pairingId));
    await this.scheduleNextAlarm();
    return { logged_out: true };
  }

  private async withUserToken<T>(
    session: PairingSession,
    operation: (accessToken: string) => Promise<T>,
  ): Promise<T> {
    let token = session.userToken;
    if (!token) throw new ApiError(401, "session_invalid", "Account session is invalid");
    if (!tokenIsFresh(token)) {
      if (!token.refreshToken) throw new ApiError(401, "session_expired", "SoundCloud session has expired");
      token = await this.refreshUserToken(session, token.refreshToken);
      session.userToken = token;
    }
    try {
      return await operation(token.accessToken);
    } catch (error) {
      if (!(error instanceof ApiError) || error.status !== 401 || !token.refreshToken) throw error;
      const rotated = await this.refreshUserToken(session, token.refreshToken);
      session.userToken = rotated;
      return operation(rotated.accessToken);
    }
  }

  private async refreshUserToken(session: PairingSession, refreshToken: string): Promise<UserToken> {
    let pending = this.userRefreshInFlight.get(session.pairingId);
    if (!pending) {
      pending = (async () => {
        const issued = await exchangeRefresh(this.env, refreshToken);
        const rotated = {
          accessToken: issued.accessToken,
          refreshToken: issued.refreshToken,
          expiresAtMs: Date.now() + issued.expiresInSeconds * 1000,
        };
        session.userToken = rotated;
        await this.state.storage.put(this.sessionKey(session.pairingId), session);
        return rotated;
      })().finally(() => this.userRefreshInFlight.delete(session.pairingId));
      this.userRefreshInFlight.set(session.pairingId, pending);
    }
    return pending;
  }

  async alarm(): Promise<void> {
    const entries = await this.state.storage.list<PairingSession>({ prefix: "pair:" });
    const now = Date.now();
    for (const session of entries.values()) {
      const expired = session.phase === "authorized"
        ? session.sessionExpiresAtMs !== undefined && session.sessionExpiresAtMs <= now
        : session.expiresAtMs <= now;
      if (expired) {
        if (session.oauthState) await this.state.storage.delete(this.oauthKey(session.oauthState));
        if (session.sessionId) await this.state.storage.delete(this.sessionIdKey(session.sessionId));
        await this.state.storage.delete(this.sessionKey(session.pairingId));
      }
    }
    await this.scheduleNextAlarm();
  }

  private async scheduleNextAlarm(): Promise<void> {
    const entries = await this.state.storage.list<PairingSession>({ prefix: "pair:" });
    const now = Date.now();
    const next = [...entries.values()]
      .map((session) => session.phase === "authorized" ? session.sessionExpiresAtMs : session.expiresAtMs)
      .filter((expiresAt): expiresAt is number => expiresAt !== undefined && expiresAt > now)
      .sort((left, right) => left - right)[0];
    if (next !== undefined) await this.state.storage.setAlarm(next + 1);
  }
}

async function hashedClientKey(request: Request): Promise<string> {
  const source = request.headers.get("cf-connecting-ip") ?? "unknown";
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(source));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function checkRateLimit(request: Request, env: Env, resource = "request"): Promise<void> {
  const clientKey = await hashedClientKey(request);
  const guard = env.REQUEST_GUARD.get(env.REQUEST_GUARD.idFromName(clientKey));
  const response = await guard.fetch("https://guard.internal/check", { method: "POST" });
  if (response.status === 429) {
    throw new ApiError(429, "rate_limited", `Too many ${resource} requests; try again shortly`);
  }
  if (!response.ok) throw new ApiError(503, "rate_guard_unavailable", "Request guard is unavailable");
}

function authStore(env: Env): DurableObjectStub {
  return env.AUTH_SESSION_STORE.get(env.AUTH_SESSION_STORE.idFromName("soundcloud-user-auth-sessions"));
}

async function callAuthStore(env: Env, path: string, body: Record<string, unknown>): Promise<Response> {
  return authStore(env).fetch(`https://auth-session.internal${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify(body),
  });
}

async function authStoreJson(
  env: Env,
  path: string,
  body: Record<string, unknown>,
  maximumBytes = 64 * 1024,
): Promise<Response> {
  const response = await callAuthStore(env, path, body);
  const bytes = await readBoundedBody(response, maximumBytes);
  return new Response(bytes, { status: response.status, headers: authNoStoreHeaders({ "content-type": "application/json; charset=utf-8" }) });
}

function pairingUrl(request: Request, pairingId: string): string {
  const url = new URL(request.url);
  url.pathname = "/auth/connect";
  url.search = new URLSearchParams({ pairing: pairingId }).toString();
  url.hash = "";
  return url.toString();
}

function cookieValue(request: Request, name: string): string | undefined {
  const cookie = request.headers.get("cookie") ?? "";
  return cookie
    .split(";")
    .map((entry) => entry.trim())
    .find((entry) => entry.startsWith(`${name}=`))
    ?.slice(name.length + 1);
}

function phonePairingPage(confirmationCode: string): string {
  const code = confirmationCode.replace(/[^0-9]/g, "");
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="no-referrer"><title>Connect SoundCloud · Brickwave</title><style>body{margin:0;background:#10110f;color:#f2eee7;font-family:system-ui,sans-serif}main{max-width:28rem;margin:10vh auto;padding:2rem;border:1px solid #735a45;border-radius:1rem;background:#20201c}h1{margin-top:0;color:#ff5500}.code{font-size:2rem;letter-spacing:.18em;font-weight:700}button{display:inline-block;margin-top:1.5rem;padding:.8rem 1rem;border:0;border-radius:.5rem;background:#ff5500;color:#111;font:inherit;font-weight:700;cursor:pointer}</style></head><body><main><h1>Connect SoundCloud</h1><p>Check that this code matches the one shown by Brickwave:</p><p class="code">${code}</p><p>Sign in only on the official SoundCloud page. Brickwave never asks for your password.</p><form method="post" action="/auth/authorize"><button type="submit">Continue to SoundCloud</button></form></main></body></html>`;
}

function browserErrorPage(message: string): Response {
  return new Response(`<!doctype html><meta charset="utf-8"><title>Brickwave connection</title><h1>Connection unavailable</h1><p>${message}</p>`, {
    status: 400,
    headers: authNoStoreHeaders({ "content-type": "text/html; charset=utf-8", "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'" }),
  });
}

async function leaseToken(env: Env, force = false): Promise<string> {
  const coordinator = env.TOKEN_COORDINATOR.get(env.TOKEN_COORDINATOR.idFromName("soundcloud-client-credentials"));
  const response = await coordinator.fetch(`https://token.internal/lease${force ? "?force=1" : ""}`, { method: "POST" });
  if (!response.ok) {
    if (response.status === 429) throw new ApiError(429, "token_rate_limited", "Token service is rate limited");
    throw new ApiError(502, "token_unavailable", "Metadata token service is unavailable");
  }
  const payload = (await response.json()) as Record<string, unknown>;
  const accessToken = clampText(payload.access_token, 4096);
  if (!accessToken) throw new ApiError(502, "token_unavailable", "Metadata token service returned an invalid response");
  return accessToken;
}

function firstSoundCloudSearchUrl(
  endpoint: string,
  query: string,
  resource: "tracks" | "playlists",
): URL {
  const url = new URL(endpoint);
  const parameters: Record<string, string> = {
    q: query,
    access: "playable,preview,blocked",
    linked_partitioning: "true",
    limit: String(SEARCH_PAGE_SIZE),
  };
  if (resource === "playlists") parameters.show_tracks = "false";
  url.search = new URLSearchParams(parameters).toString();
  return url;
}

async function soundCloudSearchResource(url: URL | undefined, accessToken: string): Promise<Response | undefined> {
  if (!url) return undefined;
  return fetchWithTimeout(url.toString(), {
    headers: {
      accept: "application/json; charset=utf-8",
      authorization: `OAuth ${accessToken}`,
    },
  });
}

type SearchCursorPayload = {
  tracks: string | null;
  playlists: string | null;
};

function encodeSearchCursor(payload: SearchCursorPayload): string {
  const bytes = new TextEncoder().encode(JSON.stringify(payload));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function validatedSearchPageUrl(raw: unknown, resource: "tracks" | "playlists", query: string): URL | undefined {
  if (typeof raw !== "string" || raw.length === 0 || raw.length > 2048) return undefined;
  try {
    const url = new URL(raw);
    const limit = Number(url.searchParams.get("limit"));
    const forbidden = [...url.searchParams.keys()].some((key) => /token|secret|client_id/i.test(key));
    if (
      url.protocol !== "https:"
      || url.hostname !== "api.soundcloud.com"
      || url.port
      || url.username
      || url.password
      || url.pathname !== `/${resource}`
      || url.hash
      || url.searchParams.get("q") !== query
      || url.searchParams.get("linked_partitioning") !== "true"
      || !Number.isInteger(limit)
      || limit < 1
      || limit > SEARCH_PAGE_SIZE
      || forbidden
    ) return undefined;
    return url;
  } catch {
    return undefined;
  }
}

function decodeSearchCursor(raw: string, query: string): {
  tracks?: URL;
  playlists?: URL;
} {
  if (!/^[A-Za-z0-9_-]+$/.test(raw) || raw.length > MAX_SEARCH_CURSOR_LENGTH) {
    throw new ApiError(400, "invalid_cursor", "Search cursor is invalid");
  }
  try {
    const padded = raw.replaceAll("-", "+").replaceAll("_", "/").padEnd(Math.ceil(raw.length / 4) * 4, "=");
    const binary = atob(padded);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const payload = JSON.parse(new TextDecoder().decode(bytes)) as SearchCursorPayload;
    const tracks = payload.tracks === null ? undefined : validatedSearchPageUrl(payload.tracks, "tracks", query);
    const playlists = payload.playlists === null ? undefined : validatedSearchPageUrl(payload.playlists, "playlists", query);
    if ((payload.tracks !== null && !tracks) || (payload.playlists !== null && !playlists) || (!tracks && !playlists)) {
      throw new Error("invalid cursor resources");
    }
    return { tracks, playlists };
  } catch {
    throw new ApiError(400, "invalid_cursor", "Search cursor is invalid");
  }
}

async function soundCloudSearch(
  query: string,
  accessToken: string,
  cursor?: { tracks?: URL; playlists?: URL },
): Promise<{ tracks?: Response; playlists?: Response }> {
  const trackUrl = cursor
    ? cursor.tracks
    : firstSoundCloudSearchUrl(SOUNDCLOUD_TRACKS_API, query, "tracks");
  const playlistUrl = cursor
    ? cursor.playlists
    : firstSoundCloudSearchUrl(SOUNDCLOUD_PLAYLISTS_API, query, "playlists");
  const [tracks, playlists] = await Promise.all([
    soundCloudSearchResource(trackUrl, accessToken),
    soundCloudSearchResource(playlistUrl, accessToken),
  ]);
  return { tracks, playlists };
}

async function readBoundedBody(response: Response, maximumBytes: number): Promise<Uint8Array> {
  const declaredLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > maximumBytes) {
    throw new ApiError(502, "upstream_response_too_large", "SoundCloud response exceeded the limit");
  }
  if (!response.body) return new Uint8Array();

  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maximumBytes) {
        await reader.cancel().catch(() => undefined);
        throw new ApiError(502, "upstream_response_too_large", "SoundCloud response exceeded the limit");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

async function readMappedSearch(response: Response | undefined): Promise<{ items: PublicTrack[]; nextHref?: string }> {
  if (!response) return { items: [] };
  if (!response.ok) {
    const status = response.status === 401 || response.status === 403 || response.status === 429 ? response.status : 502;
    const code = status === 429 ? "upstream_rate_limited" : status === 401 ? "upstream_unauthorized" : status === 403 ? "upstream_forbidden" : "upstream_error";
    throw new ApiError(status, code, "SoundCloud search request failed");
  }
  const bytes = await readBoundedBody(response, MAX_UPSTREAM_BYTES);
  let payload: { collection?: WorkerTrack[]; next_href?: string };
  try {
    payload = JSON.parse(new TextDecoder().decode(bytes)) as { collection?: WorkerTrack[]; next_href?: string };
  } catch {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid metadata");
  }
  if (!Array.isArray(payload.collection)) {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid metadata");
  }
  return { items: payload.collection
    .slice(0, SEARCH_PAGE_SIZE)
    .map(mapTrack)
    .filter((track): track is PublicTrack => Boolean(track)), nextHref: payload.next_href };
}

async function readMappedPlaylistSearch(response: Response | undefined): Promise<{ items: PublicPlaylist[]; nextHref?: string }> {
  if (!response) return { items: [] };
  if (!response.ok) {
    const status = response.status === 401 || response.status === 403 || response.status === 429 ? response.status : 502;
    const code = status === 429 ? "upstream_rate_limited" : status === 401 ? "upstream_unauthorized" : status === 403 ? "upstream_forbidden" : "upstream_error";
    throw new ApiError(status, code, "SoundCloud playlist search request failed");
  }
  const bytes = await readBoundedBody(response, MAX_UPSTREAM_BYTES);
  let payload: { collection?: WorkerPlaylist[]; next_href?: string };
  try {
    payload = JSON.parse(new TextDecoder().decode(bytes)) as { collection?: WorkerPlaylist[]; next_href?: string };
  } catch {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid playlist metadata");
  }
  if (!Array.isArray(payload.collection)) {
    throw new ApiError(502, "upstream_invalid_json", "SoundCloud returned invalid playlist metadata");
  }
  return { items: payload.collection
    .slice(0, SEARCH_PAGE_SIZE)
    .map((playlist) => mapPlaylist(playlist))
    .filter((playlist): playlist is PublicPlaylist => Boolean(playlist)), nextHref: payload.next_href };
}

async function search(request: Request, env: Env): Promise<Response> {
  const query = validateSearchQuery(new URL(request.url).searchParams.get("q"));
  const cursorValue = new URL(request.url).searchParams.get("cursor");
  const cursor = cursorValue ? decodeSearchCursor(cursorValue, query) : undefined;
  await checkRateLimit(request, env);
  let token = await leaseToken(env);
  let responses = await soundCloudSearch(query, token, cursor);
  if (responses.tracks?.status === 401 || responses.playlists?.status === 401) {
    token = await leaseToken(env, true);
    responses = await soundCloudSearch(query, token, cursor);
  } else if (
    (responses.tracks && responses.tracks.status >= 500 && responses.tracks.status <= 599)
    || (responses.playlists && responses.playlists.status >= 500 && responses.playlists.status <= 599)
  ) {
    responses = await soundCloudSearch(query, token, cursor);
  }
  const [trackPage, playlistPage] = await Promise.all([
    readMappedSearch(responses.tracks),
    readMappedPlaylistSearch(responses.playlists),
  ]);
  const nextTrackUrl = trackPage.nextHref
    ? validatedSearchPageUrl(trackPage.nextHref, "tracks", query)
    : undefined;
  const nextPlaylistUrl = playlistPage.nextHref
    ? validatedSearchPageUrl(playlistPage.nextHref, "playlists", query)
    : undefined;
  if ((trackPage.nextHref && !nextTrackUrl) || (playlistPage.nextHref && !nextPlaylistUrl)) {
    throw new ApiError(502, "upstream_invalid_pagination", "SoundCloud returned an invalid search cursor");
  }
  const nextCursor = nextTrackUrl || nextPlaylistUrl
    ? encodeSearchCursor({
      tracks: nextTrackUrl?.toString() ?? null,
      playlists: nextPlaylistUrl?.toString() ?? null,
    })
    : undefined;
  // `collection` is retained for backward compatibility with C3 clients.
  const body = { collection: trackPage.items, playlists: playlistPage.items, next_cursor: nextCursor };
  if (new TextEncoder().encode(JSON.stringify(body)).byteLength > MAX_RESPONSE_BYTES) {
    throw new ApiError(502, "response_too_large", "Mapped search response exceeded the limit");
  }
  return json(body);
}

const worker: ExportedHandler<Env> = {
  async fetch(request, env): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/health") {
      return json({ status: "ok", service: "brickwave-api", api: "soundcloud-public-metadata" });
    }
    if (request.method === "GET" && url.pathname === "/search") {
      try {
        return await search(request, env);
      } catch (error) {
        return errorResponse(error);
      }
    }
    if (request.method === "POST" && url.pathname === "/auth/device/start") {
      try {
        await checkRateLimit(request, env, "pairing");
        const response = await callAuthStore(env, "/start", { caller_key: await hashedClientKey(request) });
        const payload = (await response.json()) as Record<string, unknown>;
        if (!response.ok) return json(payload, response.status);
        const pairingId = payload.pairing_id;
        if (!validOpaqueId(pairingId)) throw new ApiError(502, "pairing_invalid", "Pairing service returned an invalid response");
        return json({ ...payload, pairing_url: pairingUrl(request, pairingId) });
      } catch (error) {
        return errorResponse(error);
      }
    }
    if (request.method === "GET" && url.pathname === "/auth/device/status") {
      const pairingId = url.searchParams.get("pairing_id");
      const deviceSecret = parseBearer(request);
      if (!pairingId || !deviceSecret) return json({ error: { code: "device_proof_required", message: "Device proof is required" } }, 401);
      return authStoreJson(env, "/status", { pairing_id: pairingId, device_secret: deviceSecret });
    }
    if (request.method === "POST" && (url.pathname === "/auth/device/complete" || url.pathname === "/auth/device/cancel")) {
      try {
        const body = (await request.json()) as Record<string, unknown>;
        const deviceSecret = parseBearer(request);
        if (!deviceSecret) return json({ error: { code: "device_proof_required", message: "Device proof is required" } }, 401);
        return await authStoreJson(env, url.pathname.endsWith("complete") ? "/complete" : "/cancel", {
          pairing_id: body.pairing_id,
          confirmation_code: body.confirmation_code,
          device_secret: deviceSecret,
        });
      } catch {
        return json({ error: { code: "invalid_request", message: "Pairing request is invalid" } }, 400);
      }
    }
    if (request.method === "GET" && url.pathname === "/auth/connect") {
      const pairingId = url.searchParams.get("pairing");
      if (!pairingId) return browserErrorPage("Open the pairing page from Brickwave and try again.");
      const browserNonce = cookieValue(request, "__Host-brickwave-auth") ?? base64UrlRandom(32);
      const response = await callAuthStore(env, "/browser-open", { pairing_id: pairingId, browser_nonce: browserNonce });
      if (!response.ok) {
        if (response.status === 410) {
          return browserErrorPage("This pairing request has expired. Create a new QR code in Brickwave.");
        }
        if (response.status === 409) {
          return browserErrorPage("Authorization has already started in another browser. Create a new QR code in Brickwave.");
        }
        return browserErrorPage("The pairing service is temporarily unavailable. Create a new QR code in Brickwave.");
      }
      const payload = (await response.json()) as { confirmation_code?: string };
      if (!payload.confirmation_code) return browserErrorPage("The pairing service returned an invalid response.");
      const headers = authNoStoreHeaders({
        "content-type": "text/html; charset=utf-8",
        "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
      });
      headers.append("set-cookie", `__Host-brickwave-auth=${browserNonce}; Path=/; Max-Age=900; Secure; HttpOnly; SameSite=Lax`);
      headers.append("set-cookie", `__Host-brickwave-pair=${pairingId}; Path=/; Max-Age=900; Secure; HttpOnly; SameSite=Lax`);
      return new Response(phonePairingPage(payload.confirmation_code), { headers });
    }
    if ((request.method === "GET" || request.method === "POST") && url.pathname === "/auth/authorize") {
      const pairingId = cookieValue(request, "__Host-brickwave-pair");
      const browserNonce = cookieValue(request, "__Host-brickwave-auth");
      if (!pairingId || !browserNonce) return browserErrorPage("Open the pairing page from Brickwave and try again.");
      const response = await callAuthStore(env, "/authorize", { pairing_id: pairingId, browser_nonce: browserNonce });
      if (!response.ok) return browserErrorPage("This pairing request cannot start authorization. Create a new QR code in Brickwave.");
      const payload = (await response.json()) as { oauth_state?: string; code_challenge?: string };
      if (!payload.oauth_state || !payload.code_challenge) return browserErrorPage("The authorization request is invalid.");
      const authorization = new URL("https://secure.soundcloud.com/authorize");
      authorization.search = new URLSearchParams({
        client_id: env.SOUNDCLOUD_CLIENT_ID,
        redirect_uri: authRedirectUri(env),
        response_type: "code",
        code_challenge: payload.code_challenge,
        code_challenge_method: "S256",
        state: payload.oauth_state,
        display: "popup",
      }).toString();
      return Response.redirect(authorization.toString(), 302);
    }
    if (request.method === "GET" && url.pathname === "/auth/callback") {
      const code = url.searchParams.get("code");
      const oauthState = url.searchParams.get("state");
      const browserNonce = cookieValue(request, "__Host-brickwave-auth");
      if (!code || !oauthState || !browserNonce || code.length > 4096) return browserErrorPage("Authorization was rejected or expired. Create a new QR code in Brickwave.");
      const begin = await callAuthStore(env, "/callback-begin", { oauth_state: oauthState, browser_nonce: browserNonce });
      if (!begin.ok) return browserErrorPage("Authorization state is invalid or was already used.");
      const callback = (await begin.json()) as { pairing_id?: string; pkce_verifier?: string };
      if (!callback.pairing_id || !callback.pkce_verifier) return browserErrorPage("Authorization callback is invalid.");
      try {
        const issued = await exchangeAuthorizationCode(env, code, callback.pkce_verifier);
        const profile = await fetchCurrentUser(issued.accessToken);
        const complete = await callAuthStore(env, "/callback-success", {
          pairing_id: callback.pairing_id,
          user_token: {
            accessToken: issued.accessToken,
            refreshToken: issued.refreshToken,
            expiresAtMs: Date.now() + issued.expiresInSeconds * 1000,
          },
          profile,
        });
        if (!complete.ok) return browserErrorPage("SoundCloud authorization could not be completed.");
        return new Response("<!doctype html><meta charset=\"utf-8\"><title>Connected · Brickwave</title><h1>SoundCloud connected</h1><p>Return to Brickwave and confirm the matching code on your device.</p>", {
          headers: authNoStoreHeaders({ "content-type": "text/html; charset=utf-8", "content-security-policy": "default-src 'none'; base-uri 'none'; frame-ancestors 'none'", "clear-site-data": "\"cookies\"" }),
        });
      } catch {
        return browserErrorPage("SoundCloud authorization failed. Return to Brickwave and create a new QR code.");
      }
    }
    if (request.method === "GET" && url.pathname === "/auth/me") {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      return authStoreJson(env, "/me", { session_id: sessionId, session_secret: sessionSecret });
    }
    if (request.method === "GET" && url.pathname === "/auth/library") {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      return authStoreJson(
        env,
        "/library",
        { session_id: sessionId, session_secret: sessionSecret },
        MAX_RESPONSE_BYTES,
      );
    }
    if (request.method === "POST" && url.pathname === "/auth/playlists") {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let body: Record<string, unknown>;
      try {
        body = await request.json() as Record<string, unknown>;
      } catch {
        return json({ error: { code: "invalid_request", message: "Request body is invalid" } }, 400);
      }
      let title: string;
      try {
        title = validatePlaylistTitle(body.title);
        await checkRateLimit(request, env, "playlist create");
      } catch (error) {
        return errorResponse(error);
      }
      return authStoreJson(env, "/playlist-create", {
        session_id: sessionId,
        session_secret: sessionSecret,
        title,
      });
    }
    const playlistDeleteMatch = request.method === "DELETE"
      ? /^\/auth\/playlists\/([^/]+)$/.exec(url.pathname)
      : null;
    if (playlistDeleteMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let playlistUrn: string;
      try {
        playlistUrn = decodeURIComponent(playlistDeleteMatch[1]);
      } catch {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      if (!validResourceUrn(playlistUrn, "playlists")) {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      try {
        await checkRateLimit(request, env, "playlist delete");
        return await authStoreJson(env, "/playlist-delete", {
          session_id: sessionId,
          session_secret: sessionSecret,
          playlist_urn: playlistUrn,
        });
      } catch (error) {
        return errorResponse(error);
      }
    }
    const playlistTracksMatch = request.method === "GET"
      ? /^\/auth\/playlists\/([^/]+)\/tracks$/.exec(url.pathname)
      : null;
    if (playlistTracksMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let playlistUrn: string;
      try {
        playlistUrn = decodeURIComponent(playlistTracksMatch[1]);
      } catch {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      if (!validResourceUrn(playlistUrn, "playlists")) {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      return authStoreJson(
        env,
        "/playlist-tracks",
        { session_id: sessionId, session_secret: sessionSecret, playlist_urn: playlistUrn },
        MAX_RESPONSE_BYTES,
      );
    }
    const playlistAddTrackMatch = request.method === "POST"
      ? /^\/auth\/playlists\/([^/]+)\/tracks$/.exec(url.pathname)
      : null;
    if (playlistAddTrackMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let playlistUrn: string;
      try {
        playlistUrn = decodeURIComponent(playlistAddTrackMatch[1]);
      } catch {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      if (!validResourceUrn(playlistUrn, "playlists")) {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      let body: Record<string, unknown>;
      try {
        body = await request.json() as Record<string, unknown>;
      } catch {
        return json({ error: { code: "invalid_request", message: "Request body is invalid" } }, 400);
      }
      const trackUrn = validResourceUrn(body.track_urn, "tracks");
      if (!trackUrn) return json({ error: { code: "invalid_track", message: "Track URN is invalid" } }, 400);
      try {
        await checkRateLimit(request, env, "playlist update");
        return await authStoreJson(env, "/playlist-add-track", {
          session_id: sessionId,
          session_secret: sessionSecret,
          playlist_urn: playlistUrn,
          track_urn: trackUrn,
        });
      } catch (error) {
        return errorResponse(error);
      }
    }
    const playlistRemoveTrackMatch = request.method === "DELETE"
      ? /^\/auth\/playlists\/([^/]+)\/tracks$/.exec(url.pathname)
      : null;
    if (playlistRemoveTrackMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let playlistUrn: string;
      try {
        playlistUrn = decodeURIComponent(playlistRemoveTrackMatch[1]);
      } catch {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      if (!validResourceUrn(playlistUrn, "playlists")) {
        return json({ error: { code: "invalid_playlist", message: "Playlist URN is invalid" } }, 400);
      }
      let body: Record<string, unknown>;
      try {
        body = await request.json() as Record<string, unknown>;
      } catch {
        return json({ error: { code: "invalid_request", message: "Request body is invalid" } }, 400);
      }
      const trackUrn = validResourceUrn(body.track_urn, "tracks");
      const trackIndex = typeof body.track_index === "number" && Number.isSafeInteger(body.track_index)
        ? body.track_index
        : -1;
      if (!trackUrn || trackIndex < 0) {
        return json({ error: { code: "invalid_track", message: "Track selection is invalid" } }, 400);
      }
      try {
        await checkRateLimit(request, env, "playlist update");
        return await authStoreJson(env, "/playlist-remove-track", {
          session_id: sessionId,
          session_secret: sessionSecret,
          playlist_urn: playlistUrn,
          track_urn: trackUrn,
          track_index: trackIndex,
        });
      } catch (error) {
        return errorResponse(error);
      }
    }
    const trackLikeMatch = request.method === "POST" || request.method === "DELETE"
      ? /^\/auth\/tracks\/([^/]+)\/like$/.exec(url.pathname)
      : null;
    if (trackLikeMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let trackUrn: string;
      try {
        trackUrn = decodeURIComponent(trackLikeMatch[1]);
      } catch {
        return json({ error: { code: "invalid_track", message: "Track URN is invalid" } }, 400);
      }
      if (!validResourceUrn(trackUrn, "tracks")) {
        return json({ error: { code: "invalid_track", message: "Track URN is invalid" } }, 400);
      }
      try {
        await checkRateLimit(request, env, "like update");
        return await authStoreJson(env, "/track-like", {
          session_id: sessionId,
          session_secret: sessionSecret,
          track_urn: trackUrn,
          liked: request.method === "POST",
        });
      } catch (error) {
        return errorResponse(error);
      }
    }
    const trackStreamMatch = request.method === "GET"
      ? /^\/auth\/tracks\/([^/]+)\/stream$/.exec(url.pathname)
      : null;
    if (trackStreamMatch) {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      let trackUrn: string;
      try {
        trackUrn = decodeURIComponent(trackStreamMatch[1]);
      } catch {
        return json({ error: { code: "invalid_track", message: "Track URN is invalid" } }, 400);
      }
      if (!validResourceUrn(trackUrn, "tracks")) {
        return json({ error: { code: "invalid_track", message: "Track URN is invalid" } }, 400);
      }
      try {
        await checkRateLimit(request, env, "stream descriptor");
        return await authStoreJson(
          env,
          "/stream-descriptor",
          { session_id: sessionId, session_secret: sessionSecret, track_urn: trackUrn },
          MAX_STREAM_DESCRIPTOR_BYTES,
        );
      } catch (error) {
        return errorResponse(error);
      }
    }
    if (request.method === "POST" && url.pathname === "/auth/logout") {
      const sessionId = request.headers.get("x-brickwave-session");
      const sessionSecret = parseBearer(request);
      if (!sessionId || !sessionSecret) return json({ error: { code: "session_required", message: "Account session is required" } }, 401);
      return authStoreJson(env, "/logout", { session_id: sessionId, session_secret: sessionSecret });
    }
    return json({ error: { code: "not_found", message: "Not found" } }, 404);
  },
};

export default worker;
