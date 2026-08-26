//! Process-wide authorization for a single fenced effect lifecycle.
//!
//! V41 only serializes grant lifecycle transitions. It does not execute an
//! effect. V42 must bind this guard to a conditional sink consume and receipt;
//! TaskRunner recovery and ACP exposure remain later phases.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

const EFFECT_FENCE_SCOPE: &str = "global";

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EffectFenceRepositoryIdentity {
    pub(crate) instance_id: String,
    pub(crate) path_binding: String,
    pub(crate) key_epoch: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EffectFenceBinding {
    pub(crate) anchor_sequence: u64,
    pub(crate) anchor_root: String,
    pub(crate) fence_epoch: i64,
}

pub(crate) struct EffectFenceGuard {
    identity: EffectFenceRepositoryIdentity,
    binding: Mutex<Option<EffectFenceBinding>>,
    abandoned_mutex_revalidation_required: AtomicBool,
    _mutex: PlatformEffectMutex,
}

impl EffectFenceGuard {
    pub(crate) fn acquire(identity: EffectFenceRepositoryIdentity) -> McpPlatformResult<Self> {
        let mutex = PlatformEffectMutex::acquire(&identity)?;
        Ok(Self {
            abandoned_mutex_revalidation_required: AtomicBool::new(mutex.was_abandoned()),
            _mutex: mutex,
            identity,
            binding: Mutex::new(None),
        })
    }

    pub(crate) fn scope(&self) -> &'static str {
        EFFECT_FENCE_SCOPE
    }

    pub(crate) fn bind(&self, binding: EffectFenceBinding) -> McpPlatformResult<()> {
        let mut current = self.binding.lock().map_err(|_| unavailable())?;
        if current.is_some() {
            return Err(rejected());
        }
        *current = Some(binding);
        Ok(())
    }

    /// `bind` is only called after the repository has completed a fresh anchored
    /// transaction. An abandoned Windows mutex must never authorize an effect
    /// until that verification has occurred.
    pub(crate) fn confirm_abandoned_mutex_revalidated(&self) {
        self.abandoned_mutex_revalidation_required
            .store(false, Ordering::Release);
    }

    pub(crate) fn verify(
        &self,
        identity: &EffectFenceRepositoryIdentity,
        binding: &EffectFenceBinding,
    ) -> McpPlatformResult<()> {
        if self
            .abandoned_mutex_revalidation_required
            .load(Ordering::Acquire)
        {
            return Err(rejected());
        }
        if &self.identity != identity {
            return Err(rejected());
        }
        let current = self.binding.lock().map_err(|_| unavailable())?;
        if current.as_ref() != Some(binding) {
            return Err(rejected());
        }
        Ok(())
    }

    pub(crate) fn advance(
        &self,
        identity: &EffectFenceRepositoryIdentity,
        binding: EffectFenceBinding,
    ) -> McpPlatformResult<()> {
        if self
            .abandoned_mutex_revalidation_required
            .load(Ordering::Acquire)
        {
            return Err(rejected());
        }
        if &self.identity != identity {
            return Err(rejected());
        }
        let mut current = self.binding.lock().map_err(|_| unavailable())?;
        if current.is_none() {
            return Err(rejected());
        }
        *current = Some(binding);
        Ok(())
    }
}

fn unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "effect fence gateway is unavailable or its named mutex was pre-created or inaccessible",
    )
}

fn rejected() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "effect fence gateway rejected stale authorization",
    )
}

#[cfg(windows)]
struct PlatformEffectMutex {
    release: Option<std::sync::mpsc::Sender<()>>,
    owner: Option<std::thread::JoinHandle<()>>,
    was_abandoned: bool,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        mutex_attributes: *const std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> windows_sys::Win32::Foundation::HANDLE;
}

#[cfg(windows)]
impl PlatformEffectMutex {
    fn acquire(identity: &EffectFenceRepositoryIdentity) -> McpPlatformResult<Self> {
        use windows_sys::Win32::Foundation::{
            CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, WAIT_ABANDONED, WAIT_OBJECT_0,
        };
        use windows_sys::Win32::System::Threading::{ReleaseMutex, WaitForSingleObject};

        let name = effect_fence_mutex_name(identity);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if handle.is_null() {
                let _ = ready_tx.send(None);
                return;
            }
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(handle);
                }
                let _ = ready_tx.send(None);
                return;
            }
            let wait = unsafe { WaitForSingleObject(handle, 0) };
            if !matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
                unsafe {
                    CloseHandle(handle);
                }
                let _ = ready_tx.send(None);
                return;
            }
            if ready_tx.send(Some(wait == WAIT_ABANDONED)).is_ok() {
                let _ = release_rx.recv();
            }
            unsafe {
                ReleaseMutex(handle);
                CloseHandle(handle);
            }
        });
        if let Some(was_abandoned) = ready_rx.recv().ok().flatten() {
            return Ok(Self {
                release: Some(release_tx),
                owner: Some(owner),
                was_abandoned,
            });
        }
        let _ = owner.join();
        Err(unavailable())
    }

    fn was_abandoned(&self) -> bool {
        self.was_abandoned
    }
}

#[cfg(windows)]
fn effect_fence_mutex_name(identity: &EffectFenceRepositoryIdentity) -> Vec<u16> {
    use sha2::{Digest as _, Sha256};

    let key_epoch = identity.key_epoch.to_string();
    let mut hasher = Sha256::new();
    for value in [
        "lumina.mcp-platform.effect-fence.v41",
        EFFECT_FENCE_SCOPE,
        identity.instance_id.as_str(),
        identity.path_binding.as_str(),
        key_epoch.as_str(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest = crate::utils::bytes_to_hex(hasher.finalize());
    // Local names are visible to processes in the current Windows login session,
    // not across sessions. The default token DACL applies only when this call
    // creates the object: a pre-created or inaccessible object is rejected, not
    // trusted. This intentionally does not prevent same-session denial of service.
    format!("Local\\Lumina.EffectFence.{digest}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
impl Drop for PlatformEffectMutex {
    fn drop(&mut self) {
        self.release.take();
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

#[cfg(not(windows))]
struct PlatformEffectMutex;

#[cfg(not(windows))]
impl PlatformEffectMutex {
    fn acquire(_: &EffectFenceRepositoryIdentity) -> McpPlatformResult<Self> {
        Err(unavailable())
    }

    fn was_abandoned(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(path_binding: &str) -> EffectFenceRepositoryIdentity {
        EffectFenceRepositoryIdentity {
            instance_id: "instance".to_string(),
            path_binding: path_binding.to_string(),
            key_epoch: 1,
        }
    }

    #[cfg(windows)]
    #[test]
    fn same_identity_is_exclusive_and_drop_releases_it() {
        fn assert_send<T: Send>() {}

        assert_send::<EffectFenceGuard>();
        let first = EffectFenceGuard::acquire(identity("a")).unwrap();
        assert!(EffectFenceGuard::acquire(identity("a")).is_err());
        let other_database = EffectFenceGuard::acquire(identity("b")).unwrap();
        drop(other_database);
        drop(first);
        assert!(EffectFenceGuard::acquire(identity("a")).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn named_mutex_is_exclusive_across_test_processes_and_releases() {
        let parent = EffectFenceGuard::acquire(identity("cross-process")).unwrap();
        let parent = assert_child_mutex_result("reject", Some(parent));
        drop(parent);
        assert_child_mutex_result("acquire", None);
    }

    #[cfg(windows)]
    #[test]
    fn different_identity_is_available_across_test_processes() {
        let parent = EffectFenceGuard::acquire(identity("cross-process-parent")).unwrap();
        assert_child_mutex_result("different-identity", Some(parent));
    }

    #[cfg(windows)]
    #[test]
    fn precreated_named_mutex_is_rejected() {
        let precreated = precreate_effect_fence_mutex(&identity("precreated"));
        assert!(EffectFenceGuard::acquire(identity("precreated")).is_err());
        drop(precreated);
    }

    #[cfg(windows)]
    #[test]
    fn abandoned_mutex_requires_revalidated_anchor_before_authorization() {
        let guard = EffectFenceGuard {
            identity: identity("abandoned"),
            binding: Mutex::new(None),
            abandoned_mutex_revalidation_required: AtomicBool::new(true),
            _mutex: PlatformEffectMutex {
                release: None,
                owner: None,
                was_abandoned: true,
            },
        };
        let binding = EffectFenceBinding {
            anchor_sequence: 1,
            anchor_root: "a".repeat(64),
            fence_epoch: 0,
        };
        guard.bind(binding.clone()).unwrap();
        assert!(guard.verify(&identity("abandoned"), &binding).is_err());
        guard.confirm_abandoned_mutex_revalidated();
        guard.verify(&identity("abandoned"), &binding).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn effect_fence_child_protocol() {
        let Some(mode) = std::env::var_os("LUMINA_EFFECT_FENCE_CHILD_MODE") else {
            return;
        };
        let path = if mode == "different-identity" {
            "cross-process-child"
        } else {
            "cross-process"
        };
        let guard = EffectFenceGuard::acquire(identity(path));
        let acquired = guard.is_ok();
        assert_eq!(acquired, mode == "acquire" || mode == "different-identity");
    }

    #[cfg(windows)]
    fn assert_child_mutex_result(
        mode: &str,
        parent: Option<EffectFenceGuard>,
    ) -> Option<EffectFenceGuard> {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let executable = match std::env::current_exe() {
            Ok(executable) => executable,
            Err(error) => {
                drop(parent);
                panic!(
                    "child protocol {mode} failed: trigger=spawn_error({})",
                    redacted_io_error(&error)
                );
            }
        };
        let mut child = match Command::new(executable)
            .args([
                "--exact",
                "mcp_platform::effect_fence::tests::effect_fence_child_protocol",
                "--nocapture",
            ])
            .env("LUMINA_EFFECT_FENCE_CHILD_MODE", mode)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                drop(parent);
                panic!(
                    "child protocol {mode} failed: trigger=spawn_error({})",
                    redacted_io_error(&error)
                );
            }
        };
        match poll_child_protocol(&mut child, Instant::now() + Duration::from_secs(10)) {
            Ok(()) => parent,
            Err(trigger) => {
                drop(parent);
                let cleanup = cleanup_child_protocol(&mut child);
                panic!("{}", format_child_protocol_failure(mode, trigger, cleanup));
            }
        }
    }

    #[cfg(windows)]
    trait ChildProtocolProcess {
        type Status: ChildProtocolStatus;

        fn try_wait(&mut self) -> std::io::Result<Option<Self::Status>>;
        fn kill(&mut self) -> std::io::Result<()>;
        fn wait(&mut self) -> std::io::Result<Self::Status>;
    }

    #[cfg(windows)]
    trait ChildProtocolStatus: std::fmt::Display {
        fn success(&self) -> bool;
    }

    #[cfg(windows)]
    impl ChildProtocolStatus for std::process::ExitStatus {
        fn success(&self) -> bool {
            std::process::ExitStatus::success(self)
        }
    }

    #[cfg(windows)]
    impl ChildProtocolProcess for std::process::Child {
        type Status = std::process::ExitStatus;

        fn try_wait(&mut self) -> std::io::Result<Option<Self::Status>> {
            std::process::Child::try_wait(self)
        }

        fn kill(&mut self) -> std::io::Result<()> {
            std::process::Child::kill(self)
        }

        fn wait(&mut self) -> std::io::Result<Self::Status> {
            std::process::Child::wait(self)
        }
    }

    #[cfg(windows)]
    enum ChildProtocolTrigger<S> {
        ChildExit(S),
        PollError(std::io::Error),
        Timeout,
    }

    #[cfg(windows)]
    struct ChildProtocolCleanup<S> {
        kill: std::io::Result<()>,
        wait: std::io::Result<S>,
    }

    #[cfg(windows)]
    fn poll_child_protocol<P>(
        child: &mut P,
        deadline: std::time::Instant,
    ) -> Result<(), ChildProtocolTrigger<P::Status>>
    where
        P: ChildProtocolProcess,
    {
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(ChildProtocolTrigger::ChildExit(status)),
                Err(error) => return Err(ChildProtocolTrigger::PollError(error)),
                Ok(None) if std::time::Instant::now() >= deadline => {
                    return Err(ChildProtocolTrigger::Timeout);
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }

    #[cfg(windows)]
    fn cleanup_child_protocol<P>(child: &mut P) -> ChildProtocolCleanup<P::Status>
    where
        P: ChildProtocolProcess,
    {
        ChildProtocolCleanup {
            kill: child.kill(),
            wait: child.wait(),
        }
    }

    #[cfg(windows)]
    fn format_child_protocol_failure<S>(
        mode: &str,
        trigger: ChildProtocolTrigger<S>,
        cleanup: ChildProtocolCleanup<S>,
    ) -> String
    where
        S: std::fmt::Display,
    {
        let trigger = match trigger {
            ChildProtocolTrigger::ChildExit(status) => {
                format!("child_exit_nonzero(status={status})")
            }
            ChildProtocolTrigger::PollError(error) => {
                format!("poll_error({})", redacted_io_error(&error))
            }
            ChildProtocolTrigger::Timeout => "timeout".to_string(),
        };
        format!(
            "child protocol {mode} failed: trigger={trigger}; cleanup_kill={}; cleanup_wait={}",
            format_kill_result(cleanup.kill),
            format_wait_result(cleanup.wait),
        )
    }

    #[cfg(windows)]
    fn format_kill_result(result: std::io::Result<()>) -> String {
        match result {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("error({})", redacted_io_error(&error)),
        }
    }

    #[cfg(windows)]
    fn format_wait_result<T: std::fmt::Display>(result: std::io::Result<T>) -> String {
        match result {
            Ok(value) => format!("ok(status={value})"),
            Err(error) => format!("error({})", redacted_io_error(&error)),
        }
    }

    #[cfg(windows)]
    fn redacted_io_error(error: &std::io::Error) -> String {
        match error.raw_os_error() {
            Some(code) => format!("kind={:?},os_code={code}", error.kind()),
            None => format!("kind={:?}", error.kind()),
        }
    }

    #[cfg(windows)]
    #[derive(Clone, Copy)]
    struct TestChildStatus {
        success: bool,
        display: &'static str,
    }

    #[cfg(windows)]
    impl std::fmt::Display for TestChildStatus {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.display)
        }
    }

    #[cfg(windows)]
    impl ChildProtocolStatus for TestChildStatus {
        fn success(&self) -> bool {
            self.success
        }
    }

    #[cfg(windows)]
    enum TestChildPoll {
        Running,
        Exit(TestChildStatus),
        Error(std::io::ErrorKind),
    }

    #[cfg(windows)]
    struct TestChildProcess {
        poll: Option<TestChildPoll>,
        kill_error: Option<std::io::ErrorKind>,
        wait_result: Option<Result<TestChildStatus, std::io::ErrorKind>>,
        kill_calls: usize,
        wait_calls: usize,
    }

    #[cfg(windows)]
    impl ChildProtocolProcess for TestChildProcess {
        type Status = TestChildStatus;

        fn try_wait(&mut self) -> std::io::Result<Option<Self::Status>> {
            match self.poll.take().unwrap_or(TestChildPoll::Running) {
                TestChildPoll::Running => Ok(None),
                TestChildPoll::Exit(status) => Ok(Some(status)),
                TestChildPoll::Error(kind) => Err(kind.into()),
            }
        }

        fn kill(&mut self) -> std::io::Result<()> {
            self.kill_calls += 1;
            match self.kill_error.take() {
                Some(kind) => Err(kind.into()),
                None => Ok(()),
            }
        }

        fn wait(&mut self) -> std::io::Result<Self::Status> {
            self.wait_calls += 1;
            match self.wait_result.take().expect("test child wait result") {
                Ok(status) => Ok(status),
                Err(kind) => Err(kind.into()),
            }
        }
    }

    #[cfg(windows)]
    fn failed_child_diagnostic(
        mut child: TestChildProcess,
        deadline: std::time::Instant,
    ) -> (String, usize, usize) {
        let trigger = match poll_child_protocol(&mut child, deadline) {
            Err(trigger) => trigger,
            Ok(()) => panic!("test child unexpectedly succeeded"),
        };
        let cleanup = cleanup_child_protocol(&mut child);
        let diagnostic = format_child_protocol_failure("test", trigger, cleanup);
        (diagnostic, child.kill_calls, child.wait_calls)
    }

    #[cfg(windows)]
    #[test]
    fn child_protocol_poll_error_is_redacted_and_reaped_once() {
        let (diagnostic, kill_calls, wait_calls) = failed_child_diagnostic(
            TestChildProcess {
                poll: Some(TestChildPoll::Error(std::io::ErrorKind::PermissionDenied)),
                kill_error: Some(std::io::ErrorKind::PermissionDenied),
                wait_result: Some(Err(std::io::ErrorKind::ConnectionAborted)),
                kill_calls: 0,
                wait_calls: 0,
            },
            std::time::Instant::now(),
        );

        assert!(diagnostic.contains("trigger=poll_error(kind=PermissionDenied)"));
        assert!(diagnostic.contains("cleanup_kill=error(kind=PermissionDenied)"));
        assert!(diagnostic.contains("cleanup_wait=error(kind=ConnectionAborted)"));
        assert_eq!((kill_calls, wait_calls), (1, 1));

        let redacted = redacted_io_error(&std::io::Error::other("C:\\private\\secret"));
        assert_eq!(redacted, "kind=Other");
    }

    #[cfg(windows)]
    #[test]
    fn child_protocol_timeout_is_reaped_once_without_hanging() {
        let (diagnostic, kill_calls, wait_calls) = failed_child_diagnostic(
            TestChildProcess {
                poll: Some(TestChildPoll::Running),
                kill_error: None,
                wait_result: Some(Ok(TestChildStatus {
                    success: false,
                    display: "exit code 1",
                })),
                kill_calls: 0,
                wait_calls: 0,
            },
            std::time::Instant::now(),
        );

        assert!(diagnostic.contains("trigger=timeout"));
        assert!(diagnostic.contains("cleanup_kill=ok"));
        assert!(diagnostic.contains("cleanup_wait=ok(status=exit code 1)"));
        assert_eq!((kill_calls, wait_calls), (1, 1));
    }

    #[cfg(windows)]
    #[test]
    fn child_protocol_nonzero_exit_is_reaped_once() {
        let (diagnostic, kill_calls, wait_calls) = failed_child_diagnostic(
            TestChildProcess {
                poll: Some(TestChildPoll::Exit(TestChildStatus {
                    success: false,
                    display: "exit code 7",
                })),
                kill_error: None,
                wait_result: Some(Ok(TestChildStatus {
                    success: false,
                    display: "exit code 7",
                })),
                kill_calls: 0,
                wait_calls: 0,
            },
            std::time::Instant::now(),
        );

        assert!(diagnostic.contains("trigger=child_exit_nonzero(status=exit code 7)"));
        assert!(diagnostic.contains("cleanup_kill=ok"));
        assert!(diagnostic.contains("cleanup_wait=ok(status=exit code 7)"));
        assert_eq!((kill_calls, wait_calls), (1, 1));
    }

    #[cfg(windows)]
    struct PrecreatedEffectFenceMutex(windows_sys::Win32::Foundation::HANDLE);

    #[cfg(windows)]
    impl Drop for PrecreatedEffectFenceMutex {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }

    #[cfg(windows)]
    fn precreate_effect_fence_mutex(
        identity: &EffectFenceRepositoryIdentity,
    ) -> PrecreatedEffectFenceMutex {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};

        let name = effect_fence_mutex_name(identity);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        assert!(!handle.is_null());
        assert_ne!(unsafe { GetLastError() }, ERROR_ALREADY_EXISTS);
        PrecreatedEffectFenceMutex(handle)
    }

    #[cfg(windows)]
    #[test]
    fn stale_anchor_or_epoch_binding_is_rejected() {
        let guard = EffectFenceGuard::acquire(identity("a")).unwrap();
        let initial = EffectFenceBinding {
            anchor_sequence: 1,
            anchor_root: "a".repeat(64),
            fence_epoch: 0,
        };
        guard.bind(initial.clone()).unwrap();
        guard.confirm_abandoned_mutex_revalidated();
        guard.verify(&identity("a"), &initial).unwrap();
        let advanced = EffectFenceBinding {
            anchor_sequence: 2,
            anchor_root: "b".repeat(64),
            fence_epoch: 1,
        };
        guard.advance(&identity("a"), advanced).unwrap();
        assert!(guard.verify(&identity("a"), &initial).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_fails_closed() {
        assert!(EffectFenceGuard::acquire(identity("a")).is_err());
    }
}
