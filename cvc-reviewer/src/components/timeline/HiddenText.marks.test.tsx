/**
 * What the search marks in a piece of text (issue #118), from the point of view of `HiddenText`, which every
 * piece of wire text is drawn through: `<mark>` elements that hold nothing but text, never more than a block's
 * worth, none across a hidden character, and none when nothing is being searched for.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup, render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { HiddenText } from './HiddenText';
import { HighlightContext } from './searchHighlight';
import { highlightTerms, parseQuery } from '../../lib/timelineSearch';
import { MAX_MARKS_PER_BLOCK } from '../../lib/searchMarks';

afterEach(cleanup);

const searching = (query: string, children: ReactNode) => (
  <HighlightContext.Provider value={highlightTerms(parseQuery(query))}>{children}</HighlightContext.Provider>
);
const marks = (container: HTMLElement) => Array.from(container.querySelectorAll('mark')).map((mark) => mark.textContent);

describe('HiddenText with a search', () => {
  it('draws exactly what it always drew when nothing is searched for', () => {
    const plain = render(<p><HiddenText text={'a needle\u200B here'} mode="strict" /></p>);
    const html = plain.container.innerHTML;
    plain.unmount();
    const withNull = render(<HighlightContext.Provider value={null}><p><HiddenText text={'a needle\u200B here'} mode="strict" /></p></HighlightContext.Provider>);
    expect(withNull.container.innerHTML).toBe(html);
    expect(html).not.toContain('<mark');
  });

  it('draws plain text as a string, with no wrapper, when the search finds nothing in it', () => {
    const { container } = render(<p>{searching('zzz', <HiddenText text="nothing to see" mode="prose" />)}</p>);
    expect(container.innerHTML).toBe('<p>nothing to see</p>');
  });

  it('marks every match, in the case it was written in, and takes nothing out of the text', () => {
    const { container } = render(<p>{searching('needle', <HiddenText text="A Needle, a needle, NEEDLE." mode="prose" />)}</p>);
    expect(marks(container)).toEqual(['Needle', 'needle', 'NEEDLE']);
    expect(container.textContent).toBe('A Needle, a needle, NEEDLE.');
    for (const mark of Array.from(container.querySelectorAll('mark'))) {
      expect(Array.from(mark.childNodes).every((node) => node.nodeType === Node.TEXT_NODE)).toBe(true);
    }
  });

  it('marks on both sides of a hidden character that is drawn as a token: the match is one, the marks are text on either side of it', () => {
    const text = `need${'\u200B'}le and a needle`;
    const { container } = render(<p>{searching('needle', <HiddenText text={text} mode="strict" />)}</p>);
    // Round 3: the first needle is split by the token and is found all the same; each half is marked, and so is the second needle.
    expect(marks(container)).toEqual(['need', 'le', 'needle']);
    expect(container.querySelector('[data-hidden-char="U+200B"]')).not.toBeNull();
    expect(container.textContent).toContain('⟨U+200B⟩');
    // The token is between the marks and in none of them: a mark holds text and nothing else.
    for (const mark of Array.from(container.querySelectorAll('mark'))) expect(mark.children).toHaveLength(0);
  });

  it('shares its allowance of marks across the whole text, hidden characters or not', () => {
    const text = `${'x '.repeat(80)}\u200B${' x'.repeat(80)}`;
    const { container } = render(<p>{searching('x', <HiddenText text={text} mode="strict" />)}</p>);
    expect(container.querySelectorAll('mark')).toHaveLength(MAX_MARKS_PER_BLOCK);
  });

  it('marks a term for a kind of text only where the text is of that kind', () => {
    const { container } = render(
      <p>
        {searching(
          'tool:bash path:src',
          <>
            <span id="plain"><HiddenText text="bash and src" mode="strict" /></span>
            <span id="tool"><HiddenText text="Bash" mode="strict" scope="tool" /></span>
            <span id="path"><HiddenText text="/repo/src/a.ts" mode="strict" scope="path" /></span>
            <span id="commit"><HiddenText text="bash src" mode="strict" scope="commit" /></span>
          </>,
        )}
      </p>,
    );
    expect(marks(container.querySelector('#plain') as HTMLElement)).toEqual([]);
    expect(marks(container.querySelector('#tool') as HTMLElement)).toEqual(['Bash']);
    expect(marks(container.querySelector('#path') as HTMLElement)).toEqual(['src']);
    expect(marks(container.querySelector('#commit') as HTMLElement)).toEqual([]);
  });

  it('marks a plain term on every kind of text', () => {
    const { container } = render(
      <p>
        {searching(
          'a1b2',
          <>
            <span id="tool"><HiddenText text="a1b2-tool" mode="strict" scope="tool" /></span>
            <span id="commit"><HiddenText text="a1b2c3d" mode="strict" scope="commit" /></span>
          </>,
        )}
      </p>,
    );
    expect(marks(container.querySelector('#tool') as HTMLElement)).toEqual(['a1b2']);
    expect(marks(container.querySelector('#commit') as HTMLElement)).toEqual(['a1b2']);
  });

  it('leaves out of the search the text it is told is not looked in', () => {
    const { container } = render(<p>{searching('claude', <HiddenText text="claude-opus" mode="strict" marks={false} />)}</p>);
    expect(container.innerHTML).toBe('<p>claude-opus</p>');
  });

  it('draws a value that is not text as it always did: React refuses it', () => {
    const { container } = render(<p>{searching('x', <HiddenText text={undefined as unknown as string} mode="strict" />)}</p>);
    expect(container.innerHTML).toBe('<p></p>');
  });

  it('marks text that is nothing but a hidden character and a match', () => {
    const { container } = render(<p>{searching('a', <HiddenText text={'\u200Ba'} mode="strict" />)}</p>);
    expect(marks(container)).toEqual(['a']);
  });
});
