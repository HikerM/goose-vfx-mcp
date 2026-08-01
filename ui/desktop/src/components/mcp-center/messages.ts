import { defineMessages } from '../../i18n';

export const mcpCenterMessages = defineMessages({
  retry: { id: 'mcpCenter.retry', defaultMessage: 'Retry' },
  reference: { id: 'mcpCenter.reference', defaultMessage: 'Reference: {value}' },
  valueLabel: {
    id: 'mcpCenter.valueLabel',
    defaultMessage:
      '{value, select, absent {Absent} registered {Registered} not_applicable {Not applicable} not_installed {Not installed} staged {Staged} installed {Installed} update_available {Update available} repair_required {Repair required} uninstall_pending {Uninstall pending} stopped {Stopped} starting {Starting} running {Running} stopping {Stopping} crashed {Crashed} unknown {Unknown} checking {Checking} healthy {Healthy} degraded {Degraded} unhealthy {Unhealthy} blocked_auth {Authentication blocked} compatible {Compatible} incompatible {Incompatible} allowed {Allowed} restricted {Restricted} denied {Denied} needs_confirmation {Needs confirmation} allow {Allowed} deny {Denied} eligible {Eligible} confirmation_required {Confirmation required} platform_unsupported {Platform unsupported} runtime_unavailable {Runtime unavailable} policy_denied {Policy denied} development_mode_required {Development mode required} external_capability_unavailable {External capability unavailable} official {Official} community {Community} local {Local} local_persistence {Local manifest file} verified_source_catalog {Signed source catalog directory} https_manifest_url {HTTPS manifest URL} enterprise_directory {Enterprise directory} manifest {Manifest} directory {Directory} remote_http {Remote HTTP} manual_stdio {Manual stdio} stdio_provider {Approved stdio provider} npm {npm package} python_wheel {Python wheel} binary_archive {Binary archive} docker {Docker} git_dev {Git development source} stdio {Standard input/output} streamable_http {Streamable HTTP} none {None} api_key_header {API key header} environment {Environment credential} oauth2 {OAuth 2} mcp_initialize {MCP initialization} mcp_list_tools {MCP tool listing} http {HTTP health endpoint} tools {Tools} resources {Resources} prompts {Prompts} sampling {Sampling} elicitation {Elicitation} logging {Logging} filesystem_read {Read files} filesystem_write {Write files} network {Network access} credentials {Credentials} process_spawn {Start processes} host_application {Host application} register {Register} install {Install} update {Update} repair {Repair} uninstall {Uninstall} health {Health check} planned {Planned} awaiting_confirmation {Awaiting confirmation} queued {Queued} cancelling {Cancelling} verifying {Verifying} activating {Activating} rolling_back {Rolling back} succeeded {Succeeded} failed {Failed} cancelled {Cancelled} interrupted {Interrupted} recovery_required {Recovery required} pending {Pending} complete {Complete} incomplete {Incomplete} not_required {Not required} in_progress {In progress} wait {Wait} cancel_when_safe {Cancel when safe} retry {Retry} resume {Resume} resolve_recovery {Resolve recovery} recreate_plan {Create a new plan} enable_after_health {Enable after health check} wait_for_task {Wait for task} resume_task {Resume task} registration_only {Registration only} task_recovery_required {Task recovery required} task_in_progress {Task in progress} task_interrupted {Task interrupted} no_update_available {No update available} review_permissions {Review permissions} choose_compatible_release {Choose a compatible release} install_required_runtime {Install the required runtime} enable_development_mode {Enable development mode} restore_verified_cache {Restore verified cache} contact_policy_administrator {Contact the policy administrator} fresh {Fresh} stale {Stale} offline_verified {Verified offline} empty {Empty} refreshing {Refreshing} idle {Idle} local_only {Local only} default_disabled {Default disabled} managed_artifact_download {Managed artifact download} existing_version_retained_until_commit {Existing version retained until commit} removes_owned_files_only {Removes owned files only} immutable_container_image {Immutable container image} writable_container_mount {Writable container mount} development_source_pinned_commit_no_build {Development source pinned to a commit without a build} policy {Policy} permission {Permission} artifact {Artifact} unavailable {Unavailable} available_after_materialization {Available after materialization} no_artifact_for_registration {No artifact for registration} remove_connection_registration {Remove connection registration} staged_activation_restores_previous_version {Restore the previous staged version} repair_restores_verified_owned_content {Restore verified owned content} uninstall_removes_owned_files {Remove owned files} projection_consistent {Projection consistent} projection_drift {Projection drift} credential_handle_missing {Credential reference missing} expected_status {Expected status} unexpected_status {Unexpected status} mcp_initialize_succeeded {MCP initialization succeeded} mcp_initialize_failed {MCP initialization failed} mcp_list_tools_succeeded {MCP tool listing succeeded} mcp_list_tools_failed {MCP tool listing failed} incompatible_health_contract {Incompatible health contract} cleanup_failed {Cleanup failed} timeout {Timed out} adapter_failed {Adapter failed} verification_failed {Verification failed} activation_failed {Activation failed} rollback_failed {Rollback failed} connection_projection {Connection projection} managed_installation {Managed installation} managed_uninstall {Managed uninstall} external_resource {External resource} other {Unknown or unsupported status}}',
  },
  managedPlatform: { id: 'mcpCenter.managedPlatform', defaultMessage: 'Managed MCP Platform' },
  title: { id: 'mcpCenter.title', defaultMessage: 'MCP Center' },
  subtitle: {
    id: 'mcpCenter.subtitle',
    defaultMessage:
      'Browse the verified offline MCP catalog stored on this device, review every plan, and manage health without exposing credentials or execution details.',
  },
  sectionsLabel: { id: 'mcpCenter.sectionsLabel', defaultMessage: 'MCP Center sections' },
  discover: { id: 'mcpCenter.discover', defaultMessage: 'Discover' },
  myMcps: { id: 'mcpCenter.myMcps', defaultMessage: 'My MCPs' },
  manual: { id: 'mcpCenter.manual', defaultMessage: 'Manual' },
  sourcePolicy: { id: 'mcpCenter.sourcePolicy', defaultMessage: 'Source & Policy' },
  catalogLabel: {
    id: 'mcpCenter.catalogLabel',
    defaultMessage: 'Verified offline MCP catalog',
  },
  catalogDescription: {
    id: 'mcpCenter.catalogDescription',
    defaultMessage:
      'This device-only catalog shows MCPs that have already been verified and stored locally. Filters narrow local results only; this page does not search the internet.',
  },
  searchCatalog: {
    id: 'mcpCenter.searchCatalog',
    defaultMessage: 'Filter the local MCP catalog',
  },
  searchMcps: { id: 'mcpCenter.searchMcps', defaultMessage: 'Filter local catalog' },
  trustLevel: { id: 'mcpCenter.trustLevel', defaultMessage: 'Trust level' },
  allTrustLevels: { id: 'mcpCenter.allTrustLevels', defaultMessage: 'All trust levels' },
  official: { id: 'mcpCenter.official', defaultMessage: 'Official' },
  community: { id: 'mcpCenter.community', defaultMessage: 'Community' },
  local: { id: 'mcpCenter.local', defaultMessage: 'Local' },
  compatibility: { id: 'mcpCenter.compatibility', defaultMessage: 'Compatibility' },
  publisher: { id: 'mcpCenter.publisher', defaultMessage: 'Publisher' },
  version: { id: 'mcpCenter.version', defaultMessage: 'Version' },
  trust: { id: 'mcpCenter.trust', defaultMessage: 'Trust' },
  verifiedAt: { id: 'mcpCenter.verifiedAt', defaultMessage: 'Verified' },
  distributionAdapter: {
    id: 'mcpCenter.distributionAdapter',
    defaultMessage: 'Distribution adapter',
  },
  transport: { id: 'mcpCenter.transport', defaultMessage: 'Transport' },
  healthContract: { id: 'mcpCenter.healthContract', defaultMessage: 'Health contract' },
  eligibility: { id: 'mcpCenter.eligibility', defaultMessage: 'Eligibility' },
  policyConclusion: {
    id: 'mcpCenter.policyConclusion',
    defaultMessage: 'Policy conclusion',
  },
  policyReason: { id: 'mcpCenter.policyReason', defaultMessage: 'Policy reason' },
  allCompatibility: { id: 'mcpCenter.allCompatibility', defaultMessage: 'All compatibility' },
  compatible: { id: 'mcpCenter.compatible', defaultMessage: 'Compatible' },
  incompatible: { id: 'mcpCenter.incompatible', defaultMessage: 'Incompatible' },
  catalogSource: { id: 'mcpCenter.catalogSource', defaultMessage: 'Catalog source' },
  allSources: { id: 'mcpCenter.allSources', defaultMessage: 'All sources' },
  loadingCatalog: {
    id: 'mcpCenter.loadingCatalog',
    defaultMessage: 'Loading verified local catalog',
  },
  readingVerifiedSources: {
    id: 'mcpCenter.readingVerifiedSources',
    defaultMessage: 'Reading verified MCP records stored on this device.',
  },
  noMcpsFound: { id: 'mcpCenter.noMcpsFound', defaultMessage: 'No MCPs found' },
  noMcpsFoundDescription: {
    id: 'mcpCenter.noMcpsFoundDescription',
    defaultMessage: 'No locally stored verified entries are currently available on this device.',
  },
  noCatalogMatchesDescription: {
    id: 'mcpCenter.noCatalogMatchesDescription',
    defaultMessage:
      'No locally stored verified entries match the current filters. Clear filters or choose another local source.',
  },
  noLocalCatalogSources: {
    id: 'mcpCenter.noLocalCatalogSources',
    defaultMessage: 'No verified local catalog sources',
  },
  noLocalCatalogSourcesDescription: {
    id: 'mcpCenter.noLocalCatalogSourcesDescription',
    defaultMessage:
      'This device does not currently have any verified offline catalog sources to browse.',
  },
  localCatalogEmpty: {
    id: 'mcpCenter.localCatalogEmpty',
    defaultMessage: 'The local catalog is empty',
  },
  localCatalogEmptyDescription: {
    id: 'mcpCenter.localCatalogEmptyDescription',
    defaultMessage:
      'Verified local sources exist, but they do not currently contain any catalog entries.',
  },
  catalogRestrictedByPolicy: {
    id: 'mcpCenter.catalogRestrictedByPolicy',
    defaultMessage: 'Catalog entries are hidden by device safety policy',
  },
  catalogRestrictedByPolicyDescription: {
    id: 'mcpCenter.catalogRestrictedByPolicyDescription',
    defaultMessage:
      'This device has verified local sources, but policy is hiding their entries to avoid unreviewed access. Ask an administrator if you need broader availability.',
  },
  loadMore: { id: 'mcpCenter.loadMore', defaultMessage: 'Load more' },
  loading: { id: 'mcpCenter.loading', defaultMessage: 'Loading…' },
  selected: { id: 'mcpCenter.selected', defaultMessage: 'Selected' },
  detailsLabel: { id: 'mcpCenter.detailsLabel', defaultMessage: 'MCP details' },
  detailsTitle: { id: 'mcpCenter.detailsTitle', defaultMessage: 'MCP detail' },
  backToResults: { id: 'mcpCenter.backToResults', defaultMessage: 'Back to results' },
  selectMcp: { id: 'mcpCenter.selectMcp', defaultMessage: 'Select an MCP' },
  selectMcpDescription: {
    id: 'mcpCenter.selectMcpDescription',
    defaultMessage: 'Choose a catalog card to review its locally verified details and eligibility.',
  },
  loadingDetails: { id: 'mcpCenter.loadingDetails', defaultMessage: 'Loading details' },
  readingManifest: {
    id: 'mcpCenter.readingManifest',
    defaultMessage: 'Reading the selected manifest.',
  },
  refreshingDetails: {
    id: 'mcpCenter.refreshingDetails',
    defaultMessage: 'Refreshing verified details',
  },
  refreshingDetailsDescription: {
    id: 'mcpCenter.refreshingDetailsDescription',
    defaultMessage:
      'The latest verified detail is loading while the current safe detail stays visible.',
  },
  capabilities: { id: 'mcpCenter.capabilities', defaultMessage: 'Capabilities' },
  permissionsPolicy: {
    id: 'mcpCenter.permissionsPolicy',
    defaultMessage: 'Permissions and policy',
  },
  noPermissions: { id: 'mcpCenter.noPermissions', defaultMessage: 'No declared permissions.' },
  createInstallPlan: { id: 'mcpCenter.createInstallPlan', defaultMessage: 'Create install plan' },
  createRegistrationPlan: {
    id: 'mcpCenter.createRegistrationPlan',
    defaultMessage: 'Create registration plan',
  },
  creatingPlan: { id: 'mcpCenter.creatingPlan', defaultMessage: 'Creating plan…' },
  catalogStatusFreshTitle: {
    id: 'mcpCenter.catalogStatusFreshTitle',
    defaultMessage: 'Using the verified local catalog stored on this device',
  },
  catalogStatusFreshDescription: {
    id: 'mcpCenter.catalogStatusFreshDescription',
    defaultMessage: 'Results are coming from the current verified local cache for this device.',
  },
  catalogStatusRefreshingTitle: {
    id: 'mcpCenter.catalogStatusRefreshingTitle',
    defaultMessage: 'Refreshing the verified local catalog',
  },
  catalogStatusRefreshingDescription: {
    id: 'mcpCenter.catalogStatusRefreshingDescription',
    defaultMessage:
      'Current verified results remain available while this device checks for a newer local copy.',
  },
  catalogStatusOfflineTitle: {
    id: 'mcpCenter.catalogStatusOfflineTitle',
    defaultMessage: 'Using a verified offline local cache',
  },
  catalogStatusOfflineDescription: {
    id: 'mcpCenter.catalogStatusOfflineDescription',
    defaultMessage:
      'This device is showing verified local catalog results that were saved earlier. Some installs or refreshes may be limited until a newer local copy is available.',
  },
  catalogStatusStaleTitle: {
    id: 'mcpCenter.catalogStatusStaleTitle',
    defaultMessage: 'Showing a stale verified local cache',
  },
  catalogStatusStaleDescription: {
    id: 'mcpCenter.catalogStatusStaleDescription',
    defaultMessage:
      'The verified local catalog is older than preferred, so some entries or updates may be missing until this device refreshes its local copy.',
  },
  catalogStatusRefreshFailedTitle: {
    id: 'mcpCenter.catalogStatusRefreshFailedTitle',
    defaultMessage: 'Catalog refresh failed, but verified results remain available',
  },
  catalogStatusRefreshFailedDescription: {
    id: 'mcpCenter.catalogStatusRefreshFailedDescription',
    defaultMessage:
      'The latest refresh did not complete. MCP Center is still showing the last verified results already stored on this device.',
  },
  lastVerified: { id: 'mcpCenter.lastVerified', defaultMessage: 'Last verified' },
  detailIntegrityTitle: {
    id: 'mcpCenter.detailIntegrityTitle',
    defaultMessage: 'The selected MCP detail could not be verified',
  },
  detailIntegrityDescription: {
    id: 'mcpCenter.detailIntegrityDescription',
    defaultMessage:
      'MCP Center blocked this detail response because it did not match the exact catalog item you selected.',
  },
  detailIntegrityNext: {
    id: 'mcpCenter.detailIntegrityNext',
    defaultMessage: 'Return to the catalog results and choose the MCP again before continuing.',
  },
  reviewIntegrityTitle: {
    id: 'mcpCenter.reviewIntegrityTitle',
    defaultMessage: 'The reviewed plan no longer matches the selected catalog item',
  },
  reviewIntegrityDescription: {
    id: 'mcpCenter.reviewIntegrityDescription',
    defaultMessage:
      'MCP Center blocked confirmation because the returned review did not match the exact catalog item you selected.',
  },
  reviewIntegrityNext: {
    id: 'mcpCenter.reviewIntegrityNext',
    defaultMessage:
      'Go back to the catalog results, reopen the MCP detail, and create a new reviewed plan.',
  },
  manualTypeLabel: {
    id: 'mcpCenter.manualTypeLabel',
    defaultMessage: 'Manual MCP connection type',
  },
  remoteHttp: { id: 'mcpCenter.remoteHttp', defaultMessage: 'Remote HTTP' },
  approvedStdio: { id: 'mcpCenter.approvedStdio', defaultMessage: 'Approved stdio provider' },
  customStdio: { id: 'mcpCenter.customStdio', defaultMessage: 'Custom MCP' },
  customStdioTitle: {
    id: 'mcpCenter.customStdioTitle',
    defaultMessage: 'Custom MCP connections',
  },
  customStdioDescription: {
    id: 'mcpCenter.customStdioDescription',
    defaultMessage:
      'Add local stdio commands or Streamable HTTP endpoints. Configure arguments, working directories, environment variables, or HTTP headers as needed.',
  },
  customStdioHealthAvailable: {
    id: 'mcpCenter.customStdioHealthAvailable',
    defaultMessage:
      'After enabling an MCP, check its tool discovery from the card. Export a diagnostic report if the check fails.',
  },
  customStdioHealthUnavailable: {
    id: 'mcpCenter.customStdioHealthUnavailable',
    defaultMessage: 'Open a chat first to check MCP tool discovery or export a diagnostic report.',
  },
  exportMcpDiagnostics: {
    id: 'mcpCenter.exportMcpDiagnostics',
    defaultMessage: 'Export MCP diagnostic report',
  },
  remoteConnection: { id: 'mcpCenter.remoteConnection', defaultMessage: 'Remote HTTP connection' },
  remoteConnectionDescription: {
    id: 'mcpCenter.remoteConnectionDescription',
    defaultMessage:
      'Add a reviewable plan for a Remote HTTP endpoint that does not require credentials. Goose still validates URL safety and machine policy before showing the plan.',
  },
  httpsEndpoint: { id: 'mcpCenter.httpsEndpoint', defaultMessage: 'HTTPS endpoint' },
  endpointPlaceholder: {
    id: 'mcpCenter.endpointPlaceholder',
    defaultMessage: 'https://mcp.example.com/v1',
  },
  authentication: { id: 'mcpCenter.authentication', defaultMessage: 'Authentication' },
  noAuthentication: { id: 'mcpCenter.noAuthentication', defaultMessage: 'No authentication' },
  remoteHttpAuthSupport: {
    id: 'mcpCenter.remoteHttpAuthSupport',
    defaultMessage: 'Authentication support',
  },
  remoteHttpNoAuthOnly: {
    id: 'mcpCenter.remoteHttpNoAuthOnly',
    defaultMessage: 'This page currently adds only no-auth Remote HTTP endpoints.',
  },
  remoteHttpCredentialUnavailable: {
    id: 'mcpCenter.remoteHttpCredentialUnavailable',
    defaultMessage:
      'For endpoints that require credentials, open the Custom MCP tab and add a Streamable HTTP connection with environment variables or headers.',
  },
  bearerReference: {
    id: 'mcpCenter.bearerReference',
    defaultMessage: 'Bearer credential reference',
  },
  credentialReference: {
    id: 'mcpCenter.credentialReference',
    defaultMessage: 'Credential reference',
  },
  existingCredentialReference: {
    id: 'mcpCenter.existingCredentialReference',
    defaultMessage: 'Existing credential reference',
  },
  credentialReferenceHelp: {
    id: 'mcpCenter.credentialReferenceHelp',
    defaultMessage:
      'Enter an opaque reference only. MCP Center never accepts or displays the bearer value.',
  },
  createConnectionPlan: {
    id: 'mcpCenter.createConnectionPlan',
    defaultMessage: 'Create connection plan',
  },
  approvedStdioDescription: {
    id: 'mcpCenter.approvedStdioDescription',
    defaultMessage:
      'The provider policy controls how the connection runs, so this page never asks you for command details or local execution settings.',
  },
  loadingApprovedProviders: {
    id: 'mcpCenter.loadingApprovedProviders',
    defaultMessage: 'Loading approved providers',
  },
  readingProviderPolicy: {
    id: 'mcpCenter.readingProviderPolicy',
    defaultMessage: 'Reading provider policy.',
  },
  stdioUnavailable: {
    id: 'mcpCenter.stdioUnavailable',
    defaultMessage: 'Stdio provider is unavailable',
  },
  stdioUnavailableDescription: {
    id: 'mcpCenter.stdioUnavailableDescription',
    defaultMessage:
      'This machine does not expose an approved stdio provider. No free-form command entry is available.',
  },
  noApprovedStdio: { id: 'mcpCenter.noApprovedStdio', defaultMessage: 'No approved stdio sources' },
  noApprovedStdioDescription: {
    id: 'mcpCenter.noApprovedStdioDescription',
    defaultMessage:
      'The provider is available but currently exposes no policy-approved MCP sources.',
  },
  providerSource: { id: 'mcpCenter.providerSource', defaultMessage: 'Provider source' },
  createProviderPlan: {
    id: 'mcpCenter.createProviderPlan',
    defaultMessage: 'Create provider plan',
  },
  loadingManaged: { id: 'mcpCenter.loadingManaged', defaultMessage: 'Loading managed MCPs' },
  readingManaged: { id: 'mcpCenter.readingManaged', defaultMessage: 'Reading managed inventory.' },
  noManaged: { id: 'mcpCenter.noManaged', defaultMessage: 'No managed MCPs' },
  noManagedDescription: {
    id: 'mcpCenter.noManagedDescription',
    defaultMessage: 'Install from Discover or add an approved manual connection.',
  },
  managedInventory: { id: 'mcpCenter.managedInventory', defaultMessage: 'Managed MCP inventory' },
  managedDetails: { id: 'mcpCenter.managedDetails', defaultMessage: 'Managed MCP details' },
  selectManaged: { id: 'mcpCenter.selectManaged', defaultMessage: 'Select a managed MCP' },
  selectManagedDescription: {
    id: 'mcpCenter.selectManagedDescription',
    defaultMessage: 'Review runtime, health, version, eligibility, and safe actions.',
  },
  loadingMcpState: { id: 'mcpCenter.loadingMcpState', defaultMessage: 'Loading MCP state' },
  readingMcpState: {
    id: 'mcpCenter.readingMcpState',
    defaultMessage: 'Reading detail and health records.',
  },
  defaultEnabled: {
    id: 'mcpCenter.defaultEnabled',
    defaultMessage: 'Enabled by default for new sessions',
  },
  runHealth: { id: 'mcpCenter.runHealth', defaultMessage: 'Run health check' },
  checking: { id: 'mcpCenter.checking', defaultMessage: 'Checking…' },
  reviewUpdate: { id: 'mcpCenter.reviewUpdate', defaultMessage: 'Review update' },
  reviewRepair: { id: 'mcpCenter.reviewRepair', defaultMessage: 'Review repair' },
  reviewUninstall: { id: 'mcpCenter.reviewUninstall', defaultMessage: 'Review uninstall' },
  credentialStatusLabel: {
    id: 'mcpCenter.credentialStatusLabel',
    defaultMessage: 'Authentication status',
  },
  credentialRefresh: {
    id: 'mcpCenter.credentialRefresh',
    defaultMessage: 'Refresh status',
  },
  credentialUnknownBadge: {
    id: 'mcpCenter.credentialUnknownBadge',
    defaultMessage: 'Authentication unconfirmed',
  },
  credentialUnknownAria: {
    id: 'mcpCenter.credentialUnknownAria',
    defaultMessage: 'Authentication status not provided or not yet confirmed',
  },
  credentialUnknownTitle: {
    id: 'mcpCenter.credentialUnknownTitle',
    defaultMessage: 'Authentication status not provided',
  },
  credentialUnknownDescription: {
    id: 'mcpCenter.credentialUnknownDescription',
    defaultMessage:
      'This record does not currently provide a confirmed authentication status, so readiness cannot be verified here.',
  },
  credentialUnknownNextStep: {
    id: 'mcpCenter.credentialUnknownNextStep',
    defaultMessage:
      'Refresh this view to check whether a reviewed authentication status becomes available.',
  },
  credentialReadyBadge: {
    id: 'mcpCenter.credentialReadyBadge',
    defaultMessage: 'Authentication ready',
  },
  credentialReadyAria: {
    id: 'mcpCenter.credentialReadyAria',
    defaultMessage: 'Authentication is ready',
  },
  credentialReadyTitle: {
    id: 'mcpCenter.credentialReadyTitle',
    defaultMessage: 'Authentication is ready',
  },
  credentialReadyDescription: {
    id: 'mcpCenter.credentialReadyDescription',
    defaultMessage:
      'The reviewed authentication state is available. Final access is still enforced by the MCP Platform at runtime.',
  },
  credentialUnconfiguredBadge: {
    id: 'mcpCenter.credentialUnconfiguredBadge',
    defaultMessage: 'Authentication required',
  },
  credentialUnconfiguredAria: {
    id: 'mcpCenter.credentialUnconfiguredAria',
    defaultMessage: 'Authentication must be completed',
  },
  credentialUnconfiguredTitle: {
    id: 'mcpCenter.credentialUnconfiguredTitle',
    defaultMessage: 'Authentication must be completed',
  },
  credentialUnconfiguredDescription: {
    id: 'mcpCenter.credentialUnconfiguredDescription',
    defaultMessage:
      'This MCP cannot use protected access until a reviewed authentication flow becomes available and is completed.',
  },
  credentialReregistrationBadge: {
    id: 'mcpCenter.credentialReregistrationBadge',
    defaultMessage: 'Re-authentication required',
  },
  credentialReregistrationAria: {
    id: 'mcpCenter.credentialReregistrationAria',
    defaultMessage: 'Authentication must be completed again',
  },
  credentialReregistrationTitle: {
    id: 'mcpCenter.credentialReregistrationTitle',
    defaultMessage: 'Authentication must be completed again',
  },
  credentialReregistrationDescription: {
    id: 'mcpCenter.credentialReregistrationDescription',
    defaultMessage:
      'The previously reviewed authentication state is no longer sufficient. Use safe status checks until re-authentication is exposed.',
  },
  credentialConflictBadge: {
    id: 'mcpCenter.credentialConflictBadge',
    defaultMessage: 'Authentication conflict',
  },
  credentialConflictAria: {
    id: 'mcpCenter.credentialConflictAria',
    defaultMessage: 'Authentication state conflict requires attention',
  },
  credentialConflictTitle: {
    id: 'mcpCenter.credentialConflictTitle',
    defaultMessage: 'Authentication state conflict',
  },
  credentialConflictDescription: {
    id: 'mcpCenter.credentialConflictDescription',
    defaultMessage:
      'The trusted authentication state is inconsistent. Refresh or run safe checks before attempting any sensitive workflow.',
  },
  credentialUnavailableBadge: {
    id: 'mcpCenter.credentialUnavailableBadge',
    defaultMessage: 'Authentication unavailable',
  },
  credentialUnavailableAria: {
    id: 'mcpCenter.credentialUnavailableAria',
    defaultMessage: 'Authentication is temporarily unavailable',
  },
  credentialUnavailableTitle: {
    id: 'mcpCenter.credentialUnavailableTitle',
    defaultMessage: 'Authentication is temporarily unavailable',
  },
  credentialUnavailableDescription: {
    id: 'mcpCenter.credentialUnavailableDescription',
    defaultMessage:
      'The authentication status cannot be confirmed right now. Retry or refresh later instead of removing or rebuilding this MCP.',
  },
  credentialUnavailableNextStep: {
    id: 'mcpCenter.credentialUnavailableNextStep',
    defaultMessage:
      'Use refresh or health checks to retry this status when the platform becomes available again.',
  },
  credentialEnrollmentUnavailable: {
    id: 'mcpCenter.credentialEnrollmentUnavailable',
    defaultMessage:
      'A public authentication entry point is not exposed in this build. Only safe refresh and review actions are available here.',
  },
  credentialEnrollmentButton: {
    id: 'mcpCenter.credentialEnrollmentButton',
    defaultMessage: 'Authentication entry not yet available',
  },
  currentTask: { id: 'mcpCenter.currentTask', defaultMessage: 'Current task' },
  currentPhase: { id: 'mcpCenter.currentPhase', defaultMessage: 'Current phase' },
  outcomeState: { id: 'mcpCenter.outcomeState', defaultMessage: 'Outcome state' },
  finalizationState: { id: 'mcpCenter.finalizationState', defaultMessage: 'Finalization' },
  rollbackState: { id: 'mcpCenter.rollbackState', defaultMessage: 'Rollback' },
  remainingEffects: { id: 'mcpCenter.remainingEffects', defaultMessage: 'Remaining effects' },
  latestUpdate: { id: 'mcpCenter.latestUpdate', defaultMessage: 'Latest update' },
  recentActivity: { id: 'mcpCenter.recentActivity', defaultMessage: 'Recent activity' },
  recommendation: { id: 'mcpCenter.recommendation', defaultMessage: 'Recommended next step' },
  taskActionContext: { id: 'mcpCenter.taskActionContext', defaultMessage: 'Before you act' },
  taskSafetyBoundary: {
    id: 'mcpCenter.taskSafetyBoundary',
    defaultMessage:
      'Renderer actions stay inside ACP-reviewed task controls. This page never exposes shell commands, environment variables, credentials, or raw local paths.',
  },
  taskStatusTitle: {
    id: 'mcpCenter.taskStatusTitle',
    defaultMessage:
      '{status, select, planned {{operation} is planned} awaiting_confirmation {Waiting for confirmation} queued {{operation} is queued} running {{operation} is running} cancelling {Cancellation is in progress} verifying {Verifying the reviewed change} activating {Activating the reviewed change} rolling_back {Rolling back the reviewed change} succeeded {{operation} completed} failed {{operation} failed} cancelled {{operation} was cancelled} interrupted {{operation} was interrupted} recovery_required {More recovery is required} other {Task update}}',
  },
  taskStatusDescription: {
    id: 'mcpCenter.taskStatusDescription',
    defaultMessage:
      '{status, select, planned {The MCP Platform has prepared the reviewed task but has not started it yet.} awaiting_confirmation {The reviewed task is waiting for a user confirmation before it can begin.} queued {The reviewed task is waiting for the MCP Platform to start it.} running {The MCP Platform is applying the reviewed task.} cancelling {The MCP Platform is stopping the reviewed task at the next safe point.} verifying {The MCP Platform is checking the reviewed result before it is committed.} activating {The MCP Platform is activating the reviewed result.} rolling_back {The MCP Platform is restoring the previous safe state.} succeeded {The reviewed task finished without requiring more action.} failed {The reviewed task stopped with an error. Review the safe recovery details below before retrying.} cancelled {The reviewed task was cancelled. Review the final state before starting anything new.} interrupted {The reviewed task stopped before completion and needs attention.} recovery_required {The MCP Platform needs an additional recovery step before this task can finish safely.} other {Waiting for a safe task update from the MCP Platform.}}',
  },
  taskNextActionDescription: {
    id: 'mcpCenter.taskNextActionDescription',
    defaultMessage:
      '{action, select, wait {Wait for the next safe task update from the MCP Platform.} cancel_when_safe {If you cancel, the MCP Platform will stop at the next safe point.} retry {You can queue the same reviewed task again.} resume {This task can continue only after a separate resume action that is not exposed safely in this build.} resolve_recovery {This task needs a separate recovery resolution action that is not exposed safely in this build.} recreate_plan {Create and review a new plan before continuing.} none {No additional action is recommended right now.} other {Review the current MCP state before continuing.}}',
  },
  taskRecoveryTitle: {
    id: 'mcpCenter.taskRecoveryTitle',
    defaultMessage:
      '{status, select, failed {The task could not finish} interrupted {The task stopped early} recovery_required {Additional recovery is required} cancelled {The task was cancelled} other {The task needs attention}}',
  },
  taskErrorMessageByCode: {
    id: 'mcpCenter.taskErrorMessageByCode',
    defaultMessage:
      '{code, select, adapter_failed {The managed task could not complete through its adapter.} verification_failed {The reviewed result did not pass verification.} activation_failed {The reviewed result could not be activated safely.} rollback_failed {Goose could not fully restore the previous safe state.} cancelled {The task was cancelled before it finished.} interrupted {The task stopped before it could finish.} unknown {The task could not be completed safely.} other {The task could not be completed safely.}}',
  },
  taskTimelineWaiting: {
    id: 'mcpCenter.taskTimelineWaiting',
    defaultMessage: 'Waiting for safe task updates from the MCP Platform.',
  },
  taskTimelineUnavailableActive: {
    id: 'mcpCenter.taskTimelineUnavailableActive',
    defaultMessage:
      'Detailed task events are not available yet. The task may still continue, so this page is waiting for the next safe update.',
  },
  taskTimelineUnavailableDone: {
    id: 'mcpCenter.taskTimelineUnavailableDone',
    defaultMessage:
      'This task finished without publishing additional safe event details to the desktop client.',
  },
  taskNoRemainingEffects: {
    id: 'mcpCenter.taskNoRemainingEffects',
    defaultMessage: 'No remaining effects are reported.',
  },
  taskCancelImpact: {
    id: 'mcpCenter.taskCancelImpact',
    defaultMessage:
      'Cancel asks the MCP Platform to stop this reviewed task at the next safe point. It does not grant direct shell or configuration control to the desktop UI.',
  },
  taskRetryImpact: {
    id: 'mcpCenter.taskRetryImpact',
    defaultMessage:
      'Retry queues the same reviewed task again with the existing ACP task context. It does not bypass plan review or policy checks.',
  },
  taskUnsupportedRecoveryAction: {
    id: 'mcpCenter.taskUnsupportedRecoveryAction',
    defaultMessage:
      'This task recommends “{action}”, but no safe ACP action is exposed for it in this build.',
  },
  taskTimelineCreated: {
    id: 'mcpCenter.taskTimelineCreated',
    defaultMessage: 'Task created',
  },
  taskTimelineStatusChanged: {
    id: 'mcpCenter.taskTimelineStatusChanged',
    defaultMessage: 'Status changed: {from} → {to}',
  },
  taskTimelineConfirmation: {
    id: 'mcpCenter.taskTimelineConfirmation',
    defaultMessage: 'Plan confirmation recorded',
  },
  taskTimelineCancellation: {
    id: 'mcpCenter.taskTimelineCancellation',
    defaultMessage: 'Cancellation requested',
  },
  taskTimelineCheckpoint: {
    id: 'mcpCenter.taskTimelineCheckpoint',
    defaultMessage:
      '{status, select, started {Checkpoint {ordinal} started} committed {Checkpoint {ordinal} committed} not_started {Checkpoint {ordinal} reset} other {Checkpoint {ordinal} updated}}',
  },
  taskTimelineRecoveryDecision: {
    id: 'mcpCenter.taskTimelineRecoveryDecision',
    defaultMessage:
      '{decision, select, resume_from_step {Recovery can resume from checkpoint {ordinal}} rollback_from_step {Recovery can roll back from checkpoint {ordinal}} requires_manual_recovery {Manual recovery is required} other {Recovery guidance was updated}}',
  },
  cancelTask: { id: 'mcpCenter.cancelTask', defaultMessage: 'Cancel task' },
  cancelling: { id: 'mcpCenter.cancelling', defaultMessage: 'Cancelling…' },
  retryTask: { id: 'mcpCenter.retryTask', defaultMessage: 'Retry task' },
  retrying: { id: 'mcpCenter.retrying', defaultMessage: 'Retrying…' },
  recoveryRequired: { id: 'mcpCenter.recoveryRequired', defaultMessage: 'recovery required' },
  noActiveVersion: { id: 'mcpCenter.noActiveVersion', defaultMessage: 'No active version' },
  noValue: { id: 'mcpCenter.noValue', defaultMessage: 'None' },
  registration: { id: 'mcpCenter.registration', defaultMessage: 'Registration' },
  installation: { id: 'mcpCenter.installation', defaultMessage: 'Installation' },
  runtime: { id: 'mcpCenter.runtime', defaultMessage: 'Runtime' },
  health: { id: 'mcpCenter.health', defaultMessage: 'Health' },
  activeVersion: { id: 'mcpCenter.activeVersion', defaultMessage: 'Active version' },
  availableVersion: { id: 'mcpCenter.availableVersion', defaultMessage: 'Available version' },
  adapter: { id: 'mcpCenter.adapter', defaultMessage: 'Adapter' },
  nextAction: { id: 'mcpCenter.nextAction', defaultMessage: 'Next action' },
  lastHealthCheck: {
    id: 'mcpCenter.lastHealthCheck',
    defaultMessage: 'Last health check: {result} in {latency} ms · {detail}',
  },
  taskErrorNext: {
    id: 'mcpCenter.taskErrorNext',
    defaultMessage: 'Next: {action} · Reference: {reference}',
  },
  activeManifest: { id: 'mcpCenter.activeManifest', defaultMessage: 'Active manifest: {value}' },
  reviewPlan: { id: 'mcpCenter.reviewPlan', defaultMessage: 'Review MCP plan' },
  reviewPlanDescription: {
    id: 'mcpCenter.reviewPlanDescription',
    defaultMessage:
      'Confirm only after reviewing permissions, policy, scope, effects, and rollback.',
  },
  planExpiresAt: { id: 'mcpCenter.planExpiresAt', defaultMessage: 'Plan expires' },
  planBoundaryNote: {
    id: 'mcpCenter.planBoundaryNote',
    defaultMessage:
      'This dialog confirms the reviewed ACP plan only. It does not expose executor steps, shell commands, credentials, or local configuration paths to the renderer.',
  },
  operation: { id: 'mcpCenter.operation', defaultMessage: 'Operation' },
  policy: { id: 'mcpCenter.policy', defaultMessage: 'Policy' },
  source: { id: 'mcpCenter.source', defaultMessage: 'Source' },
  scope: { id: 'mcpCenter.scope', defaultMessage: 'Scope' },
  currentUser: { id: 'mcpCenter.currentUser', defaultMessage: 'Current user' },
  catalogItem: { id: 'mcpCenter.catalogItem', defaultMessage: 'Catalog item' },
  manifestDigest: { id: 'mcpCenter.manifestDigest', defaultMessage: 'Manifest digest' },
  defaultState: { id: 'mcpCenter.defaultState', defaultMessage: 'Default state' },
  enabled: { id: 'mcpCenter.enabled', defaultMessage: 'Enabled' },
  disabled: { id: 'mcpCenter.disabled', defaultMessage: 'Disabled' },
  reversible: { id: 'mcpCenter.reversible', defaultMessage: 'Reversible' },
  writesFiles: { id: 'mcpCenter.writesFiles', defaultMessage: 'Writes files' },
  removesFiles: { id: 'mcpCenter.removesFiles', defaultMessage: 'Removes files' },
  ownedItems: { id: 'mcpCenter.ownedItems', defaultMessage: 'Owned items' },
  startsDuringConfirmation: {
    id: 'mcpCenter.startsDuringConfirmation',
    defaultMessage: 'Starts during confirmation',
  },
  yes: { id: 'mcpCenter.yes', defaultMessage: 'Yes' },
  no: { id: 'mcpCenter.no', defaultMessage: 'No' },
  immutableEvidence: { id: 'mcpCenter.immutableEvidence', defaultMessage: 'Immutable evidence' },
  evidenceType: { id: 'mcpCenter.evidenceType', defaultMessage: 'Evidence type' },
  sha256: { id: 'mcpCenter.sha256', defaultMessage: 'SHA-256' },
  sizeBytes: { id: 'mcpCenter.sizeBytes', defaultMessage: 'Size (bytes)' },
  image: { id: 'mcpCenter.image', defaultMessage: 'Image' },
  imageDigest: { id: 'mcpCenter.imageDigest', defaultMessage: 'Image digest' },
  repositoryOrigin: { id: 'mcpCenter.repositoryOrigin', defaultMessage: 'Repository origin' },
  commit: { id: 'mcpCenter.commit', defaultMessage: 'Commit' },
  tree: { id: 'mcpCenter.tree', defaultMessage: 'Tree evidence' },
  materializedDigest: {
    id: 'mcpCenter.materializedDigest',
    defaultMessage: 'Materialized digest',
  },
  unavailableReason: { id: 'mcpCenter.unavailableReason', defaultMessage: 'Unavailable reason' },
  networkOrigins: { id: 'mcpCenter.networkOrigins', defaultMessage: 'Network origins' },
  hostRegistrations: { id: 'mcpCenter.hostRegistrations', defaultMessage: 'Host registrations' },
  noneDeclared: { id: 'mcpCenter.noneDeclared', defaultMessage: 'None declared' },
  confirmationReasonCode: { id: 'mcpCenter.confirmationReasonCode', defaultMessage: 'Reason code' },
  confirmationPermissionId: {
    id: 'mcpCenter.confirmationPermissionId',
    defaultMessage: 'Permission ID',
  },
  planImpact: { id: 'mcpCenter.planImpact', defaultMessage: 'Plan impact' },
  requiredConfirmations: {
    id: 'mcpCenter.requiredConfirmations',
    defaultMessage: 'Required confirmations',
  },
  noAdditionalConfirmations: {
    id: 'mcpCenter.noAdditionalConfirmations',
    defaultMessage: 'No additional confirmations.',
  },
  permissions: { id: 'mcpCenter.permissions', defaultMessage: 'Permissions' },
  policyRollback: { id: 'mcpCenter.policyRollback', defaultMessage: 'Policy and rollback' },
  warnings: { id: 'mcpCenter.warnings', defaultMessage: 'Warnings' },
  noWarnings: { id: 'mcpCenter.noWarnings', defaultMessage: 'No plan warnings.' },
  rejectPlan: { id: 'mcpCenter.rejectPlan', defaultMessage: 'Reject plan' },
  rejecting: { id: 'mcpCenter.rejecting', defaultMessage: 'Rejecting…' },
  confirmStart: { id: 'mcpCenter.confirmStart', defaultMessage: 'Confirm and start' },
  confirmUninstall: { id: 'mcpCenter.confirmUninstall', defaultMessage: 'Confirm uninstall' },
  uninstallWarning: {
    id: 'mcpCenter.uninstallWarning',
    defaultMessage:
      'Uninstall removes the MCP registration and owned files listed by this plan. Review reversibility and retained user data before continuing.',
  },
  confirming: { id: 'mcpCenter.confirming', defaultMessage: 'Confirming…' },
  targetPlatform: { id: 'mcpCenter.targetPlatform', defaultMessage: 'Target platform' },
  architecture: { id: 'mcpCenter.architecture', defaultMessage: 'Architecture' },
  developmentMode: { id: 'mcpCenter.developmentMode', defaultMessage: 'Development mode' },
  dockerAllowed: { id: 'mcpCenter.dockerAllowed', defaultMessage: 'Docker allowed' },
  recovery: { id: 'mcpCenter.recovery', defaultMessage: 'Recovery' },
  manifests: { id: 'mcpCenter.manifests', defaultMessage: 'Manifests' },
  refresh: { id: 'mcpCenter.refresh', defaultMessage: 'Catalog state' },
  restricted: { id: 'mcpCenter.restricted', defaultMessage: 'Restricted' },
  denied: { id: 'mcpCenter.denied', defaultMessage: 'Denied' },
  offline: { id: 'mcpCenter.offline', defaultMessage: 'Offline copy' },
  requestFailedMessage: {
    id: 'mcpCenter.requestFailedMessage',
    defaultMessage: 'The MCP Platform request could not be completed.',
  },
  recoveryGenericTitle: {
    id: 'mcpCenter.recoveryGenericTitle',
    defaultMessage: 'MCP operation could not be completed',
  },
  recoveryGenericRetry: {
    id: 'mcpCenter.recoveryGenericRetry',
    defaultMessage: 'Retry the operation.',
  },
  recoveryGenericReview: {
    id: 'mcpCenter.recoveryGenericReview',
    defaultMessage: 'Review the MCP state before continuing.',
  },
  recoveryConnectionTitle: {
    id: 'mcpCenter.recoveryConnectionTitle',
    defaultMessage: 'MCP Center is unavailable',
  },
  recoveryConnectionNext: {
    id: 'mcpCenter.recoveryConnectionNext',
    defaultMessage: 'Check the Goose connection and retry.',
  },
  recoveryCredentialTitle: {
    id: 'mcpCenter.recoveryCredentialTitle',
    defaultMessage: 'Authenticated Remote HTTP is not available here',
  },
  recoveryCredentialNext: {
    id: 'mcpCenter.recoveryCredentialNext',
    defaultMessage:
      'Use a no-auth endpoint from this page, or wait until stored credential support is available here.',
  },
  recoveryRemotePolicyTitle: {
    id: 'mcpCenter.recoveryRemotePolicyTitle',
    defaultMessage: 'Remote HTTP is unavailable by policy',
  },
  recoveryRemotePolicyNext: {
    id: 'mcpCenter.recoveryRemotePolicyNext',
    defaultMessage: 'Review the machine policy or contact its administrator before retrying.',
  },
  recoveryStdioTitle: {
    id: 'mcpCenter.recoveryStdioTitle',
    defaultMessage: 'No approved stdio provider is available',
  },
  recoveryStdioNext: {
    id: 'mcpCenter.recoveryStdioNext',
    defaultMessage: 'Restore the approved provider source or contact the policy administrator.',
  },
  recoveryPolicyTitle: {
    id: 'mcpCenter.recoveryPolicyTitle',
    defaultMessage: 'Blocked by machine policy',
  },
  recoveryPolicyNext: {
    id: 'mcpCenter.recoveryPolicyNext',
    defaultMessage: 'Review the policy reasons and contact the policy administrator if needed.',
  },
  recoveryPlanExpiredTitle: {
    id: 'mcpCenter.recoveryPlanExpiredTitle',
    defaultMessage: 'Plan expired',
  },
  recoveryPlanExpiredNext: {
    id: 'mcpCenter.recoveryPlanExpiredNext',
    defaultMessage: 'Create and review a new plan before confirming.',
  },
  recoveryPlanStaleTitle: {
    id: 'mcpCenter.recoveryPlanStaleTitle',
    defaultMessage: 'Plan is no longer current',
  },
  recoveryPlanStaleNext: {
    id: 'mcpCenter.recoveryPlanStaleNext',
    defaultMessage: 'Refresh the MCP state and create a new plan.',
  },
  recoveryRevisionTitle: {
    id: 'mcpCenter.recoveryRevisionTitle',
    defaultMessage: 'MCP state changed',
  },
  recoveryRevisionNext: {
    id: 'mcpCenter.recoveryRevisionNext',
    defaultMessage: 'Refresh the MCP details before trying this action again.',
  },
  recoveryRepositoryTitle: {
    id: 'mcpCenter.recoveryRepositoryTitle',
    defaultMessage: 'MCP data is temporarily unavailable',
  },
  recoveryRepositoryNext: {
    id: 'mcpCenter.recoveryRepositoryNext',
    defaultMessage: 'Retry when the local MCP repository is available.',
  },
  monitorFailedTitle: {
    id: 'mcpCenter.monitorFailedTitle',
    defaultMessage: 'Task status could not be refreshed',
  },
  monitorFailedNext: {
    id: 'mcpCenter.monitorFailedNext',
    defaultMessage: 'Retry task monitoring. The task itself continues independently.',
  },
  loadingPolicy: { id: 'mcpCenter.loadingPolicy', defaultMessage: 'Loading source policy' },
  readingPolicy: {
    id: 'mcpCenter.readingPolicy',
    defaultMessage: 'Reading local source state and machine safety policy.',
  },
  importTrustedSource: {
    id: 'mcpCenter.importTrustedSource',
    defaultMessage: 'Import a trusted local source',
  },
  importTrustedSourceDescription: {
    id: 'mcpCenter.importTrustedSourceDescription',
    defaultMessage:
      'Import a governed manifest file or a signed source catalog directory that already exists on this device. MCP Center only stores or refreshes the source catalog; it does not install or enable any MCP here.',
  },
  localOnlyPhase: { id: 'mcpCenter.localOnlyPhase', defaultMessage: 'Local import only' },
  provisionTrustedSource: {
    id: 'mcpCenter.provisionTrustedSource',
    defaultMessage: 'Add a refreshable trusted source',
  },
  provisionTrustedSourceDescription: {
    id: 'mcpCenter.provisionTrustedSourceDescription',
    defaultMessage:
      'Choose an existing local source directory, inspect its signed refresh identity, then explicitly save its trusted refresh registration.',
  },
  reviewRequired: { id: 'mcpCenter.reviewRequired', defaultMessage: 'Review required' },
  provisionDirectoryLabel: {
    id: 'mcpCenter.provisionDirectoryLabel',
    defaultMessage: 'Signed source directory',
  },
  provisionDirectoryHint: {
    id: 'mcpCenter.provisionDirectoryHint',
    defaultMessage:
      'Choose a local directory containing exactly one signed source catalog document, one signed provisioning descriptor, and its referenced manifests.',
  },
  chooseProvisionDirectory: {
    id: 'mcpCenter.chooseProvisionDirectory',
    defaultMessage: 'Choose source directory',
  },
  previewTrustedSource: {
    id: 'mcpCenter.previewTrustedSource',
    defaultMessage: 'Preview source',
  },
  previewingTrustedSource: {
    id: 'mcpCenter.previewingTrustedSource',
    defaultMessage: 'Preparing preview…',
  },
  provisionPreviewOnlyHint: {
    id: 'mcpCenter.provisionPreviewOnlyHint',
    defaultMessage:
      'Previewing only validates the local source and shows what would be trusted. It does not write a registration, install an MCP, or enable anything.',
  },
  reviewTrustedSource: {
    id: 'mcpCenter.reviewTrustedSource',
    defaultMessage: 'Review trusted source',
  },
  reviewTrustedSourceDescription: {
    id: 'mcpCenter.reviewTrustedSourceDescription',
    defaultMessage:
      'Confirm only if this source identity, endpoint host, digest, and refresh transport are expected. The confirmation is short-lived and tied to this connection.',
  },
  confirmationRequired: {
    id: 'mcpCenter.confirmationRequired',
    defaultMessage: 'Explicit confirmation required',
  },
  confirmTrustedSource: {
    id: 'mcpCenter.confirmTrustedSource',
    defaultMessage: 'Confirm trusted source',
  },
  confirmingTrustedSource: {
    id: 'mcpCenter.confirmingTrustedSource',
    defaultMessage: 'Confirming trusted source…',
  },
  cancelProvisioning: {
    id: 'mcpCenter.cancelProvisioning',
    defaultMessage: 'Discard preview',
  },
  provisionReadyTitle: {
    id: 'mcpCenter.provisionReadyTitle',
    defaultMessage: 'Choose a trusted source directory to review',
  },
  provisionReadyDescription: {
    id: 'mcpCenter.provisionReadyDescription',
    defaultMessage:
      'The source will be saved only after you have reviewed the signed identity and explicitly confirmed it.',
  },
  provisioningSucceededTitle: {
    id: 'mcpCenter.provisioningSucceededTitle',
    defaultMessage: 'Trusted source saved',
  },
  provisioningSucceededDescription: {
    id: 'mcpCenter.provisioningSucceededDescription',
    defaultMessage:
      'The trusted refresh registration and local Discover catalog were saved. No plan, installation, or enable action was triggered.',
  },
  provisionAnotherSource: {
    id: 'mcpCenter.provisionAnotherSource',
    defaultMessage: 'Review another source',
  },
  repreviewTrustedSource: {
    id: 'mcpCenter.repreviewTrustedSource',
    defaultMessage: 'Preview source again',
  },
  sourceRootDigest: {
    id: 'mcpCenter.sourceRootDigest',
    defaultMessage: 'Source root digest',
  },
  endpointHost: { id: 'mcpCenter.endpointHost', defaultMessage: 'Refresh endpoint host' },
  refreshTransport: {
    id: 'mcpCenter.refreshTransport',
    defaultMessage: 'Refresh transport',
  },
  trustBasis: { id: 'mcpCenter.trustBasis', defaultMessage: 'Trust basis' },
  provisionWarnings: { id: 'mcpCenter.provisionWarnings', defaultMessage: 'Review warnings' },
  expiresAt: { id: 'mcpCenter.expiresAt', defaultMessage: 'Preview expires at' },
  confirmedAt: { id: 'mcpCenter.confirmedAt', defaultMessage: 'Confirmed at' },
  importMethod: {
    id: 'mcpCenter.importMethod',
    defaultMessage: 'Trusted local source import method',
  },
  localManifestImportTitle: {
    id: 'mcpCenter.localManifestImportTitle',
    defaultMessage: 'Local manifest file',
  },
  localManifestImportDescription: {
    id: 'mcpCenter.localManifestImportDescription',
    defaultMessage:
      'Choose one existing manifest file. Goose validates and stores the local bytes through ACP before Discover can use it.',
  },
  localDirectoryImportTitle: {
    id: 'mcpCenter.localDirectoryImportTitle',
    defaultMessage: 'Signed source catalog directory',
  },
  localDirectoryImportDescription: {
    id: 'mcpCenter.localDirectoryImportDescription',
    defaultMessage:
      'Choose one existing directory that contains a signed source catalog document and its referenced local manifests.',
  },
  chooseManifestFile: {
    id: 'mcpCenter.chooseManifestFile',
    defaultMessage: 'Choose manifest file',
  },
  chooseSourceDirectory: {
    id: 'mcpCenter.chooseSourceDirectory',
    defaultMessage: 'Choose source directory',
  },
  importSource: { id: 'mcpCenter.importSource', defaultMessage: 'Import source' },
  importingSource: { id: 'mcpCenter.importingSource', defaultMessage: 'Importing source…' },
  selectedLocalPath: { id: 'mcpCenter.selectedLocalPath', defaultMessage: 'Selected local path' },
  noPathSelected: { id: 'mcpCenter.noPathSelected', defaultMessage: 'Nothing selected yet.' },
  reimportSourceHint: {
    id: 'mcpCenter.reimportSourceHint',
    defaultMessage:
      'Importing the same source again refreshes that governed source in place. It does not create an install plan or enable any MCP.',
  },
  httpsImportUnavailableTitle: {
    id: 'mcpCenter.httpsImportUnavailableTitle',
    defaultMessage: 'HTTPS source import is not available in this phase',
  },
  httpsImportUnavailableDescription: {
    id: 'mcpCenter.httpsImportUnavailableDescription',
    defaultMessage:
      'This page does not fetch manifests or catalogs from URLs yet. Only local manifest files and local signed source directories can be imported here.',
  },
  importReadyTitle: {
    id: 'mcpCenter.importReadyTitle',
    defaultMessage: 'Choose a trusted local source to import',
  },
  importReadyDescription: {
    id: 'mcpCenter.importReadyDescription',
    defaultMessage:
      'Pick a manifest file or source directory, then import it through ACP to refresh the local governed catalog for Discover.',
  },
  importSucceededTitle: {
    id: 'mcpCenter.importSucceededTitle',
    defaultMessage: 'Trusted source imported',
  },
  importSucceededDescription: {
    id: 'mcpCenter.importSucceededDescription',
    defaultMessage:
      'The governed source catalog was stored locally. You can now review its entries from Discover before creating any plan.',
  },
  openDiscover: { id: 'mcpCenter.openDiscover', defaultMessage: 'Open Discover' },
  importAnotherSource: {
    id: 'mcpCenter.importAnotherSource',
    defaultMessage: 'Import another source',
  },
  sourceName: { id: 'mcpCenter.sourceName', defaultMessage: 'Source name' },
  sourceId: { id: 'mcpCenter.sourceId', defaultMessage: 'Source ID' },
  absent: { id: 'mcpCenter.absent', defaultMessage: 'Absent' },
  importKind: { id: 'mcpCenter.importKind', defaultMessage: 'Import kind' },
  documentKind: { id: 'mcpCenter.documentKind', defaultMessage: 'Document kind' },
  documentId: { id: 'mcpCenter.documentId', defaultMessage: 'Document ID' },
  documentDigest: { id: 'mcpCenter.documentDigest', defaultMessage: 'Document digest' },
  catalogEntries: { id: 'mcpCenter.catalogEntries', defaultMessage: 'Catalog entries' },
  cacheFreshness: { id: 'mcpCenter.cacheFreshness', defaultMessage: 'Cache freshness' },
  policyStatus: { id: 'mcpCenter.policyStatus', defaultMessage: 'Source recovery state' },
  pendingRefresh: { id: 'mcpCenter.pendingRefresh', defaultMessage: 'Refreshing' },
  machinePolicy: { id: 'mcpCenter.machinePolicy', defaultMessage: 'Machine policy' },
  readOnlyPolicy: {
    id: 'mcpCenter.readOnlyPolicy',
    defaultMessage: 'Read-only policy supplied by the MCP Platform.',
  },
  machinePolicyDescription: {
    id: 'mcpCenter.machinePolicyDescription',
    defaultMessage:
      'These read-only safety rules decide which locally stored MCP sources can appear here and which ones stay hidden to avoid unreviewed downloads, credentials, or execution paths on this device.',
  },
  readOnly: { id: 'mcpCenter.readOnly', defaultMessage: 'Read only' },
  sourcesCache: { id: 'mcpCenter.sourcesCache', defaultMessage: 'Local catalog sources' },
  sourcesCacheDescription: {
    id: 'mcpCenter.sourcesCacheDescription',
    defaultMessage:
      'Catalog source records are stored locally and verified for offline use. The list below shows what is already available on this device after any local governed imports or refreshes.',
  },
  refreshCatalogOnlyTitle: {
    id: 'mcpCenter.refreshCatalogOnlyTitle',
    defaultMessage: 'Refreshing a source only updates the local catalog',
  },
  refreshCatalogOnlyDescription: {
    id: 'mcpCenter.refreshCatalogOnlyDescription',
    defaultMessage:
      'Refresh reads a previously registered trusted source, verifies it, and updates Discover. It never creates a plan, installs an MCP, or enables anything by default.',
  },
  refreshSource: { id: 'mcpCenter.refreshSource', defaultMessage: 'Refresh source' },
  refreshingSource: { id: 'mcpCenter.refreshingSource', defaultMessage: 'Refreshing source…' },
  refreshSucceededTitle: {
    id: 'mcpCenter.refreshSucceededTitle',
    defaultMessage: 'Source catalog refreshed',
  },
  refreshSucceededDescription: {
    id: 'mcpCenter.refreshSucceededDescription',
    defaultMessage:
      'The registered source was re-verified and the local Discover catalog was updated. No plan, install, or enable action was triggered.',
  },
  lastRefreshed: { id: 'mcpCenter.lastRefreshed', defaultMessage: 'Last refreshed' },
  noRefreshableSources: {
    id: 'mcpCenter.noRefreshableSources',
    defaultMessage: 'No registered refresh sources',
  },
  noRefreshableSourcesDescription: {
    id: 'mcpCenter.noRefreshableSourcesDescription',
    defaultMessage:
      'The sources shown here are local-only imports. A trusted registration record is required before this page can refresh a source.',
  },
  noSources: { id: 'mcpCenter.noSources', defaultMessage: 'No local catalog sources' },
  noSourcesDescription: {
    id: 'mcpCenter.noSourcesDescription',
    defaultMessage:
      'The MCP Platform currently reports no verified offline catalog sources stored on this device.',
  },
  cancel: { id: 'mcpCenter.cancel', defaultMessage: 'Cancel' },
  startRuntime: { id: 'mcpCenter.startRuntime', defaultMessage: 'Start runtime' },
  startingRuntime: { id: 'mcpCenter.startingRuntime', defaultMessage: 'Starting…' },
  stopRuntime: { id: 'mcpCenter.stopRuntime', defaultMessage: 'Stop runtime' },
  stoppingRuntime: { id: 'mcpCenter.stoppingRuntime', defaultMessage: 'Stopping…' },
  stopRuntimeTitle: {
    id: 'mcpCenter.stopRuntimeTitle',
    defaultMessage: 'Stop this MCP runtime?',
  },
  stopRuntimeDescription: {
    id: 'mcpCenter.stopRuntimeDescription',
    defaultMessage: 'Goose will close only its currently owned MCP connection or child process.',
  },
  stopRuntimeImpact: {
    id: 'mcpCenter.stopRuntimeImpact',
    defaultMessage:
      'This does not uninstall the MCP or change “Enabled by default for new sessions”.',
  },
  stopRuntimeConfirm: { id: 'mcpCenter.stopRuntimeConfirm', defaultMessage: 'Stop runtime' },
  profileTab: { id: 'mcpCenter.profileTab', defaultMessage: 'Profiles' },
  profileTabDescription: {
    id: 'mcpCenter.profileTabDescription',
    defaultMessage:
      'Save reusable MCP combinations, review them later, and edit or archive them without changing the current session.',
  },
  createProfile: { id: 'mcpCenter.createProfile', defaultMessage: 'New profile' },
  showArchived: { id: 'mcpCenter.showArchived', defaultMessage: 'Show archived' },
  profileListAria: { id: 'mcpCenter.profileListAria', defaultMessage: 'Saved MCP profiles' },
  profileDetailAria: {
    id: 'mcpCenter.profileDetailAria',
    defaultMessage: 'Profile details and editor',
  },
  profileUnavailableTitle: {
    id: 'mcpCenter.profileUnavailableTitle',
    defaultMessage: 'Profiles are not available in this phase',
  },
  profileUnavailableDescription: {
    id: 'mcpCenter.profileUnavailableDescription',
    defaultMessage:
      'This Goose build does not currently expose the profile workflow from the MCP Platform. When that phase becomes available, this tab will show the real CRUD flow.',
  },
  profileCapabilityHint: {
    id: 'mcpCenter.profileCapabilityHint',
    defaultMessage: 'Managed inventory hint: {value}. Capability truth comes from the profile RPC.',
  },
  profileListLoading: {
    id: 'mcpCenter.profileListLoading',
    defaultMessage: 'Loading profiles',
  },
  profileListReading: {
    id: 'mcpCenter.profileListReading',
    defaultMessage: 'Reading saved profile records from the MCP Platform.',
  },
  profileListEmpty: { id: 'mcpCenter.profileListEmpty', defaultMessage: 'No saved profiles' },
  profileListEmptyDescription: {
    id: 'mcpCenter.profileListEmptyDescription',
    defaultMessage:
      'Create a reusable MCP profile now, or use the suggestion tools below to draft one first.',
  },
  profileHiddenErrorsTitle: {
    id: 'mcpCenter.profileHiddenErrorsTitle',
    defaultMessage: 'Profile action issues',
  },
  profileHiddenErrorsDescription: {
    id: 'mcpCenter.profileHiddenErrorsDescription',
    defaultMessage:
      'A previous profile action failed for a profile that is not currently visible. Adjust the current filter or reopen that profile to review it directly.',
  },
  profileHiddenErrorLabel: {
    id: 'mcpCenter.profileHiddenErrorLabel',
    defaultMessage: 'Profile: {value}',
  },
  archivedLabel: { id: 'mcpCenter.archivedLabel', defaultMessage: 'Archived' },
  profileDescriptionEmpty: {
    id: 'mcpCenter.profileDescriptionEmpty',
    defaultMessage: 'No description',
  },
  profileRevisionBadge: {
    id: 'mcpCenter.profileRevisionBadge',
    defaultMessage: 'Rev {revision}',
  },
  profileEntriesCount: {
    id: 'mcpCenter.profileEntriesCount',
    defaultMessage: '{count, plural, one {# MCP} other {# MCPs}}',
  },
  profileCreateTitle: {
    id: 'mcpCenter.profileCreateTitle',
    defaultMessage: 'Create profile',
  },
  profileEditTitle: { id: 'mcpCenter.profileEditTitle', defaultMessage: 'Edit profile' },
  profileEditorBoundary: {
    id: 'mcpCenter.profileEditorBoundary',
    defaultMessage:
      'Saving a profile stores metadata only. It does not enable, install, uninstall, or apply MCPs to the current session.',
  },
  profileDraftOnlyBadge: { id: 'mcpCenter.profileDraftOnlyBadge', defaultMessage: 'Review first' },
  profileNameRequired: {
    id: 'mcpCenter.profileNameRequired',
    defaultMessage: 'Enter a profile name before saving.',
  },
  profileInventoryLoading: {
    id: 'mcpCenter.profileInventoryLoading',
    defaultMessage: 'Loading managed inventory',
  },
  profileInventoryReading: {
    id: 'mcpCenter.profileInventoryReading',
    defaultMessage: 'Reading managed MCPs for profile selection.',
  },
  profileInventoryEmpty: {
    id: 'mcpCenter.profileInventoryEmpty',
    defaultMessage: 'No managed MCPs yet',
  },
  profileInventoryEmptyDescription: {
    id: 'mcpCenter.profileInventoryEmptyDescription',
    defaultMessage:
      'You can still save an empty profile or use suggestions first. Managed MCPs will appear here after they exist.',
  },
  profileNameLabel: { id: 'mcpCenter.profileNameLabel', defaultMessage: 'Profile name' },
  profileNamePlaceholder: {
    id: 'mcpCenter.profileNamePlaceholder',
    defaultMessage: 'Research setup',
  },
  profileDescriptionLabel: {
    id: 'mcpCenter.profileDescriptionLabel',
    defaultMessage: 'Description',
  },
  profileDescriptionPlaceholder: {
    id: 'mcpCenter.profileDescriptionPlaceholder',
    defaultMessage: 'Describe when to use this MCP combination.',
  },
  profileManagedMcpLabel: {
    id: 'mcpCenter.profileManagedMcpLabel',
    defaultMessage: 'Managed MCPs',
  },
  profileSelectedEntriesCount: {
    id: 'mcpCenter.profileSelectedEntriesCount',
    defaultMessage: '{count, plural, one {# selected} other {# selected}}',
  },
  profileSaving: { id: 'mcpCenter.profileSaving', defaultMessage: 'Saving…' },
  profileCreateSubmit: {
    id: 'mcpCenter.profileCreateSubmit',
    defaultMessage: 'Save profile',
  },
  profileSaveSubmit: {
    id: 'mcpCenter.profileSaveSubmit',
    defaultMessage: 'Save changes',
  },
  profileDetailEmpty: {
    id: 'mcpCenter.profileDetailEmpty',
    defaultMessage: 'Select a profile or create one',
  },
  profileDetailEmptyDescription: {
    id: 'mcpCenter.profileDetailEmptyDescription',
    defaultMessage:
      'Profiles are reusable saved MCP combinations. Selecting one lets you review entries and revision history.',
  },
  profileDetailLoading: {
    id: 'mcpCenter.profileDetailLoading',
    defaultMessage: 'Loading profile details',
  },
  profileDetailReading: {
    id: 'mcpCenter.profileDetailReading',
    defaultMessage: 'Reading the selected profile and revision history.',
  },
  editProfile: { id: 'mcpCenter.editProfile', defaultMessage: 'Edit' },
  archiveProfile: { id: 'mcpCenter.archiveProfile', defaultMessage: 'Archive' },
  applyProfile: {
    id: 'mcpCenter.applyProfile',
    defaultMessage: 'Apply to new session',
  },
  applyProfileLoading: {
    id: 'mcpCenter.applyProfileLoading',
    defaultMessage: 'Creating review…',
  },
  profileConnectionTest: {
    id: 'mcpCenter.profileConnectionTest',
    defaultMessage: 'Test connection to model',
  },
  profileConnectionTestTitle: {
    id: 'mcpCenter.profileConnectionTestTitle',
    defaultMessage: 'Test connection to model',
  },
  profileConnectionTestDescription: {
    id: 'mcpCenter.profileConnectionTestDescription',
    defaultMessage:
      'Initializes MCP, discovers its tools, and verifies connectivity to the selected provider and model. For safe isolation, discovered tool definitions are not sent to the model. This is not a tool capability or execution test.',
  },
  profileConnectionTestBoundary: {
    id: 'mcpCenter.profileConnectionTestBoundary',
    defaultMessage:
      'Only saved profile entries and configured provider models are used. Tool definitions are discovered but withheld from the model; endpoints, commands, credentials, prompts, and tool arguments are not accepted or shown.',
  },
  profileConnectionTestProviderLabel: {
    id: 'mcpCenter.profileConnectionTestProviderLabel',
    defaultMessage: 'Configured provider',
  },
  profileConnectionTestModelLabel: {
    id: 'mcpCenter.profileConnectionTestModelLabel',
    defaultMessage: 'Configured model',
  },
  profileConnectionTestLoadingProviders: {
    id: 'mcpCenter.profileConnectionTestLoadingProviders',
    defaultMessage: 'Loading configured providers…',
  },
  profileConnectionTestNoProviders: {
    id: 'mcpCenter.profileConnectionTestNoProviders',
    defaultMessage: 'No configured provider and model are available for this test.',
  },
  profileConnectionTestLoadFailed: {
    id: 'mcpCenter.profileConnectionTestLoadFailed',
    defaultMessage: 'Configured providers could not be loaded.',
  },
  profileConnectionTestRetryProviders: {
    id: 'mcpCenter.profileConnectionTestRetryProviders',
    defaultMessage: 'Retry loading providers',
  },
  profileConnectionTestRun: {
    id: 'mcpCenter.profileConnectionTestRun',
    defaultMessage: 'Run connection test',
  },
  profileConnectionTestRunning: {
    id: 'mcpCenter.profileConnectionTestRunning',
    defaultMessage: 'Testing connection…',
  },
  profileConnectionTestPassed: {
    id: 'mcpCenter.profileConnectionTestPassed',
    defaultMessage: 'Connection test passed',
  },
  profileConnectionTestFailed: {
    id: 'mcpCenter.profileConnectionTestFailed',
    defaultMessage: 'Connection test did not complete',
  },
  profileConnectionTestRetry: {
    id: 'mcpCenter.profileConnectionTestRetry',
    defaultMessage: 'Retry connection test',
  },
  profileConnectionTestSafeFailure: {
    id: 'mcpCenter.profileConnectionTestSafeFailure',
    defaultMessage:
      'Goose could not complete the connection test. No diagnostic details, configuration, or credentials are shown.',
  },
  profileConnectionTestStageEligibility: {
    id: 'mcpCenter.profileConnectionTestStageEligibility',
    defaultMessage: 'Eligibility check',
  },
  profileConnectionTestStageMcpInitialize: {
    id: 'mcpCenter.profileConnectionTestStageMcpInitialize',
    defaultMessage: 'MCP initialization',
  },
  profileConnectionTestStageToolDiscovery: {
    id: 'mcpCenter.profileConnectionTestStageToolDiscovery',
    defaultMessage: 'MCP tools discovered',
  },
  profileConnectionTestStageCleanup: {
    id: 'mcpCenter.profileConnectionTestStageCleanup',
    defaultMessage: 'Connection cleanup',
  },
  profileConnectionTestStageModelRequest: {
    id: 'mcpCenter.profileConnectionTestStageModelRequest',
    defaultMessage: 'Provider/model connectivity',
  },
  profileConnectionTestStageToolVisibility: {
    id: 'mcpCenter.profileConnectionTestStageToolVisibility',
    defaultMessage: 'Tool definitions withheld from model (safe isolation)',
  },
  profileConnectionTestPending: {
    id: 'mcpCenter.profileConnectionTestPending',
    defaultMessage: 'Pending',
  },
  profileConnectionTestSkipped: {
    id: 'mcpCenter.profileConnectionTestSkipped',
    defaultMessage: 'Skipped',
  },
  profileConnectionTestRunningStage: {
    id: 'mcpCenter.profileConnectionTestRunningStage',
    defaultMessage: 'Running',
  },
  profileConnectionTestCode: {
    id: 'mcpCenter.profileConnectionTestCode',
    defaultMessage: 'Code: {code}',
  },
  archiveProfileTitle: {
    id: 'mcpCenter.archiveProfileTitle',
    defaultMessage: 'Archive this profile?',
  },
  archiveProfileMessage: {
    id: 'mcpCenter.archiveProfileMessage',
    defaultMessage:
      'Archiving removes this profile from the default list until you show archived profiles or restore an earlier revision.',
  },
  archiveProfileImpact: {
    id: 'mcpCenter.archiveProfileImpact',
    defaultMessage:
      'This action is reversible. It only archives the saved profile record and does not remove managed MCP inventory.',
  },
  archiveProfileNoApply: {
    id: 'mcpCenter.archiveProfileNoApply',
    defaultMessage: 'This action will not uninstall, delete, enable, or apply MCPs to any session.',
  },
  archiveProfileConfirm: {
    id: 'mcpCenter.archiveProfileConfirm',
    defaultMessage: 'Archive profile',
  },
  profileRevisionLabel: {
    id: 'mcpCenter.profileRevisionLabel',
    defaultMessage: 'Current revision',
  },
  profileRevisionValue: {
    id: 'mcpCenter.profileRevisionValue',
    defaultMessage: 'Revision {revision}',
  },
  profileCreatedAtLabel: {
    id: 'mcpCenter.profileCreatedAtLabel',
    defaultMessage: 'Created',
  },
  profileUpdatedAtLabel: {
    id: 'mcpCenter.profileUpdatedAtLabel',
    defaultMessage: 'Updated',
  },
  profileEntriesLabel: { id: 'mcpCenter.profileEntriesLabel', defaultMessage: 'Entries' },
  profileEntriesEmpty: {
    id: 'mcpCenter.profileEntriesEmpty',
    defaultMessage: 'No MCP entries saved',
  },
  profileEntriesEmptyDescription: {
    id: 'mcpCenter.profileEntriesEmptyDescription',
    defaultMessage: 'This profile currently saves no managed MCPs. Edit it to add entries later.',
  },
  profileApplyReviewTitle: {
    id: 'mcpCenter.profileApplyReviewTitle',
    defaultMessage: 'Review profile application',
  },
  profileApplyReviewDescription: {
    id: 'mcpCenter.profileApplyReviewDescription',
    defaultMessage:
      'Review this safe profile summary before creating a new session. The current session will not change.',
  },
  profileApplySafeBoundary: {
    id: 'mcpCenter.profileApplySafeBoundary',
    defaultMessage:
      'This review shows only public readiness details. Credentials, tokens, digests, and provenance stay hidden.',
  },
  profileApplyOpenNewSession: {
    id: 'mcpCenter.profileApplyOpenNewSession',
    defaultMessage: 'Creates a new session only',
  },
  profileApplyReadyBadge: {
    id: 'mcpCenter.profileApplyReadyBadge',
    defaultMessage: 'Ready',
  },
  profileApplyPartialBadge: {
    id: 'mcpCenter.profileApplyPartialBadge',
    defaultMessage: 'Needs review',
  },
  profileApplyExpiredBadge: {
    id: 'mcpCenter.profileApplyExpiredBadge',
    defaultMessage: 'Expired',
  },
  profileApplyExpiresAt: {
    id: 'mcpCenter.profileApplyExpiresAt',
    defaultMessage: 'Expires',
  },
  profileApplyMergePolicy: {
    id: 'mcpCenter.profileApplyMergePolicy',
    defaultMessage: 'Merge policy',
  },
  profileApplyEntriesLabel: {
    id: 'mcpCenter.profileApplyEntriesLabel',
    defaultMessage: 'Managed MCP entries',
  },
  profileApplyEntriesEmpty: {
    id: 'mcpCenter.profileApplyEntriesEmpty',
    defaultMessage: 'No managed MCP entries in this review',
  },
  profileApplyEntriesEmptyDescription: {
    id: 'mcpCenter.profileApplyEntriesEmptyDescription',
    defaultMessage:
      'This plan does not include any managed MCP entries, so a new session would start without profile-managed MCPs.',
  },
  profileApplyAuthReady: {
    id: 'mcpCenter.profileApplyAuthReady',
    defaultMessage: 'Authentication ready',
  },
  profileApplyAuthBlocked: {
    id: 'mcpCenter.profileApplyAuthBlocked',
    defaultMessage: 'Authentication needs attention',
  },
  profileApplyPolicyReady: {
    id: 'mcpCenter.profileApplyPolicyReady',
    defaultMessage: 'Policy ready',
  },
  profileApplyPolicyBlocked: {
    id: 'mcpCenter.profileApplyPolicyBlocked',
    defaultMessage: 'Policy needs attention',
  },
  profileApplyExpiredTitle: {
    id: 'mcpCenter.profileApplyExpiredTitle',
    defaultMessage: 'This review has expired',
  },
  profileApplyExpiredDescription: {
    id: 'mcpCenter.profileApplyExpiredDescription',
    defaultMessage: 'Close this dialog and create a new review before creating a new session.',
  },
  profileApplyConfirm: {
    id: 'mcpCenter.profileApplyConfirm',
    defaultMessage: 'Confirm and create new session',
  },
  profileApplyConfirming: {
    id: 'mcpCenter.profileApplyConfirming',
    defaultMessage: 'Confirming…',
  },
  profileApplyResolvingSession: {
    id: 'mcpCenter.profileApplyResolvingSession',
    defaultMessage: 'Checking current session folder…',
  },
  profileApplyCreatingSession: {
    id: 'mcpCenter.profileApplyCreatingSession',
    defaultMessage: 'Creating session…',
  },
  profileApplyStatusConfirming: {
    id: 'mcpCenter.profileApplyStatusConfirming',
    defaultMessage: 'Confirming the reviewed profile application.',
  },
  profileApplyStatusResolvingSession: {
    id: 'mcpCenter.profileApplyStatusResolvingSession',
    defaultMessage: 'Verifying the current session folder before confirming this review.',
  },
  profileApplyStatusCreatingSession: {
    id: 'mcpCenter.profileApplyStatusCreatingSession',
    defaultMessage: 'Creating a new session from the confirmed profile application.',
  },
  profileApplySuccessTitle: {
    id: 'mcpCenter.profileApplySuccessTitle',
    defaultMessage: 'New session created',
  },
  profileApplySuccessDescription: {
    id: 'mcpCenter.profileApplySuccessDescription',
    defaultMessage: 'Opening the new session now.',
  },
  profileApplyRestart: {
    id: 'mcpCenter.profileApplyRestart',
    defaultMessage: 'Create a new review',
  },
  profileApplySuccessToast: {
    id: 'mcpCenter.profileApplySuccessToast',
    defaultMessage: 'New session created from profile "{name}".',
  },
  profileApplyInvalidTokenTitle: {
    id: 'mcpCenter.profileApplyInvalidTokenTitle',
    defaultMessage: 'Profile application is no longer available',
  },
  profileApplyInvalidTokenMessage: {
    id: 'mcpCenter.profileApplyInvalidTokenMessage',
    defaultMessage: 'This reviewed profile application could not be used to create a new session.',
  },
  profileApplyPolicyDeniedMessage: {
    id: 'mcpCenter.profileApplyPolicyDeniedMessage',
    defaultMessage:
      'Machine policy blocked this reviewed profile application from creating a new session.',
  },
  profileApplyRuntimeUnavailableTitle: {
    id: 'mcpCenter.profileApplyRuntimeUnavailableTitle',
    defaultMessage: 'A required runtime is unavailable',
  },
  profileApplyRuntimeUnavailableMessage: {
    id: 'mcpCenter.profileApplyRuntimeUnavailableMessage',
    defaultMessage:
      'The new session could not start because a required runtime is currently unavailable.',
  },
  profileApplySessionErrorTitle: {
    id: 'mcpCenter.profileApplySessionErrorTitle',
    defaultMessage: 'New session could not be created',
  },
  profileApplySessionErrorMessage: {
    id: 'mcpCenter.profileApplySessionErrorMessage',
    defaultMessage: 'The request to create a new session did not complete safely.',
  },
  profileApplySessionErrorNextStep: {
    id: 'mcpCenter.profileApplySessionErrorNextStep',
    defaultMessage: 'Create a new review and try again when the Goose connection is available.',
  },
  profileApplyCurrentSessionUnavailableTitle: {
    id: 'mcpCenter.profileApplyCurrentSessionUnavailableTitle',
    defaultMessage: 'Current session folder unavailable',
  },
  profileApplyCurrentSessionUnavailableMessage: {
    id: 'mcpCenter.profileApplyCurrentSessionUnavailableMessage',
    defaultMessage:
      'Goose could not verify the current session folder, so this reviewed profile application was not confirmed.',
  },
  profileApplyCurrentSessionUnavailableNextStep: {
    id: 'mcpCenter.profileApplyCurrentSessionUnavailableNextStep',
    defaultMessage: 'Retry after the current session folder becomes available.',
  },
  profileEntryOrdinal: {
    id: 'mcpCenter.profileEntryOrdinal',
    defaultMessage: 'Entry {ordinal}',
  },
  profileHistoryLabel: {
    id: 'mcpCenter.profileHistoryLabel',
    defaultMessage: 'Revision history',
  },
  profileHistoryEmpty: {
    id: 'mcpCenter.profileHistoryEmpty',
    defaultMessage: 'No revision history available',
  },
  profileHistoryEmptyDescription: {
    id: 'mcpCenter.profileHistoryEmptyDescription',
    defaultMessage: 'The MCP Platform did not report any older revisions for this profile.',
  },
  profileHistorySummary: {
    id: 'mcpCenter.profileHistorySummary',
    defaultMessage: '{operation}',
  },
  restoreRevision: {
    id: 'mcpCenter.restoreRevision',
    defaultMessage: 'Restore revision',
  },
  profileSuggestionsTitle: {
    id: 'mcpCenter.profileSuggestionsTitle',
    defaultMessage: 'Suggestions',
  },
  profileSuggestionsDescription: {
    id: 'mcpCenter.profileSuggestionsDescription',
    defaultMessage:
      'Describe your work to draft a profile or get model recommendations. Suggestions never save or apply automatically.',
  },
  profileSuggestionsInputLabel: {
    id: 'mcpCenter.profileSuggestionsInputLabel',
    defaultMessage: 'Describe your work',
  },
  profileSuggestionsInputPlaceholder: {
    id: 'mcpCenter.profileSuggestionsInputPlaceholder',
    defaultMessage:
      'Example: I need browser automation, web fetches, and local file access for research.',
  },
  profileDraftGenerating: {
    id: 'mcpCenter.profileDraftGenerating',
    defaultMessage: 'Generating draft…',
  },
  profileDraftGenerate: {
    id: 'mcpCenter.profileDraftGenerate',
    defaultMessage: 'Draft profile',
  },
  profileLowConfidence: {
    id: 'mcpCenter.profileLowConfidence',
    defaultMessage: 'Low confidence',
  },
  profileDraftPersisted: {
    id: 'mcpCenter.profileDraftPersisted',
    defaultMessage: 'Persisted draft',
  },
  profileDraftNotPersisted: {
    id: 'mcpCenter.profileDraftNotPersisted',
    defaultMessage: 'Transient draft',
  },
  profileUnresolvedTerms: {
    id: 'mcpCenter.profileUnresolvedTerms',
    defaultMessage: 'Unresolved terms',
  },
  profileDraftCandidates: {
    id: 'mcpCenter.profileDraftCandidates',
    defaultMessage: 'Candidate MCPs',
  },
  profileDraftCandidatesEmpty: {
    id: 'mcpCenter.profileDraftCandidatesEmpty',
    defaultMessage: 'No MCP candidates',
  },
  profileDraftCandidatesEmptyDescription: {
    id: 'mcpCenter.profileDraftCandidatesEmptyDescription',
    defaultMessage:
      'The draft response did not include candidate MCPs. You can still save a profile manually.',
  },
  profileConfidenceValue: {
    id: 'mcpCenter.profileConfidenceValue',
    defaultMessage: 'Confidence {value}%',
  },
  profileReasonCode: {
    id: 'mcpCenter.profileReasonCode',
    defaultMessage: 'Reason code: {value}',
  },
  profileApplyDraft: {
    id: 'mcpCenter.profileApplyDraft',
    defaultMessage: 'Fill form from draft',
  },
  profileModelUnavailableTitle: {
    id: 'mcpCenter.profileModelUnavailableTitle',
    defaultMessage: 'Model recommendations are not available in this phase',
  },
  profileModelUnavailableDescription: {
    id: 'mcpCenter.profileModelUnavailableDescription',
    defaultMessage:
      'The MCP Platform did not expose model recommendations in this build. Drafting profiles can still remain available separately.',
  },
  profileProviderFilterLabel: {
    id: 'mcpCenter.profileProviderFilterLabel',
    defaultMessage: 'Provider IDs (optional)',
  },
  profileProviderFilterPlaceholder: {
    id: 'mcpCenter.profileProviderFilterPlaceholder',
    defaultMessage: 'openai, anthropic',
  },
  profileProviderFilterDescription: {
    id: 'mcpCenter.profileProviderFilterDescription',
    defaultMessage:
      'Limit recommendations to these provider IDs, separated by commas, or leave blank to let the MCP Platform choose.',
  },
  profileModelsGenerating: {
    id: 'mcpCenter.profileModelsGenerating',
    defaultMessage: 'Generating model suggestions…',
  },
  profileModelsGenerate: {
    id: 'mcpCenter.profileModelsGenerate',
    defaultMessage: 'Recommend models',
  },
  profileModelInventoryOnly: {
    id: 'mcpCenter.profileModelInventoryOnly',
    defaultMessage: 'Inventory only',
  },
  profileModelNotInventoryOnly: {
    id: 'mcpCenter.profileModelNotInventoryOnly',
    defaultMessage: 'Not inventory only',
  },
  profileModelMutated: {
    id: 'mcpCenter.profileModelMutated',
    defaultMessage: 'Recommendation mutated',
  },
  profileModelNotMutated: {
    id: 'mcpCenter.profileModelNotMutated',
    defaultMessage: 'Recommendation preserved',
  },
  profileModelsEmpty: {
    id: 'mcpCenter.profileModelsEmpty',
    defaultMessage: 'No model recommendations',
  },
  profileModelsEmptyDescription: {
    id: 'mcpCenter.profileModelsEmptyDescription',
    defaultMessage: 'The MCP Platform returned no provider/model candidates for this description.',
  },
  restoreProfileTitle: {
    id: 'mcpCenter.restoreProfileTitle',
    defaultMessage: 'Restore this profile revision?',
  },
  restoreProfileMessage: {
    id: 'mcpCenter.restoreProfileMessage',
    defaultMessage: 'Revision {revision} will become the current saved version of this profile.',
  },
  restoreProfileImpact: {
    id: 'mcpCenter.restoreProfileImpact',
    defaultMessage:
      'Restoring a revision updates only the saved profile record. It does not apply anything to the current session.',
  },
  restoreProfileConfirm: {
    id: 'mcpCenter.restoreProfileConfirm',
    defaultMessage: 'Restore revision',
  },
  profileDiscardTitle: {
    id: 'mcpCenter.profileDiscardTitle',
    defaultMessage: 'Discard unsaved profile changes?',
  },
  profileDiscardMessage: {
    id: 'mcpCenter.profileDiscardMessage',
    defaultMessage:
      'Your profile edits have not been saved to the MCP Platform yet. Leaving now will discard them.',
  },
  profileDiscardConfirm: {
    id: 'mcpCenter.profileDiscardConfirm',
    defaultMessage: 'Discard changes',
  },
  importHttpsManifest: {
    id: 'mcpCenter.importHttpsManifest',
    defaultMessage: 'Import HTTPS manifest',
  },
  httpsManifestDescription: {
    id: 'mcpCenter.httpsManifestDescription',
    defaultMessage: 'Enter an HTTPS manifest URL to inspect its safe preview before continuing.',
  },
  httpsManifestUrlLabel: {
    id: 'mcpCenter.httpsManifestUrlLabel',
    defaultMessage: 'HTTPS manifest URL',
  },
  httpsManifestHelp: {
    id: 'mcpCenter.httpsManifestHelp',
    defaultMessage:
      'Only HTTPS URLs are accepted. The URL is checked by Goose before any plan is created.',
  },
  httpsManifestInvalidUrl: {
    id: 'mcpCenter.httpsManifestInvalidUrl',
    defaultMessage: 'Enter a valid HTTPS URL.',
  },
  httpsManifestError: {
    id: 'mcpCenter.httpsManifestError',
    defaultMessage: 'The HTTPS manifest could not be imported.',
  },
  httpsManifestPreview: {
    id: 'mcpCenter.httpsManifestPreview',
    defaultMessage: 'HTTPS manifest safety preview',
  },
  manifestId: { id: 'mcpCenter.manifestId', defaultMessage: 'Manifest' },
  origin: { id: 'mcpCenter.origin', defaultMessage: 'Origin' },
  preview: { id: 'mcpCenter.preview', defaultMessage: 'Preview' },
  continue: { id: 'mcpCenter.continue', defaultMessage: 'Continue' },
  mcp: { id: 'mcpCenter.mcp', defaultMessage: 'MCP' },
  provision: { id: 'mcpCenter.provision', defaultMessage: 'Provision' },
});
