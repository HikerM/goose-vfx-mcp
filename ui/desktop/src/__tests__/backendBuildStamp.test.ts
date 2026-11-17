import { createRequire } from 'node:module';
import { describe, expect, it } from 'vitest';

const require = createRequire(import.meta.url);
const stamp = require('../../scripts/prepare-platform-binaries.js') as {
  buildProfile: () => string;
  sourceRevision: () => string;
  sourceDirty: () => boolean;
};

describe('backend build provenance', () => {
  it('records the current Git revision when packaging from the checkout', () => {
    const revision = stamp.sourceRevision();
    expect(revision).toMatch(/^[0-9a-f]{40}$/);
  });

  it('uses the explicit verified revision for source archives', () => {
    const previous = process.env.LUMINA_SOURCE_REVISION;
    process.env.LUMINA_SOURCE_REVISION = 'A'.repeat(40);
    try {
      expect(stamp.sourceRevision()).toBe('a'.repeat(40));
    } finally {
      if (previous === undefined) delete process.env.LUMINA_SOURCE_REVISION;
      else process.env.LUMINA_SOURCE_REVISION = previous;
    }
  });

  it('rejects an unverified revision override', () => {
    const previous = process.env.LUMINA_SOURCE_REVISION;
    process.env.LUMINA_SOURCE_REVISION = 'unknown';
    try {
      expect(() => stamp.sourceRevision()).toThrow(/40-character Git commit SHA/);
    } finally {
      if (previous === undefined) delete process.env.LUMINA_SOURCE_REVISION;
      else process.env.LUMINA_SOURCE_REVISION = previous;
    }
  });

  it('reports whether the source checkout is dirty and accepts an explicit archive value', () => {
    expect(typeof stamp.sourceDirty()).toBe('boolean');
    const previous = process.env.LUMINA_SOURCE_DIRTY;
    process.env.LUMINA_SOURCE_DIRTY = 'true';
    try {
      expect(stamp.sourceDirty()).toBe(true);
      process.env.LUMINA_SOURCE_DIRTY = 'false';
      expect(stamp.sourceDirty()).toBe(false);
    } finally {
      if (previous === undefined) delete process.env.LUMINA_SOURCE_DIRTY;
      else process.env.LUMINA_SOURCE_DIRTY = previous;
    }
  });

  it('rejects an invalid source dirty override', () => {
    const previous = process.env.LUMINA_SOURCE_DIRTY;
    process.env.LUMINA_SOURCE_DIRTY = 'maybe';
    try {
      expect(() => stamp.sourceDirty()).toThrow(/must be either true or false/);
    } finally {
      if (previous === undefined) delete process.env.LUMINA_SOURCE_DIRTY;
      else process.env.LUMINA_SOURCE_DIRTY = previous;
    }
  });

  it('derives the build profile from the CUDA packaging flag', () => {
    const previous = process.env.LUMINA_DESKTOP_CUDA;
    const previousDual = process.env.LUMINA_DESKTOP_DUAL_BACKEND;
    try {
      delete process.env.LUMINA_DESKTOP_CUDA;
      delete process.env.LUMINA_DESKTOP_DUAL_BACKEND;
      expect(stamp.buildProfile()).toBe('cpu');
      process.env.LUMINA_DESKTOP_CUDA = '1';
      expect(stamp.buildProfile()).toBe('cuda');
      process.env.LUMINA_DESKTOP_DUAL_BACKEND = '1';
      expect(stamp.buildProfile()).toBe('dual');
    } finally {
      if (previous === undefined) delete process.env.LUMINA_DESKTOP_CUDA;
      else process.env.LUMINA_DESKTOP_CUDA = previous;
      if (previousDual === undefined) delete process.env.LUMINA_DESKTOP_DUAL_BACKEND;
      else process.env.LUMINA_DESKTOP_DUAL_BACKEND = previousDual;
    }
  });
});
