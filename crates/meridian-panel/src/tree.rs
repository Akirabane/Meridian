//! A process and every process it starts, ended together.
//!
//! A Horizon server's game runs in a child of the server, its engine. Ending the
//! server alone left the engine running, holding the world open, so the next start
//! of that server failed. On Windows, each process the panel starts goes into a job
//! object of its own, and everything it starts goes there too: ending the job ends
//! them all, and the system ends them all when the panel itself ends, however it
//! ends (Ctrl+C, its window closed, killed), since the panel holds the only handle
//! to each job.
//!
//! Elsewhere, only the process itself is ended, as before.
//!
//! This is the panel's one module with unsafe code, for the Windows calls.
use std::process::Child;

/// The processes of one process the panel started.
pub struct Tree(imp::Tree);

impl Tree {
    /// Takes `child`, and every process it starts from now on, into a tree. An
    /// error says why it could not; the child itself is unchanged.
    pub fn adopt(child: &Child) -> std::io::Result<Tree> {
        imp::Tree::adopt(child).map(Tree)
    }

    /// Ends every process of the tree, the child included.
    pub fn end(&self) {
        self.0.end();
    }

    /// How many processes of the tree are running; `None` when it cannot be told.
    pub fn running(&self) -> Option<u32> {
        self.0.running()
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };

    /// A job object, closed when dropped, which ends what is left in it.
    pub struct Tree(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread; this value is
    // its only owner.
    unsafe impl Send for Tree {}
    // SAFETY: the calls made through `&Tree` (TerminateJobObject,
    // QueryInformationJobObject) are safe to make from several threads at once.
    unsafe impl Sync for Tree {}

    impl Tree {
        pub fn adopt(child: &Child) -> std::io::Result<Tree> {
            // SAFETY: no security attributes and no name: plain values; the call
            // returns a new handle, or null on failure.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // From here, the job is closed on every way out.
            let tree = Tree(job);
            // SAFETY: a plain C struct of numbers, for which all zeros is valid.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the job is open; the pointer and the size are those of
            // `limits`, which lives through the call and is only read.
            let set = unsafe {
                SetInformationJobObject(
                    tree.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if set == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the job is open, and the process handle is the child's own,
            // open as long as `child` is borrowed.
            let assigned =
                unsafe { AssignProcessToJobObject(tree.0, child.as_raw_handle() as HANDLE) };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(tree)
        }

        pub fn end(&self) {
            // SAFETY: the job is open until this value is dropped.
            unsafe { TerminateJobObject(self.0, 1) };
        }

        pub fn running(&self) -> Option<u32> {
            // SAFETY: a plain C struct of numbers, for which all zeros is valid.
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: the job is open; the pointer and the size are those of
            // `info`, which the call may write; the returned length is not needed.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast::<c_void>(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            (ok != 0).then_some(info.ActiveProcesses)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed here
            // only, once. The system then ends what is left in the job.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Child;

    /// Nothing to hold: only the child itself is ended.
    pub struct Tree;

    impl Tree {
        pub fn adopt(_child: &Child) -> std::io::Result<Tree> {
            Ok(Tree)
        }

        pub fn end(&self) {}

        pub fn running(&self) -> Option<u32> {
            None
        }
    }
}
