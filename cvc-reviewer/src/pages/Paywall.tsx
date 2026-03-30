import { Lock, CreditCard, ChevronRight } from 'lucide-react';
import { useAuth } from '../auth/AuthContext';

export function Paywall() {
    const { isHostedMode } = useAuth();
    // Assuming platform URL is where billing settings live
    const platformUrl = import.meta.env.VITE_PLATFORM_API_URL || 'https://app.cvc.dev';
    const upgradeUrl = `${platformUrl}/dashboard/billing`;

    return (
        <div className="flex min-h-screen flex-col items-center justify-center bg-[#080808] text-[#ededed] font-sans p-4">
            <div className="w-full max-w-lg space-y-8">
                <div className="text-center">
                    <div className="mx-auto flex h-16 w-16 items-center justify-center rounded-xl bg-[#1c1c1c] border border-[#262626] mb-6">
                        <Lock className="h-8 w-8 text-[#888888]" />
                    </div>
                    <h2 className="mt-6 text-3xl font-semibold tracking-tight text-white">Private Repository Limit</h2>
                    <p className="mt-3 text-[#888888] text-sm max-w-sm mx-auto leading-relaxed">
                        You have reached a private repository that is protected. CVC Reviewer requires an active Pro subscription to analyze private codebases.
                    </p>
                </div>

                <div className="mt-10 rounded-xl border border-[#1c1c1c] bg-[#0d0d0d] overflow-hidden shadow-2xl">
                    <div className="p-8">
                        <div className="flex items-center gap-4 mb-6">
                            <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-[#5e6ad2]/10">
                                <CreditCard className="h-5 w-5 text-[#5e6ad2]" />
                            </div>
                            <div>
                                <h3 className="text-lg font-medium text-white">Upgrade your team</h3>
                                <p className="text-xs text-[#888888] mt-1">Unlock zero-trust private repository integration instantly.</p>
                            </div>
                        </div>

                        <a 
                            href={upgradeUrl}
                            className="group flex w-full items-center justify-between rounded-lg bg-[#ededed] px-4 py-3 text-sm font-medium text-[#080808] transition-all hover:bg-white"
                        >
                            <span>Manage Billing & Seats</span>
                            <ChevronRight className="h-4 w-4 text-[#555555] transition-transform group-hover:translate-x-0.5" />
                        </a>
                    </div>
                    
                    <div className="bg-[#1c1c1c]/50 p-4 border-t border-[#1c1c1c]">
                        <p className="text-center flex justify-center text-xs text-[#888888]">
                            {isHostedMode ? "You are operating in Hosted Mode" : "You are operating in Local Mode"}
                        </p>
                    </div>
                </div>

            </div>
        </div>
    );
}
