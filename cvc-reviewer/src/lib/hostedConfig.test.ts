/**
 * Whether a build is a hosted one, and where its platform is (`lib/hostedConfig.ts`). A hosted build that does not
 * give a plain http(s) address for its platform is misconfigured: no address is guessed for it.
 */
import { describe, expect, it } from 'vitest';
import { parsePlatformBase, readHostedConfig, type HostedEnv } from './hostedConfig';

const HOSTED: HostedEnv = { VITE_HOSTED_MODE: 'true', VITE_PLATFORM_API_URL: 'https://www.cvc.dev' };

describe('parsePlatformBase', () => {
  it.each([
    ['https://www.cvc.dev', 'https://www.cvc.dev'],
    ['https://www.cvc.dev/', 'https://www.cvc.dev'],
    [' https://example.com/cvc// ', 'https://example.com/cvc'],
    ['http://localhost:3000/', 'http://localhost:3000'],
    ['HTTPS://WWW.CVC.DEV', 'https://www.cvc.dev'],
    ['https://www.cvc.dev:443/', 'https://www.cvc.dev'],
  ])('reads %s as %s', (raw, base) => {
    expect(parsePlatformBase(raw)).toBe(base);
  });

  it.each<[string, string | undefined]>([
    ['missing', undefined],
    ['empty', ''],
    ['blank', '  '],
    ['without a scheme', 'www.cvc.dev'],
    ['a script', 'javascript:alert(1)'],
    ['not http(s)', 'ftp://www.cvc.dev'],
    ['carrying credentials', 'https://user:secret@www.cvc.dev'],
    ['carrying a user name', 'https://user@www.cvc.dev'],
    ['carrying a query string', 'https://www.cvc.dev/?next=1'],
    ['carrying a fragment', 'https://www.cvc.dev/#top'],
    ['not a URL', 'https://'],
  ])('refuses a value that is %s', (_label, raw) => {
    expect(parsePlatformBase(raw)).toBeNull();
  });
});

describe('readHostedConfig', () => {
  it.each<[string, HostedEnv]>([
    ['nothing is set', {}],
    ['only the platform URL is set', { VITE_PLATFORM_API_URL: 'https://www.cvc.dev' }],
    ['hosted mode is not exactly "true"', { ...HOSTED, VITE_HOSTED_MODE: 'TRUE' }],
    ['hosted mode is "1"', { ...HOSTED, VITE_HOSTED_MODE: '1' }],
  ])('is local when %s', (_label, env) => {
    expect(readHostedConfig(env)).toEqual({ mode: 'local' });
  });

  it('is hosted, at the address the build gives, when that address is a plain http(s) URL', () => {
    expect(readHostedConfig(HOSTED)).toEqual({ mode: 'hosted', platformBase: 'https://www.cvc.dev' });
    expect(readHostedConfig({ ...HOSTED, VITE_PLATFORM_API_URL: 'https://example.com/cvc/' })).toEqual({
      mode: 'hosted',
      platformBase: 'https://example.com/cvc',
    });
  });

  it.each<[string, string | undefined]>([
    ['missing', undefined],
    ['blank', '  '],
    ['a host without a scheme', 'app.cvc.dev'],
    ['not http(s)', 'ftp://www.cvc.dev'],
    ['carrying a query string', 'https://www.cvc.dev/?next=1'],
  ])('is misconfigured, with no address at all, when hosted mode is on and the platform URL is %s', (_label, url) => {
    expect(readHostedConfig({ VITE_HOSTED_MODE: 'true', VITE_PLATFORM_API_URL: url })).toEqual({ mode: 'misconfigured' });
  });
});
