//! Site permission requests (location, camera, notifications...) answered
//! with our own card over the page instead of WebView2's default bubble.
//! "Allow" and "Block" are remembered for the site; the X only dismisses.

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use crate::{
    app::{App, TabId},
    gfx::{contains, rect},
    i18n::t,
};

pub struct Prompt {
    pub tab: TabId,
    pub host: String,
    pub what: &'static str,
    args: ICoreWebView2PermissionRequestedEventArgs,
    deferral: ICoreWebView2Deferral,
    pub hover: Option<Choice>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Choice {
    Block,
    Allow,
    Dismiss,
}

/// What the site asks for, as a sentence ending ("<site> wants to ...").
fn describe(kind: COREWEBVIEW2_PERMISSION_KIND) -> &'static str {
    match kind {
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => t("usare il microfono", "use your microphone"),
        COREWEBVIEW2_PERMISSION_KIND_CAMERA => t("usare la fotocamera", "use your camera"),
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION => t("conoscere la tua posizione", "know your location"),
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS => t("mostrarti notifiche", "show notifications"),
        COREWEBVIEW2_PERMISSION_KIND_OTHER_SENSORS => t("usare i sensori del dispositivo", "use your device's sensors"),
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => t("leggere gli appunti", "read your clipboard"),
        COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS => t("scaricare più file", "download multiple files"),
        COREWEBVIEW2_PERMISSION_KIND_FILE_READ_WRITE => t("modificare i file sul PC", "edit files on your PC"),
        COREWEBVIEW2_PERMISSION_KIND_LOCAL_FONTS => t("usare i font del PC", "use your PC's fonts"),
        COREWEBVIEW2_PERMISSION_KIND_MIDI_SYSTEM_EXCLUSIVE_MESSAGES => t("controllare i dispositivi MIDI", "control MIDI devices"),
        COREWEBVIEW2_PERMISSION_KIND_WINDOW_MANAGEMENT => t("gestire le finestre sugli schermi", "manage windows on your screens"),
        _ => t("usare una funzione del dispositivo", "use a device feature"),
    }
}

const CARD_W: f32 = 340.0;
const CARD_H: f32 = 116.0;

impl App {
    pub fn permission_requested(&mut self, tab: TabId, args: ICoreWebView2PermissionRequestedEventArgs, deferral: ICoreWebView2Deferral) {
        let (mut uri, mut kind) = (windows::core::PWSTR::null(), COREWEBVIEW2_PERMISSION_KIND_UNKNOWN_PERMISSION);
        unsafe {
            let _ = args.Uri(&mut uri);
            let _ = args.PermissionKind(&mut kind);
        }
        let url = webview2_com::CoTaskMemPWSTR::from(uri).to_string();
        let host = crate::app::host_of(&url);
        let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
        self.prompts.push(Prompt { tab, host, what: describe(kind), args, deferral, hover: None });
        self.redraw();
    }

    /// The prompt shown now: the oldest one of the active tab.
    pub fn current_prompt(&self) -> Option<&Prompt> {
        self.prompts.iter().find(|p| Some(p.tab) == self.active)
    }

    /// Card in DIPs, at the page's top-left corner (next to the address).
    pub fn prompt_rect(&self) -> Option<D2D_RECT_F> {
        self.current_prompt()?;
        let p = self.page_rect();
        let (l, tp) = (p.left as f32 / self.scale + 12.0, p.top as f32 / self.scale + 12.0);
        Some(rect(l, tp, l + CARD_W, tp + CARD_H))
    }

    /// (block, allow, dismiss) buttons inside the card.
    pub fn prompt_buttons(card: D2D_RECT_F) -> [(D2D_RECT_F, Choice); 3] {
        let y = card.bottom - 46.0;
        let allow_w = crate::ui::text_pill_width(t("Consenti", "Allow")) + 8.0;
        let block_w = crate::ui::text_pill_width(t("Blocca", "Block")) + 8.0;
        let allow = rect(card.right - 16.0 - allow_w, y, card.right - 16.0, y + 32.0);
        let block = rect(allow.left - 8.0 - block_w, y, allow.left - 8.0, y + 32.0);
        let close = rect(card.right - 40.0, card.top + 12.0, card.right - 12.0, card.top + 40.0);
        [(block, Choice::Block), (allow, Choice::Allow), (close, Choice::Dismiss)]
    }

    /// Mouse over the card. Returns true when the event is the card's.
    pub fn prompt_mouse(&mut self, kind: crate::shell::Mouse, x: i32, y: i32) -> bool {
        use crate::shell::Mouse;
        let Some(card) = self.prompt_rect() else { return false };
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        let inside = contains(&card, xd, yd);
        let hover = if inside { Self::prompt_buttons(card).into_iter().find(|(r, _)| contains(r, xd, yd)).map(|(_, c)| c) } else { None };
        if let Some(p) = self.prompts.iter_mut().find(|p| Some(p.tab) == self.active)
            && p.hover != hover
        {
            p.hover = hover;
            self.redraw();
        }
        if !inside {
            return false;
        }
        if kind == Mouse::LeftUp
            && let Some(c) = hover
        {
            self.answer_prompt(c);
        }
        true
    }

    pub fn answer_prompt(&mut self, choice: Choice) {
        let Some(i) = self.prompts.iter().position(|p| Some(p.tab) == self.active) else { return };
        let p = self.prompts.remove(i);
        finish(&p, choice);
        self.redraw();
    }

    /// A closed tab's requests are refused without remembering it.
    pub fn drop_prompts(&mut self, tab: TabId) {
        for p in self.prompts.extract_if(.., |p| p.tab == tab) {
            finish(&p, Choice::Dismiss);
        }
    }
}

fn finish(p: &Prompt, choice: Choice) {
    let state = match choice {
        Choice::Allow => COREWEBVIEW2_PERMISSION_STATE_ALLOW,
        Choice::Block => COREWEBVIEW2_PERMISSION_STATE_DENY,
        // Not "default": that would bring up WebView2's own bubble.
        Choice::Dismiss => COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    unsafe {
        if let Ok(a3) = windows::core::Interface::cast::<ICoreWebView2PermissionRequestedEventArgs3>(&p.args) {
            let _ = a3.SetSavesInProfile(choice != Choice::Dismiss);
        }
        let _ = p.args.SetState(state);
        let _ = p.deferral.Complete();
    }
}
