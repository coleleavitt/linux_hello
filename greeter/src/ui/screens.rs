//! The lock screen (clock and date) and the sign-in screen (user tile, status, password,
//! sign-in options), laid out after Windows 11 until docs/windows-hello-ui.md says otherwise.

use adw::prelude::*;
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::glib;
use gtk::glib::translate::IntoGlib;

use super::hello_icon::HelloIcon;
use crate::assets::{Assets, Hello};
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
* { font-family: "Segoe UI Variable", "Segoe UI", sans-serif; font-size: 14px; }
window { background: transparent; }
label { color: white; }
.clock { font-size: 98px; font-weight: 600; font-feature-settings: "tnum"; font-variation-settings: "opsz" 36; }
.date { font-size: 20px; font-weight: 600; }
.xlarge .clock { font-size: 136px; }
.xlarge .date { font-size: 28px; }
.greeting { font-size: 34px; font-weight: 600; }
.user-name { font-size: 28px; font-weight: 600; margin-top: 12px; }
.status { font-size: 24px; font-weight: 600; margin-top: 6px; }
.notice { font-size: 14px; color: #C5FFFFFF; }
.caps-lock { font-size: 14px; font-weight: 600; }
.password-error { font-size: 14px; margin-top: 8px; margin-bottom: 8px; }
.caption { font-size: 11px; color: rgba(255,255,255,0.6); }
.sign-in-options { font-size: 14px; color: #C5FFFFFF; padding: 0; min-height: 0; }
.sign-in-options:hover { text-decoration: underline; }
.field { min-width: 296px; border-radius: 4px; background: rgba(0,0,0,0.06); border-bottom: 1px solid rgba(255,255,255,0.54); }
.field:focus-within { background: rgba(30,30,30,0.7); border-bottom: 2px solid @accent_bg_color; }
.field entry.password { background: transparent; border: none; box-shadow: none; outline: none; color: #C7FFFFFF; caret-color: #C7FFFFFF; padding: 6px 8px 4px 8px; min-height: 0; }
.field entry.password text { color: #C7FFFFFF; }
.field entry.password placeholder { color: #C7FFFFFF; }
button.submit { min-width: 28px; min-height: 24px; margin: 3px 3px 4px 3px; padding: 0; border-radius: 3px; background: transparent; color: #C7FFFFFF; border: 1px solid transparent; box-shadow: none; }
button.submit:hover { background: @accent_bg_color; color: black; }
button.option { background: transparent; border-radius: 3px; min-width: 44px; min-height: 44px; padding: 0; margin: 0 10px; border: 2px solid transparent; box-shadow: none; }
button.option:hover { background: rgba(255,255,255,0.06); }
button.option.selected { border-color: #808080; }
button.system { background: transparent; border: none; box-shadow: none; border-radius: 4px; min-height: 40px; min-width: 40px; padding: 4px 8px; margin-left: 8px; color: white; }
button.system:hover { background: rgba(255,255,255,0.06); }
"#;

pub struct LockScreen {
    pub widget: gtk::Widget,
    clock: gtk::Label,
    date: gtk::Label,
    greeting: gtk::Label,
}

impl LockScreen {
    /// `SecureLockScreenUserControl.xaml` (§3.1): the clock block's top at 15 % of the height,
    /// centred; the greeting at the top with a 40 px margin.
    pub fn new(display_name: &str) -> LockScreen {
        let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let clock = gtk::Label::new(None);
        clock.add_css_class("clock");
        let date = gtk::Label::new(None);
        date.add_css_class("date");
        block.append(&clock);
        block.append(&date);
        let greeting = gtk::Label::new(Some(&DISMISS_TO_SIGN_IN.replace("{}", display_name)));
        greeting.add_css_class("greeting");
        greeting.set_visible(false);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let layout = gtk::ConstraintLayout::new();
        use gtk::ConstraintAttribute as A;
        let rel = |target: &gtk::Widget, attribute, multiplier, constant| {
            gtk::Constraint::new(
                Some(target),
                attribute,
                gtk::ConstraintRelation::Eq,
                None::<&gtk::Widget>,
                attribute,
                multiplier,
                constant,
                gtk::ConstraintStrength::Required.into_glib(),
            )
        };
        let top = gtk::Constraint::new(
            Some(block.upcast_ref::<gtk::Widget>()),
            A::Top,
            gtk::ConstraintRelation::Eq,
            None::<&gtk::Widget>,
            A::Bottom,
            0.15,
            0.0,
            gtk::ConstraintStrength::Required.into_glib(),
        );
        layout.add_constraint(top);
        layout.add_constraint(rel(block.upcast_ref(), A::CenterX, 1.0, 0.0));
        layout.add_constraint(rel(greeting.upcast_ref(), A::Top, 1.0, 40.0));
        layout.add_constraint(rel(greeting.upcast_ref(), A::CenterX, 1.0, 0.0));
        widget.set_layout_manager(Some(layout));
        widget.append(&block);
        widget.append(&greeting);

        let screen = LockScreen { widget: widget.upcast(), clock, date, greeting };
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

    /// `dismiss_lock_screen` off: the face was recognised, a key press signs in (113).
    pub fn show_welcome(&self, welcome: bool) {
        self.greeting.set_visible(welcome);
    }
}

/// The user tile (§3.2 `SelectedUserTileUserControl`): the account picture (else Windows'
/// default one from the assets) in a 192 px circle; without any, Windows' default tile: the
/// Segoe Fluent Icons contact glyph U+EA8C at 100 px on a 6 % white disc, or the initials
/// when that font is not there.
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
    let fluent_icons = assets.fonts.iter().any(|f| f == "Segoe Fluent Icons");
    let initials = account.initials();
    area.set_draw_func(move |area, cr, width, height| {
        let radius = width.min(height) as f64 / 2.0;
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, radius, 0.0, std::f64::consts::TAU);
        cr.clip();
        if let Some(pixbuf) = &picture {
            cr.set_source_pixbuf(
                pixbuf,
                (width - pixbuf.width()) as f64 / 2.0,
                (height - pixbuf.height()) as f64 / 2.0,
            );
            let _ = cr.paint();
            return;
        }
        let (text, family, size, alpha) = if fluent_icons {
            ("\u{EA8C}", "Segoe Fluent Icons", 100.0, 0.9)
        } else {
            (initials.as_str(), "Segoe UI Variable, Segoe UI, sans-serif", 80.0, 1.0)
        };
        cr.set_source_rgba(1.0, 1.0, 1.0, if fluent_icons { 0.061 } else { 0.3 });
        let _ = cr.paint();
        let layout = area.create_pango_layout(Some(text));
        let mut font = gtk::pango::FontDescription::new();
        font.set_family(family);
        font.set_absolute_size(size * f64::from(gtk::pango::SCALE));
        layout.set_font_description(Some(&font));
        let (text_width, text_height) = layout.pixel_size();
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        cr.move_to((width - text_width) as f64 / 2.0, (height - text_height) as f64 / 2.0);
        pangocairo::functions::show_layout(cr, &layout);
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
    pub widget: gtk::Widget,
    pub password: gtk::PasswordEntry,
    pub submit: gtk::Button,
    pub face_option: gtk::Button,
    pub password_option: gtk::Button,
    hello: HelloIcon,
    status: gtk::Label,
    status_row: gtk::Box,
    notices: gtk::Label,
    caps_lock: gtk::Label,
    password_error: gtk::Label,
    options: gtk::Revealer,
    display_name: String,
}

/// A label in the icon font (Segoe Fluent Icons from the assets), else a themed icon.
fn glyph_or_icon(assets: &Assets, glyph: char, size_px: i32, icon: &str) -> gtk::Widget {
    if assets.fonts.iter().any(|f| f == "Segoe Fluent Icons") {
        let label = gtk::Label::new(Some(&glyph.to_string()));
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrString::new_family("Segoe Fluent Icons"));
        attrs.insert(gtk::pango::AttrSize::new_size_absolute(size_px * gtk::pango::SCALE));
        label.set_attributes(Some(&attrs));
        label.upcast()
    } else {
        gtk::Image::from_icon_name(icon).upcast()
    }
}

impl SignIn {
    /// `RequestCredentialUserControl.xaml` (§3.2): the tile block centred in a 530 px box that
    /// sits 35 px above the middle, content pinned to the top of it with a 72 px margin.
    pub fn new(account: &Account, assets: &Assets) -> SignIn {
        let display_name = account.display_name.clone();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Start);
        content.set_margin_top(72);

        content.append(&avatar(account, assets));

        let name = gtk::Label::new(Some(&account.display_name));
        name.add_css_class("user-name");
        name.set_wrap(true);
        name.set_max_width_chars(28);
        name.set_justify(gtk::Justification::Center);
        content.append(&name);

        // BioFeedbackUserControl: the eye, the label under it (24 px SemiBold, margin-top 6).
        let hello = HelloIcon::new(&assets.hello);
        hello.widget.set_halign(gtk::Align::Center);
        let status = gtk::Label::new(None);
        status.add_css_class("status");
        status.set_justify(gtk::Justification::Center);
        status.set_wrap(true);
        status.set_max_width_chars(40);
        let status_row = gtk::Box::new(gtk::Orientation::Vertical, 0);
        status_row.set_halign(gtk::Align::Center);
        status_row.set_margin_top(8);
        status_row.append(&hello.widget);
        status_row.append(&status);
        content.append(&status_row);

        let notices = gtk::Label::new(None);
        notices.add_css_class("notice");
        notices.set_justify(gtk::Justification::Center);
        notices.set_visible(false);
        content.append(&notices);

        // PasswordFieldTemplate: a 296 px Border with the underline, the box and the submit
        // arrow (Segoe Fluent Icons U+E72A) inside it; the list's 20 px margin above.
        let password = gtk::PasswordEntry::new();
        password.add_css_class("password");
        password.set_placeholder_text(Some("Password"));
        password.set_show_peek_icon(true);
        password.set_hexpand(true);
        let submit = gtk::Button::new();
        submit.add_css_class("submit");
        submit.set_child(Some(&glyph_or_icon(assets, '\u{E72A}', 12, "go-next-symbolic")));
        submit.set_valign(gtk::Align::Center);
        let field = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        field.add_css_class("field");
        field.set_halign(gtk::Align::Center);
        field.set_margin_top(20);
        field.append(&password);
        field.append(&submit);
        content.append(&field);

        let caps_lock = gtk::Label::new(Some("Caps Lock is on"));
        caps_lock.add_css_class("caps-lock");
        caps_lock.set_visible(false);
        let caps_row = gtk::Box::new(gtk::Orientation::Vertical, 0);
        caps_row.set_size_request(-1, 20);
        caps_row.append(&caps_lock);
        content.append(&caps_row);

        let password_error = gtk::Label::new(None);
        password_error.add_css_class("password-error");
        password_error.set_visible(false);
        content.append(&password_error);

        // CredentialsListUserControl: the hyperlink, then the 44×44 provider buttons.
        let options_link = gtk::Button::with_label("Sign-in options");
        options_link.add_css_class("flat");
        options_link.add_css_class("sign-in-options");
        options_link.set_halign(gtk::Align::Center);
        content.append(&options_link);

        let face_option = gtk::Button::new();
        face_option.add_css_class("option");
        let logo: gtk::Widget = match &assets.face_icon {
            Some(path) => {
                let image = gtk::Image::from_file(path);
                image.set_pixel_size(24);
                image.upcast()
            }
            None => {
                let icon = HelloIcon::new(&Hello::Vector);
                icon.widget.set_size_request(24, 24);
                icon.widget
            }
        };
        logo.set_halign(gtk::Align::Center);
        logo.set_valign(gtk::Align::Center);
        face_option.set_child(Some(&logo));
        face_option.set_tooltip_text(Some("Face"));
        let password_option = gtk::Button::new();
        password_option.add_css_class("option");
        password_option.set_child(Some(&glyph_or_icon(assets, '\u{E192}', 24, "dialog-password-symbolic")));
        password_option.set_tooltip_text(Some("Password"));
        let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        options_row.set_halign(gtk::Align::Center);
        options_row.set_margin_top(32);
        options_row.set_margin_bottom(32);
        options_row.append(&face_option);
        options_row.append(&password_option);
        let options = gtk::Revealer::new();
        options.set_child(Some(&options_row));
        options.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        content.append(&options);
        let revealer = options.clone();
        options_link.connect_clicked(move |_| revealer.set_reveal_child(!revealer.reveals_child()));

        // The 530 px box, centred, 70 px above the middle (Margin 0,0,0,70).
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.set_size_request(404, 530);
        frame.set_halign(gtk::Align::Center);
        frame.set_valign(gtk::Align::Center);
        frame.set_margin_bottom(70);
        frame.append(&content);

        // SystemProviderUserControl, bottom right: the power menu (Segoe Fluent Icons U+E7E8).
        let power = gtk::MenuButton::new();
        power.add_css_class("system");
        power.set_child(Some(&glyph_or_icon(assets, '\u{E7E8}', 16, "system-shutdown-symbolic")));
        power.set_tooltip_text(Some("Power"));
        let menu = gtk::gio::Menu::new();
        menu.append(Some("Sleep"), Some("app.power::suspend"));
        menu.append(Some("Shut down"), Some("app.power::poweroff"));
        menu.append(Some("Restart"), Some("app.power::reboot"));
        power.set_menu_model(Some(&menu));
        let system_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        system_bar.set_halign(gtk::Align::End);
        system_bar.set_valign(gtk::Align::End);
        system_bar.set_margin_end(12);
        system_bar.set_margin_bottom(22);
        system_bar.append(&power);

        let widget = gtk::Overlay::new();
        widget.set_child(Some(&frame));
        widget.add_overlay(&system_bar);

        SignIn {
            widget: widget.upcast(),
            password,
            submit,
            face_option,
            password_option,
            hello,
            status,
            status_row,
            notices,
            caps_lock,
            password_error,
            options,
            display_name,
        }
    }

    /// "Caps Lock is on" (CapsLockWarning).
    pub fn set_caps_lock(&self, on: bool) {
        self.caps_lock.set_visible(on);
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
        // The selected credential gets the 2 px border: face while it runs, password once a
        // prompt waits.
        let face_selected = matches!(state.face, Face::Starting | Face::Looking | Face::Verifying | Face::Welcome);
        for (button, selected) in [(&self.face_option, face_selected), (&self.password_option, !face_selected)] {
            if selected {
                button.add_css_class("selected");
            } else {
                button.remove_css_class("selected");
            }
        }
    }
}
