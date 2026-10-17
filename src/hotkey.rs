//! Atalho global via Raw Input do Windows (o mesmo jeito que os jogos leem
//! o teclado).
//!
//! - Só *observa*: a tecla continua chegando no jogo ou onde você digita,
//!   então qualquer tecla serve, sozinha ou com Ctrl/Alt/Shift.
//! - Chega mesmo com a janela fechada (RIDEV_INPUTSINK).
//! - É uma fila de mensagens normal: não tem o problema do gancho de teclado
//!   (WH_KEYBOARD_LL), que o Windows desliga sem avisar quando demora.
//! - Usa a tecla física (código do Windows): igual em ABNT2, US…
//! - Jogo rodando como administrador: o Windows esconde o teclado dele dos
//!   programas comuns (UIPI). O RePic precisa rodar como admin também; sem
//!   isso, só avisa (Event::AdminGame).

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Uma combinação de teclas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Combo {
    /// Código virtual da tecla no Windows (VK_*).
    pub vk: u32,
    /// Código físico, usado só pra mostrar o nome certo da tecla.
    pub scan: u32,
    pub extended: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Default for Combo {
    /// F8.
    fn default() -> Self {
        Self {
            vk: 0x77,
            scan: 0x42,
            extended: false,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
}

pub enum Event {
    /// Apertou o atalho.
    Pressed,
    /// Terminou de escolher um atalho novo (None = cancelou com Esc).
    Captured(Option<Combo>),
    /// Um jogo rodando como administrador veio pra frente e o RePic não é
    /// admin: o atalho não vai chegar.
    AdminGame,
}

const VK_ESCAPE: u32 = 0x1B;

fn is_modifier(vk: u32) -> bool {
    // Shift, Ctrl, Alt (genéricos e esquerdo/direito) e as teclas Windows.
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

/// Uma tecla que chegou.
#[derive(Clone, Copy)]
struct Key {
    vk: u32,
    scan: u32,
    extended: bool,
    down: bool,
    /// (ctrl, alt, shift) no momento.
    mods: (bool, bool, bool),
    /// A janela do RePic está na frente.
    ours: bool,
}

#[derive(Default)]
struct Hook {
    combo: Combo,
    capturing: bool,
    /// A tecla do atalho está pressionada (ignora a repetição de segurar).
    held: bool,
    /// Último aperto enviado (por qualquer caminho).
    last_pressed: Option<Instant>,
    tx: Option<Sender<Event>>,
}

impl Hook {
    fn on_key(&mut self, k: Key) -> Option<Event> {
        if is_modifier(k.vk) {
            return None;
        }
        if !k.down {
            if k.vk == self.combo.vk {
                self.held = false;
            }
            return None;
        }
        if self.capturing {
            // Só aceita a tecla apertada na janela do RePic: senão pegaria
            // o que você estiver digitando em outro programa.
            if !k.ours {
                return None;
            }
            self.capturing = false;
            if k.vk == VK_ESCAPE {
                return Some(Event::Captured(None));
            }
            let (ctrl, alt, shift) = k.mods;
            self.combo = Combo {
                vk: k.vk,
                scan: k.scan,
                extended: k.extended,
                ctrl,
                alt,
                shift,
            };
            // Não clipa por causa do mesmo aperto que escolheu a tecla.
            self.held = true;
            return Some(Event::Captured(Some(self.combo)));
        }
        if k.vk != self.combo.vk {
            return None;
        }
        if self.held {
            return None; // segurando a tecla
        }
        self.held = true;
        let c = self.combo;
        (k.mods == (c.ctrl, c.alt, c.shift)).then_some(Event::Pressed)
    }
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
/// HWND da janela do RePic (0 = ainda não abriu).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

fn with_hook(f: impl FnOnce(&mut Hook)) {
    if let Some(h) = HOOK.lock().unwrap().as_mut() {
        f(h);
    }
}

/// Avisa qual é a janela do RePic (pra escolher a tecla só nela).
pub fn set_window(hwnd: isize) {
    WINDOW.store(hwnd, Ordering::SeqCst);
}

/// A próxima tecla apertada na janela do RePic vira o atalho novo.
pub fn begin_capture() {
    with_hook(|h| h.capturing = true);
}

pub fn cancel_capture() {
    with_hook(|h| h.capturing = false);
}

/// Começa a escutar o teclado. `handler` roda numa thread própria.
pub fn start(combo: Combo, handler: impl Fn(Event) + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    *HOOK.lock().unwrap() = Some(Hook {
        combo,
        tx: Some(tx),
        ..Default::default()
    });
    std::thread::Builder::new()
        .name("hotkey-events".into())
        .spawn(move || {
            for ev in rx {
                handler(ev);
            }
        })
        .expect("thread do atalho");
    imp::install();
}

fn send_event(ev: Event) {
    let guard = HOOK.lock().unwrap();
    if let Some(tx) = guard.as_ref().and_then(|h| h.tx.as_ref()) {
        let _ = tx.send(ev);
    }
}

/// Dois apertos colados (ex.: tecla falhando) contam um só.
const SAME_PRESS: Duration = Duration::from_millis(400);

fn send_pressed(h: &mut Hook, via: &str) {
    if h.last_pressed.is_some_and(|t| t.elapsed() < SAME_PRESS) {
        crate::platform::log(&format!("atalho ({via}): ignorado, mesmo aperto"));
        return;
    }
    h.last_pressed = Some(Instant::now());
    crate::platform::log(&format!("atalho ({via}): apertou"));
    if let Some(tx) = &h.tx {
        let _ = tx.send(Event::Pressed);
    }
}

/// O soltar da tecla pode nunca chegar (trocou pra um jogo admin com ela
/// apertada): libera o `held` se a tecla já está solta.
fn release_if_up(is_down: impl Fn(u32) -> bool) {
    with_hook(|h| {
        if h.held && !is_down(h.combo.vk) {
            h.held = false;
        }
    });
}

fn dispatch(k: Key) {
    let mut guard = HOOK.lock().unwrap();
    let Some(h) = guard.as_mut() else {
        return;
    };
    match h.on_key(k) {
        Some(Event::Pressed) => send_pressed(h, "Raw Input"),
        Some(ev) => {
            if let Some(tx) = &h.tx {
                let _ = tx.send(ev);
            }
        }
        None => {}
    }
}

/// Nome pra mostrar na tela: "F8", "Ctrl+Shift+S", "'".
pub fn label(c: &Combo) -> String {
    let mut s = String::new();
    if c.ctrl {
        s.push_str("Ctrl+");
    }
    if c.alt {
        s.push_str("Alt+");
    }
    if c.shift {
        s.push_str("Shift+");
    }
    s.push_str(&key_name(c));
    s
}

fn key_name(c: &Combo) -> String {
    if (0x70..=0x87).contains(&c.vk) {
        return format!("F{}", c.vk - 0x6F);
    }
    let name = imp::key_name(c.scan, c.extended);
    if name.is_empty() {
        return crate::lang::fill(crate::lang::t().key_n, &[&c.vk]);
    }
    // "INSERT" -> "Insert"; símbolos e letras únicas ficam como estão.
    if name.chars().count() > 1 && name.chars().all(|ch| ch.is_uppercase() || !ch.is_alphabetic()) {
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        return first.to_string() + &chars.as_str().to_lowercase();
    }
    name
}

#[cfg(windows)]
mod imp {
    use super::{Key, WINDOW};
    use std::cell::Cell;
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyNameTextW};
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
        RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GA_ROOT, GetAncestor,
        GetForegroundWindow, GetMessageW, HWND_MESSAGE, MSG, RI_KEY_BREAK, RI_KEY_E0,
        RegisterClassW, SetTimer, WM_INPUT, WM_TIMER, WNDCLASSW,
    };

    thread_local! {
        /// Jogo admin que já ganhou o aviso (pra avisar uma vez por jogo).
        static WARNED_PID: Cell<u32> = const { Cell::new(0) };
    }

    const CHECK_TIMER: usize = 1;

    /// Jogo como administrador na frente e o RePic não: o Windows não deixa
    /// o teclado do jogo chegar aqui (UIPI). Nem RegisterHotKey nem
    /// GetAsyncKeyState passam (testado). Só dá pra avisar, uma vez por jogo.
    fn check_admin_game() {
        let pid = crate::platform::foreground_admin_app().unwrap_or(0);
        if pid != 0 && WARNED_PID.replace(pid) != pid {
            crate::platform::log(&format!("jogo admin na frente (pid {pid}): o atalho não chega"));
            super::send_event(super::Event::AdminGame);
        }
    }

    fn pressed(vk: i32) -> bool {
        unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
    }

    fn our_window_in_front() -> bool {
        let ours = WINDOW.load(Ordering::SeqCst);
        if ours == 0 {
            return false;
        }
        unsafe {
            let fg = GetForegroundWindow();
            !fg.is_null() && GetAncestor(fg, GA_ROOT) as isize == ours
        }
    }

    fn on_raw_input(lparam: LPARAM) {
        // RAWINPUT de teclado é pequeno; buffer alinhado de sobra.
        let mut buf = [0u64; 8];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let got = unsafe {
            GetRawInputData(
                lparam as HRAWINPUT,
                RID_INPUT,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut size,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if got == u32::MAX || got == 0 {
            return;
        }
        let raw = unsafe { &*(buf.as_ptr() as *const RAWINPUT) };
        if raw.header.dwType != RIM_TYPEKEYBOARD {
            return;
        }
        let kb = unsafe { raw.data.keyboard };
        // 255 = pedaço "falso" de sequências especiais (ex.: Pause).
        if kb.VKey == 0 || kb.VKey == 255 {
            return;
        }
        let flags = kb.Flags as u32;
        super::dispatch(Key {
            vk: kb.VKey as u32,
            scan: kb.MakeCode as u32,
            extended: flags & RI_KEY_E0 != 0,
            down: flags & RI_KEY_BREAK == 0,
            // 0x11 Ctrl, 0x12 Alt, 0x10 Shift.
            mods: (pressed(0x11), pressed(0x12), pressed(0x10)),
            ours: our_window_in_front(),
        });
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_INPUT => on_raw_input(lparam),
            WM_TIMER => {
                super::release_if_up(|vk| pressed(vk as i32));
                check_admin_game();
            }
            _ => {}
        }
        // DefWindowProc também libera o RAWINPUT do sistema.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub fn install() {
        std::thread::Builder::new()
            .name("hotkey-input".into())
            .spawn(|| unsafe {
                let class: Vec<u16> = "RePicKeyboard\0".encode_utf16().collect();
                let module = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: module,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
                // Janela invisível só pra receber mensagens.
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    module,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    eprintln!("atalho: não consegui criar a janela de mensagens");
                    return;
                }
                let device = RAWINPUTDEVICE {
                    usUsagePage: 0x01, // Generic Desktop
                    usUsage: 0x06,     // Teclado
                    dwFlags: RIDEV_INPUTSINK,
                    hwndTarget: hwnd,
                };
                if RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) == 0 {
                    eprintln!("atalho: o Windows recusou o Raw Input");
                    return;
                }
                // Confere a janela da frente (jogo como administrador?).
                SetTimer(hwnd, CHECK_TIMER, 300, None);
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }
            })
            .expect("thread do atalho");
    }

    pub fn key_name(scan: u32, extended: bool) -> String {
        let lparam = ((scan & 0xFF) << 16) | if extended { 1 << 24 } else { 0 };
        let mut buf = [0u16; 64];
        let n = unsafe { GetKeyNameTextW(lparam as i32, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn install() {}
    pub fn key_name(_scan: u32, _extended: bool) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: (bool, bool, bool) = (false, false, false);
    const SHIFT: (bool, bool, bool) = (false, false, true);
    const VK_QUOTE_ABNT2: u32 = 0xC0;

    fn hook(combo: Combo) -> Hook {
        Hook {
            combo,
            ..Default::default()
        }
    }

    fn key(vk: u32, down: bool, mods: (bool, bool, bool)) -> Key {
        Key {
            vk,
            scan: 0,
            extended: false,
            down,
            mods,
            ours: true,
        }
    }

    fn pressed(e: Option<Event>) -> bool {
        matches!(e, Some(Event::Pressed))
    }

    #[test]
    fn fires_once_per_press_not_while_holding() {
        let mut h = hook(Combo::default());
        assert!(pressed(h.on_key(key(0x77, true, NONE))));
        assert!(!pressed(h.on_key(key(0x77, true, NONE)))); // repetição
        h.on_key(key(0x77, false, NONE)); // soltou
        assert!(pressed(h.on_key(key(0x77, true, NONE))));
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let mut h = hook(Combo {
            vk: VK_QUOTE_ABNT2,
            ..Combo::default()
        });
        // ' sozinha não dispara com Shift (Shift+' é aspas, outro atalho).
        assert!(!pressed(h.on_key(key(VK_QUOTE_ABNT2, true, SHIFT))));
        h.on_key(key(VK_QUOTE_ABNT2, false, NONE));
        assert!(pressed(h.on_key(key(VK_QUOTE_ABNT2, true, NONE))));
    }

    #[test]
    fn capture_takes_any_key_with_its_modifiers() {
        let mut h = hook(Combo::default());
        h.capturing = true;
        // Apertar só o Shift não conta: espera a tecla de verdade.
        assert!(h.on_key(key(0xA0, true, SHIFT)).is_none());
        match h.on_key(key(VK_QUOTE_ABNT2, true, SHIFT)) {
            Some(Event::Captured(Some(c))) => {
                assert_eq!(c.vk, VK_QUOTE_ABNT2);
                assert!(c.shift && !c.ctrl && !c.alt);
            }
            _ => panic!("devia capturar"),
        }
        // O mesmo aperto que escolheu não clipa.
        assert!(!pressed(h.on_key(key(VK_QUOTE_ABNT2, true, SHIFT))));
    }

    #[test]
    fn capture_ignores_keys_typed_in_other_programs() {
        // Escolhendo tecla, mas digitando "n" em outro programa: não pega.
        let mut h = hook(Combo::default());
        h.capturing = true;
        let elsewhere = Key {
            ours: false,
            ..key(0x4E, true, NONE)
        };
        assert!(h.on_key(elsewhere).is_none());
        assert!(h.capturing, "continua esperando a tecla na janela do RePic");
        assert!(matches!(
            h.on_key(key(VK_QUOTE_ABNT2, true, NONE)),
            Some(Event::Captured(Some(_)))
        ));
    }

    #[test]
    fn escape_cancels_capture_and_keeps_old_key() {
        let mut h = hook(Combo::default());
        h.capturing = true;
        assert!(matches!(
            h.on_key(key(VK_ESCAPE, true, NONE)),
            Some(Event::Captured(None))
        ));
        assert_eq!(h.combo, Combo::default());
    }

    #[test]
    fn key_up_lost_behind_admin_game_does_not_eat_next_press() {
        let mut h = hook(Combo::default());
        assert!(pressed(h.on_key(key(0x77, true, NONE))));
        // O soltar não chegou (UIPI): fica "segurando" até o timer conferir.
        assert!(!pressed(h.on_key(key(0x77, true, NONE))));
        *HOOK.lock().unwrap() = Some(h);
        release_if_up(|_| false);
        let mut h = HOOK.lock().unwrap().take().unwrap();
        assert!(pressed(h.on_key(key(0x77, true, NONE))));
    }

    #[test]
    fn labels() {
        assert_eq!(label(&Combo::default()), "F8");
        let c = Combo {
            vk: 0x7B,
            ctrl: true,
            shift: true,
            ..Combo::default()
        };
        assert_eq!(label(&c), "Ctrl+Shift+F12");
    }
}
