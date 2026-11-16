/**
 * A hosted build that does not say where its platform is (`lib/hostedConfig.ts`) shows a configuration error on
 * every page and calls no platform; a build that does, or a local one, gets its pages.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App';

vi.mock('idb-keyval', () => ({
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
  del: vi.fn().mockResolvedValue(undefined),
}));

const NOT_CONFIGURED = { name: 'Reviewer not configured' };

let fetchSpy: ReturnType<typeof vi.fn>;

beforeEach(() => {
  // The local login page looks for a local proxy; nothing answers here.
  fetchSpy = vi.fn().mockRejectedValue(new Error('offline'));
  vi.stubGlobal('fetch', fetchSpy);
});

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '/');
});

function renderAt(path: string) {
  window.history.replaceState(null, '', path);
  render(<App />);
}

describe('App', () => {
  it.each(['/', '/app', '/pr/acme/shop/7', '/phase2'])(
    'says a hosted build without a platform URL is not configured, at %s, and calls nothing',
    (path) => {
      vi.stubEnv('VITE_HOSTED_MODE', 'true');
      vi.stubEnv('VITE_PLATFORM_API_URL', undefined);

      renderAt(path);

      expect(screen.getByRole('heading', NOT_CONFIGURED)).toBeInTheDocument();
      expect(fetchSpy).not.toHaveBeenCalled();
    },
  );

  it('opens the dashboard of a hosted build that names its platform', () => {
    vi.stubEnv('VITE_HOSTED_MODE', 'true');
    vi.stubEnv('VITE_PLATFORM_API_URL', 'https://www.cvc.dev');

    renderAt('/app');

    expect(screen.getByRole('heading', { name: 'Open a Pull Request' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', NOT_CONFIGURED)).not.toBeInTheDocument();
  });

  it('asks a local build for a GitHub token', () => {
    vi.stubEnv('VITE_HOSTED_MODE', undefined);
    vi.stubEnv('VITE_PLATFORM_API_URL', undefined);

    renderAt('/login');

    expect(screen.getByLabelText('Personal Access Token')).toBeInTheDocument();
    expect(screen.queryByRole('heading', NOT_CONFIGURED)).not.toBeInTheDocument();
  });
});
