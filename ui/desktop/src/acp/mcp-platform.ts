import type {
  McpCatalogDetail,
  McpCatalogListRequest,
  McpCatalogPage,
  McpCatalogSummary,
  McpEventsPage,
  McpHealthStatus,
  McpInstallConfirmRequest,
  McpManagedDetail,
  McpManagedPage,
  McpManagedSummary,
  McpManualConnectionInput,
  McpManualStdioSourcesPage,
  McpPlanIntent,
  McpPlanReview,
  McpPlatformErrorCode,
  McpPlatformErrorEnvelope,
  McpPlatformOutcome,
  McpSourceRefreshResult,
  McpSourcesPolicyState,
  McpTaskRef,
  McpTrustTier,
} from '@aaif/goose-sdk';
import {
  createMcpTaskMonitorCursor,
  getMcpTaskMonitorResumeRequest,
  mergeMcpTaskMonitorPage,
  releaseMcpTaskMonitorCursor,
  resolvePublicTaskMutationTarget,
  resolvePublicTaskReference,
  revokePublicTaskReference,
  toMcpCenterSafeError,
  toPublicTask,
  toPublicTaskEvent,
  type McpTaskMonitorCursor,
  type McpTaskMonitorMergeResult,
} from '../components/mcp-center/adapters';
import type {
  McpCenterSafeError,
  PublicTask,
  PublicTaskEvent,
  PublicTaskMutationTarget,
  PublicTaskReference,
} from '../components/mcp-center/public-types';
import { getAcpClient } from './acpConnection';

export type {
  McpCenterSafeError,
  PublicTask,
  PublicTaskEvent,
  PublicTaskMutationTarget,
  PublicTaskOperation,
  PublicTaskReference,
  PublicTaskStatus,
} from '../components/mcp-center/public-types';
export type {
  McpTaskMonitorCursor,
  McpTaskMonitorMergeResult,
} from '../components/mcp-center/adapters';

export type McpProfilePage = {
  items: McpProfileSummary[];
};

export type McpCatalogRef = Pick<McpCatalogSummary, 'sourceId' | 'mcpId' | 'version'>;

type McpCatalogArtifact = Extract<
  McpCatalogDetail['distribution'],
  { artifacts: unknown[] }
>['artifacts'][number];

export type McpGovernedImportSource =
  | {
      type: 'local_manifest';
      filePath: string;
    }
  | {
      type: 'local_directory';
      directoryPath: string;
    };

export type McpGovernedImportKind =
  | 'local_persistence'
  | 'verified_source_catalog'
  | 'https_manifest_url'
  | 'enterprise_directory';

export type McpGovernedCatalogDocumentKind = 'manifest' | 'directory';

export type McpGovernedCatalogSourceSummary = {
  sourceId: string;
  importKind: McpGovernedImportKind;
  displayName: string;
};

export type McpGovernedCatalogDocumentSummary = {
  sourceId: string;
  documentId: string;
  documentDigest: string;
  documentKind: McpGovernedCatalogDocumentKind;
};

export type McpGovernedImportResult = {
  source: McpGovernedCatalogSourceSummary;
  document: McpGovernedCatalogDocumentSummary;
  entries: McpCatalogSummary[];
};

export type McpSourceProvisionRefreshTransport = 'verified_source_bundle_v1';

export type McpSourceProvisionTrustBasis = 'user_pin';

export type McpSourceProvisionPreview = {
  sourceId: string;
  displayName: string;
  rootDigest: string;
  documentDigest: string;
  endpointHost: string;
  refreshTransport: McpSourceProvisionRefreshTransport;
  manifestCount: number;
  warnings: string[];
  trustBasis: McpSourceProvisionTrustBasis;
};

export type McpSourceProvisionPrepareResult = {
  provisionId: string;
  confirmationToken: string;
  expiresAtMs: number;
  preview: McpSourceProvisionPreview;
};

export type McpSourceProvisionConfirmResult = {
  provisionId: string;
  confirmedAtMs: number;
  preview: McpSourceProvisionPreview;
};

export type McpHttpsManifestPreview = {
  manifestId: string;
  version: string;
  redactedOrigin: string;
  rawDigest: string;
  parsedDigest: string;
  redirectChainDigest: string;
  dnsEvidenceDigest: string;
  warnings: string[];
};

export type McpHttpsManifestPrepareResult = {
  provisionId: string;
  confirmationToken: string;
  expiresAtMs: number;
  preview: McpHttpsManifestPreview;
};

export type McpHttpsManifestConfirmResult = {
  manifestDigest: string;
  preview: McpHttpsManifestPreview;
};

export type McpHttpsProvisionPlanReview = {
  planId: string;
  planDigest: string;
  expiresAtMs: number;
  operation: 'provision';
  mcp: { mcpId: string; name: string; version: string };
  permissions: Array<{ kind: string; required: boolean }>;
  effects: {
    file: { writesFiles: boolean; removesFiles: boolean; ownedItems: number };
    process: { processRequiredForConnection: boolean; startsDuringConfirmation: boolean };
  };
  confirmation: { required: Array<'policy' | 'permission'>; defaultDisabled: boolean };
  policy: { outcome: string; reasonCount: number };
  warnings: string[];
};

export type McpHttpsProvisionPlanReviewRequest = {
  provisionId: string;
  expectedManifestDigest: string;
  idempotencyKey: string;
};

export type McpCredentialStatus =
  | 'unconfigured'
  | 're_registration_required'
  | 'trusted_state_conflict'
  | 'temporarily_unavailable'
  | 'ready';

export type McpProfileSummary = {
  profileId: string;
  name: string;
  description: string;
  revision: number;
  archived: boolean;
  entries: McpProfileEntry[];
  createdAtMs: number;
  updatedAtMs: number;
  credentialStatus?: McpCredentialStatus;
};

export type McpProfileEntry = {
  managedMcpId: string;
  ordinal: number;
};

export type McpProfileDetail = {
  profile: McpProfileSummary;
  history: McpProfileRevisionSummary[];
};

export type McpProfileRevisionSummary = {
  revision: number;
  operation: string;
  actor: string;
  createdAtMs: number;
};

export type McpProfileDraft = {
  locale: string;
  name: string;
  description: string;
  entries: McpProfileEntry[];
  candidates: McpProfileDraftCandidate[];
  unresolvedTerms: string[];
  lowConfidence: boolean;
  persisted: boolean;
};

export type McpProfileDraftCandidate = {
  managedMcpId: string;
  mcpId: string;
  name: string;
  description: string;
  confidence: number;
  reasonCode: string;
};

export type McpModelRecommendation = {
  candidates: McpModelRecommendationCandidate[];
  inventoryOnly: boolean;
  mutated: boolean;
};

export type McpModelRecommendationCandidate = {
  providerId: string;
  modelId: string;
  confidence: number;
  reasonCodes: string[];
  caveatCodes: string[];
};

export type McpProfileApplyPlan = {
  planId: string;
  profileId: string;
  profileRevision: number;
  mergePolicy: string;
  entries: McpProfileApplyEntry[];
  expiresAtMs: number;
  confirmation: McpProfileApplyConfirmation;
};

export type McpProfileApplyEntry = {
  managedMcpId: string;
  mcpId?: string;
  name?: string;
  version?: string;
  health: string;
  authReady: boolean;
  policyReady: boolean;
  readiness: string;
};

export type McpProfileApplyConfirmation = {
  confirmationToken: string;
};

export type McpProfileApplicationToken = {
  token: string;
  planId: string;
  profileId: string;
  profileRevision: number;
  expiresAtMs: number;
};

export type McpConnectionTestPhase =
  | 'eligibility'
  | 'mcp_connect_initialize'
  | 'tool_discovery'
  | 'cleanup'
  | 'model_request'
  | 'tool_visibility';

export type McpConnectionTestStatus = 'passed' | 'failed' | 'skipped';

export type McpConnectionTestCode =
  | 'eligible'
  | 'profile_not_ready'
  | 'policy_denied'
  | 'mcp_connect_failed'
  | 'mcp_timeout'
  | 'tool_discovery_failed'
  | 'no_tools_exposed'
  | 'provider_not_configured'
  | 'model_not_configured'
  | 'provider_initialization_failed'
  | 'model_request_failed'
  | 'runtime_activation_failed'
  | 'resource_limit_exceeded'
  | 'prerequisites_failed'
  | 'cleanup_failed'
  | 'tool_visibility_validated'
  | 'tool_visibility_skipped';

export type McpProfileConnectionTestResult = {
  passed: boolean;
  stages: Array<{
    phase: McpConnectionTestPhase;
    status: McpConnectionTestStatus;
    code: McpConnectionTestCode;
  }>;
};

export type McpPlatformRecoveryViewModel = {
  title: string;
  message: string;
  nextStep: string;
  retryable: boolean;
  correlationId?: string;
  code?: McpPlatformErrorCode;
  kind?: 'service' | 'connection' | 'monitor';
};

const genericMcpRequestFailedMessage = 'The MCP Platform request could not be completed.';
const maxMcpErrorMessageLength = 32 * 1024;
const maxStructuredKeyLength = 96;
const mcpMessageTruncatedNotice = '[message truncated]';
const sensitiveCredentialKeyPattern =
  /^(?:x-api-key|api-key|access-token|refresh-token|id-token|client-secret|token|secret|password|credential(?:s)?|cookie|set-cookie)$/i;
const structuredBoundaryChars = new Set(['{', '[', '(']);
const preservedStructuredBoundaryKeys = new Set(['code', 'status', 'nextstep', 'correlationid']);
const ignoredStructuredKeyCharPattern = /[\p{Cc}\p{Cf}\p{M}]/u;

type GraphemeSegment = {
  index: number;
  segment: string;
};

type GraphemeSegmenter = {
  segment(input: string): Iterable<GraphemeSegment>;
};

const intlWithSegmenter = Intl as typeof Intl & {
  Segmenter?: new (
    locales?: string | string[],
    options?: { granularity: 'grapheme' }
  ) => GraphemeSegmenter;
};

const graphemeSegmenter = intlWithSegmenter.Segmenter
  ? new intlWithSegmenter.Segmenter(undefined, { granularity: 'grapheme' })
  : null;

type QuoteKind = '"' | "'" | '`';

type QuotePair = {
  opener: string;
  closer: string;
  kind: QuoteKind;
  symmetric: boolean;
};

const quotePairsByOpener = new Map<string, QuotePair>([
  ['"', { opener: '"', closer: '"', kind: '"', symmetric: true }],
  ['＂', { opener: '＂', closer: '＂', kind: '"', symmetric: true }],
  ["'", { opener: "'", closer: "'", kind: "'", symmetric: true }],
  ['＇', { opener: '＇', closer: '＇', kind: "'", symmetric: true }],
  ['`', { opener: '`', closer: '`', kind: '`', symmetric: true }],
  ['｀', { opener: '｀', closer: '｀', kind: '`', symmetric: true }],
  ['「', { opener: '「', closer: '」', kind: '"', symmetric: false }],
  ['﹁', { opener: '﹁', closer: '﹂', kind: '"', symmetric: false }],
]);

const credentialSkeletonCharMap = new Map<string, string>([
  ['Α', 'A'],
  ['α', 'a'],
  ['А', 'A'],
  ['а', 'a'],
  ['Ε', 'E'],
  ['ε', 'e'],
  ['Е', 'E'],
  ['е', 'e'],
  ['Η', 'H'],
  ['Н', 'H'],
  ['һ', 'h'],
  ['Ι', 'I'],
  ['ι', 'i'],
  ['І', 'I'],
  ['і', 'i'],
  ['Ӏ', 'I'],
  ['Ο', 'O'],
  ['ο', 'o'],
  ['О', 'O'],
  ['о', 'o'],
  ['Ρ', 'P'],
  ['ρ', 'p'],
  ['Р', 'P'],
  ['р', 'p'],
  ['С', 'C'],
  ['с', 'c'],
  ['Τ', 'T'],
  ['τ', 't'],
  ['Т', 'T'],
  ['т', 't'],
  ['Χ', 'X'],
  ['χ', 'x'],
  ['Х', 'X'],
  ['х', 'x'],
  ['Ζ', 'Z'],
  ['ζ', 'z'],
]);

const recoveryByCode: Partial<
  Record<
    McpPlatformErrorCode,
    Pick<McpPlatformRecoveryViewModel, 'title' | 'nextStep'> &
      Partial<Pick<McpPlatformRecoveryViewModel, 'message'>>
  >
> = {
  credential_missing: {
    title: 'This page only supports unauthenticated endpoints',
    message:
      'Manual Remote HTTP can only be added here when the endpoint does not require authentication.',
    nextStep:
      'Use an unauthenticated endpoint here. Endpoints that require credentials cannot be added from this page yet.',
  },
  remote_http_policy_unavailable: {
    title: 'Remote HTTP is unavailable by policy',
    nextStep: 'Review the machine policy or contact its administrator before retrying.',
  },
  manual_stdio_provider_unavailable: {
    title: 'No approved stdio provider is available',
    nextStep: 'Restore the approved provider source or contact the policy administrator.',
  },
  policy_denied: {
    title: 'Blocked by machine policy',
    nextStep: 'Review the policy reasons and contact the policy administrator if needed.',
  },
  plan_expired: {
    title: 'Plan expired',
    nextStep: 'Create and review a new plan before confirming.',
  },
  plan_stale: {
    title: 'Plan is no longer current',
    nextStep: 'Refresh the MCP state and create a new plan.',
  },
  revision_conflict: {
    title: 'MCP state changed',
    nextStep: 'Refresh the MCP details before trying this action again.',
  },
  repository_unavailable: {
    title: 'MCP data is temporarily unavailable',
    nextStep: 'Retry when the local MCP repository is available.',
  },
};

export class McpPlatformServiceError extends Error {
  public readonly envelope: McpPlatformErrorEnvelope;
  public readonly recovery: McpPlatformRecoveryViewModel;

  constructor(envelope: McpPlatformErrorEnvelope, recovery: McpPlatformRecoveryViewModel) {
    super(recovery.message);
    this.name = 'McpPlatformServiceError';
    this.envelope = { ...envelope, message: recovery.message };
    this.recovery = recovery;
  }
}

export function mapMcpPlatformError(error: McpPlatformErrorEnvelope): McpPlatformRecoveryViewModel {
  const mapped = recoveryByCode[error.code];
  const safeMessage = mapped?.message ?? getSafeMcpUserMessage(error.message);
  return {
    title: mapped?.title ?? 'MCP operation could not be completed',
    message: safeMessage,
    nextStep:
      mapped?.nextStep ??
      (error.retryable ? 'Retry the operation.' : 'Review the MCP state before continuing.'),
    retryable: error.retryable,
    correlationId: error.correlationId,
    code: error.code,
    kind: 'service',
  };
}

export function toMcpRecoveryViewModel(error: unknown): McpPlatformRecoveryViewModel {
  if (error instanceof McpPlatformServiceError) return error.recovery;
  return {
    title: 'MCP Center is unavailable',
    message: 'The MCP Platform request could not be completed.',
    nextStep: 'Check the Goose connection and retry.',
    retryable: true,
    kind: 'connection',
  };
}

function createMcpConfirmTransportError(): McpPlatformServiceError {
  const message = 'Unable to complete MCP operation';
  return new McpPlatformServiceError(
    { code: 'repository_unavailable', message, retryable: true, correlationId: 'mcp-confirm-transport' },
    {
      title: 'MCP operation could not be completed',
      message,
      nextStep: 'Retry the operation.',
      retryable: true,
      code: 'repository_unavailable',
      kind: 'service',
    }
  );
}

export function unwrapMcpOutcome<T>(outcome: McpPlatformOutcome<T>): T {
  if (outcome.status === 'success') return outcome.value;
  throw new McpPlatformServiceError(outcome.error, mapMcpPlatformError(outcome.error));
}

function rethrowMalformedManagedMcpResponse(error: unknown): never {
  if (error instanceof McpPlatformServiceError) {
    throw error;
  }
  if (
    error instanceof Error &&
    /invalid MCP Platform (?:outcome|success payload|error)|success without a value/.test(
      error.message
    )
  ) {
    throw createMalformedMcpPlatformResponseError();
  }
  throw error;
}

async function unwrapManagedSdkOutcome<T>(
  request: Promise<{ outcome: McpPlatformOutcome<T> }>
): Promise<T> {
  try {
    return unwrapMcpOutcome((await request).outcome);
  } catch (error) {
    rethrowMalformedManagedMcpResponse(error);
  }
}

export function sanitizeMcpUserMessage(message?: string | null): string | undefined {
  if (!message) return undefined;
  const trimmed = message.trim();
  if (!trimmed) return undefined;

  const { truncated, bounded } = truncateMcpMessage(trimmed);
  const sanitized = bounded
    .split(/\r?\n/)
    .map((line) => sanitizeMcpMessageLine(line))
    .join('\n')
    .trim();

  const finalized = truncated ? appendMcpMessageTruncationNotice(sanitized) : sanitized;
  return finalized || (truncated ? mcpMessageTruncatedNotice : undefined);
}

function getSafeMcpUserMessage(message?: string | null): string {
  const sanitizedMessage = sanitizeMcpUserMessage(message);
  return sanitizedMessage && hasMeaningfulUserMessageSignal(sanitizedMessage)
    ? sanitizedMessage
    : genericMcpRequestFailedMessage;
}

function hasMeaningfulUserMessageSignal(message: string): boolean {
  const signal = message
    .replace(
      /(?:authorization|proxy-authorization)\s*[:=：＝]\s*(?:[A-Za-z][A-Za-z0-9._-]*\s+)?\[redacted\]/gi,
      ' '
    )
    .replace(/\[message truncated\]/gi, ' ')
    .replace(
      /\[(?:redacted|address removed|path removed|environment removed|argv removed|command removed)\]/gi,
      ' '
    )
    .replace(
      /\b(?:authorization|proxy-authorization|basic|bearer|digest|token|secret|password|credential|api[-_ ]?key|cookie|environment|env|argv|args|command|cmd|path|address|removed)\b/gi,
      ' '
    )
    .replace(/\b(?:at|using|from|with|via)\b/gi, ' ')
    .replace(/[^a-z0-9]+/gi, ' ')
    .trim();

  return !!signal;
}

function stripWrappingQuotes(value: string): string {
  if (!value) return value;
  const quote = getQuotePair(value[0]);
  if (quote && value.endsWith(quote.closer)) {
    return value.slice(1, -1);
  }
  return value;
}

type ParsedStructuredValue = {
  rawValue: string;
  end: number;
  quote: QuotePair | null;
};

type ParsedQuotedValue = ParsedStructuredValue & {
  content: string;
};

type StructuredFieldStart = {
  start: number;
  key: string;
  keyQuote: QuotePair | null;
  separator: string;
  valueStart: number;
};

type StructuredKeyMatch = {
  displayKey: string;
  kind: 'authorization' | 'secret' | 'path';
};

function sanitizeMcpMessageLine(line: string): string {
  return redactBareUnixPaths(
    redactWindowsPaths(redactUrls(redactCommandMetadata(redactStructuredValues(line))))
  )
    .replace(/[^\S\r\n]+/g, ' ')
    .trim();
}

function appendMcpMessageTruncationNotice(message: string): string {
  return message ? `${message}\n${mcpMessageTruncatedNotice}` : mcpMessageTruncatedNotice;
}

function truncateMcpMessage(message: string): { bounded: string; truncated: boolean } {
  if (message.length <= maxMcpErrorMessageLength) {
    return { bounded: message, truncated: false };
  }

  const safeBoundary = getSafeTruncationBoundary(message, maxMcpErrorMessageLength);
  return {
    bounded: message.slice(0, safeBoundary),
    truncated: true,
  };
}

function getSafeTruncationBoundary(message: string, maxLength: number): number {
  if (graphemeSegmenter) {
    let safeBoundary = 0;
    let lastSafeBoundary = 0;

    for (const { index, segment } of graphemeSegmenter.segment(message)) {
      const nextBoundary = index + segment.length;
      if (nextBoundary > maxLength) break;
      safeBoundary = nextBoundary;
      if (!isUnsafeOnlyTruncationSegment(segment)) {
        lastSafeBoundary = nextBoundary;
      }
    }

    if (safeBoundary > 0) return lastSafeBoundary;
  }

  let boundary = Math.min(maxLength, message.length);
  if (
    boundary < message.length &&
    boundary > 0 &&
    isHighSurrogate(message.charCodeAt(boundary - 1)) &&
    isLowSurrogate(message.charCodeAt(boundary))
  ) {
    boundary -= 1;
  }

  while (boundary > 0) {
    const codePointStart = getPreviousCodePointStart(message, boundary);
    const codePoint = message.codePointAt(codePointStart);
    if (codePoint === undefined || !isUnsafeTrailingTruncationCodePoint(codePoint)) break;
    boundary = codePointStart;
  }

  return boundary;
}

function isHighSurrogate(codeUnit: number): boolean {
  return codeUnit >= 0xd800 && codeUnit <= 0xdbff;
}

function isLowSurrogate(codeUnit: number): boolean {
  return codeUnit >= 0xdc00 && codeUnit <= 0xdfff;
}

function getPreviousCodePointStart(value: string, end: number): number {
  if (end <= 0) return 0;
  const previous = end - 1;
  if (isLowSurrogate(value.charCodeAt(previous)) && previous > 0) {
    const candidate = previous - 1;
    if (isHighSurrogate(value.charCodeAt(candidate))) return candidate;
  }
  return previous;
}

function isUnsafeTrailingTruncationCodePoint(codePoint: number): boolean {
  if (codePoint >= 0xfe00 && codePoint <= 0xfe0f) return true;
  if (codePoint >= 0xe0100 && codePoint <= 0xe01ef) return true;
  return /[\p{Cc}\p{Cf}\p{M}]/u.test(String.fromCodePoint(codePoint));
}

function isUnsafeOnlyTruncationSegment(segment: string): boolean {
  if (!segment) return false;

  for (let cursor = 0; cursor < segment.length; ) {
    const codePoint = segment.codePointAt(cursor);
    if (codePoint === undefined || !isUnsafeTrailingTruncationCodePoint(codePoint)) {
      return false;
    }
    cursor += codePoint > 0xffff ? 2 : 1;
  }

  return true;
}

function redactStructuredValues(line: string): string {
  let result = '';
  let cursor = 0;

  while (cursor < line.length) {
    const field = parseStructuredFieldStart(line, cursor);
    if (!field) {
      result += line[cursor];
      cursor += 1;
      continue;
    }

    const keyMatch = matchStructuredKey(field.key);
    if (!keyMatch) {
      result += line[cursor];
      cursor += 1;
      continue;
    }

    const parsedValue = readStructuredValue(line, field.valueStart, keyMatch.kind);
    if (!parsedValue) {
      result += line[cursor];
      cursor += 1;
      continue;
    }

    if (
      keyMatch.kind === 'path' &&
      !looksLikeAbsolutePath(stripWrappingQuotes(parsedValue.rawValue).trim())
    ) {
      result += line[cursor];
      cursor += 1;
      continue;
    }

    result += formatStructuredReplacement(
      keyMatch.displayKey,
      field.keyQuote,
      field.separator,
      parsedValue.rawValue,
      keyMatch.kind === 'authorization'
        ? buildAuthReplacementValue(parsedValue.rawValue)
        : keyMatch.kind === 'path'
          ? '[path removed]'
          : '[redacted]'
    );
    cursor = parsedValue.end;
  }

  return result;
}

function readStructuredValue(
  line: string,
  start: number,
  kind: StructuredKeyMatch['kind']
): ParsedStructuredValue | null {
  if (start >= line.length) return null;
  const quote = getQuotePair(line[start]);
  if (quote) return readQuotedValue(line, start, quote);

  if (kind === 'path') {
    let end = start;
    while (end < line.length) {
      const char = line[end];
      if (isWhitespace(char) || isHardStructuredBoundary(char)) break;
      end += 1;
    }
    return end > start ? { rawValue: line.slice(start, end), end, quote: null } : null;
  }

  if (kind === 'authorization') {
    const end = findPlainAuthorizationValueEnd(line, start);
    return end > start ? { rawValue: line.slice(start, end), end, quote: null } : null;
  }

  let end = start;
  while (end < line.length) {
    const char = line[end];
    if (isHardStructuredBoundary(char)) break;
    if (isWhitespace(char) && startsStructuredBoundaryAfterWhitespace(line, end)) {
      break;
    }
    end += 1;
  }
  while (end > start && isWhitespace(line[end - 1])) end -= 1;
  return end > start ? { rawValue: line.slice(start, end), end, quote: null } : null;
}

function findPlainAuthorizationValueEnd(line: string, start: number): number {
  for (let cursor = start; cursor < line.length; cursor += 1) {
    const field = parseStructuredFieldStart(line, cursor);
    if (!field || matchStructuredKey(field.key)?.kind !== 'authorization') continue;
    return rewindAuthorizationBoundary(line, start, field.start);
  }

  let end = line.length;
  while (end > start && isWhitespace(line[end - 1])) end -= 1;
  return end;
}

function rewindAuthorizationBoundary(line: string, start: number, boundary: number): number {
  let cursor = boundary;
  while (cursor > start && isWhitespace(line[cursor - 1])) {
    cursor -= 1;
  }
  if (
    cursor > start &&
    (structuredBoundaryChars.has(line[cursor - 1]) ||
      line[cursor - 1] === ',' ||
      line[cursor - 1] === ';')
  ) {
    cursor -= 1;
    while (cursor > start && isWhitespace(line[cursor - 1])) {
      cursor -= 1;
    }
  }
  return cursor;
}

function startsStructuredBoundaryAfterWhitespace(line: string, start: number): boolean {
  let cursor = start;
  while (
    cursor < line.length &&
    (isWhitespace(line[cursor]) || line[cursor] === ',' || line[cursor] === ';')
  ) {
    cursor += 1;
  }
  if (cursor < line.length && structuredBoundaryChars.has(line[cursor])) {
    cursor += 1;
  }

  const field = parseStructuredFieldStart(line, cursor);
  if (!field) return false;

  const normalizedKey = normalizeStructuredKey(field.key);
  return (
    preservedStructuredBoundaryKeys.has(normalizedKey) ||
    isSensitiveStructuredKey(normalizedKey) ||
    !/\s/u.test(field.key.trim())
  );
}

function buildAuthReplacementValue(rawValue: string): string {
  const normalizedValue = stripWrappingQuotes(rawValue).trim();
  if (!normalizedValue || normalizedValue === '[redacted]') return '[redacted]';
  const schemeMatch = normalizedValue.match(/^([A-Za-z][A-Za-z0-9._-]*)(?:\s+.+)?$/);
  return schemeMatch && /\s/.test(normalizedValue) ? `${schemeMatch[1]} [redacted]` : '[redacted]';
}

function redactCommandMetadata(line: string): string {
  return line.replace(
    /\b(command|cmd|argv|args?|environment|env)\b\s*[:=]\s*(?:"[^"\r\n]*"|'[^'\r\n]*'|`[^`\r\n]*`|\[[^\]\r\n]*\]|\{[^}\r\n]*\}|\S+)/gi,
    (_match, label: string) => {
      const normalized = label.toLowerCase();
      if (normalized === 'command' || normalized === 'cmd') return '[command removed]';
      if (normalized === 'argv' || normalized === 'arg' || normalized === 'args')
        return '[argv removed]';
      return '[environment removed]';
    }
  );
}

function redactUrls(line: string): string {
  return line.replace(/\b[a-z][a-z0-9+.-]*:\/\/[^\s"'`<>()[\]{}]+/gi, (match) =>
    replaceWithTrailingPunctuation(match, '[address removed]')
  );
}

function redactWindowsPaths(line: string): string {
  return line
    .replace(/\b[A-Za-z]:\\[^\s"'`<>()[\]{}]+/g, (match) =>
      replaceWithTrailingPunctuation(match, '[path removed]')
    )
    .replace(/\\\\[^\s"'`<>()[\]{}]+/g, (match) =>
      replaceWithTrailingPunctuation(match, '[path removed]')
    );
}

function redactBareUnixPaths(line: string): string {
  return line.replace(
    /(^|[\s("'`=:[{,])\/(?:(?:tmp|var|etc|usr|opt|home|srv|mnt|private|Users|Library|System|bin|sbin|dev|proc|run)(?:\/[^\s"'`<>()[\]{}]+)*|(?:[^\s"'`<>()[\]{}]+\/)+[^\s"'`<>()[\]{}]+)/g,
    (_match, prefix: string) => `${prefix}[path removed]`
  );
}

function replaceWithTrailingPunctuation(value: string, replacement: string): string {
  const suffixMatch = value.match(/[),.;!?]+$/);
  const suffix = suffixMatch?.[0] ?? '';
  return `${replacement}${suffix}`;
}

function looksLikeAbsolutePath(value: string): boolean {
  return (
    /^\/(?:tmp|var|etc|usr|opt|home|srv|mnt|private|Users|Library|System|bin|sbin|dev|proc|run)(?:\/|$)/.test(
      value
    ) ||
    /^\/(?:[^/\s"'`<>()[\]{}]+\/)+[^/\s"'`<>()[\]{}]+$/.test(value) ||
    /^[A-Za-z]:\\/.test(value) ||
    /^\\\\/.test(value)
  );
}

function formatStructuredReplacement(
  key: string,
  keyQuote: QuotePair | null,
  separator: string,
  originalValue: string,
  replacementValue: string
): string {
  const valueQuote = getQuotePair(originalValue[0]);
  const value = valueQuote
    ? `${valueQuote.opener}${replacementValue}${valueQuote.closer}`
    : replacementValue;
  const wrappedKey = keyQuote ? `${keyQuote.opener}${key}${keyQuote.closer}` : key;
  const formattedSeparator =
    !keyQuote && normalizeStructuredSeparator(separator) === ':' ? `${separator} ` : separator;
  return `${wrappedKey}${formattedSeparator}${value}`;
}

function getQuotePair(char?: string): QuotePair | null {
  if (!char) return null;
  return quotePairsByOpener.get(char) ?? null;
}

function normalizeStructuredSeparator(char: string): ':' | '=' | null {
  const normalized = char.normalize('NFKC');
  return normalized === ':' || normalized === '=' ? normalized : null;
}

function isHardStructuredBoundary(char: string): boolean {
  return char === ',' || char === ';' || char === ')' || char === ']' || char === '}';
}

function isWhitespace(char: string): boolean {
  return /\s/u.test(char);
}

function isIgnorableStructuredKeyChar(char: string): boolean {
  return ignoredStructuredKeyCharPattern.test(char);
}

function isStructuredKeyTokenChar(char: string): boolean {
  return /^[\p{L}\p{N}_-]$/u.test(char.normalize('NFKC'));
}

function isEscapedQuote(line: string, index: number): boolean {
  let slashCount = 0;
  let cursor = index - 1;
  while (cursor >= 0 && line[cursor] === '\\') {
    slashCount += 1;
    cursor -= 1;
  }
  return slashCount % 2 === 1;
}

function readQuotedValue(line: string, start: number, quote: QuotePair): ParsedQuotedValue {
  let end = start + 1;
  while (end < line.length) {
    if (line[end] === quote.closer && (!quote.symmetric || !isEscapedQuote(line, end))) {
      return {
        rawValue: line.slice(start, end + 1),
        end: end + 1,
        quote,
        content: line.slice(start + 1, end),
      };
    }
    end += 1;
  }
  return {
    rawValue: line.slice(start),
    end: line.length,
    quote,
    content: line.slice(start + 1),
  };
}

function parseStructuredFieldStart(line: string, start: number): StructuredFieldStart | null {
  if (start >= line.length) return null;
  if (start > 0 && !isStructuredFieldPrefix(line[start - 1])) return null;

  const openingChar = line[start];
  if (
    isWhitespace(openingChar) ||
    structuredBoundaryChars.has(openingChar) ||
    openingChar === ':' ||
    openingChar === '=' ||
    isHardStructuredBoundary(openingChar)
  ) {
    return null;
  }

  let cursor = start;
  let keyQuote: QuotePair | null = null;
  let key = '';

  const quote = getQuotePair(openingChar);
  if (quote) {
    keyQuote = quote;
    const parsedKey = readQuotedValue(line, cursor, quote);
    key = parsedKey.content;
    cursor = parsedKey.end;
  } else {
    const keyStart = cursor;
    while (cursor < line.length && !normalizeStructuredSeparator(line[cursor])) {
      if (isHardStructuredBoundary(line[cursor])) return null;
      if (cursor - keyStart >= maxStructuredKeyLength) return null;
      if (
        !isWhitespace(line[cursor]) &&
        !isIgnorableStructuredKeyChar(line[cursor]) &&
        !isStructuredKeyTokenChar(line[cursor])
      ) {
        return null;
      }
      cursor += 1;
    }
    if (cursor === start || cursor >= line.length) return null;
    key = line.slice(start, cursor).replace(/\s+$/g, '');
    if (key.trim().split(/\s+/u).filter(Boolean).length > 2) return null;
  }

  if (!key) return null;
  while (cursor < line.length && isWhitespace(line[cursor])) cursor += 1;
  if (cursor >= line.length) return null;
  const separator = normalizeStructuredSeparator(line[cursor]) ? line[cursor] : null;
  if (!separator) return null;
  cursor += 1;
  while (cursor < line.length && isWhitespace(line[cursor])) cursor += 1;
  return { start, key, keyQuote, separator, valueStart: cursor };
}

function isStructuredFieldPrefix(char: string): boolean {
  return isWhitespace(char) || char === '{' || char === '[' || char === '(' || char === ',';
}

function normalizeStructuredKey(rawKey: string): string {
  let normalized = '';

  for (const char of rawKey.normalize('NFKC').normalize('NFKD')) {
    if (isIgnorableStructuredKeyChar(char)) continue;
    normalized += credentialSkeletonCharMap.get(char) ?? char;
  }

  return normalized
    .trim()
    .replace(/[\s_]+/gu, '-')
    .replace(/-+/g, '-')
    .toLowerCase();
}

function isSensitiveStructuredKey(normalizedKey: string): boolean {
  return (
    normalizedKey === 'authorization' ||
    normalizedKey === 'proxy-authorization' ||
    normalizedKey === 'path' ||
    sensitiveCredentialKeyPattern.test(normalizedKey)
  );
}

function matchStructuredKey(rawKey: string): StructuredKeyMatch | null {
  const normalizedKey = normalizeStructuredKey(rawKey);
  if (normalizedKey === 'authorization')
    return { displayKey: 'Authorization', kind: 'authorization' };
  if (normalizedKey === 'proxy-authorization') {
    return { displayKey: 'Proxy-Authorization', kind: 'authorization' };
  }
  if (normalizedKey === 'path') return { displayKey: 'path', kind: 'path' };
  if (!sensitiveCredentialKeyPattern.test(normalizedKey)) return null;
  return { displayKey: canonicalStructuredKey(rawKey, normalizedKey), kind: 'secret' };
}

function canonicalStructuredKey(rawKey: string, normalizedKey: string): string {
  if (/^[\x20-\x7E]+$/.test(rawKey)) return rawKey.trim().replace(/\s+/g, ' ');
  switch (normalizedKey) {
    case 'authorization':
      return 'Authorization';
    case 'proxy-authorization':
      return 'Proxy-Authorization';
    case 'x-api-key':
      return 'X-Api-Key';
    case 'api-key':
      return 'Api-Key';
    case 'access-token':
      return 'Access-Token';
    case 'refresh-token':
      return 'Refresh-Token';
    case 'id-token':
      return 'Id-Token';
    case 'client-secret':
      return 'Client-Secret';
    case 'token':
    case 'secret':
    case 'password':
    case 'cookie':
    case 'set-cookie':
    case 'path':
    case 'credential':
    case 'credentials':
      return normalizedKey;
    default:
      return normalizedKey;
  }
}

export function createMcpIdempotencyKey(action: string): string {
  return `${action}-${window.crypto.randomUUID()}`;
}

type McpValueParser<T> = (value: unknown) => T | null;
type McpObject = Record<string, unknown>;
type SafeMcpJsonPrimitive = boolean | number | string | null;
type SafeMcpJsonValue =
  | SafeMcpJsonPrimitive
  | SafeMcpJsonValue[]
  | { [key: string]: SafeMcpJsonValue };
type KnownMcpPlatformErrorDetails = NonNullable<McpPlatformErrorEnvelope['details']>;
type KnownMcpPlatformErrorDetailType = KnownMcpPlatformErrorDetails['type'];
type KnownMcpRecoverySuggestion = Extract<
  KnownMcpPlatformErrorDetails,
  { type: 'manual_stdio_provider_unavailable' | 'remote_http_policy_unavailable' }
>['recovery'];

const maxMcpDisplayTextLength = 2_048;
const maxMcpIdentifierLength = 256;
const maxMcpArrayLength = 256;
const maxMcpDraftLocaleLength = 32;
const displayControlCharPattern = /[\p{Cc}\p{Cf}]+/gu;
const identifierControlCharPattern = /\p{Cc}/u;
const dangerousMcpWireKeys = new Set(['__proto__', 'constructor', 'prototype']);
const invalidMcpWireValue = Symbol('invalidMcpWireValue');

const profileResponseKeys = new Set(['outcome']);
const httpsManifestPrepareResultKeys = new Set([
  'provisionId', 'confirmationToken', 'expiresAtMs', 'preview',
]);
const httpsManifestConfirmResultKeys = new Set(['manifestDigest', 'preview']);
const httpsManifestPreviewKeys = new Set([
  'manifestId', 'version', 'redactedOrigin', 'rawDigest', 'parsedDigest',
  'redirectChainDigest', 'dnsEvidenceDigest', 'warnings',
]);
const outcomeKeys = new Set(['status', 'value', 'error']);
const successOutcomeKeys = new Set(['status', 'value']);
const errorOutcomeKeys = new Set(['status', 'error']);
const errorEnvelopeKeys = new Set(['code', 'message', 'retryable', 'correlationId', 'details']);
const emptyErrorDetailKeys = new Set(['type']);
const requestRejectedDetailKeys = emptyErrorDetailKeys;
const recordMissingDetailKeys = emptyErrorDetailKeys;
const integrityValidationFailedDetailKeys = emptyErrorDetailKeys;
const phaseUnavailableDetailKeys = new Set(['type', 'operation', 'phase']);
const planMismatchDetailKeys = emptyErrorDetailKeys;
const planExpiredDetailKeys = emptyErrorDetailKeys;
const idempotencyConflictDetailKeys = emptyErrorDetailKeys;
const revisionConflictDetailKeys = emptyErrorDetailKeys;
const transitionRejectedDetailKeys = emptyErrorDetailKeys;
const repositoryTemporarilyUnavailableDetailKeys = emptyErrorDetailKeys;
const projectionConflictDetailKeys = emptyErrorDetailKeys;
const credentialMissingDetailKeys = emptyErrorDetailKeys;
const healthGateFailedDetailKeys = emptyErrorDetailKeys;
const cancellationDeferredDetailKeys = emptyErrorDetailKeys;
const recoveryRequiredDetailKeys = emptyErrorDetailKeys;
const adapterVersionIncompatibleDetailKeys = emptyErrorDetailKeys;
const policyDecisionDetailKeys = new Set(['type', 'reason_codes']);
const externalCapabilityUnavailableDetailKeys = new Set(['type', 'capability']);
const manualStdioProviderUnavailableDetailKeys = new Set(['type', 'recovery']);
const remoteHttpPolicyUnavailableDetailKeys = new Set(['type', 'recovery']);
const externalPolicyDeniedDetailKeys = new Set(['type', 'capability']);
const supplyChainMismatchDetailKeys = new Set(['type', 'authority']);
const authenticationRequiredDetailKeys = new Set(['type', 'provider']);
const permissionGrantRequiredDetailKeys = new Set(['type', 'permission']);
const originRejectedDetailKeys = new Set(['type', 'origin_type']);
const immutableCommitUnavailableDetailKeys = emptyErrorDetailKeys;
const repositoryTreeRejectedDetailKeys = emptyErrorDetailKeys;
const developmentModeRequiredDetailKeys = emptyErrorDetailKeys;
const knownMcpErrorDetailKeys = new Set([
  'type',
  'reason_codes',
  'operation',
  'phase',
  'capability',
  'recovery',
  'authority',
  'provider',
  'permission',
  'origin_type',
]);
const profilePageKeys = new Set(['items']);
const profileSummaryKeys = new Set([
  'profileId',
  'name',
  'description',
  'revision',
  'archived',
  'entries',
  'createdAtMs',
  'updatedAtMs',
  'credentialStatus',
]);
const profileEntryKeys = new Set(['managedMcpId', 'ordinal']);
const profileDetailKeys = new Set(['profile', 'history']);
const profileRevisionKeys = new Set(['revision', 'operation', 'actor', 'createdAtMs']);
const profileApplyPlanKeys = new Set([
  'planId',
  'profileId',
  'profileRevision',
  'mergePolicy',
  'entries',
  'expiresAtMs',
  'confirmation',
]);
const profileApplyEntryKeys = new Set([
  'managedMcpId',
  'mcpId',
  'name',
  'version',
  'health',
  'authReady',
  'policyReady',
  'readiness',
]);
const profileApplyConfirmationKeys = new Set(['confirmationToken']);
const profileApplicationTokenKeys = new Set([
  'token',
  'planId',
  'profileId',
  'profileRevision',
  'expiresAtMs',
]);
const profileConnectionTestResultKeys = new Set(['passed', 'stages']);
const connectionTestStageKeys = new Set([
  'phase',
  'status',
  'code',
  'diagnostic',
  'repairSuggestion',
  'durationMs',
  'managedMcpId',
]);
const profileDraftKeys = new Set([
  'locale',
  'name',
  'description',
  'entries',
  'candidates',
  'unresolvedTerms',
  'lowConfidence',
  'persisted',
]);
const profileDraftCandidateKeys = new Set([
  'managedMcpId',
  'mcpId',
  'name',
  'description',
  'confidence',
  'reasonCode',
]);
const catalogSummaryKeys = new Set([
  'sourceId',
  'mcpId',
  'version',
  'manifestDigest',
  'name',
  'description',
  'publisherId',
  'publisherName',
  'trustTier',
  'proof',
  'compatibility',
  'distribution',
  'verifiedAtMs',
  'eligibility',
]);
const sourceProofLocalKeys = new Set(['type']);
const sourceProofCatalogKeys = new Set([
  'type',
  'index_digest',
  'declared_manifest_digest',
  'signature',
]);
const signatureProofKeys = new Set(['algorithm', 'signingIdentity', 'present']);
const catalogEligibilityKeys = new Set(['outcome', 'reason', 'recovery']);
const catalogPageKeys = new Set(['items', 'nextCursor', 'cache']);
const catalogCacheMetadataKeys = new Set([
  'offline',
  'localPersistenceOnly',
  'newestVerifiedAtMs',
  'freshness',
  'refreshState',
  'recovery',
]);
const catalogDetailKeys = new Set([
  'sourceId',
  'mcpId',
  'version',
  'manifestDigest',
  'name',
  'description',
  'publisher',
  'proof',
  'trustTier',
  'distribution',
  'transport',
  'auth',
  'health',
  'capabilities',
  'permissions',
  'compatibility',
  'verifiedAtMs',
  'eligibility',
]);
const catalogPublisherKeys = new Set(['id', 'name', 'website', 'signingIdentities']);
const catalogArtifactKeys = new Set(['platform', 'architecture', 'origin', 'digest', 'sizeBytes']);
const catalogRemoteHttpDistributionKeys = new Set(['type']);
const catalogManualStdioDistributionKeys = new Set(['type', 'platforms']);
const catalogNpmDistributionKeys = new Set(['type', 'package', 'package_version', 'artifacts']);
const catalogPythonWheelDistributionKeys = new Set([
  'type',
  'package',
  'package_version',
  'python',
  'artifacts',
]);
const catalogBinaryArchiveDistributionKeys = new Set(['type', 'archive_format', 'artifacts']);
const catalogDockerDistributionKeys = new Set(['type', 'image', 'digest']);
const catalogGitDevDistributionKeys = new Set(['type', 'repository', 'commit', 'adapter']);
const catalogStdioTransportKeys = new Set(['type', 'startup_timeout_seconds']);
const catalogStreamableHttpTransportKeys = new Set([
  'type',
  'endpoint',
  'connect_timeout_seconds',
  'allowed_redirect_origins',
]);
const catalogNoneAuthKeys = new Set(['type']);
const catalogApiKeyHeaderAuthKeys = new Set(['type', 'credential_required', 'prefix_required']);
const catalogEnvironmentAuthKeys = new Set(['type', 'credential_required']);
const catalogOauth2AuthKeys = new Set([
  'type',
  'authorization_endpoint',
  'token_endpoint',
  'client_registration',
  'scopes',
]);
const catalogMcpInitializeHealthKeys = new Set(['type', 'timeout_seconds']);
const catalogMcpListToolsHealthKeys = new Set(['type', 'timeout_seconds']);
const catalogHttpHealthKeys = new Set([
  'type',
  'relative_endpoint',
  'expected_status',
  'timeout_seconds',
]);
const catalogPermissionKeys = new Set(['id', 'kind', 'reason', 'required', 'scope']);
const sourcesPolicyStateKeys = new Set(['sources', 'policy']);
const sourcePolicySourceKeys = new Set([
  'sourceId',
  'displayName',
  'importKind',
  'trustTiers',
  'manifestCount',
  'lastImportedAtMs',
  'cache',
  'compatibility',
  'recovery',
  'lastRefreshDocumentDigest',
  'lastRefreshedAtMs',
]);
const sourcePolicyCompatibilityKeys = new Set(['compatible', 'restricted', 'denied']);
const sourcePolicyMachineKeys = new Set([
  'targetPlatform',
  'targetArchitecture',
  'developmentMode',
  'dockerAllowed',
  'recovery',
]);
const modelRecommendationKeys = new Set(['candidates', 'inventoryOnly', 'mutated']);
const modelRecommendationCandidateKeys = new Set([
  'providerId',
  'modelId',
  'confidence',
  'reasonCodes',
  'caveatCodes',
]);
const sourceRefreshResultKeys = new Set([
  'sourceId',
  'displayName',
  'documentDigest',
  'manifestCount',
  'refreshedAtMs',
]);
const governedImportResultKeys = new Set(['source', 'document', 'entries']);
const governedImportSourceSummaryKeys = new Set(['sourceId', 'importKind', 'displayName']);
const governedImportDocumentSummaryKeys = new Set([
  'sourceId',
  'documentId',
  'documentDigest',
  'documentKind',
]);
const sourceProvisionPreviewKeys = new Set([
  'sourceId',
  'displayName',
  'rootDigest',
  'documentDigest',
  'endpointHost',
  'refreshTransport',
  'manifestCount',
  'warnings',
  'trustBasis',
]);
const sourceProvisionPrepareResultKeys = new Set([
  'provisionId',
  'confirmationToken',
  'expiresAtMs',
  'preview',
]);
const sourceProvisionConfirmResultKeys = new Set(['provisionId', 'confirmedAtMs', 'preview']);
const knownMcpPlatformErrorCodes = new Set<McpPlatformErrorCode>([
  'invalid_request',
  'unsafe_url',
  'not_found',
  'integrity_error',
  'policy_denied',
  'not_implemented_for_phase',
  'operation_not_supported',
  'manual_stdio_provider_unavailable',
  'remote_http_policy_unavailable',
  'plan_stale',
  'plan_expired',
  'idempotency_conflict',
  'revision_conflict',
  'invalid_transition',
  'repository_unavailable',
  'projection_conflict',
  'credential_missing',
  'health_failed',
  'task_not_cancellable',
  'rollback_incomplete',
  'adapter_incompatible',
  'docker_unavailable',
  'daemon_policy_denied',
  'image_digest_mismatch',
  'registry_auth_required',
  'mount_permission_denied',
  'git_unavailable',
  'git_origin_denied',
  'commit_unavailable',
  'unsafe_repository_tree',
  'development_mode_required',
  'runtime_control_unavailable',
]);
const mcpRecoverySuggestionValues = new Set<KnownMcpRecoverySuggestion>([
  'none',
  'review_permissions',
  'choose_compatible_release',
  'install_required_runtime',
  'enable_development_mode',
  'restore_verified_cache',
  'retry',
  'recreate_plan',
  'resolve_recovery',
  'contact_policy_administrator',
]);
const mcpCredentialStatusValues = new Set<McpCredentialStatus>([
  'unconfigured',
  're_registration_required',
  'trusted_state_conflict',
  'temporarily_unavailable',
  'ready',
]);
const mcpConnectionTestPhaseValues = new Set<McpConnectionTestPhase>([
  'eligibility',
  'mcp_connect_initialize',
  'tool_discovery',
  'cleanup',
  'model_request',
  'tool_visibility',
]);
const mcpConnectionTestStatusValues = new Set<McpConnectionTestStatus>([
  'passed',
  'failed',
  'skipped',
]);
const mcpConnectionTestCodeValues = new Set<McpConnectionTestCode>([
  'eligible',
  'profile_not_ready',
  'policy_denied',
  'mcp_connect_failed',
  'mcp_timeout',
  'tool_discovery_failed',
  'no_tools_exposed',
  'provider_not_configured',
  'model_not_configured',
  'provider_initialization_failed',
  'model_request_failed',
  'runtime_activation_failed',
  'resource_limit_exceeded',
  'prerequisites_failed',
  'cleanup_failed',
  'tool_visibility_validated',
  'tool_visibility_skipped',
]);
const governedImportKindValues = new Set<McpGovernedImportKind>([
  'local_persistence',
  'verified_source_catalog',
  'https_manifest_url',
  'enterprise_directory',
]);
const governedImportDocumentKindValues = new Set<McpGovernedCatalogDocumentKind>([
  'manifest',
  'directory',
]);
const sourceProvisionRefreshTransportValues = new Set<McpSourceProvisionRefreshTransport>([
  'verified_source_bundle_v1',
]);
const sourceProvisionTrustBasisValues = new Set<McpSourceProvisionTrustBasis>(['user_pin']);
const catalogTrustTierValues = new Set<McpTrustTier>(['official', 'community', 'local']);
const catalogCompatibilityValues = new Set(['compatible', 'incompatible'] as const);
const catalogDistributionValues = new Set([
  'remote_http',
  'manual_stdio',
  'npm',
  'python_wheel',
  'binary_archive',
  'docker',
  'git_dev',
] as const);
const catalogEligibilityOutcomeValues = new Set(['allowed', 'restricted', 'denied'] as const);
const catalogEligibilityReasonValues = new Set([
  'eligible',
  'confirmation_required',
  'platform_unsupported',
  'runtime_unavailable',
  'policy_denied',
  'development_mode_required',
  'external_capability_unavailable',
] as const);
const catalogCacheFreshnessValues = new Set([
  'fresh',
  'stale',
  'offline_verified',
  'empty',
] as const);
const catalogRefreshStateValues = new Set(['idle', 'refreshing', 'failed', 'local_only'] as const);
const catalogCapabilityValues = new Set([
  'tools',
  'resources',
  'prompts',
  'sampling',
  'elicitation',
  'logging',
] as const);
const catalogPermissionKindValues = new Set([
  'filesystem_read',
  'filesystem_write',
  'network',
  'credentials',
  'process_spawn',
  'docker',
  'host_application',
] as const);
const allowedMissingDetailCodes = new Set<McpPlatformErrorCode>(['invalid_request']);
const allowedDetailTypesByCode: Record<
  McpPlatformErrorCode,
  ReadonlySet<KnownMcpPlatformErrorDetailType>
> = {
  invalid_request: new Set(['request_rejected']),
  unsafe_url: new Set(['origin_rejected']),
  not_found: new Set(['record_missing']),
  integrity_error: new Set(['integrity_validation_failed']),
  policy_denied: new Set(['policy_decision']),
  not_implemented_for_phase: new Set(['phase_unavailable']),
  operation_not_supported: new Set(['phase_unavailable']),
  manual_stdio_provider_unavailable: new Set(['manual_stdio_provider_unavailable']),
  remote_http_policy_unavailable: new Set(['remote_http_policy_unavailable']),
  plan_stale: new Set(['plan_mismatch']),
  plan_expired: new Set(['plan_expired']),
  idempotency_conflict: new Set(['idempotency_conflict']),
  revision_conflict: new Set(['revision_conflict']),
  invalid_transition: new Set(['transition_rejected']),
  repository_unavailable: new Set(['repository_temporarily_unavailable']),
  projection_conflict: new Set(['projection_conflict']),
  credential_missing: new Set(['credential_missing']),
  health_failed: new Set(['health_gate_failed']),
  task_not_cancellable: new Set(['cancellation_deferred']),
  rollback_incomplete: new Set(['recovery_required', 'witness_expired', 'witness_consumed']),
  adapter_incompatible: new Set(['adapter_version_incompatible']),
  docker_unavailable: new Set(['external_capability_unavailable']),
  daemon_policy_denied: new Set(['external_policy_denied']),
  image_digest_mismatch: new Set(['supply_chain_mismatch']),
  registry_auth_required: new Set(['authentication_required']),
  mount_permission_denied: new Set(['permission_grant_required']),
  git_unavailable: new Set(['external_capability_unavailable']),
  git_origin_denied: new Set(['origin_rejected']),
  commit_unavailable: new Set(['immutable_commit_unavailable']),
  unsafe_repository_tree: new Set(['repository_tree_rejected']),
  development_mode_required: new Set(['development_mode_required']),
  runtime_control_unavailable: new Set(['external_capability_unavailable']),
};

function createMalformedMcpPlatformResponseError(): Error {
  return new Error(genericMcpRequestFailedMessage);
}

const httpsProvisionReviewKeys = new Set([
  'planId', 'planDigest', 'expiresAtMs', 'trustTier', 'mcpId', 'name', 'version',
  'selectedManifestDigest', 'permissions', 'fileEffects', 'processEffects',
  'reversibility', 'policy', 'warnings', 'requiredConfirmations', 'defaultDisabled', 'recovery',
]);

function readHttpsProvisionRecord(value: unknown, keys: ReadonlySet<string>): McpObject | null {
  const record = readMcpObject(value, keys);
  return record && hasExactMcpKeys(record, keys) ? record : null;
}

function requiredHttpsString(record: McpObject, key: string): string | null {
  return typeof record[key] === 'string' && record[key].trim() ? record[key] as string : null;
}

function requiredHttpsNumber(record: McpObject, key: string): number | null {
  return typeof record[key] === 'number' && Number.isFinite(record[key]) ? record[key] as number : null;
}

function requiredHttpsNonnegativeNumber(record: McpObject, key: string): number | null {
  const value = requiredHttpsNumber(record, key);
  return value !== null && value >= 0 ? value : null;
}

function requiredHttpsBoolean(record: McpObject, key: string): boolean | null {
  return typeof record[key] === 'boolean' ? record[key] : null;
}

export function projectHttpsProvisionPlanReviewRequest(
  input: McpHttpsProvisionPlanReviewRequest
): McpHttpsProvisionPlanReviewRequest {
  if (!input || typeof input !== 'object') throw createMalformedMcpPlatformResponseError();
  const record = input as unknown as Record<string, unknown>;
  const keys = new Set(Object.keys(record));
  if (keys.size !== 3 || !keys.has('provisionId') || !keys.has('expectedManifestDigest') || !keys.has('idempotencyKey')) {
    throw createMalformedMcpPlatformResponseError();
  }
  for (const key of keys) if (typeof record[key] !== 'string' || !(record[key] as string).trim()) {
    throw createMalformedMcpPlatformResponseError();
  }
  return {
    provisionId: record.provisionId as string,
    expectedManifestDigest: record.expectedManifestDigest as string,
    idempotencyKey: record.idempotencyKey as string,
  };
}

export function parseHttpsProvisionPlanReview(value: unknown): McpHttpsProvisionPlanReview | null {
  const root = readHttpsProvisionRecord(value, httpsProvisionReviewKeys);
  if (!root) return null;
  const mcpId = requiredHttpsString(root, 'mcpId');
  const name = requiredHttpsString(root, 'name');
  const version = requiredHttpsString(root, 'version');
  const planId = requiredHttpsString(root, 'planId');
  const planDigest = requiredHttpsString(root, 'planDigest');
  const expiresAtMs = requiredHttpsNumber(root, 'expiresAtMs');
  const permissions = Array.isArray(root.permissions) ? root.permissions.map((item) => {
    const permission = readHttpsProvisionRecord(item, new Set(['kind', 'required']));
    const kind = permission ? requiredHttpsString(permission, 'kind') : null;
    const required = permission ? requiredHttpsBoolean(permission, 'required') : null;
    return kind !== null && required !== null ? { kind, required } : null;
  }).filter((item): item is { kind: string; required: boolean } => item !== null) : null;
  const file = readHttpsProvisionRecord(root.fileEffects, new Set(['writesFiles', 'removesFiles', 'ownedItems']));
  const process = readHttpsProvisionRecord(root.processEffects, new Set(['processRequiredForConnection', 'startsDuringConfirmation']));
  const policy = readHttpsProvisionRecord(root.policy, new Set(['outcome', 'reasonCount']));
  const fileEffects = file ? {
    writesFiles: requiredHttpsBoolean(file, 'writesFiles'),
    removesFiles: requiredHttpsBoolean(file, 'removesFiles'),
    ownedItems: requiredHttpsNonnegativeNumber(file, 'ownedItems'),
  } : null;
  const processEffects = process ? {
    processRequiredForConnection: requiredHttpsBoolean(process, 'processRequiredForConnection'),
    startsDuringConfirmation: requiredHttpsBoolean(process, 'startsDuringConfirmation'),
  } : null;
  const policyOutcome = policy ? requiredHttpsString(policy, 'outcome') : null;
  const reasonCount = policy ? requiredHttpsNonnegativeNumber(policy, 'reasonCount') : null;
  const confirmations = Array.isArray(root.requiredConfirmations) ? root.requiredConfirmations.map((item) => {
    const confirmation = readHttpsProvisionRecord(item, new Set(['type']));
    return confirmation && (confirmation.type === 'policy' || confirmation.type === 'permission') ? confirmation.type : null;
  }).filter((item): item is 'policy' | 'permission' => item !== null) : null;
  const warnings = Array.isArray(root.warnings) && root.warnings.every((item): item is string => typeof item === 'string') ? root.warnings : null;
  if (!mcpId || !name || !version || !planId || !planDigest || expiresAtMs === null || !permissions || permissions.some((item) => !item) ||
      fileEffects === null || fileEffects.writesFiles === null || fileEffects.removesFiles === null || fileEffects.ownedItems === null ||
      processEffects === null || processEffects.processRequiredForConnection === null || processEffects.startsDuringConfirmation === null ||
      policyOutcome === null || reasonCount === null || !confirmations || confirmations.some((item) => !item) ||
      typeof root.defaultDisabled !== 'boolean' || !warnings) return null;
  return {
    planId, planDigest, expiresAtMs, operation: 'provision', mcp: { mcpId, name, version },
    permissions,
    effects: { file: { writesFiles: fileEffects.writesFiles, removesFiles: fileEffects.removesFiles, ownedItems: fileEffects.ownedItems },
      process: { processRequiredForConnection: processEffects.processRequiredForConnection, startsDuringConfirmation: processEffects.startsDuringConfirmation } },
    confirmation: { required: confirmations, defaultDisabled: root.defaultDisabled },
    policy: { outcome: policyOutcome, reasonCount }, warnings,
  };
}

function hasExactMcpKeys(value: Record<string, unknown>, keys: ReadonlySet<string>): boolean {
  const actualKeys = Object.keys(value);
  return actualKeys.length === keys.size && actualKeys.every((key) => keys.has(key));
}

function isPlainMcpWireObject(value: unknown): value is object {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  try {
    return Object.getPrototypeOf(value) === Object.prototype;
  } catch {
    return false;
  }
}

function readMcpObject(value: unknown, allowedKeys: ReadonlySet<string>): McpObject | null {
  if (!isPlainMcpWireObject(value)) return null;
  try {
    const record: McpObject = {};
    for (const ownKey of Reflect.ownKeys(value)) {
      if (
        typeof ownKey !== 'string' ||
        dangerousMcpWireKeys.has(ownKey) ||
        !allowedKeys.has(ownKey)
      ) {
        return null;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, ownKey);
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !('value' in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return null;
      }
      record[ownKey] = descriptor.value;
    }
    return record;
  } catch {
    return null;
  }
}

function isPlainMcpWireArray(value: unknown): value is unknown[] {
  if (!Array.isArray(value)) return false;
  try {
    return Object.getPrototypeOf(value) === Array.prototype;
  } catch {
    return false;
  }
}

function isMcpArrayIndexKey(key: string): boolean {
  return /^(?:0|[1-9]\d*)$/.test(key);
}

function sanitizeMcpJsonValue(
  value: unknown,
  seen = new Set<object>()
): SafeMcpJsonValue | typeof invalidMcpWireValue {
  if (value === null) return null;

  switch (typeof value) {
    case 'boolean':
    case 'string':
      return value;
    case 'number':
      return Number.isFinite(value) ? value : invalidMcpWireValue;
    case 'undefined':
    case 'bigint':
    case 'function':
    case 'symbol':
      return invalidMcpWireValue;
    case 'object':
      break;
  }

  if (seen.has(value)) return invalidMcpWireValue;
  seen.add(value);
  try {
    if (isPlainMcpWireArray(value)) {
      return sanitizeMcpJsonArray(value, seen);
    }
    if (!isPlainMcpWireObject(value)) {
      return invalidMcpWireValue;
    }
    return sanitizeMcpJsonObject(value, seen);
  } finally {
    seen.delete(value);
  }
}

function sanitizeMcpJsonArray(
  value: unknown[],
  seen: Set<object>
): SafeMcpJsonValue[] | typeof invalidMcpWireValue {
  try {
    const lengthDescriptor = Object.getOwnPropertyDescriptor(value, 'length');
    if (
      !lengthDescriptor ||
      !('value' in lengthDescriptor) ||
      lengthDescriptor.get !== undefined ||
      lengthDescriptor.set !== undefined ||
      lengthDescriptor.enumerable ||
      !Number.isSafeInteger(lengthDescriptor.value) ||
      lengthDescriptor.value < 0
    ) {
      return invalidMcpWireValue;
    }

    const length = lengthDescriptor.value;
    const sanitized: SafeMcpJsonValue[] = [];
    for (const ownKey of Reflect.ownKeys(value)) {
      if (typeof ownKey !== 'string') return invalidMcpWireValue;
      if (ownKey === 'length') continue;
      if (dangerousMcpWireKeys.has(ownKey) || !isMcpArrayIndexKey(ownKey)) {
        return invalidMcpWireValue;
      }
      const index = Number(ownKey);
      if (!Number.isSafeInteger(index) || index < 0 || index >= length) {
        return invalidMcpWireValue;
      }
    }

    for (let index = 0; index < length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !('value' in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return invalidMcpWireValue;
      }
      const sanitizedItem = sanitizeMcpJsonValue(descriptor.value, seen);
      if (sanitizedItem === invalidMcpWireValue) {
        return invalidMcpWireValue;
      }
      sanitized.push(sanitizedItem);
    }
    return sanitized;
  } catch {
    return invalidMcpWireValue;
  }
}

function sanitizeMcpJsonObject(
  value: object,
  seen: Set<object>
): { [key: string]: SafeMcpJsonValue } | typeof invalidMcpWireValue {
  try {
    const sanitized: { [key: string]: SafeMcpJsonValue } = {};
    for (const ownKey of Reflect.ownKeys(value)) {
      if (typeof ownKey !== 'string' || dangerousMcpWireKeys.has(ownKey)) {
        return invalidMcpWireValue;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, ownKey);
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !('value' in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return invalidMcpWireValue;
      }
      const sanitizedValue = sanitizeMcpJsonValue(descriptor.value, seen);
      if (sanitizedValue === invalidMcpWireValue) {
        return invalidMcpWireValue;
      }
      sanitized[ownKey] = sanitizedValue;
    }
    return sanitized;
  } catch {
    return invalidMcpWireValue;
  }
}

function normalizeMcpDisplayString(value: string, maxLength: number): string {
  return value
    .replace(displayControlCharPattern, ' ')
    .replace(/\s+/gu, ' ')
    .trim()
    .slice(0, maxLength);
}

function parseMcpDisplayString(
  value: unknown,
  {
    allowEmpty = false,
    maxLength = maxMcpDisplayTextLength,
  }: {
    allowEmpty?: boolean;
    maxLength?: number;
  } = {}
): string | null {
  if (typeof value !== 'string') return null;
  const normalized = normalizeMcpDisplayString(value, maxLength);
  if (!allowEmpty && normalized.length === 0) return null;
  return normalized;
}

function parseMcpIdentifierString(
  value: unknown,
  {
    allowEmpty = false,
    rejectTrimmedBlank = false,
    maxLength = maxMcpIdentifierLength,
  }: {
    allowEmpty?: boolean;
    rejectTrimmedBlank?: boolean;
    maxLength?: number;
  } = {}
): string | null {
  if (typeof value !== 'string') return null;
  if (
    (!allowEmpty && value.length === 0) ||
    value.length > maxLength ||
    identifierControlCharPattern.test(value)
  ) {
    return null;
  }
  if (rejectTrimmedBlank && value.trim().length === 0) return null;
  return value;
}

function parseMcpProfileOpaqueIdString(
  value: unknown,
  {
    allowEmpty = false,
    rejectTrimmedBlank = false,
    maxLength = maxMcpIdentifierLength,
  }: {
    allowEmpty?: boolean;
    rejectTrimmedBlank?: boolean;
    maxLength?: number;
  } = {}
): string | null {
  if (typeof value !== 'string') return null;
  if ((!allowEmpty && value.length === 0) || value.length > maxLength || value.includes('\0')) {
    return null;
  }
  if (rejectTrimmedBlank && value.trim().length === 0) return null;
  return value;
}

function parseMcpDraftLocaleString(value: unknown): string | null {
  if (typeof value !== 'string' || value.length > maxMcpDraftLocaleLength) {
    return null;
  }
  return value;
}

function parseMcpInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function parseMcpUnsignedInteger(value: unknown, maximum: number): number | null {
  const integer = parseMcpInteger(value);
  return integer !== null && integer <= maximum ? integer : null;
}

function parseMcpContractString(
  value: unknown,
  maxLength = maxMcpDisplayTextLength
): string | null {
  return parseMcpIdentifierString(value, { maxLength, rejectTrimmedBlank: true });
}

function parseMcpOptionalValue<T>(
  value: unknown,
  parser: (next: unknown) => T | null
): T | undefined | null {
  if (value === undefined || value === null) return undefined;
  return parser(value);
}

function parseMcpBoolean(value: unknown): boolean | null {
  return typeof value === 'boolean' ? value : null;
}

function parseMcpNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function parseMcpArray<T>(
  value: unknown,
  parseItem: (item: unknown) => T | null,
  maxLength = maxMcpArrayLength
): T[] | null {
  if (!Array.isArray(value) || value.length > maxLength) return null;
  const parsed: T[] = [];
  for (const item of value) {
    const next = parseItem(item);
    if (next === null) return null;
    parsed.push(next);
  }
  return parsed;
}

function parseMcpDisplayStringArray(
  value: unknown,
  maxLength = maxMcpArrayLength
): string[] | null {
  return parseMcpArray(
    value,
    (item) =>
      parseMcpDisplayString(item, {
        allowEmpty: false,
        maxLength: maxMcpIdentifierLength,
      }),
    maxLength
  );
}

function parseMcpIdentifierArray(
  value: unknown,
  {
    maxLength = maxMcpArrayLength,
    rejectTrimmedBlank = false,
  }: {
    maxLength?: number;
    rejectTrimmedBlank?: boolean;
  } = {}
): string[] | null {
  return parseMcpArray(
    value,
    (item) =>
      parseMcpIdentifierString(item, {
        maxLength: maxMcpIdentifierLength,
        rejectTrimmedBlank,
      }),
    maxLength
  );
}

function parseMcpPlatformErrorCode(value: unknown): McpPlatformErrorCode | null {
  const code = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return code && knownMcpPlatformErrorCodes.has(code as McpPlatformErrorCode)
    ? (code as McpPlatformErrorCode)
    : null;
}

function parseMcpRecoverySuggestion(value: unknown): KnownMcpRecoverySuggestion | null {
  const recovery = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return recovery && mcpRecoverySuggestionValues.has(recovery as KnownMcpRecoverySuggestion)
    ? (recovery as KnownMcpRecoverySuggestion)
    : null;
}

function parseMcpCredentialStatus(value: unknown): McpCredentialStatus | null {
  const status = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return status && mcpCredentialStatusValues.has(status as McpCredentialStatus)
    ? (status as McpCredentialStatus)
    : null;
}

function parseCatalogTrustTier(value: unknown): McpTrustTier | null {
  const tier = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return tier && catalogTrustTierValues.has(tier as McpTrustTier) ? (tier as McpTrustTier) : null;
}

function parseCatalogCompatibility(value: unknown): McpCatalogSummary['compatibility'] | null {
  const compatibility = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return compatibility &&
    catalogCompatibilityValues.has(compatibility as 'compatible' | 'incompatible')
    ? (compatibility as McpCatalogSummary['compatibility'])
    : null;
}

function parseCatalogDistribution(value: unknown): McpCatalogSummary['distribution'] | null {
  const distribution = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return distribution &&
    catalogDistributionValues.has(
      distribution as
        | 'remote_http'
        | 'manual_stdio'
        | 'npm'
        | 'python_wheel'
        | 'binary_archive'
        | 'docker'
        | 'git_dev'
    )
    ? (distribution as McpCatalogSummary['distribution'])
    : null;
}

function parseCatalogEligibility(value: unknown): McpCatalogSummary['eligibility'] | null {
  const record = readMcpObject(value, catalogEligibilityKeys);
  if (!record) return null;
  const outcome = parseMcpIdentifierString(record.outcome, { maxLength: maxMcpIdentifierLength });
  const reason = parseMcpIdentifierString(record.reason, { maxLength: maxMcpIdentifierLength });
  const recovery = parseMcpRecoverySuggestion(record.recovery);
  if (
    !outcome ||
    !catalogEligibilityOutcomeValues.has(outcome as 'allowed' | 'restricted' | 'denied') ||
    !reason ||
    !catalogEligibilityReasonValues.has(
      reason as
        | 'eligible'
        | 'confirmation_required'
        | 'platform_unsupported'
        | 'runtime_unavailable'
        | 'policy_denied'
        | 'development_mode_required'
        | 'external_capability_unavailable'
    ) ||
    recovery === null
  ) {
    return null;
  }
  return {
    outcome: outcome as McpCatalogSummary['eligibility']['outcome'],
    reason: reason as McpCatalogSummary['eligibility']['reason'],
    recovery,
  };
}

function parseCatalogSourceProof(value: unknown): McpCatalogSummary['proof'] | null {
  const local = readMcpObject(value, sourceProofLocalKeys);
  if (local?.type === 'local_bytes') {
    return { type: 'local_bytes' };
  }

  const catalog = readMcpObject(value, sourceProofCatalogKeys);
  if (!catalog || catalog.type !== 'catalog') return null;
  const indexDigest = parseMcpIdentifierString(catalog.index_digest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const declaredManifestDigest = parseMcpIdentifierString(catalog.declared_manifest_digest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const signatureValue =
    catalog.signature === null
      ? null
      : catalog.signature === undefined
        ? undefined
        : (() => {
            const signature = readMcpObject(catalog.signature, signatureProofKeys);
            if (!signature) return null;
            const algorithm = parseMcpIdentifierString(signature.algorithm, {
              maxLength: maxMcpIdentifierLength,
              rejectTrimmedBlank: true,
            });
            const signingIdentity = parseMcpIdentifierString(signature.signingIdentity, {
              maxLength: maxMcpDisplayTextLength,
              rejectTrimmedBlank: true,
            });
            const present = parseMcpBoolean(signature.present);
            if (algorithm === null || signingIdentity === null || present === null) {
              return null;
            }
            return {
              algorithm,
              signingIdentity,
              present,
            };
          })();

  if (indexDigest === null || declaredManifestDigest === null || signatureValue === null) {
    return null;
  }

  return {
    type: 'catalog',
    index_digest: indexDigest,
    declared_manifest_digest: declaredManifestDigest,
    ...(signatureValue === undefined ? {} : { signature: signatureValue }),
  };
}

function parseMcpCatalogSummaryValue(value: unknown): McpCatalogSummary | null {
  const record = readMcpObject(value, catalogSummaryKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const mcpId = parseMcpIdentifierString(record.mcpId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const version = parseMcpDisplayString(record.version);
  const manifestDigest = parseMcpIdentifierString(record.manifestDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const name = parseMcpDisplayString(record.name);
  const description = parseMcpDisplayString(record.description, { allowEmpty: true });
  const publisherId = parseMcpIdentifierString(record.publisherId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const publisherName = parseMcpDisplayString(record.publisherName);
  const trustTier = parseCatalogTrustTier(record.trustTier);
  const proof = parseCatalogSourceProof(record.proof);
  const compatibility = parseCatalogCompatibility(record.compatibility);
  const distribution = parseCatalogDistribution(record.distribution);
  const verifiedAtMs = parseMcpInteger(record.verifiedAtMs);
  const eligibility = parseCatalogEligibility(record.eligibility);
  if (
    sourceId === null ||
    mcpId === null ||
    version === null ||
    manifestDigest === null ||
    name === null ||
    description === null ||
    publisherId === null ||
    publisherName === null ||
    trustTier === null ||
    proof === null ||
    compatibility === null ||
    distribution === null ||
    verifiedAtMs === null ||
    eligibility === null
  ) {
    return null;
  }
  return {
    sourceId,
    mcpId,
    version,
    manifestDigest,
    name,
    description,
    publisherId,
    publisherName,
    trustTier,
    proof,
    compatibility,
    distribution,
    verifiedAtMs,
    eligibility,
  };
}

function parseMcpCatalogCacheMetadataValue(value: unknown): McpCatalogPage['cache'] | null {
  const record = readMcpObject(value, catalogCacheMetadataKeys);
  if (!record) return null;
  const offline = parseMcpBoolean(record.offline);
  const localPersistenceOnly = parseMcpBoolean(record.localPersistenceOnly);
  const newestVerifiedAtMs = parseMcpOptionalValue(record.newestVerifiedAtMs, parseMcpInteger);
  const freshness = parseMcpIdentifierString(record.freshness, {
    maxLength: maxMcpIdentifierLength,
  });
  const refreshState = parseMcpIdentifierString(record.refreshState, {
    maxLength: maxMcpIdentifierLength,
  });
  const recovery = parseMcpRecoverySuggestion(record.recovery);
  if (
    offline === null ||
    localPersistenceOnly === null ||
    newestVerifiedAtMs === null ||
    !freshness ||
    !catalogCacheFreshnessValues.has(freshness as McpCatalogPage['cache']['freshness']) ||
    !refreshState ||
    !catalogRefreshStateValues.has(refreshState as McpCatalogPage['cache']['refreshState']) ||
    recovery === null
  ) {
    return null;
  }
  return {
    offline,
    localPersistenceOnly,
    ...(newestVerifiedAtMs === undefined ? {} : { newestVerifiedAtMs }),
    freshness: freshness as McpCatalogPage['cache']['freshness'],
    refreshState: refreshState as McpCatalogPage['cache']['refreshState'],
    recovery,
  };
}

function parseMcpCatalogPageValue(value: unknown): McpCatalogPage | null {
  const record = readMcpObject(value, catalogPageKeys);
  if (!record) return null;
  const items = parseMcpArray(record.items, parseMcpCatalogSummaryValue);
  const nextCursor = parseMcpOptionalValue(record.nextCursor, (next) =>
    parseMcpContractString(next, maxMcpDisplayTextLength)
  );
  const cache = parseMcpCatalogCacheMetadataValue(record.cache);
  if (items === null || nextCursor === null || cache === null) return null;
  return {
    items,
    ...(nextCursor === undefined ? {} : { nextCursor }),
    cache,
  };
}

function parseMcpCatalogPublisherValue(value: unknown): McpCatalogDetail['publisher'] | null {
  const record = readMcpObject(value, catalogPublisherKeys);
  if (!record) return null;
  const id = parseMcpIdentifierString(record.id, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const name = parseMcpDisplayString(record.name);
  const website = parseMcpOptionalValue(record.website, (next) =>
    parseMcpContractString(next, maxMcpDisplayTextLength)
  );
  const signingIdentities = parseMcpArray(record.signingIdentities, (next) =>
    parseMcpContractString(next, maxMcpDisplayTextLength)
  );
  if (id === null || name === null || website === null || signingIdentities === null) {
    return null;
  }
  return {
    id,
    name,
    ...(website === undefined ? {} : { website }),
    signingIdentities,
  };
}

function parseMcpCatalogArtifactValue(value: unknown): McpCatalogArtifact | null {
  const record = readMcpObject(value, catalogArtifactKeys);
  if (!record) return null;
  const platform = parseMcpContractString(record.platform, maxMcpIdentifierLength);
  const architecture = parseMcpContractString(record.architecture, maxMcpIdentifierLength);
  const origin = parseMcpContractString(record.origin, maxMcpDisplayTextLength);
  const digest = parseMcpContractString(record.digest, maxMcpDisplayTextLength);
  const sizeBytes = parseMcpOptionalValue(record.sizeBytes, (next) =>
    parseMcpUnsignedInteger(next, Number.MAX_SAFE_INTEGER)
  );
  if (
    platform === null ||
    architecture === null ||
    origin === null ||
    digest === null ||
    sizeBytes === null
  ) {
    return null;
  }
  return {
    platform,
    architecture,
    origin,
    digest,
    ...(sizeBytes === undefined ? {} : { sizeBytes }),
  };
}

function parseMcpCatalogDistributionValue(value: unknown): McpCatalogDetail['distribution'] | null {
  const remoteHttp = readMcpObject(value, catalogRemoteHttpDistributionKeys);
  if (
    remoteHttp?.type === 'remote_http' &&
    hasExactMcpKeys(remoteHttp, catalogRemoteHttpDistributionKeys)
  ) {
    return { type: 'remote_http' };
  }

  const manualStdio = readMcpObject(value, catalogManualStdioDistributionKeys);
  if (
    manualStdio?.type === 'manual_stdio' &&
    hasExactMcpKeys(manualStdio, catalogManualStdioDistributionKeys)
  ) {
    const platforms = parseMcpArray(manualStdio.platforms, (next) =>
      parseMcpContractString(next, maxMcpIdentifierLength)
    );
    return platforms === null ? null : { type: 'manual_stdio', platforms };
  }

  const npm = readMcpObject(value, catalogNpmDistributionKeys);
  if (npm?.type === 'npm' && hasExactMcpKeys(npm, catalogNpmDistributionKeys)) {
    const packageName = parseMcpContractString(npm.package, maxMcpDisplayTextLength);
    const packageVersion = parseMcpContractString(npm.package_version, maxMcpIdentifierLength);
    const artifacts = parseMcpArray(npm.artifacts, parseMcpCatalogArtifactValue);
    return packageName === null || packageVersion === null || artifacts === null
      ? null
      : {
          type: 'npm',
          package: packageName,
          package_version: packageVersion,
          artifacts,
        };
  }

  const pythonWheel = readMcpObject(value, catalogPythonWheelDistributionKeys);
  if (
    pythonWheel?.type === 'python_wheel' &&
    hasExactMcpKeys(pythonWheel, catalogPythonWheelDistributionKeys)
  ) {
    const packageName = parseMcpContractString(pythonWheel.package, maxMcpDisplayTextLength);
    const packageVersion = parseMcpContractString(
      pythonWheel.package_version,
      maxMcpIdentifierLength
    );
    const python = parseMcpContractString(pythonWheel.python, maxMcpIdentifierLength);
    const artifacts = parseMcpArray(pythonWheel.artifacts, parseMcpCatalogArtifactValue);
    return packageName === null || packageVersion === null || python === null || artifacts === null
      ? null
      : {
          type: 'python_wheel',
          package: packageName,
          package_version: packageVersion,
          python,
          artifacts,
        };
  }

  const binaryArchive = readMcpObject(value, catalogBinaryArchiveDistributionKeys);
  if (
    binaryArchive?.type === 'binary_archive' &&
    hasExactMcpKeys(binaryArchive, catalogBinaryArchiveDistributionKeys)
  ) {
    const archiveFormat = parseMcpContractString(
      binaryArchive.archive_format,
      maxMcpIdentifierLength
    );
    const artifacts = parseMcpArray(binaryArchive.artifacts, parseMcpCatalogArtifactValue);
    return archiveFormat === null || artifacts === null
      ? null
      : { type: 'binary_archive', archive_format: archiveFormat, artifacts };
  }

  const docker = readMcpObject(value, catalogDockerDistributionKeys);
  if (docker?.type === 'docker' && hasExactMcpKeys(docker, catalogDockerDistributionKeys)) {
    const image = parseMcpContractString(docker.image, maxMcpDisplayTextLength);
    const digest = parseMcpContractString(docker.digest, maxMcpDisplayTextLength);
    return image === null || digest === null ? null : { type: 'docker', image, digest };
  }

  const gitDev = readMcpObject(value, catalogGitDevDistributionKeys);
  if (gitDev?.type === 'git_dev' && hasExactMcpKeys(gitDev, catalogGitDevDistributionKeys)) {
    const repository = parseMcpContractString(gitDev.repository, maxMcpDisplayTextLength);
    const commit = parseMcpContractString(gitDev.commit, maxMcpIdentifierLength);
    const adapter = parseMcpContractString(gitDev.adapter, maxMcpIdentifierLength);
    return repository === null || commit === null || adapter === null
      ? null
      : { type: 'git_dev', repository, commit, adapter };
  }

  return null;
}

function parseMcpCatalogTransportValue(value: unknown): McpCatalogDetail['transport'] | null {
  const stdio = readMcpObject(value, catalogStdioTransportKeys);
  if (stdio?.type === 'stdio') {
    const startupTimeoutSeconds = parseMcpOptionalValue(stdio.startup_timeout_seconds, (next) =>
      parseMcpUnsignedInteger(next, Number.MAX_SAFE_INTEGER)
    );
    return startupTimeoutSeconds === null
      ? null
      : {
          type: 'stdio',
          ...(startupTimeoutSeconds === undefined
            ? {}
            : { startup_timeout_seconds: startupTimeoutSeconds }),
        };
  }

  const streamableHttp = readMcpObject(value, catalogStreamableHttpTransportKeys);
  if (streamableHttp?.type !== 'streamable_http') return null;
  const endpoint = parseMcpContractString(streamableHttp.endpoint, maxMcpDisplayTextLength);
  const connectTimeoutSeconds = parseMcpOptionalValue(
    streamableHttp.connect_timeout_seconds,
    (next) => parseMcpUnsignedInteger(next, Number.MAX_SAFE_INTEGER)
  );
  const allowedRedirectOrigins = parseMcpArray(streamableHttp.allowed_redirect_origins, (next) =>
    parseMcpContractString(next, maxMcpDisplayTextLength)
  );
  if (endpoint === null || connectTimeoutSeconds === null || allowedRedirectOrigins === null) {
    return null;
  }
  return {
    type: 'streamable_http',
    endpoint,
    ...(connectTimeoutSeconds === undefined
      ? {}
      : { connect_timeout_seconds: connectTimeoutSeconds }),
    allowed_redirect_origins: allowedRedirectOrigins,
  };
}

function parseMcpCatalogAuthValue(value: unknown): McpCatalogDetail['auth'] | null {
  const none = readMcpObject(value, catalogNoneAuthKeys);
  if (none?.type === 'none' && hasExactMcpKeys(none, catalogNoneAuthKeys)) {
    return { type: 'none' };
  }

  const apiKeyHeader = readMcpObject(value, catalogApiKeyHeaderAuthKeys);
  if (
    apiKeyHeader?.type === 'api_key_header' &&
    hasExactMcpKeys(apiKeyHeader, catalogApiKeyHeaderAuthKeys)
  ) {
    const credentialRequired = parseMcpBoolean(apiKeyHeader.credential_required);
    const prefixRequired = parseMcpBoolean(apiKeyHeader.prefix_required);
    return credentialRequired === null || prefixRequired === null
      ? null
      : {
          type: 'api_key_header',
          credential_required: credentialRequired,
          prefix_required: prefixRequired,
        };
  }

  const environment = readMcpObject(value, catalogEnvironmentAuthKeys);
  if (
    environment?.type === 'environment' &&
    hasExactMcpKeys(environment, catalogEnvironmentAuthKeys)
  ) {
    const credentialRequired = parseMcpBoolean(environment.credential_required);
    return credentialRequired === null
      ? null
      : { type: 'environment', credential_required: credentialRequired };
  }

  const oauth2 = readMcpObject(value, catalogOauth2AuthKeys);
  if (oauth2?.type !== 'oauth2' || !hasExactMcpKeys(oauth2, catalogOauth2AuthKeys)) {
    return null;
  }
  const authorizationEndpoint = parseMcpContractString(
    oauth2.authorization_endpoint,
    maxMcpDisplayTextLength
  );
  const tokenEndpoint = parseMcpContractString(oauth2.token_endpoint, maxMcpDisplayTextLength);
  const clientRegistration = parseMcpContractString(
    oauth2.client_registration,
    maxMcpDisplayTextLength
  );
  const scopes = parseMcpArray(oauth2.scopes, (next) =>
    parseMcpContractString(next, maxMcpIdentifierLength)
  );
  return authorizationEndpoint === null ||
    tokenEndpoint === null ||
    clientRegistration === null ||
    scopes === null
    ? null
    : {
        type: 'oauth2',
        authorization_endpoint: authorizationEndpoint,
        token_endpoint: tokenEndpoint,
        client_registration: clientRegistration,
        scopes,
      };
}

function parseMcpCatalogHealthValue(value: unknown): McpCatalogDetail['health'] | null {
  const initialize = readMcpObject(value, catalogMcpInitializeHealthKeys);
  if (
    initialize?.type === 'mcp_initialize' &&
    hasExactMcpKeys(initialize, catalogMcpInitializeHealthKeys)
  ) {
    const timeoutSeconds = parseMcpUnsignedInteger(
      initialize.timeout_seconds,
      Number.MAX_SAFE_INTEGER
    );
    return timeoutSeconds === null
      ? null
      : { type: 'mcp_initialize', timeout_seconds: timeoutSeconds };
  }

  const listTools = readMcpObject(value, catalogMcpListToolsHealthKeys);
  if (
    listTools?.type === 'mcp_list_tools' &&
    hasExactMcpKeys(listTools, catalogMcpListToolsHealthKeys)
  ) {
    const timeoutSeconds = parseMcpUnsignedInteger(
      listTools.timeout_seconds,
      Number.MAX_SAFE_INTEGER
    );
    return timeoutSeconds === null
      ? null
      : { type: 'mcp_list_tools', timeout_seconds: timeoutSeconds };
  }

  const http = readMcpObject(value, catalogHttpHealthKeys);
  if (http?.type !== 'http' || !hasExactMcpKeys(http, catalogHttpHealthKeys)) return null;
  const relativeEndpoint = parseMcpContractString(http.relative_endpoint, maxMcpDisplayTextLength);
  const expectedStatus = parseMcpUnsignedInteger(http.expected_status, 0xffff);
  const timeoutSeconds = parseMcpUnsignedInteger(http.timeout_seconds, Number.MAX_SAFE_INTEGER);
  return relativeEndpoint === null || expectedStatus === null || timeoutSeconds === null
    ? null
    : {
        type: 'http',
        relative_endpoint: relativeEndpoint,
        expected_status: expectedStatus,
        timeout_seconds: timeoutSeconds,
      };
}

function parseMcpCatalogCapabilityValue(
  value: unknown
): McpCatalogDetail['capabilities'][number] | null {
  const capability = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return capability &&
    catalogCapabilityValues.has(capability as McpCatalogDetail['capabilities'][number])
    ? (capability as McpCatalogDetail['capabilities'][number])
    : null;
}

function parseMcpCatalogPermissionValue(
  value: unknown
): McpCatalogDetail['permissions'][number] | null {
  const record = readMcpObject(value, catalogPermissionKeys);
  if (!record) return null;
  const id = parseMcpIdentifierString(record.id, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const kind = parseMcpIdentifierString(record.kind, { maxLength: maxMcpIdentifierLength });
  const reason = parseMcpDisplayString(record.reason);
  const required = parseMcpBoolean(record.required);
  const scope = parseMcpOptionalValue(record.scope, (next) =>
    parseMcpDisplayString(next, { allowEmpty: true })
  );
  if (
    id === null ||
    !kind ||
    !catalogPermissionKindValues.has(kind as McpCatalogDetail['permissions'][number]['kind']) ||
    reason === null ||
    required === null ||
    scope === null
  ) {
    return null;
  }
  return {
    id,
    kind: kind as McpCatalogDetail['permissions'][number]['kind'],
    reason,
    required,
    ...(scope === undefined ? {} : { scope }),
  };
}

function parseMcpCatalogDetailValue(value: unknown): McpCatalogDetail | null {
  const record = readMcpObject(value, catalogDetailKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const mcpId = parseMcpIdentifierString(record.mcpId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const version = parseMcpDisplayString(record.version);
  const manifestDigest = parseMcpIdentifierString(record.manifestDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const name = parseMcpDisplayString(record.name);
  const description = parseMcpDisplayString(record.description, { allowEmpty: true });
  const publisher = parseMcpCatalogPublisherValue(record.publisher);
  const proof = parseCatalogSourceProof(record.proof);
  const trustTier = parseCatalogTrustTier(record.trustTier);
  const distribution = parseMcpCatalogDistributionValue(record.distribution);
  const transport = parseMcpCatalogTransportValue(record.transport);
  const auth = parseMcpCatalogAuthValue(record.auth);
  const health = parseMcpCatalogHealthValue(record.health);
  const capabilities = parseMcpArray(record.capabilities, parseMcpCatalogCapabilityValue);
  const permissions = parseMcpArray(record.permissions, parseMcpCatalogPermissionValue);
  const compatibility = parseCatalogCompatibility(record.compatibility);
  const verifiedAtMs = parseMcpInteger(record.verifiedAtMs);
  const eligibility = parseCatalogEligibility(record.eligibility);
  if (
    sourceId === null ||
    mcpId === null ||
    version === null ||
    manifestDigest === null ||
    name === null ||
    description === null ||
    publisher === null ||
    proof === null ||
    trustTier === null ||
    distribution === null ||
    transport === null ||
    auth === null ||
    health === null ||
    capabilities === null ||
    permissions === null ||
    compatibility === null ||
    verifiedAtMs === null ||
    eligibility === null
  ) {
    return null;
  }
  return {
    sourceId,
    mcpId,
    version,
    manifestDigest,
    name,
    description,
    publisher,
    proof,
    trustTier,
    distribution,
    transport,
    auth,
    health,
    capabilities,
    permissions,
    compatibility,
    verifiedAtMs,
    eligibility,
  };
}

function parseMcpSourcePolicyCompatibilityValue(
  value: unknown
): McpSourcesPolicyState['sources'][number]['compatibility'] | null {
  const record = readMcpObject(value, sourcePolicyCompatibilityKeys);
  if (!record || !hasExactMcpKeys(record, sourcePolicyCompatibilityKeys)) return null;
  const compatible = parseMcpUnsignedInteger(record.compatible, 0xffff_ffff);
  const restricted = parseMcpUnsignedInteger(record.restricted, 0xffff_ffff);
  const denied = parseMcpUnsignedInteger(record.denied, 0xffff_ffff);
  return compatible === null || restricted === null || denied === null
    ? null
    : { compatible, restricted, denied };
}

function parseMcpSourcePolicySourceValue(
  value: unknown
): McpSourcesPolicyState['sources'][number] | null {
  const record = readMcpObject(value, sourcePolicySourceKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const displayName = parseMcpDisplayString(record.displayName);
  const importKind = parseGovernedImportKind(record.importKind);
  const trustTiers = parseMcpArray(record.trustTiers, parseCatalogTrustTier);
  const manifestCount = parseMcpUnsignedInteger(record.manifestCount, 0xffff_ffff);
  const lastImportedAtMs = parseMcpOptionalValue(record.lastImportedAtMs, parseMcpInteger);
  const cache = parseMcpCatalogCacheMetadataValue(record.cache);
  const compatibility = parseMcpSourcePolicyCompatibilityValue(record.compatibility);
  const recovery = parseMcpRecoverySuggestion(record.recovery);
  const lastRefreshDocumentDigest = parseMcpOptionalValue(
    record.lastRefreshDocumentDigest,
    (next) => parseMcpContractString(next, maxMcpDisplayTextLength)
  );
  const lastRefreshedAtMs = parseMcpOptionalValue(record.lastRefreshedAtMs, parseMcpInteger);
  if (
    sourceId === null ||
    displayName === null ||
    importKind === null ||
    trustTiers === null ||
    manifestCount === null ||
    lastImportedAtMs === null ||
    cache === null ||
    compatibility === null ||
    recovery === null ||
    lastRefreshDocumentDigest === null ||
    lastRefreshedAtMs === null
  ) {
    return null;
  }
  return {
    sourceId,
    displayName,
    importKind,
    trustTiers,
    manifestCount,
    ...(lastImportedAtMs === undefined ? {} : { lastImportedAtMs }),
    cache,
    compatibility,
    recovery,
    ...(lastRefreshDocumentDigest === undefined ? {} : { lastRefreshDocumentDigest }),
    ...(lastRefreshedAtMs === undefined ? {} : { lastRefreshedAtMs }),
  };
}

function parseMcpSourcesPolicyStateValue(value: unknown): McpSourcesPolicyState | null {
  const record = readMcpObject(value, sourcesPolicyStateKeys);
  if (!record || !hasExactMcpKeys(record, sourcesPolicyStateKeys)) return null;
  const sources = parseMcpArray(record.sources, parseMcpSourcePolicySourceValue);
  const policy = readMcpObject(record.policy, sourcePolicyMachineKeys);
  if (!policy || !hasExactMcpKeys(policy, sourcePolicyMachineKeys)) return null;
  const targetPlatform = parseMcpContractString(policy.targetPlatform, maxMcpIdentifierLength);
  const targetArchitecture = parseMcpContractString(
    policy.targetArchitecture,
    maxMcpIdentifierLength
  );
  const developmentMode = parseMcpBoolean(policy.developmentMode);
  const dockerAllowed = parseMcpBoolean(policy.dockerAllowed);
  const recovery = parseMcpRecoverySuggestion(policy.recovery);
  if (
    sources === null ||
    targetPlatform === null ||
    targetArchitecture === null ||
    developmentMode === null ||
    dockerAllowed === null ||
    recovery === null
  ) {
    return null;
  }
  return {
    sources,
    policy: {
      targetPlatform,
      targetArchitecture,
      developmentMode,
      dockerAllowed,
      recovery,
    },
  };
}

function parseGovernedImportKind(value: unknown): McpGovernedImportKind | null {
  const kind = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return kind && governedImportKindValues.has(kind as McpGovernedImportKind)
    ? (kind as McpGovernedImportKind)
    : null;
}

function parseGovernedImportDocumentKind(value: unknown): McpGovernedCatalogDocumentKind | null {
  const kind = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return kind && governedImportDocumentKindValues.has(kind as McpGovernedCatalogDocumentKind)
    ? (kind as McpGovernedCatalogDocumentKind)
    : null;
}

function parseSourceProvisionRefreshTransport(
  value: unknown
): McpSourceProvisionRefreshTransport | null {
  const transport = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return transport &&
    sourceProvisionRefreshTransportValues.has(transport as McpSourceProvisionRefreshTransport)
    ? (transport as McpSourceProvisionRefreshTransport)
    : null;
}

function parseSourceProvisionTrustBasis(value: unknown): McpSourceProvisionTrustBasis | null {
  const trustBasis = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return trustBasis &&
    sourceProvisionTrustBasisValues.has(trustBasis as McpSourceProvisionTrustBasis)
    ? (trustBasis as McpSourceProvisionTrustBasis)
    : null;
}

function parseMcpProfileEntry(value: unknown): McpProfileEntry | null {
  const record = readMcpObject(value, profileEntryKeys);
  if (!record) return null;
  const managedMcpId = parseMcpProfileOpaqueIdString(record.managedMcpId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const ordinal = parseMcpInteger(record.ordinal);
  if (managedMcpId === null || ordinal === null) return null;
  return { managedMcpId, ordinal };
}

function parseMcpProfileSummaryValue(value: unknown): McpProfileSummary | null {
  const record = readMcpObject(value, profileSummaryKeys);
  if (!record) return null;
  const profileId = parseMcpProfileOpaqueIdString(record.profileId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const name = parseMcpDisplayString(record.name);
  const description = parseMcpDisplayString(record.description, { allowEmpty: true });
  const revision = parseMcpInteger(record.revision);
  const archived = parseMcpBoolean(record.archived);
  const entries = parseMcpArray(record.entries, parseMcpProfileEntry);
  const createdAtMs = parseMcpInteger(record.createdAtMs);
  const updatedAtMs = parseMcpInteger(record.updatedAtMs);
  const credentialStatusValue =
    record.credentialStatus === undefined
      ? undefined
      : parseMcpCredentialStatus(record.credentialStatus);
  if (
    profileId === null ||
    name === null ||
    description === null ||
    revision === null ||
    archived === null ||
    entries === null ||
    createdAtMs === null ||
    updatedAtMs === null ||
    (record.credentialStatus !== undefined && credentialStatusValue === null)
  ) {
    return null;
  }
  const credentialStatus = credentialStatusValue ?? undefined;
  return {
    profileId,
    name,
    description,
    revision,
    archived,
    entries,
    createdAtMs,
    updatedAtMs,
    ...(credentialStatus === undefined ? {} : { credentialStatus }),
  };
}

function parseMcpProfileRevisionSummaryValue(value: unknown): McpProfileRevisionSummary | null {
  const record = readMcpObject(value, profileRevisionKeys);
  if (!record) return null;
  const revision = parseMcpInteger(record.revision);
  const operation = parseMcpDisplayString(record.operation, {
    maxLength: maxMcpIdentifierLength,
  });
  const actor = parseMcpDisplayString(record.actor, { maxLength: maxMcpIdentifierLength });
  const createdAtMs = parseMcpInteger(record.createdAtMs);
  if (revision === null || operation === null || actor === null || createdAtMs === null) {
    return null;
  }
  return { revision, operation, actor, createdAtMs };
}

function parseMcpProfilePageValue(value: unknown): McpProfilePage | null {
  const record = readMcpObject(value, profilePageKeys);
  if (!record) return null;
  const items = parseMcpArray(record.items, parseMcpProfileSummaryValue);
  return items ? { items } : null;
}

function parseMcpProfileDetailValue(value: unknown): McpProfileDetail | null {
  const record = readMcpObject(value, profileDetailKeys);
  if (!record) return null;
  const profile = parseMcpProfileSummaryValue(record.profile);
  const history = parseMcpArray(record.history, parseMcpProfileRevisionSummaryValue);
  if (!profile || !history) return null;
  return { profile, history };
}

function parseMcpProfileApplyEntryValue(value: unknown): McpProfileApplyEntry | null {
  const record = readMcpObject(value, profileApplyEntryKeys);
  if (!record) return null;
  const managedMcpId = parseMcpProfileOpaqueIdString(record.managedMcpId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const mcpIdValue =
    record.mcpId === undefined
      ? undefined
      : parseMcpIdentifierString(record.mcpId, { maxLength: maxMcpIdentifierLength });
  const nameValue =
    record.name === undefined
      ? undefined
      : parseMcpDisplayString(record.name, { allowEmpty: true });
  const versionValue =
    record.version === undefined
      ? undefined
      : parseMcpDisplayString(record.version, { allowEmpty: true });
  const health = parseMcpIdentifierString(record.health, {
    maxLength: maxMcpIdentifierLength,
  });
  const authReady = parseMcpBoolean(record.authReady);
  const policyReady = parseMcpBoolean(record.policyReady);
  const readiness = parseMcpIdentifierString(record.readiness, {
    maxLength: maxMcpIdentifierLength,
  });
  if (
    managedMcpId === null ||
    health === null ||
    authReady === null ||
    policyReady === null ||
    readiness === null ||
    (record.mcpId !== undefined && mcpIdValue === null) ||
    (record.name !== undefined && nameValue === null) ||
    (record.version !== undefined && versionValue === null)
  ) {
    return null;
  }
  const mcpId = mcpIdValue ?? undefined;
  const name = nameValue ?? undefined;
  const version = versionValue ?? undefined;
  return {
    managedMcpId,
    health,
    authReady,
    policyReady,
    readiness,
    ...(mcpId === undefined ? {} : { mcpId }),
    ...(name === undefined ? {} : { name }),
    ...(version === undefined ? {} : { version }),
  };
}

function parseMcpProfileApplyConfirmationValue(value: unknown): McpProfileApplyConfirmation | null {
  const record = readMcpObject(value, profileApplyConfirmationKeys);
  if (!record) return null;
  const confirmationToken = parseMcpProfileOpaqueIdString(record.confirmationToken, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  return confirmationToken ? { confirmationToken } : null;
}

function parseMcpProfileApplyPlanValue(value: unknown): McpProfileApplyPlan | null {
  const record = readMcpObject(value, profileApplyPlanKeys);
  if (!record) return null;
  const planId = parseMcpProfileOpaqueIdString(record.planId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const profileId = parseMcpProfileOpaqueIdString(record.profileId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const profileRevision = parseMcpInteger(record.profileRevision);
  const mergePolicy = parseMcpIdentifierString(record.mergePolicy, {
    maxLength: maxMcpIdentifierLength,
  });
  const entries = parseMcpArray(record.entries, parseMcpProfileApplyEntryValue);
  const expiresAtMs = parseMcpInteger(record.expiresAtMs);
  const confirmation = parseMcpProfileApplyConfirmationValue(record.confirmation);
  if (
    planId === null ||
    profileId === null ||
    profileRevision === null ||
    mergePolicy === null ||
    entries === null ||
    expiresAtMs === null ||
    confirmation === null
  ) {
    return null;
  }
  return {
    planId,
    profileId,
    profileRevision,
    mergePolicy,
    entries,
    expiresAtMs,
    confirmation,
  };
}

function parseMcpProfileApplicationTokenValue(value: unknown): McpProfileApplicationToken | null {
  const record = readMcpObject(value, profileApplicationTokenKeys);
  if (!record) return null;
  const token = parseMcpProfileOpaqueIdString(record.token, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const planId = parseMcpProfileOpaqueIdString(record.planId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const profileId = parseMcpProfileOpaqueIdString(record.profileId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const profileRevision = parseMcpInteger(record.profileRevision);
  const expiresAtMs = parseMcpInteger(record.expiresAtMs);
  if (
    token === null ||
    planId === null ||
    profileId === null ||
    profileRevision === null ||
    expiresAtMs === null
  ) {
    return null;
  }
  return { token, planId, profileId, profileRevision, expiresAtMs };
}

function parseMcpConnectionTestPhase(value: unknown): McpConnectionTestPhase | null {
  const phase = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return phase && mcpConnectionTestPhaseValues.has(phase as McpConnectionTestPhase)
    ? (phase as McpConnectionTestPhase)
    : null;
}

function parseMcpConnectionTestStatus(value: unknown): McpConnectionTestStatus | null {
  const status = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return status && mcpConnectionTestStatusValues.has(status as McpConnectionTestStatus)
    ? (status as McpConnectionTestStatus)
    : null;
}

function parseMcpConnectionTestCode(value: unknown): McpConnectionTestCode | null {
  const code = parseMcpIdentifierString(value, { maxLength: maxMcpIdentifierLength });
  return code && mcpConnectionTestCodeValues.has(code as McpConnectionTestCode)
    ? (code as McpConnectionTestCode)
    : null;
}

function parseMcpConnectionTestStage(
  value: unknown
): McpProfileConnectionTestResult['stages'][number] | null {
  const record = readMcpObject(value, connectionTestStageKeys);
  if (!record) return null;
  const phase = parseMcpConnectionTestPhase(record.phase);
  const status = parseMcpConnectionTestStatus(record.status);
  const code = parseMcpConnectionTestCode(record.code);
  if (phase === null || status === null || code === null) return null;
  return { phase, status, code };
}

function parseMcpProfileConnectionTestResultValue(
  value: unknown
): McpProfileConnectionTestResult | null {
  const record = readMcpObject(value, profileConnectionTestResultKeys);
  if (!record) return null;
  const passed = parseMcpBoolean(record.passed);
  const stages = parseMcpArray(record.stages, parseMcpConnectionTestStage);
  if (passed === null || stages === null || stages.length === 0) return null;
  const hasFailedStage = stages.some((stage) => stage.status === 'failed');
  if (passed === hasFailedStage) return null;
  return { passed, stages };
}

function parseMcpProfileDraftCandidateValue(value: unknown): McpProfileDraftCandidate | null {
  const record = readMcpObject(value, profileDraftCandidateKeys);
  if (!record) return null;
  const managedMcpId = parseMcpProfileOpaqueIdString(record.managedMcpId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const mcpId = parseMcpIdentifierString(record.mcpId, { maxLength: maxMcpIdentifierLength });
  const name = parseMcpDisplayString(record.name);
  const description = parseMcpDisplayString(record.description, { allowEmpty: true });
  const confidence = parseMcpNumber(record.confidence);
  const reasonCode = parseMcpIdentifierString(record.reasonCode, {
    maxLength: maxMcpIdentifierLength,
  });
  if (
    managedMcpId === null ||
    mcpId === null ||
    name === null ||
    description === null ||
    confidence === null ||
    confidence < 0 ||
    confidence > 1 ||
    reasonCode === null
  ) {
    return null;
  }
  return {
    managedMcpId,
    mcpId,
    name,
    description,
    confidence,
    reasonCode,
  };
}

function parseMcpProfileDraftValue(value: unknown): McpProfileDraft | null {
  const record = readMcpObject(value, profileDraftKeys);
  if (!record) return null;
  const locale = parseMcpDraftLocaleString(record.locale);
  const name = parseMcpDisplayString(record.name);
  const description = parseMcpDisplayString(record.description, { allowEmpty: true });
  const entries = parseMcpArray(record.entries, parseMcpProfileEntry);
  const candidates = parseMcpArray(record.candidates, parseMcpProfileDraftCandidateValue);
  const unresolvedTerms = parseMcpDisplayStringArray(record.unresolvedTerms);
  const lowConfidence = parseMcpBoolean(record.lowConfidence);
  const persisted = parseMcpBoolean(record.persisted);
  if (
    locale === null ||
    name === null ||
    description === null ||
    entries === null ||
    candidates === null ||
    unresolvedTerms === null ||
    lowConfidence === null ||
    persisted === null
  ) {
    return null;
  }
  return {
    locale,
    name,
    description,
    entries,
    candidates,
    unresolvedTerms,
    lowConfidence,
    persisted,
  };
}

function parseMcpModelRecommendationCandidateValue(
  value: unknown
): McpModelRecommendationCandidate | null {
  const record = readMcpObject(value, modelRecommendationCandidateKeys);
  if (!record) return null;
  const providerId = parseMcpIdentifierString(record.providerId, {
    maxLength: maxMcpIdentifierLength,
  });
  const modelId = parseMcpIdentifierString(record.modelId, {
    maxLength: maxMcpIdentifierLength,
  });
  const confidence = parseMcpNumber(record.confidence);
  const reasonCodes = parseMcpIdentifierArray(record.reasonCodes);
  const caveatCodes = parseMcpIdentifierArray(record.caveatCodes);
  if (
    providerId === null ||
    modelId === null ||
    confidence === null ||
    confidence < 0 ||
    confidence > 1 ||
    reasonCodes === null ||
    caveatCodes === null
  ) {
    return null;
  }
  return { providerId, modelId, confidence, reasonCodes, caveatCodes };
}

function parseMcpModelRecommendationValue(value: unknown): McpModelRecommendation | null {
  const record = readMcpObject(value, modelRecommendationKeys);
  if (!record) return null;
  const candidates = parseMcpArray(record.candidates, parseMcpModelRecommendationCandidateValue);
  const inventoryOnly = parseMcpBoolean(record.inventoryOnly);
  const mutated = parseMcpBoolean(record.mutated);
  if (!candidates || inventoryOnly === null || mutated === null) return null;
  return { candidates, inventoryOnly, mutated };
}

function parseMcpGovernedImportSourceSummaryValue(
  value: unknown
): McpGovernedCatalogSourceSummary | null {
  const record = readMcpObject(value, governedImportSourceSummaryKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const importKind = parseGovernedImportKind(record.importKind);
  const displayName = parseMcpDisplayString(record.displayName);
  if (sourceId === null || importKind === null || displayName === null) {
    return null;
  }
  return { sourceId, importKind, displayName };
}

function parseMcpGovernedImportDocumentSummaryValue(
  value: unknown
): McpGovernedCatalogDocumentSummary | null {
  const record = readMcpObject(value, governedImportDocumentSummaryKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const documentId = parseMcpIdentifierString(record.documentId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const documentDigest = parseMcpIdentifierString(record.documentDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const documentKind = parseGovernedImportDocumentKind(record.documentKind);
  if (
    sourceId === null ||
    documentId === null ||
    documentDigest === null ||
    documentKind === null
  ) {
    return null;
  }
  return { sourceId, documentId, documentDigest, documentKind };
}

function parseMcpGovernedImportResultValue(value: unknown): McpGovernedImportResult | null {
  const record = readMcpObject(value, governedImportResultKeys);
  if (!record) return null;
  const source = parseMcpGovernedImportSourceSummaryValue(record.source);
  const document = parseMcpGovernedImportDocumentSummaryValue(record.document);
  const entries = parseMcpArray(record.entries, parseMcpCatalogSummaryValue);
  if (source === null || document === null || entries === null) {
    return null;
  }
  return { source, document, entries };
}

function parseMcpSourceProvisionPreviewValue(value: unknown): McpSourceProvisionPreview | null {
  const record = readMcpObject(value, sourceProvisionPreviewKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const displayName = parseMcpDisplayString(record.displayName);
  const rootDigest = parseMcpIdentifierString(record.rootDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const documentDigest = parseMcpIdentifierString(record.documentDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const endpointHost = parseMcpDisplayString(record.endpointHost, {
    maxLength: maxMcpIdentifierLength,
  });
  const refreshTransport = parseSourceProvisionRefreshTransport(record.refreshTransport);
  const manifestCount = parseMcpInteger(record.manifestCount);
  const warnings = parseMcpArray(
    record.warnings,
    (warning) => parseMcpDisplayString(warning, { maxLength: maxMcpDisplayTextLength }),
    32
  );
  const trustBasis = parseSourceProvisionTrustBasis(record.trustBasis);
  if (
    sourceId === null ||
    displayName === null ||
    rootDigest === null ||
    documentDigest === null ||
    endpointHost === null ||
    refreshTransport === null ||
    manifestCount === null ||
    manifestCount > 0xffff_ffff ||
    warnings === null ||
    trustBasis === null
  ) {
    return null;
  }
  return {
    sourceId,
    displayName,
    rootDigest,
    documentDigest,
    endpointHost,
    refreshTransport,
    manifestCount,
    warnings,
    trustBasis,
  };
}

function parseMcpSourceProvisionId(value: unknown): string | null {
  const provisionId = parseMcpIdentifierString(value, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  return provisionId?.startsWith('source_provisioning_') ? provisionId : null;
}

function parseMcpSourceProvisionConfirmationToken(value: unknown): string | null {
  const confirmationToken = parseMcpIdentifierString(value, {
    maxLength: 512,
    rejectTrimmedBlank: true,
  });
  return confirmationToken?.startsWith('source_provisioning_confirmation_')
    ? confirmationToken
    : null;
}

function parseMcpSourceProvisionPrepareResultValue(
  value: unknown
): McpSourceProvisionPrepareResult | null {
  const record = readMcpObject(value, sourceProvisionPrepareResultKeys);
  if (!record) return null;
  const provisionId = parseMcpSourceProvisionId(record.provisionId);
  const confirmationToken = parseMcpSourceProvisionConfirmationToken(record.confirmationToken);
  const expiresAtMs = parseMcpInteger(record.expiresAtMs);
  const preview = parseMcpSourceProvisionPreviewValue(record.preview);
  if (
    provisionId === null ||
    confirmationToken === null ||
    expiresAtMs === null ||
    preview === null
  ) {
    return null;
  }
  return { provisionId, confirmationToken, expiresAtMs, preview };
}

function parseMcpSourceProvisionConfirmResultValue(
  value: unknown
): McpSourceProvisionConfirmResult | null {
  const record = readMcpObject(value, sourceProvisionConfirmResultKeys);
  if (!record) return null;
  const provisionId = parseMcpSourceProvisionId(record.provisionId);
  const confirmedAtMs = parseMcpInteger(record.confirmedAtMs);
  const preview = parseMcpSourceProvisionPreviewValue(record.preview);
  if (provisionId === null || confirmedAtMs === null || preview === null) return null;
  return { provisionId, confirmedAtMs, preview };
}

const httpsManifestDigestPattern = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$/;

function parseHttpsManifestSafeString(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 && value.length <= 256 &&
    !/[\u0000-\u001f\u007f\u0080-\u009f]/.test(value) ? value : null;
}

function parseHttpsManifestDigest(value: unknown): string | null {
  return typeof value === 'string' && httpsManifestDigestPattern.test(value) ? value : null;
}

function parseMcpHttpsManifestPreviewValue(value: unknown): McpHttpsManifestPreview | null {
  const record = readMcpObject(value, httpsManifestPreviewKeys);
  if (!record) return null;
  const manifestId = parseHttpsManifestSafeString(record.manifestId);
  const version = parseHttpsManifestSafeString(record.version);
  const redactedOrigin = parseHttpsManifestSafeString(record.redactedOrigin);
  const digests = [
    record.rawDigest, record.parsedDigest, record.redirectChainDigest, record.dnsEvidenceDigest,
  ].map(parseHttpsManifestDigest);
  const warnings = record.warnings === undefined ? [] : parseMcpArray(
    record.warnings,
    (warning) => parseHttpsManifestSafeString(warning),
    32
  );
  if (manifestId === null || version === null || redactedOrigin === null ||
      digests.some((digest) => digest === null) || warnings === null) return null;
  if (!/^https:\/\/[^/?#]+$/i.test(redactedOrigin)) return null;
  return {
    manifestId, version, redactedOrigin,
    rawDigest: digests[0]!, parsedDigest: digests[1]!,
    redirectChainDigest: digests[2]!, dnsEvidenceDigest: digests[3]!, warnings,
  };
}

function parseMcpHttpsManifestPrepareResultValue(value: unknown): McpHttpsManifestPrepareResult | null {
  const record = readMcpObject(value, httpsManifestPrepareResultKeys);
  if (!record) return null;
  const provisionId = parseHttpsManifestSafeString(record.provisionId);
  const confirmationToken = parseHttpsManifestSafeString(record.confirmationToken);
  const expiresAtMs = parseMcpInteger(record.expiresAtMs);
  const preview = parseMcpHttpsManifestPreviewValue(record.preview);
  return provisionId && confirmationToken && expiresAtMs !== null && preview
    ? { provisionId, confirmationToken, expiresAtMs, preview } : null;
}

function parseMcpHttpsManifestConfirmResultValue(value: unknown): McpHttpsManifestConfirmResult | null {
  const record = readMcpObject(value, httpsManifestConfirmResultKeys);
  if (!record) return null;
  const manifestDigest = parseHttpsManifestDigest(record.manifestDigest);
  const preview = parseMcpHttpsManifestPreviewValue(record.preview);
  return manifestDigest && preview ? { manifestDigest, preview } : null;
}

function parseMcpSourceRefreshResultValue(value: unknown): McpSourceRefreshResult | null {
  const record = readMcpObject(value, sourceRefreshResultKeys);
  if (!record) return null;
  const sourceId = parseMcpIdentifierString(record.sourceId, {
    maxLength: maxMcpIdentifierLength,
    rejectTrimmedBlank: true,
  });
  const displayName = parseMcpDisplayString(record.displayName);
  const documentDigest = parseMcpIdentifierString(record.documentDigest, {
    maxLength: maxMcpDisplayTextLength,
    rejectTrimmedBlank: true,
  });
  const manifestCount = parseMcpInteger(record.manifestCount);
  const refreshedAtMs = parseMcpInteger(record.refreshedAtMs);
  if (
    sourceId === null ||
    displayName === null ||
    documentDigest === null ||
    manifestCount === null ||
    refreshedAtMs === null
  ) {
    return null;
  }
  return {
    sourceId,
    displayName,
    documentDigest,
    manifestCount,
    refreshedAtMs,
  };
}

function parseKnownMcpErrorDetails(
  code: McpPlatformErrorCode,
  value: unknown
): McpPlatformErrorEnvelope['details'] | undefined | null {
  if (value === undefined) {
    return allowedMissingDetailCodes.has(code) ? undefined : null;
  }
  const record = readMcpObject(value, knownMcpErrorDetailKeys);
  if (!record || typeof record.type !== 'string') return null;
  const parsed = parseMcpErrorDetailsByType(record);
  if (!parsed) return null;
  return allowedDetailTypesByCode[code].has(parsed.type) ? parsed : null;
}

function parseMcpErrorDetailsByType(value: McpObject): KnownMcpPlatformErrorDetails | null {
  switch (value.type) {
    case 'request_rejected': {
      return readMcpObject(value, requestRejectedDetailKeys) ? { type: 'request_rejected' } : null;
    }
    case 'record_missing': {
      return readMcpObject(value, recordMissingDetailKeys) ? { type: 'record_missing' } : null;
    }
    case 'integrity_validation_failed': {
      return readMcpObject(value, integrityValidationFailedDetailKeys)
        ? { type: 'integrity_validation_failed' }
        : null;
    }
    case 'policy_decision': {
      const record = readMcpObject(value, policyDecisionDetailKeys);
      if (!record) return null;
      const reason_codes = parseMcpIdentifierArray(record.reason_codes);
      return reason_codes ? { type: 'policy_decision', reason_codes } : null;
    }
    case 'phase_unavailable': {
      const record = readMcpObject(value, phaseUnavailableDetailKeys);
      if (!record) return null;
      const operation = parseMcpIdentifierString(record.operation, {
        maxLength: maxMcpIdentifierLength,
      });
      const phase = parseMcpIdentifierString(record.phase, {
        maxLength: maxMcpIdentifierLength,
      });
      if (operation === null || phase === null) return null;
      return { type: 'phase_unavailable', operation, phase };
    }
    case 'plan_mismatch': {
      return readMcpObject(value, planMismatchDetailKeys) ? { type: 'plan_mismatch' } : null;
    }
    case 'plan_expired': {
      return readMcpObject(value, planExpiredDetailKeys) ? { type: 'plan_expired' } : null;
    }
    case 'idempotency_conflict': {
      return readMcpObject(value, idempotencyConflictDetailKeys)
        ? { type: 'idempotency_conflict' }
        : null;
    }
    case 'revision_conflict': {
      return readMcpObject(value, revisionConflictDetailKeys)
        ? { type: 'revision_conflict' }
        : null;
    }
    case 'transition_rejected': {
      return readMcpObject(value, transitionRejectedDetailKeys)
        ? { type: 'transition_rejected' }
        : null;
    }
    case 'repository_temporarily_unavailable': {
      return readMcpObject(value, repositoryTemporarilyUnavailableDetailKeys)
        ? { type: 'repository_temporarily_unavailable' }
        : null;
    }
    case 'projection_conflict': {
      return readMcpObject(value, projectionConflictDetailKeys)
        ? { type: 'projection_conflict' }
        : null;
    }
    case 'credential_missing': {
      return readMcpObject(value, credentialMissingDetailKeys)
        ? { type: 'credential_missing' }
        : null;
    }
    case 'health_gate_failed': {
      return readMcpObject(value, healthGateFailedDetailKeys)
        ? { type: 'health_gate_failed' }
        : null;
    }
    case 'cancellation_deferred': {
      return readMcpObject(value, cancellationDeferredDetailKeys)
        ? { type: 'cancellation_deferred' }
        : null;
    }
    case 'recovery_required': {
      return readMcpObject(value, recoveryRequiredDetailKeys)
        ? { type: 'recovery_required' }
        : null;
    }
    case 'witness_expired': {
      return readMcpObject(value, recoveryRequiredDetailKeys) ? { type: 'witness_expired' } : null;
    }
    case 'witness_consumed': {
      return readMcpObject(value, recoveryRequiredDetailKeys) ? { type: 'witness_consumed' } : null;
    }
    case 'adapter_version_incompatible': {
      return readMcpObject(value, adapterVersionIncompatibleDetailKeys)
        ? { type: 'adapter_version_incompatible' }
        : null;
    }
    case 'external_capability_unavailable': {
      const record = readMcpObject(value, externalCapabilityUnavailableDetailKeys);
      if (!record) return null;
      const capability = parseMcpIdentifierString(record.capability, {
        maxLength: maxMcpIdentifierLength,
      });
      return capability ? { type: 'external_capability_unavailable', capability } : null;
    }
    case 'manual_stdio_provider_unavailable': {
      const record = readMcpObject(value, manualStdioProviderUnavailableDetailKeys);
      if (!record) return null;
      const recovery = parseMcpRecoverySuggestion(record.recovery);
      return recovery ? { type: 'manual_stdio_provider_unavailable', recovery } : null;
    }
    case 'remote_http_policy_unavailable': {
      const record = readMcpObject(value, remoteHttpPolicyUnavailableDetailKeys);
      if (!record) return null;
      const recovery = parseMcpRecoverySuggestion(record.recovery);
      return recovery ? { type: 'remote_http_policy_unavailable', recovery } : null;
    }
    case 'external_policy_denied': {
      const record = readMcpObject(value, externalPolicyDeniedDetailKeys);
      if (!record) return null;
      const capability = parseMcpIdentifierString(record.capability, {
        maxLength: maxMcpIdentifierLength,
      });
      return capability ? { type: 'external_policy_denied', capability } : null;
    }
    case 'supply_chain_mismatch': {
      const record = readMcpObject(value, supplyChainMismatchDetailKeys);
      if (!record) return null;
      const authority = parseMcpIdentifierString(record.authority, {
        maxLength: maxMcpIdentifierLength,
      });
      return authority ? { type: 'supply_chain_mismatch', authority } : null;
    }
    case 'authentication_required': {
      const record = readMcpObject(value, authenticationRequiredDetailKeys);
      if (!record) return null;
      const provider = parseMcpIdentifierString(record.provider, {
        maxLength: maxMcpIdentifierLength,
      });
      return provider ? { type: 'authentication_required', provider } : null;
    }
    case 'permission_grant_required': {
      const record = readMcpObject(value, permissionGrantRequiredDetailKeys);
      if (!record) return null;
      const permission = parseMcpIdentifierString(record.permission, {
        maxLength: maxMcpIdentifierLength,
      });
      return permission ? { type: 'permission_grant_required', permission } : null;
    }
    case 'origin_rejected': {
      const record = readMcpObject(value, originRejectedDetailKeys);
      if (!record) return null;
      const origin_type = parseMcpIdentifierString(record.origin_type, {
        maxLength: maxMcpIdentifierLength,
      });
      return origin_type ? { type: 'origin_rejected', origin_type } : null;
    }
    case 'immutable_commit_unavailable': {
      return readMcpObject(value, immutableCommitUnavailableDetailKeys)
        ? { type: 'immutable_commit_unavailable' }
        : null;
    }
    case 'repository_tree_rejected': {
      return readMcpObject(value, repositoryTreeRejectedDetailKeys)
        ? { type: 'repository_tree_rejected' }
        : null;
    }
    case 'development_mode_required': {
      return readMcpObject(value, developmentModeRequiredDetailKeys)
        ? { type: 'development_mode_required' }
        : null;
    }
    default:
      return null;
  }
}

function parseMcpErrorEnvelopeValue(value: unknown): McpPlatformErrorEnvelope | null {
  const record = readMcpObject(value, errorEnvelopeKeys);
  if (!record) return null;
  const code = parseMcpPlatformErrorCode(record.code);
  const message = parseMcpDisplayString(record.message, { allowEmpty: true });
  const retryable = parseMcpBoolean(record.retryable);
  const correlationId = parseMcpIdentifierString(record.correlationId, {
    rejectTrimmedBlank: true,
    maxLength: maxMcpIdentifierLength,
  });
  const details = code ? parseKnownMcpErrorDetails(code, record.details) : null;
  if (
    code === null ||
    message === null ||
    retryable === null ||
    correlationId === null ||
    details === null
  ) {
    return null;
  }
  return {
    code: code as McpPlatformErrorCode,
    message,
    retryable,
    correlationId,
    details,
  };
}

function parseMcpPlatformResponse<T>(
  response: unknown,
  parseValue: McpValueParser<T>
): McpPlatformOutcome<T> {
  const record = readMcpObject(response, profileResponseKeys);
  if (!record) throw createMalformedMcpPlatformResponseError();
  const outcome = readMcpObject(record.outcome, outcomeKeys);
  if (!outcome || typeof outcome.status !== 'string') {
    throw createMalformedMcpPlatformResponseError();
  }

  if (outcome.status === 'success') {
    const success = readMcpObject(record.outcome, successOutcomeKeys);
    if (!success || !hasExactMcpKeys(success, successOutcomeKeys)) {
      throw createMalformedMcpPlatformResponseError();
    }
    const sanitizedValue = sanitizeMcpJsonValue(success.value);
    if (sanitizedValue === invalidMcpWireValue) {
      throw createMalformedMcpPlatformResponseError();
    }
    const value = parseValue(sanitizedValue);
    if (value === null) throw createMalformedMcpPlatformResponseError();
    return { status: 'success', value };
  }

  if (outcome.status === 'error') {
    const failure = readMcpObject(record.outcome, errorOutcomeKeys);
    if (!failure || !hasExactMcpKeys(failure, errorOutcomeKeys)) {
      throw createMalformedMcpPlatformResponseError();
    }
    const error = parseMcpErrorEnvelopeValue(failure.error);
    if (!error) throw createMalformedMcpPlatformResponseError();
    return { status: 'error', error };
  }

  throw createMalformedMcpPlatformResponseError();
}

async function requestMcpPlatform<T>(
  method: string,
  parseValue: McpValueParser<T>,
  params: Record<string, unknown> = {}
): Promise<T> {
  const client = await getAcpClient();
  const response = await client.extMethod(method, params);
  return unwrapMcpOutcome(parseMcpPlatformResponse(response, parseValue));
}

type McpPhaseUnavailableTarget = 'profiles' | 'modelSuggestions';

const phaseUnavailableTargets: Record<
  McpPhaseUnavailableTarget,
  { phase: string; operations: Set<string> }
> = {
  profiles: {
    phase: '4B',
    operations: new Set([
      'goose.mcpProfileList_unstable',
      'goose.mcpProfileGet_unstable',
      'goose.mcpProfileCreate_unstable',
      'goose.mcpProfileUpdate_unstable',
      'goose.mcpProfileRestore_unstable',
      'goose.mcpProfileArchive_unstable',
      'goose.mcpProfileDraftCreate_unstable',
      'goose.mcpProfileApplyPlanCreate_unstable',
      'goose.mcpProfileApplyConfirm_unstable',
    ]),
  },
  modelSuggestions: {
    phase: '4B',
    operations: new Set(['goose.mcpProfileModelRecommend_unstable']),
  },
};

export function isMcpPhaseUnavailableError(
  error: unknown,
  target: McpPhaseUnavailableTarget
): boolean {
  if (!(error instanceof McpPlatformServiceError)) return false;
  if (error.envelope.code !== 'not_implemented_for_phase') {
    return false;
  }

  const details = error.envelope.details;
  if (!details || details.type !== 'phase_unavailable') return false;

  const expected = phaseUnavailableTargets[target];
  return expected.phase === details.phase && expected.operations.has(details.operation);
}

export async function listMcpCatalog(params: McpCatalogListRequest): Promise<McpCatalogPage> {
  return requestMcpPlatform('goose.mcpCatalogList_unstable', parseMcpCatalogPageValue, {
    ...params,
  });
}

export async function getMcpCatalogDetail(locator: McpCatalogRef): Promise<McpCatalogDetail>;
export async function getMcpCatalogDetail(manifestDigest: string): Promise<McpCatalogDetail>;
export async function getMcpCatalogDetail(
  locator: McpCatalogRef | string
): Promise<McpCatalogDetail> {
  const catalog =
    typeof locator === 'string'
      ? { type: 'manifest_digest' as const, manifest_digest: locator }
      : {
          type: 'catalog_ref' as const,
          source_id: locator.sourceId,
          mcp_id: locator.mcpId,
          version: locator.version,
        };
  const detail = await requestMcpPlatform(
    'goose.mcpCatalogDetail_unstable',
    parseMcpCatalogDetailValue,
    { catalog }
  );
  const matchesLocator =
    typeof locator === 'string'
      ? detail.manifestDigest === locator
      : detail.sourceId === locator.sourceId &&
        detail.mcpId === locator.mcpId &&
        detail.version === locator.version;
  if (!matchesLocator) throw createMalformedMcpPlatformResponseError();
  return detail;
}

export async function createMcpPlan(intent: McpPlanIntent): Promise<McpPlanReview> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpPlanCreate_unstable({
        intent,
        idempotencyKey: createMcpIdempotencyKey('plan'),
      })
    ).outcome
  );
}

export async function createManualMcpPlan(
  connection: McpManualConnectionInput
): Promise<McpPlanReview> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpManualPlanCreate_unstable({
        connection,
        idempotencyKey: createMcpIdempotencyKey('manual-plan'),
      })
    ).outcome
  );
}

export async function confirmMcpPlan(
  plan: Pick<McpPlanReview, 'planId' | 'planDigest'>,
  decision: McpInstallConfirmRequest['userDecision']
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  try {
    return unwrapMcpOutcome(
      (
        await client.mcpPlatform.mcpInstallConfirm_unstable({
          planId: plan.planId,
          planDigest: plan.planDigest,
          userDecision: decision,
          idempotencyKey: createMcpIdempotencyKey(`plan-${decision}`),
        })
      ).outcome
    );
  } catch (error) {
    if (error instanceof McpPlatformServiceError) throw error;
    throw createMcpConfirmTransportError();
  }
}

export async function listManagedMcps(cursor?: string): Promise<McpManagedPage> {
  const client = await getAcpClient();
  return unwrapManagedSdkOutcome(client.mcpPlatform.mcpList_unstable({ cursor, pageSize: 50 }));
}

export async function getManagedMcp(managedMcpId: string): Promise<McpManagedDetail> {
  const client = await getAcpClient();
  return unwrapManagedSdkOutcome(client.mcpPlatform.mcpGet_unstable({ managedMcpId }));
}

export async function runMcpHealth(
  managedMcpId: string,
  mode: 'registration' | 'runtime'
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpHealthRun_unstable({
        managedMcpId,
        mode,
        idempotencyKey: createMcpIdempotencyKey('health'),
      })
    ).outcome
  );
}

export async function getMcpHealth(managedMcpId: string): Promise<McpHealthStatus> {
  const client = await getAcpClient();
  return unwrapManagedSdkOutcome(client.mcpPlatform.mcpHealthGet_unstable({ managedMcpId }));
}

export async function setMcpDefaultEnabled(
  item: Pick<McpManagedSummary, 'managedMcpId' | 'revision'>,
  enabled: boolean
): Promise<McpManagedSummary> {
  const client = await getAcpClient();
  return unwrapManagedSdkOutcome(
    client.mcpPlatform.mcpSetDefaultEnabled_unstable({
      managedMcpId: item.managedMcpId,
      enabled,
      expectedRevision: item.revision,
    })
  );
}

export async function controlMcpRuntime(
  item: Pick<McpManagedSummary, 'managedMcpId' | 'revision' | 'runtimeControl'>,
  action: 'start' | 'stop'
): Promise<McpManagedSummary> {
  const binding = item.runtimeControl?.binding;
  if (!binding) throw createMalformedMcpPlatformResponseError();
  const client = await getAcpClient();
  return unwrapManagedSdkOutcome(
    client.mcpPlatform.mcpRuntimeControl_unstable({
      managedMcpId: item.managedMcpId,
      expectedRevision: item.revision,
      action,
      runtimeBinding: binding,
    })
  );
}

export async function getMcpTask(taskId: string): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpTaskGet_unstable({ taskId })).outcome);
}

export async function cancelMcpTask(
  task: Pick<McpTaskRef, 'taskId' | 'revision'>
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpTaskCancel_unstable({
        taskId: task.taskId,
        expectedRevision: task.revision,
      })
    ).outcome
  );
}

export async function retryMcpTask(
  task: Pick<McpTaskRef, 'taskId' | 'revision'>
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpTaskRetry_unstable({
        taskId: task.taskId,
        expectedRevision: task.revision,
        idempotencyKey: createMcpIdempotencyKey('task-retry'),
      })
    ).outcome
  );
}

export async function resumeMcpEvents(
  taskIds: string[],
  afterEventId?: number
): Promise<McpEventsPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpEventsResume_unstable({
        taskIds,
        afterEventId,
        limit: 100,
      })
    ).outcome
  );
}

function createUnresolvedPublicTaskError(): McpCenterSafeError {
  return toMcpCenterSafeError({
    code: 'not_found',
    correlationId: 'public-task-reference',
    message: 'Public MCP task reference could not be resolved.',
    retryable: false,
  });
}

function hasRawMcpErrorShape(value: unknown): boolean {
  if (!value || typeof value !== 'object') return false;
  const record = value as Record<string, unknown>;
  if (record.envelope && typeof record.envelope === 'object') return true;
  return typeof record.code === 'string' || typeof record.correlationId === 'string';
}

function toSafePublicTaskAdapterError(error: unknown): McpCenterSafeError {
  return toMcpCenterSafeError(
    hasRawMcpErrorShape(error)
      ? error
      : {
          code: 'adapter_failed',
          correlationId: 'mcp-task-public-adapter',
          message: error instanceof Error ? error.message : 'MCP task adapter failed.',
          retryable: false,
        }
  );
}

export function mapPublicMcpTask(task: McpTaskRef): PublicTask {
  try {
    return toPublicTask(task);
  } catch (error) {
    throw toSafePublicTaskAdapterError(error);
  }
}

export function mapPublicMcpTaskEvent(
  event: McpEventsPage['events'][number],
  taskReference?: PublicTaskReference
): PublicTaskEvent {
  try {
    return toPublicTaskEvent(event, taskReference);
  } catch (error) {
    throw toSafePublicTaskAdapterError(error);
  }
}

export function createPublicMcpTaskMonitorCursor(task: PublicTask): McpTaskMonitorCursor {
  const resolved = resolvePublicTaskMutationTarget({
    reference: task.reference,
    revision: task.revision,
  });
  if (!resolved) throw createUnresolvedPublicTaskError();
  try {
    return createMcpTaskMonitorCursor(resolved, task.reference);
  } catch (error) {
    throw toSafePublicTaskAdapterError(error);
  }
}

export function revokePublicMcpTaskReference(reference: PublicTaskReference): void {
  revokePublicTaskReference(reference);
}

export function releasePublicMcpTaskMonitorCursor(cursor: McpTaskMonitorCursor): void {
  releaseMcpTaskMonitorCursor(cursor);
}

export async function getPublicMcpTask(reference: PublicTaskReference): Promise<PublicTask> {
  const resolved = resolvePublicTaskReference(reference);
  if (!resolved) throw createUnresolvedPublicTaskError();
  try {
    return toPublicTask(await getMcpTask(resolved.taskId), reference);
  } catch (error) {
    throw toMcpCenterSafeError(error);
  }
}

export async function cancelPublicMcpTask(target: PublicTaskMutationTarget): Promise<PublicTask> {
  const resolved = resolvePublicTaskMutationTarget(target);
  if (!resolved) throw createUnresolvedPublicTaskError();
  try {
    return toPublicTask(await cancelMcpTask(resolved), target.reference);
  } catch (error) {
    throw toMcpCenterSafeError(error);
  }
}

export async function retryPublicMcpTask(target: PublicTaskMutationTarget): Promise<PublicTask> {
  const resolved = resolvePublicTaskMutationTarget(target);
  if (!resolved) throw createUnresolvedPublicTaskError();
  try {
    return toPublicTask(await retryMcpTask(resolved), target.reference);
  } catch (error) {
    throw toMcpCenterSafeError(error);
  }
}

export async function resumePublicMcpTaskMonitor(
  cursor: McpTaskMonitorCursor,
  currentTask: PublicTask,
  currentEvents: readonly PublicTaskEvent[]
): Promise<McpTaskMonitorMergeResult> {
  const request = getMcpTaskMonitorResumeRequest(cursor);
  if (!request) throw createUnresolvedPublicTaskError();
  try {
    const [latestTask, page] = await Promise.all([
      getMcpTask(request.taskIds[0]),
      resumeMcpEvents(request.taskIds, request.afterEventId),
    ]);
    return mergeMcpTaskMonitorPage(cursor, currentTask, currentEvents, latestTask, page);
  } catch (error) {
    throw toMcpCenterSafeError(
      hasRawMcpErrorShape(error)
        ? error
        : {
            code: 'monitor_paused',
            correlationId: 'mcp-task-monitor',
            message: error instanceof Error ? error.message : 'MCP task monitor paused.',
            retryable: true,
          }
    );
  }
}

export async function getMcpSourcesPolicy(): Promise<McpSourcesPolicyState> {
  return requestMcpPlatform('goose.mcpSourcesPolicyGet_unstable', parseMcpSourcesPolicyStateValue);
}

export async function refreshMcpSource(sourceId: string): Promise<McpSourceRefreshResult> {
  const result = await requestMcpPlatform(
    'goose.mcpSourceRefresh_unstable',
    parseMcpSourceRefreshResultValue,
    { sourceId }
  );
  if (result.sourceId !== sourceId) throw createMalformedMcpPlatformResponseError();
  return result;
}

export async function prepareMcpSourceProvision(
  localDirectory: string
): Promise<McpSourceProvisionPrepareResult> {
  return requestMcpPlatform(
    'goose.mcpSourceProvisionPrepare_unstable',
    parseMcpSourceProvisionPrepareResultValue,
    { localDirectory }
  );
}

export async function prepareHttpsMcpManifest(
  url: string
): Promise<McpHttpsManifestPrepareResult> {
  if (typeof url !== 'string' || !/^https:\/\/[^\s]+$/i.test(url)) {
    throw createMalformedMcpPlatformResponseError();
  }
  try {
    return await requestMcpPlatform(
      'goose.mcpHttpsManifestPrepare_unstable',
      parseMcpHttpsManifestPrepareResultValue,
      { url }
    );
  } catch {
    throw createMalformedMcpPlatformResponseError();
  }
}

export async function createHttpsProvisionPlanReview(
  input: McpHttpsProvisionPlanReviewRequest
): Promise<McpHttpsProvisionPlanReview> {
  const request = projectHttpsProvisionPlanReviewRequest(input);
  try {
    return await requestMcpPlatform(
      'goose.mcpHttpsProvisionPlanCreate_unstable',
      parseHttpsProvisionPlanReview,
      {
        provisionId: request.provisionId,
        expectedManifestDigest: request.expectedManifestDigest,
        idempotencyKey: request.idempotencyKey,
      }
    );
  } catch (error) {
    if (error instanceof McpPlatformServiceError) throw error;
    throw createMalformedMcpPlatformResponseError();
  }
}

export async function confirmHttpsMcpManifest(input: {
  provisionId: string;
  confirmationToken: string;
  confirm: boolean;
}): Promise<McpHttpsManifestConfirmResult> {
  if (!input || typeof input.provisionId !== 'string' ||
      typeof input.confirmationToken !== 'string' || typeof input.confirm !== 'boolean') {
    throw createMalformedMcpPlatformResponseError();
  }
  try {
    return await requestMcpPlatform(
      'goose.mcpHttpsManifestConfirm_unstable',
      parseMcpHttpsManifestConfirmResultValue,
      {
        provisionId: input.provisionId,
        confirmationToken: input.confirmationToken,
        confirm: input.confirm,
      }
    );
  } catch {
    throw createMalformedMcpPlatformResponseError();
  }
}

export async function confirmMcpSourceProvision(
  prepared: Pick<McpSourceProvisionPrepareResult, 'provisionId' | 'confirmationToken'>
): Promise<McpSourceProvisionConfirmResult> {
  const result = await requestMcpPlatform(
    'goose.mcpSourceProvisionConfirm_unstable',
    parseMcpSourceProvisionConfirmResultValue,
    {
      provisionId: prepared.provisionId,
      confirmationToken: prepared.confirmationToken,
      confirm: true,
    }
  );
  if (result.provisionId !== prepared.provisionId) throw createMalformedMcpPlatformResponseError();
  return result;
}

export async function importGovernedMcpSource(
  source: McpGovernedImportSource
): Promise<McpGovernedImportResult> {
  return requestMcpPlatform('goose.mcpGovernedImport_unstable', parseMcpGovernedImportResultValue, {
    source,
  });
}

export async function listManualStdioSources(): Promise<McpManualStdioSourcesPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpManualStdioSourcesList_unstable()).outcome);
}

export async function listMcpProfiles(includeArchived: boolean): Promise<McpProfilePage> {
  return requestMcpPlatform('goose.mcpProfileList_unstable', parseMcpProfilePageValue, {
    includeArchived,
  });
}

export async function getMcpProfile(profileId: string): Promise<McpProfileDetail> {
  return requestMcpPlatform('goose.mcpProfileGet_unstable', parseMcpProfileDetailValue, {
    profileId,
  });
}

export async function createMcpProfile(params: {
  name: string;
  description: string;
  managedMcpIds: string[];
}): Promise<McpProfileSummary> {
  return requestMcpPlatform('goose.mcpProfileCreate_unstable', parseMcpProfileSummaryValue, {
    name: params.name,
    description: params.description,
    managedMcpIds: params.managedMcpIds,
    idempotencyKey: createMcpIdempotencyKey('profile-create'),
  });
}

export async function updateMcpProfile(params: {
  profileId: string;
  expectedRevision: number;
  name: string;
  description: string;
  managedMcpIds: string[];
}): Promise<McpProfileSummary> {
  return requestMcpPlatform('goose.mcpProfileUpdate_unstable', parseMcpProfileSummaryValue, {
    profileId: params.profileId,
    expectedRevision: params.expectedRevision,
    name: params.name,
    description: params.description,
    managedMcpIds: params.managedMcpIds,
    idempotencyKey: createMcpIdempotencyKey('profile-update'),
  });
}

export async function restoreMcpProfile(params: {
  profileId: string;
  sourceRevision: number;
  expectedRevision: number;
}): Promise<McpProfileSummary> {
  return requestMcpPlatform('goose.mcpProfileRestore_unstable', parseMcpProfileSummaryValue, {
    profileId: params.profileId,
    sourceRevision: params.sourceRevision,
    expectedRevision: params.expectedRevision,
    idempotencyKey: createMcpIdempotencyKey('profile-restore'),
  });
}

export async function archiveMcpProfile(params: {
  profileId: string;
  expectedRevision: number;
}): Promise<McpProfileSummary> {
  return requestMcpPlatform('goose.mcpProfileArchive_unstable', parseMcpProfileSummaryValue, {
    profileId: params.profileId,
    expectedRevision: params.expectedRevision,
    idempotencyKey: createMcpIdempotencyKey('profile-archive'),
  });
}

export async function createMcpProfileApplyPlan(params: {
  profileId: string;
  profileRevision: number;
}): Promise<McpProfileApplyPlan> {
  return requestMcpPlatform(
    'goose.mcpProfileApplyPlanCreate_unstable',
    parseMcpProfileApplyPlanValue,
    {
      profileId: params.profileId,
      profileRevision: params.profileRevision,
      idempotencyKey: createMcpIdempotencyKey('profile-apply-plan'),
    }
  );
}

export async function confirmMcpProfileApply(params: {
  planId: string;
  confirmationToken: string;
  confirm: boolean;
}): Promise<McpProfileApplicationToken> {
  return requestMcpPlatform(
    'goose.mcpProfileApplyConfirm_unstable',
    parseMcpProfileApplicationTokenValue,
    {
      planId: params.planId,
      confirmationToken: params.confirmationToken,
      confirm: params.confirm,
    }
  );
}

export async function testMcpProfileConnection(params: {
  profileId: string;
  providerId: string;
  modelId: string;
}): Promise<McpProfileConnectionTestResult> {
  return requestMcpPlatform(
    'goose.mcpProfileConnectionTest_unstable',
    parseMcpProfileConnectionTestResultValue,
    {
      profileId: params.profileId,
      providerId: params.providerId,
      modelId: params.modelId,
    }
  );
}

export async function createMcpProfileDraft(
  text: string,
  locale: string
): Promise<McpProfileDraft> {
  return requestMcpPlatform('goose.mcpProfileDraftCreate_unstable', parseMcpProfileDraftValue, {
    text,
    locale,
  });
}

export async function recommendMcpProfileModels(
  text: string,
  providerIds: string[]
): Promise<McpModelRecommendation> {
  return requestMcpPlatform(
    'goose.mcpProfileModelRecommend_unstable',
    parseMcpModelRecommendationValue,
    {
      text,
      providerIds,
    }
  );
}
