//! Pedaços específicos do Windows. Em outros sistemas viram no-op,
//! só pra o projeto compilar e dar pra testar a lógica.

use std::process::{Child, Command};

/// Command que não abre janela de console no Windows.
pub fn hidden_command(program: &std::path::Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND};
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    static JOB: OnceLock<Option<Job>> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            Some(Job(h))
        })
        .as_ref()
        .map(|j| j.0)
    }

    /// Se o RePic fechar (até crashando), o Windows mata o ffmpeg junto.
    pub fn tie_to_app(child: &std::process::Child) {
        if let Some(j) = job() {
            unsafe {
                AssignProcessToJobObject(j, child.as_raw_handle() as HANDLE);
            }
        }
    }

    /// true se já tem outro RePic aberto.
    pub fn already_running() -> bool {
        let name: Vec<u16> = "Local\\RePicSingleInstance\0".encode_utf16().collect();
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            // O handle fica aberto até o processo morrer, de propósito.
            !h.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
        }
    }

    pub fn show_window(hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let h = hwnd as HWND;
        unsafe {
            if IsIconic(h) != 0 {
                ShowWindow(h, SW_RESTORE);
            } else {
                ShowWindow(h, SW_SHOW);
            }
            SetForegroundWindow(h);
        }
    }

    pub fn beep_ok() {
        unsafe {
            MessageBeep(MB_ICONASTERISK);
        }
    }

    pub fn beep_error() {
        unsafe {
            MessageBeep(MB_ICONHAND);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn tie_to_app(_child: &std::process::Child) {}
    pub fn already_running() -> bool {
        false
    }
    pub fn show_window(_hwnd: isize) {}
    pub fn beep_ok() {}
    pub fn beep_error() {}
}

pub use imp::{already_running, beep_error, beep_ok, show_window};

pub fn tie_to_app(child: &Child) {
    imp::tie_to_app(child)
}

/// Abre um arquivo/pasta com o programa padrão.
pub fn open_path(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = hidden_command(std::path::Path::new("explorer"))
        .arg(path)
        .spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Abre o Explorer com o arquivo selecionado.
pub fn reveal_path(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    if let Some(dir) = path.parent() {
        open_path(dir);
    }
}
