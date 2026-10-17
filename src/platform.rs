//! Pedaços específicos do Windows. Em outros sistemas viram no-op,
//! só pra o projeto compilar e dar pra testar a lógica.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Diário em %APPDATA%\RePic\repic.log, pra entender o que aconteceu num
/// aperto que não dá pra reproduzir aqui (ex.: jogo como administrador).
/// Recomeça quando passa de 512 KB.
pub fn log(msg: &str) {
    use std::io::Write;
    let path = crate::config::app_dir().join("repic.log");
    let append = std::fs::metadata(&path).is_ok_and(|m| m.len() < 512 * 1024);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path);
    if let Ok(mut f) = file {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "{now} {msg}");
    }
}

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
    /// Taxa de atualização (Hz). Gravar acima disso só repete quadros.
    pub refresh: u32,
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
    use super::{Monitor, Toast, log};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS,
        GetLastError, HANDLE, HWND, LocalFree, WAIT_OBJECT_0,
        LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateRoundRectRgn,
        CreateSolidBrush, DEFAULT_CHARSET, DEVMODEW, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOPREFIX,
        DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, ENUM_CURRENT_SETTINGS, EndPaint,
        EnumDisplaySettingsW, FW_SEMIBOLD, FillRect, GetDC, GetMonitorInfoW, HFONT, HMONITOR,
        InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
        MonitorFromPoint, MonitorFromWindow, OUT_DEFAULT_PRECIS, PAINTSTRUCT, ReleaseDC,
        SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT, ValidateRect,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::Security::{
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
        TokenElevation, TokenUser,
    };
    use windows_sys::Win32::UI::Shell::{SHQueryUserNotificationState, ShellExecuteW};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW,
        RegSetKeyValueW,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentProcess, GetCurrentProcessId,
        INFINITE, OpenEventW,
        OpenProcess, OpenProcessToken, SetEvent, WaitForMultipleObjects,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
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
            if h.is_null() {
                // O outro roda como administrador: existe, mas não deixa abrir.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(h);
                return true;
            }
            // O handle fica aberto até o processo morrer, de propósito.
            false
        }
    }

    // --- Administrador ---------------------------------------------------

    /// true se o processo roda como administrador. Se nem dá pra perguntar
    /// (processo protegido), trata como se rodasse.
    fn elevated(pid: u32) -> bool {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return true;
            }
            let mut token = std::ptr::null_mut();
            let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
            CloseHandle(process);
            if opened == 0 {
                return true;
            }
            let mut info: TOKEN_ELEVATION = std::mem::zeroed();
            let mut len = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && info.TokenIsElevated != 0
        }
    }

    pub fn is_admin() -> bool {
        static ADMIN: OnceLock<bool> = OnceLock::new();
        *ADMIN.get_or_init(|| elevated(unsafe { GetCurrentProcessId() }))
    }

    /// pid do programa admin que está na frente, se o RePic não for admin
    /// (aí o Windows não deixa o teclado dele chegar aqui).
    pub fn foreground_admin_app() -> Option<u32> {
        if is_admin() {
            return None;
        }
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(fg, &mut pid);
            (pid != 0 && pid != GetCurrentProcessId() && elevated(pid)).then_some(pid)
        }
    }

    fn signal_name(s: super::Signal) -> Vec<u16> {
        wide(match s {
            super::Signal::Show => "Local\\RePicShow",
            super::Signal::Quit => "Local\\RePicQuit",
        })
    }

    /// Manda o sinal pro RePic aberto. false = não tem RePic ouvindo.
    pub fn signal_other(s: super::Signal) -> bool {
        unsafe {
            let h = OpenEventW(EVENT_MODIFY_STATE, 0, signal_name(s).as_ptr());
            if h.is_null() {
                return false;
            }
            let ok = SetEvent(h) != 0;
            CloseHandle(h);
            ok
        }
    }

    /// Fica ouvindo os sinais numa thread.
    pub fn listen_signals(on: impl Fn(super::Signal) + Send + 'static) {
        use super::Signal;
        // Qualquer usuário logado pode sinalizar, e o rótulo "integridade
        // média" deixa um processo comum mexer mesmo com o RePic admin.
        let sddl = wide("D:(A;;GA;;;AU)(A;;GA;;;SY)S:(ML;;NW;;;ME)");
        let mut sd = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: if ok != 0 { sd } else { std::ptr::null_mut() },
            bInheritHandle: 0,
        };
        let signals = [Signal::Show, Signal::Quit];
        let events: Vec<isize> = signals
            .iter()
            .map(|&s| unsafe { CreateEventW(&sa, 0, 0, signal_name(s).as_ptr()) as isize })
            .collect();
        if events.iter().any(|&h| h == 0) {
            log("sinais: não consegui criar os eventos");
            return;
        }
        std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                let handles: Vec<HANDLE> = events.iter().map(|&h| h as HANDLE).collect();
                loop {
                    let r = unsafe {
                        WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                    };
                    match r.wrapping_sub(WAIT_OBJECT_0) as usize {
                        i if i < signals.len() => on(signals[i]),
                        _ => return,
                    }
                }
            })
            .ok();
    }

    /// Abre o RePic de novo como administrador (o Windows pergunta).
    /// false = a pessoa recusou ou deu erro.
    pub fn relaunch_as_admin(args: &str) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let (verb, file, params) = (wide("runas"), wide(&exe.to_string_lossy()), wide(args));
        // ShellExecute devolve > 32 quando deu certo.
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            ) as isize
                > 32
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

    // --- Aviso na tela ---------------------------------------------------

    /// Texto e tipo do aviso que está (ou vai estar) na tela.
    static TOAST: Mutex<Option<(Vec<u16>, Toast)>> = Mutex::new(None);
    static TOAST_HWND: AtomicIsize = AtomicIsize::new(0);
    static TOAST_STARTED: AtomicBool = AtomicBool::new(false);
    const TOAST_SHOW: u32 = WM_APP + 1;
    const TOAST_TIMER: usize = 1;

    /// TOAST_HWND quando a janela do aviso não pôde ser criada.
    const TOAST_FAILED: isize = -1;

    /// Mostra um aviso no canto do monitor onde está a janela da frente
    /// (o jogo), por cima de tudo e sem roubar o foco. false = não dá pra
    /// mostrar avisos (aí quem chamou avisa de outro jeito).
    pub fn toast(text: &str, kind: Toast) -> bool {
        *TOAST.lock().unwrap() = Some((text.encode_utf16().collect(), kind));
        if !TOAST_STARTED.swap(true, Ordering::SeqCst) {
            let started = std::thread::Builder::new()
                .name("toast".into())
                .spawn(toast_thread)
                .is_ok();
            if !started {
                TOAST_HWND.store(TOAST_FAILED, Ordering::SeqCst);
            }
            return started; // a thread mostra o texto quando a janela ficar pronta
        }
        match TOAST_HWND.load(Ordering::SeqCst) {
            TOAST_FAILED => false,
            0 => true, // ainda criando: vai ler o texto novo
            h => unsafe {
                PostMessageW(h as HWND, TOAST_SHOW, 0, 0);
                true
            },
        }
    }

    fn toast_thread() {
        unsafe {
            let class = wide("RePicToast");
            let module = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(toast_proc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            // Clicável "através" (TRANSPARENT) e sem foco (NOACTIVATE): o jogo
            // continua recebendo tudo.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                TOAST_HWND.store(TOAST_FAILED, Ordering::SeqCst);
                return;
            }
            SetLayeredWindowAttributes(hwnd, 0, 240, LWA_ALPHA);
            // Fora da gravação: senão o "Clipando…" sai no próprio clipe
            // (Windows 10 2004+; nos antigos a chamada só falha).
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            TOAST_HWND.store(hwnd as isize, Ordering::SeqCst);
            PostMessageW(hwnd, TOAST_SHOW, 0, 0);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
    }

    /// Tamanho de tudo: segue a escala do Windows naquele monitor (125%…)
    /// e, em monitor maior que 1080p, a altura dele.
    fn monitor_scale(mon: HMONITOR) -> f32 {
        unsafe {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(mon, &mut mi);
            let by_height = (mi.rcMonitor.bottom - mi.rcMonitor.top) as f32 / 1080.0;
            let (mut dpi_x, mut dpi_y) = (96, 96);
            GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            by_height.max(dpi_y as f32 / 96.0).max(1.0)
        }
    }

    fn toast_scale(hwnd: HWND) -> f32 {
        monitor_scale(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })
    }

    fn toast_font(scale: f32) -> HFONT {
        let face = wide("Segoe UI");
        unsafe {
            CreateFontW(
                -(17.0 * scale) as i32,
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                0,
                face.as_ptr(),
            )
        }
    }

    unsafe fn toast_show(hwnd: HWND) {
        let Some((mut text, kind)) = TOAST.lock().unwrap().clone() else {
            return;
        };
        unsafe {
            // Monitor do jogo, não o do RePic.
            let fg = GetForegroundWindow();
            let mon = if fg.is_null() {
                // Trocando de janela: usa o principal.
                MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)
            } else {
                MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST)
            };
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) == 0 {
                return;
            }
            let m = mi.rcMonitor;
            let scale = monitor_scale(mon);
            let px = |v: f32| (v * scale) as i32;

            // Mede o texto.
            let dc = GetDC(hwnd);
            let font = toast_font(scale);
            let old = SelectObject(dc, font);
            let mut r: RECT = std::mem::zeroed();
            DrawTextW(dc, text.as_mut_ptr(), text.len() as i32, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, old);
            DeleteObject(font);
            ReleaseDC(hwnd, dc);

            let w = (r.right - r.left + px(48.0)).min(px(680.0));
            let h = px(52.0);
            let x = m.right - w - px(24.0);
            let y = m.top + px(24.0);
            SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, px(12.0), px(12.0)), 1);
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            InvalidateRect(hwnd, std::ptr::null(), 1);
            // "Salvando…" fica até chegar o resultado (com um limite).
            let ms = match kind {
                Toast::Busy => 30_000,
                Toast::Ok => 3_000,
                Toast::Error => 5_000,
            };
            SetTimer(hwnd, TOAST_TIMER, ms, None);

            // 3 = jogo em tela cheia exclusiva: aí nada aparece por cima.
            let mut state = 0;
            SHQueryUserNotificationState(&mut state);
            super::log(&format!(
                "aviso \"{}\" em ({x},{y}) {w}x{h}, monitor ({},{})-({},{}), estado do Windows {state}",
                String::from_utf16_lossy(&text),
                m.left,
                m.top,
                m.right,
                m.bottom
            ));
        }
    }

    unsafe fn toast_paint(hwnd: HWND) {
        let Some((mut text, kind)) = TOAST.lock().unwrap().clone() else {
            // Sem isso o WM_PAINT voltaria sem parar.
            unsafe { ValidateRect(hwnd, std::ptr::null()) };
            return;
        };
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut r: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut r);
            let scale = toast_scale(hwnd);
            let px = |v: f32| (v * scale) as i32;

            let bg = CreateSolidBrush(rgb(24, 24, 28));
            FillRect(dc, &r, bg);
            DeleteObject(bg);
            let accent = match kind {
                Toast::Busy => rgb(150, 150, 160),
                Toast::Ok => rgb(70, 200, 120),
                Toast::Error => rgb(235, 64, 64),
            };
            let bar = CreateSolidBrush(accent);
            let bar_rect = RECT { right: px(6.0), ..r };
            FillRect(dc, &bar_rect, bar);
            DeleteObject(bar);

            let font = toast_font(scale);
            let old = SelectObject(dc, font);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, rgb(240, 240, 240));
            let mut tr = RECT {
                left: px(24.0),
                right: r.right - px(18.0),
                ..r
            };
            DrawTextW(
                dc,
                text.as_mut_ptr(),
                text.len() as i32,
                &mut tr,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(dc, old);
            DeleteObject(font);
            EndPaint(hwnd, &ps);
        }
    }

    fn rgb(r: u8, g: u8, b: u8) -> u32 {
        r as u32 | (g as u32) << 8 | (b as u32) << 16
    }

    unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                TOAST_SHOW => toast_show(hwnd),
                WM_PAINT => toast_paint(hwnd),
                WM_TIMER => {
                    KillTimer(hwnd, TOAST_TIMER);
                    ShowWindow(hwnd, SW_HIDE);
                }
                // Nunca pega o foco nem o mouse.
                WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
                WM_NCHITTEST => return HTTRANSPARENT as LRESULT,
                _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
            }
            0
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
                        let mut mode: DEVMODEW = std::mem::zeroed();
                        mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
                        let refresh = if EnumDisplaySettingsW(
                            d.DeviceName.as_ptr(),
                            ENUM_CURRENT_SETTINGS,
                            &mut mode,
                        ) != 0
                        {
                            mode.dmDisplayFrequency
                        } else {
                            60
                        };
                        out.push(Monitor {
                            refresh: if refresh < 24 { 60 } else { refresh },
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

    /// Como admin, o "Executar" do registro faria o Windows perguntar a cada
    /// login; uma tarefa agendada com "privilégios mais altos" abre direto.
    const TASK_NAME: &str = "RePic";

    pub fn autostart_enabled() -> bool {
        run_value_exists() || task_exists()
    }

    fn run_value_exists() -> bool {
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

    fn schtasks(args: &[&str]) -> Result<(), String> {
        let out = super::hidden_command(Path::new("schtasks"))
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err.trim().lines().last().unwrap_or("schtasks").to_string())
        }
    }

    fn task_exists() -> bool {
        schtasks(&["/Query", "/TN", TASK_NAME]).is_ok()
    }

    /// "S-1-5-21-…" do usuário deste processo.
    fn current_user_sid() -> Option<String> {
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut buf = [0u64; 64];
            let mut len = 0;
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                std::mem::size_of_val(&buf) as u32,
                &mut len,
            );
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                return None;
            }
            let n = (0..).take_while(|&i| *s.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s as _);
            Some(sid)
        }
    }

    fn create_task(exe: &Path) -> Result<(), String> {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        // SID e não "PC\usuário": nome com acento ou domínio não atrapalha.
        let user = current_user_sid().ok_or("SID")?;
        // Sem limite de tempo e funcionando na bateria (o padrão do Windows
        // mataria o RePic depois de 3 dias e não abriria em notebook na bateria).
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--tray</Arguments></Exec></Actions>
</Task>"#,
            esc(&exe.to_string_lossy())
        );
        // O schtasks só aceita o XML em UTF-16 (com BOM).
        let bytes: Vec<u8> = std::iter::once(0xFEFF)
            .chain(xml.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let file = std::env::temp_dir().join("repic-task.xml");
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
        let r = schtasks(&["/Create", "/TN", TASK_NAME, "/XML", &file.to_string_lossy(), "/F"]);
        let _ = std::fs::remove_file(&file);
        r
    }

    /// `admin`: abre como administrador (tarefa agendada; precisa que o
    /// RePic já esteja como admin pra criar). Senão, o "Executar" do registro.
    pub fn set_autostart(on: bool, admin: bool) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if on && admin {
            if !is_admin() {
                return Err(crate::lang::t().needs_admin.into());
            }
            create_task(&exe)?;
            return set_run_value(false);
        }
        // Tarefa antiga (de quando era admin): só some se der.
        if task_exists() {
            let _ = schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]);
        }
        set_run_value(on)
    }

    fn set_run_value(on: bool) -> Result<(), String> {
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
    pub fn toast(_text: &str, _kind: super::Toast) -> bool {
        false
    }
    pub fn list_monitors() -> Vec<Monitor> {
        Vec::new()
    }
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_on: bool, _admin: bool) -> Result<(), String> {
        Err("só no Windows".into())
    }
    pub fn is_admin() -> bool {
        false
    }
    pub fn foreground_admin_app() -> Option<u32> {
        None
    }
    pub fn relaunch_as_admin(_args: &str) -> bool {
        false
    }
    pub fn signal_other(_s: super::Signal) -> bool {
        false
    }
    pub fn listen_signals(_on: impl Fn(super::Signal) + Send + 'static) {}
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
    already_running, autostart_enabled, beep_error, beep_ok, foreground_admin_app, is_admin,
    relaunch_as_admin,
    foreground_is_fullscreen, list_monitors, pick_folder, save_dialog, set_autostart, show_window,
    toast,
};

/// Cor do aviso na tela.
#[derive(Clone, Copy)]
pub enum Toast {
    /// Salvando: fica na tela até o resultado.
    Busy,
    Ok,
    Error,
}

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
    // O outro pode ser admin: aí o Windows não deixa mexer na janela dele
    // daqui, mas o sinal chega.
    if imp::signal_other(Signal::Show) {
        return;
    }
    if let Some(h) = std::fs::read_to_string(instance_file())
        .ok()
        .and_then(|s| s.trim().parse::<isize>().ok())
    {
        show_window(h);
    }
}

/// Pedidos de outro processo pro RePic aberto (funcionam mesmo com ele
/// como administrador e quem pede não).
#[derive(Clone, Copy)]
pub enum Signal {
    /// Abriram o .exe de novo: mostra a janela.
    Show,
    /// `RePic.exe --quit` (instalador atualizando/desinstalando).
    Quit,
}

pub use imp::{listen_signals, signal_other};

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
            refresh: 144,
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
