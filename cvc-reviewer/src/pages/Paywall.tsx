import { Lock, CreditCard, ChevronRight } from 'lucide-react';
import { useAuth } from '../auth/AuthContext';
import { ThemeToggle } from '../components/ui/ThemeToggle';

export function Paywall() {
    const { isHostedMode } = useAuth();
    // Assuming platform URL is where billing settings live
    const platformUrl = import.meta.env.VITE_PLATFORM_API_URL || 'https://app.cvc.dev';
    const upgradeUrl = `${platformUrl}/dashboard/billing`;

    return (
        <div className="relative flex min-h-screen flex-col items-center justify-center p-4 text-ink">
            <div className="fixed right-6 top-6 z-20">
                <ThemeToggle compact />
            </div>

            <div className="w-full max-w-lg space-y-8">
                <div className="text-center">
                    <div className="mx-auto mb-6 flex h-16 w-16 items-center justify-center rounded-2xl border border-line bg-canvas/80">
                        <Lock className="h-8 w-8 text-muted" />
                    </div>
                    <h2 className="mt-6 font-serif text-4xl font-semibold tracking-tight text-ink">Private Repository Limit</h2>
                    <p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted">
                        You have reached a private repository that is protected. CVC Reviewer requires an active Pro subscription to analyze private codebases.
                    </p>
                </div>

                <div className="rr-panel mt-10 overflow-hidden rounded-[2rem] shadow-2xl">
                    <div className="p-8">
                        <div className="flex items-center gap-4 mb-6">
                            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl bg-action/10">
                                <CreditCard className="h-5 w-5 text-action" />
                            </div>
                            <div>
                                <h3 className="font-serif text-2xl font-semibold text-ink">Upgrade your team</h3>
                                <p className="mt-1 text-xs text-muted">Unlock zero-trust private repository integration instantly.</p>
                            </div>
                        </div>

                        <a 
                            href={upgradeUrl}
                            className="rr-action-button group flex w-full items-center justify-between rounded-full px-4 py-3 text-sm font-medium transition-opacity hover:opacity-90"
                        >
                            <span>Manage Billing & Seats</span>
                            <ChevronRight className="h-4 w-4 transition-transform group-hover:translate-x-0.5" />
                        </a>
                    </div>
                    
                    <div className="border-t border-line/80 bg-surface/40 p-4">
                        <p className="flex justify-center text-center text-xs text-muted">
                            {isHostedMode ? "You are operating in Hosted Mode" : "You are operating in Local Mode"}
                        </p>
                    </div>
                </div>

            </div>
        </div>
    );
}
