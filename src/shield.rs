//! Connects the adblock engine to one WebView: blocks requests, injects
//! scriptlets + site CSS at document creation, and hides generic ad slots
//! by matching the page's classes/ids (same approach as Brave).

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::Result;
use windows::{Win32::UI::Shell::SHCreateMemStream, core::*};

use crate::adblock::{Blocker, Verdict};

/// Per-tab adblock state, shared with the event handlers.
#[derive(Default)]
pub struct ShieldState {
    pub blocked: Cell<u32>,
    /// Off for this tab's current site (user allowed it).
    pub paused: Cell<bool>,
    pending_nav: RefCell<String>,
    exceptions: RefCell<HashSet<String>>,
    script_id: RefCell<Option<String>>,
    generation: Cell<u64>,
}

/// Random name for the page↔host channel, so pages can't guess it in advance.
pub(crate) fn token() -> &'static str {
    use std::sync::OnceLock;
    static T: OnceLock<String> = OnceLock::new();
    T.get_or_init(|| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("_{:x}", (n ^ (std::process::id() as u128) << 64) as u64)
    })
}

pub fn install(
    env: &ICoreWebView2Environment,
    webview: &ICoreWebView2,
    blocker: Rc<Blocker>,
    state: Rc<ShieldState>,
) -> Result<()> {
    unsafe {
        // Covers frames, workers and service workers on recent runtimes.
        match webview.cast::<ICoreWebView2_22>() {
            Ok(wv) => wv.AddWebResourceRequestedFilterWithRequestSourceKinds(
                w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            )?,
            Err(_) => webview.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?,
        }
        let mut token_out = 0;

        // Navigation: register this page's scriptlets + CSS before it loads.
        let (b, s) = (blocker.clone(), state.clone());
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(args)) = (sender, args) else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let uri = CoTaskMemPWSTR::from(uri).to_string();
                // Dangerous pages (badware list) never load: a warning
                // page says why. Other lists only stop what a page opens on
                // its own (popunder redirects to ad networks): a link the
                // user clicked, with its redirects, always opens.
                if !s.paused.get() {
                    let mut p = PWSTR::null();
                    wv.Source(&mut p)?;
                    let source = CoTaskMemPWSTR::from(p).to_string();
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    let danger = b.dangerous(&uri, &source);
                    if danger || (!user.as_bool() && b.navigation_blocked(&uri, &source)) {
                        args.SetCancel(true)?;
                        s.blocked.set(s.blocked.get() + 1);
                        eprintln!("adblock: navigation blocked {uri}");
                        if danger {
                            let page = format!(
                                "https://{}/error.html?e=danger&l={}&u={}",
                                crate::app::INTERNAL_HOST,
                                crate::i18n::code(),
                                crate::settings::url_encode(&uri)
                            );
                            wv.Navigate(&HSTRING::from(page))?;
                        }
                        return Ok(());
                    }
                }
                *s.pending_nav.borrow_mut() = uri.clone();
                register_page_script(&wv, &b, &s, &uri)
            })),
            &mut token_out,
        )?;

        // Popups and new tabs a page opens on its own: ad networks are
        // refused (the page gets `null` from window.open). What the user
        // opens goes through (dangerous pages: the navigation check above).
        let (b, s) = (blocker.clone(), state.clone());
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(args)) = (sender, args) else { return Ok(()) };
                if s.paused.get() {
                    return Ok(());
                }
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                let uri = CoTaskMemPWSTR::from(p).to_string();
                let mut user = BOOL(0);
                args.IsUserInitiated(&mut user)?;
                if user.as_bool() {
                    return Ok(());
                }
                let mut p = PWSTR::null();
                wv.Source(&mut p)?;
                let source = CoTaskMemPWSTR::from(p).to_string();
                if b.popup_blocked(&uri, &source) {
                    args.SetHandled(true)?;
                    s.blocked.set(s.blocked.get() + 1);
                    eprintln!("adblock: popup blocked {uri}");
                }
                Ok(())
            })),
            &mut token_out,
        )?;

        // Network blocking.
        let (b, s, env) = (blocker.clone(), state.clone(), env.clone());
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(args)) = (sender, args) else { return Ok(()) };
                if s.paused.get() {
                    return Ok(());
                }
                let started = std::time::Instant::now();
                let _timer = Defer(move || crate::sys::perf("request check", started, 5));
                let req = args.Request()?;
                let mut p = PWSTR::null();
                req.Uri(&mut p)?;
                let uri = CoTaskMemPWSTR::from(p).to_string();
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
                args.ResourceContext(&mut ctx)?;
                let is_doc = ctx == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT;
                if is_doc && *s.pending_nav.borrow() == uri {
                    return Ok(()); // the page the user asked for
                }
                let mut p = PWSTR::null();
                req.Method(&mut p)?;
                let method = CoTaskMemPWSTR::from(p).to_string();
                let mut p = PWSTR::null();
                wv.Source(&mut p)?;
                let source = CoTaskMemPWSTR::from(p).to_string();

                let response = match b.check(&uri, &source, request_type(ctx), &method) {
                    Verdict::Allow => return Ok(()),
                    Verdict::Block => env.CreateWebResourceResponse(None, 403, w!("Blocked"), w!(""))?,
                    Verdict::Redirect { mime, body } => {
                        let stream = SHCreateMemStream(Some(&body));
                        let headers = format!("Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *");
                        env.CreateWebResourceResponse(stream.as_ref(), 200, w!("OK"), &HSTRING::from(headers))?
                    }
                };
                args.SetResponse(&response)?;
                s.blocked.set(s.blocked.get() + 1);
                Ok(())
            })),
            &mut token_out,
        )?;

        // Generic hiding: page reports classes/ids, we answer with CSS.
        let (b, s) = (blocker, state);
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(args)) = (sender, args) else { return Ok(()) };
                let mut p = PWSTR::null();
                args.WebMessageAsJson(&mut p)?;
                let json = CoTaskMemPWSTR::from(p).to_string();
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(&json) else { return Ok(()) };
                if msg.get("k").and_then(|k| k.as_str()) != Some(token()) {
                    return Ok(());
                }
                let list = |key: &str| -> Vec<String> {
                    msg.get(key)
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().take(5000).filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                };
                let css = b.generic_css(&list("c"), &list("i"), &s.exceptions.borrow());
                if css.is_empty() {
                    return Ok(());
                }
                let js = format!("window.{t}&&window.{t}({css})", t = token(), css = js_string(&css));
                wv.ExecuteScript(&HSTRING::from(js), None)?;
                Ok(())
            })),
            &mut token_out,
        )?;
    }
    Ok(())
}

fn register_page_script(wv: &ICoreWebView2, blocker: &Blocker, s: &Rc<ShieldState>, uri: &str) -> Result<()> {
    let generation = s.generation.get() + 1;
    s.generation.set(generation);
    if let Some(old) = s.script_id.borrow_mut().take() {
        unsafe { wv.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(old))? };
    }
    let host = uri.split("://").nth(1).and_then(|r| r.split(['/', '?', '#', ':']).next()).unwrap_or("");
    if s.paused.get() || !uri.starts_with("http") || host.is_empty() {
        return Ok(());
    }
    let cos = blocker.page_cosmetics(uri);
    *s.exceptions.borrow_mut() = cos.exceptions;
    let js = PAGE_SCRIPT
        .replace("__HOST__", &js_string(host))
        .replace("__TOKEN__", &js_string(token()))
        .replace("__GENERIC__", if cos.generichide { "false" } else { "true" })
        .replace("__CSS__", &js_string(&cos.css))
        .replace("/*__SCRIPTLETS__*/", &cos.scriptlets);

    let s2 = s.clone();
    let wv2 = wv.clone();
    let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |err, id| {
        err?;
        if s2.generation.get() == generation {
            *s2.script_id.borrow_mut() = Some(id);
        } else {
            // A newer navigation started meanwhile: this script is stale.
            unsafe { wv2.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id))? };
        }
        Ok(())
    }));
    unsafe { wv.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(js), &handler) }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

fn request_type(ctx: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> &'static str {
    match ctx {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => "sub_frame",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => {
            "xmlhttprequest"
        }
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
        _ => "other",
    }
}

/// Runs in every frame before page scripts. Only acts on the host it was
/// registered for (the navigation it belongs to).
const PAGE_SCRIPT: &str = r#"(() => {
if (location.hostname !== __HOST__) return;
/*__SCRIPTLETS__*/
const sheet = new CSSStyleSheet();
const add = (css) => {
  for (const rule of css.split('\n')) {
    if (rule) try { sheet.insertRule(rule, sheet.cssRules.length); } catch (e) {}
  }
};
const attach = () => {
  if (!document.adoptedStyleSheets.includes(sheet))
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
};
add(__CSS__);
attach();
document.addEventListener('DOMContentLoaded', attach);
window.addEventListener('load', attach);
if (!__GENERIC__ || window.top !== window || !(window.chrome && chrome.webview)) return;
const T = __TOKEN__;
Object.defineProperty(window, T, { value: (css) => { add(css); attach(); } });
const seenC = new Set(), seenI = new Set();
let newC = [], newI = [], timer = 0;
const flush = () => {
  timer = 0;
  if (newC.length || newI.length) chrome.webview.postMessage({ k: T, c: newC, i: newI });
  newC = []; newI = [];
};
const scan = (el) => {
  if (el.id && !seenI.has(el.id)) { seenI.add(el.id); newI.push(el.id); }
  for (const c of el.classList) if (!seenC.has(c)) { seenC.add(c); newC.push(c); }
};
const scanTree = (root) => {
  if (root.nodeType !== 1) return;
  scan(root);
  for (const el of root.querySelectorAll('[id],[class]')) scan(el);
  if (!timer) timer = setTimeout(flush, 120);
};
const start = () => {
  scanTree(document.documentElement);
  new MutationObserver((muts) => {
    for (const m of muts) {
      if (m.type === 'attributes') { if (m.target.nodeType === 1) { scan(m.target); if (!timer) timer = setTimeout(flush, 120); } }
      else for (const n of m.addedNodes) scanTree(n);
    }
  }).observe(document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'id'] });
};
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start); else start();
})();"#;

/// Runs a closure when dropped (timing of early-return paths).
struct Defer<F: FnMut()>(F);
impl<F: FnMut()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}