/**
 * Independent QA of the address codec for issue #118 (src/lib/timelineSearchUrl.ts): what is read from an address
 * written by anyone, what is written, and how the rest of the address is left alone. Pure functions; the code that
 * reads and writes `window.location` is exercised in ShadowTimeline.qa118.acceptance.test.tsx.
 */
import { describe, expect, it } from 'vitest';
import { stripHidden } from './hiddenChars';
import { NO_FILTERS, type Filters } from './timelineSearch';
import {
  EMPTY_SEARCH,
  encodeSearchState,
  MAX_QUERY_LENGTH,
  readSearchState,
  SEARCH_PARAMS,
  viewLink,
  withFragmentParams,
  withoutSearchParams,
  type AddressParts,
  type SearchState,
} from './timelineSearchUrl';

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

const read = (search: string) => readSearchState(search);
const state = (q: string, filters: Partial<Filters> = {}): SearchState => ({ q, filters: { ...NO_FILTERS, ...filters } });

describe('an address with nothing usable in it', () => {
  it.each(['', '#', '#x=1', '#q=', '#q', '#origin=nope', '#link=Generated', '#commit=zzzzzzz', '#commit=abc', '#conv=has space', '#conv=', '#tool=', '#file=', '#q=&conv=&origin=&tool=&link=&commit=&file=', '#%71%',  '#&&&', '#=q', '#=&='])(
    'reads %j as the one empty state',
    (search) => {
      // The very object: nothing downstream sees a change for an address that says nothing.
      expect(read(search)).toBe(EMPTY_SEARCH);
    },
  );
});

describe('each parameter, and what it accepts', () => {
  it('q: any text, the first one if there are several, cut at 500 characters', () => {
    expect(read('#q=retry+limit').q).toBe('retry limit');
    expect(read('#q=a%20b%2Bc%26d%3De').q).toBe('a b+c&d=e');
    expect(read('#q=first&q=second').q).toBe('first');
    expect(read(`#q=${'x'.repeat(5_000)}`).q).toHaveLength(MAX_QUERY_LENGTH);
    expect(MAX_QUERY_LENGTH).toBe(500);
    expect(read('#q=%E6%97%A5%E6%9C%AC%E8%AA%9E%F0%9F%98%80').q).toBe('日本語😀');
  });

  it('q: hidden characters are left out, and tabs and line breaks become spaces', () => {
    expect(read('#q=pa%E2%80%8Bss%E2%80%AEword').q).toBe('password');
    expect(read('#q=a%0Ab%0D%0Ac%09d%0De').q).toBe('a b c d e');
    expect(read('#q=%E2%80%8B').q).toBe('');
    expect(read('#q=%E2%80%8B')).toBe(EMPTY_SEARCH);
  });

  it('q: a bare percent sign or a half-written escape is read as the browser reads it, without failing', () => {
    expect(() => read('#q=%')).not.toThrow();
    expect(() => read('#q=%zz')).not.toThrow();
    expect(read('#q=100%25').q).toBe('100%');
    expect(read('#q=%E0%A4%A').q).toContain('\ufffd');
  });

  it('conv: ASCII letters, digits and . _ - : up to 256 of them, once each, in the order given', () => {
    expect(read('#conv=a&conv=b:agent-c0ffee&conv=a').filters.conv).toEqual(['a', 'b:agent-c0ffee']);
    expect(read('#conv=5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70').filters.conv).toEqual(['5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70']);
    expect(read(`#conv=${'a'.repeat(256)}`).filters.conv).toHaveLength(1);
    expect(read(`#conv=${'a'.repeat(257)}`)).toBe(EMPTY_SEARCH);
    for (const bad of ['a b', 'a%2Fb', 'a%00b', '%E6%97%A5', 'a;b', 'a,b', '"a"', 'a%0Ab', '%3Cscript%3E', 'a%5Cb']) expect(read(`#conv=${bad}`), bad).toBe(EMPTY_SEARCH);
    // A bad one does not take the good ones with it.
    expect(read('#conv=bad%20one&conv=good').filters.conv).toEqual(['good']);
  });

  it('origin and link: exactly the words, in lower case, once each', () => {
    expect(read('#origin=prompt&origin=tool-results&origin=other&origin=prompt').filters.origin).toEqual(['prompt', 'tool-results', 'other']);
    expect(read('#link=generated&link=temporal&link=verified&link=publisher-asserted').filters.link).toEqual(['generated', 'temporal', 'verified', 'publisher-asserted']);
    for (const bad of ['Prompt', 'PROMPT', 'prompt%20', 'tool_results', 'human', '']) expect(read(`#origin=${bad}`), bad).toBe(EMPTY_SEARCH);
    for (const bad of ['Generated', 'rewrite_exact', 'squash_exact', 'publisher_asserted', '']) expect(read(`#link=${bad}`), bad).toBe(EMPTY_SEARCH);
  });

  it('tool: any name up to 200 characters, cleaned, once each', () => {
    expect(read('#tool=Bash&tool=github+%C2%B7+create_issue&tool=web_search+(server-side)').filters.tool).toEqual(['Bash', 'github · create_issue', 'web_search (server-side)']);
    expect(read(`#tool=${'t'.repeat(200)}`).filters.tool).toHaveLength(1);
    expect(read(`#tool=${'t'.repeat(201)}`)).toBe(EMPTY_SEARCH);
    expect(read('#tool=Ba%E2%80%8Bsh').filters.tool).toEqual(['Bash']);
    expect(read('#tool=%E2%80%8B')).toBe(EMPTY_SEARCH);
  });

  it('commit: seven to sixty-four hexadecimal digits, lower-cased, once each', () => {
    expect(read('#commit=ABCDEF1').filters.commit).toEqual(['abcdef1']);
    expect(read('#commit=ABCDEF1&commit=abcdef1&commit=abcdef12').filters.commit).toEqual(['abcdef1', 'abcdef12']);
    expect(read(`#commit=${'a'.repeat(64)}`).filters.commit).toHaveLength(1);
    for (const bad of ['abcdef', `${'a'.repeat(65)}`, 'abcdefg', 'abcdef%20', '0x1234567', 'abc%00def', '']) expect(read(`#commit=${bad}`), bad).toBe(EMPTY_SEARCH);
  });

  it('file: any path up to 1024 characters, as text', () => {
    expect(read('#file=src%2Fa%20b.ts').filters.file).toEqual({ mode: 'path', path: 'src/a b.ts' });
    expect(read(`#file=${'p'.repeat(5_000)}`).filters.file).toEqual({ mode: 'path', path: 'p'.repeat(1024) });
    expect(read('#file=a&file=b').filters.file).toEqual({ mode: 'path', path: 'a' });
    expect(read('#file=%E2%80%8B')).toBe(EMPTY_SEARCH);
  });

  it('keeps at most 50 values of a kind', () => {
    const many = Array.from({ length: 80 }, (_, k) => `conv=c${k}`).join('&');
    expect(read(`#${many}`).filters.conv).toEqual(Array.from({ length: 50 }, (_, k) => `c${k}`));
    const tools = Array.from({ length: 80 }, (_, k) => `tool=t${k}`).join('&');
    expect(read(`#${tools}`).filters.tool).toHaveLength(50);
  });
});

describe('hostile addresses', () => {
  const HOSTILE = [
    '#q=<script>alert(1)</script>',
    '#q="><img src=x onerror=alert(1)>',
    "#q='; DROP TABLE steps; --",
    // The query string is not read, whatever it holds: a `?` at the start of a fragment is a character of the first name.
    '#?q=secret&?tool=Bash',
    '#q=javascript:alert(1)',
    '#q=data:text/html,<script>alert(1)</script>',
    '#q=${process.env.SECRET}&q=%7B%7Bconstructor.constructor(%22alert(1)%22)()%7D%7D',
    '#q=%00%00%00',
    '#q=%E2%80%AEgnp.exe',
    '#q=__proto__&conv=__proto__&tool=__proto__&file=__proto__',
    '#q=constructor&tool=constructor&tool=toString&tool=hasOwnProperty',
    '#file=javascript:alert(1)&file=data:text/html,x',
    '#file=../../../../etc/passwd',
    '#file=C:\\Windows\\System32',
    '#tool=<b>bold</b>&tool="quoted"&tool=%26amp%3B',
    `#q=${'%F0%9F%98%80'.repeat(5_000)}`,
    `#q=${'%'.repeat(1_000)}`,
    `#${'q=a&'.repeat(10_000)}`,
    `#${'conv=a&'.repeat(10_000)}x=1`,
    `#${Array.from({ length: 20_000 }, (_, k) => `p${k}=v`).join('&')}`,
  ];

  it.each(HOSTILE.map((search) => [search.slice(0, 60), search] as const))('reads %j into plain values without failing', (_label, search) => {
    const started = performance.now();
    const got = read(search);
    expect(performance.now() - started).toBeLessThan(1_000);
    expect(got.q.length).toBeLessThanOrEqual(MAX_QUERY_LENGTH);
    expect(got.filters.conv.length).toBeLessThanOrEqual(50);
    for (const id of got.filters.conv) expect(id).toMatch(/^[A-Za-z0-9._:-]{1,256}$/);
    for (const key of got.filters.origin) expect(['prompt', 'tool-results', 'other']).toContain(key);
    for (const kind of got.filters.link) expect(['generated', 'temporal', 'verified', 'publisher-asserted']).toContain(kind);
    for (const hash of got.filters.commit) expect(hash).toMatch(/^[0-9a-f]{7,64}$/);
    for (const name of got.filters.tool) expect(name.length).toBeLessThanOrEqual(200);
    // Whatever was kept holds no hidden character and no line break.
    for (const text of [got.q, ...got.filters.tool, got.filters.file.mode === 'path' ? got.filters.file.path : '']) {
      expect(stripHidden(text, 'strict')).toBe(text);
      expect(text).not.toMatch(/[\n\r\t]/);
    }
    // And it can be written back without failing.
    expect(() => encodeSearchState(got, null)).not.toThrow();
  });

  it('does not read the query string, and a `?` that starts a fragment is part of a name and not a way into the query string', () => {
    expect(read('?q=secret')).toBe(EMPTY_SEARCH);
    expect(read('#?q=secret')).toBe(EMPTY_SEARCH);
    expect(read('#?q=secret&tool=Bash').q).toBe('');
    expect(read('#?q=secret&tool=Bash').filters.tool).toEqual(['Bash']);
    expect(read('#&?q=secret')).toBe(EMPTY_SEARCH);
    expect(read('#x=1?q=secret&q=real').q).toBe('real');
  });

  it('keeps hostile text as the text it is, exactly', () => {
    expect(read('#q=<script>alert(1)</script>').q).toBe('<script>alert(1)</script>');
    expect(read('#file=javascript:alert(1)').filters.file).toEqual({ mode: 'path', path: 'javascript:alert(1)' });
    expect(read('#tool=<b>x</b>').filters.tool).toEqual(['<b>x</b>']);
    expect(read('#q=__proto__').q).toBe('__proto__');
    expect(read('#conv=__proto__').filters.conv).toEqual(['__proto__']);
    // No prototype was touched by reading these.
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    expect(Object.keys(read('#q=a&conv=b')).sort()).toEqual(['filters', 'q']);
  });

  it('writes hostile text back percent-encoded, so that it can not break out of the address', () => {
    const written = encodeSearchState(state('<script>"\'&=#?', { tool: ['"><x>'], file: { mode: 'path', path: 'javascript:alert(1)#frag?x=1&y=2' } }), null);
    expect(written).not.toMatch(/[<>"'#\s]/);
    expect(written.split('&').every((pair) => /^[a-z]+=/.test(pair))).toBe(true);
    expect(read(`#${written}`).q).toBe('<script>"\'&=#?');
    expect(read(`#${written}`).filters.file).toEqual({ mode: 'path', path: 'javascript:alert(1)#frag?x=1&y=2' });
  });
});

describe('writing', () => {
  it('writes nothing for nothing, and the parameters in the order the module documents', () => {
    expect(encodeSearchState(EMPTY_SEARCH, null)).toBe('');
    expect(encodeSearchState(state(''), null)).toBe('');
    const all = state('retry limit', {
      conv: ['s1', 's2'],
      origin: ['prompt'],
      tool: ['Bash', 'Read'],
      link: ['temporal'],
      commit: ['abcdef1'],
      file: { mode: 'path', path: 'src/a.ts' },
    });
    expect(encodeSearchState(all, null)).toBe('q=retry+limit&conv=s1&conv=s2&origin=prompt&tool=Bash&tool=Read&link=temporal&commit=abcdef1&file=src%2Fa.ts');
    expect([...new URLSearchParams(encodeSearchState(all, null)).keys()].filter((key, at, keys) => keys.indexOf(key) === at)).toEqual(['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file']);
    expect(SEARCH_PARAMS).toEqual(['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file']);
  });

  it('writes the file a filter that follows the open one means, and nothing when none is open', () => {
    const follows = state('', { file: { mode: 'open' } });
    expect(encodeSearchState(follows, 'src/a.ts')).toBe('file=src%2Fa.ts');
    expect(encodeSearchState(follows, null)).toBe('');
    expect(encodeSearchState(follows, '')).toBe('');
    // A file named by a link stays that file, whatever is open.
    expect(encodeSearchState(state('', { file: { mode: 'path', path: 'a.ts' } }), 'b.ts')).toBe('file=a.ts');
  });

  it('leaves out what is longer than it reads, and never writes a value cut short', () => {
    // M3: a value the reader would cut or refuse is left out of the address whole; a cut value is a different value.
    expect(encodeSearchState(state('q'.repeat(500)), null)).toBe(`q=${'q'.repeat(500)}`);
    expect(encodeSearchState(state('q'.repeat(501)), null)).toBe('');
    expect(encodeSearchState(state('q'.repeat(900)), null)).toBe('');
    expect(new URLSearchParams(encodeSearchState(state('', { file: { mode: 'path', path: 'f'.repeat(1_024) } }), null)).get('file')).toHaveLength(1024);
    expect(encodeSearchState(state('', { file: { mode: 'path', path: 'f'.repeat(1_025) } }), null)).toBe('');
    expect(encodeSearchState(state('', { file: { mode: 'path', path: 'f'.repeat(3_000) } }), null)).toBe('');
    expect(encodeSearchState(state('', { file: { mode: 'open' } }), 'f'.repeat(3_000))).toBe('');
    expect(encodeSearchState(state('', { tool: ['t'.repeat(200), 't'.repeat(201)] }), null)).toBe(`tool=${'t'.repeat(200)}`);
    expect(encodeSearchState(state('', { conv: ['a'.repeat(256), 'a'.repeat(257)] }), null)).toBe(`conv=${'a'.repeat(256)}`);
    // The values around the one that is left out are written, in their order.
    expect(encodeSearchState(state('', { tool: ['Bash', 'x'.repeat(5_000), 'Read'] }), null)).toBe('tool=Bash&tool=Read');
  });
});

describe('what is written is what is read', () => {
  const ALPHABET = ['a', 'B', ' ', '&', '=', '+', '%', '#', '?', '/', '"', "'", '<', '>', '日', '本', '😀', 'é', '\\', ':', ';', ',', '.', '_', '-', '*', '[', ']', '(', ')', '~', '!', '@', '$', '^', '|', '{', '}'];
  const text = (rand: () => number, max: number) => {
    let out = '';
    for (let n = Math.floor(rand() * max); n > 0; n--) out += ALPHABET[Math.floor(rand() * ALPHABET.length)];
    return out;
  };
  const pick = <T,>(rand: () => number, list: readonly T[], max: number): T[] => {
    const out: T[] = [];
    for (let n = Math.floor(rand() * max); n > 0; n--) {
      const value = list[Math.floor(rand() * list.length)];
      if (!out.includes(value)) out.push(value);
    }
    return out;
  };

  it('for any state, up to the hidden characters and white space that reading cleans', () => {
    const rand = rng(2026);
    let withFilters = 0;
    for (let k = 0; k < 600; k++) {
      const filters: Filters = {
        conv: pick(rand, ['s1', 's1:agent-a0bd78e0', '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70', 'x.y_z-1'], 3),
        origin: pick(rand, ['prompt', 'tool-results', 'other'], 3),
        tool: pick(rand, ['Bash', 'github · create_issue', 'web_search (server-side)', '日本語', 'a&b=c', 'x y', '😀'], 4),
        link: pick(rand, ['generated', 'temporal', 'verified', 'publisher-asserted'], 3),
        commit: pick(rand, ['abcdef1', 'ABCDEF1'.toLowerCase(), '0123456789abcdef0123456789abcdef01234567'], 3),
        file: rand() < 0.4 ? { mode: 'path', path: text(rand, 40) || 'p' } : { mode: 'off' },
      };
      const q = text(rand, 60);
      const original = state(q, filters);
      const back = read(`#${encodeSearchState(original, null)}`);
      // What reading cleans: nothing here is hidden, and there is no line break in the alphabet.
      const expected = q === '' && Object.values(filters).every((value) => (Array.isArray(value) ? value.length === 0 : value.mode === 'off')) ? EMPTY_SEARCH : original;
      expect(back, JSON.stringify(original)).toEqual(expected);
      if (expected !== EMPTY_SEARCH && Object.values(filters).some((value) => (Array.isArray(value) ? value.length > 0 : value.mode !== 'off'))) withFilters++;
    }
    expect(withFilters).toBeGreaterThan(400);
  });

  it('with the parameters of the page around it, and in whatever place they are', () => {
    const rand = rng(77);
    for (let k = 0; k < 300; k++) {
      const foreign = Array.from({ length: Math.floor(rand() * 5) }, (_, n) => `${['tab', 'x[]', 'utm_source', 'a b', 'ünï'][n % 5]}=${encodeURIComponent(text(rand, 12))}`);
      const own = encodeSearchState(state(text(rand, 20) || 'x', { tool: ['Bash'] }), null);
      const search = `#${[...foreign.slice(0, 2), ...own.split('&'), ...foreign.slice(2)].join('&')}`;
      const rewritten = withFragmentParams(search, own);
      expect(read(rewritten)).toEqual(read(search));
      // The foreign parameters are in it, in order, byte for byte.
      expect(rewritten.slice(1).split('&').filter((pair) => !/^(q|conv|origin|tool|link|commit|file)=/.test(pair))).toEqual(foreign);
    }
  });
});

describe('rewriting the fragment of the page', () => {
  it('keeps every parameter that is not ours, in order, byte for byte, and puts ours last', () => {
    const search = '#tab=files&a=b%20c+d&x[]=1&x[]=2&empty&=weird&a=b=c&utm=%E2%9C%93&Q=upper&qq=2&q2=3&q+x=y&q%20=z';
    const out = withFragmentParams(search, 'q=new&tool=Bash');
    expect(out).toBe('#tab=files&a=b%20c+d&x[]=1&x[]=2&empty&=weird&a=b=c&utm=%E2%9C%93&Q=upper&qq=2&q2=3&q+x=y&q%20=z&q=new&tool=Bash');
  });

  it('takes ours out wherever they are, however they are spelled, and however many there are', () => {
    const search = '#q=a&tab=1&%71=b&conv=1&Conv=keep&or%69gin=prompt&tool=t&tool=u&link=generated&commit=abcdef1&file=f&q&file&z=9';
    expect(withFragmentParams(search, '')).toBe('#tab=1&Conv=keep&z=9');
    expect(withFragmentParams(search, 'q=x')).toBe('#tab=1&Conv=keep&z=9&q=x');
  });

  it('gives nothing back, not a lone #, when nothing is left', () => {
    expect(withFragmentParams('#q=a&tool=b', '')).toBe('');
    expect(withFragmentParams('', '')).toBe('');
    expect(withFragmentParams('#', '')).toBe('');
    expect(withFragmentParams('', 'q=x')).toBe('#q=x');
    expect(withFragmentParams('q=a', 'q=b')).toBe('#q=b');
  });

  it('is idempotent, and does not mind a fragment with or without its #', () => {
    const once = withFragmentParams('#a=1&q=old', 'q=new');
    expect(withFragmentParams(once, 'q=new')).toBe(once);
    expect(withFragmentParams('a=1&q=old', 'q=new')).toBe(once);
  });

  it('leaves a parameter with an escape it can not decode alone, and does not fail on it', () => {
    expect(withFragmentParams('#%zz=1&q=a&%E0%A4%A=2', 'q=b')).toBe('#%zz=1&%E0%A4%A=2&q=b');
  });

  it('drops empty pairs (a stray &&), and nothing else it keeps', () => {
    expect(withFragmentParams('#a=1&&b=2&', 'q=x')).toBe('#a=1&b=2&q=x');
  });

  it('takes time in proportion to the address: a hundred thousand parameters is a fraction of a second', () => {
    const big = `#${Array.from({ length: 100_000 }, (_, k) => (k % 7 === 0 ? `q=${k}` : `p${k}=v`)).join('&')}`;
    const started = performance.now();
    const out = withFragmentParams(big, 'q=z');
    expect(performance.now() - started).toBeLessThan(2_000);
    expect(out.endsWith('&q=z')).toBe(true);
    expect(out.split('&')).toHaveLength(100_000 - Math.ceil(100_000 / 7) + 1);
  });
});

describe('rewriting the query string of the page', () => {
  // A link from before the filters moved to the fragment may carry ours in the query string, which is the part of an address a
  // server reads. They are never read from there; they are taken out of it, and the rest of it is left as it was.
  it('leaves a query string that has none of our names exactly as it is, whatever else is in it', () => {
    for (const search of [
      '',
      '?',
      '??',
      '?tab=files',
      'tab=files',
      '?a=1&&b=2',
      '?a=1&',
      '?&a=1',
      '?x=%zz&y=%E0%A4%A&z=%',
      '?Q=upper&qq=2&q2=3&q+x=y&q%20=z&file2=1&tools=2&Conv=3&Origin=prompt&query=1&filter=2&links=3&commits=4',
      '?a[]=1&a[]=2&=x&=&empty&a=b=c',
      '?utm_source=%E2%9C%93&redirect=https%3A%2F%2Fexample.com%2F%3Fq%3Dsecret',
      '??q=1',
      '?a=1&?q=2',
    ]) {
      expect(withoutSearchParams(search), JSON.stringify(search)).toBe(search);
    }
  });

  it('takes ours out wherever they are, however they are spelled, and however many there are, and no others', () => {
    const search = '?q=a&tab=1&%71=b&conv=1&Conv=keep&or%69gin=prompt&tool=t&tool=u&link=generated&commit=abcdef1&file=f&q&file&z=9&to+ol=x&to%6Fl';
    expect(withoutSearchParams(search)).toBe('?tab=1&Conv=keep&z=9&to+ol=x');
    for (const name of SEARCH_PARAMS) {
      expect(withoutSearchParams(`?${name}=v&keep=1`), name).toBe('?keep=1');
      expect(withoutSearchParams(`?keep=1&${name}=v`), name).toBe('?keep=1');
      expect(withoutSearchParams(`?keep=1&${name}=v&keep=2`), name).toBe('?keep=1&keep=2');
      expect(withoutSearchParams(`?${name}&keep=1`), `${name} with no value`).toBe('?keep=1');
      expect(withoutSearchParams(`?${name}=`), `${name} with an empty value`).toBe('');
      expect(withoutSearchParams(`?${name.toUpperCase()}=v`), `${name.toUpperCase()} is another name`).toBe(`?${name.toUpperCase()}=v`);
    }
  });

  it('gives nothing back, not a lone ?, when nothing is left; and starts what it gives with a ?, as the address does', () => {
    expect(withoutSearchParams('?q=a&tool=b')).toBe('');
    expect(withoutSearchParams('q=a')).toBe('');
    expect(withoutSearchParams('?q')).toBe('');
    expect(withoutSearchParams('?&q=1&&')).toBe('');
    expect(withoutSearchParams('q=a&tab=1')).toBe('?tab=1');
    expect(withoutSearchParams('?q=a&tab=1')).toBe('?tab=1');
  });

  it('keeps the other parameters byte for byte, and drops only the empty pairs when it has taken something out', () => {
    expect(withoutSearchParams('?x=%E2%9C%93&q=1&y=a+b&z=%20&w=%zz&v=a%2Bb')).toBe('?x=%E2%9C%93&y=a+b&z=%20&w=%zz&v=a%2Bb');
    expect(withoutSearchParams('?a=1&&q=2&b=2&')).toBe('?a=1&b=2');
    expect(withoutSearchParams('?redirect=https%3A%2F%2Fexample.com%2F%3Fq%3Dsecret&q=1')).toBe('?redirect=https%3A%2F%2Fexample.com%2F%3Fq%3Dsecret');
  });

  it('is idempotent, and agrees with the names a browser reads, on any query string made of the parts addresses are made of', () => {
    const rand = rng(1801);
    const NAMES = ['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file', 'Q', 'qq', 'tab', 'x', '%71', 'or%69gin', 'to%6Fl', 'q+', '%zz', 'q%', '%', '', 'file%2', 'a[]', '?q', '?tab', 'q%00', 'com%6dit', 'c%6fnv', '%C3%A9', 'q%C3'];
    const VALUES = ['', '1', 'secret', 'a%20b', 'a+b', '%zz', '%E2%9C%93', 'https%3A%2F%2Fx', '=', 'a=b'];
    const own = new Set<string>(SEARCH_PARAMS);
    /** The name of a pair as a browser reads it (the way every query string is read on a server). */
    const nameOfPair = (pair: string) => [...new URLSearchParams(`&${pair}`).keys()][0] ?? '';
    let touched = 0;
    for (let k = 0; k < 4_000; k++) {
      const pairs = Array.from({ length: 1 + Math.floor(rand() * 6) }, () => {
        const name = NAMES[Math.floor(rand() * NAMES.length)];
        return rand() < 0.2 ? name : `${name}=${VALUES[Math.floor(rand() * VALUES.length)]}`;
      });
      if (rand() < 0.15) pairs.splice(Math.floor(rand() * pairs.length), 0, '');
      const search = (rand() < 0.85 ? '?' : '') + pairs.join('&');
      const body = search.startsWith('?') ? search.slice(1) : search;
      const all = body.split('&');
      const expected = all.some((pair) => own.has(nameOfPair(pair))) ? (all.filter((pair) => pair !== '' && !own.has(nameOfPair(pair))).length === 0 ? '' : `?${all.filter((pair) => pair !== '' && !own.has(nameOfPair(pair))).join('&')}`) : search;
      const got = withoutSearchParams(search);
      expect(got, JSON.stringify(search)).toBe(expected);
      expect(withoutSearchParams(got), `again: ${JSON.stringify(got)}`).toBe(got);
      if (got !== search) touched++;
      // And what is left says none of ours, to a server.
      for (const name of new URLSearchParams(got).keys()) expect(own.has(name), `${name} in ${JSON.stringify(got)}`).toBe(false);
    }
    expect(touched).toBeGreaterThan(1_500);
  });

  it('takes time in proportion to the address: a hundred thousand parameters is a fraction of a second', () => {
    const big = `?${Array.from({ length: 100_000 }, (_, k) => (k % 7 === 0 ? `q=${k}` : `p${k}=v`)).join('&')}`;
    const started = performance.now();
    const out = withoutSearchParams(big);
    expect(performance.now() - started).toBeLessThan(2_000);
    expect(out.split('&')).toHaveLength(100_000 - Math.ceil(100_000 / 7));
    expect(out.startsWith('?p1=v')).toBe(true);
  });
});

describe('the link to a view', () => {
  const address = (search: string, hash = ''): AddressParts => ({ origin: 'https://reviewer.example', pathname: '/pr/acme/shop/7', search, hash });
  const BASE = 'https://reviewer.example/pr/acme/shop/7';

  it('is the address with the text and the filters in the fragment, and the rest of the address as it was', () => {
    expect(viewLink(address(''), state('refund', { tool: ['Edit'] }), null, true)).toBe(`${BASE}#q=refund&tool=Edit`);
    expect(viewLink(address('?tab=files'), state('refund', { tool: ['Edit'] }), null, true)).toBe(`${BASE}?tab=files#q=refund&tool=Edit`);
    expect(viewLink(address('?tab=files&x=%E2%9C%93', '#section'), state('refund', { origin: ['prompt'] }), null, true)).toBe(`${BASE}?tab=files&x=%E2%9C%93#section&q=refund&origin=prompt`);
  });

  it('leaves the text out when it is asked to, and the filters stay', () => {
    expect(viewLink(address('?tab=files'), state('refund', { tool: ['Edit'] }), null, false)).toBe(`${BASE}?tab=files#tool=Edit`);
    expect(viewLink(address(''), state('refund'), null, false)).toBe(BASE);
    expect(viewLink(address('?tab=files', '#section'), state('refund'), null, false)).toBe(`${BASE}?tab=files#section`);
  });

  it('has none of ours in the query string, whatever the address had in it, and nothing else of it changed', () => {
    const link = viewLink(address('?q=old&tab=files&tool=Bash&%71=x&conv=1&file=f', '#q=older&section'), state('refund', { tool: ['Edit'] }), null, true);
    expect(link).toBe(`${BASE}?tab=files#section&q=refund&tool=Edit`);
    expect(new URL(link).search).toBe('?tab=files');
    // Nothing is left of the address but our own words, and a bare "?" does not stay behind.
    expect(viewLink(address('?q=old&tool=Bash'), state('refund', { tool: ['Edit'] }), null, true)).toBe(`${BASE}#q=refund&tool=Edit`);
    expect(viewLink(address('?q=old&tool=Bash'), state('', {}), null, true)).toBe(BASE);
    expect(viewLink(address('?q=old'), state('refund'), null, false)).toBe(BASE);
  });

  it('never has a search text or a filter anywhere a server reads, for any address and any view, and reads back as the view', () => {
    const rand = rng(404);
    const SEARCHES = ['', '?', '?tab=files', '?q=leak&tab=files', '?tool=Bash&q=leak', '?%71=leak', '?conv=c1&x=1&file=src%2Fa.ts', '?a=1&&q=2', '??q=1'];
    const HASHES = ['', '#', '#section', '#q=older&tool=Old', '#a=1&q=2&b=3'];
    const TEXTS = ['', 'refund', 'retry limit', 'pássword sécret', 'a&b=c#d?e', '日本語', 'x'.repeat(500), 'x'.repeat(501)];
    let checked = 0;
    for (let k = 0; k < 600; k++) {
      const text = TEXTS[Math.floor(rand() * TEXTS.length)];
      const view = state(text, { tool: rand() < 0.5 ? ['Edit', 'Bash'] : [], origin: rand() < 0.5 ? ['prompt'] : [], conv: rand() < 0.3 ? ['c1:agent-a'] : [] });
      const parts = address(SEARCHES[Math.floor(rand() * SEARCHES.length)], HASHES[Math.floor(rand() * HASHES.length)]);
      const withText = rand() < 0.6;
      const link = viewLink(parts, view, null, withText);
      const url = new URL(link);
      expect(link.startsWith(`${BASE}`)).toBe(true);
      for (const name of new URLSearchParams(url.search).keys()) expect(SEARCH_PARAMS as readonly string[], `${name} in ${link}`).not.toContain(name);
      expect(url.search.includes('leak'), link).toBe(false);
      // The query string's other parameters are in it, as they were.
      expect(url.search).toBe(withoutSearchParams(parts.search) === '?' ? '' : withoutSearchParams(parts.search));
      // The fragment reads back as the view it is the link to (the text only when it was put in, and only up to what the box takes).
      const read = readSearchState(url.hash);
      expect(read.filters.tool).toEqual(view.filters.tool);
      expect(read.filters.origin).toEqual(view.filters.origin);
      expect(read.filters.conv).toEqual(view.filters.conv);
      expect(read.q).toBe(withText && text.length <= MAX_QUERY_LENGTH ? text : '');
      checked++;
    }
    expect(checked).toBe(600);
  });
});
