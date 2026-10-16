//! Last-resort panic guard around interactive commands. `Session::execute` and interactive input
//! already catch a panicking command; this wrapper does the same for the rest of a command
//! machine: its factory, `begin` (the first step of `Session::start`), `prompt` and `preview`
//! (called by the UI every frame). A panic ends the command with an internal error instead of
//! taking down the app: from `begin` or `input` at once (the session restores the drawing), from
//! `prompt` or `preview` (which can't end the command) at the next input, with a fallback prompt
//! and no preview until then.
//!
//! It also runs a plan-only command in a level construction plane in World (see
//! [`crate::cplane`]): every step with [`Session::cplane`] World, the points given in the plane
//! taken into the world, the prompt's rubber-band base and the previews brought back.

use std::any::Any;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering::Relaxed;

use cadcraft_doc::EntityKind;
use cadcraft_geom::Vec2;

use crate::cmd::Factory;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub(crate) struct Guarded {
    inner: Box<dyn Interactive>,
    /// What panicked in `prompt` or `preview`; the command ends at the next input.
    broken: Cell<Option<&'static str>>,
    /// A plan-only command started in a level construction plane: it works in World.
    world: bool,
}

/// While `s` runs a step of a command that works in World: [`Session::cplane`] is World.
fn in_world<T>(s: &Session, on: bool, f: impl FnOnce() -> T) -> T {
    if !on {
        return f();
    }
    let was = s.plan_world.swap(true, Relaxed);
    let r = f();
    s.plan_world.store(was, Relaxed);
    r
}

pub(crate) fn message(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

impl Guarded {
    /// Create the machine of command `id` and wrap it.
    pub(crate) fn create(id: &str, factory: Factory, s: &Session) -> Result<Box<dyn Interactive>> {
        let world = crate::cplane::in_world(s, id);
        let inner =
            in_world(s, world, || catch_unwind(AssertUnwindSafe(|| factory(s)))).map_err(|p| EngineError::Internal(id.into(), message(&*p)))??;
        Ok(Box::new(Guarded { inner, broken: Cell::new(None), world }))
    }

    /// Wrap a machine created by other means (the hot-grip edit, a dynamic grip's drag); with
    /// `world` it works in World as a plan-only command in a level plane does.
    pub(crate) fn wrap(inner: Box<dyn Interactive>, world: bool) -> Box<dyn Interactive> {
        Box::new(Guarded { inner, broken: Cell::new(None), world })
    }

    fn internal(&self, p: &(dyn Any + Send)) -> EngineError {
        EngineError::Internal(self.inner.name().to_ascii_lowercase(), message(p))
    }
}

impl Interactive for Guarded {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if self.broken.get().is_none() {
            let c = s.cplane();
            match in_world(s, self.world, || catch_unwind(AssertUnwindSafe(|| self.inner.prompt(s)))) {
                // Pending point modifiers (FROM, M2P…) ask for their points in its place.
                Ok(mut p) => {
                    if self.world {
                        p.base = p.base.map(|b| crate::cplane::plan_to_plane(&c, b));
                    }
                    return s.overlay_prompt(p);
                }
                Err(_) => self.broken.set(Some("panic in prompt")),
            }
        }
        Prompt::new("Internal error; press Enter to end the command", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if let Some(m) = self.broken.get() {
            return Err(EngineError::Internal(self.inner.name().to_ascii_lowercase(), m.into()));
        }
        if !self.world {
            return catch_unwind(AssertUnwindSafe(|| self.inner.input(s, i))).unwrap_or_else(|p| Err(self.internal(&*p)));
        }
        // Points given in the plane, taken into the world (with their heights) for this step.
        let (i, z) = crate::cplane::input_to_world(&s.cplane(), i, s.point_z);
        let given = std::mem::replace(&mut s.point_z, z);
        let was = s.plan_world.swap(true, Relaxed);
        let r = catch_unwind(AssertUnwindSafe(|| self.inner.input(s, i)));
        s.plan_world.store(was, Relaxed);
        s.point_z = given;
        r.unwrap_or_else(|p| Err(self.internal(&*p)))
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        let was = self.world.then(|| s.plan_world.swap(true, Relaxed));
        let r = catch_unwind(AssertUnwindSafe(|| self.inner.begin(s)));
        if let Some(w) = was {
            s.plan_world.store(w, Relaxed);
        }
        r.unwrap_or_else(|p| Err(self.internal(&*p)))
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        if self.broken.get().is_some() {
            return Vec::new();
        }
        let c = s.cplane();
        let at = if self.world { crate::cplane::plane_to_plan(&c, cursor) } else { cursor };
        match in_world(s, self.world, || catch_unwind(AssertUnwindSafe(|| self.inner.preview(s, at)))) {
            Ok(k) if self.world => crate::cplane::kinds_to_plane(&c, k),
            Ok(k) => k,
            Err(_) => {
                self.broken.set(Some("panic in preview"));
                Vec::new()
            }
        }
    }
    fn drag(&self, s: &Session, cursor: Vec2) -> Option<crate::prompt::Drag> {
        if self.broken.get().is_some() {
            return None;
        }
        let c = s.cplane();
        let at = if self.world { crate::cplane::plane_to_plan(&c, cursor) } else { cursor };
        match in_world(s, self.world, || catch_unwind(AssertUnwindSafe(|| self.inner.drag(s, at)))) {
            Ok(Some(d)) if self.world => Some(crate::prompt::Drag {
                matrix: crate::cplane::matrix_to_plane(&c, d.matrix),
                extra: crate::cplane::kinds_to_plane(&c, d.extra),
                ..d
            }),
            Ok(d) => d,
            Err(_) => {
                self.broken.set(Some("panic in preview"));
                None
            }
        }
    }
    fn editing_mtext(&self) -> Option<cadcraft_doc::MText> {
        if self.broken.get().is_some() {
            return None;
        }
        catch_unwind(AssertUnwindSafe(|| self.inner.editing_mtext())).unwrap_or_else(|_| {
            self.broken.set(Some("panic in prompt"));
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Running;

    fn dot(p: Vec2) -> EntityKind {
        EntityKind::Point(cadcraft_doc::Point { p: p.to3(0.0), angle: 0.0 })
    }

    /// Panics in the step named by `at` ("factory", "begin", "prompt", "preview" or "input").
    struct PanicM(&'static str);

    impl Interactive for PanicM {
        fn name(&self) -> &'static str {
            "PANICTEST"
        }
        fn prompt(&self, _s: &Session) -> Prompt {
            assert_ne!(self.0, "prompt", "prompt panicked");
            Prompt::new("Specify a point", Accept::POINT)
        }
        fn input(&mut self, s: &mut Session, _i: Input) -> Result<Step> {
            s.add_entity(dot(Vec2::ZERO))?;
            assert_ne!(self.0, "input", "input panicked");
            Ok(Step::Continue)
        }
        fn begin(&mut self, s: &mut Session) -> Result<Step> {
            s.add_entity(dot(Vec2::ZERO))?;
            assert_ne!(self.0, "begin", "begin panicked");
            Ok(Step::Continue)
        }
        fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
            assert_ne!(self.0, "preview", "preview panicked");
            vec![dot(cursor)]
        }
    }

    /// Start a PanicM the way `Session::start` starts an interactive command.
    fn start(s: &mut Session, at: &'static str) -> Result<()> {
        fn factory_panics(_: &Session) -> Result<Box<dyn Interactive>> {
            panic!("factory panicked")
        }
        let factory: Factory = match at {
            "factory" => factory_panics,
            "begin" => |_| Ok(Box::new(PanicM("begin"))),
            "prompt" => |_| Ok(Box::new(PanicM("prompt"))),
            "preview" => |_| Ok(Box::new(PanicM("preview"))),
            _ => |_| Ok(Box::new(PanicM("input"))),
        };
        let machine = Guarded::create("panictest", factory, s)?;
        let st = s.state()?;
        let (before, selection_before) = (st.doc.clone(), st.selection.clone());
        s.running = Some(Running { id: "panictest".into(), machine, before, selection_before });
        s.feed(None)
    }

    fn points(s: &Session) -> usize {
        s.doc().unwrap().model.iter().count()
    }

    #[test]
    fn panics_in_a_command_machine_end_the_command_not_the_app() {
        // Factory and begin: an internal error, nothing running, the drawing as it was.
        for at in ["factory", "begin"] {
            let mut s = Session::new();
            let e = start(&mut s, at).unwrap_err();
            assert!(matches!(&e, EngineError::Internal(_, m) if m.contains("panicked")), "{at}: {e}");
            assert!(s.running.is_none() && s.current_prompt().is_none(), "{at}");
            assert_eq!(points(&s), 0, "{at}: the drawing is restored");
            s.cmdline("line 0,0 1,0").unwrap();
            assert!(s.running.is_some(), "{at}: the session still works");
        }

        // Prompt and preview: a fallback prompt and no preview from then on; the next input ends
        // the command and restores the drawing.
        let at = Vec2::new(1.0, 1.0);
        let mut s = Session::new();
        start(&mut s, "prompt").unwrap();
        assert_eq!(points(&s), 1, "begin ran");
        assert_eq!(s.preview(at).len(), 1, "the preview works until the prompt panics");
        assert!(s.current_prompt().unwrap().message.contains("Internal error"));
        assert!(s.prompt_text().starts_with("PANICTEST Internal error"), "{}", s.prompt_text());
        assert!(s.preview(at).is_empty());
        assert!(matches!(s.input(Input::Enter), Err(EngineError::Internal(_, m)) if m == "panic in prompt"));
        assert!(s.running.is_none());
        assert_eq!(points(&s), 0, "the drawing is restored");

        let mut s = Session::new();
        start(&mut s, "preview").unwrap();
        assert!(s.preview(at).is_empty());
        assert!(s.preview(at).is_empty(), "not called again");
        assert!(s.current_prompt().unwrap().message.contains("Internal error"));
        assert!(matches!(s.input(Input::Point(at)), Err(EngineError::Internal(_, m)) if m == "panic in preview"));
        assert!(s.running.is_none());
        assert_eq!(points(&s), 0, "the drawing is restored");

        // Input (already guarded by the session; the wrapper agrees).
        let mut s = Session::new();
        start(&mut s, "input").unwrap();
        assert!(matches!(s.input(Input::Point(Vec2::ZERO)), Err(EngineError::Internal(..))));
        assert!(s.running.is_none());
        assert_eq!(points(&s), 0);
    }
}
