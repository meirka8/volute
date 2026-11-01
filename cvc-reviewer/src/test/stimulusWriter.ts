/**
 * A writer of version 2 stimuli for tests: the layout of `cvc-core/src/stimulus.rs` (`render`), without
 * its scrubbing and size bounds. The parts, notes and marker spellings are the producer's; what a test
 * decides is what is in them, so a part can be cut, withheld or left out wherever it wants to be.
 *
 * It is the writer's rule that the boundary is the smallest candidate whose token occurs in no part's
 * content and in no tool name, which is what lets a reader trust a marker: tool output cannot have
 * contained a token for an id that did not exist yet.
 */
import { boundaryToken, type PartKind } from '../lib/stimulusV2';

export interface PartSpec {
  kind: PartKind;
  /** The tool's name in the marker of a tool result. Default `Bash`. */
  name?: string;
  /** The harness flagged the tool result as an error. */
  error?: boolean;
  /** The content, uncut. */
  text?: string;
  /** The part as the capture cut it: the head it kept, how many bytes it removed, and the tail it kept. */
  cut?: { head?: string; bytes: number | bigint | string; tail?: string };
  /** The part could not be scrubbed: the `withheld` note is all its content. */
  withheld?: boolean;
}

export interface WriteOptions {
  /** Close the stimulus with an `omitted` note for this many parts. */
  omitted?: number | bigint | string;
  /** Use this candidate even if its token occurs in a part. */
  candidate?: number;
}

export const SEPARATOR = '\n\n';

/** The part kinds in order, as the hash input spells them. */
export function shapeOf(parts: readonly Pick<PartSpec, 'kind'>[]): string {
  return parts.map((part) => part.kind).join(',');
}

/** The marker of a part, spelled as the producer spells it. */
export function markerOf(part: PartSpec, boundary: string): string {
  switch (part.kind) {
    case 'prompt':
      return `[prompt ${boundary}] `;
    case 'task':
      return `[task ${boundary}] `;
    case 'meta':
      return `[meta ${boundary}] `;
    case 'compaction_summary':
      return `[compaction summary ${boundary}]\n`;
    case 'tool_result':
      return `[tool_result ${part.name ?? 'Bash'} ${part.error ? 'error ' : ''}${boundary}] `;
  }
}

export function truncatedNote(bytes: number | bigint | string, boundary: string): string {
  return `…[cvc: truncated ${bytes} bytes ${boundary}]`;
}

export function withheldNote(boundary: string): string {
  return `[cvc: withheld ${boundary}]`;
}

export function omittedNote(parts: number | bigint | string, boundary: string): string {
  return `[cvc: omitted ${parts} parts ${boundary}]`;
}

/** The content of a part as the capture writes it. */
export function contentOf(part: PartSpec, boundary: string): string {
  if (part.withheld) return withheldNote(boundary);
  if (part.cut) {
    const { head = '', bytes, tail = '' } = part.cut;
    const note = truncatedNote(bytes, boundary);
    return [head === '' ? null : head, note, tail === '' ? null : tail].filter((piece) => piece !== null).join('\n');
  }
  return part.text ?? '';
}

/** What a writer checks a candidate's token against: everything the parts hold, and their tool names. */
function writtenText(part: PartSpec): string[] {
  return [part.text ?? '', part.cut?.head ?? '', part.cut?.tail ?? '', part.name ?? ''];
}

/** The text of the stimulus of interaction `id` made of `parts`. */
export function writeStimulus(id: string, parts: readonly PartSpec[], options: WriteOptions = {}): string {
  const shape = shapeOf(parts);
  const texts = parts.flatMap(writtenText);
  let candidate = options.candidate ?? 0;
  if (options.candidate === undefined) {
    for (; candidate < 64; candidate++) {
      const token = boundaryToken(id, shape, candidate);
      if (!texts.some((text) => text.includes(token))) break;
    }
    if (candidate === 64) throw new Error('no boundary candidate is free: the writer would write the undivided note');
  }
  const boundary = boundaryToken(id, shape, candidate);
  const written = parts.map((part) => markerOf(part, boundary) + contentOf(part, boundary));
  if (options.omitted !== undefined) written.push(omittedNote(options.omitted, boundary));
  return written.join(SEPARATOR);
}

/** The first boundary-shaped token of `text`. */
export function firstToken(text: string): string {
  const found = /~cvc-[0-9a-f]{16}/.exec(text);
  if (!found) throw new Error('the text holds no token');
  return found[0];
}
