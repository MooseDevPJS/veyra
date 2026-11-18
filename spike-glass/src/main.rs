use std::{cell::RefCell, sync::mpsc};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{Interface, Result};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::{Common::*, *},
            Direct3D::*,
            Direct3D11::*,
            DirectComposition::*,
            Dwm::*,
            Dxgi::{Common::*, IDXGIDevice},
        },
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::{Controls::MARGINS, HiDpi::*, WindowsAndMessaging::*},
    },
    core::*,
};

const SIDEBAR: f32 = 260.0;
const INSET: f32 = 8.0;
const RADIUS: f32 = 14.0;

struct State {
    dcomp: IDCompositionDesktopDevice,
    sidebar: IDCompositionVisual2,
    page: IDCompositionVisual2,
    clip: IDCompositionRectangleClip,
    controller: Option<ICoreWebView2CompositionController>,
    ctrl: Option<ICoreWebView2Controller>,
    cursor: HCURSOR,
    scale: f32,
}

thread_local! { static S: RefCell<Option<State>> = const { RefCell::new(None) }; }

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance.into(),
            lpszClassName: w!("Glass"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("Glass"),
            w!("Glass"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            1280,
            800,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // Glass backdrop over the whole window.
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        DwmExtendFrameIntoClientArea(hwnd, &m)?;
        let dark = BOOL(1);
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const BOOL).cast(), 4)?;
        let backdrop = DWMSBT_TRANSIENTWINDOW;
        DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast(), 4)?;
        // Re-run NCCALCSIZE so the frame removal takes effect.
        SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER)?;

        // Devices: D3D11 (WARP fallback) -> D2D -> DComp.
        let mut d3d = None;
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        if D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None).is_err() {
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None)?;
        }
        let dxgi: IDXGIDevice = d3d.unwrap().cast()?;
        let d2d = D2D1CreateDevice(&dxgi, None)?;
        let dcomp: IDCompositionDesktopDevice = DCompositionCreateDevice3(&d2d)?;
        let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
        let root = dcomp.CreateVisual()?;
        let sidebar = dcomp.CreateVisual()?;
        let page = dcomp.CreateVisual()?;
        let clip = dcomp.CreateRectangleClip()?;
        page.SetClip(&clip)?;
        root.AddVisual(&page, false, None)?;
        root.AddVisual(&sidebar, false, None)?;
        target.SetRoot(&root)?;
        std::mem::forget(target);
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        S.with(|s| *s.borrow_mut() = Some(State { dcomp: dcomp.clone(), sidebar, page, clip, controller: None, ctrl: None, cursor, scale }));
        layout(hwnd);

        // WebView2 in visual hosting mode.
        let (tx, rx) = mpsc::channel();
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(|h| CreateCoreWebView2Environment(&h).map_err(webview2_com::Error::WindowsError)),
            Box::new(move |e, env| {
                e?;
                tx.send(env.unwrap()).unwrap();
                Ok(())
            }),
        )
        .unwrap();
        let env: ICoreWebView2Environment3 = rx.recv().unwrap().cast()?;
        let (tx, rx) = mpsc::channel();
        CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |h| env.CreateCoreWebView2CompositionController(hwnd, &h).map_err(webview2_com::Error::WindowsError)),
            Box::new(move |e, c| {
                e?;
                tx.send(c.unwrap()).unwrap();
                Ok(())
            }),
        )
        .unwrap();
        let cc: ICoreWebView2CompositionController = rx.recv().unwrap();
        let page_visual = S.with(|s| s.borrow().as_ref().unwrap().page.clone());
        cc.SetRootVisualTarget(&page_visual)?;
        let ctrl: ICoreWebView2Controller = cc.cast()?;
        ctrl.SetIsVisible(true)?;
        let mut tok = 0;
        cc.add_CursorChanged(
            &CursorChangedEventHandler::create(Box::new(|sender, _| {
                if let Some(c) = sender {
                    let mut h = HCURSOR::default();
                    c.Cursor(&mut h)?;
                    S.with(|s| s.borrow_mut().as_mut().unwrap().cursor = h);
                }
                Ok(())
            })),
            &mut tok,
        )?;
        S.with(|s| {
            let mut s = s.borrow_mut();
            let s = s.as_mut().unwrap();
            s.controller = Some(cc.clone());
            s.ctrl = Some(ctrl.clone());
        });
        layout(hwnd);
        ctrl.CoreWebView2()?.Navigate(w!("https://www.youtube.com"))?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn page_rect(hwnd: HWND, scale: f32) -> RECT {
    let mut r = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut r); }
    RECT {
        left: (SIDEBAR * scale) as i32,
        top: (INSET * scale) as i32,
        right: r.right - (INSET * scale) as i32,
        bottom: r.bottom - (INSET * scale) as i32,
    }
}

fn layout(hwnd: HWND) {
    S.with(|s| {
        let s = s.borrow();
        let Some(s) = s.as_ref() else { return };
        let pr = page_rect(hwnd, s.scale);
        unsafe {
            let _ = s.page.SetOffsetX2(pr.left as f32);
            let _ = s.page.SetOffsetY2(pr.top as f32);
            let (w, h) = ((pr.right - pr.left) as f32, (pr.bottom - pr.top) as f32);
            let _ = s.clip.SetLeft2(0.0);
            let _ = s.clip.SetTop2(0.0);
            let _ = s.clip.SetRight2(w);
            let _ = s.clip.SetBottom2(h);
            let r = RADIUS * s.scale;
            for f in [
                IDCompositionRectangleClip::SetTopLeftRadiusX2, IDCompositionRectangleClip::SetTopLeftRadiusY2,
                IDCompositionRectangleClip::SetTopRightRadiusX2, IDCompositionRectangleClip::SetTopRightRadiusY2,
                IDCompositionRectangleClip::SetBottomLeftRadiusX2, IDCompositionRectangleClip::SetBottomLeftRadiusY2,
                IDCompositionRectangleClip::SetBottomRightRadiusX2, IDCompositionRectangleClip::SetBottomRightRadiusY2,
            ] {
                let _ = f(&s.clip, r);
            }
            if let Some(c) = &s.ctrl {
                let _ = c.SetBounds(RECT { left: 0, top: 0, right: pr.right - pr.left, bottom: pr.bottom - pr.top });
            }
            draw_sidebar(s, hwnd);
            let _ = s.dcomp.Commit();
        }
    });
}

fn draw_sidebar(s: &State, hwnd: HWND) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
        let w = (SIDEBAR * s.scale) as u32;
        let h = r.bottom.max(1) as u32;
        let Ok(surface) = s.dcomp.CreateSurface(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED) else { return };
        let mut off = POINT::default();
        let Ok(dc) = surface.BeginDraw::<ID2D1DeviceContext>(None, &mut off) else { return };
        dc.SetDpi(96.0 * s.scale, 96.0 * s.scale);
        dc.SetTransform(&windows_numerics::Matrix3x2::translation(off.x as f32 / s.scale, off.y as f32 / s.scale));
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.10 }, None).unwrap();
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 10.0, top: 120.0, right: SIDEBAR - 10.0, bottom: 156.0 }, radiusX: 10.0, radiusY: 10.0 }, &b);
        let _ = surface.EndDraw();
        let _ = s.sidebar.SetContent(&surface);
    }
}

fn forward_mouse(kind: COREWEBVIEW2_MOUSE_EVENT_KIND, w: WPARAM, l: LPARAM, hwnd: HWND, screen_coords: bool) -> bool {
    S.with(|s| {
        let s = s.borrow();
        let Some(s) = s.as_ref() else { return false };
        let Some(cc) = &s.controller else { return false };
        let mut pt = POINT { x: (l.0 & 0xffff) as i16 as i32, y: ((l.0 >> 16) & 0xffff) as i16 as i32 };
        if screen_coords {
            unsafe { let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt); }
        }
        let pr = page_rect(hwnd, s.scale);
        let inside = pt.x >= pr.left && pt.x < pr.right && pt.y >= pr.top && pt.y < pr.bottom;
        if !inside && kind != COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE {
            return false;
        }
        let data = if kind == COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL { ((w.0 >> 16) & 0xffff) as i16 as i32 as u32 } else { 0 };
        let rel = POINT { x: pt.x - pr.left, y: pt.y - pr.top };
        unsafe {
            let _ = cc.SendMouseInput(kind, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS((w.0 & 0xffff) as i32), data, rel);
        }
        true
    })
}

extern "system" fn proc_(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCALCSIZE if w.0 != 0 => {
                // Whole window is client area; when maximized keep it on screen.
                if IsZoomed(hwnd).as_bool() {
                    let p = &mut *(l.0 as *mut NCCALCSIZE_PARAMS);
                    let f = GetSystemMetricsForDpi(SM_CXFRAME, GetDpiForWindow(hwnd)) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, GetDpiForWindow(hwnd));
                    p.rgrc[0].left += f;
                    p.rgrc[0].top += f;
                    p.rgrc[0].right -= f;
                    p.rgrc[0].bottom -= f;
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let mut pt = POINT { x: (l.0 & 0xffff) as i16 as i32, y: ((l.0 >> 16) & 0xffff) as i16 as i32 };
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                let b = 6;
                let (l_, t, rr, bb) = (pt.x < b, pt.y < b, pt.x >= r.right - b, pt.y >= r.bottom - b);
                let hit = match (l_, t, rr, bb) {
                    (true, true, _, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, _, true, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTTOP,
                    (_, _, true, _) => HTRIGHT,
                    (.., true) => HTBOTTOM,
                    _ if pt.y < 44 && pt.x < 260 => HTCAPTION,
                    _ => HTCLIENT,
                };
                LRESULT(hit as isize)
            }
            WM_SETCURSOR if (l.0 & 0xffff) as u32 == HTCLIENT => {
                let c = S.with(|s| s.borrow().as_ref().map(|s| s.cursor));
                if let Some(c) = c { SetCursor(Some(c)); return LRESULT(1); }
                DefWindowProcW(hwnd, msg, w, l)
            }
            WM_MOUSEMOVE => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, w, l, hwnd, false); LRESULT(0) }
            WM_LBUTTONDOWN => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN, w, l, hwnd, false); LRESULT(0) }
            WM_LBUTTONUP => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP, w, l, hwnd, false); LRESULT(0) }
            WM_RBUTTONDOWN => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN, w, l, hwnd, false); LRESULT(0) }
            WM_RBUTTONUP => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP, w, l, hwnd, false); LRESULT(0) }
            WM_MOUSEWHEEL => { forward_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL, w, l, hwnd, true); LRESULT(0) }
            WM_SIZE => { layout(hwnd); LRESULT(0) }
            WM_DESTROY => { PostQuitMessage(0); LRESULT(0) }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
