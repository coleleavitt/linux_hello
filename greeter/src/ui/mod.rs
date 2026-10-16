//! The windows: one per monitor, the lock screen and the sign-in screen on the first, the
//! background on the others; the attempts and the effects of the state machine.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;
use gtk4_layer_shell::LayerShell;
use tracing::{error, info, warn};

use crate::assets::Assets;
use crate::auth::{self, Attempt, AuthEvent};
use crate::config::Config;
use crate::state::{Effect, Face, Input, Screen, State};
use crate::users::Account;

mod background;
mod hello_icon;
mod screens;

use background::Background;
use screens::{LockScreen, SignIn};

/// How the windows reach the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Under greetd's compositor: a layer-shell overlay on every monitor.
    LayerShell,
    /// Inside the user's session: `ext-session-lock-v1`.
    SessionLock,
    /// A plain 1600×1000 window (development: screenshots inside a nested niri).
    Window,
}

/// Who runs the PAM stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Greetd(PathBuf),
    /// `hello-lock`, from `/etc/pam.d` or (development) from a directory.
    Pam {
        confdir: Option<PathBuf>,
    },
}

pub struct Options {
    pub surface: Surface,
    pub backend: Backend,
    pub config: Config,
    pub assets: Assets,
    pub account: Account,
    /// greetd: the command of the session to start.
    pub session_command: Vec<String>,
}

pub fn run(options: Options) -> glib::ExitCode {
    let app =
        adw::Application::new(Some("io.github.coleleavitt.hello_greeter"), gtk::gio::ApplicationFlags::NON_UNIQUE);
    let options = RefCell::new(Some(options));
    app.connect_activate(move |app| {
        if let Some(options) = options.take() {
            App::start(app, options);
        }
    });
    app.run_with_args::<&str>(&[])
}

struct App {
    app: adw::Application,
    options: Options,
    state: RefCell<State>,
    attempt: RefCell<Option<Attempt>>,
    events: async_channel::Sender<AuthEvent>,
    lock: Option<gtk4_session_lock::Instance>,
    main: RefCell<Option<MainWindow>>,
    /// Keeps the application alive while the session lock has no window yet.
    _hold: gtk::gio::ApplicationHoldGuard,
}

struct MainWindow {
    window: gtk::Window,
    background: Background,
    stack: gtk::Stack,
    lock_screen: LockScreen,
    sign_in: SignIn,
}

impl App {
    fn start(app: &adw::Application, options: Options) {
        load_css(&options.assets);
        let (events, receiver) = async_channel::unbounded();
        let lock = (options.surface == Surface::SessionLock).then(gtk4_session_lock::Instance::new);
        let state = State::new(
            matches!(options.backend, Backend::Pam { .. }),
            options.config.dismiss_lock_screen,
            options.config.lock_face_seconds,
            Instant::now(),
        );
        let this = Rc::new(App {
            app: app.clone(),
            options,
            state: RefCell::new(state),
            attempt: RefCell::new(None),
            events,
            lock,
            main: RefCell::new(None),
            _hold: app.hold(),
        });

        let events = Rc::clone(&this);
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                info!("auth: {event:?}");
                events.dispatch(Input::Auth(event));
            }
        });

        match &this.lock {
            Some(lock) => {
                let on_locked = Rc::clone(&this);
                lock.connect_locked(move |lock| {
                    info!("session locked");
                    on_locked.open_windows(Some(lock));
                });
                let on_failed = app.clone();
                lock.connect_failed(move |_| {
                    error!("could not lock the session (another locker running?)");
                    on_failed.quit();
                });
                let on_unlocked = app.clone();
                lock.connect_unlocked(move |_| {
                    info!("session unlocked");
                    on_unlocked.quit();
                });
                if !lock.lock() {
                    error!("ext-session-lock-v1 is not available");
                    app.quit();
                }
            }
            None => this.open_windows(None),
        }
    }

    fn open_windows(self: &Rc<Self>, lock: Option<&gtk4_session_lock::Instance>) {
        let display = gtk::gdk::Display::default().expect("display");
        let monitors = display.monitors();
        let count = monitors.n_items();
        for i in 0..count {
            let monitor = monitors.item(i).and_downcast::<gtk::gdk::Monitor>().expect("monitor");
            let window = gtk::Window::new();
            window.set_application(Some(&self.app));
            window.set_title(Some("Sign in"));
            window.set_decorated(false);
            let main = i == 0;
            if main {
                self.build_main(&window);
            } else {
                window.set_child(Some(&Background::new(self.options.assets.background.as_deref())));
            }
            match self.options.surface {
                Surface::LayerShell => {
                    window.init_layer_shell();
                    window.set_layer(gtk4_layer_shell::Layer::Overlay);
                    window.set_namespace(Some("hello-greeter"));
                    for edge in [
                        gtk4_layer_shell::Edge::Top,
                        gtk4_layer_shell::Edge::Bottom,
                        gtk4_layer_shell::Edge::Left,
                        gtk4_layer_shell::Edge::Right,
                    ] {
                        window.set_anchor(edge, true);
                    }
                    window.set_keyboard_mode(if main {
                        gtk4_layer_shell::KeyboardMode::Exclusive
                    } else {
                        gtk4_layer_shell::KeyboardMode::None
                    });
                    window.set_monitor(Some(&monitor));
                }
                Surface::SessionLock => lock.expect("lock instance").assign_window_to_monitor(&window, &monitor),
                Surface::Window => {
                    if !main {
                        continue;
                    }
                    // Never fullscreen: this runs inside someone's session (a nested niri for
                    // screenshots, docs/greeter.md).
                    window.set_default_size(1600, 1000);
                }
            }
            window.present();
        }
        info!(
            "{} window(s) on {count} monitor(s); {}",
            if self.options.surface == Surface::Window { 1 } else { count },
            self.options.assets.describe()
        );
        let effects = self.state.borrow_mut().start();
        self.apply(effects);
        self.render();
    }

    fn build_main(self: &Rc<Self>, window: &gtk::Window) {
        let background = Background::new(self.options.assets.background.as_deref());
        let lock_screen = LockScreen::new(&self.options.account.display_name);
        let sign_in = SignIn::new(&self.options.account, &self.options.assets);
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::SlideUp);
        stack.set_transition_duration(300);
        stack.add_named(&lock_screen.widget, Some("lock"));
        stack.add_named(&sign_in.widget, Some("sign-in"));
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&background));
        overlay.add_overlay(&stack);
        if self.options.surface == Surface::Window {
            let caption = gtk::Label::new(Some(&self.options.assets.describe()));
            caption.add_css_class("caption");
            caption.set_halign(gtk::Align::End);
            caption.set_valign(gtk::Align::End);
            caption.set_margin_end(16);
            caption.set_margin_bottom(8);
            caption.set_wrap(true);
            caption.set_max_width_chars(80);
            overlay.add_overlay(&caption);
        }
        window.set_child(Some(&overlay));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::clone(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if this.state.borrow().screen == Screen::Lock {
                this.dispatch(Input::Wake);
                return glib::Propagation::Stop;
            }
            // Typing anywhere on the sign-in screen goes to the password box, as on Windows.
            let main = this.main.borrow();
            if let Some(main) = main.as_ref() {
                let entry: &gtk::Widget = main.sign_in.password.upcast_ref();
                let in_entry =
                    GtkWindowExt::focus(&main.window).is_some_and(|focus| focus == *entry || focus.is_ancestor(entry));
                if !in_entry && key.to_unicode().is_some() && !modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                {
                    main.sign_in.password.grab_focus();
                }
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::clone(self);
        click.connect_pressed(move |_, _, _, _| {
            if this.state.borrow().screen == Screen::Lock {
                this.dispatch(Input::Wake);
            }
        });
        window.add_controller(click);

        let this = Rc::clone(self);
        sign_in.password.connect_activate(move |entry| {
            let password = entry.text().to_string();
            entry.set_text("");
            this.dispatch(Input::Submit(password));
        });
        let this = Rc::clone(self);
        let entry = sign_in.password.clone();
        sign_in.submit.connect_clicked(move |_| {
            let password = entry.text().to_string();
            entry.set_text("");
            this.dispatch(Input::Submit(password));
        });
        let this = Rc::clone(self);
        sign_in.face_option.connect_clicked(move |_| this.dispatch(Input::RetryFace));
        let this = Rc::clone(self);
        sign_in.password_option.connect_clicked(move |_| this.dispatch(Input::UsePassword));

        *self.main.borrow_mut() = Some(MainWindow { window: window.clone(), background, stack, lock_screen, sign_in });
    }

    fn dispatch(self: &Rc<Self>, input: Input) {
        let effects = self.state.borrow_mut().handle(input, Instant::now());
        self.apply(effects);
        self.render();
    }

    fn apply(self: &Rc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            info!("effect: {effect:?}");
            match effect {
                Effect::StartAttempt => {
                    let user = &self.options.account.name;
                    let attempt = match &self.options.backend {
                        Backend::Greetd(socket) => auth::greetd::start(socket, user, self.events.clone()),
                        Backend::Pam { confdir } => auth::pam::start_in(confdir.as_deref(), user, self.events.clone()),
                    };
                    *self.attempt.borrow_mut() = Some(attempt);
                }
                Effect::Answer(text) => match self.attempt.borrow().as_ref() {
                    Some(attempt) => attempt.answer(Some(text)),
                    None => warn!("answer without an attempt"),
                },
                Effect::StartSession => match self.attempt.borrow().as_ref() {
                    Some(attempt) => attempt.start_session(self.options.session_command.clone(), vec![]),
                    None => warn!("start_session without an attempt"),
                },
                Effect::Unlock => {
                    if let Some(lock) = &self.lock {
                        lock.unlock();
                    } else {
                        self.app.quit();
                    }
                }
                Effect::Exit => self.app.quit(),
                Effect::FocusPassword => {
                    if let Some(main) = self.main.borrow().as_ref() {
                        main.sign_in.password.grab_focus();
                    }
                }
            }
        }
    }

    /// Show the state.
    fn render(&self) {
        let main = self.main.borrow();
        let Some(main) = main.as_ref() else { return };
        let state = self.state.borrow();
        let sign_in_screen = state.screen == Screen::SignIn;
        main.stack.set_visible_child_name(if sign_in_screen { "sign-in" } else { "lock" });
        main.background.set_blurred(sign_in_screen);
        main.lock_screen.show_welcome(state.face == Face::Welcome);
        main.sign_in.render(&state);
    }
}

/// Typography and colours; Segoe first, the system sans when the assets are missing.
fn load_css(assets: &Assets) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(screens::CSS);
    let display = gtk::gdk::Display::default().expect("display");
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    if !assets.fonts.is_empty() {
        info!("fonts from the assets: {}", assets.fonts.join(", "));
    }
}
