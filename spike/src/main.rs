//! Engine spike: one WebView2 window with adblock-rust blocking network
//! requests and hiding ad elements. Usage:
//!   engine-spike [URL] [--no-adblock]
//! Prints engine build time and, on exit, how many requests were blocked.

use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::mpsc,
    time::Instant,
};

use adblock::{
    Engine,
    lists::{FilterSet, ParseOptions},
    request::Request,
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IStream},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::*,
};

const LISTS: &[(&str, &str)] = &[
    ("easylist.txt", "https://easylist.to/easylist/easylist.txt"),
    ("easyprivacy.txt", "https://easylist.to/easylist/easyprivacy.txt"),
    (
        "ublock-filters.txt",
        "https://ublockorigin.github.io/uAssets/filters/filters.txt",
    ),
    (
        "ublock-privacy.txt",
        "https://ublockorigin.github.io/uAssets/filters/privacy.txt",
    ),
];

thread_local! {
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Stats {
    total: u64,
    blocked: u64,
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let adblock_on = !args.iter().any(|a| a == "--no-adblock");
    let url = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "https://www.youtube.com".into());

    let data_dir = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("BrowserSpike");
    let engine = adblock_on.then(|| Rc::new(build_engine(&data_dir.join("lists"))));

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE)?;
    }
    let hwnd = create_window()?;
    let started = Instant::now();

    let env = create_environment(&data_dir.join("profile"))?;
    let controller = create_controller(&env, hwnd)?;
    let webview = unsafe { controller.CoreWebView2()? };
    eprintln!("webview ready in {:?}", started.elapsed());
    unsafe { controller.SetIsVisible(true)? };
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller.clone()));
    resize(hwnd);

    let stats = Rc::new(RefCell::new(Stats::default()));
    if let Some(engine) = &engine {
        install_adblock(&env, &webview, engine.clone(), stats.clone(), hwnd)?;
    }

    unsafe {
        webview.Navigate(&HSTRING::from(url.as_str()))?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let s = stats.borrow();
    eprintln!("requests seen: {}, blocked: {}", s.total, s.blocked);
    Ok(())
}

// ---------- adblock ----------

fn build_engine(lists_dir: &Path) -> Engine {
    std::fs::create_dir_all(lists_dir).expect("create lists dir");
    let t = Instant::now();
    let mut set = FilterSet::new(false);
    for (file, url) in LISTS {
        let path = lists_dir.join(file);
        if !path.exists() {
            eprintln!("downloading {url}");
            let ok = Command::new("curl.exe")
                .args(["-sSfL", "--ssl-no-revoke", "-o"])
                .arg(&path)
                .arg(url)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                eprintln!("  failed, skipping {file}");
                let _ = std::fs::remove_file(&path);
                continue;
            }
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        set.add_filter_list(text, ParseOptions::default());
    }
    let engine = Engine::new_with_filter_set(set);
    eprintln!("adblock engine built in {:?}", t.elapsed());
    engine
}

fn request_type(ctx: COREWEBVIEW2_WEB_RESOURCE_CONTEXT, is_main_frame: bool) -> &'static str {
    match ctx {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT if is_main_frame => "document",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => "sub_frame",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST
        | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => "xmlhttprequest",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
        _ => "other",
    }
}

fn install_adblock(
    env: &ICoreWebView2Environment,
    webview: &ICoreWebView2,
    engine: Rc<Engine>,
    stats: Rc<RefCell<Stats>>,
    hwnd: HWND,
) -> windows::core::Result<()> {
    // URL of the top-level navigation in flight, so its own document request
    // is never mistaken for an ad iframe.
    let pending_nav = Rc::new(RefCell::new(String::new()));

    unsafe {
        // Prefer the API that also covers iframes and service workers.
        if let Ok(wv22) = webview.cast::<ICoreWebView2_22>() {
            wv22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            )?;
        } else {
            webview.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        }

        let nav = pending_nav.clone();
        let mut token = 0;
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    *nav.borrow_mut() = CoTaskMemPWSTR::from(uri).to_string();
                }
                Ok(())
            })),
            &mut token,
        )?;

        let env = env.clone();
        let eng = engine.clone();
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |sender, args| {
                let (Some(sender), Some(args)) = (sender, args) else {
                    return Ok(());
                };
                let request = args.Request()?;
                let mut uri = PWSTR::null();
                request.Uri(&mut uri)?;
                let uri = CoTaskMemPWSTR::from(uri).to_string();
                let mut method = PWSTR::null();
                request.Method(&mut method)?;
                let method = CoTaskMemPWSTR::from(method).to_string();
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
                args.ResourceContext(&mut ctx)?;

                let is_main = *pending_nav.borrow() == uri;
                if is_main && ctx == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT {
                    return Ok(()); // never block the page the user asked for
                }
                let mut source = PWSTR::null();
                sender.Source(&mut source)?;
                let source = CoTaskMemPWSTR::from(source).to_string();

                let mut s = stats.borrow_mut();
                s.total += 1;
                let Ok(req) = Request::new(&uri, &source, request_type(ctx, is_main), &method) else {
                    return Ok(());
                };
                if eng.check_network_request(&req).should_block() {
                    s.blocked += 1;
                    let resp = env.CreateWebResourceResponse(
                        None::<&IStream>,
                        403,
                        w!("Blocked"),
                        w!(""),
                    )?;
                    args.SetResponse(&resp)?;
                    if s.blocked % 10 == 1 {
                        let title = format!("Spike - {} blocked / {}", s.blocked, s.total);
                        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;

        // Cosmetic filtering: hide matched elements once the DOM exists.
        if let Ok(wv2) = webview.cast::<ICoreWebView2_2>() {
            wv2.add_DOMContentLoaded(
                &DOMContentLoadedEventHandler::create(Box::new(move |sender, _| {
                    let Some(sender) = sender else { return Ok(()) };
                    let mut source = PWSTR::null();
                    sender.Source(&mut source)?;
                    let source = CoTaskMemPWSTR::from(source).to_string();
                    let res = engine.url_cosmetic_resources(&source);
                    if res.hide_selectors.is_empty() {
                        return Ok(());
                    }
                    let js = hide_script(&res.hide_selectors);
                    sender.ExecuteScript(
                        &HSTRING::from(js),
                        &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))),
                    )?;
                    Ok(())
                })),
                &mut token,
            )?;
        }
    }
    Ok(())
}

/// One rule per selector: an invalid selector then drops only its own rule.
fn hide_script(selectors: &HashSet<String>) -> String {
    let css: String = selectors
        .iter()
        .map(|s| format!("{s}{{display:none!important}}\n"))
        .collect();
    let css = css.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${");
    format!(
        "(()=>{{const s=document.createElement('style');s.textContent=`{css}`;document.documentElement.appendChild(s);}})()"
    )
}

// ---------- WebView2 plumbing ----------

fn create_environment(profile: &Path) -> windows::core::Result<ICoreWebView2Environment> {
    let (tx, rx) = mpsc::channel();
    let profile = HSTRING::from(profile.as_os_str());
    let options: ICoreWebView2EnvironmentOptions = CoreWebView2EnvironmentOptions::default().into();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &profile, &options, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |err, env| {
            err?;
            tx.send(env.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send env");
            Ok(())
        }),
    )
    .map_err(to_win)?;
    rx.recv().expect("env channel")
}

fn create_controller(
    env: &ICoreWebView2Environment,
    hwnd: HWND,
) -> windows::core::Result<ICoreWebView2Controller> {
    let (tx, rx) = mpsc::channel();
    let env = env.clone();
    CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            env.CreateCoreWebView2Controller(hwnd, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |err, controller| {
            err?;
            tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send controller");
            Ok(())
        }),
    )
    .map_err(to_win)?;
    rx.recv().expect("controller channel")
}

fn to_win(e: webview2_com::Error) -> windows::core::Error {
    match e {
        webview2_com::Error::WindowsError(e) => e,
        other => windows::core::Error::new(E_POINTER, format!("{other:?}")),
    }
}

fn create_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("BrowserSpike"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BrowserSpike"),
            w!("Spike"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            800,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

fn resize(hwnd: HWND) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    CONTROLLER.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            unsafe {
                let _ = c.SetBounds(rect);
            }
        }
    });
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            resize(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            CONTROLLER.with(|c| {
                if let Some(c) = c.borrow_mut().take() {
                    unsafe {
                        let _ = c.Close();
                    }
                }
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}
