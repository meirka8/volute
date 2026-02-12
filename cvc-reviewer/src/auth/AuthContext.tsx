import React, { createContext, useContext, useState } from 'react';

// Use strict types for the context
interface AuthContextType {
    token: string | null;
    isAuthenticated: boolean;
    login: (token: string) => Promise<boolean>;
    logout: () => void;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

import { StorageService } from './StorageService';

export function AuthProvider({ children }: { children: React.ReactNode }) {
    // Initialize from StorageService
    const [token, setToken] = useState<string | null>(() => {
        return StorageService.getToken();
    });

    const isAuthenticated = !!token;

    const login = async (newToken: string): Promise<boolean> => {
        // Validate token
        try {
            const response = await fetch('https://api.github.com/user', {
                headers: {
                    Authorization: `Bearer ${newToken}`,
                    Accept: 'application/vnd.github.v3+json',
                },
            });

            if (response.ok) {
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
        setToken(null);
        StorageService.clear();
    };

    return (
        <AuthContext.Provider value={{ token, isAuthenticated, login, logout }}>
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
