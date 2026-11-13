export interface MediaResultIdentity {
  sessionId: string;
  resultId: string;
}

type ImageSpec = { kind: 'image'; width: number; height: number };
type AudioSpec = {
  kind: 'audio';
  durationMicros: number;
  sampleRate: number;
  channels: number;
  sampleFrames: number;
};
type VideoSpec = {
  kind: 'video';
  width: number;
  height: number;
  durationMicros: number;
  frameCount: number;
  fpsNumerator: number;
  fpsDenominator: number;
};

export type MediaResultPresentation = MediaResultIdentity & {
  fileName: string;
  mime: string;
  bytes: number;
} & (
    | { kind: 'image'; spec: ImageSpec }
    | { kind: 'audio'; spec: AudioSpec }
    | { kind: 'video'; spec: VideoSpec }
  );

export type MediaAvailability =
  | 'available'
  | 'publishing'
  | 'missing'
  | 'access_denied'
  | 'tampered'
  | 'unsafe_path'
  | 'unsupported_format'
  | 'io_unavailable';

export type MediaFileAction = 'open' | 'reveal' | 'copyPath' | 'saveAs';
export type MediaActionOutcome =
  | { status: 'completed' }
  | { status: 'cancelled' }
  | { status: 'failed'; message?: string };
export type MediaResultActions = Partial<
  Record<MediaFileAction, (identity: MediaResultIdentity) => Promise<MediaActionOutcome>>
>;

export interface MediaResultCardProps {
  result: MediaResultPresentation;
  availability: MediaAvailability;
  /** Issued by the parent's controlled media transport, never derived from a resource URI or path. */
  previewUrl: string | null;
  /** False when its conversation is no longer active, even if the card remains mounted. */
  isActive: boolean;
  actions: MediaResultActions;
}
