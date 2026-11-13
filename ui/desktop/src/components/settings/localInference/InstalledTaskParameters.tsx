import { useEffect, useState } from 'react';
import type {
  NativeTaskDescriptionDto,
  NativeTaskIdentityDto,
  NativeTaskIntentDto,
} from '@hikerm/lumina-sdk';
import { describeInstalledTask, listInstalledTasks } from '../../../acp/local-inference';
import { acpChatSessionStore } from '../../../acp/chatSessionStore';
import { defineMessages, useIntl } from '../../../i18n';
import { getSessionDisplayName } from '../../../sessions';
import { Button } from '../../ui/button';

const messages = defineMessages({
  title: { id: 'installedTasks.title', defaultMessage: 'Installed model parameters' },
  session: { id: 'installedTasks.session', defaultMessage: 'Chat session' },
  intent: { id: 'installedTasks.intent', defaultMessage: 'Task' },
  model: { id: 'installedTasks.model', defaultMessage: 'Installed model and variant' },
  openChat: {
    id: 'installedTasks.openChat',
    defaultMessage: 'Open a chat to inspect the local models available to it.',
  },
  loading: { id: 'installedTasks.loading', defaultMessage: 'Loading model parameters…' },
  empty: { id: 'installedTasks.empty', defaultMessage: 'No installed model offers this task.' },
  missing: {
    id: 'installedTasks.missing',
    defaultMessage: 'This installed version is no longer available. Refresh the model list.',
  },
  refresh: { id: 'installedTasks.refresh', defaultMessage: 'Refresh model list' },
  required: { id: 'installedTasks.required', defaultMessage: 'Required' },
  defaultValue: { id: 'installedTasks.default', defaultMessage: 'Default: {value}' },
  min: { id: 'installedTasks.min', defaultMessage: 'Minimum: {value}' },
  max: { id: 'installedTasks.max', defaultMessage: 'Maximum: {value}' },
  step: { id: 'installedTasks.step', defaultMessage: 'Increment: {value}' },
  options: { id: 'installedTasks.options', defaultMessage: 'Choices: {values}' },
  language: { id: 'installedTasks.language', defaultMessage: 'Model language: {language}' },
  rate: { id: 'installedTasks.rate', defaultMessage: 'Output sample rate: {rate} Hz' },
  unsupported: { id: 'installedTasks.unsupported', defaultMessage: 'Unsupported parameters' },
  unknown: { id: 'installedTasks.unknown', defaultMessage: 'Support unverified' },
});

const intents = defineMessages({
  text_to_image: { id: 'installedTasks.textToImage', defaultMessage: 'Text to image' },
  text_to_video: { id: 'installedTasks.textToVideo', defaultMessage: 'Text to video' },
  text_to_video_with_audio: {
    id: 'installedTasks.textToVideoWithAudio',
    defaultMessage: 'Text to video with audio',
  },
  first_frame_to_video_with_audio: {
    id: 'installedTasks.frameToVideo',
    defaultMessage: 'First frame to video with audio',
  },
  speech_synthesis: { id: 'installedTasks.speech', defaultMessage: 'Speech synthesis' },
  transcription: { id: 'installedTasks.transcription', defaultMessage: 'Transcription' },
} satisfies Record<NativeTaskIntentDto, { id: string; defaultMessage: string }>);

const selectClass =
  'block w-full min-w-0 rounded border border-border-subtle bg-background-default px-2 py-1 text-sm text-text-default';
type Load<T> =
  | { status: 'loading' }
  | { status: 'error'; message: string }
  | { status: 'ready'; data: T };

function display(value: unknown): string {
  return typeof value === 'string' && value !== '' ? value : JSON.stringify(value);
}

function TaskFields({ task }: { task: NativeTaskDescriptionDto }) {
  const { formatMessage: t } = useIntl();
  const { descriptor } = task;
  const unsupported = descriptor.family === 'generation' ? {} : descriptor.parameters.unsupported;
  const rows = Object.entries(descriptor.parameters.fields).map(([name, field]) => {
    const details: string[] = [];
    if ('support' in field && field.support === 'unsupported') {
      unsupported[name] = field.constraints.join(' ');
      return null;
    }
    if ('support' in field && field.support === 'unknown') details.push(t(messages.unknown));
    if ('required' in field && field.required) details.push(t(messages.required));
    if (field.default != null)
      details.push(t(messages.defaultValue, { value: display(field.default) }));
    if ('minimum' in field && field.minimum != null)
      details.push(t(messages.min, { value: field.minimum }));
    if ('maximum' in field && field.maximum != null)
      details.push(t(messages.max, { value: field.maximum }));
    if ('step' in field && field.step != null)
      details.push(t(messages.step, { value: field.step }));
    const choices =
      descriptor.family === 'qwen3_asr' && name === 'language'
        ? descriptor.parameters.recognitionLanguages
        : 'enumValues' in field
          ? field.enumValues
          : [];
    if (choices.length)
      details.push(t(messages.options, { values: choices.map(display).join(', ') }));
    return (
      <div
        key={name}
        className="grid gap-1 border-t border-border-subtle py-2 sm:grid-cols-[minmax(8rem,1fr)_2fr]"
      >
        <dt className="break-all font-medium">{name}</dt>
        <dd className="min-w-0 break-words">
          {[field.valueType, ...details, ...field.constraints].join(' · ')}
        </dd>
      </div>
    );
  });
  return (
    <>
      {descriptor.family === 'pocket_tts' && (
        <p>
          {t(messages.language, { language: descriptor.parameters.modelLanguage })} ·{' '}
          {t(messages.rate, { rate: descriptor.parameters.outputSampleRate })}
        </p>
      )}
      <dl>{rows}</dl>
      {Object.keys(unsupported).length > 0 && (
        <details>
          <summary className="cursor-pointer">{t(messages.unsupported)}</summary>
          <dl>
            {Object.entries(unsupported).map(([name, reason]) => (
              <div key={name}>
                <dt className="font-medium">{name}</dt>
                <dd>{reason}</dd>
              </div>
            ))}
          </dl>
        </details>
      )}
    </>
  );
}

function ModelDetails({
  sessionId,
  identity,
}: {
  sessionId: string;
  identity: NativeTaskIdentityDto;
}) {
  const intl = useIntl();
  const [result, setResult] = useState<Load<NativeTaskDescriptionDto | null>>({
    status: 'loading',
  });
  useEffect(() => {
    let current = true;
    describeInstalledTask(sessionId, identity).then(
      (response) => {
        if (current)
          setResult({ status: 'ready', data: response.status === 'found' ? response.task : null });
      },
      (error) => {
        if (current) setResult({ status: 'error', message: String(error) });
      }
    );
    return () => {
      current = false;
    };
  }, [sessionId, identity]);
  if (result.status === 'error') return <p role="alert">{result.message}</p>;
  if (result.status === 'loading' || !result.data)
    return (
      <p role="status">
        {intl.formatMessage(result.status === 'loading' ? messages.loading : messages.missing)}
      </p>
    );
  return <TaskFields task={result.data} />;
}

function ModelList({ sessionId, intent }: { sessionId: string; intent: NativeTaskIntentDto }) {
  const intl = useIntl();
  const [result, setResult] = useState<Load<NativeTaskDescriptionDto[]>>({ status: 'loading' });
  const [selected, setSelected] = useState('');
  useEffect(() => {
    let current = true;
    listInstalledTasks(sessionId, intent).then(
      (data) => {
        if (current) setResult({ status: 'ready', data });
      },
      (error) => {
        if (current) setResult({ status: 'error', message: String(error) });
      }
    );
    return () => {
      current = false;
    };
  }, [sessionId, intent]);
  if (result.status === 'error') return <p role="alert">{result.message}</p>;
  if (result.status === 'loading' || !result.data.length)
    return (
      <p role="status">
        {intl.formatMessage(result.status === 'loading' ? messages.loading : messages.empty)}
      </p>
    );
  const task =
    result.data.find((row) => JSON.stringify(row.identity) === selected) ?? result.data[0];
  const key = JSON.stringify(task.identity);
  return (
    <>
      <label className="block">
        {intl.formatMessage(messages.model)}
        <select
          className={selectClass}
          value={key}
          onChange={(event) => setSelected(event.target.value)}
        >
          {result.data.map(({ identity }) => (
            <option key={JSON.stringify(identity)} value={JSON.stringify(identity)}>
              {identity.installationId} · {identity.taskVariant} · {identity.versionId}
            </option>
          ))}
        </select>
      </label>
      <ModelDetails key={key} sessionId={sessionId} identity={task.identity} />
    </>
  );
}

export function InstalledTaskParameters({ sessionIds }: { sessionIds: string[] }) {
  const intl = useIntl();
  const [selected, setSelected] = useState('');
  const [intent, setIntent] = useState<NativeTaskIntentDto>('text_to_image');
  const [revision, setRevision] = useState(0);
  const sessionId = sessionIds.includes(selected) ? selected : sessionIds[0];
  return (
    <section className="min-w-0 space-y-3 rounded-lg border border-border-subtle p-4 text-sm">
      <h3 className="font-semibold">{intl.formatMessage(messages.title)}</h3>
      {!sessionId ? (
        <p>{intl.formatMessage(messages.openChat)}</p>
      ) : (
        <>
          <div className="grid gap-2 sm:grid-cols-2">
            <label>
              {intl.formatMessage(messages.session)}
              <select
                className={selectClass}
                value={sessionId}
                onChange={(event) => setSelected(event.target.value)}
              >
                {sessionIds.map((id) => {
                  const session = acpChatSessionStore.getSnapshot(id)?.session;
                  return (
                    <option key={id} value={id}>
                      {session ? getSessionDisplayName(session) : id}
                    </option>
                  );
                })}
              </select>
            </label>
            <label>
              {intl.formatMessage(messages.intent)}
              <select
                className={selectClass}
                value={intent}
                onChange={(event) => setIntent(event.target.value as NativeTaskIntentDto)}
              >
                {Object.entries(intents).map(([tag, label]) => (
                  <option key={tag} value={tag}>
                    {intl.formatMessage(label)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setRevision((value) => value + 1)}
          >
            {intl.formatMessage(messages.refresh)}
          </Button>
          <ModelList
            key={JSON.stringify([sessionId, intent, revision])}
            sessionId={sessionId}
            intent={intent}
          />
        </>
      )}
    </section>
  );
}
