//! Windows Job Object 沙箱后端。
//!
//! 用 `CREATE_SUSPENDED` 创建子进程 → `AssignProcessToJobObject` 挂入受限
//! Job → 恢复主线程。Job 限制：
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：Job 句柄释放即终止整个进程树；
//! - `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`：活动进程数上限；
//! - 可选 `JOB_OBJECT_LIMIT_JOB_MEMORY`：Job 内存上限。
//!
//! Job 句柄由独立线程持有到进程退出后才释放（先 `WaitForSingleObject` 再
//! `CloseHandle`），避免 kill-on-close 误杀仍在运行的子进程树。
//!
//! 局限（诚实约束）：Job Object **不直接限制网络与文件系统写路径**；整网
//! 开关与文件/目录白名单需要 WFP 过滤器或 AppContainer（后续项）。运行时
//! 行为由 CI 的 windows-latest 测试矩阵验证。

use crate::Sandbox;
use std::io;
use tokio::process::{Child, Command};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, FALSE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenThread, ResumeThread, WaitForSingleObject, CREATE_SUSPENDED, INFINITE,
    THREAD_SUSPEND_RESUME,
};

/// Windows Job Object 沙箱：进程树隔离 + 活动进程数/内存限制。
#[derive(Debug, Clone)]
pub struct JobSandbox {
    /// 活动进程数上限（同一 Job 内）。
    max_processes: u32,
    /// Job 总内存上限（MB；`None` = 不设内存限制）。
    memory_limit_mb: Option<u64>,
}

impl JobSandbox {
    /// 默认限制：活动进程 ≤ 64、无内存上限。
    pub fn new() -> Self {
        Self {
            max_processes: 64,
            memory_limit_mb: None,
        }
    }

    /// 显式限制（供测试与未来配置接线）。
    pub fn with_limits(max_processes: u32, memory_limit_mb: Option<u64>) -> Self {
        Self {
            max_processes: max_processes.max(1),
            memory_limit_mb,
        }
    }

    /// 创建受限 Job 并 assign 子进程。
    fn spawn_in_job(&self, mut cmd: Command) -> io::Result<Child> {
        // SAFETY：全部调用为 Windows 原生 API；句柄生命周期由下述代码显式管理：
        // job 句柄在 spawn 失败路径立即关闭，成功路径交由 keep_job_alive 线程
        // 在进程退出后关闭；dup 句柄由同一线程关闭；thread 句柄用完即关。
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.BasicLimitInformation.ActiveProcessLimit = self.max_processes;
            if let Some(mb) = self.memory_limit_mb {
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.JobMemoryLimit = (mb as usize).saturating_mul(1024 * 1024);
            }
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return Err(io::Error::last_os_error());
            }

            // 挂起创建，assign 成功后再恢复主线程（避免子进程在挂入 Job 前
            // 抢先派生逃逸进程树）。
            cmd.creation_flags(CREATE_SUSPENDED);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    CloseHandle(job);
                    return Err(e);
                }
            };

            let proc_handle = child.raw_handle().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "child raw handle unavailable")
            })? as HANDLE;
            if AssignProcessToJobObject(job, proc_handle) == 0 {
                let _ = child.kill();
                CloseHandle(job);
                return Err(io::Error::last_os_error());
            }

            let pid = child.id().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "child process id unavailable")
            })?;
            if let Err(e) = resume_primary_thread(pid) {
                let _ = child.kill();
                CloseHandle(job);
                return Err(e);
            }

            keep_job_alive_until_process_exit(job, proc_handle);
            Ok(child)
        }
    }
}

impl Default for JobSandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox for JobSandbox {
    fn sandbox(&self, cmd_executable: &str, cmd_args: &[String]) -> (String, Vec<String>) {
        (cmd_executable.to_string(), cmd_args.to_vec())
    }

    fn name(&self) -> &str {
        "job-object"
    }

    fn is_active(&self) -> bool {
        true
    }

    fn backend_available(&self) -> bool {
        true
    }

    fn spawn(&self, cmd: Command) -> io::Result<Child> {
        self.spawn_in_job(cmd)
    }
}

/// 恢复 CREATE_SUSPENDED 子进程的主线程（Toolhelp 枚举该 pid 的第一个线程）。
unsafe fn resume_primary_thread(pid: u32) -> io::Result<()> {
    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry: THREADENTRY32 = std::mem::zeroed();
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut thread_id = None;
    if Thread32First(snapshot, &mut entry) != 0 {
        loop {
            if entry.th32OwnerProcessID == pid {
                thread_id = Some(entry.th32ThreadID);
                break;
            }
            if Thread32Next(snapshot, &mut entry) == 0 {
                break;
            }
        }
    }
    CloseHandle(snapshot);

    let thread_id = thread_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no suspended primary thread found for spawned process",
        )
    })?;
    let thread = OpenThread(THREAD_SUSPEND_RESUME, FALSE, thread_id);
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    let prev = ResumeThread(thread);
    CloseHandle(thread);
    if prev == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 持有 Job 句柄到进程退出：等待（重复的）进程句柄后再关闭 Job，保证
/// KILL_ON_JOB_CLOSE 语义不误杀仍运行的进程树。
fn keep_job_alive_until_process_exit(job: HANDLE, proc_handle: HANDLE) {
    // 句柄转 usize 跨线程传递（usize 原生 Send）；关闭前再转回 HANDLE。
    // 生命周期约定不变：本线程是 job 句柄的唯一关闭者。
    let job = job as usize;
    let proc_handle = proc_handle as usize;
    std::thread::spawn(move || unsafe {
        let job = job as HANDLE;
        let proc_handle = proc_handle as HANDLE;
        let mut dup: HANDLE = std::ptr::null_mut();
        let ok = DuplicateHandle(
            GetCurrentProcess(),
            proc_handle,
            GetCurrentProcess(),
            &mut dup,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        );
        // DuplicateHandle 失败（罕见）时退化为直接等待 Child 仍持有的句柄：
        // 该线程只等待、不关闭进程句柄（关闭权仍在 Child）。
        let wait_handle = if ok != 0 { dup } else { proc_handle };
        WaitForSingleObject(wait_handle, INFINITE);
        if ok != 0 {
            CloseHandle(dup);
        }
        CloseHandle(job);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sandbox;

    #[test]
    fn job_sandbox_identity() {
        let sb = JobSandbox::default();
        assert_eq!(sb.name(), "job-object");
        assert!(sb.is_active());
        assert!(sb.backend_available());
        let (exe, args) = sb.sandbox("cmd", &["/C".into(), "echo hi".into()]);
        assert_eq!(exe, "cmd");
        assert_eq!(args, vec!["/C", "echo hi"]);
    }

    #[test]
    fn job_sandbox_spawns_command_in_job() {
        let sb = JobSandbox::default();
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "echo job-ok"]);
        let child = sb.spawn(cmd).expect("spawn under job object");
        let out = child.wait_with_output().expect("wait for child");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("job-ok"),
            "got stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(out.status.success());
    }

    #[test]
    fn job_sandbox_limits_configuration() {
        let sb = JobSandbox::with_limits(2, Some(512));
        assert_eq!(sb.max_processes, 2);
        assert_eq!(sb.memory_limit_mb, Some(512));
        let clamped = JobSandbox::with_limits(0, None);
        assert_eq!(clamped.max_processes, 1, "进程数上限至少为 1");
    }
}
