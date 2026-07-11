//! Evaluation driving: budget-sliced continuous runs (so the UI stays
//! responsive) and one-interaction-at-a-time stepping for the debugger panel.

use std::collections::VecDeque;
use std::sync::Mutex;

use atlas_core::vm::exec::{ExecPolicy, Executor, FiniteBudget, InteractionType, UnlimitedBudget};
use atlas_core::vm::heap::TermPtr;

use crate::session::Session;

/// Interactions per continuous-run slice: small enough to redraw between
/// slices, large enough that slicing overhead is negligible.
const SLICE: u64 = 10_000;
/// Recent interactions kept for the stepper panel.
const HISTORY_CAP: usize = 200;

/// A policy that performs at most a single interaction before stopping, and
/// remembers which one it was. The stepper drives reduction with this one step
/// at a time so each snapshot is a real intermediate term.
#[derive(Default)]
pub struct StepPolicy {
    // Interior mutability through `&self`. A `Mutex` (rather than a `Cell`) keeps
    // the policy `Sync`, which the async reduction drivers require of `&self`.
    stepped: Mutex<Option<InteractionType>>,
}

impl StepPolicy {
    /// The interaction performed this step, if any.
    pub fn stepped(&self) -> Option<InteractionType> {
        *self.stepped.lock().unwrap()
    }
}

impl ExecPolicy for StepPolicy {
    fn next_step(&self, interaction: InteractionType) {
        let mut slot = self.stepped.lock().unwrap();
        // Record only the first interaction (reduction stops right after).
        slot.get_or_insert(interaction);
    }
    fn should_continue(&self) -> bool {
        // Keep going only until the first interaction fires.
        self.stepped.lock().unwrap().is_none()
    }
}

/// A pending evaluation: its (heap-mutated-in-place) root and progress. The
/// root is `Option` only because the reduction entry points consume and return
/// the pointer; it is `Some` whenever the state is at rest.
pub struct RunState<'h> {
    root: Option<TermPtr<'h>>,
    /// Strong normalization vs weak head normal form, latched at submit.
    pub strong: bool,
    /// Total interaction budget, latched at submit.
    pub budget: u64,
    /// Interactions performed so far (across slices and steps).
    pub steps: u64,
    /// Paused evaluations advance only via [`EvalState::step`] (or resume).
    pub paused: bool,
    /// Recent `(step number, interaction)` pairs, newest last.
    pub history: VecDeque<(u64, InteractionType)>,
}

pub enum EvalState<'h> {
    Idle,
    Running(RunState<'h>),
}

/// What a tick/step did, for the transcript.
pub enum EvalEvent<'h> {
    /// A paused-mode step fired one interaction; the intermediate term is
    /// readable via [`EvalState::root_ptr`].
    Stepped(InteractionType),
    Finished {
        result: TermPtr<'h>,
        steps: u64,
    },
    BudgetExhausted {
        partial: TermPtr<'h>,
        steps: u64,
    },
    Error {
        message: String,
    },
}

impl<'h> EvalState<'h> {
    pub fn start(&mut self, root: TermPtr<'h>, strong: bool, budget: u64, paused: bool) {
        *self = EvalState::Running(RunState {
            root: Some(root),
            strong,
            budget,
            steps: 0,
            paused,
            history: VecDeque::new(),
        });
    }

    pub fn run_state(&self) -> Option<&RunState<'h>> {
        match self {
            EvalState::Idle => None,
            EvalState::Running(run) => Some(run),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, EvalState::Running(_))
    }

    /// Actively reducing (a tick will make progress).
    pub fn is_active(&self) -> bool {
        matches!(self, EvalState::Running(run) if !run.paused)
    }

    /// The pending root (the current intermediate term), for readback and as a
    /// heap-explorer root.
    pub fn root_ptr(&self) -> Option<&TermPtr<'h>> {
        self.run_state().and_then(|run| run.root.as_ref())
    }

    pub fn set_paused(&mut self, paused: bool) {
        if let EvalState::Running(run) = self {
            run.paused = paused;
        }
    }

    /// Run one budget slice of an active evaluation. Returns an event when it
    /// finishes (or exhausts its budget); `None` while still in flight.
    pub fn tick(&mut self, session: &Session<'h>) -> Option<EvalEvent<'h>> {
        let EvalState::Running(run) = self else {
            return None;
        };
        if run.paused {
            return None;
        }
        let slice = SLICE.min(run.budget.saturating_sub(run.steps));
        let root = run.root.take().expect("running eval has a root");
        let (root, policy) = match reduce(session, root, run.strong, FiniteBudget::new(slice)) {
            Ok(result) => result,
            Err(message) => {
                *self = EvalState::Idle;
                return Some(EvalEvent::Error { message });
            }
        };
        let interactions = policy.interactions();
        run.steps += interactions;
        // A slice that stops short of its budget hit (weak head) normal form.
        let finished = interactions < slice;
        let steps = run.steps;
        if finished {
            *self = EvalState::Idle;
            Some(EvalEvent::Finished {
                result: root,
                steps,
            })
        } else if steps >= run.budget {
            *self = EvalState::Idle;
            Some(EvalEvent::BudgetExhausted {
                partial: root,
                steps,
            })
        } else {
            run.root = Some(root);
            None
        }
    }

    /// Perform exactly one interaction of the pending evaluation.
    pub fn step(&mut self, session: &Session<'h>) -> Option<EvalEvent<'h>> {
        let EvalState::Running(run) = self else {
            return None;
        };
        let root = run.root.take().expect("running eval has a root");
        let (root, policy) = match reduce(session, root, run.strong, StepPolicy::default()) {
            Ok(result) => result,
            Err(message) => {
                *self = EvalState::Idle;
                return Some(EvalEvent::Error { message });
            }
        };
        let steps = run.steps;
        match policy.stepped() {
            // No interaction fired: the term was already in normal form.
            None => {
                *self = EvalState::Idle;
                Some(EvalEvent::Finished {
                    result: root,
                    steps,
                })
            }
            Some(interaction) => {
                run.steps += 1;
                run.history.push_back((run.steps, interaction));
                if run.history.len() > HISTORY_CAP {
                    run.history.pop_front();
                }
                let steps = run.steps;
                if steps >= run.budget {
                    *self = EvalState::Idle;
                    Some(EvalEvent::BudgetExhausted {
                        partial: root,
                        steps,
                    })
                } else {
                    run.root = Some(root);
                    Some(EvalEvent::Stepped(interaction))
                }
            }
        }
    }

    /// Cancel the pending evaluation, handing back its current term and step
    /// count. The caller decides whether to erase or keep the partial term.
    pub fn abort(&mut self) -> Option<(TermPtr<'h>, u64)> {
        match std::mem::replace(self, EvalState::Idle) {
            EvalState::Idle => None,
            EvalState::Running(mut run) => {
                Some((run.root.take().expect("running eval has a root"), run.steps))
            }
        }
    }
}

fn reduce<'h, P: ExecPolicy>(
    session: &Session<'h>,
    root: TermPtr<'h>,
    strong: bool,
    policy: P,
) -> Result<(TermPtr<'h>, P), String> {
    let exec = Executor::with_extensions(session.h, policy, &session.extensions);
    let root = if strong {
        session.runtime.block_on(exec.normalize_at(root))
    } else {
        session.runtime.block_on(exec.whnf_at(root))
    };
    if let Some(error) = exec.take_extension_error() {
        exec.erase(session.h.pull(root));
        return Err(error);
    }
    Ok((root, exec.policy))
}

/// Reclaim a term (an aborted partial result, a replaced `last_result`, …).
pub fn erase<'h>(session: &Session<'h>, ptr: TermPtr<'h>) {
    let exec = Executor::with_extensions(session.h, UnlimitedBudget, &session.extensions);
    exec.erase(session.h.pull(ptr));
}

/// Reduce `root` to completion under the session's budget/strength (tests and
/// preload use this; the interactive path goes through [`EvalState`]).
#[cfg(test)]
pub fn run_to_completion<'h>(session: &Session<'h>, root: TermPtr<'h>) -> TermPtr<'h> {
    let (root, _) = reduce(
        session,
        root,
        session.strong,
        FiniteBudget::new(session.budget),
    )
    .expect("test evaluation should not fail");
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{LangMode, Session, SubmitResult};
    use atlas_core::vm::heap::Heap;
    use atlas_core::vm::printer::Printer;

    #[test]
    fn stepper_walks_to_normal_form() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            let root = match session.submit(LangMode::Core, "(\\x -> x + 1) 2") {
                SubmitResult::StartEval { root, .. } => root,
                _ => panic!("expected an evaluation"),
            };
            let mut eval = EvalState::Idle;
            eval.start(root, false, session.budget, true);

            let mut stepped = 0;
            let result = loop {
                match eval.step(&session).expect("eval is running") {
                    EvalEvent::Stepped(_) => {
                        stepped += 1;
                        assert!(eval.root_ptr().is_some());
                    }
                    EvalEvent::Finished { result, steps } => {
                        assert_eq!(steps, stepped);
                        break result;
                    }
                    EvalEvent::BudgetExhausted { .. } => panic!("budget exhausted"),
                    EvalEvent::Error { message } => panic!("evaluation failed: {message}"),
                }
            };
            assert!(stepped > 0, "at least one interaction fires");
            assert_eq!(Printer::new(h).pretty(&result).to_string(), "3");
            erase(&session, result);
        });
    }

    #[test]
    fn tick_slices_run_to_completion() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut session = Session::new(h, 1_000, false);
            let root = match session.submit(LangMode::Core, "(\\x -> x + 1) 2") {
                SubmitResult::StartEval { root, .. } => root,
                _ => panic!("expected an evaluation"),
            };
            let mut eval = EvalState::Idle;
            eval.start(root, false, session.budget, false);
            let event = loop {
                if let Some(event) = eval.tick(&session) {
                    break event;
                }
            };
            match event {
                EvalEvent::Finished { result, steps } => {
                    assert!(steps > 0);
                    assert_eq!(Printer::new(h).pretty(&result).to_string(), "3");
                    erase(&session, result);
                }
                _ => panic!("expected a finish"),
            }
        });
    }
}
