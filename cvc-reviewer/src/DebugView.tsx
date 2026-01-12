import { useState } from 'react';
import { useCVCBlobs } from './hooks/useCVCBlobs';
import { usePR } from './hooks/usePR';
import { createGithubClient } from './api/github';
import { useAuth } from './auth/AuthContext';
import { CommitRanger } from './lib/CommitRanger';
import { InteractionMapper } from './lib/InteractionMapper';
import { useQuery } from '@tanstack/react-query';
import type { InteractionNode } from './types/cvc';

// Initial defaults
const DEFAULT_OWNER = "meirka8";
const DEFAULT_REPO = "cvc";
const DEFAULT_PR = 1;

export function DebugView() {
    const { token } = useAuth();

    // State for inputs
    const [owner, setOwner] = useState(DEFAULT_OWNER);
    const [repo, setRepo] = useState(DEFAULT_REPO);
    const [prStr, setPrStr] = useState(String(DEFAULT_PR));

    const prNumber = parseInt(prStr) || 1;

    // 1. Fetch Blobs
    const { data: interactions, isLoading: isLoadingBlobs, error: blobError } = useCVCBlobs(owner, repo);

    // 2. Fetch PR
    usePR(owner, repo, prNumber);

    // 3. Join Logic (Derived Query)
    const { data: joinedData } = useQuery({
        queryKey: ['join', owner, repo, prNumber, interactions?.length],
        queryFn: async () => {
            if (!interactions || !token) return null;

            const client = createGithubClient(token);
            const ranger = new CommitRanger(client);

            // Get PR Commits
            const commits = await ranger.getCommitShas(owner, repo, prNumber);

            // Map Interactions
            const mapper = new InteractionMapper(interactions);
            const prInteractions = mapper.getInteractionsForRange(commits);

            return { commits, prInteractions };
        },
        enabled: !!interactions && !!token && !isLoadingBlobs
    });

    return (
        <div className="p-8 space-y-8 bg-black min-h-screen text-gray-300 font-mono text-sm">
            <h1 className="text-xl font-bold text-indigo-400 border-b border-gray-800 pb-2">CVC Phase 2 Debugger</h1>

            {/* Input Controls */}
            <div className="flex gap-4 items-end bg-gray-900 p-4 rounded border border-gray-800">
                <label className="flex flex-col gap-1">
                    <span className="text-xs text-gray-500">Owner</span>
                    <input className="bg-black border border-gray-700 rounded px-2 py-1 text-white" value={owner} onChange={e => setOwner(e.target.value)} />
                </label>
                <label className="flex flex-col gap-1">
                    <span className="text-xs text-gray-500">Repo</span>
                    <input className="bg-black border border-gray-700 rounded px-2 py-1 text-white" value={repo} onChange={e => setRepo(e.target.value)} />
                </label>
                <label className="flex flex-col gap-1">
                    <span className="text-xs text-gray-500">PR #</span>
                    <input className="bg-black border border-gray-700 rounded px-2 py-1 text-white w-20" value={prStr} onChange={e => setPrStr(e.target.value)} />
                </label>
            </div>

            <div className="grid grid-cols-2 gap-8">
                <section className="bg-gray-900/50 p-4 rounded border border-gray-800">
                    <h2 className="font-bold text-white mb-4">1. Raw Blobs (refs/cvc/main)</h2>
                    {isLoadingBlobs && <div>Loading blobs...</div>}
                    {blobError && <div className="text-red-400">{String(blobError)}</div>}
                    {interactions && (
                        <div className="space-y-2">
                            <div className="text-green-400">Fetched {interactions.length} interactions</div>
                            <pre className="text-xs overflow-auto max-h-60 bg-black p-2 rounded">
                                {JSON.stringify(interactions.slice(0, 2), null, 2)}
                                {interactions.length > 2 && '\n...'}
                            </pre>
                        </div>
                    )}
                </section>

                <section className="bg-gray-900/50 p-4 rounded border border-gray-800">
                    <h2 className="font-bold text-white mb-4">2. The Join (PR #{prNumber})</h2>
                    {joinedData ? (
                        <div className="space-y-4">
                            <div>
                                <span className="text-gray-500">Commits in PR:</span>
                                <span className="ml-2 text-white">{joinedData.commits.length}</span>
                            </div>
                            <div>
                                <span className="text-gray-500">Relevant Thoughts:</span>
                                <span className="ml-2 text-indigo-400 font-bold">{joinedData.prInteractions.length}</span>
                            </div>

                            <div className="space-y-2 mt-4">
                                {joinedData.prInteractions.map((node: InteractionNode) => (
                                    <div key={node.id} className="border-l-2 border-indigo-500 pl-3 py-1">
                                        <div className="text-xs text-indigo-300">{node.author}</div>
                                        <div className="text-white">{node.user_prompt?.substring(0, 50)}...</div>
                                        <div className="text-xs text-gray-500 mt-1">
                                            Linked to: {node.artifact_links?.map(l => l.git_commit_hash.substring(0, 7)).join(', ')}
                                        </div>
                                    </div>
                                ))}
                            </div>
                        </div>
                    ) : (
                        <div>Waiting for data...</div>
                    )}
                </section>
            </div>
        </div>
    );
}
