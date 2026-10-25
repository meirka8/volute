/**
 * Round 3 QA of issue #118: the edges of `looksLikeSecret` (src/lib/secretLike.ts), which the examples of secretLike.test.ts do
 * not reach: each threshold is tried one below, at, and one above; a word boundary is tried with and without; a pattern is
 * tried with one of its parts left out. (Mutants that moved a threshold by one, dropped a boundary or asked for fewer parts
 * of a token survived the examples.)
 *
 * Every secret here is made up as plainly as the others: sequential or repeated characters.
 */
import { describe, expect, it } from 'vitest';
import { looksLikeSecret } from './secretLike';

const letters = (n: number) => 'abcdefghijklmnopqrstuvwxyz'.repeat(Math.ceil(n / 26)).slice(0, n);
/** `n` characters of lower-case letters and digits, with both. */
const mixed = (n: number) => 'a1b2c3d4e5f6g7h8i9j0'.repeat(Math.ceil(n / 20)).slice(0, n);
/** `n` characters of base64 text that has both cases, digits and letters past `f`: what a long run must have to be taken for a key. */
const key = (n: number) => 'aB1cD2eF3gH4iJ5kL6mN7oP8qR9sT0'.repeat(Math.ceil(n / 30)).slice(0, n);

describe('a long run of base64 text', () => {
  it('is a secret from 40 characters, with an upper-case letter, a lower-case letter, a digit and a letter past f', () => {
    expect(looksLikeSecret(key(40))).toBe(true);
    expect(looksLikeSecret(key(39))).toBe(false);
    expect(looksLikeSecret(key(41))).toBe(true);
    expect(looksLikeSecret(key(200))).toBe(true);
  });

  it('needs all of them: take away the capitals, the lower case, the digits or the letters past f and it is not a secret, however long', () => {
    for (const length of [40, 64, 500]) {
      expect(looksLikeSecret(key(length).toLowerCase()), `no capitals, ${length}`).toBe(false);
      expect(looksLikeSecret(key(length).toUpperCase()), `no lower case, ${length}`).toBe(false);
      expect(looksLikeSecret(key(length).replace(/[0-9]/g, 'x')), `no digits, ${length}`).toBe(false);
      // Letters and digits that are all hexadecimal digits are a hash, in capitals, in lower case or in both.
      expect(looksLikeSecret('aB1cD2eF30'.repeat(Math.ceil(length / 10)).slice(0, length)), `hexadecimal, ${length}`).toBe(false);
    }
  });

  it('is a secret again when one character of a mixed-case hexadecimal run is not a hexadecimal digit', () => {
    const hash = 'aB1cD2eF30'.repeat(8); // 80 characters
    expect(looksLikeSecret(hash)).toBe(false);
    for (const odd of ['g', 'G', 'z', '_', '-', '+', '/', '=']) {
      expect(looksLikeSecret(`${hash.slice(0, 40)}${odd}${hash.slice(41)}`), odd).toBe(true);
      expect(looksLikeSecret(`${odd}${hash.slice(1)}`), `first ${odd}`).toBe(true);
      expect(looksLikeSecret(`${hash.slice(0, 79)}${odd}`), `last ${odd}`).toBe(true);
    }
  });

  it('takes every hexadecimal digit, in both cases, for one, and no character next to them in the alphabet', () => {
    // A run with capitals, lower case and a digit already, and forty of one more character: only if it is not a hexadecimal digit is it a key.
    for (const hex of '0123456789abcdefABCDEF') expect(looksLikeSecret(`aB1${hex.repeat(40)}`), hex).toBe(false);
    expect(looksLikeSecret('0123456789abcdefABCDEF'.repeat(4))).toBe(false);
    for (const other of 'ghijklmnopqrstuvwxyzGHIJKLMNOPQRSTUVWXYZ') expect(looksLikeSecret(`aB1${other.repeat(40)}`), other).toBe(true);
    // The edges of the letters and digits: A, Z, a, z, 0, 9 are in the run, and @ [ ` { : are not (they end it).
    for (const edge of 'AZaz09') expect(looksLikeSecret(`${key(40)}${edge}`), edge).toBe(true);
    for (const outside of '@[`{:') {
      expect(looksLikeSecret(`${key(20)}${outside}${key(19)}`), outside).toBe(false);
      expect(looksLikeSecret(`${key(39)}${outside}`), outside).toBe(false);
    }
  });

  it('is not a hash, a UUID, a conversation id, a short hash or a name', () => {
    const sha1 = 'a94a8fe5ccb19ba61c4c0873d391e987982fbbd3';
    const sha256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
    for (const ok of [
      '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70',
      '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70:agent-a0bd78e0',
      sha1,
      sha256,
      sha256.toUpperCase(),
      `${sha1} ${sha256}`,
      'a1b2c3d',
      'fetchWithRetryAndBackoff_v2',
    ]) {
      expect(looksLikeSecret(ok), ok).toBe(false);
    }
  });

  it('is counted over the characters of base64 text in both of its alphabets and its padding, and ended by any other character', () => {
    // 20 + the character + 20 = 41 characters: a run for the characters of the alphabet, two runs of 20 for any other.
    for (const joiner of ['+', '/', '=', '_', '-']) expect(looksLikeSecret(`${key(20)}${joiner}${key(20)}`), joiner).toBe(true);
    for (const stopper of [' ', '\n', '\t', '.', ':', ',', ';', '@', '!', '?', '*', '(', ')', '\\', '"', "'", '\u00a0', '\u200b', 'é', '\u{1f600}']) {
      expect(looksLikeSecret(`${key(20)}${stopper}${key(20)}`), JSON.stringify(stopper)).toBe(false);
    }
    // The three kinds can be in different parts of one run.
    expect(looksLikeSecret(`${'a'.repeat(14)}-${'B'.repeat(14)}_${'1'.repeat(10)}`)).toBe(true); // 40
    expect(looksLikeSecret(`${'a'.repeat(14)}-${'B'.repeat(14)}_${'1'.repeat(9)}`)).toBe(false); // 39
    // Base64 padding at the end belongs to the run.
    expect(looksLikeSecret(`${key(38)}==`)).toBe(true);
    expect(looksLikeSecret(`${key(37)}==`)).toBe(false);
  });

  it('is found in the middle of a sentence, at the start, at the end, and after a long run that is not one', () => {
    expect(looksLikeSecret(`the value was ${key(40)} here`)).toBe(true);
    expect(looksLikeSecret(`the value was ${key(40)}`)).toBe(true);
    expect(looksLikeSecret(`${key(40)} was the value`)).toBe(true);
    expect(looksLikeSecret(`${'a'.repeat(60)} ${key(40)}`)).toBe(true); // a lower-case run before it
    expect(looksLikeSecret(`${'a1'.repeat(40)} ${key(39)}`)).toBe(false); // a hash-like run before it, then one short
    // A digit in the last place counts, and so does a capital in the first.
    expect(looksLikeSecret(`${letters(39).replace(/./, 'A')}7`)).toBe(true);
    expect(looksLikeSecret(`A${letters(38)}7`)).toBe(true);
    expect(looksLikeSecret(`${letters(40)}7`)).toBe(false); // 41, but no capital
  });
});

describe('a name that holds a secret, and its value', () => {
  const NAMES = ['aws_secret_access_key', 'api_key', 'api-key', 'apikey', 'secret', 'password', 'token'];

  it.each(NAMES)('%s: a value of eight characters is a secret and seven is not, after = and after :', (name) => {
    for (const mark of ['=', ':', ' = ', ': ', '  =   ']) {
      expect(looksLikeSecret(`${name}${mark}12345678`), `${name}${mark}8`).toBe(true);
      expect(looksLikeSecret(`${name}${mark}1234567`), `${name}${mark}7`).toBe(false);
      expect(looksLikeSecret(`${name.toUpperCase()}${mark}abcdefgh`), `${name.toUpperCase()}${mark}`).toBe(true);
    }
  });

  it('needs the name to start a word: a longer word that ends in it is not the name', () => {
    expect(looksLikeSecret('mypassword=12345678')).toBe(false);
    expect(looksLikeSecret('my password=12345678')).toBe(true);
    expect(looksLikeSecret('(password=12345678)')).toBe(true);
    expect(looksLikeSecret('x_token=12345678')).toBe(false); // an underscore is a character of the word
  });

  it('needs the value to be the next thing: a word between them is not the value', () => {
    expect(looksLikeSecret('password is 12345678')).toBe(false);
    expect(looksLikeSecret('password = ')).toBe(false);
    expect(looksLikeSecret('password=')).toBe(false);
  });
});

describe('the tokens that begin with a known prefix', () => {
  it.each([
    ['ghp_', 20],
    ['gho_', 20],
    ['ghu_', 20],
    ['ghs_', 20],
    ['ghr_', 20],
    ['github_pat_', 20],
    ['glpat-', 20],
    ['sk-', 20],
    ['ya29.', 20],
    ['xoxb-', 10],
    ['xoxa-', 10],
    ['xoxp-', 10],
    ['xoxr-', 10],
    ['xoxs-', 10],
    ['xoxe-', 10],
  ])('%s: %i characters after it is a token and one fewer is not', (prefix, least) => {
    expect(looksLikeSecret(`${prefix}${mixed(least)}`), `${prefix}${least}`).toBe(true);
    expect(looksLikeSecret(`${prefix}${mixed(least - 1)}`), `${prefix}${least - 1}`).toBe(false);
    // It starts a word: after a space or a quote it is a token, after a letter it is part of another word.
    expect(looksLikeSecret(`the key ${prefix}${mixed(least)}`)).toBe(true);
    expect(looksLikeSecret(`"${prefix}${mixed(least)}"`)).toBe(true);
    expect(looksLikeSecret(`x${prefix}${mixed(least)}`)).toBe(false);
  });

  it('knows a Google API key by AIza and thirty-five more characters, of which a dash or an underscore can be any', () => {
    // (A body of letters only: with a digit in it, "xAIza" and thirty-five more would be a long run of its own.)
    expect(looksLikeSecret(`AIza${letters(35)}`)).toBe(true);
    expect(looksLikeSecret(`AIza${letters(34)}`)).toBe(false);
    expect(looksLikeSecret(`AIza${letters(36)}`)).toBe(true);
    expect(looksLikeSecret(`AIza${letters(10)}-${letters(10)}_${letters(13)}`)).toBe(true); // 10 + 1 + 10 + 1 + 13 = 35
    expect(looksLikeSecret(`AIza${letters(10)}-${letters(10)}_${letters(12)}`)).toBe(false); // 34
    expect(looksLikeSecret(`AIza${letters(10)}.${letters(30)}`)).toBe(false); // a dot ends it
    expect(looksLikeSecret(`aiza${letters(35)}`)).toBe(false); // the prefix is in the case it is written
    expect(looksLikeSecret(`AIZA${letters(35)}`)).toBe(false);
    expect(looksLikeSecret(`the key AIza${letters(35)}`)).toBe(true);
    expect(looksLikeSecret(`xAIza${letters(35)}`)).toBe(false);
  });

  it('counts the characters after glpat-, sk-, ya29. and the Google prefix in the characters each of them can hold', () => {
    // A GitLab token and an OpenAI key can hold dashes and underscores; a GitHub token (ghp_) can hold neither; a Slack token, dashes.
    expect(looksLikeSecret(`glpat-${letters(9)}-${letters(5)}_${letters(5)}`)).toBe(true); // 9 + 1 + 5 + 1 + 5 = 21
    expect(looksLikeSecret(`glpat-${letters(9)}-${letters(5)}_${letters(4)}`)).toBe(true); // 20
    expect(looksLikeSecret(`glpat-${letters(9)}-${letters(5)}_${letters(3)}`)).toBe(false); // 19
    expect(looksLikeSecret(`sk-${letters(9)}-${letters(5)}_${letters(4)}`)).toBe(true);
    expect(looksLikeSecret(`sk-${letters(9)}-${letters(5)}_${letters(3)}`)).toBe(false);
    expect(looksLikeSecret(`ya29.${letters(9)}-${letters(5)}_${letters(4)}`)).toBe(true);
    expect(looksLikeSecret(`ghp_${letters(10)}_${letters(10)}`)).toBe(false);
    expect(looksLikeSecret(`ghp_${letters(10)}-${letters(10)}`)).toBe(false);
    expect(looksLikeSecret(`xoxb-${letters(4)}-${letters(5)}`)).toBe(true); // 4 + 1 + 5 = 10
    expect(looksLikeSecret(`xoxb-${letters(4)}-${letters(4)}`)).toBe(false);
    expect(looksLikeSecret(`xoxb-${letters(4)}_${letters(5)}`)).toBe(false); // an underscore is not part of a Slack token
    expect(looksLikeSecret(`github_pat_${letters(9)}_${letters(10)}`)).toBe(true); // 9 + 1 + 10 = 20, underscores count
    expect(looksLikeSecret(`github_pat_${letters(9)}-${letters(10)}`)).toBe(false); // a dash is not
  });

  it('knows an AWS key by its prefix and exactly sixteen letters and digits in capitals', () => {
    for (const prefix of ['AKIA', 'ASIA']) {
      expect(looksLikeSecret(`${prefix}ABCDEFGHIJKLMNOP`)).toBe(true);
      expect(looksLikeSecret(`${prefix}ABCDEFGHIJKLMNO`)).toBe(false); // 15
      expect(looksLikeSecret(`${prefix}ABCDEFGHIJKLMNOPQ`)).toBe(false); // 17
      expect(looksLikeSecret(`${prefix.toLowerCase()}ABCDEFGHIJKLMNOP`)).toBe(false);
      expect(looksLikeSecret(`x${prefix}ABCDEFGHIJKLMNOP`)).toBe(false);
      expect(looksLikeSecret(`key=${prefix}ABCDEFGHIJKLMNOP.`)).toBe(true);
    }
  });

  it('knows a JSON web token by its three parts of eight characters or more after the first three', () => {
    const part = (n: number) => 'A'.repeat(n);
    expect(looksLikeSecret(`eyJ${part(8)}.eyJ${part(8)}.${part(8)}`)).toBe(true);
    expect(looksLikeSecret(`eyJ${part(8)}.eyJ${part(8)}`)).toBe(false); // two parts
    expect(looksLikeSecret(`eyJ${part(8)}`)).toBe(false); // one
    expect(looksLikeSecret(`eyJ${part(7)}.eyJ${part(8)}.${part(8)}`)).toBe(false); // a header of seven
    expect(looksLikeSecret(`eyJ${part(8)}.${part(7)}.${part(8)}`)).toBe(false); // a payload of seven
    expect(looksLikeSecret(`eyJ${part(8)}.${part(8)}.${part(8)}`)).toBe(true); // (the payload need not start with eyJ)
    expect(looksLikeSecret(`eyJ${part(8)}.eyJ${part(8)}.${part(7)}`)).toBe(false); // a signature of seven
    expect(looksLikeSecret(`xeyJ${part(8)}.eyJ${part(8)}.${part(8)}`)).toBe(false); // not at the start of a word
  });
});

describe('an authorization header, a bearer token, and credentials in a URL', () => {
  it('needs twelve characters of token after "bearer " and after "authorization:"', () => {
    expect(looksLikeSecret('Bearer abcdef012345')).toBe(true); // 12
    expect(looksLikeSecret('Bearer abcdef01234')).toBe(false); // 11
    expect(looksLikeSecret('bearer   abcdef012345')).toBe(true);
    expect(looksLikeSecret('Bearerabcdef012345')).toBe(false); // no gap
    expect(looksLikeSecret('Authorization: abcdef012345')).toBe(true);
    expect(looksLikeSecret('authorization :   abcdef012345')).toBe(true);
    expect(looksLikeSecret('Authorization: abcdef01234')).toBe(false);
    expect(looksLikeSecret('Authorization abcdef012345')).toBe(false); // no colon
    expect(looksLikeSecret('xBearer abcdef012345')).toBe(false);
  });

  it('needs a user, a colon, a password and an @ in the address', () => {
    expect(looksLikeSecret('postgres://admin:pass1234@db.internal/app')).toBe(true);
    expect(looksLikeSecret('postgres://admin@db.internal/app')).toBe(false); // no password
    expect(looksLikeSecret('postgres://admin:@db.internal/app')).toBe(false); // an empty one
    expect(looksLikeSecret('postgres://:pass1234@db.internal/app')).toBe(false); // no user
    expect(looksLikeSecret('postgres://admin:pass1234 db.internal/app')).toBe(false); // no @
    expect(looksLikeSecret('://admin:pass1234@db')).toBe(false); // no scheme
    expect(looksLikeSecret('1ttp://admin:pass1234@db')).toBe(false); // a scheme starts with a letter
    expect(looksLikeSecret('git+ssh://user:pass1234@host/repo.git')).toBe(true);
  });

  it('knows a private key by its two lines, in either of the key types', () => {
    expect(looksLikeSecret('-----BEGIN PRIVATE KEY-----')).toBe(true);
    expect(looksLikeSecret('-----BEGIN EC PRIVATE KEY-----')).toBe(true);
    expect(looksLikeSecret('-----BEGIN PUBLIC KEY-----')).toBe(false);
    expect(looksLikeSecret('PRIVATE KEY-----')).toBe(false);
    expect(looksLikeSecret('-----BEGIN ')).toBe(false);
  });
});

describe('how much of a text is read', () => {
  it('reads the first 2,000 characters: a secret that ends inside them is found, one that starts after them is not', () => {
    const inside = `${'x '.repeat(900)}ghp_${mixed(20)}`; // ends at 1,824
    const after = `${'x '.repeat(1_100)}ghp_${mixed(20)}`; // starts at 2,200
    expect(inside.length).toBeLessThan(2_000);
    expect(looksLikeSecret(inside)).toBe(true);
    expect(looksLikeSecret(after)).toBe(false);
  });

  it('reads exactly 2,000 characters: a token whose last character is the 2,000th is found, one that ends a character later is not', () => {
    const token = `ghp_${mixed(20)}`; // 24 characters
    expect(looksLikeSecret(`${' '.repeat(2_000 - token.length)}${token}`)).toBe(true);
    expect(looksLikeSecret(`${' '.repeat(2_000 - token.length + 1)}${token}`)).toBe(false);
    expect(looksLikeSecret(`${' '.repeat(2_000 - token.length)}${token}${mixed(500)}`)).toBe(true); // what comes after is not read, and not needed
    // The same with a name and its value: "password=" and eight characters.
    const assignment = 'password=12345678';
    expect(looksLikeSecret(`${' '.repeat(2_000 - assignment.length)}${assignment}`)).toBe(true);
    expect(looksLikeSecret(`${' '.repeat(2_000 - assignment.length + 1)}${assignment}`)).toBe(false);
    // A long run is judged on the 2,000 characters too: forty of them, the last at 2,000, and one short of forty.
    expect(looksLikeSecret(`${' '.repeat(1_960)}${key(40)}`)).toBe(true);
    expect(looksLikeSecret(`${' '.repeat(1_961)}${key(40)}`)).toBe(false);
  });

  it('judges a secret that is cut by the end of what is read by what is there', () => {
    // 1,994 characters up to and with the prefix, then a token of which only the first six characters are inside the 2,000.
    const cut = `${'x '.repeat(995)}ghp_${mixed(40)}`; // the prefix ends at 1,994
    expect(looksLikeSecret(cut)).toBe(false);
    const whole = `${'x '.repeat(985)}ghp_${mixed(20)}`; // ends at 1,994: all of it is read
    expect(looksLikeSecret(whole)).toBe(true);
  });
});

describe('a secret that comes after a character whose lower case is longer than it', () => {
  // U+0130 (a capital I with a dot) is one code unit, and its lower case is two. A scan that lower-cases a text and then reads the
  // text at the places it found in the copy reads every place after the dot one too far, and misses the secret or invents one.
  const DOTTED = 'İ';

  it.each([
    ['an assignment', 'password=hunter2hunter2'],
    ['an assignment with spaces', 'api_key : abcdef0123456789'],
    ['an authorization header', 'Authorization: abcdef0123456789ABCD'],
    ['a bearer token', 'Bearer abcdef0123456789ABCD'],
  ])('is still known: %s', (_name, secret) => {
    expect(looksLikeSecret(secret), 'on its own').toBe(true);
    for (const count of [1, 2, 4, 20, 300]) {
      expect(looksLikeSecret(`${DOTTED.repeat(count)} ${secret}`), `after ${count}`).toBe(true);
      expect(looksLikeSecret(`${DOTTED.repeat(count)}${secret}`), `right after ${count}`).toBe(true);
      expect(looksLikeSecret(`${DOTTED} ${secret.toUpperCase()}`), 'in capitals').toBe(true);
    }
  });

  it('is not made one by it: a name that is part of a longer word is still not a name, and a value that is not next is still not the value', () => {
    for (const count of [1, 2, 4, 20]) {
      const dots = DOTTED.repeat(count);
      expect(looksLikeSecret(`${dots} mypassword=12345678`)).toBe(false);
      expect(looksLikeSecret(`${dots} password is 12345678`)).toBe(false);
      expect(looksLikeSecret(`${dots} password=1234567`)).toBe(false);
      expect(looksLikeSecret(`${dots} xBearer abcdef0123456789ABCD`)).toBe(false);
      expect(looksLikeSecret(`${dots} bearer abcdef01234`)).toBe(false);
    }
  });
});

// ---------------------------------------------------------------------------
// The rules, written again as regular expressions, and a few thousand texts built from their parts
// ---------------------------------------------------------------------------

/**
 * `looksLikeSecret` has no regular expression in it (the text is wire data and typed by a person). Its rules are written
 * here as regular expressions, from the description at the top of the module, and the two are compared on texts made of
 * the parts a rule is made of, cut one short, exact and one over: the tokens with each prefix, the names with each mark,
 * the values with a character that ends them, the words they must start. A text on which they differ is a text on which
 * one of the two does not do what the description says.
 */
const WORD_START = String.raw`(?<![A-Za-z0-9_])`;
const SPACE = String.raw`[ \t\n\v\f\r]`;
const RULES: ReadonlyArray<readonly [string, RegExp]> = [
  ['private key', /-----BEGIN [\s\S]*?PRIVATE KEY-----/],
  ['github', new RegExp(`${WORD_START}(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})`)],
  ['gitlab', new RegExp(`${WORD_START}glpat-[A-Za-z0-9_-]{20,}`)],
  ['openai', new RegExp(`${WORD_START}sk-[A-Za-z0-9_-]{20,}`)],
  ['slack', new RegExp(`${WORD_START}xox[baprse]-[A-Za-z0-9-]{10,}`)],
  ['google key', new RegExp(`${WORD_START}AIza[A-Za-z0-9_-]{35,}`)],
  ['google token', new RegExp(`${WORD_START}ya29\\.[A-Za-z0-9_-]{20,}`)],
  ['aws', new RegExp(`${WORD_START}(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Za-z0-9_])`)],
  ['jwt', new RegExp(`${WORD_START}eyJ[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}`)],
  ['bearer', new RegExp(`${WORD_START}bearer${SPACE}+[A-Za-z0-9._~+/=-]{12,}`, 'i')],
  ['authorization', new RegExp(`${WORD_START}authorization${SPACE}*:${SPACE}*[A-Za-z0-9._~+/=-]{12,}`, 'i')],
  ['url', new RegExp(`(?<![A-Za-z0-9+.-])[A-Za-z][A-Za-z0-9+.-]*://[^ \\t\\n\\v\\f\\r/:@]+:[^ \\t\\n\\v\\f\\r@]+@`)],
  ['assignment', new RegExp(`${WORD_START}(?:aws_secret_access_key|api_key|api-key|apikey|secret|password|token)${SPACE}*[:=]${SPACE}*[^ \\t\\n\\v\\f\\r'"\`]{8,}`, 'i')],
];

function isLongRun(text: string): boolean {
  for (const run of text.match(/[A-Za-z0-9+/=_-]+/g) ?? []) {
    if (run.length >= 40 && /[A-Z]/.test(run) && /[a-z]/.test(run) && /[0-9]/.test(run) && /[^0-9A-Fa-f]/.test(run)) return true;
  }
  return false;
}

/** The rules of the description, on the first 2,000 characters; the name of the first that holds, or null. */
function expectedRule(text: string): string | null {
  const read = text.slice(0, 2_000);
  for (const [name, rule] of RULES) if (rule.test(read)) return name;
  return isLongRun(read) ? 'long run' : null;
}

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe('looksLikeSecret against the rules written as regular expressions', () => {
  const rand = rng(1118);
  const pick = <T,>(items: readonly T[]): T => items[Math.floor(rand() * items.length)];
  const upto = (n: number) => Math.floor(rand() * n);
  const from = (alphabet: string, n: number) => Array.from({ length: n }, () => alphabet[upto(alphabet.length)]).join('');
  /** A length at, one under and one over a threshold, or anywhere near it. */
  const around = (least: number) => Math.max(0, least + pick([-2, -1, -1, 0, 0, 0, 1, 1, 2, 10]));
  const caseOf = (text: string) => Array.from(text, (character) => (rand() < 0.5 ? character.toUpperCase() : character)).join('');

  const ALNUM = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  // The ends of the ranges of letters and digits, and the characters on either side of them, are in the bodies on purpose.
  const BODIES = [ALNUM, ALNUM + '_', ALNUM + '-', ALNUM + '_-', ALNUM + '.', ALNUM + '+/=', 'abcdef0123456789', 'ABCDEF0123456789', 'a1', 'Z9', 'AZaz09@[`{:/', 'AZaz09gG', '09afAF', '@[`{/:'];
  const SPACES = [' ', '  ', '\t', '\n', '\r\n', '\v', '\f', '\u0008', '\u000e', '\u001f', '\u00a0', '\u3000', '\u2003', '\u200b', ''];
  const STOPPERS = [' ', '\n', '"', "'", '`', '.', ',', ';', ')', ']', '@', 'é', '\u200b', '\u{1f600}', 'İ', '', ''];
  const BEFORE = ['', '', '', ' ', '"', "'", '(', '=', ':', '/', '-', '.', 'x', 'X', '_', '9', 'é', 'İ', 'İİ', '\u200b', '\u00a0'];
  const NAMES = ['aws_secret_access_key', 'api_key', 'api-key', 'apikey', 'secret', 'password', 'token', 'passwords', 'secrets', 'pass', 'tokens', 'key'];
  const FILLER = ['the', 'value', 'was', 'retry', 'limit', 'src/lib/a.ts', 'path:src/payments', 'tool:Bash', 'commit', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70', 'ghp', 'sk', 'eyJ', 'Bearer', 'bearer'];

  /** One part: a token, an assignment, a header, an address, a key, a run, or a word; cut near its threshold. */
  function part(): string {
    const kind = upto(14);
    const stop = pick(STOPPERS);
    if (kind === 0) return pick(['ghp_', 'gho_', 'ghu_', 'ghs_', 'ghr_', 'ghx_', 'github_pat_', 'GHP_']) + from(pick(BODIES), around(20)) + stop;
    if (kind === 1) return pick(['glpat-', 'sk-', 'sk-proj-', 'ya29.', 'ya29', 'SK-']) + from(pick(BODIES), around(20)) + stop;
    if (kind === 2) return pick(['xoxb-', 'xoxa-', 'xoxp-', 'xoxr-', 'xoxs-', 'xoxe-', 'xoxz-', 'xox-']) + from(pick(BODIES), around(10)) + stop;
    if (kind === 3) return pick(['AIza', 'AIZA', 'aiza', 'AIz']) + from(pick(BODIES), around(35)) + stop;
    if (kind === 4) return pick(['AKIA', 'ASIA', 'AKIB', 'akia']) + from(pick(['ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789', ALNUM, 'A9']), around(16)) + pick(['', '', '', '_', '-', ' ', 'a', '9', '.']);
    if (kind === 5) {
      const piece = () => from(pick(BODIES), around(8));
      return pick(['eyJ', 'eyj', 'EYJ']) + piece() + pick(['.', '.', '..', '-']) + pick(['eyJ', '']) + piece() + pick(['.', '.', ',']) + piece() + stop;
    }
    if (kind === 6) return caseOf(pick(['bearer', 'authorization', 'Bearer', 'Authorization'])) + pick(SPACES) + pick([':', ':', ' ', '=', '']) + pick(SPACES) + pick(['', 'Bearer ', 'bearer\t']) + from(pick(BODIES), around(12)) + stop;
    if (kind === 7) return pick(NAMES) + pick(SPACES) + pick(['=', ':', '=', ':', ' ', ':=', '==']) + pick(SPACES) + from(pick(BODIES.concat(['\'"`', 'a b'])), around(8)) + stop;
    if (kind === 8) return caseOf(pick(NAMES)) + pick(SPACES) + pick(['=', ':']) + pick(SPACES) + from(pick(BODIES), around(8)) + stop;
    if (kind === 9) {
      return pick(['postgres', 'git+ssh', '1ttp', 'h', 'x.y-z+w', '', '_http', 'a1']) + pick(['://', '://', ':/', '//']) + from(pick(['abc', 'a:b', 'a@b', 'a/b', 'a b', '']), upto(5)) + pick([':', ':', '']) + from(pick(['abc12', 'a@b', 'a b', '']), upto(7)) + pick(['@', '@', ' ', '']) + pick(['db.internal/app', '']);
    }
    if (kind === 10) return pick(['-----BEGIN ', '-----BEGIN ', '----BEGIN ']) + pick(['RSA ', 'EC ', 'OPENSSH ', 'PUBLIC ', '', 'CERTIFICATE ']) + pick(['PRIVATE KEY-----', 'PRIVATE KEY----', 'PUBLIC KEY-----', 'private key-----']);
    if (kind === 11) return from(pick(BODIES.concat(['abcdef0123456789ABCDEF', 'ghijklmnop', '0123456789'])), around(40)) + stop;
    return pick(FILLER);
  }

  function randomText(): string {
    const pieces: string[] = [];
    for (let n = 1 + upto(4); n > 0; n--) pieces.push(pick(BEFORE), part(), pick(['', ' ', ' ', '\n', ' and ', ': ', ', ']));
    let text = pieces.join('');
    // Now and then, push it past the first 2,000 characters, or start it half way into them.
    const roll = rand();
    if (roll < 0.04) text = 'x '.repeat(940 + upto(100)) + text;
    else if (roll < 0.06) text = pick(['İ', 'é', '\u{1f600}']).repeat(1_990 + upto(20)) + text;
    return text;
  }

  it('agrees with them on 40,000 texts, and the texts are not all of one kind', { timeout: 30_000 }, () => {
    const disagreements: string[] = [];
    const rules = new Map<string, number>();
    let secrets = 0;
    for (let k = 0; k < 40_000; k++) {
      const text = randomText();
      const rule = expectedRule(text);
      if (rule !== null) {
        secrets++;
        rules.set(rule, (rules.get(rule) ?? 0) + 1);
      }
      if (looksLikeSecret(text) !== (rule !== null) && disagreements.length < 12) disagreements.push(`${rule === null ? 'passes' : `is ${rule}`}: ${JSON.stringify(text.length > 300 ? `${text.slice(0, 150)}...${text.slice(-150)}` : text)}`);
    }
    expect(disagreements.join('\n')).toBe('');
    // Neither answer is rare, and every rule is the reason for many texts.
    expect(secrets).toBeGreaterThan(8_000);
    expect(secrets).toBeLessThan(32_000);
    for (const [name] of [...RULES, ['long run', null] as const]) expect(rules.get(name) ?? 0, `texts that hold only by ${name}`).toBeGreaterThan(20);
  });
});
