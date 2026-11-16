mod access;
#[cfg(feature = "local-inference")]
mod audio_editor;
mod domain;
mod files;
mod formats;
#[cfg(feature = "local-inference")]
mod image_editor;
mod ingest;
mod project_assets;
#[cfg(feature = "local-inference")]
mod project_payload;
#[cfg(feature = "local-inference")]
mod project_task_reuse;
mod store;
#[cfg(feature = "local-inference")]
mod task_configuration;
#[cfg(feature = "local-inference")]
mod task_inputs;
#[cfg(feature = "local-inference")]
mod task_jobs;
#[cfg(feature = "local-inference")]
mod task_output;
#[cfg(feature = "local-inference")]
mod task_publication;
#[cfg(feature = "local-inference")]
mod task_reuse;
#[cfg(feature = "local-inference")]
mod task_service;

pub use access::{
    MediaAccessDisconnectGuard, MediaAccessOwner, MediaAccessPurpose, MediaAccessRequest,
    MediaAccessStore, MediaAccessTicket, MediaContentLease, MediaLocalActionLease,
    MediaTicketError, MAX_MEDIA_ACCESS_LIFETIME, MAX_MEDIA_ACCESS_TICKETS,
};
#[cfg(feature = "local-inference")]
pub(crate) use audio_editor::{apply_wav_time_range, audio_edit_result_id};
pub use domain::*;
#[cfg(feature = "local-inference")]
pub(crate) use image_editor::apply_image_rect_overlay;
#[cfg(feature = "local-inference")]
pub(crate) use image_editor::image_edit_result_id;
#[cfg(feature = "local-inference")]
pub(crate) use ingest::native_result_link;
pub(crate) use ingest::{hydrate_tool_media, persist_tool_media};
#[cfg(feature = "local-inference")]
pub(crate) use project_task_reuse::{
    prepare_project_reused_task, PreparedLocalEdit, PreparedProjectTask, ProjectEditScope,
    ProjectTaskReuseRequest,
};
pub use store::{BeginMediaPublish, MediaResultStore, StagedMedia, VerifiedMediaContent};
#[cfg(feature = "local-inference")]
pub use task_configuration::{
    TaskConfigurationRecord, TaskConfigurationSnapshot, TaskConfigurationStore,
    MAX_TASK_CONFIGURATION_BYTES,
};
#[cfg(feature = "local-inference")]
pub(crate) use task_jobs::{NativeTaskClaim, NativeTaskJobs, NativeTaskTerminal};
#[cfg(feature = "local-inference")]
pub use task_output::{
    import_native_task_output, ImportedTaskOutput, NativeTaskTranscript, MAX_TASK_TRANSCRIPT_BYTES,
};
#[cfg(feature = "local-inference")]
pub(crate) use task_reuse::resolve_media_patch;
#[cfg(feature = "local-inference")]
pub use task_reuse::{prepare_reused_task, TaskReuseRequest};
#[cfg(feature = "local-inference")]
pub(crate) use task_service::configured_task_engines;
#[cfg(feature = "local-inference")]
pub use task_service::{execute_prepared_task, PreparedTaskExecution};
#[cfg(feature = "local-inference")]
pub(crate) use task_service::{
    execute_prepared_task_controlled, execute_project_prepared_task_controlled,
};
