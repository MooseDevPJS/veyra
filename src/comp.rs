//! DirectComposition scene for the whole window.
//!
//! Visual tree (bottom → top):
//!   frame   – solid background when glass is off (empty when glass is on,
//!             so the Acrylic backdrop shows through)
//!   page    – the active tab's WebView, rounded clip, blur when dimmed
//!   chrome  – sidebar, window buttons (D2D surface)
//!   overlay – command bar, tooltips (D2D surface)

use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, POINT, RECT},
        Graphics::{
            Direct2D::{Common::*, *},
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device},
            DirectComposition::*,
            Dxgi::{Common::*, IDXGIDevice},
        },
    },
    core::*,
};

pub struct Layer {
    pub visual: IDCompositionVisual2,
    surface: Option<IDCompositionSurface>,
    size: (u32, u32),
}

pub struct Comp {
    pub device: IDCompositionDesktopDevice,
    _target: IDCompositionTarget,
    pub frame: Layer,
    pub page: IDCompositionVisual2,
    page_clip: IDCompositionRectangleClip,
    page_blur: Option<IDCompositionGaussianBlurEffect>,
    pub chrome: Layer,
    /// Link preview card (Alt+click), between the sidebar and the overlay.
    pub peek: IDCompositionVisual2,
    peek_clip: IDCompositionRectangleClip,
    peek_shown: Option<IDCompositionVisual2>,
    pub overlay: Layer,
    /// Visual currently shown in `page` (the active tab's root visual).
    shown: Option<IDCompositionVisual2>,
    pub software: bool,
    /// Factory of the D2D device (icons and geometry must come from it).
    pub factory: ID2D1Factory,
}

impl Comp {
    /// The Direct3D device (about 200 ms: run it on another thread while the
    /// window starts). True when it is the software one.
    pub fn d3d_device() -> Result<(ID3D11Device, bool)> {
        unsafe {
            // Hardware first; WARP keeps the UI working on PCs without a usable GPU.
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            let mut d3d = None;
            let mut software = false;
            if D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None).is_err() {
                software = true;
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None)?;
            }
            Ok((d3d.ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?, software))
        }
    }

    pub fn new(hwnd: HWND, (d3d, software): (ID3D11Device, bool)) -> Result<Self> {
        unsafe {
            let dxgi: IDXGIDevice = d3d.cast()?;
            let d2d = D2D1CreateDevice(&dxgi, None)?;
            let factory = d2d.GetFactory()?;
            let device: IDCompositionDesktopDevice = DCompositionCreateDevice3(&d2d)?;
            // Not topmost: child windows (the docked DevTools) draw above.
            let target = device.CreateTargetForHwnd(hwnd, false)?;
            let root = device.CreateVisual()?;
            let layer = |d: &IDCompositionDesktopDevice| -> Result<Layer> {
                Ok(Layer { visual: d.CreateVisual()?, surface: None, size: (0, 0) })
            };
            let frame = layer(&device)?;
            let page = device.CreateVisual()?;
            let page_clip = device.CreateRectangleClip()?;
            page.SetClip(&page_clip)?;
            let chrome = layer(&device)?;
            let peek = device.CreateVisual()?;
            let peek_clip = device.CreateRectangleClip()?;
            peek.SetClip(&peek_clip)?;
            let overlay = layer(&device)?;
            root.AddVisual(&frame.visual, true, None)?;
            root.AddVisual(&page, true, &frame.visual)?;
            root.AddVisual(&chrome.visual, true, &page)?;
            root.AddVisual(&peek, true, &chrome.visual)?;
            root.AddVisual(&overlay.visual, true, &peek)?;
            target.SetRoot(&root)?;
            let page_blur = device.cast::<IDCompositionDevice3>().ok().and_then(|d| d.CreateGaussianBlurEffect().ok());
            Ok(Comp { device, _target: target, frame, page, page_clip, page_blur, chrome, peek, peek_clip, peek_shown: None, overlay, shown: None, software, factory })
        }
    }

    /// Creates the container visual a tab's WebView renders into.
    pub fn tab_visual(&self) -> Result<IDCompositionVisual2> {
        unsafe { self.device.CreateVisual() }
    }

    /// Shows `visual` (a tab's container) as the page, replacing the previous one.
    pub fn show_page(&mut self, visual: Option<&IDCompositionVisual2>) {
        unsafe {
            if let Some(old) = self.shown.take() {
                let _ = self.page.RemoveVisual(&old);
            }
            if let Some(v) = visual {
                let _ = self.page.AddVisual(v, true, None);
                self.shown = Some(v.clone());
            }
        }
    }

    /// Shows the preview's visual (or nothing).
    pub fn show_peek(&mut self, visual: Option<&IDCompositionVisual2>) {
        unsafe {
            if let Some(old) = self.peek_shown.take() {
                let _ = self.peek.RemoveVisual(&old);
            }
            if let Some(v) = visual {
                let _ = self.peek.AddVisual(v, true, None);
                self.peek_shown = Some(v.clone());
            }
            // A link preview is always fully visible (the history panel fades).
            if let Ok(v3) = self.peek.cast::<IDCompositionVisual3>() {
                let _ = v3.SetOpacity2(1.0);
            }
        }
    }

    /// Takes `visual` out of the preview layer if it is the one shown.
    pub fn hide_peek(&mut self, visual: &IDCompositionVisual2) {
        if self.peek_shown.as_ref() == Some(visual) {
            self.show_peek(None);
        }
    }

    /// Fades the preview layer (1 = opaque).
    pub fn set_peek_opacity(&self, opacity: f32) {
        if let Ok(v3) = self.peek.cast::<IDCompositionVisual3>() {
            unsafe {
                let _ = v3.SetOpacity2(opacity.clamp(0.0, 1.0));
            }
        }
    }

    pub fn set_peek_rect(&self, r: RECT, radius: f32) {
        set_clip(&self.peek, &self.peek_clip, r, radius);
    }

    /// Positions the page card (physical px) with rounded corners.
    pub fn set_page_rect(&self, r: RECT, radius: f32) {
        set_clip(&self.page, &self.page_clip, r, radius);
    }

    /// Blurs the page behind overlays (0 = off). Software rendering skips
    /// the blur (too slow on WARP); the dim layer alone still separates it.
    pub fn set_page_blur(&self, sigma: f32) {
        let Some(blur) = &self.page_blur else { return };
        let Ok(v3) = self.page.cast::<IDCompositionVisual3>() else { return };
        unsafe {
            if sigma < 0.5 || self.software {
                let _ = v3.SetEffect(None::<&IDCompositionEffect>);
            } else {
                let _ = blur.SetStandardDeviation2(sigma);
                let _ = blur.SetBorderMode(D2D1_BORDER_MODE_HARD);
                let _ = v3.SetEffect(blur);
            }
        }
    }

    pub fn commit(&self) {
        unsafe {
            let _ = self.device.Commit();
        }
    }
}

/// Moves `visual` to `r` (physical px) and clips it to a rounded rect.
fn set_clip(visual: &IDCompositionVisual2, c: &IDCompositionRectangleClip, r: RECT, radius: f32) {
    let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
    unsafe {
        let _ = visual.SetOffsetX2(r.left as f32);
        let _ = visual.SetOffsetY2(r.top as f32);
        let _ = c.SetLeft2(0.0);
        let _ = c.SetTop2(0.0);
        let _ = c.SetRight2(w);
        let _ = c.SetBottom2(h);
        let _ = c.SetTopLeftRadiusX2(radius);
        let _ = c.SetTopLeftRadiusY2(radius);
        let _ = c.SetTopRightRadiusX2(radius);
        let _ = c.SetTopRightRadiusY2(radius);
        let _ = c.SetBottomLeftRadiusX2(radius);
        let _ = c.SetBottomLeftRadiusY2(radius);
        let _ = c.SetBottomRightRadiusX2(radius);
        let _ = c.SetBottomRightRadiusY2(radius);
    }
}

impl Layer {
    /// Redraws the whole layer: `draw` gets a device context in DIPs.
    pub fn draw(&mut self, device: &IDCompositionDesktopDevice, w: u32, h: u32, scale: f32, draw: impl FnOnce(&ID2D1DeviceContext)) {
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            if self.surface.is_none() || self.size != (w, h) {
                match device.CreateSurface(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED) {
                    Ok(s) => {
                        let _ = self.visual.SetContent(&s);
                        self.surface = Some(s);
                        self.size = (w, h);
                    }
                    Err(e) => {
                        eprintln!("surface {w}x{h}: {e}");
                        return;
                    }
                }
            }
            let Some(surface) = &self.surface else { return };
            let mut off = POINT::default();
            let Ok(dc) = surface.BeginDraw::<ID2D1DeviceContext>(None, &mut off) else { return };
            dc.SetDpi(96.0 * scale, 96.0 * scale);
            dc.SetTransform(&windows_numerics::Matrix3x2::translation(off.x as f32 / scale, off.y as f32 / scale));
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.Clear(Some(&D2D1_COLOR_F::default()));
            draw(&dc);
            let _ = surface.EndDraw();
        }
    }
}
