import { useQuery } from '@tanstack/react-query';
import { useAuth } from '../auth/AuthContext';
import { createGithubClient } from '../api/github';

export function usePR(owner: string, repo: string, pullNumber: number) {
    const { token } = useAuth();

    return useQuery({
        queryKey: ['pr', owner, repo, pullNumber],
        queryFn: async () => {
            if (!token) throw new Error("No token");
            const client = createGithubClient(token);

            const [pr, files] = await Promise.all([
                client.getPullRequest(owner, repo, pullNumber),
                client.getPullRequestFiles(owner, repo, pullNumber)
            ]);

            return { pr, files };
        },
        enabled: !!token && !!owner && !!repo && !!pullNumber,
    });
}
