import type { MediaAvailability, MediaResultIdentity, MediaResultPresentation } from './types';
import type { MediaContentMetadata } from '../../types/desktopMedia';

const availabilityValues = new Set<MediaAvailability>([
  'available',
  'publishing',
  'missing',
  'access_denied',
  'tampered',
  'unsafe_path',
  'unsupported_format',
  'io_unavailable',
]);
function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function positive(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
}

export function readMediaPresentation(
  value: unknown,
  identity: MediaResultIdentity
): {
  result: MediaResultPresentation;
  metadata: MediaContentMetadata;
  availability: MediaAvailability;
} | null {
  if (record(value) && value.status === 'notFound') return null;
  const fail = (): never => {
    throw new Error('Invalid media result.');
  };
  if (!record(value) || value.status !== 'found' || !record(value.result)) return fail();
  const result = value.result;
  if (
    result.schemaVersion !== 1 ||
    result.sessionId !== identity.sessionId ||
    result.resultId !== identity.resultId ||
    typeof result.fileName !== 'string' ||
    result.fileName.length === 0 ||
    result.fileName.length > 255 ||
    /[\\/]/.test(result.fileName) ||
    Array.from(result.fileName).some((character) => character.charCodeAt(0) < 32) ||
    typeof result.mime !== 'string' ||
    result.mime.length > 128 ||
    !positive(result.bytes) ||
    typeof result.sha256 !== 'string' ||
    !/^[0-9a-f]{64}$/.test(result.sha256) ||
    !availabilityValues.has(value.availability as MediaAvailability) ||
    !record(result.spec) ||
    result.kind !== result.spec.kind
  )
    return fail();
  const metadata = {
    fileName: result.fileName,
    mime: result.mime,
    bytes: result.bytes,
    sha256: result.sha256,
  };
  const common = {
    ...identity,
    fileName: metadata.fileName,
    mime: metadata.mime,
    bytes: metadata.bytes,
  };
  const spec = result.spec;
  let presentation: MediaResultPresentation;
  if (result.kind === 'image' && positive(spec.width) && positive(spec.height)) {
    presentation = {
      ...common,
      kind: 'image',
      spec: { kind: 'image', width: spec.width, height: spec.height },
    };
  } else if (
    result.kind === 'audio' &&
    positive(spec.durationMicros) &&
    positive(spec.sampleRate) &&
    positive(spec.channels) &&
    positive(spec.sampleFrames)
  ) {
    presentation = {
      ...common,
      kind: 'audio',
      spec: {
        kind: 'audio',
        durationMicros: spec.durationMicros,
        sampleRate: spec.sampleRate,
        channels: spec.channels,
        sampleFrames: spec.sampleFrames,
      },
    };
  } else if (
    result.kind === 'video' &&
    positive(spec.width) &&
    positive(spec.height) &&
    positive(spec.durationMicros) &&
    positive(spec.frameCount) &&
    positive(spec.fpsNumerator) &&
    positive(spec.fpsDenominator)
  ) {
    presentation = {
      ...common,
      kind: 'video',
      spec: {
        kind: 'video',
        width: spec.width,
        height: spec.height,
        durationMicros: spec.durationMicros,
        frameCount: spec.frameCount,
        fpsNumerator: spec.fpsNumerator,
        fpsDenominator: spec.fpsDenominator,
      },
    };
  } else return fail();
  return { result: presentation, metadata, availability: value.availability as MediaAvailability };
}
