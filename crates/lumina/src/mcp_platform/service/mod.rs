mod application;
mod dependencies;
mod dto;
pub(crate) mod port;
mod profile;
mod source_adapter;

pub use crate::mcp_platform::managed_remote::{
    CoreManagedRemoteHttpNetworkPolicy, ManagedRemoteHttpClient, ManagedRemoteResolver,
    RemoteHttpNetworkPolicy, TokioManagedRemoteResolver, UnavailableRemoteHttpNetworkPolicy,
};
#[cfg(test)]
pub(crate) use application::GovernedSourceRefreshFetcher;
pub use application::{McpPlatformService, McpPlatformServiceOptions};
pub use dependencies::{
    Clock, IdGenerator, ManualStdioProvider, ResolvedManualStdioSource, SystemClock,
    UnsupportedManualStdioProvider, UuidGenerator,
};
pub use dto::*;
pub(crate) use port::McpPlatformRepositoryPort;
#[cfg(feature = "integration-test-support")]
pub use port::{HttpsManifestFetchResult, HttpsManifestSourceFetcher};
pub use profile::*;
pub use source_adapter::*;
