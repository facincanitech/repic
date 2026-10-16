//! Atalho global via gancho de teclado do Windows (WH_KEYBOARD_LL).
//!
//! Diferente do RegisterHotKey, o gancho só *observa*: a tecla continua
//! chegando no jogo ou onde você estiver digitando. Por isso qualquer tecla
//! serve, sozinha ou com Ctrl/Alt/Shift. E como é a tecla física (código do
//! Windows), funciona igual em qualquer layout de teclado (ABNT2, US…).

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::mpsc::Sender;

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
}

const VK_ESCAPE: u32 = 0x1B;

fn is_modifier(vk: u32) -> bool {
    // Shift, Ctrl, Alt (genéricos e esquerdo/direito) e as teclas Windows.
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

#[derive(Default)]
struct Hook {
    combo: Combo,
    capturing: bool,
    /// A tecla do atalho está pressionada (ignora a repetição de segurar).
    held: bool,
    tx: Option<Sender<Event>>,
}

impl Hook {
    /// Decide o que fazer com uma tecla. `mods` = (ctrl, alt, shift) no momento.
    fn on_key(
        &mut self,
        vk: u32,
        scan: u32,
        extended: bool,
        down: bool,
        mods: (bool, bool, bool),
    ) -> Option<Event> {
        if is_modifier(vk) {
            return None;
        }
        if !down {
            if vk == self.combo.vk {
                self.held = false;
            }
            return None;
        }
        if self.capturing {
            self.capturing = false;
            if vk == VK_ESCAPE {
                return Some(Event::Captured(None));
            }
            let (ctrl, alt, shift) = mods;
            self.combo = Combo {
                vk,
                scan,
                extended,
                ctrl,
                alt,
                shift,
            };
            // Não clipa por causa do mesmo aperto que escolheu a tecla.
            self.held = true;
            return Some(Event::Captured(Some(self.combo)));
        }
        if vk != self.combo.vk {
            return None;
        }
        if self.held {
            return None; // segurando a tecla
        }
        self.held = true;
        let c = self.combo;
        (mods == (c.ctrl, c.alt, c.shift)).then_some(Event::Pressed)
    }
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

fn with_hook(f: impl FnOnce(&mut Hook)) {
    if let Some(h) = HOOK.lock().unwrap().as_mut() {
        f(h);
    }
}

/// A próxima tecla apertada (em qualquer lugar) vira o atalho novo.
pub fn begin_capture() {
    with_hook(|h| h.capturing = true);
}

pub fn cancel_capture() {
    with_hook(|h| h.capturing = false);
}

/// Liga o gancho de teclado. `handler` roda numa thread própria, nunca
/// dentro do gancho (o Windows desliga ganchos que demoram).
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

fn dispatch(vk: u32, scan: u32, extended: bool, down: bool, mods: (bool, bool, bool)) {
    // try_lock: o gancho nunca pode travar o teclado esperando.
    if let Ok(mut guard) = HOOK.try_lock()
        && let Some(h) = guard.as_mut()
        && let Some(ev) = h.on_key(vk, scan, extended, down, mods)
        && let Some(tx) = &h.tx
    {
        let _ = tx.send(ev);
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
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyNameTextW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG,
        SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
    };

    fn pressed(vk: i32) -> bool {
        unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
    }

    unsafe extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // Aqui tem que ser rápido: se o gancho demorar, o Windows tira ele
        // sem avisar. Nada de disco, log ou lock bloqueante.
        if code == HC_ACTION as i32 {
            let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let msg = wparam as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if down || up {
                // 0x11 Ctrl, 0x12 Alt, 0x10 Shift.
                let mods = (pressed(0x11), pressed(0x12), pressed(0x10));
                super::dispatch(k.vkCode, k.scanCode, k.flags & LLKHF_EXTENDED != 0, down, mods);
            }
        }
        // Sempre passa a tecla adiante: o jogo recebe normalmente.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    pub fn install() {
        std::thread::Builder::new()
            .name("hotkey-hook".into())
            .spawn(|| unsafe {
                // Jogo pesado deixa o PC engasgado; com prioridade alta o
                // gancho responde a tempo e o Windows não remove ele.
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                let module = GetModuleHandleW(std::ptr::null());
                let mut hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), module, 0);
                if hook.is_null() {
                    eprintln!("não consegui instalar o gancho de teclado");
                }
                // Mesmo assim, se o Windows tirar o gancho, ele não avisa.
                // Então reinstala de tempos em tempos.
                SetTimer(std::ptr::null_mut(), 0, 30_000, None);
                // O gancho precisa de uma fila de mensagens rodando nessa thread.
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_TIMER {
                        let fresh = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), module, 0);
                        if !fresh.is_null() {
                            if !hook.is_null() {
                                UnhookWindowsHookEx(hook);
                            }
                            hook = fresh;
                        }
                    }
                }
            })
            .expect("thread do gancho");
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

    fn pressed(e: Option<Event>) -> bool {
        matches!(e, Some(Event::Pressed))
    }

    #[test]
    fn fires_once_per_press_not_while_holding() {
        let mut h = hook(Combo::default());
        assert!(pressed(h.on_key(0x77, 0x42, false, true, NONE)));
        assert!(!pressed(h.on_key(0x77, 0x42, false, true, NONE))); // repetição
        h.on_key(0x77, 0x42, false, false, NONE); // soltou
        assert!(pressed(h.on_key(0x77, 0x42, false, true, NONE)));
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let mut h = hook(Combo {
            vk: VK_QUOTE_ABNT2,
            ..Combo::default()
        });
        // ' sozinha não dispara com Shift (Shift+' é aspas, outro atalho).
        assert!(!pressed(h.on_key(VK_QUOTE_ABNT2, 0x29, false, true, SHIFT)));
        h.on_key(VK_QUOTE_ABNT2, 0x29, false, false, NONE);
        assert!(pressed(h.on_key(VK_QUOTE_ABNT2, 0x29, false, true, NONE)));
    }

    #[test]
    fn capture_takes_any_key_with_its_modifiers() {
        let mut h = hook(Combo::default());
        h.capturing = true;
        // Apertar só o Shift não conta: espera a tecla de verdade.
        assert!(h.on_key(0xA0, 0x2A, false, true, SHIFT).is_none());
        match h.on_key(VK_QUOTE_ABNT2, 0x29, false, true, SHIFT) {
            Some(Event::Captured(Some(c))) => {
                assert_eq!(c.vk, VK_QUOTE_ABNT2);
                assert!(c.shift && !c.ctrl && !c.alt);
            }
            _ => panic!("devia capturar"),
        }
        // O mesmo aperto que escolheu não clipa.
        assert!(!pressed(h.on_key(VK_QUOTE_ABNT2, 0x29, false, true, SHIFT)));
    }

    #[test]
    fn escape_cancels_capture_and_keeps_old_key() {
        let mut h = hook(Combo::default());
        h.capturing = true;
        assert!(matches!(
            h.on_key(VK_ESCAPE, 0x01, false, true, NONE),
            Some(Event::Captured(None))
        ));
        assert_eq!(h.combo, Combo::default());
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
