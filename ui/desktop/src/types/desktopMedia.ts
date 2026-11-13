import type { MediaActionOutcome, MediaResultIdentity } from '../components/media/types';

export type DesktopMediaPurpose = 'preview' | 'save_as' | 'open' | 'reveal' | 'copy_path';

export interface MediaContentMetadata {
  fileName: string;
  mime: string;
  bytes: number;
  sha256: string;
}

export interface DesktopMediaGrant {
  ticket: string;
  expiresInMs: number;
  metadata: MediaContentMetadata;
}

export interface DesktopMediaHandle {
  handleId: string;
  nativeActions: boolean;
}

export interface DesktopMediaPreviewRequest extends DesktopMediaGrant {
  handleId: string;
}

export interface DesktopMediaActionRequest extends DesktopMediaGrant {
  handleId: string;
  purpose: Exclude<DesktopMediaPurpose, 'preview'>;
  /** A separately scoped copy_path ticket, used only to prevent saving over a managed-local source. */
  sourcePathTicket?: string;
}

export interface DesktopMediaAPI {
  mediaOpen(identity: MediaResultIdentity): Promise<DesktopMediaHandle>;
  mediaPreview(request: DesktopMediaPreviewRequest): Promise<{ url: string }>;
  mediaAction(request: DesktopMediaActionRequest): Promise<MediaActionOutcome>;
  mediaClose(handleId: string): Promise<void>;
}
