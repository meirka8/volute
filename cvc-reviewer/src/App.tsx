import { Route, Switch, Redirect } from "wouter";
import { AuthProvider, useAuth } from "./auth/AuthContext";
import Login from "./pages/Login";
import { DebugView } from "./DebugView";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60 * 5, // 5 minutes
      retry: 2,
      retryDelay: attemptIndex => Math.min(1000 * 2 ** attemptIndex, 30000),
    },
  },
});



// Placeholder for main app
function Dashboard() {
  const { logout } = useAuth();
  return (
    <div className="p-10 text-white">
      <h1 className="text-2xl font-bold">Welcome to CVC Reviewer</h1>
      <DebugView />
      <button onClick={logout} className="mt-8 px-4 py-2 bg-red-900/50 text-red-200 border border-red-900 rounded hover:bg-red-900">Logout</button>
    </div>
  );
}

function Routes() {
  const { isAuthenticated } = useAuth();

  return (
    <Switch>
      <Route path="/login">
        {isAuthenticated ? <Redirect to="/app" /> : <Login />}
      </Route>
      <Route path="/app">
        {isAuthenticated ? <Dashboard /> : <Redirect to="/login" />}
      </Route>
      <Route path="/debug">
        <DebugView />
      </Route>
      <Route path="/">
        <Redirect to="/debug" />
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
