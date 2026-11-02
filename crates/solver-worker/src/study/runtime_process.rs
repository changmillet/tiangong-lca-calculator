//! Monotonic native-process supervision. Database lease expiry is never compared to host UTC.
use crate::resource::CancellationToken;
use anyhow::{Context, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    process::Command,
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
}
fn finished(
    pid: u32,
    peak: u64,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<ChildProof> {
    let failure = if cancel.is_cancelled() {
        Some("study_job_cancelled")
    } else if Instant::now() >= deadline {
        Some("study_job_timeout")
    } else {
        None
    };
    if let Some(code) = failure {
        return Err(NativeFailure {
            code,
            pid,
            peak_rss_bytes: peak,
            reaped: true,
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
    let mut child = Command::new(binary)
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
    let pid = child.id().context("study_child_pid_missing")?;
    let mut tick = interval(Duration::from_millis(100));
    let mut peak = 0;
    loop {
        tokio::select! {biased;
            status=child.wait()=>{ensure!(status?.success(),"study_native_execution_failed");return finished(pid,peak,cancel,deadline);}
            _=tick.tick()=>{
                let failure=if cancel.is_cancelled(){Some("study_job_cancelled")}else if Instant::now()>=deadline{Some("study_job_timeout")}else if let Ok(rss)=process_rss(pid).await {peak=peak.max(rss);(rss>max_rss).then_some("study_native_memory_limit")} else {if let Some(status)=child.try_wait()?{ensure!(status.success(),"study_native_execution_failed");return finished(pid,peak,cancel,deadline);}Some("study_native_memory_probe_failed")};
                if let Some(code)=failure {let _=child.kill().await;let reaped=child.wait().await.is_ok();return Err(NativeFailure{code,pid,peak_rss_bytes:peak,reaped}.into());}
            }
        }
    }
}
#[cfg(target_os = "linux")]
async fn process_rss(pid: u32) -> anyhow::Result<u64> {
    let text = tokio::fs::read_to_string(format!("/proc/{pid}/status")).await?;
    let row = text
        .lines()
        .find(|s| s.starts_with("VmRSS:"))
        .context("study_rss_missing")?;
    Ok(row
        .split_whitespace()
        .nth(1)
        .context("study_rss_missing")?
        .parse::<u64>()?
        .checked_mul(1024)
        .context("study_rss_overflow")?)
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
        let pid = libc::pid_t::try_from(failure.pid).unwrap();
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
}
