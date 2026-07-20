//! Окно оверлея: поверх всего, сквозное для мыши, никогда не забирающее фокус.

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Класс окна. Регистрируется один раз за процесс.
const CLASS_NAME: PCWSTR = w!("BladestatsOverlay");

pub struct OverlayWindow {
    pub hwnd: HWND,
}

impl OverlayWindow {
    /// Создаёт окно оверлея заданного размера в точке `(x, y)` экранных координат.
    ///
    /// Набор расширенных стилей здесь — не украшение, каждый из них обязателен:
    ///
    /// - `WS_EX_NOREDIRECTIONBITMAP` — у окна нет буфера перенаправления, картинку целиком
    ///   отдаёт DirectComposition. Именно это даёт настоящую попиксельную альфу. Заметим, что
    ///   `WS_EX_LAYERED` тут не только не нужен, но и вреден: он включает старый путь через
    ///   `UpdateLayeredWindow`, несовместимый с композицией.
    /// - `WS_EX_TRANSPARENT` — окно прозрачно для попадания мыши, клики уходят в игру.
    /// - `WS_EX_NOACTIVATE` — окно не получает фокус, поэтому не выдёргивает игру из
    ///   полноэкранного режима и не мешает вводу.
    /// - `WS_EX_TOOLWINDOW` — нет кнопки в панели задач и в Alt-Tab.
    /// - `WS_EX_TOPMOST` — поверх остальных окон; удерживается не только этим флагом,
    ///   см. [`OverlayWindow::reassert_topmost`].
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Result<Self> {
        unsafe {
            // Ставим осознание DPI из кода, а не манифестом: манифест понадобится позже
            // ради прав администратора для ETW, и до тех пор его лучше не заводить.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

            let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;

            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                hbrBackground: HBRUSH::default(),
                ..Default::default()
            };
            // Повторная регистрация того же класса безобидна и возвращает 0 — не проверяем.
            RegisterClassExW(&class);

            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                CLASS_NAME,
                w!("bladestats"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("CreateWindowExW")?;

            Ok(Self { hwnd })
        }
    }

    /// Возвращает окно на самый верх.
    ///
    /// Одного `WS_EX_TOPMOST` при создании не хватает: игра, переходя в полноэкранный режим
    /// или просто активируясь, перебивает порядок окон и оверлей уезжает под неё. Поэтому
    /// порядок подтверждается заново — по таймеру и при смене активного окна.
    ///
    /// `SWP_NOACTIVATE` обязателен: без него мы бы отбирали фокус у игры каждую секунду.
    pub fn reassert_topmost(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Понадобится на этапе слежения за целью: оверлей переезжает вслед за окном игры
    /// на другой монитор.
    #[allow(dead_code)]
    pub fn set_position(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    pub fn resize(&self, width: i32, height: i32) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    /// Понадобится, когда появится определение exclusive fullscreen: в нём оверлей
    /// прячется целиком.
    #[allow(dead_code)]
    pub fn show(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Подстраховка к WS_EX_TRANSPARENT: сообщаем системе, что попадания в это окно
            // нет вовсе, и курсор с кликами достаются тому, кто под нами.
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
