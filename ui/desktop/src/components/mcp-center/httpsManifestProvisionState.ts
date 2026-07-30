export type HttpsProvisionState =
  | 'idle'
  | 'validating'
  | 'preparing'
  | 'preview'
  | 'confirming'
  | 'plan-review'
  | 'submitting'
  | 'task-created'
  | 'error'
  | 'cancelled';

export type HttpsProvisionModel<TPreview = unknown, TPlan = unknown> = {
  state: HttpsProvisionState;
  url: string;
  preview: TPreview | null;
  plan: TPlan | null;
  error: string | null;
};

export function initialHttpsProvisionModel<TPreview = unknown, TPlan = unknown>(): HttpsProvisionModel<TPreview, TPlan> {
  return { state: 'idle', url: '', preview: null, plan: null, error: null };
}

export function isHttpsManifestUrl(value: string): boolean {
  if (!value || value !== value.trim() || /\s/.test(value)) {
    return false;
  }

  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }

  if (parsed.protocol.toLowerCase() !== 'https:') {
    return false;
  }

  if (parsed.hash) {
    return false;
  }

  const authority = value.slice(value.indexOf('//') + 2).split(/[/?#]/, 1)[0];
  const hostname = parsed.hostname.toLowerCase().replace(/^\[|\]$/g, '');
  const isIpv4Loopback = /^127(?:\.\d{1,3}){3}$/.test(hostname);
  const isIpv6Loopback = hostname === '::1';
  const isLocalName = hostname === 'local'
    || hostname.endsWith('.local')
    || hostname === 'localhost'
    || hostname.endsWith('.localhost');

  return !authority.includes('@') && !isLocalName && !isIpv4Loopback && !isIpv6Loopback;
}
