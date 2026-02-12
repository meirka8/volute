import { Route, Switch, Redirect } from "wouter";
import { AuthProvider, useAuth } from "./auth/AuthContext";
import Login from "./pages/Login";
import { PRReviewPage } from "./pages/PRReview";
import { DebugView } from "./DebugView";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60 * 5, // 5 minutes
      retry: 2,
      retryDelay: (attemptIndex) => Math.min(1000 * 2 ** attemptIndex, 30000),
    },
  },
});

// Simple landing/dashboard to select a PR
function Dashboard() {
  const { logout } = useAuth();

  return (
    <div className="min-h-screen bg-[#080808] text-[#ededed] p-8">
      <div className="max-w-2xl mx-auto">
        <div className="flex items-center justify-between mb-8">
          <h1 className="text-2xl font-bold">CVC Reviewer</h1>
          <button
            onClick={logout}
            className="px-3 py-1.5 text-sm bg-[#1c1c1c] hover:bg-[#262626] border border-[#262626] rounded transition-colors"
          >
            Logout
          </button>
        </div>

        <div className="bg-[#0d0d0d] border border-[#1c1c1c] rounded-lg p-6">
          <h2 className="text-lg font-medium mb-4">Open a Pull Request</h2>
          <p className="text-[#888888] text-sm mb-6">
            Navigate to a PR using the URL pattern:
          </p>
          <code className="block bg-[#1c1c1c] px-4 py-3 rounded font-mono text-sm text-[#5e6ad2] mb-6">
            /pr/:owner/:repo/:pr_number
          </code>

          <div className="space-y-3">
            <p className="text-[#888888] text-sm">Examples:</p>
            <div className="space-y-2">
              <a
                href="/pr/meirka8/cvc/1"
                className="block px-4 py-2 bg-[#1c1c1c] hover:bg-[#262626] rounded text-sm transition-colors"
              >
                /pr/meirka8/cvc/1
              </a>
              <a
                href="/pr/facebook/react/1"
                className="block px-4 py-2 bg-[#1c1c1c] hover:bg-[#262626] rounded text-sm transition-colors"
              >
                /pr/facebook/react/1
              </a>
            </div>
          </div>
        </div>

        <div className="mt-6 text-center">
          <a
            href="/phase2"
            className="text-sm text-[#5e6ad2] hover:text-[#7c86e2] transition-colors"
          >
            View Phase 2 Debug Interface →
          </a>
        </div>
      </div>
    </div>
  );
}

function Routes() {
  const { isAuthenticated } = useAuth();

  return (
    <Switch>
      {/* Login */}
      <Route path="/login">
        {isAuthenticated ? <Redirect to="/app" /> : <Login />}
      </Route>

      {/* Main dashboard */}
      <Route path="/app">
        {isAuthenticated ? <Dashboard /> : <Redirect to="/login" />}
      </Route>

      {/* PR Review (Phase 3) */}
      <Route path="/pr/:owner/:repo/:pr">
        {isAuthenticated ? <PRReviewPage /> : <Redirect to="/login" />}
      </Route>

      {/* Phase 2 Debug View (preserved) */}
      <Route path="/phase2">
        {isAuthenticated ? <DebugView /> : <Redirect to="/login" />}
      </Route>

      {/* Legacy debug route redirect */}
      <Route path="/debug">
        <Redirect to="/phase2" />
      </Route>

      {/* Root redirect */}
      <Route path="/">
        <Redirect to={isAuthenticated ? "/app" : "/login"} />
      </Route>
    </Switch>
  );
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <Routes />
      </AuthProvider>
    </QueryClientProvider>
  );
}

export default App;
