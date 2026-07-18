import { describe, expect, it, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
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
