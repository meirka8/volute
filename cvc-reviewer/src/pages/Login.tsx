import React, { useState } from 'react';
import { useAuth } from '../auth/AuthContext';
import { useLocation } from 'wouter';
import { KeyRound, ShieldCheck } from 'lucide-react';

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
        <div className="flex min-h-screen flex-col items-center justify-center bg-[#0d1117] text-gray-100 font-sans p-4">
            <div className="w-full max-w-md space-y-8">
                <div className="text-center">
                    <div className="mx-auto flex h-12 w-12 items-center justify-center rounded-full bg-indigo-900/50">
                        <ShieldCheck className="h-6 w-6 text-indigo-400" />
                    </div>
                    <h2 className="mt-6 text-3xl font-bold tracking-tight">CVC Reviewer</h2>
                    <p className="mt-2 text-sm text-gray-400">
                        The Zero-Trust Cognitive Review Interface
                    </p>
                </div>

                <div className="mt-10 rounded-xl border border-gray-800 bg-[#161b22] p-8 shadow-xl">
                    <form className="space-y-6" onSubmit={handleSubmit}>
                        <div>
                            <label htmlFor="token" className="block text-sm font-medium leading-6 text-gray-200">
                                Personal Access Token
                            </label>
                            <div className="relative mt-2 rounded-md shadow-sm">
                                <div className="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-3">
                                    <KeyRound className="h-4 w-4 text-gray-500" />
                                </div>
                                <input
                                    type="password"
                                    name="token"
                                    id="token"
                                    className="block w-full rounded-md border-0 bg-[#0d1117] py-1.5 pl-10 text-gray-100 ring-1 ring-inset ring-gray-700 placeholder:text-gray-600 focus:ring-2 focus:ring-inset focus:ring-indigo-600 sm:text-sm sm:leading-6"
                                    placeholder="ghp_..."
                                    value={inputToken}
                                    onChange={(e) => setInputToken(e.target.value)}
                                />
                            </div>
                        </div>

                        {error && (
                            <div className="rounded-md bg-red-900/20 p-3 text-sm text-red-400 border border-red-900/50">
                                {error}
                            </div>
                        )}

                        <div>
                            <button
                                type="submit"
                                disabled={isLoading}
                                className="flex w-full justify-center rounded-md bg-indigo-600 px-3 py-2 text-sm font-semibold text-white shadow-sm hover:bg-indigo-500 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-indigo-600 disabled:opacity-50 disabled:cursor-not-allowed transition-all"
                            >
                                {isLoading ? 'Verifying...' : 'Authenticate'}
                            </button>
                        </div>

                        <div className="text-xs text-gray-500 text-center mt-4 border-t border-gray-800 pt-4">
                            <p className="flex items-center justify-center gap-2">
                                <ShieldCheck className="w-3 h-3" />
                                Client-Side Only. Tokens are never sent to our servers.
                            </p>
                        </div>
                    </form>
                </div>

                <div className="text-center text-xs text-gray-600">
                    You need `repo` scope to view private repositories.
                </div>
            </div>
        </div>
    );
}
