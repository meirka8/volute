import { describe, expect, it } from 'vitest';
import { looksLikeSecret } from './secretLike';

// Every secret in this file is made up, as plainly as can be: sequential or repeated characters, as in
// cvc-core/tests/test_privacy.rs, apart from AWS's own documented example key. Keep it that way: a value that looks
// real (or is a vendor's docs example) can raise a secret-scanning alert on a public repository.
//
// A value that has the shape of a vendor's token is joined at runtime, with `fake`, from parts that are none of them that shape:
// GitHub push protection scans the text of the source, not what the test makes of it, and it takes a token for one wherever it
// is whole in a file, made up or not. So no such value is whole anywhere in this one (the runtime values are what they are).
const fake = (...parts: string[]): string => parts.join('');

describe('looksLikeSecret', () => {
  it.each([
    ['a PEM private key', fake('-----BEGIN RSA ', 'PRIVATE KEY-----\nAAAAAAAA')],
    ['an OpenSSH private key', fake('-----BEGIN OPENSSH ', 'PRIVATE KEY-----')],
    ['an AWS access key', fake('AKIA', 'IOSFODNN7EXAMPLE')],
    ['a temporary AWS access key', fake('key ASIA', 'ABCDEFGHIJKLMNOP here')],
    ['a JSON web token', fake('ey', 'JAAAAAAAAAA.', 'ey', 'JBBBBBBBBBB.CCCCCCCCCCCC')],
    ['a GitHub personal access token', fake('ghp', '_0123456789abcdefghijklmnopqrstuvwxyz')],
    ['a GitHub OAuth token', fake('gho', '_0123456789abcdefghijklmnopqrstuvwxyz')],
    ['a fine-grained GitHub token', fake('github', '_pat_0123456789abcdefghijklmnopqrstuvwxyz_0123456789')],
    ['a GitLab personal access token', fake('glpat', '-0123456789abcdefghij')],
    ['a GitLab personal access token with a dash and an underscore in it', fake('glpat', '-0123456789_-abcdefgh')],
    ['an OpenAI key', fake('sk', '-abcdefghijklmnopqrstuvwxyz0123456789')],
    ['an OpenAI project key', fake('sk', '-proj-abcdefghijklmnopqrstuvwxyz0123456789')],
    ['a Slack token', fake('xox', 'b-1234567890-abcdefghij')],
    ['a Slack configuration token', fake('xox', 'e-1-0123456789abcdef')],
    ['a Google access token', fake('ya29', '.0123456789abcdefghijklmnopqrs')],
    ['a Google API key', fake('AIza', '0123456789abcdefghijklmnopqrstuvwxy')],
    ['a Google API key with a dash and an underscore in it', fake('AIza', '0123456789abcdefghijklmnopqrstu-_AB')],
    ['a bearer token', 'Bearer abcdef0123456789ABCD'],
    ['an authorization header', 'Authorization: abcdef0123456789ABCD'],
    ['an authorization header with a bearer token', 'Authorization: Bearer abcdef0123456789ABCD'],
    ['credentials in a URL', 'postgres://admin:0123456789@db.internal:5432/app'],
    ['a password assignment', 'password = hunter2hunter2'],
    ['a token assignment', 'API_KEY: 0123456789abcdef'],
    ['an api-key assignment', 'api-key=abcd1234efgh'],
    ['an AWS secret access key with a slash and a plus in it', fake('AbCdEfGhIjKlMnOpQrStUvWxYz', '0123456789+/AB')],
    ['an AWS secret access key with a slash and a plus in it, in a sentence', fake('the key was AbCdEfGhIjKlMnOpQrStUvWxYz', '0123456789+/AB here')],
    ['a long run of mixed-case letters and digits', 'AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRr0123456789'],
    ['a long run of mixed-case letters and digits inside a sentence', 'the value was AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRr0123456789 here'],
    ['a long run of the other base64 alphabet, with dashes and underscores', 'AaBbCcDdEeFfGgHhIiJjKkLlMm0123456789_-_-AaBb'],
    ['a long run with the padding of base64 at its end', 'AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRr0123456789=='],
  ])('knows %s', (_name, text) => {
    expect(looksLikeSecret(text)).toBe(true);
  });

  it.each([
    ['nothing', ''],
    ['an ordinary word', 'retry'],
    ['a phrase', 'retry limit exceeded while fetching the page'],
    ['a field search', 'path:src/payments/refund_handler.ts tool:Bash'],
    ['a UUID, which is not a secret', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70'],
    ['a UUID in capital letters', '5C1D8E2A-7F3B-4A96-B0C4-9E2D6F1A3B70'],
    ['two UUIDs joined by a dash', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70-5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70'],
    ['a conversation id of a subagent', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70:agent-a0bd78e0'],
    ['a short hash', 'a1b2c3d'],
    ['a commit hash of forty hexadecimal digits', 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678'],
    ['a commit hash in capital letters', 'A1B2C3D4E5F60718293A4B5C6D7E8F9012345678'],
    ['a commit hash in both cases', 'a1B2c3D4e5F60718293a4B5c6D7e8F9012345678'],
    ['a SHA-256 of sixty-four hexadecimal digits', '0123456789abcdef'.repeat(4)],
    ['a SHA-256 in both cases', '0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789ABCDEF'],
    ['a SHA-256 in a sentence', 'the file with the hash 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef was changed'],
    ['a long word', 'pneumonoultramicroscopicsilicovolcanoconiosis'],
    ['a long run of digits', '12345678901234567890123456789012345'],
    ['an identifier', 'fetchWithRetryAndBackoff_v2'],
    ['a long name in kebab case', 'fetch-with-retry-and-backoff-for-the-payment-handlers-v2'],
    ['a long name in snake case', 'fetch_with_retry_and_backoff_for_the_payment_handlers_v2'],
    ['a long sentence', 'retry limit exceeded while fetching the page for the payment handlers, then giving up for good'],
    ['a long run of mixed-case letters with no digit', 'AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRrSsTtUuVvWwXx'],
    ['a long run of lower-case letters and digits with no capital', 'a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0u1v2w3x4'],
    ['a long run of capital letters and digits with no lower-case letter', 'A1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q7R8S9T0U1V2W3X4'],
    ['a word like token with nothing after it', 'the token'],
    ['a password in prose', 'reset the password later'],
    ['a short value after a name', 'token: abc'],
    ['a URL without credentials', 'https://example.com/a/b?c=d'],
    ['a prefix that is only that', 'ghp_ and sk- and xoxb- and AKIA and eyJ and AIza and glpat- and xoxe-'],
    ['a bearer without a token', 'bearer of bad news'],
    ['a public PEM block', '-----BEGIN CERTIFICATE-----'],
  ])('lets %s through', (_name, text) => {
    expect(looksLikeSecret(text)).toBe(false);
  });

  it('takes forty characters of a mixed-case run for a secret and thirty-nine for a name', () => {
    const run = 'AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRr0123456789';
    expect(looksLikeSecret(run.slice(0, 40))).toBe(true);
    expect(looksLikeSecret(run.slice(0, 39))).toBe(false);
    expect(looksLikeSecret(`${run.slice(0, 20)} ${run.slice(20, 40)}`)).toBe(false); // a space ends a run
    expect(looksLikeSecret(`${run.slice(0, 20)}.${run.slice(20, 40)}`)).toBe(false); // so does a dot
  });

  it('wants all three of a capital letter, a small one and a digit, and something that is not a hexadecimal digit', () => {
    const letters = 'AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRrSsTt';
    expect(looksLikeSecret(letters)).toBe(false); // no digit
    expect(looksLikeSecret(letters.toLowerCase().replace(/[a-t]/, '1'))).toBe(false); // no capital
    expect(looksLikeSecret(letters.toUpperCase().replace(/[A-T]/, '1'))).toBe(false); // no small letter
    expect(looksLikeSecret(`${letters}1`)).toBe(true);
    // Hexadecimal digits in both cases, with a digit: a hash, whatever its length.
    expect(looksLikeSecret('aBcDeF0123456789'.repeat(8))).toBe(false);
    expect(looksLikeSecret(`${'aBcDeF0123456789'.repeat(8)}g`)).toBe(true);
  });

  it('is not fooled by case in the words that start with a capital', () => {
    expect(looksLikeSecret('PASSWORD: hunter2hunter2')).toBe(true);
    expect(looksLikeSecret('authorization: BEARER abcdef0123456789ABCD')).toBe(true);
  });

  it.each(['', 'İ ', 'İİİ ', 'İ'.repeat(60), '\u{1F600} ', 'ß ', 'ǅ '])('finds a name or a scheme after %j, whatever that does to the length of a lower-case copy', (lead) => {
    // `toLowerCase` makes `İ` two characters, so a place found in the copy is one place too far in the text, for each one.
    expect(looksLikeSecret(`${lead}Bearer abcdef0123456789ABCD`)).toBe(true);
    expect(looksLikeSecret(`${lead}BEARER abcdef0123456789ABCD`)).toBe(true);
    expect(looksLikeSecret(`${lead}Authorization: abcdef0123456789ABCD`)).toBe(true);
    expect(looksLikeSecret(`${lead}Authorization: Bearer abcdef0123456789ABCD`)).toBe(true);
    expect(looksLikeSecret(`${lead}password=hunter2hunter2`)).toBe(true);
    expect(looksLikeSecret(`${lead}PASSWORD = hunter2hunter2`)).toBe(true);
    expect(looksLikeSecret(`${lead}API_KEY: 0123456789abcdef`)).toBe(true);
    expect(looksLikeSecret(`${lead}AWS_SECRET_ACCESS_KEY=0123456789abcdef`)).toBe(true);
    // And it is as strict as it was: the same texts with what makes them a secret taken out are not.
    expect(looksLikeSecret(`${lead}Bearer abcdef`)).toBe(false);
    expect(looksLikeSecret(`${lead}password is hunter2hunter2`)).toBe(false);
    expect(looksLikeSecret(`${lead}mypassword=hunter2hunter2`)).toBe(false);
  });

  it('reads no more than the beginning of a long text, and takes a fraction of a second on any text', () => {
    const started = performance.now();
    expect(looksLikeSecret(`${'x '.repeat(5_000)}${fake('ghp', '_0123456789abcdefghijklmnopqrstuvwxyz')}`)).toBe(false); // past what it reads
    expect(looksLikeSecret(`${'x '.repeat(5_000)}AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRr0123456789`)).toBe(false);
    for (const hostile of [
      'sk-'.repeat(700),
      'eyJ'.repeat(700),
      'a.'.repeat(1_000),
      'password='.repeat(300),
      '://'.repeat(600),
      'Bearer '.repeat(250),
      '-----BEGIN '.repeat(180),
      'AIza'.repeat(500),
      'glpat-'.repeat(350),
      'xoxe-'.repeat(400),
      'A1b'.repeat(700),
      '=-_+/'.repeat(400),
      'İ'.repeat(2_000),
      `${'İ'.repeat(1_000)}${'Bearer '.repeat(140)}`,
    ]) {
      looksLikeSecret(hostile);
    }
    expect(performance.now() - started).toBeLessThan(1_000);
  });
});
