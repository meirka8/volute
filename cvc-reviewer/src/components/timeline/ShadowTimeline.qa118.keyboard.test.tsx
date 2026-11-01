/**
 * (Revision round: the count says "N of M steps" while anything narrows the list, and there is a "Copy link to this view"
 * button between the arrows and the filters; the assertions that say so are the ones that changed.)
 *
 * Independent QA of issue #118: the keyboard and what assistive technology is told. The `/` shortcut and what it must
 * leave alone, Enter, Shift+Enter and Escape in the box, the live count, the names of the controls, where focus goes
 * when the control that had it goes away, no interactive element inside another, and the page's own shortcuts (Space,
 * j, k, x) and the selection of a card, which the search must not have changed.
 */
import '@testing-library/jest-dom/vitest';
import type { FormEvent } from 'react';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShadowTimeline } from './ShadowTimeline';
import { useKeyboardNavigation } from '../../hooks/useKeyboardNavigation';
import { installDomStubs, restoreDomStubs } from '../../test/domStubs';
import { generatedLink, makeNode, toolCall } from '../../test/fixtures';
import { searchFixture } from '../../test/searchFixtures';
import type { InteractionNode } from '../../types/cvc';

vi.mock('framer-motion', async () => (await import('../../test/motionStub')).framerMotionStub);

// Drawing cards in jsdom is slow on a slow core: none of these should fail by the clock.
vi.setConfig({ testTimeout: 30_000 });

const consoleProblems: string[] = [];

beforeEach(() => {
  consoleProblems.length = 0;
  // A warning from React (a key, an update during render, a state update outside act) is a defect of the feature.
  vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  vi.spyOn(console, 'warn').mockImplementation((...args: unknown[]) => void consoleProblems.push(args.map(String).join(' ').slice(0, 300)));
  installDomStubs();
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  const problems = [...consoleProblems];
  cleanup();
  document.body.replaceChildren();
  vi.useRealTimers();
  restoreDomStubs();
  window.history.replaceState(null, '', '/');
  expect(problems).toEqual([]);
});

const BASE = Date.parse('2026-03-02T09:15:00.000Z');
const harnessId = (n: number) => `1f0e5c3a-9d2b-8c4e-a1b7-${n.toString(16).padStart(12, '0')}`;
const step = (n: number, extra: Partial<InteractionNode> = {}) =>
  makeNode({ id: harnessId(n), author: 'human', timestamp: BASE + n * 1000, model_name: `Step ${n}`, user_prompt: `Step ${n}`, artifact_links: [generatedLink(harnessId(n))], ...extra });

/** Eight steps; 2, 4, 6 and 8 hold "needle"; 4 and 8 call Bash, 6 calls Read. */
const EIGHT = [
  step(1),
  step(2, { model_response: 'a needle here' }),
  step(3),
  step(4, { tool_executions: [toolCall('Bash', { command: 'grep needle x' })] }),
  step(5),
  step(6, { model_response: 'needle again', tool_executions: [toolCall('Read', { file_path: '/repo/src/a.ts' })] }),
  step(7),
  step(8, { model_response: 'the last needle', tool_executions: [toolCall('Bash', { command: 'ls' })] }),
];

const box = () => screen.getByRole('searchbox', { name: 'Search steps' }) as HTMLInputElement;
const typeQuery = (value: string) => fireEvent.change(box(), { target: { value } });
const press = (key: string, init: KeyboardEventInit = {}, target: Element = box()) => fireEvent.keyDown(target, { key, ...init });
const count = () => screen.getByRole('status').textContent;
const cards = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>('ol > li'));
const stepsOf = (container: HTMLElement) => cards(container).map((card) => Number(/Step (\d+)/.exec(card.textContent ?? '')?.[1]));
const currentStep = (container: HTMLElement) => cards(container).filter((card) => card.getAttribute('aria-current') === 'true').map((card) => Number(/Step (\d+)/.exec(card.textContent ?? '')?.[1]));
const openFilters = () => fireEvent.click(screen.getByRole('button', { name: /^Filters/ }));

// ---------------------------------------------------------------------------
// "/" comes to the box, unless it is a character
// ---------------------------------------------------------------------------

describe('the / shortcut', () => {
  const slash = (target: Element | Document = document.body, init: KeyboardEventInit = {}) => fireEvent.keyDown(target, { key: '/', ...init });

  /** Elements a person types into, which own the key. */
  const TYPED_INTO: Array<[string, () => HTMLElement]> = [
    ...['text', 'search', 'email', 'url', 'tel', 'password', 'number', 'date', 'time', 'datetime-local', 'month', 'week'].map(
      (type): [string, () => HTMLElement] => [`input type=${type}`, () => Object.assign(document.createElement('input'), { type })],
    ),
    ['input with no type', () => document.createElement('input')],
    ['textarea', () => document.createElement('textarea')],
    ['select', () => document.createElement('select')],
    ['contenteditable=""', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['contenteditable=true', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['contenteditable=plaintext-only', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['role=textbox', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['role=searchbox', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['role=combobox', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
    ['role=spinbutton', () => Object.assign(document.createElement('div'), { tabIndex: 0 }) as HTMLElement],
  ];
  const decorate = (name: string, element: HTMLElement): HTMLElement => {
    if (name.startsWith('contenteditable=')) element.setAttribute('contenteditable', name.slice('contenteditable='.length).replaceAll('"', ''));
    if (name.startsWith('role=')) element.setAttribute('role', name.slice(5));
    return element;
  };

  it.each(TYPED_INTO.map(([name]) => name))('is a character, and nothing else, typed into %s', (name) => {
    render(<ShadowTimeline interactions={EIGHT} />);
    const factory = TYPED_INTO.find(([label]) => label === name)![1];
    const element = decorate(name, factory());
    document.body.appendChild(element);
    element.focus();
    const notPrevented = slash(element);
    expect(notPrevented, 'the key press is left alone').toBe(true);
    expect(document.activeElement).not.toBe(box());
    // Also when the element is inside something that is not itself editable (a child of an editable region).
    if (name.startsWith('contenteditable')) {
      const child = document.createElement('span');
      element.appendChild(child);
      expect(slash(child)).toBe(true);
      expect(document.activeElement).not.toBe(box());
    }
  });

  /** Elements that take no text, which do not own it. */
  const NOT_TYPED_INTO: Array<[string, () => HTMLElement]> = [
    ...['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'range', 'color', 'file'].map(
      (type): [string, () => HTMLElement] => [`input type=${type}`, () => Object.assign(document.createElement('input'), { type })],
    ),
    ['button', () => document.createElement('button')],
    ['a link', () => Object.assign(document.createElement('a'), { href: '#x', textContent: 'link' })],
    ['a div', () => Object.assign(document.createElement('div'), { tabIndex: 0 })],
    ['a contenteditable=false region', () => Object.assign(document.createElement('div'), { tabIndex: 0 })],
  ];

  it.each(NOT_TYPED_INTO.map(([name]) => name))('comes to the box from %s', (name) => {
    render(<ShadowTimeline interactions={EIGHT} />);
    const element = NOT_TYPED_INTO.find(([label]) => label === name)![1]();
    if (name.includes('contenteditable=false')) element.setAttribute('contenteditable', 'false');
    document.body.appendChild(element);
    element.focus();
    expect(slash(element), 'the key press is taken').toBe(false);
    expect(document.activeElement).toBe(box());
  });

  it('comes to the box from the page, a card header, a mark, the Filters control and a check box of the filters', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    const header = container.querySelector<HTMLElement>('button[aria-pressed]')!;
    const targets: Element[] = [document.body, header, container.querySelector('mark')!, screen.getByRole('button', { name: /^Filters/ })];
    openFilters();
    targets.push(screen.getAllByRole('checkbox')[0]);
    for (const target of targets) {
      (document.activeElement as HTMLElement | null)?.blur();
      expect(slash(target)).toBe(false);
      expect(document.activeElement).toBe(box());
    }
  });

  it('selects what is in the box, so that the next character replaces it, and types nothing', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    slash();
    expect(box().value).toBe('needle');
    expect(box().selectionStart).toBe(0);
    expect(box().selectionEnd).toBe('needle'.length);
  });

  it('is left alone with ctrl, meta or alt, for a repeat, while composing, or once something else has taken it', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    for (const init of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { repeat: true }, { isComposing: true }]) {
      expect(slash(document.body, init), JSON.stringify(init)).toBe(true);
      expect(document.activeElement).not.toBe(box());
    }
    const taken = document.createElement('div');
    taken.addEventListener('keydown', (event) => event.preventDefault());
    document.body.appendChild(taken);
    slash(taken);
    expect(document.activeElement).not.toBe(box());
  });

  it('works with shift held, as on a keyboard where the slash is the shifted 7, and is not triggered by other keys', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    expect(slash(document.body, { shiftKey: true })).toBe(false);
    expect(document.activeElement).toBe(box());
    box().blur();
    for (const key of ['?', '\\', 'Divide', 's', 'Enter', ' ']) {
      fireEvent.keyDown(document.body, { key });
      expect(document.activeElement, key).not.toBe(box());
    }
  });

  it('is gone with the timeline: nothing listens once it is unmounted', () => {
    const first = render(<ShadowTimeline interactions={EIGHT} />);
    const focus = vi.spyOn(HTMLInputElement.prototype, 'focus');
    slash();
    expect(focus).toHaveBeenCalledTimes(1);
    first.unmount();
    focus.mockClear();
    slash();
    expect(focus).not.toHaveBeenCalled();
    focus.mockRestore();
  });

  it('does nothing while the steps are loading, when there is no box to come to', () => {
    render(<ShadowTimeline interactions={[]} isLoading />);
    expect(() => slash()).not.toThrow();
    expect(screen.queryByRole('searchbox')).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The box
// ---------------------------------------------------------------------------

describe('Enter, Shift+Enter and Escape in the box', () => {
  it('Enter goes to the next match and Shift+Enter to the previous, both ways round the list', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    // Listed newest first: 8, 6, 4, 2.
    expect(stepsOf(container)).toEqual([8, 6, 4, 2]);
    expect(count()).toBe('1 of 4 · 4 of 8 steps');
    expect(currentStep(container)).toEqual([8]);
    press('Enter');
    expect(count()).toBe('2 of 4 · 4 of 8 steps');
    expect(currentStep(container)).toEqual([6]);
    press('Enter');
    press('Enter');
    expect(count()).toBe('4 of 4 · 4 of 8 steps');
    press('Enter');
    expect(count()).toBe('1 of 4 · 4 of 8 steps');
    press('Enter', { shiftKey: true });
    expect(count()).toBe('4 of 4 · 4 of 8 steps');
    expect(currentStep(container)).toEqual([2]);
    press('Enter', { shiftKey: true });
    expect(count()).toBe('3 of 4 · 4 of 8 steps');
  });

  it('is never taken for a submit, and does nothing with nothing to go to', () => {
    const onSubmit = vi.fn((event: FormEvent) => event.preventDefault());
    const { container } = render(
      <form onSubmit={onSubmit}>
        <ShadowTimeline interactions={EIGHT} />
      </form>,
    );
    expect(press('Enter')).toBe(false); // prevented, with no query at all
    typeQuery('zzzz');
    expect(press('Enter')).toBe(false);
    expect(press('Enter', { shiftKey: true })).toBe(false);
    expect(count()).toBe('No matches · 0 of 8 steps');
    typeQuery('needle');
    expect(press('Enter')).toBe(false);
    expect(onSubmit).not.toHaveBeenCalled();
    expect(container.querySelector('form')).not.toBeNull();
  });

  it('goes to the only match and stays there when there is one', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('last needle');
    expect(count()).toBe('1 of 1 · 1 of 8 steps');
    press('Enter');
    press('Enter', { shiftKey: true });
    expect(count()).toBe('1 of 1 · 1 of 8 steps');
    expect(currentStep(container)).toEqual([8]);
    expect(screen.getByRole('button', { name: 'Next match' })).toBeDisabled();
  });

  it('moves the outline when the arrows are used, as it does for the keys', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    fireEvent.click(screen.getByRole('button', { name: 'Next match' }));
    expect(currentStep(container)).toEqual([6]);
    fireEvent.click(screen.getByRole('button', { name: 'Previous match' }));
    fireEvent.click(screen.getByRole('button', { name: 'Previous match' }));
    expect(currentStep(container)).toEqual([2]);
  });

  it('ignores Enter and Escape while a composition is going on', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    expect(press('Enter', { isComposing: true })).toBe(true);
    expect(count()).toBe('1 of 4 · 4 of 8 steps');
    expect(press('Escape', { isComposing: true })).toBe(true);
    expect(box().value).toBe('needle');
  });

  it('Escape clears the box and keeps focus in it; the next Escape leaves it; the filters stay', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /Bash/ }));
    typeQuery('needle');
    box().focus();
    press('Escape');
    expect(box().value).toBe('');
    expect(document.activeElement).toBe(box());
    expect(count()).toBe('2 of 8 steps');
    press('Escape');
    expect(document.activeElement).not.toBe(box());
    expect(screen.getByRole('button', { name: 'Remove filter Tool: Bash' })).toBeInTheDocument();
    expect(count()).toBe('2 of 8 steps');
  });

  it('typing j, k, x and a space is typing, and the keys of the page do not hear them', () => {
    const onNext = vi.fn();
    const onPrevious = vi.fn();
    const onExpand = vi.fn();
    const onMarkViewed = vi.fn();
    function Page() {
      useKeyboardNavigation({ onNext, onPrevious, onExpand, onMarkViewed });
      return <ShadowTimeline interactions={EIGHT} />;
    }
    render(<Page />);
    box().focus();
    for (const key of ['j', 'k', 'x', ' ', 'J', 'K', 'X']) press(key);
    expect(onNext).not.toHaveBeenCalled();
    expect(onPrevious).not.toHaveBeenCalled();
    expect(onExpand).not.toHaveBeenCalled();
    expect(onMarkViewed).not.toHaveBeenCalled();
    // ...and a key typed with a Filters check box focused is not a shortcut either.
    openFilters();
    const checkbox = screen.getAllByRole('checkbox')[0];
    checkbox.focus();
    for (const key of ['j', 'k', 'x', ' ']) fireEvent.keyDown(checkbox, { key });
    expect(onNext).not.toHaveBeenCalled();
    expect(onMarkViewed).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// The page's own shortcuts and the selection of a card are as they were
// ---------------------------------------------------------------------------

describe('the shortcuts of the page, and selecting a card', () => {
  function setUp(interactions: InteractionNode[] = EIGHT) {
    const onNext = vi.fn();
    const onPrevious = vi.fn();
    const onExpand = vi.fn();
    const onMarkViewed = vi.fn();
    const onSelect = vi.fn();
    function Page({ selected }: { selected: string | null }) {
      useKeyboardNavigation({ onNext, onPrevious, onExpand, onMarkViewed });
      return <ShadowTimeline interactions={interactions} selectedInteractionId={selected} onSelectInteraction={onSelect} />;
    }
    const rendered = render(<Page selected={null} />);
    return { ...rendered, onNext, onPrevious, onExpand, onMarkViewed, onSelect, Page };
  }

  it('j, k, x and Space work from the page, with a search on or off', () => {
    const { onNext, onPrevious, onExpand, onMarkViewed } = setUp();
    typeQuery('needle');
    for (const key of ['j', 'k', 'x', ' ']) fireEvent.keyDown(document.body, { key });
    expect(onNext).toHaveBeenCalledTimes(1);
    expect(onPrevious).toHaveBeenCalledTimes(1);
    expect(onExpand).toHaveBeenCalledTimes(1);
    expect(onMarkViewed).toHaveBeenCalledTimes(1);
  });

  it('Space on a card header, the arrows, the Filters control or a link is the control\'s, not "mark the file viewed"', () => {
    const { container, onMarkViewed } = setUp();
    typeQuery('needle');
    const header = container.querySelector<HTMLElement>('button[aria-pressed]')!;
    for (const target of [header, screen.getByRole('button', { name: 'Next match' }), screen.getByRole('button', { name: /^Filters/ }), screen.getByRole('button', { name: 'Clear search' })]) {
      target.focus();
      expect(fireEvent.keyDown(target, { key: ' ' })).toBe(true);
    }
    expect(onMarkViewed).not.toHaveBeenCalled();
  });

  it('a click on a header, on the card, or on a mark in it selects the step, while the list is narrowed as well', () => {
    const { container, onSelect } = setUp();
    typeQuery('needle');
    const [first, second, third] = cards(container);
    fireEvent.click(first.querySelector('button[aria-pressed]')!);
    fireEvent.click(second);
    fireEvent.click(third.querySelector('mark')!);
    expect(onSelect.mock.calls.map(([id]) => id)).toEqual([harnessId(8), harnessId(6), harnessId(4)]);
  });

  it('keeps the selection, and shows it, through a search and its clearing', () => {
    const { container, rerender, Page } = setUp();
    rerender(<Page selected={harnessId(4)} />);
    const pressed = () => cards(container).filter((card) => card.querySelector('button[aria-pressed="true"]') !== null).map((card) => Number(/Step (\d+)/.exec(card.textContent ?? '')?.[1]));
    expect(pressed()).toEqual([4]);
    typeQuery('needle');
    expect(pressed()).toEqual([4]);
    typeQuery('last needle'); // step 4 is out
    expect(pressed()).toEqual([]);
    typeQuery('');
    expect(pressed()).toEqual([4]);
  });

  it('does not select a step by moving the outline, or move the outline by selecting', () => {
    const { container, onSelect } = setUp();
    typeQuery('needle');
    press('Enter');
    press('Enter');
    expect(onSelect).not.toHaveBeenCalled();
    fireEvent.click(cards(container)[3].querySelector('button[aria-pressed]')!);
    expect(count()).toBe('3 of 4 · 4 of 8 steps');
    expect(currentStep(container)).toEqual([4]);
  });
});

// ---------------------------------------------------------------------------
// What a screen reader is told
// ---------------------------------------------------------------------------

describe('the live count', () => {
  it('is one polite, atomic status that stays the same element while its words change', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    const region = screen.getByRole('status');
    expect(region).toHaveAttribute('aria-live', 'polite');
    expect(region).toHaveAttribute('aria-atomic', 'true');
    expect(screen.getAllByRole('status')).toHaveLength(1);
    const said: string[] = [region.textContent ?? ''];
    const note = () => said.push(screen.getByRole('status').textContent ?? '');
    typeQuery('needle');
    note();
    press('Enter');
    note();
    typeQuery('zzzz');
    note();
    typeQuery('');
    note();
    openFilters();
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /Read/ }));
    note();
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getByRole('checkbox', { name: /Bash/ }));
    note();
    typeQuery('needle');
    note();
    expect(said).toEqual(['', '1 of 4 · 4 of 8 steps', '2 of 4 · 4 of 8 steps', 'No matches · 0 of 8 steps', '', '1 of 8 steps', '3 of 8 steps', '1 of 3 · 3 of 8 steps']);
    // The element a screen reader is watching was not replaced on the way.
    expect(screen.getByRole('status')).toBe(region);
  });

  it('is not repeated by the cards: the list does not carry a live region, and a card does not announce itself', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    expect(container.querySelectorAll('[aria-live]')).toHaveLength(1);
    expect(container.querySelectorAll('[role="alert"], [role="status"]')).toHaveLength(1);
  });
});

describe('names, labels and roles', () => {
  it('gives every control a name, and the lists their names', () => {
    const fixture = searchFixture(40);
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    openFilters();
    fireEvent.click(within(screen.getByRole('group', { name: 'Tool' })).getAllByRole('checkbox')[0]);
    fireEvent.click(within(screen.getByRole('group', { name: 'Stimulus origin' })).getAllByRole('checkbox')[0]);
    typeQuery('the');
    const bar = container.querySelector<HTMLElement>('input[type=search]')!.closest('div.border-b')!.parentElement!;
    const controls = Array.from(bar.querySelectorAll<HTMLElement>('button, input, select, textarea, summary, [role=button]'));
    expect(controls.length).toBeGreaterThan(12);
    for (const control of controls) expect(control, control.outerHTML.slice(0, 120)).toHaveAccessibleName();
    expect(screen.getByRole('searchbox', { name: 'Search steps' })).toHaveAttribute('aria-keyshortcuts', '/');
    expect(screen.getByRole('list', { name: 'Interactions linked to this PR' })).toBeInTheDocument();
    expect(screen.getByRole('list', { name: 'Active filters' })).toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'Filters' })).toBeInTheDocument();
    for (const legend of ['Conversation', 'Stimulus origin', 'Tool', 'Link type', 'Commit']) expect(screen.getByRole('group', { name: legend })).toBeInTheDocument();
    // A chip says what it removes, as a name a person can act on.
    for (const chip of screen.getAllByRole('button', { name: /^Remove filter/ })) expect(chip.getAttribute('aria-label')).toMatch(/^Remove filter [A-Za-z ]+: .+/);
  });

  it('puts the box first, then the controls in the order the eye finds them, and leaves disabled arrows out of the tab order', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    const order = () =>
      Array.from(document.querySelectorAll<HTMLElement>('input, button:not(:disabled)'))
        .filter((element) => !element.closest('ol'))
        .map((element) => element.getAttribute('aria-label') || element.textContent?.trim() || `input:${element.getAttribute('type')}`);
    expect(order()).toEqual(['input:search', 'Copy link to this view', 'Filters']);
    typeQuery('needle');
    expect(order()).toEqual(['input:search', 'Clear search', 'Previous match', 'Next match', 'Copy link to this view', 'Filters']);
  });

  it('says whether the filters are open, and what they control only while there is something to control', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    const toggle = screen.getByRole('button', { name: 'Filters' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).not.toHaveAttribute('aria-controls');
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    const panel = document.getElementById(toggle.getAttribute('aria-controls')!);
    expect(panel).not.toBeNull();
    expect(panel).toBe(screen.getByRole('group', { name: 'Filters' }));
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('group', { name: 'Filters' })).toBeNull();
  });

  it('counts the filters that are on in the name of the control that opens them', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    const tool = within(screen.getByRole('group', { name: 'Tool' }));
    fireEvent.click(tool.getByRole('checkbox', { name: /Bash/ }));
    expect(screen.getByRole('button', { name: 'Filters (1 active)' })).toBeInTheDocument();
    fireEvent.click(tool.getByRole('checkbox', { name: /Read/ }));
    expect(screen.getByRole('button', { name: 'Filters (2 active)' })).toBeInTheDocument();
  });

  it('keeps the check boxes of a filter reachable and named by what they filter, with how many steps each holds', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    const bash = screen.getByRole('checkbox', { name: /Bash/ });
    expect(bash).toHaveAccessibleName(/^Bash 2/);
    expect(bash).not.toBeChecked();
    fireEvent.click(bash);
    expect(bash).toBeChecked();
    // Unticking from the chip leaves the box unticked.
    fireEvent.click(screen.getByRole('button', { name: 'Remove filter Tool: Bash' }));
    expect(bash).not.toBeChecked();
  });
});

describe('where focus goes when the control that had it goes away', () => {
  it('to the Filters control, when a chip is removed, and when "Clear filters" is used', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    const tool = within(screen.getByRole('group', { name: 'Tool' }));
    fireEvent.click(tool.getByRole('checkbox', { name: /Bash/ }));
    fireEvent.click(tool.getByRole('checkbox', { name: /Read/ }));
    const chip = screen.getByRole('button', { name: 'Remove filter Tool: Bash' });
    chip.focus();
    fireEvent.click(chip);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /^Filters/ }));
    fireEvent.click(tool.getByRole('checkbox', { name: /Bash/ }));
    const clear = screen.getByRole('button', { name: 'Clear filters' });
    clear.focus();
    fireEvent.click(clear);
    expect(screen.queryByRole('button', { name: /^Remove filter/ })).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /^Filters/ }));
  });

  it('back to the box, when the clear button of the box is used', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    const clear = screen.getByRole('button', { name: 'Clear search' });
    clear.focus();
    fireEvent.click(clear);
    expect(document.activeElement).toBe(box());
    expect(screen.queryByRole('button', { name: 'Clear search' })).toBeNull();
  });

  it('stays on a check box that is ticked and unticked with the keyboard', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    const bash = screen.getByRole('checkbox', { name: /Bash/ });
    bash.focus();
    fireEvent.click(bash);
    expect(document.activeElement).toBe(bash);
    fireEvent.click(bash);
    expect(document.activeElement).toBe(bash);
  });
});

describe('no interactive element inside another', () => {
  const INTERACTIVE = 'a[href], button, input, select, textarea, summary, [role="button"], [role="checkbox"], [role="link"], [role="switch"], [tabindex]:not([tabindex="-1"])';

  it('holds in the search bar, its filters and chips, and in every card drawn with a search on', () => {
    const fixture = searchFixture(24);
    const { container } = render(<ShadowTimeline interactions={fixture.nodes} commits={fixture.commits} />);
    openFilters();
    for (const group of ['Conversation', 'Stimulus origin', 'Tool', 'Link type', 'Commit']) fireEvent.click(within(screen.getByRole('group', { name: group })).getAllByRole('checkbox')[0]);
    typeQuery('the');
    // Open every fold on the page, so that the controls inside the folds are drawn too.
    for (let pass = 0; pass < 3; pass++) {
      for (const toggle of Array.from(container.querySelectorAll<HTMLElement>('button[aria-expanded="false"]'))) if (!toggle.matches('[aria-controls][aria-expanded]:not(ol *)')) fireEvent.click(toggle);
    }
    const all = Array.from(container.querySelectorAll<HTMLElement>(INTERACTIVE));
    expect(all.length).toBeGreaterThan(30);
    const offenders = all.filter((element) => {
      const parent = element.parentElement?.closest(INTERACTIVE);
      return parent !== null && parent !== undefined && container.contains(parent);
    });
    expect(offenders.map((element) => `${element.tagName} in ${element.parentElement?.closest(INTERACTIVE)?.tagName}: ${element.outerHTML.slice(0, 80)}`)).toEqual([]);
    // A card is a list item named by its header; it is not itself a control.
    for (const card of cards(container)) {
      expect(card.getAttribute('role')).toBeNull();
      expect(card.getAttribute('tabindex')).toBeNull();
    }
  });

  it('holds for the labelled check boxes: a check box has its label around it and nothing else interactive in it', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    openFilters();
    for (const label of Array.from(document.querySelectorAll('fieldset label'))) {
      expect(label.querySelectorAll('input')).toHaveLength(1);
      expect(label.querySelectorAll('button, a[href], select, textarea')).toHaveLength(0);
    }
  });
});

// ---------------------------------------------------------------------------
// A few things that are easy to break while changing a timeline
// ---------------------------------------------------------------------------

describe('focus and the keyboard while the list changes under them', () => {
  it('does not lose focus in the box as the list narrows, widens and empties', () => {
    render(<ShadowTimeline interactions={EIGHT} />);
    box().focus();
    for (const query of ['n', 'ne', 'nee', 'needle', 'needlezz', 'needle', '', 'needle']) {
      typeQuery(query);
      expect(document.activeElement, query).toBe(box());
    }
  });

  it('keeps a card header focused through a search that keeps the card, and does not throw when it does not', () => {
    const { container } = render(<ShadowTimeline interactions={EIGHT} />);
    const header = () => cards(container).find((card) => card.textContent?.includes('Step 4'))!.querySelector<HTMLElement>('button[aria-pressed]')!;
    header().focus();
    expect(document.activeElement).toBe(header());
    typeQuery('needle');
    expect(document.activeElement).toBe(header());
    expect(() => typeQuery('last needle')).not.toThrow(); // the card is gone, and so is the focus
  });

  it('keeps working after the timeline is given a new list of steps while a search is on', () => {
    const { rerender, container } = render(<ShadowTimeline interactions={EIGHT} />);
    typeQuery('needle');
    press('Enter');
    rerender(<ShadowTimeline interactions={[...EIGHT, step(9, { model_response: 'another needle' })]} />);
    act(() => void 0);
    expect(count()).toMatch(/^\d of 5 · 5 of 9 steps$/);
    expect(stepsOf(container)).toEqual([9, 8, 6, 4, 2]);
    press('Enter', { shiftKey: true });
    expect(currentStep(container)).toHaveLength(1);
  });
});
