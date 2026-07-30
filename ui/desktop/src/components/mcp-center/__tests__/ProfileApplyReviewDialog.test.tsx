import { IntlProvider } from 'react-intl';
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import {
  ProfileApplyReviewDialog,
  type ProfileApplyReviewDialogState,
} from '../ProfileApplyReviewDialog';

function renderDialog(state: ProfileApplyReviewDialogState) {
  render(
    <IntlProvider locale="en">
      <ProfileApplyReviewDialog
        state={state}
        onClose={vi.fn()}
        onConfirm={vi.fn()}
        onRecreatePlan={vi.fn()}
      />
    </IntlProvider>
  );
}

describe('ProfileApplyReviewDialog', () => {
  it('renders only safe public review fields and never shows forbidden secrets or provenance', () => {
    const state = {
      profileId: 'profile-a',
      profileName: 'Research setup',
      review: {
        profileId: 'profile-a',
        profileRevision: 2,
        mergePolicy: 'replace_managed_only',
        entries: [
          {
            managedMcpId: 'managed-a',
            mcpId: 'fetch',
            name: 'Fetch',
            version: '1.0.0',
            health: 'healthy',
            authReady: true,
            policyReady: false,
            readiness: 'incomplete',
          },
        ],
        expiresAtMs: Date.now() + 60_000,
        digest: 'plan-digest-should-never-render',
        credentialRef: 'credential-ref-should-never-render',
        provenance: 'source-fingerprint-should-never-render',
      },
      phase: 'review',
      confirmationTokenAvailable: true,
      recovery: null,
      canRecreatePlan: false,
    } as unknown as ProfileApplyReviewDialogState;

    renderDialog(state);

    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(screen.getByText('Review profile application')).toBeInTheDocument();
    expect(screen.getByText('Fetch')).toBeInTheDocument();
    expect(screen.getByText('replace managed only')).toBeInTheDocument();
    expect(screen.getByText('Policy needs attention')).toBeInTheDocument();
    expect(screen.queryByText('confirmation-token-should-never-render')).not.toBeInTheDocument();
    expect(screen.queryByText('plan-digest-should-never-render')).not.toBeInTheDocument();
    expect(screen.queryByText('credential-ref-should-never-render')).not.toBeInTheDocument();
    expect(screen.queryByText('source-fingerprint-should-never-render')).not.toBeInTheDocument();
  });

  it('covers expired review state by disabling confirmation and showing recovery copy', () => {
    renderDialog({
      profileId: 'profile-a',
      profileName: 'Research setup',
      review: {
        profileId: 'profile-a',
        profileRevision: 2,
        mergePolicy: 'replace_managed_only',
        entries: [],
        expiresAtMs: Date.now() - 1,
      },
      phase: 'review',
      confirmationTokenAvailable: true,
      recovery: null,
      canRecreatePlan: true,
    });

    expect(screen.getByText('This review has expired')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Confirm and create new session' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Create a new review' })).toBeInTheDocument();
  });
});
