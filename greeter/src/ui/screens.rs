//! The lock screen (clock and date) and the sign-in screen (user tile, status, password,
//! sign-in options), laid out after Windows 11 until docs/windows-hello-ui.md says otherwise.

use adw::prelude::*;
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::glib;

use super::hello_icon::HelloIcon;
use crate::assets::Assets;
use crate::state::{Face, State};
use crate::users::Account;

/// Windows' texts (FaceCredentialProvider.dll.mui ids, credprovhost 109, winlogon;
/// docs/windows-hello-ui.md §1.4, §2).
pub const GETTING_READY: &str = "Getting ready...";
pub const LOOKING_FOR_YOU: &str = "Looking for you...";
pub const MAKING_SURE: &str = "Making sure it's you...";
pub const WELCOME: &str = "Welcome";
/// 113: on the lock screen, when a key press is still needed (`dismiss_lock_screen` off).
pub const DISMISS_TO_SIGN_IN: &str = "Hello, {}! Dismiss the lock screen to sign in.";
/// 157 and 124: camera errors hide the eye (state 7).
pub const CAMERA_IN_USE: &str = "Camera is in use. Please sign in with your password.";
pub const CAMERA_NOT_AVAILABLE: &str = "Camera not available. Please sign in with your password.";

/// The label under the eye for a failed check: the module's reason, Windows' camera texts for
/// the module's "not available" lines, nothing when face sign-in was not there at all. `false`
/// hides the eye (state 7).
fn failure_text(reason: Option<&str>) -> (String, bool) {
    match reason {
        None => (String::new(), false),
        Some("Face login not available: another program is using the camera") => (CAMERA_IN_USE.into(), false),
        Some("Face login not available: the lid is closed") => (CAMERA_NOT_AVAILABLE.into(), false),
        Some(reason) => (reason.to_string(), true),
    }
}

pub const CSS: &str = r#"
* { font-family: "Segoe UI Variable", "Segoe UI", sans-serif; }
window { background: transparent; }
label { color: white; }
.clock { font-size: 96px; font-weight: 300; font-variation-settings: "opsz" 36; }
.date { font-size: 28px; font-weight: 400; }
.greeting { font-size: 34px; font-weight: 600; }
.user-name { font-size: 28px; font-weight: 350; margin-top: 12px; }
.status { font-size: 15px; }
.notice { font-size: 13px; color: rgba(255,255,255,0.8); }
.password-error { font-size: 14px; }
.caption { font-size: 11px; color: rgba(255,255,255,0.6); }
.sign-in-options { font-size: 14px; color: white; }
.sign-in-options:hover { text-decoration: underline; }
entry.password { min-width: 260px; min-height: 30px; border-radius: 4px; background: rgba(255,255,255,0.9); color: black; caret-color: black; }
button.submit { min-width: 30px; min-height: 30px; padding: 0; border-radius: 4px; background: rgba(255,255,255,0.9); color: black; }
button.option { background: rgba(255,255,255,0.15); border-radius: 4px; min-width: 44px; min-height: 44px; }
button.option:hover { background: rgba(255,255,255,0.3); }
"#;

pub struct LockScreen {
    pub widget: gtk::Box,
    clock: gtk::Label,
    date: gtk::Label,
    welcome: gtk::Label,
}

impl LockScreen {
    pub fn new(display_name: &str) -> LockScreen {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_halign(gtk::Align::Start);
        widget.set_valign(gtk::Align::End);
        widget.set_margin_start(48);
        widget.set_margin_bottom(48);
        let clock = gtk::Label::new(None);
        clock.add_css_class("clock");
        clock.set_xalign(0.0);
        let date = gtk::Label::new(None);
        date.add_css_class("date");
        date.set_xalign(0.0);
        let welcome = gtk::Label::new(Some(&DISMISS_TO_SIGN_IN.replace("{}", display_name)));
        welcome.add_css_class("greeting");
        welcome.set_xalign(0.0);
        welcome.set_margin_top(24);
        welcome.set_visible(false);
        widget.append(&clock);
        widget.append(&date);
        widget.append(&welcome);
        let screen = LockScreen { widget, clock, date, welcome };
        screen.tick();
        let (clock, date) = (screen.clock.clone(), screen.date.clone());
        glib::timeout_add_seconds_local(1, move || {
            set_time(&clock, &date);
            glib::ControlFlow::Continue
        });
        screen
    }

    fn tick(&self) {
        set_time(&self.clock, &self.date);
    }

    /// `dismiss_lock_screen` off: the face was recognised, a key press signs in.
    pub fn show_welcome(&self, welcome: bool) {
        self.welcome.set_visible(welcome);
    }
}

/// The user tile: the user's picture (else Windows' default one from the assets) in a circle,
/// else the initials on a grey disc.
fn avatar(account: &Account, assets: &Assets) -> gtk::DrawingArea {
    const SIZE: i32 = 192;
    let area = gtk::DrawingArea::new();
    area.set_size_request(SIZE, SIZE);
    area.set_halign(gtk::Align::Center);
    let picture = account.picture().or_else(|| assets.user_picture.clone()).and_then(|path| {
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, SIZE, SIZE, false)
            .inspect_err(|e| tracing::warn!("user picture {}: {e}", path.display()))
            .ok()
    });
    let initials = account.initials();
    area.set_draw_func(move |area, cr, width, height| {
        let radius = width.min(height) as f64 / 2.0;
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, radius, 0.0, std::f64::consts::TAU);
        cr.clip();
        match &picture {
            Some(pixbuf) => {
                cr.set_source_pixbuf(
                    pixbuf,
                    (width - pixbuf.width()) as f64 / 2.0,
                    (height - pixbuf.height()) as f64 / 2.0,
                );
                let _ = cr.paint();
            }
            None => {
                cr.set_source_rgb(0.55, 0.55, 0.55);
                let _ = cr.paint();
                let layout = area.create_pango_layout(Some(&initials));
                let mut font = gtk::pango::FontDescription::new();
                font.set_family("Segoe UI Variable, Segoe UI, sans-serif");
                font.set_absolute_size(f64::from(SIZE) * 0.42 * f64::from(gtk::pango::SCALE));
                layout.set_font_description(Some(&font));
                let (text_width, text_height) = layout.pixel_size();
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.move_to((width - text_width) as f64 / 2.0, (height - text_height) as f64 / 2.0);
                pangocairo::functions::show_layout(cr, &layout);
            }
        }
    });
    area
}

fn set_time(clock: &gtk::Label, date: &gtk::Label) {
    if let Ok(now) = glib::DateTime::now_local() {
        // 12-hour time without a leading zero and the long date, as the Windows (en-US) lock screen.
        if let Ok(text) = now.format("%l:%M") {
            clock.set_text(text.trim());
        }
        if let Ok(text) = now.format("%A, %B %e") {
            date.set_text(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
}

pub struct SignIn {
    pub widget: gtk::Box,
    pub password: gtk::PasswordEntry,
    pub submit: gtk::Button,
    pub face_option: gtk::Button,
    pub password_option: gtk::Button,
    hello: HelloIcon,
    status: gtk::Label,
    status_row: gtk::Box,
    notices: gtk::Label,
    password_error: gtk::Label,
    options: gtk::Revealer,
    display_name: String,
}

impl SignIn {
    pub fn new(account: &Account, assets: &Assets) -> SignIn {
        let display_name = account.display_name.clone();
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.set_halign(gtk::Align::Center);
        widget.set_valign(gtk::Align::Center);

        widget.append(&avatar(account, assets));

        let name = gtk::Label::new(Some(&account.display_name));
        name.add_css_class("user-name");
        widget.append(&name);

        let hello = HelloIcon::new(&assets.hello);
        let status = gtk::Label::new(None);
        status.add_css_class("status");
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        status_row.set_halign(gtk::Align::Center);
        status_row.set_margin_top(8);
        status_row.append(&hello.widget);
        status_row.append(&status);
        widget.append(&status_row);

        let notices = gtk::Label::new(None);
        notices.add_css_class("notice");
        notices.set_justify(gtk::Justification::Center);
        notices.set_visible(false);
        widget.append(&notices);

        let password = gtk::PasswordEntry::new();
        password.add_css_class("password");
        password.set_placeholder_text(Some("Password"));
        password.set_show_peek_icon(true);
        let submit = gtk::Button::from_icon_name("go-next-symbolic");
        submit.add_css_class("submit");
        let entry_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        entry_row.set_halign(gtk::Align::Center);
        entry_row.set_margin_top(12);
        entry_row.append(&password);
        entry_row.append(&submit);
        widget.append(&entry_row);

        let password_error = gtk::Label::new(None);
        password_error.add_css_class("password-error");
        password_error.set_visible(false);
        widget.append(&password_error);

        let options_link = gtk::Button::with_label("Sign-in options");
        options_link.add_css_class("flat");
        options_link.add_css_class("sign-in-options");
        options_link.set_halign(gtk::Align::Center);
        options_link.set_margin_top(16);
        widget.append(&options_link);

        let face_option = gtk::Button::new();
        face_option.add_css_class("option");
        face_option.set_child(Some(&HelloIcon::new(&assets.hello).widget));
        face_option.set_tooltip_text(Some("Windows Hello Face"));
        let password_option = gtk::Button::from_icon_name("dialog-password-symbolic");
        password_option.add_css_class("option");
        password_option.set_tooltip_text(Some("Password"));
        let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        options_row.set_halign(gtk::Align::Center);
        options_row.append(&face_option);
        options_row.append(&password_option);
        let options = gtk::Revealer::new();
        options.set_child(Some(&options_row));
        options.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        widget.append(&options);
        let revealer = options.clone();
        options_link.connect_clicked(move |_| revealer.set_reveal_child(!revealer.reveals_child()));

        SignIn {
            widget,
            password,
            submit,
            face_option,
            password_option,
            hello,
            status,
            status_row,
            notices,
            password_error,
            options,
            display_name,
        }
    }

    pub fn render(&self, state: &State) {
        let (text, eye) = if let Some(error) = &state.backend_error {
            (error.clone(), false)
        } else if state.signing_in {
            (WELCOME.to_string(), false)
        } else {
            match &state.face {
                Face::Idle => (String::new(), false),
                Face::Starting => (GETTING_READY.to_string(), true),
                Face::Looking => (LOOKING_FOR_YOU.to_string(), true),
                Face::Verifying => (MAKING_SURE.to_string(), true),
                Face::Welcome => (format!("Hello {}", self.display_name), true),
                Face::Failed(reason) => failure_text(reason.as_deref()),
            }
        };
        self.status.set_text(&text);
        self.status_row.set_visible(!text.is_empty());
        self.hello.widget.set_visible(eye);
        self.hello.set_face(&state.face);
        self.notices.set_text(&state.notices.join("\n"));
        self.notices.set_visible(!state.notices.is_empty());
        match &state.password_error {
            Some(error) => {
                self.password_error.set_text(error);
                self.password_error.set_visible(true);
            }
            None => self.password_error.set_visible(false),
        }
        if state.face == Face::Welcome {
            self.options.set_reveal_child(false);
        }
    }
}
