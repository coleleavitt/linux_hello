//! The real Windows sign-in assets, when they are there (docs/windows-hello-ui.md lists the
//! sources; `scripts/extract_windows_hello_assets.py` extracts them to
//! `models/windows_hello/assets/`, git-ignored, and contrib/install-greeter.sh copies that
//! directory to `/usr/local/share/linux_hello/assets`). Everything has a fallback, so the
//! greeter works without them; [`Assets::describe`] says what is in use.
//!
//! ```text
//! assets/
//!   fonts/*.ttf         Windows/Fonts: SegUIVar.ttf (Segoe UI Variable), segoeui*.ttf,
//!                       SegoeIcons.ttf (Segoe Fluent Icons), segmdl2.ttf; added to fontconfig
//!   screen/img100.jpg   Windows/Web/Screen: the lock-screen background
//!   user-pictures/user-192.png   ProgramData/Microsoft/User Account Pictures: the default avatar
//!   hello/frames/NNN.png | hello/hello.png   the Hello face icon (animation frames or one image)
//! ```

use std::ffi::{c_char, c_int, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::config::{Config, INSTALLED_ASSETS};

/// Where the Hello face icon comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hello {
    /// Windows' animation frames, in order.
    Frames(Vec<PathBuf>),
    /// One image.
    Image(PathBuf),
    /// A glyph of an icon font from the assets (`hello_glyph` in the config).
    Glyph { family: String, ch: char },
    /// The greeter's own Cairo drawing.
    Vector,
}

#[derive(Debug, Clone)]
pub struct Assets {
    pub dir: Option<PathBuf>,
    /// Font families added to fontconfig from `fonts/`; empty means the system sans is used.
    pub fonts: Vec<String>,
    pub background: Option<PathBuf>,
    pub hello: Hello,
    pub user_picture: Option<PathBuf>,
}

impl Assets {
    /// The assets of `dir` (`--assets`, else the config, else `$LINUX_HELLO_ASSETS`, else the
    /// installed directory, else the repository's). Must run before GTK creates its font map.
    pub fn load(config: &Config, dir: Option<&Path>) -> Assets {
        let dir = dir.map(Path::to_path_buf).or_else(|| config.assets.clone()).or_else(find);
        let Some(dir) = dir else {
            return Assets { dir: None, fonts: vec![], background: None, hello: Hello::Vector, user_picture: None };
        };
        let fonts = add_fonts(&dir.join("fonts"));
        let background =
            if config.background.is_absolute() { config.background.clone() } else { dir.join(&config.background) };
        let hello = hello_source(&dir, &config.hello_glyph, &fonts);
        let user_picture = dir.join("user-pictures/user-192.png");
        Assets {
            background: background.is_file().then_some(background),
            user_picture: user_picture.is_file().then_some(user_picture),
            hello,
            fonts,
            dir: Some(dir),
        }
    }

    /// One line for the log and the dev window: what is Windows' and what is the fallback.
    pub fn describe(&self) -> String {
        let dir = self.dir.as_ref().map_or("none".to_string(), |d| d.display().to_string());
        let fonts = if self.fonts.is_empty() { "system sans".to_string() } else { self.fonts.join(", ") };
        let hello = match &self.hello {
            Hello::Frames(frames) => format!("{} frames", frames.len()),
            Hello::Image(path) => path.display().to_string(),
            Hello::Glyph { family, ch } => format!("U+{:04X} of {family}", *ch as u32),
            Hello::Vector => "built-in icon".to_string(),
        };
        let background = self.background.as_ref().map_or("solid colour".to_string(), |p| p.display().to_string());
        let picture = self.user_picture.as_ref().map_or("initials".to_string(), |p| p.display().to_string());
        format!("assets: {dir}; fonts: {fonts}; hello: {hello}; background: {background}; default picture: {picture}")
    }
}

/// The first assets directory that exists: `$LINUX_HELLO_ASSETS`, the installed one, the
/// repository's (for `cargo run`).
pub fn find() -> Option<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models/windows_hello/assets");
    std::env::var_os("LINUX_HELLO_ASSETS")
        .map(PathBuf::from)
        .into_iter()
        .chain([PathBuf::from(INSTALLED_ASSETS), repo])
        .find(|d| d.is_dir())
}

fn hello_source(dir: &Path, glyph: &str, fonts: &[String]) -> Hello {
    let mut frames: Vec<PathBuf> = std::fs::read_dir(dir.join("hello/frames"))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "png"))
                .collect()
        })
        .unwrap_or_default();
    frames.sort();
    if !frames.is_empty() {
        return Hello::Frames(frames);
    }
    let image = dir.join("hello/hello.png");
    if image.is_file() {
        return Hello::Image(image);
    }
    if let Some(ch) = u32::from_str_radix(glyph.trim_start_matches("U+"), 16).ok().and_then(char::from_u32) {
        if let Some(family) = fonts.iter().find(|f| f.contains("Icons") || f.contains("MDL2")) {
            return Hello::Glyph { family: family.clone(), ch };
        }
    }
    Hello::Vector
}

// fontconfig: application fonts are added to the current configuration, which Pango's
// font map reads when GTK first uses it.
#[repr(C)]
struct FcFontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut *mut c_void,
}

#[link(name = "fontconfig")]
extern "C" {
    fn FcConfigGetCurrent() -> *mut c_void;
    fn FcConfigAppFontAddDir(config: *mut c_void, dir: *const c_char) -> c_int;
    fn FcConfigGetFonts(config: *mut c_void, set: c_int) -> *mut FcFontSet;
    fn FcPatternGetString(pattern: *mut c_void, object: *const c_char, n: c_int, s: *mut *mut c_char) -> c_int;
}
const FC_SET_APPLICATION: c_int = 1;
const FC_RESULT_MATCH: c_int = 0;

/// Add every font file under `dir` to fontconfig; the family names found, sorted.
fn add_fonts(dir: &Path) -> Vec<String> {
    if !dir.is_dir() {
        return vec![];
    }
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else { return vec![] };
    let mut families = vec![];
    // SAFETY: plain fontconfig calls on the process-wide configuration; the font set and the
    // strings it returns are owned by fontconfig and only read here.
    unsafe {
        let config = FcConfigGetCurrent();
        if FcConfigAppFontAddDir(config, path.as_ptr()) == 0 {
            return vec![];
        }
        let set = FcConfigGetFonts(config, FC_SET_APPLICATION);
        if !set.is_null() {
            for i in 0..(*set).nfont.max(0) as usize {
                let mut family: *mut c_char = ptr::null_mut();
                if FcPatternGetString(*(*set).fonts.add(i), c"family".as_ptr(), 0, &mut family) == FC_RESULT_MATCH
                    && !family.is_null()
                {
                    let name = std::ffi::CStr::from_ptr(family).to_string_lossy().into_owned();
                    if !families.contains(&name) {
                        families.push(name);
                    }
                }
            }
        }
    }
    families.sort();
    families
}
