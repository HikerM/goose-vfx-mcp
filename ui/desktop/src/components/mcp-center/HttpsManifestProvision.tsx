import { useEffect, useRef, useState } from 'react';
import type { McpHttpsManifestPreview, McpHttpsProvisionPlanReview } from '../../acp/mcp-platform';
import type { McpTaskRef } from '@hikerm/lumina-sdk';
import {
  confirmHttpsMcpManifest,
  createHttpsProvisionPlanReview,
  prepareHttpsMcpManifest,
  toMcpRecoveryViewModel,
} from '../../acp/mcp-platform';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '../ui/dialog';
import { DefinitionList, RecoveryPanel } from './McpCenterCommon';
import { PlanReviewDialog } from './PlanReviewDialog';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';
import { isHttpsManifestUrl, type HttpsProvisionState } from './httpsManifestProvisionState';

export function HttpsManifestProvision({ onTaskCreated }: { onTaskCreated: (task: McpTaskRef) => void }) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const [url, setUrl] = useState('');
  const [state, setState] = useState<HttpsProvisionState>('idle');
  const [preview, setPreview] = useState<McpHttpsManifestPreview | null>(null);
  const [plan, setPlan] = useState<McpHttpsProvisionPlanReview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const tokenRef = useRef<string | null>(null);
  const provisionIdRef = useRef<string | null>(null);
  const planIdempotencyKeyRef = useRef<string | null>(null);
  const sequenceRef = useRef(0);
  const entryRef = useRef<HTMLButtonElement>(null);
  const clear = () => { tokenRef.current = null; provisionIdRef.current = null; planIdempotencyKeyRef.current = null; };
  const close = () => { sequenceRef.current += 1; clear(); setOpen(false); setState('cancelled'); setPreview(null); setPlan(null); setError(null); };
  useEffect(() => () => { sequenceRef.current += 1; clear(); }, []);
  useEffect(() => { if (!open) entryRef.current?.focus(); }, [open]);
  const fail = (cause: unknown, seq: number) => { if (seq !== sequenceRef.current) return; clear(); setError(toMcpRecoveryViewModel(cause).message); setState('error'); };
  const prepare = async () => {
    const value = url;
    const seq = ++sequenceRef.current;
    setState('validating'); setError(null);
    if (!isHttpsManifestUrl(value)) { setError(intl.formatMessage(messages.httpsManifestInvalidUrl)); setState('error'); return; }
    setState('preparing');
    try {
      const result = await prepareHttpsMcpManifest(value);
      if (seq !== sequenceRef.current) return;
      tokenRef.current = result.confirmationToken; provisionIdRef.current = result.provisionId;
      setPreview(result.preview); setState('preview');
    } catch (cause) { fail(cause, seq); }
  };
  const confirm = async () => {
    if (!preview || !tokenRef.current || !provisionIdRef.current || state !== 'preview') return;
    const seq = ++sequenceRef.current;
    const provisionId = provisionIdRef.current;
    const confirmationToken = tokenRef.current;
    setState('confirming');
    try {
      const result = await confirmHttpsMcpManifest({ provisionId: provisionId as string, confirmationToken: confirmationToken as string, confirm: true });
      if (seq !== sequenceRef.current) return;
      const idempotencyKey = planIdempotencyKeyRef.current ?? (() => {
        const randomUUID = globalThis.crypto?.randomUUID;
        if (typeof randomUUID !== 'function') throw new Error('secure random source unavailable');
        const key = randomUUID.call(globalThis.crypto);
        if (!key) throw new Error('secure random source unavailable');
        planIdempotencyKeyRef.current = key;
        return key;
      })();
      const next = await createHttpsProvisionPlanReview({ provisionId: provisionId as string, expectedManifestDigest: result.manifestDigest, idempotencyKey });
      if (seq !== sequenceRef.current) return;
      tokenRef.current = null; provisionIdRef.current = null;
      setPlan(next); setState('plan-review');
    } catch (cause) { fail(cause, seq); }
  };
  return <>
    <Button ref={entryRef} variant="outline" onClick={() => { setOpen(true); setState('idle'); }}>{intl.formatMessage(messages.importHttpsManifest)}</Button>
    <Dialog open={open && state !== 'plan-review'} onOpenChange={(value) => { if (!value && state !== 'confirming' && state !== 'preparing') close(); }}>
      <DialogContent className="max-h-[min(760px,calc(100dvh-32px))] max-w-xl overflow-hidden">
        <div className="flex min-h-0 flex-col gap-5">
          <DialogHeader><DialogTitle>{intl.formatMessage(messages.importHttpsManifest)}</DialogTitle><DialogDescription>{intl.formatMessage(messages.httpsManifestDescription)}</DialogDescription></DialogHeader>
          <div className="min-h-0 space-y-4 overflow-y-auto pr-1">
            <label className="block"><span className="mb-1 block text-sm font-medium">{intl.formatMessage(messages.httpsManifestUrlLabel)}</span><Input value={url} onChange={(e) => setUrl(e.target.value)} aria-invalid={state === 'error'} aria-describedby="https-manifest-help" placeholder="https://…" disabled={state === 'preparing' || state === 'confirming'} /><span id="https-manifest-help" className="mt-1 block break-words text-xs text-text-secondary">{intl.formatMessage(messages.httpsManifestHelp)}</span></label>
            {error && <div><RecoveryPanel recovery={{ title: intl.formatMessage(messages.httpsManifestError), message: error, nextStep: intl.formatMessage(messages.retry), retryable: true }} onRetry={() => void prepare()} /></div>}
            {preview && <section role="status" aria-label={intl.formatMessage(messages.httpsManifestPreview)}><DefinitionList items={[[intl.formatMessage(messages.manifestId), preview.manifestId], [intl.formatMessage(messages.version), preview.version], [intl.formatMessage(messages.origin), preview.redactedOrigin], [intl.formatMessage(messages.warnings), preview.warnings.length ? preview.warnings.join(' ') : intl.formatMessage(messages.noneDeclared)]]} /></section>}
          </div>
          <DialogFooter className="sticky bottom-0 border-t border-border-primary bg-background-primary pt-4"><Button variant="outline" disabled={state === 'preparing' || state === 'confirming'} onClick={close}>{intl.formatMessage(messages.cancel)}</Button><Button disabled={state === 'preparing' || state === 'confirming' || state === 'validating'} onClick={() => void (state === 'preview' ? confirm() : prepare())}>{state === 'preview' ? intl.formatMessage(messages.continue) : intl.formatMessage(messages.preview)}</Button></DialogFooter>
        </div>
      </DialogContent>
    </Dialog>
    <PlanReviewDialog httpsPlan={plan} plan={null} operation="provision" onClose={() => { setPlan(null); close(); }} onTaskCreated={(task) => { clear(); setState('task-created'); onTaskCreated(task); }} />
  </>;
}
