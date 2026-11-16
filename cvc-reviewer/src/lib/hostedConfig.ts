/**
 * Whether this build of the Reviewer is a hosted one, and where its platform is, read in one place for the whole
 * app: the token, sign-in and sign-out requests (`auth/AuthContext.tsx`), the billing link (`pages/Paywall.tsx`)
 * and the link previews that `middleware.ts` adds on the server (`unfurl/prUnfurl.ts`). The browser and the
 * middleware both run it, so it uses nothing that only one of them has.
 *
 * Hosted mode needs the deployment to say where its platform is, in `VITE_PLATFORM_API_URL`. When it does not, no
 * address is guessed: the page's Content-Security-Policy (`index.html`) lets the app connect only to the address
 * that same variable gives, so the browser would refuse a guessed one anyway, and a build that was not pointed at a
 * platform has no reason to call one. Such a build is misconfigured, and every page of it says so.
 */

/** The two build-time variables this reads. `import.meta.env` and `process.env` both have them. */
export type HostedEnv = {
  readonly VITE_HOSTED_MODE?: string;
  readonly VITE_PLATFORM_API_URL?: string;
};

export type HostedConfig =
  /** `VITE_HOSTED_MODE` is not `true`: the browser keeps a GitHub PAT and talks to GitHub alone. */
  | { readonly mode: 'local' }
  /** `VITE_HOSTED_MODE=true` and a usable `VITE_PLATFORM_API_URL`. */
  | { readonly mode: 'hosted'; readonly platformBase: string }
  /** `VITE_HOSTED_MODE=true` without a usable `VITE_PLATFORM_API_URL`: there is no platform to talk to. */
  | { readonly mode: 'misconfigured' };

/**
 * The platform's address without its trailing slash, for example `https://www.cvc.dev`, or null when the value is
 * not a plain http(s) URL: one without credentials, a query string or a fragment. A path is kept
 * (`https://example.com/cvc/` gives `https://example.com/cvc`).
 */
export function parsePlatformBase(raw: string | undefined): string | null {
  const value = raw?.trim();
  if (!value) {
    return null;
  }

  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }

  if (
    (url.protocol !== 'https:' && url.protocol !== 'http:') ||
    url.username !== '' ||
    url.password !== '' ||
    url.search !== '' ||
    url.hash !== ''
  ) {
    return null;
  }

  return `${url.origin}${url.pathname.replace(/\/+$/, '')}`;
}

export function readHostedConfig(env: HostedEnv): HostedConfig {
  if (env.VITE_HOSTED_MODE !== 'true') {
    return { mode: 'local' };
  }

  const platformBase = parsePlatformBase(env.VITE_PLATFORM_API_URL);
  return platformBase === null ? { mode: 'misconfigured' } : { mode: 'hosted', platformBase };
}
