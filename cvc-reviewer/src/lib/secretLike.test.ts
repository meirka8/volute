import { describe, expect, it } from 'vitest';
import { looksLikeSecret } from './secretLike';

// Every secret in this file is made up, as plainly as can be: sequential or repeated characters, as in
// cvc-core/tests/test_privacy.rs, apart from AWS's own documented example key. Keep it that way: a value that looks
// real (or is a vendor's docs example) can raise a secret-scanning alert on a public repository.
describe('looksLikeSecret', () => {
  it.each([
    ['a PEM private key', '-----BEGIN RSA PRIVATE KEY-----\nAAAAAAAA'],
    ['an OpenSSH private key', '-----BEGIN OPENSSH PRIVATE KEY-----'],
    ['an AWS access key', 'AKIAIOSFODNN7EXAMPLE'],
    ['a temporary AWS access key', 'key ASIAABCDEFGHIJKLMNOP here'],
    ['a JSON web token', 'eyJAAAAAAAAAA.eyJBBBBBBBBBB.CCCCCCCCCCCC'],
    ['a GitHub personal access token', 'ghp_0123456789abcdefghijklmnopqrstuvwxyz'],
    ['a GitHub OAuth token', 'gho_0123456789abcdefghijklmnopqrstuvwxyz'],
    ['a fine-grained GitHub token', 'github_pat_0123456789abcdefghijklmnopqrstuvwxyz_0123456789'],
    ['an OpenAI key', 'sk-abcdefghijklmnopqrstuvwxyz0123456789'],
    ['an OpenAI project key', 'sk-proj-abcdefghijklmnopqrstuvwxyz0123456789'],
    ['a Slack token', 'xoxb-1234567890-abcdefghij'],
    ['a Google access token', 'ya29.0123456789abcdefghijklmnopqrs'],
    ['a bearer token', 'Bearer abcdef0123456789ABCD'],
    ['an authorization header', 'Authorization: abcdef0123456789ABCD'],
    ['an authorization header with a bearer token', 'Authorization: Bearer abcdef0123456789ABCD'],
    ['credentials in a URL', 'postgres://admin:0123456789@db.internal:5432/app'],
    ['a password assignment', 'password = hunter2hunter2'],
    ['a token assignment', 'API_KEY: 0123456789abcdef'],
    ['an api-key assignment', 'api-key=abcd1234efgh'],
    ['a long run of letters and digits', 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678'],
    ['a long run of letters and digits inside a sentence', 'the value was a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8 here'],
  ])('knows %s', (_name, text) => {
    expect(looksLikeSecret(text)).toBe(true);
  });

  it.each([
    ['nothing', ''],
    ['an ordinary word', 'retry'],
    ['a phrase', 'retry limit exceeded while fetching the page'],
    ['a field search', 'path:src/payments/refund_handler.ts tool:Bash'],
    ['a UUID, which is not a secret', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70'],
    ['a conversation id of a subagent', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70:agent-a0bd78e0'],
    ['a short hash', 'a1b2c3d'],
    ['a long word', 'pneumonoultramicroscopicsilicovolcanoconiosis'],
    ['a long run of digits', '12345678901234567890123456789012345'],
    ['an identifier', 'fetchWithRetryAndBackoff_v2'],
    ['a word like token with nothing after it', 'the token'],
    ['a password in prose', 'reset the password later'],
    ['a short value after a name', 'token: abc'],
    ['a URL without credentials', 'https://example.com/a/b?c=d'],
    ['a prefix that is only that', 'ghp_ and sk- and xoxb- and AKIA and eyJ'],
    ['a bearer without a token', 'bearer of bad news'],
    ['a public PEM block', '-----BEGIN CERTIFICATE-----'],
  ])('lets %s through', (_name, text) => {
    expect(looksLikeSecret(text)).toBe(false);
  });

  it('is not fooled by case in the words that start with a capital', () => {
    expect(looksLikeSecret('PASSWORD: hunter2hunter2')).toBe(true);
    expect(looksLikeSecret('authorization: BEARER abcdef0123456789ABCD')).toBe(true);
  });

  it('reads no more than the beginning of a long text, and takes a fraction of a second on any text', () => {
    const started = performance.now();
    expect(looksLikeSecret(`${'x '.repeat(5_000)}ghp_0123456789abcdefghijklmnopqrstuvwxyz`)).toBe(false); // past what it reads
    for (const hostile of ['sk-'.repeat(700), 'eyJ'.repeat(700), 'a.'.repeat(1_000), 'password='.repeat(300), '://'.repeat(600), 'Bearer '.repeat(250), '-----BEGIN '.repeat(180)]) {
      looksLikeSecret(hostile);
    }
    expect(performance.now() - started).toBeLessThan(1_000);
  });
});
