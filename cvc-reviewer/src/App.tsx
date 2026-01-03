import { Route, Switch, Redirect } from "wouter";
import { AuthProvider, useAuth } from "./auth/AuthContext";
import Login from "./pages/Login";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60 * 5, // 5 minutes
      retry: false,
    },
  },
});



// Placeholder for main app
function Dashboard() {
  const { logout } = useAuth();
  return (
    <div className="p-10 text-white">
      <h1 className="text-2xl font-bold">Welcome to CVC Reviewer</h1>
      <p className="mt-4">You are authenticated.</p>
      <button onClick={logout} className="mt-4 px-4 py-2 bg-red-600 rounded">Logout</button>
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
