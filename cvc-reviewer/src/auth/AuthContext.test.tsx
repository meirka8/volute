import { describe, expect, it, vi, afterEach, beforeEach } from "vitest";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

const { mockDel } = vi.hoisted(() => ({ mockDel: vi.fn().mockResolvedValue(undefined) }));
vi.mock("idb-keyval", () => ({ del: mockDel }));

import { AuthProvider, useAuth } from "./AuthContext";

function wrapperFor(queryClient: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}><AuthProvider>{children}</AuthProvider></QueryClientProvider>
  );
}

describe("AuthProvider cache boundaries", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    sessionStorage.clear();
  });

  it("clears in-memory and IndexedDB reviewer caches on logout", async () => {
    const queryClient = new QueryClient();
    queryClient.setQueryData(["cvc-node", "owner", "repo", "node"], { secret: true });
    const { result } = renderHook(() => useAuth(), { wrapper: wrapperFor(queryClient) });

    result.current.logout();

    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", "node"])).toBeUndefined();
    await waitFor(() => expect(mockDel).toHaveBeenCalledWith("cvc-reviewer-query-cache"));
  });

  it("clears caches before accepting a replacement PAT/account", async () => {
    const queryClient = new QueryClient();
    queryClient.setQueryData(["cvc-node", "owner", "repo", "node"], { secret: true });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true }));
    const { result } = renderHook(() => useAuth(), { wrapper: wrapperFor(queryClient) });

    await expect(result.current.login("replacement-pat")).resolves.toBe(true);

    expect(queryClient.getQueryData(["cvc-node", "owner", "repo", "node"])).toBeUndefined();
    expect(mockDel).toHaveBeenCalledWith("cvc-reviewer-query-cache");
  });
});

// Hosted mode reaches the platform at the address the build gives (lib/hostedConfig.ts), and nowhere else: a hosted
// build that gives no usable address gets no token, takes no PAT and goes to no guessed address.
describe("AuthProvider platform address", () => {
  const PAGE = "https://reviewer.example/pr/acme/shop/7";
  let location: { href: string };

  beforeEach(() => {
    vi.clearAllMocks();
    location = { href: PAGE };
    vi.stubGlobal("location", location);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  function hosted(platformUrl: string | undefined) {
    vi.stubEnv("VITE_HOSTED_MODE", "true");
    vi.stubEnv("VITE_PLATFORM_API_URL", platformUrl);
  }

  function renderAuth() {
    return renderHook(() => useAuth(), { wrapper: wrapperFor(new QueryClient()) });
  }

  it("asks the platform the build names for a token, with its trailing slash dropped", async () => {
    hosted("https://www.cvc.dev/");
    const expiresAt = new Date(Date.now() + 60 * 60 * 1000).toISOString();
    const fetchSpy = vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({ token: "vended", expiresAt }) });
    vi.stubGlobal("fetch", fetchSpy);
    const { result } = renderAuth();

    expect(result.current.hostedConfig).toEqual({ mode: "hosted", platformBase: "https://www.cvc.dev" });
    await expect(result.current.acquireToken("acme", "shop")).resolves.toBe("vended");
    expect(fetchSpy).toHaveBeenCalledOnce();
    expect(fetchSpy).toHaveBeenCalledWith(
      "https://www.cvc.dev/api/platform/vend-token",
      expect.objectContaining({ method: "POST", credentials: "include", body: JSON.stringify({ owner: "acme", repo: "shop" }) }),
    );
  });

  it("sends a signed-out reader to the platform's sign-in page", async () => {
    hosted("https://www.cvc.dev");
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 401 }));
    const { result } = renderAuth();

    await expect(result.current.acquireToken("acme", "shop")).rejects.toThrow("Unauthorized");
    expect(location.href).toBe("https://www.cvc.dev/login");
  });

  it("signs out on the platform", async () => {
    hosted("https://www.cvc.dev");
    const { result } = renderAuth();

    result.current.logout();

    expect(location.href).toBe("https://www.cvc.dev/logout");
    await waitFor(() => expect(mockDel).toHaveBeenCalledWith("cvc-reviewer-query-cache"));
  });

  it.each<[string, string | undefined]>([
    ["missing", undefined],
    ["blank", "  "],
    ["a host without a scheme", "app.cvc.dev"],
    ["not http(s)", "ftp://www.cvc.dev"],
  ])("guesses no platform when hosted mode is on and the platform URL is %s", async (_label, platformUrl) => {
    hosted(platformUrl);
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    const { result } = renderAuth();

    expect(result.current.hostedConfig).toEqual({ mode: "misconfigured" });
    expect(result.current.isHostedMode).toBe(true);
    await expect(result.current.acquireToken("acme", "shop")).rejects.toThrow(/VITE_PLATFORM_API_URL/);
    await expect(result.current.login("ghp_example")).resolves.toBe(false);
    result.current.logout();

    expect(fetchSpy).not.toHaveBeenCalled();
    expect(location.href).toBe(PAGE);
    await waitFor(() => expect(mockDel).toHaveBeenCalledWith("cvc-reviewer-query-cache"));
  });
});
