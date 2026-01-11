/**
 * StorageService
 * 
 * Provides a wrapper around sessionStorage with basic obfuscation to prevent
 * plain-text secrets from being easily read by casual inspection or simple scrapers.
 * 
 * NOTE: Since this is a client-side only application without a user-provided password
 * to drive key derivation, true encryption is impossible (the key would also be in memory).
 * This obfuscation prevents simple "Right Click -> Application Tab" leakage but does NOT
 * protect against a determined attacker with XSS capabilities who can read the JS memory.
 */

const STORAGE_KEY = 'cvc_secure_session';

// Simple XOR with a localized static key (obfuscation only)
const XOR_KEY = 'cvc-alcatraz-static-salt';

function obfuscate(input: string): string {
    let result = '';
    for (let i = 0; i < input.length; i++) {
        result += String.fromCharCode(input.charCodeAt(i) ^ XOR_KEY.charCodeAt(i % XOR_KEY.length));
    }
    return btoa(result); // Base64 encode
}

function deobfuscate(input: string): string {
    try {
        const raw = atob(input);
        let result = '';
        for (let i = 0; i < raw.length; i++) {
            result += String.fromCharCode(raw.charCodeAt(i) ^ XOR_KEY.charCodeAt(i % XOR_KEY.length));
        }
        return result;
    } catch (e) {
        console.error("Failed to deobfuscate storage", e);
        return '';
    }
}

export const StorageService = {
    setToken: (token: string) => {
        sessionStorage.setItem(STORAGE_KEY, obfuscate(token));
    },

    getToken: (): string | null => {
        const stored = sessionStorage.getItem(STORAGE_KEY);
        if (!stored) return null;
        return deobfuscate(stored);
    },

    clear: () => {
        sessionStorage.removeItem(STORAGE_KEY);
    }
};
