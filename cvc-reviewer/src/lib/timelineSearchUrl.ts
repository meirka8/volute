/**
 * The filters of the timeline in the page's address, and the link to a view (issue #118).
 *
 * The address of the page keeps the FILTERS, after the `#`, so that a reload, or a step back to the page, finds
 * them again. It never keeps what was typed in the search box: a search is often a word from a log, a name, or a
 * secret, and the address goes to the history, to the screen, and to whoever the page is shared with. The
 * fragment is never sent to a server (and the page sends no referrer, see `index.html`).
 *
 *     #conv=<id>&origin=prompt&tool=Bash&link=temporal&commit=<sha>&file=src/a.ts
 *
 * - `conv`, `origin`, `tool`, `link` and `commit` are filters, written once for each value chosen
 *   (`tool=Bash&tool=Read`); `file` is the path of the file the steps must have touched.
 * - The search text is `q`. It is never written into the address of the page. It is written into the link
 *   that "Copy link to this view" makes (`viewLink`), because a link that does not hold it does not
 *   reproduce the view, and the person chose to copy it. A page opened with a `q` in its fragment applies it
 *   and takes it out of the address at once.
 * - What is read from the address is written by anyone, so every value is checked: one that is not what its
 *   parameter holds is left out, without a word, and the rest of the address is read as if it had not been
 *   there. Every value that is kept is cleaned of hidden characters and is only ever drawn as text.
 * - What is written is what is read: a value that the reader would refuse or change (too long, not the shape
 *   of an id, with a hidden character in it) is left out of the address, never written cut.
 * - Writing keeps the parameters of the fragment that are not ours, byte for byte; ours are written last, in
 *   this order, and only when they have a value. The query string of the address is never read for a search or a
 *   filter; the parameters of ours that one holds (a link from before the filters moved to the fragment) are taken
 *   out of it, and the rest of it is left as it was (`withoutSearchParams`).
 *
 * Pure: the code that reads and writes `window.location` is `hooks/useSearchUrl`.
 */
import { stripHidden } from './hiddenChars';
import { filterCount, LINK_KINDS, NO_FILTERS, ORIGIN_KEYS, type Filters, type LinkKind, type OriginKey } from './timelineSearch';

/** Text typed in the box, and the filters. */
export interface SearchState {
  readonly q: string;
  readonly filters: Filters;
}

export const EMPTY_SEARCH: SearchState = { q: '', filters: NO_FILTERS };

/** The parameters this module owns, in the order it writes them. */
export const SEARCH_PARAMS = ['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file'] as const;

/** Longest text typed in the box; longer is cut when read. */
export const MAX_QUERY_LENGTH = 500;
const MAX_VALUES = 50;
const MAX_ID_LENGTH = 256;
const MAX_NAME_LENGTH = 200;
const MAX_PATH_LENGTH = 1024;

/** Hidden characters out, and the white space that can not be part of a name turned into a space. */
function cleaned(value: string): string {
  const text = stripHidden(value, 'strict');
  if (text.indexOf('\n') < 0 && text.indexOf('\r') < 0 && text.indexOf('\t') < 0) return text;
  return text.split('\r\n').join(' ').split('\n').join(' ').split('\r').join(' ').split('\t').join(' ');
}

/** An id the capture accepts: 1 to 256 ASCII letters, digits, `.`, `_`, `-` or `:`. */
function isIdentifier(value: string): boolean {
  if (value.length === 0 || value.length > MAX_ID_LENGTH) return false;
  for (let at = 0; at < value.length; at++) {
    const code = value.charCodeAt(at);
    const word = (code >= 0x30 && code <= 0x39) || (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a);
    if (!word && code !== 0x2e && code !== 0x5f && code !== 0x2d && code !== 0x3a) return false;
  }
  return true;
}

/** Seven to sixty-four hexadecimal digits: the start of a commit hash, or the hash. */
function isHashPrefix(value: string): boolean {
  if (value.length < 7 || value.length > 64) return false;
  for (let at = 0; at < value.length; at++) {
    const code = value.charCodeAt(at);
    if (!((code >= 0x30 && code <= 0x39) || (code >= 0x61 && code <= 0x66))) return false;
  }
  return true;
}

/** A name or a path the reader would keep as it is: not too long, and nothing in it that reading cleans away. */
function isName(value: string, limit: number): boolean {
  return value !== '' && value.length <= limit && cleaned(value) === value;
}

function unique<T>(values: Iterable<T>): T[] {
  const kept: T[] = [];
  for (const value of values) {
    if (!kept.includes(value)) kept.push(value);
    if (kept.length >= MAX_VALUES) break;
  }
  return kept;
}

/** The text of a fragment (`location.hash`) without its `#`. */
function fragmentBody(fragment: string): string {
  return fragment.startsWith('#') ? fragment.slice(1) : fragment;
}

/**
 * Reads the search and filters out of the fragment of an address (`location.hash`, with or without its `#`).
 * Nothing else of the address is read: the old `?q=` of the query string, which was never released, is not.
 */
export function readSearchState(fragment: string): SearchState {
  let params: URLSearchParams;
  try {
    const body = fragmentBody(fragment);
    // A leading `?` is the query string's way of starting, not the fragment's: it is part of the first name here.
    params = new URLSearchParams(body.startsWith('?') ? `&${body}` : body);
  } catch {
    return EMPTY_SEARCH;
  }
  const first = (name: string): string | null => params.get(name);
  const all = (name: string): string[] => params.getAll(name);

  const q = cleaned(first('q') ?? '').slice(0, MAX_QUERY_LENGTH);
  const file = cleaned(first('file') ?? '').slice(0, MAX_PATH_LENGTH);
  const filters: Filters = {
    conv: unique(all('conv').filter(isIdentifier)),
    origin: unique(all('origin').filter((value): value is OriginKey => (ORIGIN_KEYS as readonly string[]).includes(value))),
    tool: unique(
      all('tool')
        .map(cleaned)
        .filter((name) => name !== '' && name.length <= MAX_NAME_LENGTH),
    ),
    link: unique(all('link').filter((value): value is LinkKind => (LINK_KINDS as readonly string[]).includes(value))),
    commit: unique(all('commit').map((value) => value.toLowerCase()).filter(isHashPrefix)),
    file: file === '' ? { mode: 'off' } : { mode: 'path', path: file },
  };
  // Nothing usable in the address: the one empty state, so that nothing downstream sees a change.
  if (q === '' && filterCount(filters) === 0) return EMPTY_SEARCH;
  return { q, filters };
}

/**
 * The parameters that say the FILTERS of `filters`, as the fragment of an address without its `#`: empty when there is
 * nothing to say. `openFile` is the file the file filter means when it follows the open one. A value that could not be
 * read back as it is (too long, not the shape of an id) is left out.
 */
export function encodeFilters(filters: Filters, openFile: string | null): string {
  const params = new URLSearchParams();
  for (const id of filters.conv) if (isIdentifier(id)) params.append('conv', id);
  for (const key of filters.origin) params.append('origin', key);
  for (const name of filters.tool) if (isName(name, MAX_NAME_LENGTH)) params.append('tool', name);
  for (const kind of filters.link) params.append('link', kind);
  for (const hash of filters.commit) if (isHashPrefix(hash)) params.append('commit', hash);
  const file = filters.file.mode === 'path' ? filters.file.path : filters.file.mode === 'open' ? openFile : null;
  if (file !== null && isName(file, MAX_PATH_LENGTH)) params.append('file', file);
  return params.toString();
}

/**
 * The parameters that say all of `state`, the search text first: for a link someone chose to make. (The address
 * of the page is written with `encodeFilters`.) The text is left out when it is longer than the box takes.
 */
export function encodeSearchState(state: SearchState, openFile: string | null): string {
  const filters = encodeFilters(state.filters, openFile);
  if (state.q === '' || state.q.length > MAX_QUERY_LENGTH) return filters;
  const text = new URLSearchParams({ q: state.q }).toString();
  return filters === '' ? text : `${text}&${filters}`;
}

const OWN = new Set<string>(SEARCH_PARAMS);

/** The name of a `name=value` pair of a fragment, decoded as a browser decodes one. */
function nameOf(pair: string): string {
  const equals = pair.indexOf('=');
  const raw = (equals < 0 ? pair : pair.slice(0, equals)).split('+').join(' ');
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

/**
 * `fragment` (an address's `#...`, with or without the `#`) with our parameters replaced by `encoded` (see
 * `encodeFilters`) and every other parameter left exactly as it was. The result starts with `#`, or is empty
 * when nothing is left.
 */
export function withFragmentParams(fragment: string, encoded: string): string {
  const body = fragmentBody(fragment);
  const kept = body === '' ? [] : body.split('&').filter((pair) => pair !== '' && !OWN.has(nameOf(pair)));
  if (encoded !== '') kept.push(encoded);
  return kept.length === 0 ? '' : `#${kept.join('&')}`;
}

/**
 * `search` (an address's `?...`, with or without the `?`) without the parameters that are ours (`q`, `conv`, `origin`,
 * `tool`, `link`, `commit`, `file`), and with every other parameter exactly as it was. They are not read from the query
 * string (see `readSearchState`), so one found there was left by a link from before the filters moved to the fragment, and
 * the query string is the part of an address that is sent to a server. The result starts with `?`, or is empty when nothing
 * is left; an address that has none of our names is returned as it is, byte for byte.
 */
export function withoutSearchParams(search: string): string {
  const body = search.startsWith('?') ? search.slice(1) : search;
  const pairs = body.split('&');
  if (!pairs.some((pair) => OWN.has(nameOf(pair)))) return search;
  const kept = pairs.filter((pair) => pair !== '' && !OWN.has(nameOf(pair)));
  return kept.length === 0 ? '' : `?${kept.join('&')}`;
}

/** The parts of an address a link is made from. */
export interface AddressParts {
  readonly origin: string;
  readonly pathname: string;
  readonly search: string;
  readonly hash: string;
}

/**
 * The link to the view `state` is: the address as it is, with the filters and, if `withText`, the search text in
 * the fragment (`#q=...&conv=...`), where a server never sees them. The parameters of the query string and of the
 * fragment that are not ours stay, as they were; ours are taken out of the query string (see `withoutSearchParams`),
 * so a link never sends to a server a search or a filter that an older link left there.
 */
export function viewLink(address: AddressParts, state: SearchState, openFile: string | null, withText: boolean): string {
  const encoded = encodeSearchState(withText ? state : { q: '', filters: state.filters }, openFile);
  return `${address.origin}${address.pathname}${withoutSearchParams(address.search)}${withFragmentParams(address.hash, encoded)}`;
}
