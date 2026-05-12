import React, { useState } from 'react';
import { useAuth } from '../auth/AuthContext';
import { useLocation } from 'wouter';
import { KeyRound, ShieldCheck } from 'lucide-react';
import { ThemeToggle } from '../components/ui/ThemeToggle';

export default function Login() {
    const { login } = useAuth();
    const [inputToken, setInputToken] = useState('');
    const [error, setError] = useState('');
    const [isLoading, setIsLoading] = useState(false);
    const [, setLocation] = useLocation();

    React.useEffect(() => {
        // Check for local proxy mode
        fetch('http://localhost:3000/health')
            .then(res => {
                if (res.ok) {
                    // If local proxy is alive, we might auto-login or switch mode.
                    // For now, simpler: just console log or maybe show a banner.
                    // The story says "skips the login screen". 
                    // That implies the proxy handles auth.
                    // I'll just auto-navigate to app or invoke a special login method.
                    console.log("Local proxy detected");
                    // Implement auto-login logic if proxy exists (mock for now)
                    // login("local-proxy-token"); 
                }
            })
            .catch(() => { /* ignore */ });
    }, []);

    const handleSubmit = async (e: React.FormEvent) => {
        e.preventDefault();
        setError('');
        setIsLoading(true);

        if (!inputToken.trim()) {
            setError('Token is required');
            setIsLoading(false);
            return;
        }

        const success = await login(inputToken);

        if (success) {
            setLocation('/app');
        } else {
            setError('Invalid Personal Access Token. Please check your token and scopes.');
        }
        setIsLoading(false);
    };

    return (
        <div className="relative flex min-h-screen flex-col items-center justify-center p-4 text-ink">
            <div className="fixed right-6 top-6 z-20">
                <ThemeToggle compact />
            </div>

            <div className="w-full max-w-md space-y-8">
                <div className="text-center">
                    <div className="mx-auto flex h-14 w-14 items-center justify-center rounded-full bg-action/10">
                        <ShieldCheck className="h-6 w-6 text-action" />
                    </div>
                    <h2 className="mt-6 font-serif text-4xl font-semibold tracking-tight">CVC Reviewer</h2>
                    <p className="mt-2 text-sm text-muted">
                        The Zero-Trust Cognitive Review Interface
                    </p>
                </div>

                <div className="rr-panel mt-10 rounded-[2rem] p-8 shadow-xl">
                    <form className="space-y-6" onSubmit={handleSubmit}>
                        <div>
                            <label htmlFor="token" className="block text-sm font-medium leading-6 text-ink">
                                Personal Access Token
                            </label>
                            <div className="relative mt-2 rounded-full shadow-sm">
                                <div className="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-3">
                                    <KeyRound className="h-4 w-4 text-muted" />
                                </div>
                                <input
                                    type="password"
                                    name="token"
                                    id="token"
                                    className="block w-full rounded-full border border-line bg-canvas/70 py-3 pl-10 pr-4 text-ink placeholder:text-muted focus:outline-none focus:ring-2 focus:ring-action/30 sm:text-sm sm:leading-6"
                                    placeholder="ghp_..."
                                    value={inputToken}
                                    onChange={(e) => setInputToken(e.target.value)}
                                />
                            </div>
                        </div>

                        {error && (
                            <div className="rounded-[1.25rem] border border-danger/20 bg-danger/10 p-3 text-sm text-danger">
                                {error}
                            </div>
                        )}

                        <div>
                            <button
                                type="submit"
                                disabled={isLoading}
                                className="rr-action-button flex w-full justify-center rounded-full px-3 py-3 text-sm font-semibold transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
                            >
                                {isLoading ? 'Verifying...' : 'Authenticate'}
                            </button>
                        </div>

                        <div className="mt-4 border-t border-line/80 pt-4 text-center text-xs text-muted">
                            <p className="flex items-center justify-center gap-2">
                                <ShieldCheck className="w-3 h-3" />
                                Client-Side Only. Tokens are never sent to our servers.
                            </p>
                        </div>
                    </form>
                </div>

                <div className="text-center text-xs text-muted">
                    You need `repo` scope to view private repositories.
                </div>
            </div>
        </div>
    );
}
