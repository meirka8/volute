import { useQuery } from '@tanstack/react-query';
import { useAuth } from '../auth/AuthContext';
import { createGithubClient } from '../api/github';

export function usePR(owner: string, repo: string, pullNumber: number) {
    const { isAuthenticated, acquireToken } = useAuth();

    return useQuery({
        queryKey: ['pr', owner, repo, pullNumber],
        queryFn: async () => {
            const currentToken = await acquireToken(owner, repo);
            if (!currentToken) throw new Error("Could not acquire token");
            
            const client = createGithubClient(currentToken);

            const [pr, files] = await Promise.all([
                client.getPullRequest(owner, repo, pullNumber),
                client.getPullRequestFiles(owner, repo, pullNumber)
            ]);

            return { pr, files };
        },
        enabled: isAuthenticated && !!owner && !!repo && !!pullNumber,
    });
}
