import { describe, expect, it } from 'vitest';
import { defaultSettings, mergeExternalBackendConfig, redactExternalBackendSecret } from '../settings';

describe('renderer settings projection', () => {
  it('never exposes the external backend secret', () => {
    const settings = {
      ...defaultSettings,
      externalGoosed: { ...defaultSettings.externalGoosed, secret: 'must-not-leak' },
    };

    expect(redactExternalBackendSecret(settings).externalGoosed.secret).toBe('');
    expect(settings.externalGoosed.secret).toBe('must-not-leak');
  });

  it('preserves a secret when a redacted settings update omits a replacement', () => {
    const current = { ...defaultSettings.externalGoosed, secret: 'existing' };
    expect(mergeExternalBackendConfig(current, { url: 'https://backend.test' }).secret).toBe('existing');
  });

  it('supports replacement and explicit clearing as separate operations', () => {
    const current = { ...defaultSettings.externalGoosed, secret: 'existing' };
    expect(mergeExternalBackendConfig(current, { secret: 'replacement' }).secret).toBe('replacement');
    expect(mergeExternalBackendConfig(current, { secret: '', clearSecret: true }).secret).toBe('');
  });
});
