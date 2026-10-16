//! Pedaços específicos do Windows. Em outros sistemas viram no-op,
//! só pra o projeto compilar e dar pra testar a lógica.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Command que não abre janela de console no Windows.
pub fn hidden_command(program: &Path) -> Command {
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

/// Um monitor que dá pra gravar.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// O que fica salvo no config: placa + posição na placa ("10de:0").
    /// O nome do Windows (`\\.\DISPLAY5`) muda quando monitores reconectam.
    pub id: String,
    pub label: String,
    pub primary: bool,
    /// Placa de vídeo onde o monitor está ligado (índice do DXGI).
    pub adapter: u32,
    pub vendor_id: u32,
    /// true se nenhuma outra placa tem o mesmo fabricante (aí dá pra
    /// escolher a placa pelo fabricante, que não muda de ordem).
    pub unique_vendor: bool,
    /// Índice do monitor dentro da placa (o `output_idx` do ddagrab).
    pub output: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Escolhe o monitor salvo no config; se sumiu (ou vazio), o principal.
pub fn pick_monitor(monitors: &[Monitor], id: &str) -> Option<Monitor> {
    monitors
        .iter()
        .find(|m| !id.is_empty() && m.id == id)
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
        .cloned()
}

#[cfg(windows)]
mod imp {
    use super::Monitor;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, HANDLE, HWND,
        RECT,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
        RegSetKeyValueW,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

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
        let name = wide("Local\\RePicSingleInstance");
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
            if IsWindow(h) == 0 {
                return;
            }
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

    // --- Monitores -------------------------------------------------------

    pub fn list_monitors() -> Vec<Monitor> {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let mut out = Vec::new();
        let mut vendors = Vec::new();
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
                return out;
            };
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                let vendor = adapter.GetDesc1().map(|d| d.VendorId).unwrap_or(0);
                vendors.push(vendor);
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    if let Ok(d) = output.GetDesc()
                        && d.AttachedToDesktop.as_bool()
                    {
                        let r = d.DesktopCoordinates;
                        out.push(Monitor {
                            label: String::new(),
                            primary: r.left == 0 && r.top == 0,
                            adapter: a,
                            vendor_id: vendor,
                            unique_vendor: false,
                            output: o,
                            x: r.left,
                            y: r.top,
                            width: r.right - r.left,
                            height: r.bottom - r.top,
                            id: String::new(),
                        });
                    }
                    o += 1;
                }
                a += 1;
            }
        }
        // Principal primeiro, depois da esquerda pra direita.
        out.sort_by_key(|m| (!m.primary, m.x, m.y));
        let t = crate::lang::t();
        for (i, m) in out.iter_mut().enumerate() {
            m.unique_vendor = vendors.iter().filter(|&&v| v == m.vendor_id).count() == 1;
            // Fabricante não muda de ordem; o índice da placa pode mudar.
            m.id = if m.unique_vendor {
                format!("{:04x}:{}", m.vendor_id, m.output)
            } else {
                format!("gpu{}:{}", m.adapter, m.output)
            };
            m.label = crate::lang::fill(t.monitor_label, &[&(i + 1), &m.width, &m.height]);
            if m.primary {
                m.label.push_str(t.primary_suffix);
            }
        }
        out
    }

    // --- Iniciar com o Windows -------------------------------------------

    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RUN_VALUE: &str = "RePic";

    pub fn autostart_enabled() -> bool {
        let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
        let mut size = 0u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            ) == ERROR_SUCCESS
        }
    }

    pub fn set_autostart(on: bool) -> Result<(), String> {
        let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
        let err = unsafe {
            if on {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let data = wide(&format!("\"{}\" --tray", exe.display()));
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    value.as_ptr(),
                    REG_SZ,
                    data.as_ptr() as *const core::ffi::c_void,
                    (data.len() * 2) as u32,
                )
            } else {
                match RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) {
                    ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                    e => e,
                }
            }
        };
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(crate::lang::fill(crate::lang::t().windows_refused, &[&err]))
        }
    }

    // --- Jogo em tela cheia ----------------------------------------------

    /// true se a janela em foco ocupa o monitor inteiro sem barra de título
    /// (jogo em tela cheia ou janela sem bordas, vídeo em tela cheia…).
    pub fn foreground_is_fullscreen() -> bool {
        unsafe {
            let h = GetForegroundWindow();
            if h.is_null() {
                return false;
            }
            let mut class = [0u16; 64];
            let n = GetClassNameW(h, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..n]);
            // Área de trabalho e barra de tarefas não contam.
            if matches!(
                class.as_str(),
                "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
            ) {
                return false;
            }
            // Janela maximizada normal tem barra de título; jogo não.
            let style = GetWindowLongW(h, GWL_STYLE) as u32;
            if style & WS_CAPTION == WS_CAPTION {
                return false;
            }
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(h, &mut r) == 0 {
                return false;
            }
            let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) == 0 {
                return false;
            }
            let m = mi.rcMonitor;
            r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
        }
    }

    // --- Diálogos --------------------------------------------------------

    pub fn save_dialog(file_name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(crate::lang::t().export_dialog)
            .set_file_name(file_name)
            .add_filter(crate::lang::t().video_filter, &["mp4"])
            .save_file()
    }

    pub fn pick_folder(start: &Path) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(crate::lang::t().folder_dialog)
            .set_directory(start)
            .pick_folder()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Monitor;
    use std::path::{Path, PathBuf};

    pub fn tie_to_app(_child: &std::process::Child) {}
    pub fn already_running() -> bool {
        false
    }
    pub fn show_window(_hwnd: isize) {}
    pub fn beep_ok() {}
    pub fn beep_error() {}
    pub fn list_monitors() -> Vec<Monitor> {
        Vec::new()
    }
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_on: bool) -> Result<(), String> {
        Err("só no Windows".into())
    }
    pub fn foreground_is_fullscreen() -> bool {
        true
    }
    pub fn save_dialog(_file_name: &str) -> Option<PathBuf> {
        None
    }
    pub fn pick_folder(_start: &Path) -> Option<PathBuf> {
        None
    }
}

pub use imp::{
    already_running, autostart_enabled, beep_error, beep_ok,
    foreground_is_fullscreen, list_monitors, pick_folder, save_dialog, set_autostart, show_window,
};

pub fn tie_to_app(child: &Child) {
    imp::tie_to_app(child)
}

// --- Uma instância só ----------------------------------------------------
// A janela aberta anota o próprio HWND num arquivo; se alguém abrir o .exe
// de novo, o segundo processo lê e traz a janela da primeira pra frente.

fn instance_file() -> PathBuf {
    crate::config::app_dir().join("instance.hwnd")
}

pub fn remember_window(hwnd: isize) {
    let _ = std::fs::create_dir_all(crate::config::app_dir());
    let _ = std::fs::write(instance_file(), hwnd.to_string());
}

pub fn focus_other_instance() {
    if let Some(h) = std::fs::read_to_string(instance_file())
        .ok()
        .and_then(|s| s.trim().parse::<isize>().ok())
    {
        show_window(h);
    }
}

/// Abre um arquivo/pasta com o programa padrão.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    let _ = hidden_command(Path::new("explorer")).arg(path).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Abre o Explorer com o arquivo selecionado.
pub fn reveal_path(path: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: &str, primary: bool) -> Monitor {
        Monitor {
            id: id.into(),
            label: id.into(),
            primary,
            adapter: 0,
            vendor_id: 0,
            unique_vendor: true,
            output: 0,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn pick_monitor_falls_back_to_primary() {
        let list = [mon("8086:0", false), mon("10de:0", true)];
        assert_eq!(pick_monitor(&list, "").unwrap().id, "10de:0");
        assert_eq!(pick_monitor(&list, "8086:0").unwrap().id, "8086:0");
        // Monitor desconectado (ou config antigo com `\\.\DISPLAY5`): volta pro principal.
        assert_eq!(pick_monitor(&list, "10de:3").unwrap().id, "10de:0");
        assert_eq!(pick_monitor(&list, "\\\\.\\DISPLAY5").unwrap().id, "10de:0");
        assert!(pick_monitor(&[], "").is_none());
    }
}
