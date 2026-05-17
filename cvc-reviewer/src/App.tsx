import { Route, Switch, Redirect } from "wouter";
import { AuthProvider, useAuth } from "./auth/AuthContext";
import Login from "./pages/Login";
import { PRReviewPage } from "./pages/PRReview";
import { DebugView } from "./DebugView";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ThemeToggle } from "./components/ui/ThemeToggle";

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
    <div className="min-h-screen p-6 text-ink sm:p-8">
      <div className="max-w-2xl mx-auto">
        <div className="flex items-center justify-between mb-8">
          <div>
            <div className="text-xs font-semibold uppercase tracking-[0.2em] text-action">Hosted Reviewer</div>
            <h1 className="mt-2 font-serif text-3xl font-semibold">CVC Reviewer</h1>
          </div>
          <div className="flex items-center gap-3">
            <ThemeToggle compact />
            <button
              onClick={logout}
              className="rr-panel rounded-full px-4 py-2 text-sm font-medium text-muted transition-colors hover:bg-surface hover:text-ink"
            >
              Logout
            </button>
          </div>
        </div>

        <div className="rr-panel rounded-[2rem] p-6">
          <h2 className="font-serif text-2xl font-semibold mb-4">Open a Pull Request</h2>
          <p className="text-sm text-muted mb-6">
            Navigate to a PR using the URL pattern:
          </p>
          <code className="rr-code mb-6 block rounded-[1.25rem] px-4 py-3 font-mono text-sm text-action">
            /pr/:owner/:repo/:pr_number
          </code>

          <div className="space-y-3">
            <p className="text-sm text-muted">Examples:</p>
            <div className="space-y-2">
              <a
                href="/pr/meirka8/cvc/1"
                className="rr-code block rounded-[1.25rem] px-4 py-3 text-sm transition-colors hover:bg-surface"
              >
                /pr/meirka8/cvc/1
              </a>
              <a
                href="/pr/facebook/react/1"
                className="rr-code block rounded-[1.25rem] px-4 py-3 text-sm transition-colors hover:bg-surface"
              >
                /pr/facebook/react/1
              </a>
            </div>
          </div>
        </div>

        <div className="mt-6 text-center">
          <a
            href="/phase2"
            className="text-sm text-action transition-colors hover:opacity-80"
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
