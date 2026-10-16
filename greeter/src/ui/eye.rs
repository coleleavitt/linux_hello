//! Windows' Hello eye, frame-exact: the font-glyph animation of `FontGlyphAnimator.dll`
//! (`hello/eye/animation.json`: sequences of 33 ms frames rendered to `seqNN/FFF.png`, and
//! the transition table — for the sequence showing and the state wanted, play one sequence
//! then hold; docs/windows-hello-ui.md §1.3). This is the Windows 10 look and Windows' own
//! fallback; the Windows 11 composition is [`super::eye_vector`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::glib;
use gtk::prelude::*;
use serde::Deserialize;

use crate::state::Face;

/// `BioFeedbackState` values the face provider uses (§1.2).
const NONE: u32 = 0;
const FAIL: u32 = 1;
const START: u32 = 2;
const LOOP: u32 = 3;
const SUCCESS: u32 = 4;

pub fn wanted_state(face: &Face) -> u32 {
    match face {
        Face::Idle => NONE,
        Face::Starting | Face::Looking => START,
        Face::Verifying | Face::Blink => LOOP,
        Face::Welcome => SUCCESS,
        Face::Failed(_) => FAIL,
    }
}

#[derive(Deserialize)]
struct AnimationFile {
    sequences: HashMap<String, SequenceInfo>,
    transitions: HashMap<String, Vec<Transition>>,
}

#[derive(Deserialize)]
struct SequenceInfo {
    frame_ms: u64,
}

#[derive(Deserialize, Clone, Copy)]
struct Transition {
    /// The state wanted; -1 = any other.
    wanted: i64,
    play: u32,
    /// `None` = hold until the wanted state changes, 0 = go on at once.
    hold_ms: Option<u64>,
}

struct Sequence {
    frames: Vec<Pixbuf>,
    frame: Duration,
}

enum Phase {
    Playing {
        index: usize,
        next: Instant,
    },
    /// After the last frame: until the deadline, or until the wanted state changes.
    Holding {
        until: Option<Instant>,
    },
}

struct Player {
    sequences: HashMap<u32, Sequence>,
    transitions: HashMap<u32, Vec<Transition>>,
    wanted: u32,
    current: u32,
    phase: Phase,
}

impl Player {
    fn load(dir: &Path) -> Result<Player> {
        let file: AnimationFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join("animation.json")).context("animation.json")?)
                .context("animation.json")?;
        let mut sequences = HashMap::new();
        for (id, info) in &file.sequences {
            let id: u32 = id.parse().context("sequence id")?;
            let seq_dir = dir.join(format!("seq{id:02}"));
            let mut paths: Vec<_> = std::fs::read_dir(&seq_dir)
                .with_context(|| seq_dir.display().to_string())?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "png"))
                .collect();
            paths.sort();
            let frames = paths
                .iter()
                .map(|p| Pixbuf::from_file(p).with_context(|| p.display().to_string()))
                .collect::<Result<_>>()?;
            sequences.insert(id, Sequence { frames, frame: Duration::from_millis(info.frame_ms) });
        }
        let transitions =
            file.transitions.into_iter().map(|(k, v)| Ok((k.parse::<u32>()?, v))).collect::<Result<_>>()?;
        Ok(Player { sequences, transitions, wanted: NONE, current: NONE, phase: Phase::Holding { until: None } })
    }

    /// The table's row for the sequence showing and the state wanted.
    fn next_transition(&self) -> Option<Transition> {
        let row = self.transitions.get(&self.current)?;
        row.iter().find(|t| t.wanted == i64::from(self.wanted)).or_else(|| row.iter().find(|t| t.wanted == -1)).copied()
    }

    fn start(&mut self, transition: Transition, now: Instant) {
        self.current = transition.play;
        let frames = self.sequences.get(&self.current).map_or(0, |s| s.frames.len());
        self.phase = if frames == 0 {
            Phase::Holding { until: transition.hold_ms.map(|ms| now + Duration::from_millis(ms)) }
        } else {
            Phase::Playing { index: 0, next: now + self.sequences[&self.current].frame }
        };
        // The hold after a sequence is read from the same row when it ends.
    }

    fn set_wanted(&mut self, wanted: u32, now: Instant) {
        if wanted == self.wanted {
            return;
        }
        self.wanted = wanted;
        if let Phase::Holding { until: None } = self.phase {
            if let Some(t) = self.next_transition() {
                self.start(t, now);
            }
        }
    }

    /// Advance to `now`; whether the picture changed.
    fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for _ in 0..8 {
            match self.phase {
                Phase::Playing { index, next } => {
                    if now < next {
                        break;
                    }
                    let seq = &self.sequences[&self.current];
                    if index + 1 < seq.frames.len() {
                        self.phase = Phase::Playing { index: index + 1, next: next + seq.frame };
                        changed = true;
                    } else {
                        let hold = self.next_transition().and_then(|t| t.hold_ms);
                        self.phase = Phase::Holding { until: hold.map(|ms| next + Duration::from_millis(ms)) };
                    }
                }
                Phase::Holding { until: Some(until) } if now >= until => match self.next_transition() {
                    Some(t) => {
                        self.start(t, until);
                        changed = true;
                    }
                    None => self.phase = Phase::Holding { until: None },
                },
                Phase::Holding { .. } => break,
            }
        }
        changed
    }

    fn frame(&self) -> Option<&Pixbuf> {
        let seq = self.sequences.get(&self.current)?;
        match self.phase {
            Phase::Playing { index, .. } => seq.frames.get(index),
            Phase::Holding { .. } => seq.frames.last(),
        }
    }
}

/// The eye as a widget; `size` is its logical size (Windows: 64 px Lottie, 60 px glyph in a
/// 72 px frame).
pub struct Eye {
    pub widget: gtk::DrawingArea,
    player: Rc<RefCell<Player>>,
}

impl Eye {
    pub fn new(dir: &Path, size: i32) -> Result<Eye> {
        let player = Rc::new(RefCell::new(Player::load(dir)?));
        let widget = gtk::DrawingArea::new();
        widget.set_size_request(size, size);
        let draw = Rc::clone(&player);
        widget.set_draw_func(move |_, cr, width, height| {
            if let Some(frame) = draw.borrow().frame() {
                let scale = f64::from(width.min(height)) / f64::from(frame.width().max(frame.height()));
                cr.translate(
                    (f64::from(width) - scale * f64::from(frame.width())) / 2.0,
                    (f64::from(height) - scale * f64::from(frame.height())) / 2.0,
                );
                cr.scale(scale, scale);
                cr.set_source_pixbuf(frame, 0.0, 0.0);
                let _ = cr.paint();
            }
        });
        let tick = Rc::clone(&player);
        widget.add_tick_callback(move |widget, _| {
            if tick.borrow_mut().advance(Instant::now()) {
                widget.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        Ok(Eye { widget, player })
    }

    pub fn set_face(&self, face: &Face) {
        self.player.borrow_mut().set_wanted(wanted_state(face), Instant::now());
        self.widget.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-sequence table in the file's shape: 2 (entrance, 2 frames) holds; wanted 3 plays
    /// 8 (1 frame) with a 100 ms hold, then 9, then back to 8.
    fn player() -> Player {
        let frame = |n| Sequence {
            frames: (0..n).map(|_| Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 2, 2).unwrap()).collect(),
            frame: Duration::from_millis(33),
        };
        let t = |wanted, play, hold_ms| Transition { wanted, play, hold_ms };
        Player {
            sequences: [(2, frame(2)), (8, frame(1)), (9, frame(1))].into_iter().collect(),
            transitions: [
                (0, vec![t(2, 2, None), t(-1, 0, None)]),
                (2, vec![t(3, 8, Some(100)), t(-1, 0, None)]),
                (8, vec![t(3, 9, Some(100)), t(-1, 0, None)]),
                (9, vec![t(3, 8, Some(100)), t(-1, 0, None)]),
            ]
            .into_iter()
            .collect(),
            wanted: 0,
            current: 0,
            phase: Phase::Holding { until: None },
        }
    }

    #[test]
    fn entrance_holds_then_the_loop_alternates_with_holds() {
        let mut p = player();
        let t0 = Instant::now();
        p.set_wanted(2, t0);
        assert_eq!(p.current, 2);
        assert!(p.advance(t0 + Duration::from_millis(40)));
        assert!(!p.advance(t0 + Duration::from_millis(80)));
        assert!(matches!(p.phase, Phase::Holding { until: None }));
        p.set_wanted(3, t0 + Duration::from_millis(100));
        assert_eq!(p.current, 8);
        // One frame, then the 100 ms hold, then sequence 9.
        p.advance(t0 + Duration::from_millis(140));
        assert!(matches!(p.phase, Phase::Holding { until: Some(_) }));
        assert!(p.advance(t0 + Duration::from_millis(300)));
        assert_eq!(p.current, 9);
        // 9's frame at 233 ms, its hold until 366; 8 again at 366 (held until 499), 9 at 499.
        p.advance(t0 + Duration::from_millis(400));
        assert_eq!(p.current, 8);
        p.advance(t0 + Duration::from_millis(550));
        assert_eq!(p.current, 9);
    }
}
