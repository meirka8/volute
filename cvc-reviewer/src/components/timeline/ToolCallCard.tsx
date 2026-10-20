import { useId, useMemo, useState, type ReactNode } from 'react';
import { CircleCheck, CircleDashed, CircleDot } from 'lucide-react';
import { clsx } from 'clsx';
import { usePaged } from '../../hooks/usePaged';
import {
  describeToolCall,
  toolOutcome,
  type ToolCallView,
  type ToolField,
  type ToolOutcome,
} from '../../lib/toolCalls';
import type { ToolExecution } from '../../types/cvc';
import { ClampedText } from './ClampedText';
import { CollapsibleCode } from './CollapsibleCode';
import { ExpandToggle } from './ExpandToggle';
import { HiddenText } from './HiddenText';
import { RENDER_LIMITS } from './limits';
import { ShowMore } from './ShowMore';
import { TextDiff } from './TextDiff';
import { BADGE_DANGER, BADGE_NEUTRAL, BADGE_SUCCESS, CHIP } from './styles';

const count = (value: number) => value.toLocaleString('en-US');

function Chips({ chips }: { chips: string[] }) {
  if (chips.length === 0) return null;
  return (
    <ul className="mt-2 flex flex-wrap gap-1.5">
      {chips.map((chip, index) => (
        <li key={index} className={clsx(CHIP, 'break-all')}>
          <HiddenText text={chip} mode="strict" />
        </li>
      ))}
    </ul>
  );
}

/** A file path or similar identifier. Long unbroken text must not widen the pane. */
function Path({ text }: { text: string }) {
  return (
    <div className="break-all font-mono text-xs text-muted">
      <HiddenText text={text} mode="strict" />
    </div>
  );
}

/** What the model said a call was for: a sentence, so prose. */
function Caption({ text }: { text: string }) {
  return (
    <p className="mb-2 break-words text-xs text-muted">
      <HiddenText text={text} mode="prose" />
    </p>
  );
}

/**
 * A note the Reviewer wrote, as opposed to text the capture recorded. Never in
 * brackets: bracketed text in this layout means "recorded, but not kept".
 */
function Muted({ children, className }: { children: ReactNode; className?: string }) {
  return <p className={clsx('text-xs italic text-muted', className)}>{children}</p>;
}

function FieldRow({ field }: { field: ToolField }) {
  if (field.unrenderable) {
    return (
      <div className="flex min-w-0 gap-2">
        <dt className="shrink-0 font-mono text-muted">
          <HiddenText text={field.key} mode="strict" />
        </dt>
        <dd className="min-w-0 italic text-muted">(too deeply nested to display)</dd>
      </div>
    );
  }
  if (field.block) {
    return (
      <div className="min-w-0">
        <dt className="break-all font-mono text-muted">
          <HiddenText text={field.key} mode="strict" />
        </dt>
        <dd className="mt-1">
          <CollapsibleCode text={field.value} wrap />
          {field.omitted ? <Muted className="mt-1">{`… ${count(field.omitted)} more characters not shown`}</Muted> : null}
        </dd>
      </div>
    );
  }
  return (
    <div className="flex min-w-0 gap-2">
      <dt className="shrink-0 font-mono text-muted">
        <HiddenText text={field.key} mode="strict" />
      </dt>
      <dd className="min-w-0 break-words font-mono text-ink">
        <HiddenText text={field.value} mode="strict" />
      </dd>
    </div>
  );
}

function FieldRows({ fields }: { fields: ToolField[] }) {
  const { visible, remaining, showMore } = usePaged(fields.length, RENDER_LIMITS.fieldRows);
  return (
    <>
      <dl className="space-y-1.5 text-xs">
        {fields.slice(0, visible).map((field, index) => (
          <FieldRow key={index} field={field} />
        ))}
      </dl>
      <ShowMore remaining={remaining} page={RENDER_LIMITS.fieldRows} one="field" many="fields" onShow={showMore} />
    </>
  );
}

function TodoStatus({ status }: { status: string }) {
  const icon =
    status === 'completed' ? (
      <CircleCheck size={14} className="text-success" aria-hidden="true" />
    ) : status === 'in_progress' ? (
      <CircleDot size={14} className="text-action" aria-hidden="true" />
    ) : (
      <CircleDashed size={14} className="text-muted" aria-hidden="true" />
    );
  return (
    <span className="mt-0.5 shrink-0">
      {icon}
      <span className="sr-only">
        <HiddenText text={`${status.replaceAll('_', ' ')}: `} mode="strict" />
      </span>
    </span>
  );
}

function TodoList({ todos }: { todos: Extract<ToolCallView, { kind: 'todos' }>['todos'] }) {
  const { visible, remaining, showMore } = usePaged(todos.length, RENDER_LIMITS.todos);
  return (
    <>
      <ul className="space-y-1.5">
        {todos.slice(0, visible).map((todo, index) => (
          <li key={index} className="flex min-w-0 items-start gap-2 text-xs text-ink">
            <TodoStatus status={todo.status} />
            <span className="min-w-0">
              <span className={clsx('break-words', todo.status === 'completed' && 'text-muted line-through')}>
                <HiddenText text={todo.content} mode="prose" />
              </span>
              {todo.activeForm !== undefined && (
                <span className="block break-words text-[11px] text-muted">
                  <HiddenText text={todo.activeForm} mode="prose" />
                </span>
              )}
            </span>
          </li>
        ))}
      </ul>
      <ShowMore remaining={remaining} page={RENDER_LIMITS.todos} one="item" many="items" onShow={showMore} />
    </>
  );
}

function EditList({ view }: { view: Extract<ToolCallView, { kind: 'edit' }> }) {
  const { visible, remaining, showMore } = usePaged(view.edits.length, RENDER_LIMITS.edits);
  return (
    <>
      <div className="mt-2 space-y-3">
        {view.edits.slice(0, visible).map((edit, index) => (
          <div key={index} className="min-w-0">
            {(view.edits.length > 1 || edit.chips.length > 0) && (
              <div className="mb-1 flex flex-wrap items-center gap-1.5 text-[11px] text-muted">
                {view.edits.length > 1 && <span>{`Edit ${index + 1} of ${view.edits.length}`}</span>}
                {edit.chips.map((chip) => (
                  <span key={chip} className={CHIP}>
                    <HiddenText text={chip} mode="strict" />
                  </span>
                ))}
              </div>
            )}
            <TextDiff oldText={edit.oldText} newText={edit.newText} />
          </div>
        ))}
      </div>
      <ShowMore remaining={remaining} page={RENDER_LIMITS.edits} one="edit" many="edits" onShow={showMore} />
    </>
  );
}

/** A sub-agent's prompt can be long and is rarely the point: hidden until asked for. */
function PromptDisclosure({ prompt }: { prompt: string }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <div className="min-w-0">
      {open && (
        <div id={id}>
          <CollapsibleCode text={prompt} wrap maxLines={12} mode="prose" />
        </div>
      )}
      <ExpandToggle
        expanded={open}
        onToggle={() => setOpen((value) => !value)}
        collapsedLabel="Show subagent prompt"
        expandedLabel="Hide subagent prompt"
        controls={open ? id : undefined}
      />
    </div>
  );
}

function ToolCallBodyView({ view }: { view: ToolCallView }) {
  switch (view.kind) {
    case 'command':
      return (
        <>
          {view.caption && <Caption text={view.caption} />}
          {view.command === '' ? <Muted>Empty command.</Muted> : <CollapsibleCode text={view.command} label="Command" />}
          <Chips chips={view.chips} />
        </>
      );

    case 'edit':
      return (
        <>
          <Path text={view.filePath} />
          <Chips chips={view.chips} />
          <EditList view={view} />
        </>
      );

    case 'file_content':
      return (
        <>
          <Path text={view.filePath} />
          <Chips chips={view.chips} />
          <div className="mt-2">
            {view.content === '' ? (
              <Muted>New content is empty.</Muted>
            ) : (
              <CollapsibleCode text={view.content} label="File content" />
            )}
          </div>
        </>
      );

    case 'lookup':
      return (
        <>
          <div className="flex min-w-0 items-baseline gap-2">
            <span className="shrink-0 text-[11px] uppercase tracking-[0.14em] text-muted">{view.label}</span>
            <span className="min-w-0 break-all font-mono text-xs text-ink">
              <HiddenText text={view.value} mode="strict" />
            </span>
          </div>
          {view.detail && (
            <ClampedText text={view.detail} lines={3} className="mt-2 text-xs leading-relaxed text-muted" />
          )}
          <Chips chips={view.chips} />
        </>
      );

    case 'delegation':
      return (
        <>
          {view.caption && <Caption text={view.caption} />}
          <Chips chips={view.chips} />
          <PromptDisclosure prompt={view.prompt} />
        </>
      );

    case 'todos':
      return view.todos.length === 0 ? <Muted>Empty list.</Muted> : <TodoList todos={view.todos} />;

    case 'fields':
      return view.fields.length === 0 ? <Muted>Called with no arguments.</Muted> : <FieldRows fields={view.fields} />;

    case 'truncated':
      return <Muted>{`Arguments not recorded: ${view.bytes} bytes exceeded the capture limit.`}</Muted>;

    case 'none':
      return <Muted>No arguments recorded.</Muted>;

    case 'raw':
      return (
        <>
          {view.unrenderable ? (
            <Muted>(too deeply nested to display)</Muted>
          ) : (
            <CollapsibleCode text={view.text} label="Arguments" />
          )}
          {view.omitted ? <Muted className="mt-1">{`… ${count(view.omitted)} more characters not shown`}</Muted> : null}
        </>
      );
  }
}

function OutcomeBadge({ outcome }: { outcome: ToolOutcome }) {
  switch (outcome) {
    case 'failure':
      // `failure` also covers calls interrupted before a result arrived.
      return (
        <span
          className={clsx(BADGE_DANGER, 'ml-auto shrink-0')}
          title="The tool reported an error, or no result was recorded (for example, the call was interrupted)."
        >
          failed
        </span>
      );
    case 'unrecorded':
      // See `toolOutcome`: cvc 0.7.0 records every server-side call as failed.
      return (
        <span
          className={clsx(BADGE_NEUTRAL, 'ml-auto shrink-0 text-right')}
          title={
            'Recorded as a server-side tool call (the name starts with "server:"), which, by that naming, the model ' +
            'provider runs rather than the local session. cvc 0.7.0 records every such call as "failure" because their results ' +
            'are not captured, so how it ended is unknown. It is not counted as a failed call.'
          }
        >
          server-side · result not recorded
        </span>
      );
    case 'success':
      return <span className={clsx(BADGE_SUCCESS, 'ml-auto shrink-0')}>success</span>;
    case 'unknown':
      return (
        <span
          className={clsx(BADGE_NEUTRAL, 'ml-auto shrink-0')}
          title={'The recorded status is neither "success" nor "failure", so how the call ended is unknown.'}
        >
          status not recorded
        </span>
      );
  }
}

/**
 * One recorded tool call: its name and outcome, then its input in the form a
 * reviewer reads it (a command, a diff, a file path), never as escaped JSON.
 */
export function ToolCallCard({ exec }: { exec: ToolExecution }) {
  const view = useMemo(() => describeToolCall(exec), [exec]);
  const outcome = toolOutcome(exec);

  return (
    <div className="min-w-0 rounded-2xl bg-canvas/75 px-3 py-2.5" data-status={outcome}>
      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="min-w-0 break-words font-mono text-xs font-semibold text-ink">
          <HiddenText text={view.title} mode="strict" />
        </span>
        <OutcomeBadge outcome={outcome} />
      </div>
      <ToolCallBodyView view={view} />
      {view.extras.length > 0 && (
        <div className="mt-2 border-t border-line/70 pt-2">
          <FieldRows fields={view.extras} />
        </div>
      )}
    </div>
  );
}
