/**
 * Whether a text LOOKS like a secret (issue #118), so that "Copy link to this view" can stop and ask before it
 * puts the text of a search into a link that anyone who has the link will read.
 *
 * The patterns are the high-signal ones of `scrub` in `cvc-core/src/privacy.rs` (read, not shared): a PEM private
 * key, the access keys and tokens that begin with a known prefix (AWS, GitHub, GitLab, OpenAI, Slack, Google), a JSON
 * web token, an authorization header or bearer token, credentials in a URL, and an assignment to a name such as
 * `password`, `token` or `api_key`; and, for the secrets that have no shape of their own (an AWS secret key, a
 * random key from a service with no prefix), a long run of the characters of base64 text that has an upper-case
 * letter, a lower-case letter and a digit. A hash is not taken for one: a commit hash, a SHA-256 and a UUID are all
 * hexadecimal digits (and dashes), and are searched for all the time, and a warning on every search for an id would
 * teach people to ignore it. It is a nudge and not a guarantee: a secret in another shape passes (a short password is
 * one), and a long mixed-case word with a digit in it is taken for one.
 *
 * Pure, and no regular expression: the text is typed by a person, but the same code reads whatever the address
 * brought, and it reads at most the first `LOOKED_AT` characters, once each.
 */

/** The text read: a search box takes 500 characters; a text longer than this is judged by its beginning. */
const LOOKED_AT = 2_000;
/**
 * A run of base64 characters this long, that has an upper-case letter, a lower-case letter and a digit and is not all
 * hexadecimal, is taken for a secret: the length of an AWS secret access key, which is the shortest of the kind. (A SHA-256 is
 * sixty-four hexadecimal digits, a commit hash forty, and a UUID thirty-six with its dashes, and none of them is one.)
 */
const LONG_RUN = 40;

const isDigit = (code: number) => code >= 0x30 && code <= 0x39;
const isLetter = (code: number) => (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a);
const isAlphanumeric = (code: number) => isDigit(code) || isLetter(code);
/** What `\w` matches. */
const isWord = (code: number) => isAlphanumeric(code) || code === 0x5f;
const isSpace = (code: number) => code === 0x20 || (code >= 0x09 && code <= 0x0d);

const isHexDigit = (code: number) => isDigit(code) || (code >= 0x41 && code <= 0x46) || (code >= 0x61 && code <= 0x66);
/** The characters of base64 text, in its two alphabets: [A-Za-z0-9+/=_-]. */
const isBase64Char = (code: number) => isAlphanumeric(code) || code === 0x2b || code === 0x2f || code === 0x3d || code === 0x5f || code === 0x2d;

/**
 * `text` with `A` to `Z` in lower case and every other character as it is: always as long as `text`, so that a place in
 * one is the same place in the other. (`toLowerCase` makes the two characters `i` and a dot of `İ`, and every place after
 * it in the lower-case copy is one too many.)
 */
function asciiLower(text: string): string {
  let out = '';
  let from = 0;
  for (let at = 0; at < text.length; at++) {
    const code = text.charCodeAt(at);
    if (code >= 0x41 && code <= 0x5a) {
      out += text.slice(from, at) + String.fromCharCode(code + 0x20);
      from = at + 1;
    }
  }
  return from === 0 ? text : out + text.slice(from);
}

/** A word boundary before `at`, as `\b` means it before a word character. */
const startsWord = (text: string, at: number) => at === 0 || !isWord(text.charCodeAt(at - 1));

/** How many characters from `at` on are in a class, up to `most`. */
function runOf(text: string, at: number, inClass: (code: number) => boolean, most = Infinity): number {
  let end = at;
  while (end < text.length && end - at < most && inClass(text.charCodeAt(end))) end++;
  return end - at;
}

const tokenChar = (code: number) => isAlphanumeric(code) || code === 0x5f || code === 0x2d; // [A-Za-z0-9_-]
const dashChar = (code: number) => isAlphanumeric(code) || code === 0x2d; // [A-Za-z0-9-]
const upperDigit = (code: number) => isDigit(code) || (code >= 0x41 && code <= 0x5a);
const credentialChar = (code: number) => isAlphanumeric(code) || '._~+/=-'.includes(String.fromCharCode(code)); // [A-Za-z0-9._~+/=-]

/** Every place `needle` is in `text`, from the start. */
function* occurrences(text: string, needle: string): Generator<number> {
  for (let at = text.indexOf(needle); at >= 0; at = text.indexOf(needle, at + 1)) yield at;
}

/** `prefix` followed by at least `least` characters of a class, starting a word. */
function prefixed(text: string, prefix: string, inClass: (code: number) => boolean, least: number): boolean {
  for (const at of occurrences(text, prefix)) {
    if (startsWord(text, at) && runOf(text, at + prefix.length, inClass) >= least) return true;
  }
  return false;
}

function hasPrivateKey(text: string): boolean {
  for (const at of occurrences(text, '-----BEGIN ')) if (text.indexOf('PRIVATE KEY-----', at) >= 0) return true;
  return false;
}

function hasAwsKey(text: string): boolean {
  for (const prefix of ['AKIA', 'ASIA']) {
    for (const at of occurrences(text, prefix)) {
      const end = at + prefix.length;
      if (startsWord(text, at) && runOf(text, end, upperDigit, 16) === 16 && (end + 16 >= text.length || !isWord(text.charCodeAt(end + 16)))) return true;
    }
  }
  return false;
}

function hasJwt(text: string): boolean {
  for (const at of occurrences(text, 'eyJ')) {
    if (!startsWord(text, at)) continue;
    let end = at + 3;
    let parts = 0;
    // A header (the `eyJ` and eight or more more), a dot, two more parts of eight or more.
    for (;;) {
      const run = runOf(text, end, tokenChar);
      if (run < 8) break;
      parts++;
      end += run;
      if (parts === 3) return true;
      if (text.charCodeAt(end) !== 0x2e) break;
      end++;
    }
  }
  return false;
}

function hasKnownToken(text: string): boolean {
  for (const kind of ['ghp_', 'gho_', 'ghu_', 'ghs_', 'ghr_']) if (prefixed(text, kind, isAlphanumeric, 20)) return true;
  if (prefixed(text, 'github_pat_', (code) => isAlphanumeric(code) || code === 0x5f, 20)) return true;
  if (prefixed(text, 'glpat-', tokenChar, 20)) return true;
  if (prefixed(text, 'sk-proj-', tokenChar, 20) || prefixed(text, 'sk-', tokenChar, 20)) return true;
  for (const kind of ['b', 'a', 'p', 'r', 's', 'e']) if (prefixed(text, `xox${kind}-`, dashChar, 10)) return true;
  // A Google API key is `AIza` and thirty-five more, and any of them can be a dash or an underscore.
  if (prefixed(text, 'AIza', tokenChar, 35)) return true;
  return prefixed(text, 'ya29.', tokenChar, 20);
}

/** `authorization:` or `bearer ` (in any case) followed by a token of twelve characters or more. */
function hasAuthorization(text: string): boolean {
  const lower = asciiLower(text);
  for (const at of occurrences(lower, 'bearer')) {
    if (!startsWord(text, at)) continue;
    const gap = runOf(text, at + 6, isSpace);
    if (gap > 0 && runOf(text, at + 6 + gap, credentialChar) >= 12) return true;
  }
  for (const at of occurrences(lower, 'authorization')) {
    if (!startsWord(text, at)) continue;
    let end = at + 13;
    end += runOf(text, end, isSpace);
    if (text.charCodeAt(end) !== 0x3a) continue;
    end += 1 + runOf(text, end + 1, isSpace);
    if (runOf(text, end, credentialChar) >= 12) return true;
  }
  return false;
}

/** `scheme://user:password@`. */
function hasUrlCredentials(text: string): boolean {
  for (const at of occurrences(text, '://')) {
    // A scheme before it: a letter, then letters, digits, `+`, `.` or `-`.
    let start = at;
    while (start > 0 && (isAlphanumeric(text.charCodeAt(start - 1)) || '+.-'.includes(text[start - 1]))) start--;
    if (start === at || !isLetter(text.charCodeAt(start))) continue;
    let end = at + 3;
    const notUser = (code: number) => isSpace(code) || code === 0x2f || code === 0x3a || code === 0x40; // whitespace / : @
    const user = runOf(text, end, (code) => !notUser(code));
    end += user;
    if (user === 0 || text.charCodeAt(end) !== 0x3a) continue;
    end++;
    const secret = runOf(text, end, (code) => !isSpace(code) && code !== 0x40);
    if (secret > 0 && text.charCodeAt(end + secret) === 0x40) return true;
  }
  return false;
}

const SECRET_NAMES = ['aws_secret_access_key', 'api_key', 'api-key', 'apikey', 'secret', 'password', 'token'] as const;

/** `password = hunter2hunter2`, `token: abc...`: a name that holds a secret, and a value of eight characters or more. */
function hasSecretAssignment(text: string): boolean {
  const lower = asciiLower(text);
  for (const name of SECRET_NAMES) {
    for (const at of occurrences(lower, name)) {
      if (!startsWord(text, at)) continue;
      let end = at + name.length;
      end += runOf(text, end, isSpace);
      const mark = text.charCodeAt(end);
      if (mark !== 0x3a && mark !== 0x3d) continue;
      end += 1 + runOf(text, end + 1, isSpace);
      if (runOf(text, end, (code) => !isSpace(code) && code !== 0x27 && code !== 0x22 && code !== 0x60) >= 8) return true;
    }
  }
  return false;
}

/**
 * A run of `LONG_RUN` characters or more of base64 text (either alphabet, and the padding) that has an upper-case letter, a
 * lower-case letter and a digit, and that is not all hexadecimal digits: not a hash, which is.
 */
function hasLongRun(text: string): boolean {
  let at = 0;
  while (at < text.length) {
    if (!isBase64Char(text.charCodeAt(at))) {
      at++;
      continue;
    }
    const start = at;
    let upper = false;
    let lower = false;
    let digit = false;
    let notHex = false;
    for (; at < text.length; at++) {
      const code = text.charCodeAt(at);
      if (!isBase64Char(code)) break;
      if (code >= 0x41 && code <= 0x5a) upper = true;
      else if (code >= 0x61 && code <= 0x7a) lower = true;
      else if (isDigit(code)) digit = true;
      if (!isHexDigit(code)) notHex = true;
    }
    if (at - start >= LONG_RUN && upper && lower && digit && notHex) return true;
  }
  return false;
}

/** Whether `text` looks like a secret (see the top of this file). */
export function looksLikeSecret(text: string): boolean {
  const read = text.length > LOOKED_AT ? text.slice(0, LOOKED_AT) : text;
  return (
    hasPrivateKey(read) ||
    hasKnownToken(read) ||
    hasAwsKey(read) ||
    hasJwt(read) ||
    hasAuthorization(read) ||
    hasUrlCredentials(read) ||
    hasSecretAssignment(read) ||
    hasLongRun(read)
  );
}
