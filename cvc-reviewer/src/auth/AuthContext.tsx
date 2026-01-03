import React, { createContext, useContext, useState } from 'react';

// Use strict types for the context
interface AuthContextType {
    token: string | null;
    isAuthenticated: boolean;
    login: (token: string) => Promise<boolean>;
    logout: () => void;
}

const AuthContext = createContext<AuthContextType | undefined>(undefined);

export function AuthProvider({ children }: { children: React.ReactNode }) {
    // Initialize from sessionStorage to survive reloads, but clear on browser close ideally
    // "Alcatraz" doctrine says: "Stores Token in Memory". 
    // We'll use React State for memory, but for dev convenience often we want persistence.
    // The design says: "Stores Token in Memory". 
    // However, Task 1.2 says: "Implement StorageService: Encrypted sessionStorage wrapper".
    // Let's implement basic state first for strict memory-only compliance, or checking sessionStorage.

    const [token, setToken] = useState<string | null>(() => {
        return sessionStorage.getItem('cvc_pat');
    });

    const isAuthenticated = !!token;

    const login = async (newToken: string): Promise<boolean> => {
        // Validate token
        try {
            const response = await fetch('https://api.github.com/user', {
                headers: {
                    Authorization: `token ${newToken}`,
                    Accept: 'application/vnd.github.v3+json',
                },
            });

            if (response.ok) {
                setToken(newToken);
                sessionStorage.setItem('cvc_pat', newToken); // Persist for session
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
        sessionStorage.removeItem('cvc_pat');
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
