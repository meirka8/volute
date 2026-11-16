/**
 * The paywall's billing link goes to the platform the build names (`lib/hostedConfig.ts`), and nowhere when the build
 * names no usable one.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { AuthProvider } from '../auth/AuthContext';
import { Paywall } from './Paywall';

vi.mock('idb-keyval', () => ({ del: vi.fn().mockResolvedValue(undefined) }));

function renderPaywall(platformUrl: string | undefined) {
  vi.stubEnv('VITE_HOSTED_MODE', 'true');
  vi.stubEnv('VITE_PLATFORM_API_URL', platformUrl);
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AuthProvider>
        <Paywall />
      </AuthProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
});

describe('Paywall', () => {
  it('links to billing on the platform the build names', () => {
    renderPaywall('https://www.cvc.dev/');

    expect(screen.getByRole('link', { name: /manage billing/i })).toHaveAttribute(
      'href',
      'https://www.cvc.dev/dashboard/billing',
    );
  });

  it.each<[string, string | undefined]>([
    ['missing', undefined],
    ['a host without a scheme', 'app.cvc.dev'],
  ])('links nowhere when the platform URL is %s', (_label, platformUrl) => {
    renderPaywall(platformUrl);

    expect(screen.getByRole('heading', { name: 'Private Repository Limit' })).toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });
});
