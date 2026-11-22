import { describe, expect, it } from 'vitest';
import { reasoningBoundary } from './reasoningMarker';
import { PROVIDER_SUMMARY_LABEL, REASONING_LABEL, WITHHELD_LABEL, reasoningDisplay } from './reasoningView';

const A = '6b1f0c0e-1111-4aaa-8bbb-000000000001';
const marked = (body: string) => `[cvc: provider summary of the model's reasoning ${reasoningBoundary(A, 'provider_summary', 0)}]\n\n${body}`;

describe('reasoningDisplay', () => {
  it("labels a verified summary as the provider's, with the marker stripped", () => {
    expect(reasoningDisplay(A, marked('Looking at the loop'))).toEqual({ kind: 'provider_summary', label: PROVIDER_SUMMARY_LABEL, text: 'Looking at the loop' });
    expect(PROVIDER_SUMMARY_LABEL).toBe("Provider summary of the model's reasoning");
  });

  it('keeps today\'s label for unmarked text, and shows a marker that does not verify literally', () => {
    expect(reasoningDisplay(A, 'thinking')).toEqual({ kind: 'exposed', label: REASONING_LABEL, text: 'thinking' });
    const forged = marked('x').replace(/~cvc-[0-9a-f]{16}/, '~cvc-0123456789abcdef');
    expect(reasoningDisplay(A, forged)).toEqual({ kind: 'exposed', label: 'Reasoning', text: forged });
  });

  it('labels the capture\'s withheld note "Withheld"', () => {
    expect(reasoningDisplay(A, '[cvc: withheld 7690 bytes that could not be scrubbed]')).toEqual({ kind: 'withheld', label: WITHHELD_LABEL, bytes: 7690n });
    expect(WITHHELD_LABEL).toBe('Withheld');
  });

  it('shows text that merely mentions the note, or a summary whose body reads like one, as text', () => {
    const mention = 'It means [cvc: withheld 5 bytes that could not be scrubbed] in a longer sentence.';
    expect(reasoningDisplay(A, mention).kind).toBe('exposed');
    const note = '[cvc: withheld 12 bytes that could not be scrubbed]';
    expect(reasoningDisplay(A, marked(note))).toMatchObject({ kind: 'provider_summary', text: note });
  });

  it('never calls reasoning full or complete', () => {
    for (const label of [PROVIDER_SUMMARY_LABEL, REASONING_LABEL, WITHHELD_LABEL]) expect(label).not.toMatch(/full|complete|real|actual/i);
  });
});
