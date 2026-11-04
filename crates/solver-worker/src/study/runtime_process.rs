//! Monotonic native-process supervision. Database lease expiry is never compared to host UTC.
use crate::resource::CancellationToken;
use anyhow::{Context, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    time::{Instant, interval},
};

#[derive(Debug)]
pub struct ChildProof {
    pub pid: u32,
    pub peak_rss_bytes: u64,
    pub reaped: bool,
}
#[derive(Debug, thiserror::Error)]
#[error("{code}")]
pub struct NativeFailure {
    pub code: &'static str,
    pub pid: u32,
    pub peak_rss_bytes: u64,
    pub reaped: bool,
    pub probe_kind: Option<&'static str>,
}
const RSS_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(any(target_os = "linux", test))]
const EXIT_CANCEL_POLL: Duration = Duration::from_millis(10);

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, thiserror::Error)]
#[error("study_rss_missing")]
struct LinuxMissingVmRss;

fn probe_kind(error: &anyhow::Error) -> &'static str {
    #[cfg(any(target_os = "linux", test))]
    if error.is::<LinuxMissingVmRss>() {
        return "linux_missing_vm_rss";
    }
    if error.is::<std::io::Error>() {
        "io_error"
    } else if error.is::<std::num::ParseIntError>() {
        "invalid_rss"
    } else {
        "probe_error"
    }
}

fn control_failure(cancel: &CancellationToken, deadline: Instant) -> Option<&'static str> {
    if cancel.is_cancelled() {
        Some("study_job_cancelled")
    } else if Instant::now() >= deadline {
        Some("study_job_timeout")
    } else {
        None
    }
}

fn finished(
    pid: u32,
    peak: u64,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<ChildProof> {
    let failure = control_failure(cancel, deadline);
    if let Some(code) = failure {
        return Err(NativeFailure {
            code,
            pid,
            peak_rss_bytes: peak,
            reaped: true,
            probe_kind: None,
        }
        .into());
    }
    Ok(ChildProof {
        pid,
        peak_rss_bytes: peak,
        reaped: true,
    })
}
fn supported_host() -> bool {
    if (cfg!(target_os = "linux") && !cfg!(target_env = "gnu"))
        || (cfg!(windows) && !cfg!(target_env = "msvc"))
    {
        return false;
    }
    matches!(
        (std::env::consts::OS, std::env::consts::ARCH),
        ("linux", "x86_64" | "aarch64") | ("macos", "aarch64") | ("windows", "x86_64")
    )
}
/// Both cancellation and lease loss set the shared token. This function then kills AND waits;
/// the caller joins this future rather than dropping it when the lease renewal task fails.
pub async fn run_native(
    binary: &Path,
    request: &Path,
    cancel: &CancellationToken,
    deadline: Instant,
    max_rss: u64,
) -> anyhow::Result<ChildProof> {
    ensure!(supported_host(), "study_native_platform_unsupported");
    ensure!(
        binary.is_absolute() && request.is_absolute() && max_rss > 0,
        "study_native_configuration_invalid"
    );
    cancel.check("study_child_launch")?;
    ensure!(Instant::now() < deadline, "study_job_timeout");
    let child = Command::new(binary)
        .arg("--request")
        .arg(request)
        .env_clear()
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("OMP_NUM_THREADS", "1")
        .env("RAYON_NUM_THREADS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    supervise(child, cancel, deadline, max_rss, process_rss).await
}

async fn terminate(
    child: &mut Child,
    pid: u32,
    peak: u64,
    code: &'static str,
    probe_kind: Option<&'static str>,
) -> anyhow::Error {
    let _ = child.kill().await;
    let reaped = child.wait().await.is_ok();
    NativeFailure {
        code,
        pid,
        peak_rss_bytes: peak,
        reaped,
        probe_kind,
    }
    .into()
}

fn exited(
    status: std::process::ExitStatus,
    pid: u32,
    peak: u64,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<ChildProof> {
    if !status.success() {
        return Err(NativeFailure {
            code: "study_native_execution_failed",
            pid,
            peak_rss_bytes: peak,
            reaped: true,
            probe_kind: None,
        }
        .into());
    }
    finished(pid, peak, cancel, deadline)
}

// Linux may drop the exiting task's mm (and VmRSS) before it becomes waitable.
// Reconcile only that typed condition, once, for at most one sampling period.
// No additional successful sample or zero-RSS substitution is manufactured.
#[cfg(any(target_os = "linux", test))]
async fn reconcile_exit(
    child: &mut Child,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<std::process::ExitStatus, &'static str> {
    let until = (Instant::now() + RSS_INTERVAL).min(deadline);
    loop {
        if let Some(code) = control_failure(cancel, deadline) {
            return Err(code);
        }
        let now = Instant::now();
        if now >= until {
            return Err("study_native_memory_probe_failed");
        }
        tokio::select! { biased;
            status = child.wait() => {
                if let Some(code) = control_failure(cancel, deadline) { return Err(code); }
                if Instant::now() >= until { return Err("study_native_memory_probe_failed"); }
                return status.map_err(|_| "study_native_execution_failed");
            },
            () = tokio::time::sleep_until((now + EXIT_CANCEL_POLL).min(until)) => {}
        }
    }
}

async fn supervise<F, Fut>(
    mut child: Child,
    cancel: &CancellationToken,
    deadline: Instant,
    max_rss: u64,
    mut probe: F,
) -> anyhow::Result<ChildProof>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<u64>>,
{
    let pid = child.id().context("study_child_pid_missing")?;
    let mut tick = interval(RSS_INTERVAL);
    let mut peak = 0;
    loop {
        tokio::select! { biased;
            status = child.wait() => {
                return match status {
                    Ok(status) => exited(status, pid, peak, cancel, deadline),
                    Err(_) => Err(terminate(&mut child, pid, peak, "study_native_execution_failed", None).await),
                };
            }
            _ = tick.tick() => {}
        }
        if let Some(code) = control_failure(cancel, deadline) {
            return Err(terminate(&mut child, pid, peak, code, None).await);
        }
        match probe(pid).await {
            Ok(rss) => {
                peak = peak.max(rss);
                if rss > max_rss {
                    return Err(terminate(
                        &mut child,
                        pid,
                        peak,
                        "study_native_memory_limit",
                        None,
                    )
                    .await);
                }
            }
            Err(error) => {
                let kind = probe_kind(&error);
                tracing::warn!(pid, probe_kind = kind, "Study native RSS probe failed");
                match child.try_wait() {
                    Ok(Some(status)) => return exited(status, pid, peak, cancel, deadline),
                    Err(_) => {
                        return Err(terminate(
                            &mut child,
                            pid,
                            peak,
                            "study_native_execution_failed",
                            Some(kind),
                        )
                        .await);
                    }
                    Ok(None) => {}
                }
                #[cfg(any(target_os = "linux", test))]
                if error.is::<LinuxMissingVmRss>() {
                    match reconcile_exit(&mut child, cancel, deadline).await {
                        Ok(status) => return exited(status, pid, peak, cancel, deadline),
                        Err(code) => {
                            return Err(terminate(&mut child, pid, peak, code, Some(kind)).await);
                        }
                    }
                }
                return Err(terminate(
                    &mut child,
                    pid,
                    peak,
                    "study_native_memory_probe_failed",
                    Some(kind),
                )
                .await);
            }
        }
    }
}
#[cfg(target_os = "linux")]
async fn process_rss(pid: u32) -> anyhow::Result<u64> {
    let text = tokio::fs::read_to_string(format!("/proc/{pid}/status")).await?;
    linux_status_rss(&text)
}
#[cfg(target_os = "linux")]
fn linux_status_rss(text: &str) -> anyhow::Result<u64> {
    let row = text
        .lines()
        .find(|s| s.starts_with("VmRSS:"))
        .ok_or(LinuxMissingVmRss)?;
    row.split_whitespace()
        .nth(1)
        .context("study_rss_missing")?
        .parse::<u64>()?
        .checked_mul(1024)
        .context("study_rss_overflow")
}
#[cfg(target_os = "macos")]
async fn process_rss(pid: u32) -> anyhow::Result<u64> {
    use tokio::io::AsyncReadExt;
    let mut child = Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().context("study_rss_pipe")?;
    let reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .take(128)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let status =
        if let Ok(status) = tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
            status?
        } else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = reader.await;
            anyhow::bail!("study_rss_probe_timeout");
        };
    let bytes = reader.await??;
    ensure!(status.success(), "study_rss_probe_failed");
    std::str::from_utf8(&bytes)?
        .trim()
        .parse::<u64>()?
        .checked_mul(1024)
        .context("study_rss_overflow")
}
#[cfg(windows)]
async fn process_rss(pid: u32) -> anyhow::Result<u64> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak: usize,
        working: usize,
        paged_peak: usize,
        paged: usize,
        nonpaged_peak: usize,
        nonpaged: usize,
        pagefile: usize,
        pagefile_peak: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn K32GetProcessMemoryInfo(handle: *mut c_void, counters: *mut Counters, size: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let size = u32::try_from(std::mem::size_of::<Counters>())?;
    let mut counters = Counters {
        cb: size,
        faults: 0,
        peak: 0,
        working: 0,
        paged_peak: 0,
        paged: 0,
        nonpaged_peak: 0,
        nonpaged: 0,
        pagefile: 0,
        pagefile_peak: 0,
    };
    // The handle refers only to our just-spawned child; structure/size follow PROCESS_MEMORY_COUNTERS.
    let success = unsafe {
        let handle = OpenProcess(0x0400 | 0x0010, 0, pid);
        ensure!(!handle.is_null(), "study_rss_handle");
        let ok = K32GetProcessMemoryInfo(handle, &raw mut counters, size);
        CloseHandle(handle);
        ok
    };
    ensure!(success != 0, "study_rss_probe_failed");
    Ok(u64::try_from(counters.working)?)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
async fn process_rss(_: u32) -> anyhow::Result<u64> {
    anyhow::bail!("study_native_platform_unsupported")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn helper(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("native-helper");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s' $$ > \"$2\"\nexec /bin/sleep 60\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn assert_reaped(error: &anyhow::Error) {
        let failure = error.downcast_ref::<NativeFailure>().unwrap();
        assert!(failure.reaped);
        assert_pid_reaped(failure.pid);
    }
    fn assert_pid_reaped(pid: u32) {
        let pid = libc::pid_t::try_from(pid).unwrap();
        let mut status = 0;
        // The supervised child must have been awaited; an exited-but-unreaped child returns its PID.
        let result = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    #[tokio::test]
    async fn timeout_kills_and_reaps_actual_native_child() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let error = run_native(
            &helper(dir.path()),
            &path,
            &CancellationToken::default(),
            Instant::now() + Duration::from_millis(250),
            512 * 1024 * 1024,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timeout"));
        assert_reaped(&error);
    }
    #[tokio::test]
    async fn cancellation_joins_child_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let cancel = CancellationToken::default();
        let signal = cancel.clone();
        let trigger = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            signal.cancel();
        });
        let error = run_native(
            &helper(dir.path()),
            &path,
            &cancel,
            Instant::now() + Duration::from_secs(5),
            512 * 1024 * 1024,
        )
        .await
        .unwrap_err();
        trigger.await.unwrap();
        assert!(error.to_string().contains("cancelled"));
        assert_reaped(&error);
    }
    #[tokio::test]
    async fn rss_limit_terminates_child_and_retains_no_zombie() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let error = run_native(
            &helper(dir.path()),
            &path,
            &CancellationToken::default(),
            Instant::now() + Duration::from_secs(5),
            1,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("memory_limit"));
        assert_reaped(&error);
    }
    // FIFO control keeps the owned shell alive until a test releases it; no child
    // sleep subprocess or wall-clock guess establishes when reconciliation starts.
    fn blocked_child(dir: &Path, exit_code: u8) -> (Child, std::fs::File) {
        use std::os::unix::ffi::OsStrExt;
        let path = dir.join("exit.fifo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "read signal < \"$1\"; exit \"$2\"",
                "native-test-helper",
            ])
            .arg(path)
            .arg(exit_code.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        (child, writer)
    }

    #[tokio::test]
    async fn missing_vm_rss_reconciles_success_and_preserves_sampled_peak() {
        for exit_code in [0, 7] {
            let dir = tempfile::tempdir().unwrap();
            let (child, writer) = blocked_child(dir.path(), exit_code);
            let pid = child.id().unwrap();
            let mut writer = Some(writer);
            let mut sampled = false;
            let result = supervise(
                child,
                &CancellationToken::default(),
                Instant::now() + Duration::from_secs(5),
                16384,
                |_| {
                    if !sampled {
                        sampled = true;
                        return std::future::ready(Ok(8192));
                    }
                    let mut writer = writer.take().unwrap();
                    // This task cannot release the child before the synchronous
                    // try_wait immediately following our ready probe result.
                    tokio::spawn(async move {
                        use std::io::Write;
                        tokio::task::yield_now().await;
                        writer.write_all(b"exit\n").unwrap();
                    });
                    std::future::ready(Err(LinuxMissingVmRss.into()))
                },
            )
            .await;
            if exit_code == 0 {
                let proof = result.unwrap();
                assert_eq!(proof.peak_rss_bytes, 8192);
                assert!(proof.reaped);
            } else {
                let error = result.unwrap_err();
                let failure = error.downcast_ref::<NativeFailure>().unwrap();
                assert_eq!(failure.code, "study_native_execution_failed");
                assert_eq!(failure.peak_rss_bytes, 8192);
                assert_reaped(&error);
            }
            assert_pid_reaped(pid);
        }
    }

    #[tokio::test]
    async fn missing_vm_rss_of_live_child_fails_after_one_period_and_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let (child, _writer) = blocked_child(dir.path(), 0);
        let mut calls = 0;
        let started = Instant::now();
        let error = supervise(
            child,
            &CancellationToken::default(),
            started + Duration::from_secs(5),
            16384,
            |_| {
                calls += 1;
                std::future::ready(Err(LinuxMissingVmRss.into()))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(calls, 1); // No second probe or renewed grace period.
        assert!(started.elapsed() >= RSS_INTERVAL);
        let failure = error.downcast_ref::<NativeFailure>().unwrap();
        assert_eq!(failure.code, "study_native_memory_probe_failed");
        assert_eq!(failure.probe_kind, Some("linux_missing_vm_rss"));
        assert_eq!(failure.peak_rss_bytes, 0);
        assert_reaped(&error);
    }

    #[tokio::test]
    async fn cancellation_during_exit_reconciliation_kills_and_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let (child, _writer) = blocked_child(dir.path(), 0);
        let cancel = CancellationToken::default();
        let signal = cancel.clone();
        let error = supervise(
            child,
            &cancel,
            Instant::now() + Duration::from_secs(5),
            16384,
            |_| {
                let signal = signal.clone();
                tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    signal.cancel();
                });
                std::future::ready(Err(LinuxMissingVmRss.into()))
            },
        )
        .await
        .unwrap_err();
        let failure = error.downcast_ref::<NativeFailure>().unwrap();
        assert_eq!(failure.code, "study_job_cancelled");
        assert_eq!(failure.probe_kind, Some("linux_missing_vm_rss"));
        assert_reaped(&error);
    }

    #[tokio::test]
    async fn original_deadline_bounds_exit_reconciliation_and_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let (child, _writer) = blocked_child(dir.path(), 0);
        let error = supervise(
            child,
            &CancellationToken::default(),
            Instant::now() + Duration::from_millis(30),
            16384,
            |_| std::future::ready(Err(LinuxMissingVmRss.into())),
        )
        .await
        .unwrap_err();
        let failure = error.downcast_ref::<NativeFailure>().unwrap();
        assert_eq!(failure.code, "study_job_timeout");
        assert_eq!(failure.probe_kind, Some("linux_missing_vm_rss"));
        assert_reaped(&error);
    }

    #[tokio::test]
    async fn other_live_probe_failures_do_not_receive_exit_grace() {
        let errors: [anyhow::Error; 2] = [
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
            "invalid".parse::<u64>().unwrap_err().into(),
        ];
        for (error, kind) in errors.into_iter().zip(["io_error", "invalid_rss"]) {
            let dir = tempfile::tempdir().unwrap();
            let (child, _writer) = blocked_child(dir.path(), 0);
            let mut error = Some(error);
            let error = supervise(
                child,
                &CancellationToken::default(),
                // If mistakenly reconciled, this deadline would yield timeout.
                Instant::now() + Duration::from_millis(50),
                16384,
                |_| std::future::ready(Err(error.take().unwrap())),
            )
            .await
            .unwrap_err();
            let failure = error.downcast_ref::<NativeFailure>().unwrap();
            assert_eq!(failure.code, "study_native_memory_probe_failed");
            assert_eq!(failure.probe_kind, Some(kind));
            assert_reaped(&error);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_absent_linux_vm_rss_row_is_reconcilable() {
        let absent = linux_status_rss("State:\tR (running)\n").unwrap_err();
        assert!(absent.is::<LinuxMissingVmRss>());
        assert_eq!(probe_kind(&absent), "linux_missing_vm_rss");
        assert_eq!(linux_status_rss("VmRSS:\t12 kB\n").unwrap(), 12288);
        for row in [
            "VmRSS:\tbroken kB",
            "VmRSS:",
            "VmRSS: 18446744073709551615 kB",
        ] {
            assert!(!linux_status_rss(row).unwrap_err().is::<LinuxMissingVmRss>());
        }
    }
    #[cfg(target_os = "linux")]
    fn exit_mapping_shape() -> (usize, usize) {
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        assert!(page_bytes > 0);
        ((80 * 1024 * 1024 / page_bytes).min(20_000), page_bytes)
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "subprocess fixture for explicit Linux exit-race qualification"]
    fn linux_exit_rss_probe_child() {
        // At most 20,000 alternating-protection private pages / 80 MiB virtual,
        // untouched (not resident). The kernel releases the VMAs on process exit.
        let (pages, page_bytes) = exit_mapping_shape();
        for index in 0..pages {
            let protection = libc::PROT_READ | if index % 2 == 0 { libc::PROT_WRITE } else { 0 };
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page_bytes,
                    protection,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(mapping, libc::MAP_FAILED);
        }
        // Do not unmap in userspace: this fixture measures kernel exit-mm teardown.
        std::process::exit(0);
    }

    #[cfg(target_os = "linux")]
    fn log_exit_race_proof(
        proof: &ChildProof,
        samples: u32,
        observed: Instant,
        attempt: u32,
    ) -> anyhow::Result<()> {
        let (pages, page_bytes) = exit_mapping_shape();
        eprintln!(
            "{}",
            serde_json::json!({
                "schemaVersion":"lca.study-linux-exit-race-proof.v1",
                "scope":"actual OS exit race; original hosted failure cause remains unknown",
                "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
                "sourceCommit":std::env::var("GITHUB_SHA").ok(),
                "attempt":attempt,"maxAttempts":24,"wholeBudgetSeconds":5,
                "virtualMappingPages":pages,"pageBytes":page_bytes,
                "probeKind":"linux_missing_vm_rss","immediateTryWait":"none",
                "successfulExit":true,"reaped":proof.reaped,
                "sampledPeakRssBytes":proof.peak_rss_bytes,"samples":samples,
                "exitReconciliationMicros":u64::try_from(observed.elapsed().as_micros())?
            })
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn qualification_try_wait(
        child: &mut Child,
        pid: u32,
        peak: u64,
    ) -> anyhow::Result<Option<std::process::ExitStatus>> {
        match child.try_wait() {
            Ok(status) => Ok(status),
            Err(_) => Err(terminate(child, pid, peak, "study_native_execution_failed", None).await),
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "explicit actual Linux missing-VmRSS/try_wait race qualification"]
    async fn linux_exit_rss_race_reconciles_actual_child() -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        for attempt in 1..=24 {
            let mut child = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "study::runtime_process::tests::linux_exit_rss_probe_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let pid = child.id().context("exit_race_child_pid")?;
            let mut peak = 0;
            let mut samples = 0;
            loop {
                if Instant::now() >= deadline {
                    return Err(terminate(&mut child, pid, peak, "study_job_timeout", None).await);
                }
                let probe = match tokio::time::timeout_at(deadline, process_rss(pid)).await {
                    Ok(probe) => probe,
                    Err(_) => {
                        return Err(
                            terminate(&mut child, pid, peak, "study_job_timeout", None).await
                        );
                    }
                };
                if let Err(error) = &probe {
                    if error.is::<LinuxMissingVmRss>()
                        && qualification_try_wait(&mut child, pid, peak)
                            .await?
                            .is_none()
                    {
                        let observed = Instant::now();
                        let status = match reconcile_exit(
                            &mut child,
                            &CancellationToken::default(),
                            deadline,
                        )
                        .await
                        {
                            Ok(status) => status,
                            Err(code) => {
                                return Err(terminate(
                                    &mut child,
                                    pid,
                                    peak,
                                    code,
                                    Some("linux_missing_vm_rss"),
                                )
                                .await);
                            }
                        };
                        let proof =
                            exited(status, pid, peak, &CancellationToken::default(), deadline)?;
                        assert!(proof.reaped);
                        assert_eq!(proof.peak_rss_bytes, peak);
                        assert_pid_reaped(pid);
                        log_exit_race_proof(&proof, samples, observed, attempt)?;
                        return Ok(());
                    }
                }
                if let Some(status) = qualification_try_wait(&mut child, pid, peak).await? {
                    assert!(status.success());
                    assert_pid_reaped(pid);
                    break;
                }
                match probe {
                    Ok(rss) => {
                        peak = peak.max(rss);
                        samples += 1;
                    }
                    Err(error) if error.is::<LinuxMissingVmRss>() => {}
                    Err(error) => {
                        return Err(terminate(
                            &mut child,
                            pid,
                            peak,
                            "study_native_memory_probe_failed",
                            Some(probe_kind(&error)),
                        )
                        .await);
                    }
                }
            }
        }
        anyhow::bail!("linux_exit_rss_race_not_observed_in_bounded_attempts")
    }
}
