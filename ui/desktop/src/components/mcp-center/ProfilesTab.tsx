import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { McpManagedSummary } from '@aaif/goose-sdk';
import { Archive, FileStack, PencilLine, RefreshCw, Sparkles, Wand2 } from 'lucide-react';
import { useNavigate } from 'react-router-dom';
import { toast } from 'react-toastify';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { ScrollArea } from '../ui/scroll-area';
import { ConfirmationModal } from '../ui/ConfirmationModal';
import {
  archiveMcpProfile,
  confirmMcpProfileApply,
  createMcpProfile,
  createMcpProfileApplyPlan,
  createMcpProfileDraft,
  getMcpProfile,
  isMcpPhaseUnavailableError,
  listManagedMcps,
  listMcpProfiles,
  McpPlatformServiceError,
  recommendMcpProfileModels,
  restoreMcpProfile,
  toMcpRecoveryViewModel,
  updateMcpProfile,
  type McpModelRecommendation,
  type McpProfileApplyPlan,
  type McpProfileApplicationToken,
  type McpProfileDetail,
  type McpProfileDraft,
  type McpProfileSummary,
  type McpProfileRevisionSummary,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import { acpGetSessionListItem } from '../../acp/sessions';
import { acpChatSessionStore } from '../../acp/chatSessionStore';
import { AppEvents } from '../../constants/events';
import { useChatContext } from '../../contexts/ChatContext';
import {
  CredentialAttentionPanel,
  CredentialStatusBadge,
  DefinitionList,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
} from './McpCenterCommon';
import {
  ProfileApplyReviewDialog,
  type PublicProfileApplyReview,
  type ProfileApplyReviewDialogState,
} from './ProfileApplyReviewDialog';
import { ProfileConnectionTestDialog } from './ProfileConnectionTestDialog';
import { mcpCenterMessages as messages } from './messages';
import { useConfig } from '../ConfigContext';
import { useIntl } from '../../i18n';
import { createSession } from '../../sessions';
import { errorMessage } from '../../utils/conversionUtils';

type CapabilityState = 'available' | 'unknown' | 'unavailable';
type EditorMode = 'create' | 'edit' | null;

type ProfileFormState = {
  name: string;
  description: string;
  managedMcpIds: string[];
};

type PendingDiscardAction = (() => void) | null;
type LoadProfilesOptions = {
  preferredProfileId?: string | null;
  selectionContextVersion?: number | null;
};
type ProfileContextSnapshot = {
  contextVersion: number;
  selectedProfileId: string | null;
  editorMode: EditorMode;
  showArchived: boolean;
};
type ProfileAsyncOperation = ProfileContextSnapshot & {
  requestId: number;
  profileId: string | null;
  profileName: string;
  mutationGeneration?: number;
};
type InputAsyncOperation = {
  requestId: number;
  text: string;
  locale: string;
  providerIds?: string[];
};
type ProfileScopedError = {
  profileId: string;
  profileName: string | null;
  recovery: McpPlatformRecoveryViewModel;
};
type ActiveProfileApplyDialogState = ProfileApplyReviewDialogState & {
  flowId: number;
};
type ActiveProfileApplyConfirmation = {
  flowId: number;
  planId: string;
  confirmationToken: string;
};

const emptyFormState: ProfileFormState = {
  name: '',
  description: '',
  managedMcpIds: [],
};

function formatProfileTimestamp(intl: ReturnType<typeof useIntl>, updatedAtMs: number): string {
  return `${intl.formatDate(updatedAtMs, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })} ${intl.formatTime(updatedAtMs, {
    hour: 'numeric',
    minute: '2-digit',
  })}`;
}

function toSortedManagedIds(ids: string[]): string[] {
  return [...ids].sort((left, right) => left.localeCompare(right));
}

function buildFormState(profile?: McpProfileSummary): ProfileFormState {
  if (!profile) return emptyFormState;
  return {
    name: profile.name,
    description: profile.description,
    managedMcpIds: profile.entries
      .slice()
      .sort((left, right) => left.ordinal - right.ordinal)
      .map((entry) => entry.managedMcpId),
  };
}

function pickDeterministicNextProfileId(
  items: McpProfileSummary[],
  previousItems: McpProfileSummary[],
  missingProfileId: string | null
): string | null {
  if (items.length === 0) return null;
  if (!missingProfileId) return items[0]?.profileId ?? null;

  const visibleIds = new Set(items.map((item) => item.profileId));
  const previousIndex = previousItems.findIndex((item) => item.profileId === missingProfileId);
  if (previousIndex === -1) return items[0]?.profileId ?? null;

  for (let index = previousIndex + 1; index < previousItems.length; index += 1) {
    const candidateId = previousItems[index]?.profileId;
    if (candidateId && visibleIds.has(candidateId)) return candidateId;
  }

  for (let index = previousIndex - 1; index >= 0; index -= 1) {
    const candidateId = previousItems[index]?.profileId;
    if (candidateId && visibleIds.has(candidateId)) return candidateId;
  }

  return items[0]?.profileId ?? null;
}

function areFormsEqual(left: ProfileFormState, right: ProfileFormState): boolean {
  return (
    left.name === right.name &&
    left.description === right.description &&
    JSON.stringify(toSortedManagedIds(left.managedMcpIds)) ===
      JSON.stringify(toSortedManagedIds(right.managedMcpIds))
  );
}

function parseProviderIds(value: string): string[] {
  if (value.length === 0) return [];
  return value.split(',');
}

function sanitizeOpaqueDisplayValue(value: string | null | undefined): string | null {
  if (!value) return null;
  const sanitized = value
    .replace(/[\p{Cc}\p{Cf}]+/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
  return sanitized || null;
}

function isProfileApplicationTokenExpired(token: McpProfileApplicationToken): boolean {
  return token.expiresAtMs <= Date.now();
}

function normalizeWorkingDir(value: string | null | undefined): string | null {
  if (typeof value !== 'string') return null;
  return value.trim().length > 0 ? value : null;
}

function mapProfileApplySessionRecovery(
  intl: ReturnType<typeof useIntl>,
  error: unknown
): McpPlatformRecoveryViewModel {
  const message = errorMessage(error, '');
  const normalized = message.toLowerCase();

  if (normalized.includes('invalid_profile_application_token')) {
    return {
      title: intl.formatMessage(messages.profileApplyInvalidTokenTitle),
      message: intl.formatMessage(messages.profileApplyInvalidTokenMessage),
      nextStep: intl.formatMessage(messages.profileApplySessionErrorNextStep),
      retryable: false,
      kind: 'connection',
    };
  }

  if (normalized.includes('policy_denied')) {
    return {
      title: intl.formatMessage(messages.recoveryPolicyTitle),
      message: intl.formatMessage(messages.profileApplyPolicyDeniedMessage),
      nextStep: intl.formatMessage(messages.recoveryPolicyNext),
      retryable: false,
      kind: 'connection',
    };
  }

  if (/runtime[_ ]unavailable/.test(normalized)) {
    return {
      title: intl.formatMessage(messages.profileApplyRuntimeUnavailableTitle),
      message: intl.formatMessage(messages.profileApplyRuntimeUnavailableMessage),
      nextStep: intl.formatMessage(messages.profileApplySessionErrorNextStep),
      retryable: false,
      kind: 'connection',
    };
  }

  return {
    title: intl.formatMessage(messages.profileApplySessionErrorTitle),
    message: intl.formatMessage(messages.profileApplySessionErrorMessage),
    nextStep: intl.formatMessage(messages.profileApplySessionErrorNextStep),
    retryable: false,
    kind: 'connection',
  };
}

function currentSessionWorkingDirUnavailableRecovery(
  intl: ReturnType<typeof useIntl>
): McpPlatformRecoveryViewModel {
  return {
    title: intl.formatMessage(messages.profileApplyCurrentSessionUnavailableTitle),
    message: intl.formatMessage(messages.profileApplyCurrentSessionUnavailableMessage),
    nextStep: intl.formatMessage(messages.profileApplyRestart),
    retryable: false,
    kind: 'connection',
  };
}

function invalidProfileApplyConfirmationRecovery(
  intl: ReturnType<typeof useIntl>
): McpPlatformRecoveryViewModel {
  return {
    title: intl.formatMessage(messages.profileApplyInvalidTokenTitle),
    message: intl.formatMessage(messages.profileApplyInvalidTokenMessage),
    nextStep: intl.formatMessage(messages.profileApplySessionErrorNextStep),
    retryable: false,
    kind: 'connection',
  };
}

export function redactProfileApplyReview(plan: McpProfileApplyPlan): PublicProfileApplyReview {
  return {
    profileId: plan.profileId,
    profileRevision: plan.profileRevision,
    mergePolicy: plan.mergePolicy,
    entries: plan.entries.map((entry) => ({
      managedMcpId: entry.managedMcpId,
      mcpId: entry.mcpId,
      name: entry.name,
      version: entry.version,
      health: entry.health,
      authReady: entry.authReady,
      policyReady: entry.policyReady,
      readiness: entry.readiness,
    })),
    expiresAtMs: plan.expiresAtMs,
  };
}

function inventoryCapabilityHint(
  items: McpManagedSummary[],
  key: 'profiles' | 'modelSuggestions'
): string | undefined {
  const hinted = items.map((item) => item.phaseCapabilities[key]).find(Boolean);
  return hinted;
}

function HistoryRestoreButton({
  revision,
  disabled,
  onRestore,
}: {
  revision: McpProfileRevisionSummary;
  disabled: boolean;
  onRestore: (revision: McpProfileRevisionSummary) => void;
}) {
  const intl = useIntl();
  return (
    <Button size="sm" variant="outline" disabled={disabled} onClick={() => onRestore(revision)}>
      {intl.formatMessage(messages.restoreRevision)}
    </Button>
  );
}

function CapabilityUnavailablePanel({
  title,
  description,
  hint,
  recovery,
}: {
  title: string;
  description: string;
  hint?: string;
  recovery?: McpPlatformRecoveryViewModel | null;
}) {
  return (
    <div className="rounded-xl border border-border-primary bg-background-primary p-5">
      <h2 className="text-lg font-medium text-text-primary">{title}</h2>
      <p className="mt-2 text-sm text-text-secondary">{description}</p>
      {hint && <p className="mt-3 text-sm text-text-tertiary">{hint}</p>}
      {recovery?.correlationId && (
        <p className="mt-3 truncate font-mono text-xs text-text-tertiary">
          {recovery.correlationId}
        </p>
      )}
    </div>
  );
}

export function ProfilesTab() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { extensionsList } = useConfig();
  const chatContext = useChatContext();
  const [profilesCapability, setProfilesCapability] = useState<CapabilityState>('unknown');
  const [profilesCapabilityRecovery, setProfilesCapabilityRecovery] =
    useState<McpPlatformRecoveryViewModel | null>(null);
  const [profiles, setProfiles] = useState<McpProfileSummary[]>([]);
  const [profilesLoading, setProfilesLoading] = useState(true);
  const [profilesError, setProfilesError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [showArchived, setShowArchived] = useState(false);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [detail, setDetail] = useState<McpProfileDetail | null>(null);
  const [detailError, setDetailError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [detailOperation, setDetailOperation] = useState<ProfileAsyncOperation | null>(null);
  const [inventory, setInventory] = useState<McpManagedSummary[]>([]);
  const [inventoryLoading, setInventoryLoading] = useState(true);
  const [inventoryError, setInventoryError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [editorMode, setEditorMode] = useState<EditorMode>(null);
  const [form, setForm] = useState<ProfileFormState>(emptyFormState);
  const [initialForm, setInitialForm] = useState<ProfileFormState>(emptyFormState);
  const [formNameError, setFormNameError] = useState<string | null>(null);
  const [createEditorError, setCreateEditorError] = useState<McpPlatformRecoveryViewModel | null>(
    null
  );
  const [profileErrors, setProfileErrors] = useState<Record<string, ProfileScopedError>>({});
  const [saveOperation, setSaveOperation] = useState<ProfileAsyncOperation | null>(null);
  const [archiveConfirmOpen, setArchiveConfirmOpen] = useState(false);
  const [archiveOperation, setArchiveOperation] = useState<ProfileAsyncOperation | null>(null);
  const [restoreRevision, setRestoreRevision] = useState<McpProfileRevisionSummary | null>(null);
  const [restoreOperation, setRestoreOperation] = useState<ProfileAsyncOperation | null>(null);
  const [discardOpen, setDiscardOpen] = useState(false);
  const [draftText, setDraftText] = useState('');
  const [draftError, setDraftError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [draftResult, setDraftResult] = useState<McpProfileDraft | null>(null);
  const [draftSelection, setDraftSelection] = useState<string[]>([]);
  const [draftOperation, setDraftOperation] = useState<InputAsyncOperation | null>(null);
  const [providerFilter, setProviderFilter] = useState('');
  const [modelCapability, setModelCapability] = useState<CapabilityState>('unknown');
  const [modelCapabilityRecovery, setModelCapabilityRecovery] =
    useState<McpPlatformRecoveryViewModel | null>(null);
  const [modelError, setModelError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [modelRecommendation, setModelRecommendation] = useState<McpModelRecommendation | null>(
    null
  );
  const [modelOperation, setModelOperation] = useState<InputAsyncOperation | null>(null);
  const [applyPlanOperation, setApplyPlanOperation] = useState<ProfileAsyncOperation | null>(null);
  const [applyDialogState, setApplyDialogState] = useState<ActiveProfileApplyDialogState | null>(
    null
  );
  const [connectionTestOpen, setConnectionTestOpen] = useState(false);
  const discardActionRef = useRef<PendingDiscardAction>(null);
  const profilesRef = useRef<McpProfileSummary[]>([]);
  const selectedProfileIdRef = useRef<string | null>(null);
  const editorModeRef = useRef<EditorMode>(null);
  const showArchivedRef = useRef(false);
  const contextVersionRef = useRef(0);
  const listRequestRef = useRef(0);
  const detailRequestRef = useRef(0);
  const inventoryRequestRef = useRef(0);
  const profileMutationGenerationRef = useRef<Record<string, number>>({});
  const saveRequestRef = useRef(0);
  const archiveRequestRef = useRef(0);
  const restoreRequestRef = useRef(0);
  const draftRequestRef = useRef(0);
  const modelRequestRef = useRef(0);
  const applyPlanRequestRef = useRef(0);
  const applyFlowRef = useRef(0);
  const applyRedirectTimerRef = useRef<number | null>(null);
  const applyPlanInFlightRef = useRef(false);
  const applyConfirmInFlightRef = useRef(false);
  const applyConfirmationRef = useRef<ActiveProfileApplyConfirmation | null>(null);

  const commitProfileContext = useCallback(
    (updates: {
      selectedProfileId?: string | null;
      editorMode?: EditorMode;
      showArchived?: boolean;
    }) => {
      const {
        selectedProfileId: nextSelectedProfileId,
        editorMode: nextEditorMode,
        showArchived: nextShowArchived,
      } = updates;
      let changed = false;

      if ('selectedProfileId' in updates) {
        const resolvedProfileId = nextSelectedProfileId ?? null;
        if (selectedProfileIdRef.current !== resolvedProfileId) {
          selectedProfileIdRef.current = resolvedProfileId;
          setSelectedProfileId(resolvedProfileId);
          changed = true;
        }
      }

      if ('editorMode' in updates) {
        const resolvedEditorMode = nextEditorMode ?? null;
        if (editorModeRef.current !== resolvedEditorMode) {
          editorModeRef.current = resolvedEditorMode;
          setEditorMode(resolvedEditorMode);
          changed = true;
        }
      }

      if ('showArchived' in updates && showArchived !== nextShowArchived) {
        showArchivedRef.current = nextShowArchived ?? false;
        setShowArchived(nextShowArchived ?? false);
        changed = true;
      }

      if (changed) {
        contextVersionRef.current += 1;
      }

      return contextVersionRef.current;
    },
    [showArchived]
  );

  const captureProfileContext = useCallback(
    (): ProfileContextSnapshot => ({
      contextVersion: contextVersionRef.current,
      selectedProfileId: selectedProfileIdRef.current,
      editorMode: editorModeRef.current,
      showArchived: showArchivedRef.current,
    }),
    []
  );

  const isCurrentProfileContext = useCallback((context: ProfileContextSnapshot): boolean => {
    return (
      contextVersionRef.current === context.contextVersion &&
      selectedProfileIdRef.current === context.selectedProfileId &&
      editorModeRef.current === context.editorMode &&
      showArchivedRef.current === context.showArchived
    );
  }, []);

  const isCurrentProfileOperation = useCallback(
    (operation: ProfileAsyncOperation | null): boolean =>
      operation !== null && isCurrentProfileContext(operation),
    [isCurrentProfileContext]
  );

  const beginProfileMutation = useCallback(
    (requestId: number, profileId: string, profileName: string): ProfileAsyncOperation => {
      const generation = (profileMutationGenerationRef.current[profileId] ?? 0) + 1;
      profileMutationGenerationRef.current[profileId] = generation;
      return {
        ...captureProfileContext(),
        requestId,
        profileId,
        profileName,
        mutationGeneration: generation,
      };
    },
    [captureProfileContext]
  );

  const isLatestProfileMutation = useCallback(
    (operation: ProfileAsyncOperation | null): boolean => {
      if (!operation?.profileId || operation.mutationGeneration === undefined) {
        return true;
      }
      return (
        (profileMutationGenerationRef.current[operation.profileId] ?? 0) ===
        operation.mutationGeneration
      );
    },
    []
  );

  const setProfileError = useCallback(
    (profileId: string, profileName: string | null, recovery: McpPlatformRecoveryViewModel) => {
      setProfileErrors((current) => ({
        ...current,
        [profileId]: { profileId, profileName, recovery },
      }));
    },
    []
  );

  const clearProfileError = useCallback((profileId: string | null) => {
    if (!profileId) return;
    setProfileErrors((current) => {
      if (!(profileId in current)) return current;
      const next = { ...current };
      delete next[profileId];
      return next;
    });
  }, []);

  const inventoryHint = useMemo(() => inventoryCapabilityHint(inventory, 'profiles'), [inventory]);
  const modelCapabilityHint = useMemo(
    () => inventoryCapabilityHint(inventory, 'modelSuggestions'),
    [inventory]
  );
  const inventoryById = useMemo(
    () =>
      new Map(
        inventory.map((item) => [
          item.managedMcpId,
          {
            managedMcpId: item.managedMcpId,
            mcpId: item.mcpId,
            updatedAtMs: item.updatedAtMs,
            health: item.health,
            runtime: item.runtime,
          },
        ])
      ),
    [inventory]
  );
  const normalizedDraftText = draftText.trim();
  const providerIds = useMemo(() => parseProviderIds(providerFilter), [providerFilter]);
  const draftCanSubmit = normalizedDraftText.length > 0;
  const dirty = editorMode !== null && !areFormsEqual(form, initialForm);
  const showInitialProfilesLoading = profilesLoading && profiles.length === 0 && !profilesError;
  const showBlockingProfilesError = profilesError && profiles.length === 0;
  const detailLoading = isCurrentProfileOperation(detailOperation);
  const saveLoading =
    isCurrentProfileOperation(saveOperation) && isLatestProfileMutation(saveOperation);
  const archiveSubmitting =
    isCurrentProfileOperation(archiveOperation) && isLatestProfileMutation(archiveOperation);
  const restorePending =
    isCurrentProfileOperation(restoreOperation) && isLatestProfileMutation(restoreOperation);
  const draftLoading =
    draftOperation !== null &&
    draftOperation.text === normalizedDraftText &&
    draftOperation.locale === intl.locale;
  const modelLoading =
    modelOperation !== null &&
    modelOperation.text === normalizedDraftText &&
    modelOperation.locale === intl.locale &&
    JSON.stringify(modelOperation.providerIds ?? []) === JSON.stringify(providerIds);
  const selectedProfileError = selectedProfileId
    ? (profileErrors[selectedProfileId] ?? null)
    : null;
  const selectedProfileSummary = selectedProfileId
    ? (profiles.find((profile) => profile.profileId === selectedProfileId) ?? null)
    : null;
  const activeSessionId = chatContext?.chat.sessionId ?? '';
  const detailCredentialStatus =
    detail?.profile.credentialStatus ?? selectedProfileSummary?.credentialStatus;
  const editorCredentialStatus =
    editorMode === 'edit'
      ? (detail?.profile.credentialStatus ?? selectedProfileSummary?.credentialStatus)
      : undefined;
  const editorError =
    editorMode === 'create' ? createEditorError : (selectedProfileError?.recovery ?? null);
  const visibleProfileIds = useMemo(
    () => new Set(profiles.map((profile) => profile.profileId)),
    [profiles]
  );
  const hiddenProfileErrors = useMemo(
    () =>
      Object.values(profileErrors).filter(
        (error) => error.profileId !== selectedProfileId && !visibleProfileIds.has(error.profileId)
      ),
    [profileErrors, selectedProfileId, visibleProfileIds]
  );
  const applyBlocked = applyPlanOperation !== null || applyDialogState !== null;
  const activeApplyProfileId = applyPlanOperation?.profileId ?? applyDialogState?.profileId ?? null;
  const isApplyPendingForProfile = useCallback(
    (profileId: string): boolean =>
      activeApplyProfileId === profileId &&
      (applyPlanOperation !== null ||
        applyDialogState?.phase === 'resolving-session' ||
        applyDialogState?.phase === 'confirming' ||
        applyDialogState?.phase === 'creating-session'),
    [activeApplyProfileId, applyDialogState?.phase, applyPlanOperation]
  );
  const applyLabelForProfile = useCallback(
    (profileId: string): string =>
      isApplyPendingForProfile(profileId)
        ? intl.formatMessage(messages.applyProfileLoading)
        : intl.formatMessage(messages.applyProfile),
    [intl, isApplyPendingForProfile]
  );

  const loadInventory = useCallback(async () => {
    const requestId = ++inventoryRequestRef.current;
    setInventoryLoading(true);
    setInventoryError(null);
    try {
      const items: McpManagedSummary[] = [];
      let cursor: string | undefined;
      do {
        const page = await listManagedMcps(cursor);
        items.push(...page.items);
        cursor = page.nextCursor ?? undefined;
      } while (cursor);
      if (inventoryRequestRef.current !== requestId) return;
      items.sort((left, right) => left.mcpId.localeCompare(right.mcpId));
      setInventory(items);
    } catch (cause) {
      if (inventoryRequestRef.current !== requestId) return;
      setInventory([]);
      setInventoryError(toMcpRecoveryViewModel(cause));
    } finally {
      if (inventoryRequestRef.current === requestId) {
        setInventoryLoading(false);
      }
    }
  }, []);

  const loadDetail = useCallback(
    async (profileId: string) => {
      const requestId = ++detailRequestRef.current;
      const operation: ProfileAsyncOperation = {
        ...captureProfileContext(),
        requestId,
        profileId,
        profileName: '',
      };
      setDetailOperation(operation);
      setDetailError(null);
      try {
        const nextDetail = await getMcpProfile(profileId);
        if (detailRequestRef.current !== requestId || !isCurrentProfileContext(operation)) {
          return;
        }
        setDetail(nextDetail);
      } catch (cause) {
        if (detailRequestRef.current !== requestId || !isCurrentProfileContext(operation)) {
          return;
        }
        setDetail(null);
        setDetailError(toMcpRecoveryViewModel(cause));
      } finally {
        setDetailOperation((current) => (current?.requestId === requestId ? null : current));
      }
    },
    [captureProfileContext, isCurrentProfileContext]
  );

  const loadProfiles = useCallback(
    async (options: LoadProfilesOptions = {}) => {
      const requestId = ++listRequestRef.current;
      const previousProfiles = profilesRef.current;
      setProfilesLoading(true);
      setProfilesError(null);
      setProfilesCapabilityRecovery(null);
      try {
        const page = await listMcpProfiles(showArchived);
        if (listRequestRef.current !== requestId) return;
        setProfilesCapability('available');
        profilesRef.current = page.items;
        setProfiles(page.items);
        const canReconcileSelection =
          editorModeRef.current === null &&
          (options.selectionContextVersion === null ||
            options.selectionContextVersion === undefined ||
            contextVersionRef.current === options.selectionContextVersion);

        if (!canReconcileSelection) return;

        const currentSelectedProfileId = selectedProfileIdRef.current;
        if (
          currentSelectedProfileId &&
          page.items.some((item) => item.profileId === currentSelectedProfileId)
        ) {
          return;
        }

        const preferredProfileId = options.preferredProfileId ?? null;
        const nextSelectedProfileId =
          preferredProfileId && page.items.some((item) => item.profileId === preferredProfileId)
            ? preferredProfileId
            : pickDeterministicNextProfileId(
                page.items,
                previousProfiles,
                currentSelectedProfileId ?? preferredProfileId
              );

        commitProfileContext({ selectedProfileId: nextSelectedProfileId });
      } catch (cause) {
        if (listRequestRef.current !== requestId) return;
        if (isMcpPhaseUnavailableError(cause, 'profiles')) {
          setProfilesCapability('unavailable');
          setProfilesCapabilityRecovery(toMcpRecoveryViewModel(cause));
          profilesRef.current = [];
          setProfiles([]);
          commitProfileContext({ selectedProfileId: null });
          setDetail(null);
          setDetailError(null);
          setDetailOperation(null);
        } else {
          setProfilesError(toMcpRecoveryViewModel(cause));
        }
      } finally {
        if (listRequestRef.current === requestId) {
          setProfilesLoading(false);
        }
      }
    },
    [commitProfileContext, showArchived]
  );

  useEffect(() => {
    void loadInventory();
  }, [loadInventory]);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

  useEffect(() => {
    if (!selectedProfileId || profilesCapability !== 'available') {
      setDetail(null);
      setDetailError(null);
      setDetailOperation(null);
      return;
    }
    void loadDetail(selectedProfileId);
  }, [loadDetail, profilesCapability, selectedProfileId]);

  useEffect(() => {
    setArchiveConfirmOpen(false);
    setRestoreRevision(null);
    setConnectionTestOpen(false);
  }, [editorMode, selectedProfileId, showArchived]);

  useEffect(() => {
    return () => {
      applyFlowRef.current += 1;
      applyPlanInFlightRef.current = false;
      applyConfirmInFlightRef.current = false;
      applyConfirmationRef.current = null;
      if (applyRedirectTimerRef.current !== null) {
        window.clearTimeout(applyRedirectTimerRef.current);
      }
    };
  }, []);

  const requestDiscard = (action: () => void) => {
    discardActionRef.current = action;
    setDiscardOpen(true);
  };

  const beginCreate = () => {
    const open = () => {
      commitProfileContext({ editorMode: 'create' });
      setForm(emptyFormState);
      setInitialForm(emptyFormState);
      setFormNameError(null);
      setCreateEditorError(null);
    };
    if (dirty) {
      requestDiscard(open);
      return;
    }
    open();
  };

  const beginEdit = () => {
    if (!detail) return;
    const nextForm = buildFormState(detail.profile);
    commitProfileContext({ editorMode: 'edit' });
    setForm(nextForm);
    setInitialForm(nextForm);
    setFormNameError(null);
    setCreateEditorError(null);
  };

  const cancelEditor = () => {
    const close = () => {
      commitProfileContext({ editorMode: null });
      setFormNameError(null);
      setCreateEditorError(null);
      if (detail) {
        const nextForm = buildFormState(detail.profile);
        setForm(nextForm);
        setInitialForm(nextForm);
      } else {
        setForm(emptyFormState);
        setInitialForm(emptyFormState);
      }
    };
    if (dirty) {
      requestDiscard(close);
      return;
    }
    close();
  };

  const selectProfile = (profileId: string) => {
    if (profileId === selectedProfileId && editorMode !== 'create') return;
    const applySelection = () => {
      commitProfileContext({
        selectedProfileId: profileId,
        editorMode: null,
      });
      setCreateEditorError(null);
      setFormNameError(null);
    };
    if (dirty) {
      requestDiscard(applySelection);
      return;
    }
    applySelection();
  };

  const closeApplyDialog = useCallback(() => {
    applyFlowRef.current += 1;
    applyConfirmInFlightRef.current = false;
    applyConfirmationRef.current = null;
    if (applyRedirectTimerRef.current !== null) {
      window.clearTimeout(applyRedirectTimerRef.current);
      applyRedirectTimerRef.current = null;
    }
    setApplyDialogState(null);
  }, []);

  const findProfileForApply = useCallback(
    (profileId: string): McpProfileSummary | null =>
      (detail?.profile.profileId === profileId ? detail.profile : null) ??
      profiles.find((profile) => profile.profileId === profileId) ??
      null,
    [detail, profiles]
  );

  const beginApply = useCallback(
    async (profile: McpProfileSummary, options?: { force?: boolean }) => {
      if (profile.archived || applyPlanInFlightRef.current || (applyBlocked && !options?.force)) {
        return;
      }

      applyPlanInFlightRef.current = true;
      const requestId = ++applyPlanRequestRef.current;
      const flowId = ++applyFlowRef.current;
      const operation: ProfileAsyncOperation = {
        ...captureProfileContext(),
        requestId,
        profileId: profile.profileId,
        profileName: profile.name,
      };
      setApplyPlanOperation(operation);
      clearProfileError(profile.profileId);
      applyConfirmationRef.current = null;
      setApplyDialogState(null);

      try {
        const plan = await createMcpProfileApplyPlan({
          profileId: profile.profileId,
          profileRevision: profile.revision,
        });
        if (
          applyPlanRequestRef.current !== requestId ||
          applyFlowRef.current !== flowId ||
          !isCurrentProfileContext(operation)
        ) {
          return;
        }
        const review = redactProfileApplyReview(plan);
        const confirmationToken = plan.confirmation.confirmationToken;
        const confirmationTokenAvailable = confirmationToken.trim().length > 0;
        applyConfirmationRef.current = confirmationTokenAvailable
          ? {
              flowId,
              planId: plan.planId,
              confirmationToken,
            }
          : null;
        setApplyDialogState({
          flowId,
          profileId: profile.profileId,
          profileName: profile.name,
          review,
          phase: confirmationTokenAvailable ? 'review' : 'error',
          confirmationTokenAvailable,
          recovery: confirmationTokenAvailable
            ? null
            : invalidProfileApplyConfirmationRecovery(intl),
          canRecreatePlan: !confirmationTokenAvailable,
        });
      } catch (cause) {
        if (
          applyPlanRequestRef.current !== requestId ||
          applyFlowRef.current !== flowId ||
          !isCurrentProfileContext(operation)
        ) {
          return;
        }
        applyConfirmationRef.current = null;
        setProfileError(
          profile.profileId,
          sanitizeOpaqueDisplayValue(profile.name),
          toMcpRecoveryViewModel(cause)
        );
      } finally {
        applyPlanInFlightRef.current = false;
        setApplyPlanOperation((current) => (current?.requestId === requestId ? null : current));
      }
    },
    [
      applyBlocked,
      captureProfileContext,
      clearProfileError,
      intl,
      isCurrentProfileContext,
      setProfileError,
    ]
  );

  const recreateApplyPlan = useCallback(() => {
    if (!applyDialogState) return;
    closeApplyDialog();
    const nextProfile = findProfileForApply(applyDialogState.profileId);
    if (!nextProfile) {
      return;
    }
    void beginApply(nextProfile, { force: true });
  }, [applyDialogState, beginApply, closeApplyDialog, findProfileForApply]);

  const resolveActiveSessionWorkingDir = useCallback(async (): Promise<string> => {
    if (!activeSessionId) {
      throw new Error('current_session_working_dir_unavailable');
    }

    const cachedWorkingDir = normalizeWorkingDir(
      acpChatSessionStore.getSnapshot(activeSessionId)?.session?.working_dir
    );
    if (cachedWorkingDir) {
      return cachedWorkingDir;
    }

    const session = await acpGetSessionListItem(activeSessionId);
    const resolvedWorkingDir = normalizeWorkingDir(session.workingDir);
    if (!resolvedWorkingDir) {
      throw new Error('current_session_working_dir_unavailable');
    }

    return resolvedWorkingDir;
  }, [activeSessionId]);

  const confirmApply = useCallback(async () => {
    const current = applyDialogState;
    if (applyConfirmInFlightRef.current || !current || current.phase !== 'review') {
      return;
    }

    const pendingConfirmation = applyConfirmationRef.current;
    if (!pendingConfirmation || pendingConfirmation.flowId !== current.flowId) {
      setApplyDialogState((state) =>
        state && state.flowId === current.flowId
          ? {
              ...state,
              phase: 'error',
              confirmationTokenAvailable: false,
              recovery: invalidProfileApplyConfirmationRecovery(intl),
              canRecreatePlan: true,
            }
          : state
      );
      return;
    }

    applyConfirmInFlightRef.current = true;
    const flowId = current.flowId;
    const { confirmationToken, planId } = pendingConfirmation;
    applyConfirmationRef.current = null;
    setApplyDialogState({
      ...current,
      phase: 'resolving-session',
      confirmationTokenAvailable: false,
      recovery: null,
      canRecreatePlan: false,
    });

    let activeWorkingDir: string;
    try {
      activeWorkingDir = await resolveActiveSessionWorkingDir();
    } catch {
      if (applyFlowRef.current !== flowId) {
        return;
      }
      applyConfirmInFlightRef.current = false;
      setApplyDialogState((state) =>
        state && state.flowId === flowId
          ? {
              ...state,
              phase: 'error',
              confirmationTokenAvailable: false,
              recovery: currentSessionWorkingDirUnavailableRecovery(intl),
              canRecreatePlan: true,
            }
          : state
      );
      return;
    }

    if (applyFlowRef.current !== flowId) {
      return;
    }
    setApplyDialogState((state) =>
      state && state.flowId === flowId
        ? {
            ...state,
            phase: 'confirming',
            confirmationTokenAvailable: false,
            recovery: null,
            canRecreatePlan: false,
          }
        : state
    );

    try {
      const application = await confirmMcpProfileApply({
        planId,
        confirmationToken,
        confirm: true,
      });

      if (applyFlowRef.current !== flowId) {
        return;
      }

      if (isProfileApplicationTokenExpired(application)) {
        setApplyDialogState((state) =>
          state && state.flowId === flowId
            ? {
                ...state,
                phase: 'error',
                confirmationTokenAvailable: false,
                recovery: invalidProfileApplyConfirmationRecovery(intl),
                canRecreatePlan: true,
              }
            : state
        );
        applyConfirmInFlightRef.current = false;
        return;
      }

      setApplyDialogState((state) =>
        state && state.flowId === flowId
          ? {
              ...state,
              phase: 'creating-session',
              confirmationTokenAvailable: false,
              recovery: null,
              canRecreatePlan: false,
            }
          : state
      );

      const session = await createSession(activeWorkingDir, {
        allExtensions: extensionsList,
        profileApplicationToken: application.token,
      });

      if (applyFlowRef.current !== flowId) {
        return;
      }

      setApplyDialogState((state) =>
        state && state.flowId === flowId
          ? {
              ...state,
              phase: 'success',
              confirmationTokenAvailable: false,
              recovery: null,
              canRecreatePlan: false,
            }
          : state
      );

      toast.success(
        intl.formatMessage(messages.profileApplySuccessToast, { name: current.profileName })
      );

      applyRedirectTimerRef.current = window.setTimeout(() => {
        window.dispatchEvent(
          new CustomEvent(AppEvents.SESSION_CREATED, {
            detail: { session },
          })
        );
        window.dispatchEvent(
          new CustomEvent(AppEvents.ADD_ACTIVE_SESSION, {
            detail: { sessionId: session.id, initialMessage: undefined },
          })
        );
        navigate(`/pair?resumeSessionId=${session.id}`);
        applyFlowRef.current += 1;
        applyRedirectTimerRef.current = null;
        applyConfirmInFlightRef.current = false;
        applyConfirmationRef.current = null;
        setApplyDialogState(null);
      }, 0);
    } catch (cause) {
      if (applyFlowRef.current !== flowId) {
        return;
      }
      applyConfirmInFlightRef.current = false;
      const recovery =
        cause instanceof McpPlatformServiceError
          ? toMcpRecoveryViewModel(cause)
          : mapProfileApplySessionRecovery(intl, cause);
      setApplyDialogState((state) =>
        state && state.flowId === flowId
          ? {
              ...state,
              phase: 'error',
              confirmationTokenAvailable: false,
              recovery,
              canRecreatePlan: true,
            }
          : state
      );
    }
  }, [applyDialogState, extensionsList, intl, navigate, resolveActiveSessionWorkingDir]);

  const toggleManagedSelection = (managedMcpId: string) => {
    setForm((current) => {
      const hasItem = current.managedMcpIds.includes(managedMcpId);
      return {
        ...current,
        managedMcpIds: hasItem
          ? current.managedMcpIds.filter((item) => item !== managedMcpId)
          : [...current.managedMcpIds, managedMcpId],
      };
    });
  };

  const validateForm = () => {
    const name = form.name.trim();
    if (!name) {
      setFormNameError(intl.formatMessage(messages.profileNameRequired));
      return false;
    }
    setFormNameError(null);
    return true;
  };

  const reloadAfterWrite = async (
    profileId: string,
    selectionContextVersion: number,
    retainFocus: boolean
  ) => {
    const preferredProfileId = retainFocus ? profileId : undefined;
    await loadProfiles({
      preferredProfileId,
      selectionContextVersion: retainFocus ? selectionContextVersion : null,
    });

    if (
      !retainFocus ||
      contextVersionRef.current !== selectionContextVersion ||
      selectedProfileIdRef.current !== profileId ||
      editorModeRef.current !== null
    ) {
      return;
    }

    await loadDetail(profileId);
  };

  const saveProfile = async () => {
    if (!validateForm()) return;
    const requestId = ++saveRequestRef.current;
    const profileId = editorMode === 'edit' && detail ? detail.profile.profileId : null;
    const profileName = profileId ? (detail?.profile.name ?? form.name.trim()) : form.name.trim();
    const operation =
      profileId !== null
        ? beginProfileMutation(requestId, profileId, profileName)
        : ({
            ...captureProfileContext(),
            requestId,
            profileId,
            profileName,
          } satisfies ProfileAsyncOperation);
    setSaveOperation(operation);
    setCreateEditorError(null);
    clearProfileError(profileId);
    try {
      const next =
        editorMode === 'edit' && detail
          ? await updateMcpProfile({
              profileId: detail.profile.profileId,
              expectedRevision: detail.profile.revision,
              name: form.name.trim(),
              description: form.description.trim(),
              managedMcpIds: form.managedMcpIds,
            })
          : await createMcpProfile({
              name: form.name.trim(),
              description: form.description.trim(),
              managedMcpIds: form.managedMcpIds,
            });
      if (profileId && !isLatestProfileMutation(operation)) {
        return;
      }
      clearProfileError(profileId ?? next.profileId);
      const retainFocus = isCurrentProfileContext(operation);
      const selectionContextVersion = retainFocus
        ? commitProfileContext({ editorMode: null })
        : contextVersionRef.current;
      await reloadAfterWrite(next.profileId, selectionContextVersion, retainFocus);
    } catch (cause) {
      const recovery = toMcpRecoveryViewModel(cause);
      if (profileId) {
        if (!isLatestProfileMutation(operation)) {
          return;
        }
        setProfileError(profileId, sanitizeOpaqueDisplayValue(profileName), recovery);
      } else if (isCurrentProfileContext(operation)) {
        setCreateEditorError(recovery);
      }
    } finally {
      setSaveOperation((current) => (current?.requestId === requestId ? null : current));
    }
  };

  const confirmArchive = async () => {
    if (!detail) return;
    const archivedProfileId = detail.profile.profileId;
    const requestId = ++archiveRequestRef.current;
    const operation = beginProfileMutation(requestId, archivedProfileId, detail.profile.name);
    setArchiveOperation(operation);
    setArchiveConfirmOpen(false);
    clearProfileError(archivedProfileId);
    setCreateEditorError(null);
    try {
      await archiveMcpProfile({
        profileId: archivedProfileId,
        expectedRevision: detail.profile.revision,
      });
      if (!isLatestProfileMutation(operation)) {
        return;
      }
      clearProfileError(archivedProfileId);
      const retainFocus = isCurrentProfileContext(operation);
      await reloadAfterWrite(
        archivedProfileId,
        retainFocus ? operation.contextVersion : contextVersionRef.current,
        retainFocus
      );
    } catch (cause) {
      if (!isLatestProfileMutation(operation)) {
        return;
      }
      setProfileError(
        archivedProfileId,
        sanitizeOpaqueDisplayValue(detail.profile.name),
        toMcpRecoveryViewModel(cause)
      );
    } finally {
      setArchiveOperation((current) => (current?.requestId === requestId ? null : current));
    }
  };

  const confirmRestore = async () => {
    if (!detail || !restoreRevision) return;
    const restoredProfileId = detail.profile.profileId;
    const requestedRevision = restoreRevision.revision;
    const requestId = ++restoreRequestRef.current;
    const operation = beginProfileMutation(requestId, restoredProfileId, detail.profile.name);
    setRestoreOperation(operation);
    setRestoreRevision(null);
    clearProfileError(restoredProfileId);
    setCreateEditorError(null);
    try {
      await restoreMcpProfile({
        profileId: restoredProfileId,
        sourceRevision: requestedRevision,
        expectedRevision: detail.profile.revision,
      });
      if (!isLatestProfileMutation(operation)) {
        return;
      }
      clearProfileError(restoredProfileId);
      const retainFocus = isCurrentProfileContext(operation);
      await reloadAfterWrite(
        restoredProfileId,
        retainFocus ? operation.contextVersion : contextVersionRef.current,
        retainFocus
      );
    } catch (cause) {
      if (!isLatestProfileMutation(operation)) {
        return;
      }
      setProfileError(
        restoredProfileId,
        sanitizeOpaqueDisplayValue(detail.profile.name),
        toMcpRecoveryViewModel(cause)
      );
    } finally {
      setRestoreOperation((current) => (current?.requestId === requestId ? null : current));
    }
  };

  const applyDraftToForm = () => {
    if (!draftResult) return;
    const apply = () => {
      const nextForm = {
        name: draftResult.name,
        description: draftResult.description,
        managedMcpIds: draftSelection,
      };
      commitProfileContext({ editorMode: editorMode === 'edit' ? 'edit' : 'create' });
      setForm(nextForm);
      setInitialForm(
        editorMode === 'edit' && detail ? buildFormState(detail.profile) : emptyFormState
      );
      setCreateEditorError(null);
      setFormNameError(null);
    };
    if (dirty) {
      requestDiscard(apply);
      return;
    }
    apply();
  };

  const runDraftSuggestion = async () => {
    if (!draftCanSubmit) return;
    const requestId = ++draftRequestRef.current;
    const operation: InputAsyncOperation = {
      requestId,
      text: normalizedDraftText,
      locale: intl.locale,
    };
    setDraftOperation(operation);
    setDraftError(null);
    setDraftResult(null);
    setDraftSelection([]);
    try {
      const nextDraft = await createMcpProfileDraft(operation.text, operation.locale);
      if (
        draftRequestRef.current !== requestId ||
        operation.text !== normalizedDraftText ||
        operation.locale !== intl.locale
      ) {
        return;
      }
      setDraftResult(nextDraft);
      setDraftSelection(
        nextDraft.entries
          .slice()
          .sort((left, right) => left.ordinal - right.ordinal)
          .map((entry) => entry.managedMcpId)
      );
    } catch (cause) {
      if (
        draftRequestRef.current !== requestId ||
        operation.text !== normalizedDraftText ||
        operation.locale !== intl.locale
      ) {
        return;
      }
      setDraftError(toMcpRecoveryViewModel(cause));
    } finally {
      setDraftOperation((current) => (current?.requestId === requestId ? null : current));
    }
  };

  const runModelSuggestion = async () => {
    if (!draftCanSubmit || modelCapability === 'unavailable') return;
    const requestId = ++modelRequestRef.current;
    const operation: InputAsyncOperation = {
      requestId,
      text: normalizedDraftText,
      locale: intl.locale,
      providerIds,
    };
    setModelOperation(operation);
    setModelError(null);
    setModelRecommendation(null);
    try {
      const nextRecommendation = await recommendMcpProfileModels(
        operation.text,
        operation.providerIds ?? []
      );
      if (
        modelRequestRef.current !== requestId ||
        operation.text !== normalizedDraftText ||
        operation.locale !== intl.locale ||
        JSON.stringify(operation.providerIds ?? []) !== JSON.stringify(providerIds)
      ) {
        return;
      }
      setModelCapability('available');
      setModelCapabilityRecovery(null);
      setModelRecommendation(nextRecommendation);
    } catch (cause) {
      if (
        modelRequestRef.current !== requestId ||
        operation.text !== normalizedDraftText ||
        operation.locale !== intl.locale ||
        JSON.stringify(operation.providerIds ?? []) !== JSON.stringify(providerIds)
      ) {
        return;
      }
      if (isMcpPhaseUnavailableError(cause, 'modelSuggestions')) {
        setModelCapability('unavailable');
        setModelCapabilityRecovery(toMcpRecoveryViewModel(cause));
        setModelRecommendation(null);
      } else {
        setModelError(toMcpRecoveryViewModel(cause));
      }
    } finally {
      setModelOperation((current) => (current?.requestId === requestId ? null : current));
    }
  };

  const inventorySelection = (
    <div className="rounded-lg border border-border-primary">
      {inventoryLoading ? (
        <div className="p-4">
          <StatePanel
            kind="loading"
            title={intl.formatMessage(messages.profileInventoryLoading)}
            description={intl.formatMessage(messages.profileInventoryReading)}
          />
        </div>
      ) : inventoryError ? (
        <div className="p-4">
          <RecoveryPanel recovery={inventoryError} onRetry={() => void loadInventory()} />
        </div>
      ) : inventory.length === 0 ? (
        <div className="p-4">
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.profileInventoryEmpty)}
            description={intl.formatMessage(messages.profileInventoryEmptyDescription)}
          />
        </div>
      ) : (
        <ScrollArea className="max-h-72">
          <div className="space-y-2 p-3">
            {inventory.map((item) => {
              const checked = form.managedMcpIds.includes(item.managedMcpId);
              return (
                <label
                  key={item.managedMcpId}
                  className="flex cursor-pointer gap-3 rounded-lg border border-border-primary bg-background-primary p-3 hover:bg-background-secondary"
                >
                  <input
                    type="checkbox"
                    className="mt-1 h-4 w-4 shrink-0"
                    checked={checked}
                    disabled={saveLoading}
                    onChange={() => toggleManagedSelection(item.managedMcpId)}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="flex flex-wrap items-center gap-2">
                      <span className="font-medium text-text-primary">{item.mcpId}</span>
                      <StatusBadge>{item.health}</StatusBadge>
                      <CredentialStatusBadge status={item.credentialStatus} />
                    </span>
                    <span className="mt-1 block truncate text-sm text-text-secondary">
                      {item.managedMcpId}
                    </span>
                  </span>
                </label>
              );
            })}
          </div>
        </ScrollArea>
      )}
    </div>
  );

  const profileListContent =
    profilesCapability === 'unavailable' ? (
      <CapabilityUnavailablePanel
        title={intl.formatMessage(messages.profileUnavailableTitle)}
        description={intl.formatMessage(messages.profileUnavailableDescription)}
        hint={
          inventoryHint
            ? intl.formatMessage(messages.profileCapabilityHint, { value: inventoryHint })
            : undefined
        }
        recovery={profilesCapabilityRecovery}
      />
    ) : showInitialProfilesLoading ? (
      <StatePanel
        kind="loading"
        title={intl.formatMessage(messages.profileListLoading)}
        description={intl.formatMessage(messages.profileListReading)}
      />
    ) : showBlockingProfilesError ? (
      <RecoveryPanel recovery={profilesError} onRetry={() => void loadProfiles()} />
    ) : profiles.length === 0 ? (
      <StatePanel
        kind="empty"
        title={intl.formatMessage(messages.profileListEmpty)}
        description={intl.formatMessage(messages.profileListEmptyDescription)}
        action={
          <Button variant="outline" onClick={beginCreate}>
            {intl.formatMessage(messages.createProfile)}
          </Button>
        }
      />
    ) : (
      <ScrollArea className="min-h-0 min-[1200px]:h-[calc(100dvh-260px)]">
        <div className="space-y-3 pr-2">
          {profiles.map((profile) => (
            <div
              key={profile.profileId}
              className="rounded-xl border border-border-primary bg-background-primary p-4"
            >
              <div className="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
                <button
                  onClick={() => selectProfile(profile.profileId)}
                  className="min-w-0 flex-1 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
                  aria-pressed={selectedProfileId === profile.profileId}
                >
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <h3 className="truncate font-medium text-text-primary" title={profile.name}>
                          {profile.name}
                        </h3>
                        {profile.archived && (
                          <StatusBadge tone="warning">
                            {intl.formatMessage(messages.archivedLabel)}
                          </StatusBadge>
                        )}
                      </div>
                      <p
                        className="mt-2 line-clamp-2 text-sm text-text-secondary"
                        title={
                          profile.description ||
                          intl.formatMessage(messages.profileDescriptionEmpty)
                        }
                      >
                        {profile.description ||
                          intl.formatMessage(messages.profileDescriptionEmpty)}
                      </p>
                    </div>
                    <div className="flex flex-wrap justify-end gap-1.5">
                      <CredentialStatusBadge status={profile.credentialStatus} />
                      <StatusBadge
                        title={intl.formatMessage(messages.profileRevisionBadge, {
                          revision: profile.revision,
                        })}
                      >
                        {intl.formatMessage(messages.profileRevisionBadge, {
                          revision: profile.revision,
                        })}
                      </StatusBadge>
                    </div>
                  </div>
                  <div className="mt-3 flex flex-wrap gap-2 text-xs text-text-tertiary">
                    <span>
                      {intl.formatMessage(messages.profileEntriesCount, {
                        count: profile.entries.length,
                      })}
                    </span>
                    <span>{formatProfileTimestamp(intl, profile.updatedAtMs)}</span>
                  </div>
                </button>

                <Button
                  className="w-full shrink-0 md:w-auto"
                  disabled={profile.archived || applyBlocked}
                  onClick={() => void beginApply(profile)}
                >
                  {applyLabelForProfile(profile.profileId)}
                </Button>
              </div>
            </div>
          ))}
        </div>
      </ScrollArea>
    );

  const hiddenProfileErrorsPanel =
    hiddenProfileErrors.length > 0 ? (
      <div className="space-y-3 rounded-xl border border-border-primary bg-background-primary p-4">
        <div>
          <h3 className="font-medium text-text-primary">
            {intl.formatMessage(messages.profileHiddenErrorsTitle)}
          </h3>
          <p className="mt-1 text-sm text-text-secondary">
            {intl.formatMessage(messages.profileHiddenErrorsDescription)}
          </p>
        </div>
        <div className="space-y-3">
          {hiddenProfileErrors.map((error) => (
            <div key={error.profileId} className="space-y-2">
              {error.profileName && (
                <p className="text-sm font-medium text-text-primary">
                  {intl.formatMessage(messages.profileHiddenErrorLabel, {
                    value: error.profileName,
                  })}
                </p>
              )}
              <RecoveryPanel recovery={error.recovery} />
            </div>
          ))}
        </div>
      </div>
    ) : null;

  const detailPanel = showInitialProfilesLoading ? (
    <StatePanel
      kind="loading"
      title={intl.formatMessage(messages.profileListLoading)}
      description={intl.formatMessage(messages.profileListReading)}
    />
  ) : showBlockingProfilesError ? (
    <RecoveryPanel recovery={profilesError} onRetry={() => void loadProfiles()} />
  ) : profilesCapability === 'unavailable' ? (
    <CapabilityUnavailablePanel
      title={intl.formatMessage(messages.profileUnavailableTitle)}
      description={intl.formatMessage(messages.profileUnavailableDescription)}
      hint={
        inventoryHint
          ? intl.formatMessage(messages.profileCapabilityHint, { value: inventoryHint })
          : undefined
      }
      recovery={profilesCapabilityRecovery}
    />
  ) : editorMode ? (
    <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-xl font-medium text-text-primary">
            {intl.formatMessage(
              editorMode === 'create' ? messages.profileCreateTitle : messages.profileEditTitle
            )}
          </h2>
          <p className="mt-1 max-w-2xl text-sm text-text-secondary">
            {intl.formatMessage(messages.profileEditorBoundary)}
          </p>
        </div>
        <StatusBadge tone="info">{intl.formatMessage(messages.profileDraftOnlyBadge)}</StatusBadge>
      </div>

      <CredentialAttentionPanel status={editorCredentialStatus} mode="note" />

      {editorError && (
        <RecoveryPanel
          recovery={editorError}
          onRetry={
            editorError.code === 'revision_conflict' && detail
              ? () => void loadDetail(detail.profile.profileId)
              : undefined
          }
        />
      )}

      <div className="space-y-1.5">
        <label htmlFor="profile-name" className="text-sm font-medium text-text-primary">
          {intl.formatMessage(messages.profileNameLabel)}
        </label>
        <Input
          id="profile-name"
          value={form.name}
          onChange={(event) => setForm((current) => ({ ...current, name: event.target.value }))}
          aria-describedby={formNameError ? 'profile-name-error' : undefined}
          aria-invalid={!!formNameError}
          disabled={saveLoading}
          placeholder={intl.formatMessage(messages.profileNamePlaceholder)}
        />
        {formNameError && (
          <p id="profile-name-error" className="text-sm text-text-danger">
            {formNameError}
          </p>
        )}
      </div>

      <div className="space-y-1.5">
        <label htmlFor="profile-description" className="text-sm font-medium text-text-primary">
          {intl.formatMessage(messages.profileDescriptionLabel)}
        </label>
        <textarea
          id="profile-description"
          value={form.description}
          onChange={(event) =>
            setForm((current) => ({ ...current, description: event.target.value }))
          }
          disabled={saveLoading}
          rows={4}
          className="flex w-full rounded-md border border-border-primary bg-background-primary px-3 py-2 text-sm text-text-primary placeholder:text-text-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
          placeholder={intl.formatMessage(messages.profileDescriptionPlaceholder)}
        />
      </div>

      <div className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <label className="text-sm font-medium text-text-primary">
            {intl.formatMessage(messages.profileManagedMcpLabel)}
          </label>
          <span className="text-xs text-text-tertiary">
            {intl.formatMessage(messages.profileSelectedEntriesCount, {
              count: form.managedMcpIds.length,
            })}
          </span>
        </div>
        {inventorySelection}
      </div>

      <div className="flex flex-wrap gap-2">
        <Button disabled={saveLoading} onClick={() => void saveProfile()}>
          {intl.formatMessage(
            saveLoading
              ? messages.profileSaving
              : editorMode === 'create'
                ? messages.profileCreateSubmit
                : messages.profileSaveSubmit
          )}
        </Button>
        <Button variant="outline" disabled={saveLoading} onClick={cancelEditor}>
          {intl.formatMessage(messages.cancel)}
        </Button>
      </div>
    </div>
  ) : !selectedProfileId ? (
    <StatePanel
      kind="empty"
      title={intl.formatMessage(messages.profileDetailEmpty)}
      description={intl.formatMessage(messages.profileDetailEmptyDescription)}
      action={
        <Button variant="outline" onClick={beginCreate}>
          {intl.formatMessage(messages.createProfile)}
        </Button>
      }
    />
  ) : detailLoading ? (
    <StatePanel
      kind="loading"
      title={intl.formatMessage(messages.profileDetailLoading)}
      description={intl.formatMessage(messages.profileDetailReading)}
    />
  ) : detailError ? (
    <RecoveryPanel recovery={detailError} onRetry={() => void loadDetail(selectedProfileId)} />
  ) : detail ? (
    <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2
              className="truncate text-xl font-medium text-text-primary"
              title={detail.profile.name}
            >
              {detail.profile.name}
            </h2>
            {detail.profile.archived && (
              <StatusBadge tone="warning">{intl.formatMessage(messages.archivedLabel)}</StatusBadge>
            )}
            <CredentialStatusBadge status={detailCredentialStatus} />
          </div>
          <p className="mt-2 text-sm text-text-secondary">
            {detail.profile.description || intl.formatMessage(messages.profileDescriptionEmpty)}
          </p>
          <p className="mt-3 text-sm text-text-tertiary">
            {intl.formatMessage(messages.profileEditorBoundary)}
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button
            disabled={detail.profile.archived || applyBlocked}
            onClick={() => void beginApply(detail.profile)}
          >
            {applyLabelForProfile(detail.profile.profileId)}
          </Button>
          <Button
            variant="outline"
            disabled={detail.profile.archived}
            onClick={() => setConnectionTestOpen(true)}
          >
            {intl.formatMessage(messages.profileConnectionTest)}
          </Button>
          <Button variant="outline" onClick={beginEdit}>
            <PencilLine /> {intl.formatMessage(messages.editProfile)}
          </Button>
          <Button
            variant="destructive"
            disabled={detail.profile.archived}
            onClick={() => setArchiveConfirmOpen(true)}
          >
            <Archive /> {intl.formatMessage(messages.archiveProfile)}
          </Button>
        </div>
      </div>

      {editorError && (
        <RecoveryPanel
          recovery={editorError}
          onRetry={
            editorError.code === 'revision_conflict'
              ? () => void loadDetail(detail.profile.profileId)
              : undefined
          }
        />
      )}

      <DefinitionList
        items={[
          [
            intl.formatMessage(messages.profileRevisionLabel),
            intl.formatMessage(messages.profileRevisionValue, {
              revision: detail.profile.revision,
            }),
          ],
          [
            intl.formatMessage(messages.profileCreatedAtLabel),
            formatProfileTimestamp(intl, detail.profile.createdAtMs),
          ],
          [
            intl.formatMessage(messages.profileUpdatedAtLabel),
            formatProfileTimestamp(intl, detail.profile.updatedAtMs),
          ],
          [
            intl.formatMessage(messages.profileEntriesLabel),
            intl.formatMessage(messages.profileEntriesCount, {
              count: detail.profile.entries.length,
            }),
          ],
          [
            intl.formatMessage(messages.credentialStatusLabel),
            <CredentialStatusBadge
              key="profile-credential-status"
              status={detailCredentialStatus}
            />,
          ],
        ]}
      />

      <CredentialAttentionPanel
        status={detailCredentialStatus}
        actions={[
          {
            label: intl.formatMessage(messages.credentialRefresh),
            onClick: () => void loadDetail(detail.profile.profileId),
          },
        ]}
      />

      <div>
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <h3 className="font-medium text-text-primary">
            {intl.formatMessage(messages.profileEntriesLabel)}
          </h3>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void loadDetail(detail.profile.profileId)}
          >
            <RefreshCw /> {intl.formatMessage(messages.refresh)}
          </Button>
        </div>
        {detail.profile.entries.length === 0 ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.profileEntriesEmpty)}
            description={intl.formatMessage(messages.profileEntriesEmptyDescription)}
          />
        ) : (
          <div className="space-y-2">
            {detail.profile.entries
              .slice()
              .sort((left, right) => left.ordinal - right.ordinal)
              .map((entry) => {
                const inventoryItem = inventoryById.get(entry.managedMcpId);
                const safeManagedMcpId = sanitizeOpaqueDisplayValue(entry.managedMcpId);
                return (
                  <div
                    key={`${entry.managedMcpId}:${entry.ordinal}`}
                    className="rounded-lg border border-border-primary bg-background-secondary/40 p-3"
                  >
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div className="min-w-0">
                        <p className="truncate font-medium text-text-primary">
                          {inventoryItem?.mcpId ?? safeManagedMcpId}
                        </p>
                        {safeManagedMcpId && (
                          <p className="truncate text-sm text-text-secondary">{safeManagedMcpId}</p>
                        )}
                      </div>
                      <StatusBadge>
                        {intl.formatMessage(messages.profileEntryOrdinal, {
                          ordinal: entry.ordinal + 1,
                        })}
                      </StatusBadge>
                    </div>
                  </div>
                );
              })}
          </div>
        )}
      </div>

      <div>
        <h3 className="mb-2 font-medium text-text-primary">
          {intl.formatMessage(messages.profileHistoryLabel)}
        </h3>
        {detail.history.length === 0 ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.profileHistoryEmpty)}
            description={intl.formatMessage(messages.profileHistoryEmptyDescription)}
          />
        ) : (
          <div className="space-y-2">
            {detail.history.map((revision) => {
              const canRestore = revision.revision !== detail.profile.revision;
              return (
                <div
                  key={revision.revision}
                  className="rounded-lg border border-border-primary bg-background-secondary/20 p-3"
                >
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="min-w-0">
                      <p className="font-medium text-text-primary">
                        {intl.formatMessage(messages.profileRevisionValue, {
                          revision: revision.revision,
                        })}
                      </p>
                      <p className="mt-1 text-sm text-text-secondary">
                        {intl.formatMessage(messages.profileHistorySummary, {
                          operation: revision.operation,
                        })}
                      </p>
                      <p className="mt-1 text-xs text-text-tertiary">
                        {formatProfileTimestamp(intl, revision.createdAtMs)}
                      </p>
                    </div>
                    {canRestore && (
                      <HistoryRestoreButton
                        revision={revision}
                        disabled={restorePending}
                        onRestore={setRestoreRevision}
                      />
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  ) : null;

  return (
    <>
      <div className="grid min-h-0 gap-5 min-[1200px]:grid-cols-[minmax(320px,0.4fr)_minmax(0,1fr)]">
        <section
          aria-label={intl.formatMessage(messages.profileListAria)}
          className="min-w-0 space-y-4"
        >
          <div className="rounded-xl border border-border-primary bg-background-primary p-4">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-text-secondary">
                  <FileStack className="h-4 w-4" />
                  <span className="text-sm font-medium">
                    {intl.formatMessage(messages.profileTab)}
                  </span>
                </div>
                <p className="mt-2 text-sm text-text-secondary">
                  {intl.formatMessage(messages.profileTabDescription)}
                </p>
              </div>
              <Button
                onClick={beginCreate}
                disabled={profilesCapability === 'unavailable' || profilesLoading}
              >
                {intl.formatMessage(messages.createProfile)}
              </Button>
            </div>
            <div className="mt-4 flex flex-wrap items-center gap-3">
              <label className="flex items-center gap-2 text-sm text-text-primary">
                <input
                  type="checkbox"
                  checked={showArchived}
                  onChange={(event) => commitProfileContext({ showArchived: event.target.checked })}
                />
                {intl.formatMessage(messages.showArchived)}
              </label>
              <Button variant="outline" size="sm" onClick={() => void loadProfiles()}>
                <RefreshCw /> {intl.formatMessage(messages.refresh)}
              </Button>
            </div>
          </div>
          {hiddenProfileErrorsPanel}
          {profileListContent}
        </section>

        <section
          aria-label={intl.formatMessage(messages.profileDetailAria)}
          className="min-w-0 space-y-5"
        >
          {detailPanel}

          {profilesCapability === 'available' && (
            <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <div className="flex items-center gap-2">
                    <Sparkles className="h-4 w-4 text-text-secondary" />
                    <h2 className="text-lg font-medium text-text-primary">
                      {intl.formatMessage(messages.profileSuggestionsTitle)}
                    </h2>
                  </div>
                  <p className="mt-1 text-sm text-text-secondary">
                    {intl.formatMessage(messages.profileSuggestionsDescription)}
                  </p>
                </div>
                <StatusBadge tone="info">
                  {intl.formatMessage(messages.profileDraftOnlyBadge)}
                </StatusBadge>
              </div>

              <div className="space-y-1.5">
                <label htmlFor="profile-work" className="text-sm font-medium text-text-primary">
                  {intl.formatMessage(messages.profileSuggestionsInputLabel)}
                </label>
                <textarea
                  id="profile-work"
                  value={draftText}
                  onChange={(event) => setDraftText(event.target.value)}
                  rows={4}
                  className="flex w-full rounded-md border border-border-primary bg-background-primary px-3 py-2 text-sm text-text-primary placeholder:text-text-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
                  placeholder={intl.formatMessage(messages.profileSuggestionsInputPlaceholder)}
                />
              </div>

              <div className="flex flex-wrap gap-2">
                <Button
                  disabled={!draftCanSubmit || draftLoading}
                  onClick={() => void runDraftSuggestion()}
                >
                  <Wand2 />{' '}
                  {intl.formatMessage(
                    draftLoading ? messages.profileDraftGenerating : messages.profileDraftGenerate
                  )}
                </Button>
              </div>

              {draftError && (
                <RecoveryPanel recovery={draftError} onRetry={() => void runDraftSuggestion()} />
              )}

              {draftResult && (
                <div className="space-y-4 rounded-lg border border-border-primary p-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div>
                      <h3 className="font-medium text-text-primary">{draftResult.name}</h3>
                      <p className="mt-1 text-sm text-text-secondary">{draftResult.description}</p>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {draftResult.lowConfidence && (
                        <StatusBadge tone="warning">
                          {intl.formatMessage(messages.profileLowConfidence)}
                        </StatusBadge>
                      )}
                      <StatusBadge tone={draftResult.persisted ? 'success' : 'neutral'}>
                        {intl.formatMessage(
                          draftResult.persisted
                            ? messages.profileDraftPersisted
                            : messages.profileDraftNotPersisted
                        )}
                      </StatusBadge>
                    </div>
                  </div>

                  {draftResult.unresolvedTerms.length > 0 && (
                    <div>
                      <h4 className="font-medium text-text-primary">
                        {intl.formatMessage(messages.profileUnresolvedTerms)}
                      </h4>
                      <div className="mt-2 flex flex-wrap gap-1.5">
                        {draftResult.unresolvedTerms.map((term) => (
                          <StatusBadge key={term} tone="warning">
                            {term}
                          </StatusBadge>
                        ))}
                      </div>
                    </div>
                  )}

                  <div className="space-y-2">
                    <h4 className="font-medium text-text-primary">
                      {intl.formatMessage(messages.profileDraftCandidates)}
                    </h4>
                    {draftResult.candidates.length === 0 ? (
                      <StatePanel
                        kind="empty"
                        title={intl.formatMessage(messages.profileDraftCandidatesEmpty)}
                        description={intl.formatMessage(
                          messages.profileDraftCandidatesEmptyDescription
                        )}
                      />
                    ) : (
                      draftResult.candidates.map((candidate) => (
                        <label
                          key={candidate.managedMcpId}
                          className="flex cursor-pointer gap-3 rounded-lg border border-border-primary p-3 hover:bg-background-secondary"
                        >
                          <input
                            type="checkbox"
                            className="mt-1 h-4 w-4 shrink-0"
                            checked={draftSelection.includes(candidate.managedMcpId)}
                            onChange={() =>
                              setDraftSelection((current) =>
                                current.includes(candidate.managedMcpId)
                                  ? current.filter((item) => item !== candidate.managedMcpId)
                                  : [...current, candidate.managedMcpId]
                              )
                            }
                          />
                          <span className="min-w-0 flex-1">
                            <span className="flex flex-wrap items-center gap-2">
                              <span className="font-medium text-text-primary">
                                {candidate.name}
                              </span>
                              <StatusBadge tone="info">
                                {intl.formatMessage(messages.profileConfidenceValue, {
                                  value: Math.round(candidate.confidence * 100),
                                })}
                              </StatusBadge>
                            </span>
                            <span className="mt-1 block truncate text-sm text-text-secondary">
                              {candidate.mcpId}
                            </span>
                            <span className="mt-1 block text-sm text-text-secondary">
                              {candidate.description}
                            </span>
                            <span className="mt-1 block text-xs text-text-tertiary">
                              {intl.formatMessage(messages.profileReasonCode, {
                                value: candidate.reasonCode,
                              })}
                            </span>
                          </span>
                        </label>
                      ))
                    )}
                  </div>

                  <Button variant="outline" onClick={applyDraftToForm}>
                    {intl.formatMessage(messages.profileApplyDraft)}
                  </Button>
                </div>
              )}

              {modelCapability === 'unavailable' ? (
                <CapabilityUnavailablePanel
                  title={intl.formatMessage(messages.profileModelUnavailableTitle)}
                  description={intl.formatMessage(messages.profileModelUnavailableDescription)}
                  hint={
                    modelCapabilityHint
                      ? intl.formatMessage(messages.profileCapabilityHint, {
                          value: modelCapabilityHint,
                        })
                      : undefined
                  }
                  recovery={modelCapabilityRecovery}
                />
              ) : (
                <div className="space-y-4 rounded-lg border border-border-primary p-4">
                  <div className="space-y-1.5">
                    <label
                      htmlFor="profile-provider-filter"
                      className="text-sm font-medium text-text-primary"
                    >
                      {intl.formatMessage(messages.profileProviderFilterLabel)}
                    </label>
                    <Input
                      id="profile-provider-filter"
                      value={providerFilter}
                      onChange={(event) => setProviderFilter(event.target.value)}
                      placeholder={intl.formatMessage(messages.profileProviderFilterPlaceholder)}
                    />
                    <p className="text-sm text-text-secondary">
                      {intl.formatMessage(messages.profileProviderFilterDescription)}
                    </p>
                  </div>

                  <Button
                    disabled={!draftCanSubmit || modelLoading}
                    onClick={() => void runModelSuggestion()}
                  >
                    <Sparkles />{' '}
                    {intl.formatMessage(
                      modelLoading
                        ? messages.profileModelsGenerating
                        : messages.profileModelsGenerate
                    )}
                  </Button>

                  {modelError && (
                    <RecoveryPanel
                      recovery={modelError}
                      onRetry={() => void runModelSuggestion()}
                    />
                  )}

                  {modelRecommendation && (
                    <div className="space-y-3">
                      <div className="flex flex-wrap gap-2">
                        <StatusBadge tone={modelRecommendation.inventoryOnly ? 'info' : 'neutral'}>
                          {intl.formatMessage(
                            modelRecommendation.inventoryOnly
                              ? messages.profileModelInventoryOnly
                              : messages.profileModelNotInventoryOnly
                          )}
                        </StatusBadge>
                        <StatusBadge tone={modelRecommendation.mutated ? 'warning' : 'success'}>
                          {intl.formatMessage(
                            modelRecommendation.mutated
                              ? messages.profileModelMutated
                              : messages.profileModelNotMutated
                          )}
                        </StatusBadge>
                      </div>

                      {modelRecommendation.candidates.length === 0 ? (
                        <StatePanel
                          kind="empty"
                          title={intl.formatMessage(messages.profileModelsEmpty)}
                          description={intl.formatMessage(messages.profileModelsEmptyDescription)}
                        />
                      ) : (
                        modelRecommendation.candidates.map((candidate) => (
                          <div
                            key={`${candidate.providerId}:${candidate.modelId}`}
                            className="rounded-lg border border-border-primary p-3"
                          >
                            <div className="flex flex-wrap items-start justify-between gap-2">
                              <div className="min-w-0">
                                <p className="truncate font-medium text-text-primary">
                                  {candidate.providerId} / {candidate.modelId}
                                </p>
                                <p className="mt-1 text-xs text-text-tertiary">
                                  {intl.formatMessage(messages.profileConfidenceValue, {
                                    value: Math.round(candidate.confidence * 100),
                                  })}
                                </p>
                              </div>
                            </div>
                            <div className="mt-3 flex flex-wrap gap-1.5">
                              {candidate.reasonCodes.map((reason) => (
                                <StatusBadge key={reason} tone="success">
                                  {reason}
                                </StatusBadge>
                              ))}
                              {candidate.caveatCodes.map((caveat) => (
                                <StatusBadge key={caveat} tone="warning">
                                  {caveat}
                                </StatusBadge>
                              ))}
                            </div>
                          </div>
                        ))
                      )}
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </section>
      </div>

      <ConfirmationModal
        isOpen={archiveConfirmOpen}
        title={intl.formatMessage(messages.archiveProfileTitle)}
        message={intl.formatMessage(messages.archiveProfileMessage)}
        detail={
          <div className="space-y-2 text-sm text-text-secondary">
            <p>{intl.formatMessage(messages.archiveProfileImpact)}</p>
            <p>{intl.formatMessage(messages.archiveProfileNoApply)}</p>
          </div>
        }
        confirmVariant="destructive"
        confirmLabel={intl.formatMessage(messages.archiveProfileConfirm)}
        cancelLabel={intl.formatMessage(messages.cancel)}
        isSubmitting={archiveSubmitting}
        onConfirm={() => void confirmArchive()}
        onCancel={() => setArchiveConfirmOpen(false)}
      />

      <ConfirmationModal
        isOpen={!!restoreRevision}
        title={intl.formatMessage(messages.restoreProfileTitle)}
        message={intl.formatMessage(messages.restoreProfileMessage, {
          revision: restoreRevision?.revision ?? 0,
        })}
        detail={
          <div className="space-y-2 text-sm text-text-secondary">
            <p>{intl.formatMessage(messages.restoreProfileImpact)}</p>
            <p>{intl.formatMessage(messages.archiveProfileNoApply)}</p>
          </div>
        }
        confirmLabel={intl.formatMessage(messages.restoreProfileConfirm)}
        cancelLabel={intl.formatMessage(messages.cancel)}
        isSubmitting={restorePending}
        onConfirm={() => void confirmRestore()}
        onCancel={() => setRestoreRevision(null)}
      />

      <ConfirmationModal
        isOpen={discardOpen}
        title={intl.formatMessage(messages.profileDiscardTitle)}
        message={intl.formatMessage(messages.profileDiscardMessage)}
        confirmLabel={intl.formatMessage(messages.profileDiscardConfirm)}
        cancelLabel={intl.formatMessage(messages.cancel)}
        onConfirm={() => {
          const action = discardActionRef.current;
          discardActionRef.current = null;
          setDiscardOpen(false);
          action?.();
        }}
        onCancel={() => {
          discardActionRef.current = null;
          setDiscardOpen(false);
        }}
      />

      <ProfileApplyReviewDialog
        state={applyDialogState}
        onClose={closeApplyDialog}
        onConfirm={() => void confirmApply()}
        onRecreatePlan={recreateApplyPlan}
      />
      {detail && (
        <ProfileConnectionTestDialog
          profile={detail.profile}
          open={connectionTestOpen}
          onOpenChange={setConnectionTestOpen}
        />
      )}
    </>
  );
}
