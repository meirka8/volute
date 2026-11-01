/**
 * The search and filters as parameters of the fragment of the address (issue #118): what is written, what is
 * read back, what is refused, and what is left alone. The text of the search is never in the address of the page;
 * it is in a link that someone chose to copy.
 */
import { describe, expect, it } from 'vitest';
import { filterCount, NO_FILTERS, type Filters } from './timelineSearch';
import {
  EMPTY_SEARCH,
  encodeFilters,
  encodeSearchState,
  MAX_QUERY_LENGTH,
  readSearchState,
  SEARCH_PARAMS,
  viewLink,
  withFragmentParams,
  withoutSearchParams,
  type SearchState,
} from './timelineSearchUrl';

const SESSION = '5c1d8e2a-7f3b-4a96-b0c4-9e2d6f1a3b70';
const AGENT = `${SESSION}:agent-a0bd78e0`;
const SHA = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678';
const only = (partial: Partial<Filters>): Filters => ({ ...NO_FILTERS, ...partial });
const state = (q: string, filters: Partial<Filters> = {}): SearchState => ({ q, filters: only(filters) });
const roundTrip = (value: SearchState, openFile: string | null = null) => readSearchState(encodeSearchState(value, openFile));

describe('reading the address', () => {
  it('reads an address with none of our parameters as the empty search, always the same object', () => {
    expect(readSearchState('')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#other=1&x=2')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#tool=%00&link=nope&q=')).toBe(EMPTY_SEARCH);
  });

  it('reads every parameter', () => {
    const read = readSearchState(
      `#q=retry+limit&conv=${encodeURIComponent(AGENT)}&conv=${SESSION}&origin=prompt&origin=tool-results&tool=Bash&tool=github+%C2%B7+create_issue&link=temporal&commit=${SHA}&file=src%2Fnet%2Ffetch.ts`,
    );
    expect(read).toEqual({
      q: 'retry limit',
      filters: {
        conv: [AGENT, SESSION],
        origin: ['prompt', 'tool-results'],
        tool: ['Bash', 'github · create_issue'],
        link: ['temporal'],
        commit: [SHA],
        file: { mode: 'path', path: 'src/net/fetch.ts' },
      },
    });
  });

  it('works with or without the question mark, and ignores the parameters that are not ours', () => {
    expect(readSearchState('q=a&utm_source=x')).toEqual(state('a'));
    expect(readSearchState('#utm_source=x&q=a')).toEqual(state('a'));
  });

  it('silently leaves out a value that is not what its parameter holds', () => {
    const read = readSearchState(
      [
        'origin=prompt', 'origin=everything', 'origin=PROMPT',
        'link=generated', 'link=rewrite_exact', 'link=',
        'conv=ok-id_1.2:3', 'conv=has+space', 'conv=has%2Fslash', 'conv=', `conv=${'x'.repeat(257)}`,
        'commit=ABCDEF1', 'commit=abc12', 'commit=nothex1', `commit=${'a'.repeat(65)}`, `commit=${'a'.repeat(64)}`,
        'tool=', `tool=${'t'.repeat(201)}`, 'tool=Bash',
      ].join('&'),
    );
    expect(read.filters.origin).toEqual(['prompt']);
    expect(read.filters.link).toEqual(['generated']);
    expect(read.filters.conv).toEqual(['ok-id_1.2:3']);
    expect(read.filters.commit).toEqual(['abcdef1', 'a'.repeat(64)]);
    expect(read.filters.tool).toEqual(['Bash']);
  });

  it('keeps one of each value and at most fifty', () => {
    expect(readSearchState('origin=prompt&origin=prompt&tool=A&tool=A').filters).toMatchObject({ origin: ['prompt'], tool: ['A'] });
    const many = Array.from({ length: 80 }, (_, k) => `tool=t${k}`).join('&');
    expect(readSearchState(many).filters.tool).toHaveLength(50);
  });

  it('takes the first of a parameter that is given twice, where only one is meant', () => {
    expect(readSearchState('q=first&q=second').q).toBe('first');
    expect(readSearchState('file=a.ts&file=b.ts').filters.file).toEqual({ mode: 'path', path: 'a.ts' });
  });

  it('cuts the text and the path to a length, and leaves out hidden characters', () => {
    expect(readSearchState(`q=${'x'.repeat(2000)}`).q).toHaveLength(MAX_QUERY_LENGTH);
    expect(readSearchState(`q=pass%E2%80%8Bword%E2%80%AE`).q).toBe('password');
    expect(readSearchState('file=src%2F%E2%80%AEa.ts').filters.file).toEqual({ mode: 'path', path: 'src/a.ts' });
    expect(readSearchState('tool=Ba%E2%80%8Bsh').filters.tool).toEqual(['Bash']);
    expect(readSearchState('q=%E2%80%8B')).toBe(EMPTY_SEARCH);
  });

  it('turns a line break or a tab in the text into a space: a box holds one line', () => {
    expect(readSearchState('q=a%0Ab%09c%0D%0Ad').q).toBe('a b c d');
  });

  it('reads text as text: markup, script and a javascript address are only characters', () => {
    const hostile = '<img src=x onerror=alert(1)>';
    expect(readSearchState(`q=${encodeURIComponent(hostile)}&file=${encodeURIComponent('javascript:alert(1)')}`)).toEqual({
      q: hostile,
      filters: { ...NO_FILTERS, file: { mode: 'path', path: 'javascript:alert(1)' } },
    });
  });

  it('survives an address that is not valid', () => {
    expect(() => readSearchState('%E0%A4%A&q=%')).not.toThrow();
    expect(readSearchState('q=%E0%A4%A').q.length).toBeGreaterThan(0);
  });
});

describe('writing the address', () => {
  it('writes nothing for the empty search', () => {
    expect(encodeSearchState(EMPTY_SEARCH, null)).toBe('');
    expect(encodeSearchState(state(''), 'a.ts')).toBe('');
  });

  it('writes the parameters in a fixed order, one for each value', () => {
    const encoded = encodeSearchState(
      state('retry limit', { tool: ['Bash', 'Read'], conv: [AGENT], origin: ['prompt'], link: ['verified'], commit: [SHA], file: { mode: 'path', path: 'src/a b.ts' } }),
      null,
    );
    expect(encoded).toBe(`q=retry+limit&conv=${encodeURIComponent(AGENT)}&origin=prompt&tool=Bash&tool=Read&link=verified&commit=${SHA}&file=src%2Fa+b.ts`);
    expect(encoded.split('&').map((pair) => pair.split('=')[0]).filter((name, k, all) => all.indexOf(name) === k)).toEqual(['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file']);
  });

  it('writes the file the filter means when it follows the open one, and nothing when none is open', () => {
    expect(encodeSearchState(state('', { file: { mode: 'open' } }), 'src/open.ts')).toBe('file=src%2Fopen.ts');
    expect(encodeSearchState(state('', { file: { mode: 'open' } }), null)).toBe('');
    expect(encodeSearchState(state('x', { file: { mode: 'open' } }), '')).toBe('q=x');
  });

  it('reads back what it writes', () => {
    const states: SearchState[] = [
      state('retry limit'),
      state('path:src/net "a phrase" tool:Bash'),
      state('', { conv: [SESSION, AGENT] }),
      state('x', { origin: ['prompt', 'other'], link: ['publisher-asserted'], tool: ['github · create_issue', 'web_search (server-side)'] }),
      state('', { commit: [SHA, 'abcdef1'], file: { mode: 'path', path: 'docs/ünïcode & spaces.md' } }),
      state('100% + more = <b>"quoted"</b> #hash ?query &amp;'),
    ];
    for (const value of states) expect(roundTrip(value), JSON.stringify(value)).toEqual(value);
  });

  it('reads back the file of a filter that follows the open one as that file, named', () => {
    expect(roundTrip(state('a', { file: { mode: 'open' } }), 'src/open.ts')).toEqual(state('a', { file: { mode: 'path', path: 'src/open.ts' } }));
  });

  it('counts the same filters before and after', () => {
    const value = state('x', { conv: [AGENT], tool: ['Bash', 'Read'], file: { mode: 'path', path: 'a.ts' } });
    expect(filterCount(roundTrip(value).filters)).toBe(filterCount(value.filters));
  });
});

describe('putting our parameters into an address', () => {
  it('adds them to an address that has none, and takes them out of one that has them', () => {
    expect(withFragmentParams('', 'q=a')).toBe('#q=a');
    expect(withFragmentParams('#q=a', '')).toBe('');
    expect(withFragmentParams('#q=a&tool=Bash&file=x', '')).toBe('');
  });

  it('replaces ours and leaves every other parameter exactly as it was, in its place', () => {
    expect(withFragmentParams('#tab=files&q=old&tool=Bash&view=a%20b+c&empty=&flag', 'q=new')).toBe('#tab=files&view=a%20b+c&empty=&flag&q=new');
    expect(withFragmentParams('utm=1', 'q=a')).toBe('#utm=1&q=a');
  });

  it('knows its parameters by name however they are spelled, and none that merely begin the same', () => {
    expect(withFragmentParams('#%71=a&qq=b&q2=c&file%5B%5D=d', 'q=z')).toBe('#qq=b&q2=c&file%5B%5D=d&q=z');
    expect(SEARCH_PARAMS).toEqual(['q', 'conv', 'origin', 'tool', 'link', 'commit', 'file']);
  });

  it('is stable: putting the same parameters in twice changes nothing', () => {
    const once = withFragmentParams('#keep=1', 'q=a&tool=Bash');
    expect(withFragmentParams(once, 'q=a&tool=Bash')).toBe(once);
  });

  it('survives a name that does not decode', () => {
    expect(withFragmentParams('#%E0%A4%A=1&q=a', 'q=b')).toBe('#%E0%A4%A=1&q=b');
  });
});

describe('the filters are in the fragment, and the text of the search is never in the address of the page', () => {
  it('writes the filters with no search text: that is what the address of the page holds', () => {
    const filters = only({ tool: ['Bash'], conv: [AGENT], origin: ['prompt'], link: ['verified'], commit: [SHA], file: { mode: 'path', path: 'src/a.ts' } });
    expect(encodeFilters(filters, null)).toBe(`conv=${encodeURIComponent(AGENT)}&origin=prompt&tool=Bash&link=verified&commit=${SHA}&file=src%2Fa.ts`);
    expect(encodeFilters(NO_FILTERS, 'src/open.ts')).toBe('');
    expect(encodeFilters(only({ file: { mode: 'open' } }), 'src/open.ts')).toBe('file=src%2Fopen.ts');
    expect(encodeFilters(only({ file: { mode: 'open' } }), null)).toBe('');
  });

  it('does not read the query string, which this feature never released: a leading question mark is not the start of a fragment', () => {
    // The first name here is "?q", which is not ours; the parameters after it are read as they would be anywhere.
    expect(readSearchState('?q=a')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#?q=a')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#?q=a&tool=Bash')).toEqual(state('', { tool: ['Bash'] }));
    expect(readSearchState('?q=a&tool=Bash').q).toBe('');
  });

  it('reads a fragment with or without its hash, and ignores the parameters that are not ours', () => {
    expect(readSearchState('#tool=Bash&section=2')).toEqual(state('', { tool: ['Bash'] }));
    expect(readSearchState('tool=Bash')).toEqual(state('', { tool: ['Bash'] }));
    expect(readSearchState('#L10-L20')).toBe(EMPTY_SEARCH);
    expect(readSearchState('#')).toBe(EMPTY_SEARCH);
  });

  it('reads the text a link brought (q) like any parameter, cut to what the box takes', () => {
    expect(readSearchState('#q=retry+limit&tool=Bash')).toEqual(state('retry limit', { tool: ['Bash'] }));
    expect(readSearchState(`#q=${'x'.repeat(2_000)}`).q).toHaveLength(MAX_QUERY_LENGTH);
  });

  it('puts our parameters into a fragment and leaves the rest of it, and of the address, alone', () => {
    expect(withFragmentParams('', 'tool=Bash')).toBe('#tool=Bash');
    expect(withFragmentParams('#section-2', 'tool=Bash')).toBe('#section-2&tool=Bash');
    expect(withFragmentParams('#section-2&q=old&tool=Read', 'tool=Bash')).toBe('#section-2&tool=Bash');
    expect(withFragmentParams('#q=old', '')).toBe('');
    expect(withFragmentParams('#', '')).toBe('');
    expect(withFragmentParams('#keep=1&a%20b=c+d&x[]=1', 'file=a.ts')).toBe('#keep=1&a%20b=c+d&x[]=1&file=a.ts');
  });

  describe('the link to a view', () => {
    const address = { origin: 'https://reviewer.example', pathname: '/pr/acme/shop/7', search: '?tab=files', hash: '' };

    it('holds the filters and the text in the fragment, and the rest of the address as it is', () => {
      const link = viewLink(address, state('retry limit', { tool: ['Bash'], file: { mode: 'path', path: 'src/a.ts' } }), null, true);
      expect(link).toBe('https://reviewer.example/pr/acme/shop/7?tab=files#q=retry+limit&tool=Bash&file=src%2Fa.ts');
      expect(new URL(link).search).toBe('?tab=files');
      expect(readSearchState(new URL(link).hash)).toEqual(state('retry limit', { tool: ['Bash'], file: { mode: 'path', path: 'src/a.ts' } }));
    });

    it('leaves the text out when asked to', () => {
      const link = viewLink(address, state('password=hunter2hunter2', { tool: ['Bash'] }), null, false);
      expect(link).toBe('https://reviewer.example/pr/acme/shop/7?tab=files#tool=Bash');
      expect(link).not.toContain('hunter2');
    });

    it('means the open file by name, and keeps the parameters of the fragment that are not ours', () => {
      const link = viewLink({ ...address, search: '', hash: '#section-2&q=old' }, state('x', { file: { mode: 'open' } }), 'src/open.ts', true);
      expect(link).toBe('https://reviewer.example/pr/acme/shop/7#section-2&q=x&file=src%2Fopen.ts');
    });

    it('is the address itself when there is nothing to say', () => {
      expect(viewLink(address, EMPTY_SEARCH, null, true)).toBe('https://reviewer.example/pr/acme/shop/7?tab=files');
    });

    it('does not carry our names in the query string, which a server is sent, and keeps every other parameter as it was', () => {
      // A link from before the filters moved to the fragment left a `q` (and the rest) there.
      const old = { ...address, search: '?tab=files&q=password%3Dhunter2hunter2&conv=abc&origin=prompt&tool=Bash&link=temporal&commit=abcdef0&file=a.ts&utm=x%20y' };
      const link = viewLink(old, state('retry limit', { tool: ['Bash'] }), null, true);
      expect(link).toBe('https://reviewer.example/pr/acme/shop/7?tab=files&utm=x%20y#q=retry+limit&tool=Bash');
      expect(new URL(link).search).toBe('?tab=files&utm=x%20y');
      // Without the text, or without anything to say, the query string is as clean.
      expect(viewLink(old, state('retry limit'), null, false)).toBe('https://reviewer.example/pr/acme/shop/7?tab=files&utm=x%20y');
      expect(viewLink({ ...address, search: '?q=old' }, EMPTY_SEARCH, null, true)).toBe('https://reviewer.example/pr/acme/shop/7');
    });

    it('does not send a leftover search text to the server, whatever else the query string says, and however the name is spelled', () => {
      for (const search of ['?q=secret', '?a=1&q=secret', '?q=secret&a=1', '?%71=secret', '?q', '?a=1&q&b=2', '?q=secret&q=again', '?conv=abc&file=src%2Fa.ts']) {
        const link = viewLink({ ...address, search }, EMPTY_SEARCH, null, true);
        expect(new URL(link).search, search).not.toMatch(/(^|[?&])(q|%71|conv|file)([=&]|$)/);
      }
      expect(viewLink({ ...address, search: '?a=1&%71=secret&b=2' }, EMPTY_SEARCH, null, true)).toBe('https://reviewer.example/pr/acme/shop/7?a=1&b=2');
    });
  });

  describe('the query string of an address', () => {
    it('is returned as it is, byte for byte, when none of our names is in it', () => {
      for (const search of ['', '?', '??', '?tab=files', '?a=1&&b=2', '?a=1&', '?Q=upper&Conv=x', '?query=1&filetype=2&tooling=3', '?%E2%82%AC=1&x=%zz', '?q2=1&xq=2', '?tab=files&utm=x%20y+z']) {
        expect(withoutSearchParams(search), search).toBe(search);
      }
    });

    it('is the same without its question mark as a fragment is without its hash', () => {
      expect(withoutSearchParams('tab=files&q=x')).toBe('?tab=files');
      expect(withoutSearchParams('q=x')).toBe('');
    });

    it('loses each of our names, with every value it has, and keeps the others in their order', () => {
      expect(withoutSearchParams('?q=x')).toBe('');
      expect(withoutSearchParams('?q=x&conv=a&origin=prompt&tool=Bash&link=temporal&commit=abcdef0&file=a.ts')).toBe('');
      expect(withoutSearchParams('?a=1&q=x&b=2&conv=a&conv=b&c=3')).toBe('?a=1&b=2&c=3');
      expect(withoutSearchParams('?tool=Bash&tool=Read&tab=files')).toBe('?tab=files');
      expect(withoutSearchParams('?tab=files&utm=x%20y+z&q=1')).toBe('?tab=files&utm=x%20y+z');
    });

    it('knows our names however they are spelled in the address, and none that merely begin or end the same', () => {
      expect(withoutSearchParams('?%71=secret&a=1')).toBe('?a=1');
      expect(withoutSearchParams('?c%6Fnv=a&a=1')).toBe('?a=1');
      expect(withoutSearchParams('?fi%6Ce=a.ts&a=1')).toBe('?a=1');
      expect(withoutSearchParams('?q%5B%5D=1&a=1')).toBe('?q%5B%5D=1&a=1');
      expect(withoutSearchParams('?q2=1&a=1&xq=2&Q=3')).toBe('?q2=1&a=1&xq=2&Q=3');
    });

    it('takes a name with no value, and an empty one, out too, and leaves no stray separator', () => {
      expect(withoutSearchParams('?q&a=1')).toBe('?a=1');
      expect(withoutSearchParams('?a=1&q=&b=2')).toBe('?a=1&b=2');
      expect(withoutSearchParams('?q=1&&&a=1&')).toBe('?a=1');
    });

    it('survives a name that does not decode, which is not ours', () => {
      expect(withoutSearchParams('?%E0%A4%A=1&q=2')).toBe('?%E0%A4%A=1');
      expect(withoutSearchParams('?%zz=1&a=2')).toBe('?%zz=1&a=2');
    });

    it('leaves out exactly the names the codec owns', () => {
      for (const name of SEARCH_PARAMS) expect(withoutSearchParams(`?${name}=x&keep=1`), name).toBe('?keep=1');
    });

    it('takes time in proportion to its length', () => {
      const long = `?${'a=1&'.repeat(100_000)}q=x`;
      const started = performance.now();
      expect(withoutSearchParams(long)).toBe(`?${'a=1&'.repeat(100_000).slice(0, -1)}`);
      expect(performance.now() - started).toBeLessThan(1_000);
    });
  });

  it('leaves out of the address a value that reading would change or refuse, whole', () => {
    expect(encodeFilters(only({ conv: ['has space', 'ok-id'] }), null)).toBe('conv=ok-id');
    expect(encodeFilters(only({ tool: ['Ba\u200Bsh', 'Bash', 'tab\there'] }), null)).toBe('tool=Bash');
    expect(encodeFilters(only({ commit: ['abc12', 'ABCDEF1', 'abcdef1'] }), null)).toBe('commit=abcdef1');
    expect(encodeFilters(only({ file: { mode: 'path', path: 'a\nb' } }), null)).toBe('');
  });
});
