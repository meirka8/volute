/* eslint-disable react-refresh/only-export-components */
import React, { createContext, useContext, useState, useRef, useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { StorageService } from './StorageService';
import { PaymentRequiredError } from './errors';
import { clearReviewerCaches } from '../lib/cognitiveCache';
import { readHostedConfig, type HostedConfig } from '../lib/hostedConfig';

// Use strict types for the context
interface AuthContextType {
    token: string | null;
    isAuthenticated: boolean;
    /** The build asks for hosted mode (VITE_HOSTED_MODE=true), whether or not it says where its platform is. */
    isHostedMode: boolean;
    /** Where the platform is, when the build says (see lib/hostedConfig.ts). */
    hostedConfig: HostedConfig;
    login: (token: string) => Promise<boolean>;
    logout: () => void;
    acquireToken: (owner?: string, repo?: string) => Promise<string>;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

/** Why a hosted build that does not say where its platform is cannot get a token. */
const MISSING_PLATFORM_MESSAGE =
    "Hosted mode is on (VITE_HOSTED_MODE=true), but VITE_PLATFORM_API_URL is missing or is not an http(s) URL, so there is no platform to get a token from.";

export function AuthProvider({ children }: { children: React.ReactNode }) {
    const queryClient = useQueryClient();
    // Each variable is named: `import.meta.env` as a whole would put every VITE_ variable of the build into the bundle.
    const hostedConfig = readHostedConfig({
        VITE_HOSTED_MODE: import.meta.env.VITE_HOSTED_MODE,
        VITE_PLATFORM_API_URL: import.meta.env.VITE_PLATFORM_API_URL,
    });
    // A hosted build that does not say where its platform is stays a hosted build: it takes no PAT, and it calls no
    // platform (App shows it a configuration error instead).
    const isHostedMode = hostedConfig.mode !== 'local';
    const platformBase = hostedConfig.mode === 'hosted' ? hostedConfig.platformBase : null;

    // Initialize from StorageService for local mode
    const [token, setToken] = useState<string | null>(() => {
        return isHostedMode ? null : StorageService.getToken();
    });

    // In hosted mode, we consider the user implicitly "authenticated" on the client side
    // because token fetching is deferred and validated per-repository via the backend session.
    const isAuthenticated = isHostedMode ? true : !!token;

    // Cache structure: repoKey -> { token, expiresAt, timeoutId, promise }
    const tokenCache = useRef<Map<string, { token: string, expiresAt: number, timeoutId?: number, promise?: Promise<string> }>>(new Map());

    // Clear all scheduled refresh timeouts on unmount to prevent stale timers
    // from firing after AuthProvider is removed (navigation, tests, hot reload).
    useEffect(() => {
        const cache = tokenCache.current;

        return () => {
            cache.forEach(entry => {
                if (entry.timeoutId) window.clearTimeout(entry.timeoutId);
            });
            cache.clear();
        };
    }, []);

    const acquireToken = async (owner?: string, repo?: string): Promise<string> => {
        if (!isHostedMode) {
            const currentToken = StorageService.getToken();
            if (!currentToken) throw new Error("No token found in local storage.");
            return currentToken;
        }

        if (platformBase === null) {
            // No platform address is guessed (lib/hostedConfig.ts), so no request leaves for one.
            throw new Error(MISSING_PLATFORM_MESSAGE);
        }

        if (!owner || !repo) {
            throw new Error("Owner and repo are required to acquire a scoped token in Hosted Mode.");
        }

        const repoKey = `${owner}/${repo}`;
        const cached = tokenCache.current.get(repoKey);

        const now = Date.now();
        // If valid token exists and > 5 mins remaining
        if (cached && cached.token && (cached.expiresAt - now > 5 * 60 * 1000)) {
            return cached.token;
        }

        // Prevent concurrent identical requests
        if (cached?.promise) {
            return cached.promise;
        }

        const fetchToken = async (): Promise<string> => {
            try {
                const response = await fetch(`${platformBase}/api/platform/vend-token`, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    credentials: 'include',
                    body: JSON.stringify({ owner, repo })
                });

                if (response.status === 401) {
                    window.location.href = `${platformBase}/login`;
                    throw new Error("Unauthorized");
                }

                if (response.status === 402) {
                    const data = await response.json().catch(() => ({}));
                    throw new PaymentRequiredError(data.message || "Subscription required for private repositories.");
                }

                if (!response.ok) {
                    throw new Error(`Failed to fetch vended token: ${response.status}`);
                }

                const data = await response.json();
                
                // Validate response shape
                if (!data.token || typeof data.token !== 'string') {
                    throw new Error("Invalid token response: token is missing or not a string");
                }
                
                const parsedExpiry = new Date(data.expiresAt).getTime();
                if (!data.expiresAt || !Number.isFinite(parsedExpiry)) {
                    throw new Error("Invalid token response: expiresAt is missing or not parseable");
                }
                
                const newToken = data.token;

                // Clear existing timeout
                if (cached?.timeoutId) {
                    window.clearTimeout(cached.timeoutId);
                }

                // Recompute now after the network round-trip for accurate scheduling
                const nowAfterFetch = Date.now();

                // Validate expiresAt — fall back to a conservative 5 min TTL if unusable
                // (Though we just validated it above, we keep the check to ensure it's in the future)
                const expiresAt = parsedExpiry > nowAfterFetch
                    ? parsedExpiry
                    : nowAfterFetch + 5 * 60 * 1000;

                // Schedule auto-refresh 5 mins before expiry
                const timeUntilRefresh = (expiresAt - nowAfterFetch) - (5 * 60 * 1000);
                let newTimeoutId;
                if (timeUntilRefresh > 0) {
                    newTimeoutId = window.setTimeout(() => {
                        acquireToken(owner, repo).catch(console.error);
                    }, timeUntilRefresh);
                }

                tokenCache.current.set(repoKey, { token: newToken, expiresAt, timeoutId: newTimeoutId });
                return newToken;
            } finally {
                // Clear the inflight promise
                const currentEntry = tokenCache.current.get(repoKey);
                if (currentEntry) {
                    tokenCache.current.set(repoKey, { ...currentEntry, promise: undefined });
                }
            }
        };

        const promise = fetchToken();
        tokenCache.current.set(repoKey, { ...(cached || { token: '', expiresAt: 0, timeoutId: undefined }), promise });
        
        return promise;
    };

    const login = async (newToken: string): Promise<boolean> => {
        if (isHostedMode) return false;
        try {
            const response = await fetch('https://api.github.com/user', {
                headers: {
                    Authorization: `Bearer ${newToken}`,
                    Accept: 'application/vnd.github.v3+json',
                },
            });

            if (response.ok) {
                // A new PAT may represent a different GitHub account. Never let it
                // inherit a previous account's hydrated or IndexedDB graph cache.
                await clearReviewerCaches(queryClient);
                setToken(newToken);
                StorageService.setToken(newToken);
                return true;
            }
            return false;
        } catch (error) {
            console.error("Auth validation failed", error);
            return false;
        }
    };

    const logout = () => {
        // This intentionally runs before hosted navigation as well: a redirect can
        // be interrupted, whereas browser-resident cognitive data must be removed.
        void clearReviewerCaches(queryClient);
        if (isHostedMode) {
            // In hosted mode, log out via the platform. A build that does not say where its platform is has no
            // platform session to end, and guesses no address to go to (lib/hostedConfig.ts).
            if (platformBase !== null) {
                window.location.href = `${platformBase}/logout`;
            }
            return;
        }
        setToken(null);
        StorageService.clear();
        tokenCache.current.forEach(cache => {
            if (cache.timeoutId) window.clearTimeout(cache.timeoutId);
        });
        tokenCache.current.clear();
    };

    return (
        <AuthContext.Provider value={{ token, isAuthenticated, isHostedMode, hostedConfig, login, logout, acquireToken }}>
            {children}
        </AuthContext.Provider>
    );
}

export function useAuth() {
    const context = useContext(AuthContext);
    if (context === undefined) {
        throw new Error('useAuth must be used within an AuthProvider');
    }
    return context;
}
