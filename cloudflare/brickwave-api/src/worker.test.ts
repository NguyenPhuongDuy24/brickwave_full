import { beforeEach, describe, expect, it, vi } from "vitest";

import worker, {
  AuthSessionStore,
  RequestGuard,
  TokenCoordinator,
  mapPlaylist,
  mapTrack,
  pkceChallenge,
  validateSearchQuery,
  type Env,
  type PublicTrack,
} from "./worker";

class MemoryStorage {
  readonly values = new Map<string, unknown>();
  alarmAt: number | undefined;

  async get<T>(key: string): Promise<T | undefined> {
    return this.values.get(key) as T | undefined;
  }

  async put<T>(key: string, value: T): Promise<void> {
    this.values.set(key, value);
  }

  async delete(key: string): Promise<boolean> {
    return this.values.delete(key);
  }

  async list<T>(options?: { prefix?: string }): Promise<Map<string, T>> {
    return new Map(
      [...this.values.entries()].filter(([key]) => !options?.prefix || key.startsWith(options.prefix)),
    ) as Map<string, T>;
  }

  async setAlarm(scheduledTime: number): Promise<void> {
    this.alarmAt = scheduledTime;
  }

  async deleteAll(): Promise<void> {
    this.values.clear();
  }
}

function coordinatorEnvironment(): Env {
  return {
    SOUNDCLOUD_CLIENT_ID: "test-client-id",
    SOUNDCLOUD_CLIENT_SECRET: "test-client-secret",
    AUTH_REDIRECT_URI: "https://worker.example/auth/callback",
  } as Env;
}

function coordinator(storage = new MemoryStorage()): { coordinator: TokenCoordinator; storage: MemoryStorage } {
  const state = { storage } as unknown as DurableObjectState;
  return { coordinator: new TokenCoordinator(state, coordinatorEnvironment()), storage };
}

function tokenResponse(accessToken: string, refreshToken: string, expiresIn = 3600): Response {
  return Response.json({ access_token: accessToken, refresh_token: refreshToken, expires_in: expiresIn });
}

function publicEnvironment(options?: { rateStatus?: number }): Env {
  const tokenStub = {
    fetch: vi.fn().mockImplementation(() => Promise.resolve(Response.json({ access_token: "internal-access-token" }))),
  };
  const guardStub = {
    fetch: vi.fn().mockResolvedValue(new Response(JSON.stringify({ allowed: true }), { status: options?.rateStatus ?? 200 })),
  };
  const env = {
    SOUNDCLOUD_CLIENT_ID: "not-exposed",
    SOUNDCLOUD_CLIENT_SECRET: "not-exposed",
    AUTH_REDIRECT_URI: "https://worker.example/auth/callback",
    TOKEN_COORDINATOR: {
      idFromName: vi.fn().mockReturnValue("token-id"),
      get: vi.fn().mockReturnValue(tokenStub),
    },
    REQUEST_GUARD: {
      idFromName: vi.fn().mockReturnValue("guard-id"),
      get: vi.fn().mockReturnValue(guardStub),
    },
  } as unknown as Env;
  const authStorage = new MemoryStorage();
  const authService = new AuthSessionStore({ storage: authStorage } as unknown as DurableObjectState, env);
  env.AUTH_SESSION_STORE = {
    idFromName: vi.fn().mockReturnValue("auth-id"),
    get: vi.fn().mockReturnValue({ fetch: (input: RequestInfo | URL, init?: RequestInit) => authService.fetch(new Request(input, init)) }),
  } as unknown as DurableObjectNamespace;
  return env;
}

function authStore(storage = new MemoryStorage()): { service: AuthSessionStore; storage: MemoryStorage; env: Env } {
  const env = publicEnvironment();
  const service = new AuthSessionStore({ storage } as unknown as DurableObjectState, env);
  return { service, storage, env };
}

async function startPairing(service: AuthSessionStore): Promise<Record<string, unknown>> {
  const response = await service.fetch(
    new Request("https://auth.internal/start", {
      method: "POST",
      body: JSON.stringify({ caller_key: "a".repeat(64) }),
    }),
  );
  expect(response.status).toBe(200);
  return (await response.json()) as Record<string, unknown>;
}

async function createAuthenticatedSession(
  service: AuthSessionStore,
  storage: MemoryStorage,
  accessToken = "stream-user-access",
): Promise<Record<string, string>> {
  const started = await startPairing(service);
  const key = `pair:${started.pairing_id}`;
  const record = storage.values.get(key) as Record<string, unknown>;
  record.phase = "authorization_complete";
  record.profile = { id: 101, username: "stream-listener" };
  record.userToken = {
    accessToken,
    refreshToken: "stream-user-refresh",
    expiresAtMs: Date.now() + 3600_000,
  };
  storage.values.set(key, record);
  const response = await service.fetch(new Request("https://auth.internal/complete", {
    method: "POST",
    body: JSON.stringify({
      pairing_id: started.pairing_id,
      device_secret: started.device_secret,
      confirmation_code: started.confirmation_code,
    }),
  }));
  expect(response.status).toBe(200);
  return (await response.json()) as Record<string, string>;
}

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("public mapping", () => {
  it("validates bounded search input and returns only safe metadata", () => {
    expect(validateSearchQuery("  ambient  ")).toBe("ambient");
    expect(() => validateSearchQuery(" ")).toThrow("Query is required");
    expect(() => validateSearchQuery("x".repeat(81))).toThrow("Query is too long");
    expect(
      mapTrack({
        id: 7,
        title: "A track",
        metadata_artist: "Artist",
        user: { id: 70, username: "Uploader" },
        duration: 1234,
        artwork_url: "https://i1.sndcdn.com/image.jpg",
        waveform_url: "https://wave.sndcdn.com/wave.png",
        access: "playable",
        genre: "Ambient",
      }),
    ).toEqual({
      id: 7,
      title: "A track",
      metadata_artist: "Artist",
      user: { id: 70, username: "Uploader" },
      duration: 1234,
      artwork_url: "https://i1.sndcdn.com/image.jpg",
      waveform_url: "https://wave.sndcdn.com/wave.png",
      access: "playable",
      genre: "Ambient",
    });
    expect(mapTrack({ id: 0, title: "bad" })).toBeUndefined();
    expect(mapTrack({ id: 8, title: "bad artwork", artwork_url: "http://unsafe.invalid/a.jpg" })?.artwork_url).toBeUndefined();
    expect(mapTrack({ id: 9, title: "bad waveform", waveform_url: "https://evil.example/wave.png" })?.waveform_url).toBeUndefined();
    expect(mapTrack({ id: 10, title: "insecure waveform", waveform_url: "http://wave.sndcdn.com/wave.png" })?.waveform_url).toBeUndefined();
    expect(mapTrack({ urn: "soundcloud:tracks:90071992547409930", title: "URN track" })?.urn)
      .toBe("soundcloud:tracks:90071992547409930");
  });

  it("maps bounded playlist metadata without exposing arbitrary fields", () => {
    expect(mapPlaylist({
      id: 91,
      title: "Night Drive",
      description: "Saved by the listener",
      artwork_url: "https://i1.sndcdn.com/playlist.jpg",
      track_count: 14,
    })).toEqual({
      id: 91,
      title: "Night Drive",
      description: "Saved by the listener",
      artwork_url: "https://i1.sndcdn.com/playlist.jpg",
      track_count: 14,
    });
    expect(mapPlaylist({ id: 0, title: "invalid" })).toBeUndefined();
    expect(mapPlaylist({ urn: "soundcloud:playlists:90071992547409931", title: "URN playlist" })?.urn)
      .toBe("soundcloud:playlists:90071992547409931");
  });
});

describe("Durable Object token coordinator", () => {
  it("reads credentials only from env and shares one concurrent client-credentials exchange", async () => {
    const fetchMock = vi.fn().mockResolvedValue(tokenResponse("token-one", "refresh-one"));
    vi.stubGlobal("fetch", fetchMock);
    const { coordinator: service, storage } = coordinator();
    const [first, second] = await Promise.all([
      service.fetch(new Request("https://token.internal/lease", { method: "POST" })),
      service.fetch(new Request("https://token.internal/lease", { method: "POST" })),
    ]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    expect(fetchMock.mock.calls[0]?.[1]?.headers).toMatchObject({
      authorization: expect.stringMatching(/^Basic /),
    });
    const stored = storage.values.get("soundcloud-token") as { refreshToken: string };
    expect(stored.refreshToken).toBe("refresh-one");
  });

  it("refreshes expired state once and stores the rotated refresh token before reuse", async () => {
    const { coordinator: service, storage } = coordinator();
    storage.values.set("soundcloud-token", {
      accessToken: "expired",
      refreshToken: "refresh-one",
      expiresAtMs: 0,
    });
    const fetchMock = vi.fn().mockResolvedValue(tokenResponse("token-two", "refresh-two"));
    vi.stubGlobal("fetch", fetchMock);
    const response = await service.fetch(new Request("https://token.internal/lease", { method: "POST" }));
    expect(response.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0]?.[1]?.body)).toContain("grant_type=refresh_token");
    expect((storage.values.get("soundcloud-token") as { refreshToken: string }).refreshToken).toBe("refresh-two");
  });
});

describe("Durable Object request guard", () => {
  it("denies the twenty-first request in the same fixed window", async () => {
    const state = { storage: new MemoryStorage() } as unknown as DurableObjectState;
    const guard = new RequestGuard(state);
    for (let attempt = 0; attempt < 20; attempt += 1) {
      expect((await guard.fetch(new Request("https://guard.internal/check", { method: "POST" }))).status).toBe(200);
    }
    expect((await guard.fetch(new Request("https://guard.internal/check", { method: "POST" }))).status).toBe(429);
    expect((state.storage as unknown as MemoryStorage).alarmAt).toBeTypeOf("number");
    await guard.alarm();
    expect((state.storage as unknown as MemoryStorage).values.size).toBe(0);
  });
});

describe("QR OAuth pairing", () => {
  it("generates the RFC 7636 S256 challenge", async () => {
    expect(await pkceChallenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")).toBe(
      "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
    );
  });

  it("returns a QR URL without device proof and rejects unauthenticated polling", async () => {
    const env = publicEnvironment();
    const started = await worker.fetch!(
      new Request("https://brickwave.example/auth/device/start", {
        method: "POST",
        headers: { "cf-connecting-ip": "192.0.2.44" },
      }),
      env,
      {} as ExecutionContext,
    );
    expect(started.status).toBe(200);
    const payload = (await started.json()) as Record<string, string>;
    expect(payload.pairing_url).toContain(`/auth/connect?pairing=${payload.pairing_id}`);
    expect(payload.pairing_url).not.toContain(payload.device_secret);
    expect(payload.pairing_url).not.toContain("secret");

    const pairingPage = await worker.fetch!(
      new Request(payload.pairing_url),
      env,
      {} as ExecutionContext,
    );
    expect(pairingPage.status).toBe(200);
    const pairingHtml = await pairingPage.text();
    expect(pairingHtml).toContain('<form method="post" action="/auth/authorize">');
    expect(pairingHtml).not.toContain('<a href="/auth/authorize">');

    const unauthenticated = await worker.fetch!(
      new Request(`https://brickwave.example/auth/device/status?pairing_id=${payload.pairing_id}`),
      env,
      {} as ExecutionContext,
    );
    expect(unauthenticated.status).toBe(401);
    const authenticated = await worker.fetch!(
      new Request(`https://brickwave.example/auth/device/status?pairing_id=${payload.pairing_id}`, {
        headers: { authorization: `Bearer ${payload.device_secret}` },
      }),
      env,
      {} as ExecutionContext,
    );
    expect(authenticated.status).toBe(200);
    const text = await authenticated.text();
    expect(text).not.toContain(payload.device_secret);
    expect(text).not.toContain("access_token");
    expect(text).not.toContain("refresh_token");
  });

  it("binds one-time OAuth state to the browser cookie proof", async () => {
    const { service } = authStore();
    const started = await startPairing(service);
    const pairingId = String(started.pairing_id);
    const browserNonce = "browser_nonce_abcdefghijklmnopqrstuvwxyz012345";
    expect(
      (
        await service.fetch(
          new Request("https://auth.internal/browser-open", {
            method: "POST",
            body: JSON.stringify({ pairing_id: pairingId, browser_nonce: browserNonce }),
          }),
        )
      ).status,
    ).toBe(200);
    const authorize = await service.fetch(
      new Request("https://auth.internal/authorize", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, browser_nonce: browserNonce }),
      }),
    );
    const authorization = (await authorize.json()) as Record<string, string>;
    expect(authorization.code_challenge).toMatch(/^[A-Za-z0-9_-]{43}$/);

    const wrongBrowser = await service.fetch(
      new Request("https://auth.internal/callback-begin", {
        method: "POST",
        body: JSON.stringify({ oauth_state: authorization.oauth_state, browser_nonce: "wrong_browser_nonce_abcdefghijklmnopqrstuvwxyz" }),
      }),
    );
    expect(wrongBrowser.status).toBe(400);
    const first = await service.fetch(
      new Request("https://auth.internal/callback-begin", {
        method: "POST",
        body: JSON.stringify({ oauth_state: authorization.oauth_state, browser_nonce: browserNonce }),
      }),
    );
    expect(first.status).toBe(200);
    const replay = await service.fetch(
      new Request("https://auth.internal/callback-begin", {
        method: "POST",
        body: JSON.stringify({ oauth_state: authorization.oauth_state, browser_nonce: browserNonce }),
      }),
    );
    expect(replay.status).toBe(400);
  });

  it("lets the real browser replace a QR preview nonce before OAuth and locks it afterwards", async () => {
    const { service } = authStore();
    const started = await startPairing(service);
    const pairingId = String(started.pairing_id);
    const previewNonce = "preview_nonce_abcdefghijklmnopqrstuvwxyz012345";
    const browserNonce = "browser_nonce_abcdefghijklmnopqrstuvwxyz012345";

    const preview = await service.fetch(
      new Request("https://auth.internal/browser-open", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, browser_nonce: previewNonce }),
      }),
    );
    expect(preview.status).toBe(200);

    const browser = await service.fetch(
      new Request("https://auth.internal/browser-open", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, browser_nonce: browserNonce }),
      }),
    );
    expect(browser.status).toBe(200);

    const authorize = await service.fetch(
      new Request("https://auth.internal/authorize", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, browser_nonce: browserNonce }),
      }),
    );
    expect(authorize.status).toBe(200);

    const lateBrowser = await service.fetch(
      new Request("https://auth.internal/browser-open", {
        method: "POST",
        body: JSON.stringify({
          pairing_id: pairingId,
          browser_nonce: "late_browser_nonce_abcdefghijklmnopqrstuvwxyz012345",
        }),
      }),
    );
    expect(lateBrowser.status).toBe(409);
  });

  it("rejects wrong device proof, expired QR and cancelled pairing", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const pairingId = String(started.pairing_id);
    const wrong = await service.fetch(
      new Request("https://auth.internal/status", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, device_secret: "wrong_device_secret_abcdefghijklmnopqrstuvwxyz" }),
      }),
    );
    expect(wrong.status).toBe(401);

    const key = `pair:${pairingId}`;
    const record = storage.values.get(key) as { expiresAtMs: number };
    record.expiresAtMs = 0;
    storage.values.set(key, record);
    const expired = await service.fetch(
      new Request("https://auth.internal/status", {
        method: "POST",
        body: JSON.stringify({ pairing_id: pairingId, device_secret: started.device_secret }),
      }),
    );
    expect(expired.status).toBe(410);

    const active = await startPairing(service);
    const cancelled = await service.fetch(
      new Request("https://auth.internal/cancel", {
        method: "POST",
        body: JSON.stringify({ pairing_id: active.pairing_id, device_secret: active.device_secret }),
      }),
    );
    expect(cancelled.status).toBe(200);
    const afterCancel = await service.fetch(
      new Request("https://auth.internal/status", {
        method: "POST",
        body: JSON.stringify({ pairing_id: active.pairing_id, device_secret: active.device_secret }),
      }),
    );
    expect(afterCancel.status).toBe(410);
  });

  it("requires visual confirmation, isolates session proof, returns no SoundCloud token, and logout invalidates it", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const key = `pair:${started.pairing_id}`;
    const record = storage.values.get(key) as Record<string, unknown>;
    record.phase = "authorization_complete";
    record.profile = { id: 77, username: "real-listener", displayName: "Real Listener", avatarUrl: "https://i1.sndcdn.com/avatar.jpg" };
    record.userToken = { accessToken: "user-access-secret", refreshToken: "user-refresh-secret", expiresAtMs: Date.now() + 3600_000 };
    storage.values.set(key, record);

    const rejected = await service.fetch(
      new Request("https://auth.internal/complete", {
        method: "POST",
        body: JSON.stringify({ pairing_id: started.pairing_id, device_secret: started.device_secret, confirmation_code: "000000" }),
      }),
    );
    expect(rejected.status).toBe(409);
    const completed = await service.fetch(
      new Request("https://auth.internal/complete", {
        method: "POST",
        body: JSON.stringify({ pairing_id: started.pairing_id, device_secret: started.device_secret, confirmation_code: started.confirmation_code }),
      }),
    );
    expect(completed.status).toBe(200);
    const completedText = await completed.text();
    expect(completedText).not.toContain("user-access-secret");
    expect(completedText).not.toContain("user-refresh-secret");
    const appSession = JSON.parse(completedText) as Record<string, string>;

    const wrongTenant = await service.fetch(
      new Request("https://auth.internal/me", {
        method: "POST",
        body: JSON.stringify({ session_id: appSession.session_id, session_secret: "another_installation_secret_abcdefghijklmnopqrstuvwxyz" }),
      }),
    );
    expect(wrongTenant.status).toBe(401);

    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({ id: 77, username: "real-listener", full_name: "Real Listener" })));
    const profile = await service.fetch(
      new Request("https://auth.internal/me", {
        method: "POST",
        body: JSON.stringify({ session_id: appSession.session_id, session_secret: appSession.session_secret }),
      }),
    );
    expect(profile.status).toBe(200);
    expect(await profile.json()).toMatchObject({ profile: { id: 77, username: "real-listener" } });

    const logout = await service.fetch(
      new Request("https://auth.internal/logout", {
        method: "POST",
        body: JSON.stringify({ session_id: appSession.session_id, session_secret: appSession.session_secret }),
      }),
    );
    expect(logout.status).toBe(200);
    const afterLogout = await service.fetch(
      new Request("https://auth.internal/me", {
        method: "POST",
        body: JSON.stringify({ session_id: appSession.session_id, session_secret: appSession.session_secret }),
      }),
    );
    expect(afterLogout.status).toBe(401);
  });

  it("single-flights rotation of an expired single-use user refresh token", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const key = `pair:${started.pairing_id}`;
    const record = storage.values.get(key) as Record<string, unknown>;
    record.phase = "authorization_complete";
    record.profile = { id: 88, username: "rotation-test" };
    record.userToken = { accessToken: "expired-access", refreshToken: "single-use-refresh", expiresAtMs: 0 };
    storage.values.set(key, record);
    const complete = await service.fetch(
      new Request("https://auth.internal/complete", {
        method: "POST",
        body: JSON.stringify({ pairing_id: started.pairing_id, device_secret: started.device_secret, confirmation_code: started.confirmation_code }),
      }),
    );
    const appSession = (await complete.json()) as Record<string, string>;
    const network = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/oauth/token")) return Promise.resolve(tokenResponse("rotated-access", "rotated-refresh"));
      return Promise.resolve(Response.json({ id: 88, username: "rotation-test" }));
    });
    vi.stubGlobal("fetch", network);
    const request = () => service.fetch(
      new Request("https://auth.internal/me", {
        method: "POST",
        body: JSON.stringify({ session_id: appSession.session_id, session_secret: appSession.session_secret }),
      }),
    );
    const [first, second] = await Promise.all([request(), request()]);
    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    expect(network.mock.calls.filter(([input]) => String(input).includes("/oauth/token"))).toHaveLength(1);
    expect(JSON.stringify([...storage.values.values()])).not.toContain("single-use-refresh");
  });

  it("loads liked tracks and playlists with the user token while returning only mapped metadata", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const key = `pair:${started.pairing_id}`;
    const record = storage.values.get(key) as Record<string, unknown>;
    record.phase = "authorization_complete";
    record.profile = { id: 99, username: "library-listener" };
    record.userToken = {
      accessToken: "user-access-secret",
      refreshToken: "user-refresh-secret",
      expiresAtMs: Date.now() + 3600_000,
    };
    storage.values.set(key, record);
    const complete = await service.fetch(
      new Request("https://auth.internal/complete", {
        method: "POST",
        body: JSON.stringify({
          pairing_id: started.pairing_id,
          device_secret: started.device_secret,
          confirmation_code: started.confirmation_code,
        }),
      }),
    );
    const appSession = (await complete.json()) as Record<string, string>;
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth user-access-secret");
      if (url.includes("/me/likes/tracks")) {
        return Promise.resolve(Response.json({ collection: [{
          id: 501,
          title: "Liked track",
          user: { id: 7, username: "Artist" },
          duration: 123000,
          artwork_url: "https://i1.sndcdn.com/liked.jpg",
          access: "playable",
        }] }));
      }
      if (url.includes("/me/playlists")) {
        return Promise.resolve(Response.json({ collection: [{
          id: 601,
          title: "My playlist",
          description: "Account playlist",
          artwork_url: "https://i1.sndcdn.com/playlist.jpg",
          track_count: 8,
          secret_token: "must-not-leave-worker",
        }] }));
      }
      if (url.includes("/me/likes/playlists")) {
        return Promise.resolve(Response.json({ collection: [{
          id: 602,
          title: "Liked playlist",
          track_count: 5,
        }] }));
      }
      if (url.includes("/me/feed/tracks")) {
        return Promise.resolve(Response.json({ collection: [{
          type: "track",
          origin: {
            id: 701,
            title: "Home feed track",
            user: { id: 8, username: "Followed artist" },
            duration: 180000,
            access: "playable",
          },
        }] }));
      }
      if (url.includes("/tracks/soundcloud%3Atracks%3A501/related")) {
        return Promise.resolve(Response.json({ collection: [{
          id: 801,
          title: "Related discovery",
          user: { id: 9, username: "Related artist" },
          duration: 210000,
          access: "playable",
        }] }));
      }
      return Promise.resolve(new Response("{}", { status: 404 }));
    });
    vi.stubGlobal("fetch", upstream);

    const response = await service.fetch(
      new Request("https://auth.internal/library", {
        method: "POST",
        body: JSON.stringify({
          session_id: appSession.session_id,
          session_secret: appSession.session_secret,
        }),
      }),
    );
    expect(response.status).toBe(200);
    const text = await response.text();
    expect(text).toContain("Liked track");
    expect(text).toContain("My playlist");
    expect(text).toContain("Liked playlist");
    expect(text).toContain("Home feed track");
    expect(text).toContain("Related discovery");
    expect(text).not.toContain("user-access-secret");
    expect(text).not.toContain("user-refresh-secret");
    expect(text).not.toContain("must-not-leave-worker");
    const library = JSON.parse(text) as { playlists: Array<{ title: string; editable?: boolean }> };
    expect(library.playlists.find((playlist) => playlist.title === "My playlist")?.editable).toBe(true);
    expect(library.playlists.find((playlist) => playlist.title === "Liked playlist")?.editable).toBeUndefined();
    expect(upstream).toHaveBeenCalledTimes(5);
  });

  it("loads an authenticated playlist in order with bounded official pagination", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const key = `pair:${started.pairing_id}`;
    const record = storage.values.get(key) as Record<string, unknown>;
    record.phase = "authorization_complete";
    record.profile = { id: 99, username: "playlist-listener" };
    record.userToken = {
      accessToken: "playlist-user-access",
      refreshToken: "playlist-user-refresh",
      expiresAtMs: Date.now() + 3600_000,
    };
    storage.values.set(key, record);
    const complete = await service.fetch(
      new Request("https://auth.internal/complete", {
        method: "POST",
        body: JSON.stringify({
          pairing_id: started.pairing_id,
          device_secret: started.device_secret,
          confirmation_code: started.confirmation_code,
        }),
      }),
    );
    const appSession = (await complete.json()) as Record<string, string>;
    const playlistUrn = "soundcloud:playlists:601";
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth playlist-user-access");
      const url = String(input);
      if (url.includes("cursor=second")) {
        return Promise.resolve(Response.json({ collection: [
          { urn: "soundcloud:tracks:503", title: "Third", user: { username: "Artist" }, access: "playable" },
        ] }));
      }
      expect(url).toContain("/playlists/soundcloud%3Aplaylists%3A601/tracks");
      return Promise.resolve(Response.json({
        collection: [
          { urn: "soundcloud:tracks:501", title: "First", user: { username: "Artist" }, access: "playable" },
          { urn: "soundcloud:tracks:502", title: "Second", user: { username: "Artist" }, access: "preview" },
        ],
        next_href: "https://api.soundcloud.com/playlists/soundcloud:playlists:601/tracks?cursor=second",
      }));
    });
    vi.stubGlobal("fetch", upstream);

    const response = await service.fetch(
      new Request("https://auth.internal/playlist-tracks", {
        method: "POST",
        body: JSON.stringify({
          session_id: appSession.session_id,
          session_secret: appSession.session_secret,
          playlist_urn: playlistUrn,
        }),
      }),
    );
    expect(response.status).toBe(200);
    const body = await response.json() as { playlist_urn: string; tracks: PublicTrack[] };
    expect(body.playlist_urn).toBe(playlistUrn);
    expect(body.tracks.map((track) => track.urn)).toEqual([
      "soundcloud:tracks:501",
      "soundcloud:tracks:502",
      "soundcloud:tracks:503",
    ]);
    expect(upstream).toHaveBeenCalledTimes(2);
    const encoded = JSON.stringify(body);
    expect(encoded).not.toContain("playlist-user-access");
    expect(encoded).not.toContain("playlist-user-refresh");
  });

  it("likes and unlikes a track through the user session without exposing OAuth", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "like-user-access");
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      expect(String(input)).toBe("https://api.soundcloud.com/likes/tracks/soundcloud%3Atracks%3A501");
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth like-user-access");
      expect(["POST", "DELETE"]).toContain(init?.method);
      return Promise.resolve(Response.json({}));
    });
    vi.stubGlobal("fetch", upstream);

    for (const liked of [true, false]) {
      const response = await service.fetch(new Request("https://auth.internal/track-like", {
        method: "POST",
        body: JSON.stringify({
          session_id: appSession.session_id,
          session_secret: appSession.session_secret,
          track_urn: "soundcloud:tracks:501",
          liked,
        }),
      }));
      expect(response.status).toBe(200);
      const text = await response.text();
      expect(text).toContain(`\"liked\":${liked}`);
      expect(text).not.toContain("like-user-access");
    }
    expect(upstream.mock.calls.map(([, init]) => (init as RequestInit).method)).toEqual(["POST", "DELETE"]);
  });

  it("adds a track only after verifying an owned complete playlist", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-write-access");
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth playlist-write-access");
      if (url.pathname === "/playlists/soundcloud%3Aplaylists%3A601" && init?.method === "PUT") {
        const payload = JSON.parse(String(init.body)) as { playlist: { tracks: Array<{ urn: string }> } };
        expect(payload.playlist.tracks.map((track) => track.urn)).toEqual([
          "soundcloud:tracks:501",
          "soundcloud:tracks:502",
          "soundcloud:tracks:503",
        ]);
        return Promise.resolve(Response.json({ urn: "soundcloud:playlists:601" }));
      }
      if (url.pathname === "/playlists/soundcloud%3Aplaylists%3A601/tracks") {
        return Promise.resolve(Response.json({ collection: [
          { urn: "soundcloud:tracks:501", title: "First" },
          { urn: "soundcloud:tracks:502", title: "Second" },
        ] }));
      }
      return Promise.resolve(Response.json({
        urn: "soundcloud:playlists:601",
        title: "Owned playlist",
        track_count: 2,
        user: { id: 101, urn: "soundcloud:users:101" },
      }));
    });
    vi.stubGlobal("fetch", upstream);

    const response = await service.fetch(new Request("https://auth.internal/playlist-add-track", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        playlist_urn: "soundcloud:playlists:601",
        track_urn: "soundcloud:tracks:503",
      }),
    }));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ track_count: 3 });
    expect(upstream.mock.calls.some(([, init]) => (init as RequestInit)?.method === "PUT")).toBe(true);
  });

  it("removes the exact selected playlist occurrence and preserves track order", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-remove-access");
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth playlist-remove-access");
      if (url.pathname === "/playlists/soundcloud%3Aplaylists%3A601" && init?.method === "PUT") {
        const payload = JSON.parse(String(init.body)) as { playlist: { tracks: Array<{ urn: string }> } };
        expect(payload.playlist.tracks.map((track) => track.urn)).toEqual([
          "soundcloud:tracks:501",
          "soundcloud:tracks:502",
        ]);
        return Promise.resolve(Response.json({ urn: "soundcloud:playlists:601" }));
      }
      if (url.pathname === "/playlists/soundcloud%3Aplaylists%3A601/tracks") {
        return Promise.resolve(Response.json({ collection: [
          { urn: "soundcloud:tracks:501", title: "First occurrence" },
          { urn: "soundcloud:tracks:502", title: "Middle" },
          { urn: "soundcloud:tracks:501", title: "Second occurrence" },
        ] }));
      }
      return Promise.resolve(Response.json({
        urn: "soundcloud:playlists:601",
        title: "Owned playlist",
        track_count: 3,
        user: { id: 101, urn: "soundcloud:users:101" },
      }));
    });
    vi.stubGlobal("fetch", upstream);

    const response = await service.fetch(new Request("https://auth.internal/playlist-remove-track", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        playlist_urn: "soundcloud:playlists:601",
        track_urn: "soundcloud:tracks:501",
        track_index: 2,
      }),
    }));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ track_index: 2, track_count: 2 });
    expect(upstream.mock.calls.map(([, init]) => (init as RequestInit)?.method ?? "GET"))
      .toEqual(["GET", "GET", "PUT"]);
  });

  it("rejects a stale playlist row instead of deleting another occurrence", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-remove-access");
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input));
      if (url.pathname.endsWith("/tracks")) {
        return Promise.resolve(Response.json({ collection: [
          { urn: "soundcloud:tracks:501", title: "First" },
          { urn: "soundcloud:tracks:502", title: "Second" },
        ] }));
      }
      return Promise.resolve(Response.json({
        urn: "soundcloud:playlists:601",
        title: "Owned playlist",
        track_count: 2,
        user: { id: 101, urn: "soundcloud:users:101" },
      }));
    });
    vi.stubGlobal("fetch", upstream);

    const response = await service.fetch(new Request("https://auth.internal/playlist-remove-track", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        playlist_urn: "soundcloud:playlists:601",
        track_urn: "soundcloud:tracks:501",
        track_index: 1,
      }),
    }));
    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ error: { code: "playlist_changed" } });
    expect(upstream.mock.calls.some(([, init]) => (init as RequestInit)?.method === "PUT")).toBe(false);
  });

  it("creates a private playlist and deletes it only after an ownership check", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-manage-access");
    const playlistUrn = "soundcloud:playlists:777";
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth playlist-manage-access");
      if (url.pathname === "/playlists" && init?.method === "POST") {
        const payload = JSON.parse(String(init.body)) as {
          playlist: { title: string; sharing: string; tracks: unknown[] };
        };
        expect(payload).toEqual({
          playlist: { title: "Brick favourites", sharing: "private", tracks: [] },
        });
        return Promise.resolve(Response.json({
          urn: playlistUrn,
          title: "Brick favourites",
          track_count: 0,
          user: { id: 101, urn: "soundcloud:users:101" },
        }, { status: 201 }));
      }
      if (url.pathname === "/playlists/soundcloud%3Aplaylists%3A777" && init?.method === "DELETE") {
        return Promise.resolve(Response.json({}));
      }
      return Promise.resolve(Response.json({
        urn: playlistUrn,
        title: "Brick favourites",
        track_count: 0,
        user: { id: 101, urn: "soundcloud:users:101" },
      }));
    });
    vi.stubGlobal("fetch", upstream);

    const created = await service.fetch(new Request("https://auth.internal/playlist-create", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        title: "  Brick favourites  ",
      }),
    }));
    expect(created.status).toBe(201);
    expect(await created.json()).toMatchObject({
      playlist: { urn: playlistUrn, title: "Brick favourites", track_count: 0, editable: true },
    });

    const deleted = await service.fetch(new Request("https://auth.internal/playlist-delete", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        playlist_urn: playlistUrn,
      }),
    }));
    expect(deleted.status).toBe(200);
    expect(await deleted.json()).toMatchObject({ playlist_urn: playlistUrn, deleted: true });
    expect(upstream.mock.calls.map(([, init]) => (init as RequestInit)?.method ?? "GET"))
      .toEqual(["POST", "GET", "DELETE"]);
  });

  it("rejects invalid playlist names before contacting SoundCloud", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-manage-access");
    const upstream = vi.fn();
    vi.stubGlobal("fetch", upstream);
    for (const title of ["   ", "x".repeat(101)]) {
      const response = await service.fetch(new Request("https://auth.internal/playlist-create", {
        method: "POST",
        body: JSON.stringify({
          session_id: appSession.session_id,
          session_secret: appSession.session_secret,
          title,
        }),
      }));
      expect(response.status).toBe(400);
      expect(await response.json()).toMatchObject({ error: { code: "invalid_playlist_title" } });
    }
    expect(upstream).not.toHaveBeenCalled();
  });

  it("refuses to update a playlist that is not owned by the connected account", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage, "playlist-write-access");
    const upstream = vi.fn().mockResolvedValue(Response.json({
      urn: "soundcloud:playlists:601",
      title: "Someone else's playlist",
      track_count: 1,
      user: { id: 202, urn: "soundcloud:users:202" },
    }));
    vi.stubGlobal("fetch", upstream);
    const response = await service.fetch(new Request("https://auth.internal/playlist-add-track", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        playlist_urn: "soundcloud:playlists:601",
        track_urn: "soundcloud:tracks:503",
      }),
    }));
    expect(response.status).toBe(403);
    expect(upstream.mock.calls.some(([, init]) => (init as RequestInit)?.method === "PUT")).toBe(false);
  });

  it("rejects playlist pagination that leaves the fixed SoundCloud resource", async () => {
    const { service, storage } = authStore();
    const started = await startPairing(service);
    const key = `pair:${started.pairing_id}`;
    const record = storage.values.get(key) as Record<string, unknown>;
    record.phase = "authorization_complete";
    record.profile = { id: 99, username: "playlist-listener" };
    record.userToken = { accessToken: "access", expiresAtMs: Date.now() + 3600_000 };
    storage.values.set(key, record);
    const complete = await service.fetch(new Request("https://auth.internal/complete", {
      method: "POST",
      body: JSON.stringify({ pairing_id: started.pairing_id, device_secret: started.device_secret, confirmation_code: started.confirmation_code }),
    }));
    const appSession = (await complete.json()) as Record<string, string>;
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      collection: [],
      next_href: "https://evil.example/steal",
    })));
    const response = await service.fetch(new Request("https://auth.internal/playlist-tracks", {
      method: "POST",
      body: JSON.stringify({ session_id: appSession.session_id, session_secret: appSession.session_secret, playlist_urn: "soundcloud:playlists:601" }),
    }));
    expect(response.status).toBe(502);
  });

  it("returns a no-store AAC-HLS descriptor without exposing the user OAuth token", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      expect(String(input)).toBe("https://api.soundcloud.com/tracks/soundcloud%3Atracks%3A501/streams");
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth stream-user-access");
      expect(init?.redirect).toBe("manual");
      return Promise.resolve(Response.json({
        hls_aac_160_url: "https://playback.media-streaming.soundcloud.cloud/abc/playlist.m3u8?Policy=signed",
        hls_aac_96_url: "https://playback.media-streaming.soundcloud.cloud/low/playlist.m3u8?Policy=signed",
        hls_mp3_128_url: "https://cf-hls-media.sndcdn.com/playlist/abc.128.mp3/playlist.m3u8?Policy=signed",
      }));
    });
    vi.stubGlobal("fetch", upstream);
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    const text = await response.text();
    expect(JSON.parse(text)).toMatchObject({
      track_urn: "soundcloud:tracks:501",
      format: "hls_aac_160",
      media_url: "https://playback.media-streaming.soundcloud.cloud/abc/playlist.m3u8?Policy=signed",
    });
    expect(text).not.toContain("stream-user-access");
    expect(text).not.toContain("stream-user-refresh");
    expect(upstream).toHaveBeenCalledTimes(1);
  });

  it("falls back to the exact approved MP3-HLS host when AAC is unavailable", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      hls_mp3_128_url: "https://cf-hls-media.sndcdn.com/playlist/legacy.128.mp3/playlist.m3u8?Policy=signed",
      preview_mp3_128_url: "https://cf-preview-media.sndcdn.com/preview/legacy.128.mp3?Policy=signed",
    })));
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({
      track_urn: "soundcloud:tracks:501",
      format: "hls_mp3_128",
      media_url: "https://cf-hls-media.sndcdn.com/playlist/legacy.128.mp3/playlist.m3u8?Policy=signed",
    });
  });

  it("does not use a preview MP3 URL as full-track playback", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      preview_mp3_128_url: "https://cf-preview-media.sndcdn.com/preview/legacy.128.mp3?Policy=signed",
    })));
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(409);
    expect(await response.json()).toMatchObject({ error: { code: "stream_format_unavailable" } });
  });

  it("rejects an MP3-HLS descriptor on the AAC media host", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      hls_mp3_128_url: "https://playback.media-streaming.soundcloud.cloud/wrong/playlist.m3u8?Policy=signed",
    })));
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { code: "stream_url_rejected" } });
  });

  it("resolves exactly one authenticated API hop without forwarding OAuth to the media CDN", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth stream-user-access");
      if (url.endsWith("/streams")) {
        return Promise.resolve(Response.json({
          hls_aac_160_url: "https://api.soundcloud.com/tracks/soundcloud%3Atracks%3A501/streams/stream_id_123/hls",
        }));
      }
      expect(init?.redirect).toBe("manual");
      return Promise.resolve(new Response(null, {
        status: 302,
        headers: { location: "https://playback.media-streaming.soundcloud.cloud/abc/playlist.m3u8?Signature=opaque" },
      }));
    });
    vi.stubGlobal("fetch", upstream);
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(200);
    expect(upstream).toHaveBeenCalledTimes(2);
    expect(await response.json()).toMatchObject({ format: "hls_aac_160" });
  });

  it("resolves an MP3-HLS API hop to the exact approved MP3 CDN", async () => {
    const { service, storage } = authStore();
    const appSession = await createAuthenticatedSession(service, storage);
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      expect((init?.headers as Record<string, string>).authorization).toBe("OAuth stream-user-access");
      if (url.endsWith("/streams")) {
        return Promise.resolve(Response.json({
          hls_mp3_128_url: "https://api.soundcloud.com/tracks/soundcloud%3Atracks%3A501/streams/stream_id_mp3/hls",
        }));
      }
      expect(init?.redirect).toBe("manual");
      return Promise.resolve(new Response(null, {
        status: 302,
        headers: { location: "https://cf-hls-media.sndcdn.com/playlist/legacy.128.mp3/playlist.m3u8?Signature=opaque" },
      }));
    });
    vi.stubGlobal("fetch", upstream);
    const response = await service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({
        session_id: appSession.session_id,
        session_secret: appSession.session_secret,
        track_urn: "soundcloud:tracks:501",
      }),
    }));
    expect(response.status).toBe(200);
    expect(upstream).toHaveBeenCalledTimes(2);
    expect(await response.json()).toMatchObject({ format: "hls_mp3_128" });
  });

  it("rejects unapproved stream hosts and refuses an authenticated manifest proxy", async () => {
    const first = authStore();
    const firstSession = await createAuthenticatedSession(first.service, first.storage);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      hls_aac_160_url: "https://evil.example/playlist.m3u8?token=steal",
    })));
    const rejected = await first.service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({ session_id: firstSession.session_id, session_secret: firstSession.session_secret, track_urn: "soundcloud:tracks:501" }),
    }));
    expect(rejected.status).toBe(502);
    expect(await rejected.json()).toMatchObject({ error: { code: "stream_url_rejected" } });

    const second = authStore();
    const secondSession = await createAuthenticatedSession(second.service, second.storage);
    vi.stubGlobal("fetch", vi.fn()
      .mockResolvedValueOnce(Response.json({
        hls_aac_96_url: "https://api.soundcloud.com/tracks/soundcloud%3Atracks%3A501/streams/stream_id_123/hls",
      }))
      .mockResolvedValueOnce(new Response("#EXTM3U", { status: 200, headers: { "content-type": "application/vnd.apple.mpegurl" } })));
    const proxyRequired = await second.service.fetch(new Request("https://auth.internal/stream-descriptor", {
      method: "POST",
      body: JSON.stringify({ session_id: secondSession.session_id, session_secret: secondSession.session_secret, track_urn: "soundcloud:tracks:501" }),
    }));
    expect(proxyRequired.status).toBe(409);
    expect(await proxyRequired.json()).toMatchObject({ error: { code: "stream_proxy_required" } });
  });

  it("keeps a pending pairing across an AuthSessionStore restart", async () => {
    const storage = new MemoryStorage();
    const first = authStore(storage);
    const started = await startPairing(first.service);
    const restarted = new AuthSessionStore(
      { storage } as unknown as DurableObjectState,
      first.env,
    );
    const status = await restarted.fetch(
      new Request("https://auth.internal/status", {
        method: "POST",
        body: JSON.stringify({
          pairing_id: started.pairing_id,
          device_secret: started.device_secret,
        }),
      }),
    );
    expect(status.status).toBe(200);
    expect(await status.json()).toMatchObject({ phase: "waiting_for_scan" });
  });

  it("rejects a callback without a code and applies the existing caller rate guard to session creation", async () => {
    const callback = await worker.fetch!(
      new Request("https://brickwave.example/auth/callback?state=missing-code"),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(callback.status).toBe(400);
    expect(await callback.text()).not.toContain("access_token");

    const limited = await worker.fetch!(
      new Request("https://brickwave.example/auth/device/start", {
        method: "POST",
        headers: { "cf-connecting-ip": "192.0.2.91" },
      }),
      publicEnvironment({ rateStatus: 429 }),
      {} as ExecutionContext,
    );
    expect(limited.status).toBe(429);
  });
});

describe("public Worker", () => {
  it("serves health without a token and maps a real-backend-shaped search response", async () => {
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input));
      if (url.pathname === "/playlists") {
        return Promise.resolve(Response.json({
          collection: [{
            urn: "soundcloud:playlists:61",
            title: "Fixture ambient playlist",
            description: "Fixture collection",
            artwork_url: "https://i1.sndcdn.com/playlist.jpg",
            track_count: 24,
          }],
        }));
      }
      return Promise.resolve(Response.json({
          collection: [
            {
              id: 51,
              title: "Fixture ambient",
              metadata_artist: "Fixture Artist",
              user: { id: 70, username: "Uploader" },
              duration: 245000,
              artwork_url: "https://i1.sndcdn.com/fixture.jpg",
              access: "playable",
              genre: "Ambient",
            },
          ],
        }));
    });
    vi.stubGlobal("fetch", upstream);
    const env = publicEnvironment();
    const health = await worker.fetch!(new Request("https://brickwave.example/health"), env, {} as ExecutionContext);
    expect(health.status).toBe(200);
    expect(await health.json()).toMatchObject({ status: "ok", service: "brickwave-api" });
    expect(upstream).not.toHaveBeenCalled();

    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.1" } }),
      env,
      {} as ExecutionContext,
    );
    const payload = await response.text();
    expect(response.status).toBe(200);
    expect(payload).toContain("Fixture ambient");
    expect(payload).toContain("Fixture ambient playlist");
    expect(payload).not.toContain("internal-access-token");
    expect(upstream).toHaveBeenCalledTimes(2);
    const requestedUrls = upstream.mock.calls.map(([input]) => new URL(String(input)));
    expect(requestedUrls.every((url) => url.searchParams.get("limit") === "12")).toBe(true);
    expect(requestedUrls.find((url) => url.pathname === "/playlists")?.searchParams.get("show_tracks")).toBe("false");
  });

  it("returns bounded pages and continues both result columns with one validated cursor", async () => {
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input));
      const offset = Number(url.searchParams.get("offset") ?? "0");
      const next = offset === 0
        ? `https://api.soundcloud.com${url.pathname}?q=large&access=playable%2Cpreview%2Cblocked&linked_partitioning=true&limit=12&offset=12`
        : undefined;
      if (url.pathname === "/playlists") {
        return Promise.resolve(Response.json({
          collection: Array.from({ length: 12 }, (_, index) => ({
            urn: `soundcloud:playlists:${1000 + offset + index}`,
            title: `Playlist ${offset + index}`,
            track_count: index,
          })),
          next_href: next,
        }));
      }
      return Promise.resolve(Response.json({
        collection: Array.from({ length: 12 }, (_, index) => ({
          urn: `soundcloud:tracks:${2000 + offset + index}`,
          title: `Track ${offset + index}`,
          metadata_artist: "Artist",
          user: { id: 70, username: "Uploader" },
          duration: 120000,
          access: "playable",
        })),
        next_href: next,
      }));
    });
    vi.stubGlobal("fetch", upstream);
    const firstResponse = await worker.fetch!(
      new Request("https://brickwave.example/search?q=large", { headers: { "cf-connecting-ip": "192.0.2.8" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    const first = await firstResponse.json() as { collection: unknown[]; playlists: unknown[]; next_cursor?: string };
    expect(firstResponse.status).toBe(200);
    expect(first.collection).toHaveLength(12);
    expect(first.playlists).toHaveLength(12);
    expect(first.next_cursor).toMatch(/^[A-Za-z0-9_-]+$/);

    const secondResponse = await worker.fetch!(
      new Request(`https://brickwave.example/search?q=large&cursor=${first.next_cursor}`, { headers: { "cf-connecting-ip": "192.0.2.8" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    const second = await secondResponse.json() as { collection: Array<{ title: string }>; playlists: Array<{ title: string }>; next_cursor?: string };
    expect(secondResponse.status).toBe(200);
    expect(second.collection[0].title).toBe("Track 12");
    expect(second.playlists[0].title).toBe("Playlist 12");
    expect(second.next_cursor).toBeUndefined();
  });

  it("rejects a search cursor that attempts to leave the fixed SoundCloud resources", async () => {
    const upstream = vi.fn();
    vi.stubGlobal("fetch", upstream);
    const raw = btoa(JSON.stringify({
      tracks: "https://evil.example/tracks?q=large&linked_partitioning=true&limit=12",
      playlists: null,
    })).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");

    const response = await worker.fetch!(
      new Request(`https://brickwave.example/search?q=large&cursor=${raw}`, {
        headers: { "cf-connecting-ip": "192.0.2.9" },
      }),
      publicEnvironment(),
      {} as ExecutionContext,
    );

    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ error: { code: "invalid_cursor" } });
    expect(upstream).not.toHaveBeenCalled();
  });

  it.each([401, 403, 429])("returns structured upstream HTTP %i errors", async (status) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}", { status })));
    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.2" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(response.status).toBe(status);
    expect(await response.json()).toMatchObject({ error: { code: expect.any(String) } });
  });

  it("rejects invalid input and rate-limit denial without contacting SoundCloud", async () => {
    const upstream = vi.fn();
    vi.stubGlobal("fetch", upstream);
    const invalid = await worker.fetch!(new Request("https://brickwave.example/search"), publicEnvironment(), {} as ExecutionContext);
    expect(invalid.status).toBe(400);
    const limited = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.3" } }),
      publicEnvironment({ rateStatus: 429 }),
      {} as ExecutionContext,
    );
    expect(limited.status).toBe(429);
    expect(upstream).not.toHaveBeenCalled();
  });

  it("does not expose a token route and bounds an undeclared oversized upstream response", async () => {
    const upstream = vi.fn().mockImplementation(() => Promise.resolve(new Response("x".repeat(1_000_001))));
    vi.stubGlobal("fetch", upstream);
    const token = await worker.fetch!(new Request("https://brickwave.example/token"), publicEnvironment(), {} as ExecutionContext);
    expect(token.status).toBe(404);
    expect(upstream).not.toHaveBeenCalled();

    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.4" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { code: "upstream_response_too_large" } });
  });

  it("makes at most one upstream retry for a transient upstream failure", async () => {
    let trackRequests = 0;
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input));
      if (url.pathname === "/tracks" && trackRequests++ === 0) {
        return Promise.resolve(new Response("{}", { status: 503 }));
      }
      return Promise.resolve(Response.json({ collection: [] }));
    });
    vi.stubGlobal("fetch", upstream);
    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.5" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(response.status).toBe(200);
    expect(upstream).toHaveBeenCalledTimes(4);
  });

  it("releases a forced lease once after a 401 and performs only that one retry", async () => {
    let trackRequests = 0;
    const upstream = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = new URL(String(input));
      if (url.pathname === "/tracks" && trackRequests++ === 0) {
        return Promise.resolve(new Response("{}", { status: 401 }));
      }
      return Promise.resolve(Response.json({ collection: [] }));
    });
    vi.stubGlobal("fetch", upstream);
    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.6" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(response.status).toBe(200);
    expect(upstream).toHaveBeenCalledTimes(4);
  });

  it("maps an upstream timeout to a safe structured response", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new DOMException("timed out", "TimeoutError")));
    const response = await worker.fetch!(
      new Request("https://brickwave.example/search?q=ambient", { headers: { "cf-connecting-ip": "192.0.2.7" } }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(response.status).toBe(504);
    expect(await response.json()).toMatchObject({ error: { code: "upstream_timeout" } });
  });

  it("requires an app session and a valid URN for the playlist tracks route", async () => {
    const unauthenticated = await worker.fetch!(
      new Request("https://brickwave.example/auth/playlists/soundcloud%3Aplaylists%3A601/tracks"),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(unauthenticated.status).toBe(401);

    const invalid = await worker.fetch!(
      new Request("https://brickwave.example/auth/playlists/not-a-urn/tracks", {
        headers: {
          authorization: "Bearer opaque-session-secret",
          "x-brickwave-session": "opaque-session-id",
        },
      }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(invalid.status).toBe(400);

    const unauthenticatedRemove = await worker.fetch!(
      new Request("https://brickwave.example/auth/playlists/soundcloud%3Aplaylists%3A601/tracks", {
        method: "DELETE",
        body: JSON.stringify({ track_urn: "soundcloud:tracks:501", track_index: 0 }),
      }),
      publicEnvironment(),
      {} as ExecutionContext,
    );
    expect(unauthenticatedRemove.status).toBe(401);
  });

  it("routes an authenticated stream descriptor through the existing session store", async () => {
    const env = publicEnvironment();
    const storage = new MemoryStorage();
    const service = new AuthSessionStore({ storage } as unknown as DurableObjectState, env);
    env.AUTH_SESSION_STORE = {
      idFromName: vi.fn().mockReturnValue("auth-id"),
      get: vi.fn().mockReturnValue({
        fetch: (input: RequestInfo | URL, init?: RequestInit) => service.fetch(new Request(input, init)),
      }),
    } as unknown as DurableObjectNamespace;
    const appSession = await createAuthenticatedSession(service, storage);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
      hls_aac_96_url: "https://playback.media-streaming.soundcloud.cloud/live/playlist.m3u8?Policy=signed",
    })));

    const response = await worker.fetch!(
      new Request("https://brickwave.example/auth/tracks/soundcloud%3Atracks%3A501/stream", {
        headers: {
          authorization: `Bearer ${appSession.session_secret}`,
          "x-brickwave-session": appSession.session_id,
          "cf-connecting-ip": "192.0.2.10",
        },
      }),
      env,
      {} as ExecutionContext,
    );
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toMatchObject({
      track_urn: "soundcloud:tracks:501",
      format: "hls_aac_96",
    });
  });
});
