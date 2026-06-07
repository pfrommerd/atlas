//! The async, multi-stack reduction engine.
//!
//! [`Executor::eval_whnf`] / [`Executor::eval_normalize`] return an [`Eval`]
//! future you can `.await` on a tokio runtime. Unlike the synchronous
//! [`Executor::whnf`] (kept for the REPL's per-interaction stepping), the engine
//! does not recurse on the Rust stack to force strict sub-positions. Instead it
//! keeps an explicit set of **fibers** — each a reduction stack of its own — and
//! a ready queue:
//!
//! - forcing a strict position (`Bop` operands, a `Mat` scrutinee, a primitive's
//!   arguments) **forks** child fibers and parks the parent until they finish,
//!   rather than calling [`Executor::whnf`] recursively. Because the two operands
//!   of a `Bop` become *sibling* fibers, `(a + b)` drives `a` and `b`
//!   concurrently;
//! - an async primitive ([`PrimResult::Pending`]) parks its fiber on the future
//!   without blocking the worker;
//! - [`Future::poll`] runs every runnable fiber as far as it can go using only
//!   synchronous interactions, and returns [`Poll::Pending`] only once every
//!   remaining fiber is waiting on async I/O (their inner futures having been
//!   polled with the real waker, so the runtime wakes us).
//!
//! The interaction *rules* are the same `Executor` methods the synchronous
//! driver uses ([`app_lam`](Executor::app_lam), [`dup_head`](Executor::dup_head),
//! [`combine_bop`](Executor::combine_bop), …); only the driving loop differs.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::vm::exec::{ExecPolicy, Executor, Extensions, InteractionType, PrimFuture, PrimResult};
use crate::vm::memory::{NodePtr, PairPtr, TriplePtr};
use crate::vm::term::{MatchId, Node, PrimId, Term};

/// Index of a fiber in [`Eval::fibers`].
type FiberId = usize;

/// What a parked fiber should do once its children (or its async future)
/// complete. While running normally a fiber carries [`Cont::Whnf`].
enum Cont {
    /// Run the reduction loop from the fiber's current `term`.
    Whnf,
    /// Operands of this binary-op triple are now WHNF; combine them.
    Bop(TriplePtr),
    /// The scrutinee of this match (held by application `app`) is now WHNF.
    Mat(MatchId, PairPtr),
    /// Arguments of this primitive (one per collected application) are now WHNF.
    Pri(PrimId, Vec<PairPtr>),
    /// An async primitive produced this node; resume with it.
    AfterAsync(Node),
    /// A normalize fiber's children are normalized; the fiber is finished.
    NormChildren,
}

/// A fiber's lifecycle state. "Active" covers both runnable (in the ready queue)
/// and blocked-on-children (waiting for `pending` to reach zero).
enum FiberState {
    Active,
    /// Parked on an async primitive's future.
    Async(PrimFuture),
    Done,
}

/// One independent reduction stack.
struct Fiber {
    /// Cell this fiber reduces in place; its result is written here on finish.
    slot: NodePtr,
    /// Current head term under reduction.
    term: Node,
    /// Spine of `App`/`Dp0`/`Dp1` continuations (the would-be Rust call stack).
    spine: Vec<Node>,
    /// Resumption action (set when the fiber parks to fork or await).
    cont: Cont,
    /// Parent to notify on completion, if any.
    parent: Option<FiberId>,
    /// Number of child fibers not yet finished.
    pending: usize,
    /// Whether to fully normalize (SNF) after reaching WHNF.
    normalize: bool,
    state: FiberState,
}

/// What a single [`Eval::drive`] of a fiber produced.
enum Step {
    /// The fiber reached its result (WHNF, or SNF for a normalize fiber).
    Done(Node),
    /// The fiber parked (forked children, or awaiting async); state is saved.
    Parked,
}

/// The hand-written future driving a reduction to completion. Awaitable on any
/// tokio runtime; see the [module docs](self).
pub struct Eval<'e, 'h, P: ExecPolicy, X: Extensions> {
    exec: &'e mut Executor<'h, P, X>,
    fibers: Vec<Fiber>,
    /// Fibers ready to run.
    ready: VecDeque<FiberId>,
    /// Fibers parked on an async future, re-polled each outer poll.
    async_parked: Vec<FiberId>,
    /// Set once the root fiber finishes.
    done: Option<Node>,
}

impl<'h, P: ExecPolicy, X: Extensions> Executor<'h, P, X> {
    /// Reduce `node` to weak head normal form, asynchronously.
    pub fn eval_whnf(&mut self, node: Node) -> Eval<'_, 'h, P, X> {
        Eval::new(self, node, false)
    }

    /// Reduce `node` to strong (full) normal form, asynchronously, normalizing
    /// independent sub-terms as concurrent fibers.
    pub fn eval_normalize(&mut self, node: Node) -> Eval<'_, 'h, P, X> {
        Eval::new(self, node, true)
    }
}

impl<'e, 'h, P: ExecPolicy, X: Extensions> Eval<'e, 'h, P, X> {
    fn new(exec: &'e mut Executor<'h, P, X>, node: Node, normalize: bool) -> Self {
        let slot = exec.heap.memory.alloc_cell(node);
        let root = Fiber {
            slot,
            term: node,
            spine: Vec::new(),
            cont: Cont::Whnf,
            parent: None,
            pending: 0,
            normalize,
            state: FiberState::Active,
        };
        Eval {
            exec,
            fibers: vec![root],
            ready: VecDeque::from([0]),
            async_parked: Vec::new(),
            done: None,
        }
    }

    /// Spawn one child fiber per slot, parking `parent` until all finish.
    fn fork(&mut self, parent: FiberId, slots: &[NodePtr], normalize: bool) {
        self.fibers[parent].pending = slots.len();
        for &slot in slots {
            let term = self.exec.heap.node(slot);
            let id = self.fibers.len();
            self.fibers.push(Fiber {
                slot,
                term,
                spine: Vec::new(),
                cont: Cont::Whnf,
                parent: Some(parent),
                pending: 0,
                normalize,
                state: FiberState::Active,
            });
            self.ready.push_back(id);
        }
    }

    /// Park `fid` to fork `slots`, saving its resumption point.
    fn park_fork(
        &mut self,
        fid: FiberId,
        term: Node,
        spine: Vec<Node>,
        cont: Cont,
        slots: &[NodePtr],
        normalize: bool,
    ) -> Step {
        self.fibers[fid].term = term;
        self.fibers[fid].spine = spine;
        self.fibers[fid].cont = cont;
        self.fork(fid, slots, normalize);
        Step::Parked
    }

    /// Record a fiber's result: write it to the fiber's slot and notify its
    /// parent (or set [`Eval::done`] for the root).
    fn complete(&mut self, fid: FiberId, value: Node) {
        self.exec.heap.set(self.fibers[fid].slot, value);
        self.fibers[fid].state = FiberState::Done;
        match self.fibers[fid].parent {
            Some(p) => {
                self.fibers[p].pending -= 1;
                if self.fibers[p].pending == 0 {
                    self.ready.push_back(p);
                }
            }
            None => self.done = Some(value),
        }
    }

    /// A WHNF head has been reached. For a plain fiber that's the result; for a
    /// normalize fiber, fork children for each sub-position (if any).
    fn finish(&mut self, fid: FiberId, term: Node) -> Step {
        if !self.fibers[fid].normalize {
            return Step::Done(term);
        }
        let slots = self.norm_child_slots(term);
        if slots.is_empty() {
            return Step::Done(term);
        }
        self.fibers[fid].term = term;
        self.fibers[fid].cont = Cont::NormChildren;
        self.fork(fid, &slots, true);
        Step::Parked
    }

    /// The sub-position cells a normalize fiber must reduce after WHNF (matches
    /// the synchronous `normalize`'s recursion).
    fn norm_child_slots(&self, term: Node) -> Vec<NodePtr> {
        match term.unpack() {
            Term::Lam(p) => vec![p.second()],
            Term::Use(c) => vec![c],
            Term::App(p) => vec![p.first(), p.second()],
            Term::Sup(t) | Term::Bop(t) => vec![t.second(), t.third()],
            Term::Ctr(c) => {
                let (_, arity) = self.exec.heap.ctr_head(c);
                (0..arity.0).map(|i| c.field(i)).collect()
            }
            _ => vec![],
        }
    }

    /// Run fiber `fid` until it finishes, forks, or parks on async.
    fn drive(&mut self, fid: FiberId) -> Step {
        let mut spine = std::mem::take(&mut self.fibers[fid].spine);
        let mut at_unwind = false;

        // Resume from where the fiber parked (or start fresh).
        let mut term = match std::mem::replace(&mut self.fibers[fid].cont, Cont::Whnf) {
            Cont::Whnf => self.fibers[fid].term,
            Cont::AfterAsync(n) => n,
            Cont::Bop(ptr) => match self.exec.combine_bop(ptr) {
                Some(t) => t,
                None => {
                    at_unwind = true;
                    Term::Bop(ptr).pack()
                }
            },
            Cont::Mat(id, app) => {
                let arg = self.exec.heap.node(app.second());
                match self.exec.app_mat(id, arg) {
                    Some(t) => {
                        spine.pop(); // the application that held the scrutinee
                        self.exec.heap.free_pair(app);
                        t
                    }
                    None => {
                        // no arm matched: the `(mat arg)` application is stuck.
                        at_unwind = true;
                        Term::Mat(id).pack()
                    }
                }
            }
            Cont::Pri(id, apps) => {
                let args: Vec<Node> = apps.iter().map(|a| self.exec.heap.node(a.second())).collect();
                self.exec.policy.next_step(InteractionType::AppPri);
                match self.exec.extensions.apply(self.exec.heap, id, &args) {
                    PrimResult::Done(t) => {
                        for a in &apps {
                            self.exec.heap.free_pair(*a);
                        }
                        t
                    }
                    PrimResult::Pending(fut) => {
                        // Park on the future; the async result (a leaf node) does
                        // not depend on the args, so reclaim them now.
                        for a in &apps {
                            self.exec.heap.free_pair(*a);
                        }
                        for arg in args {
                            self.exec.erase(arg);
                        }
                        self.fibers[fid].spine = spine;
                        self.fibers[fid].state = FiberState::Async(fut);
                        self.async_parked.push(fid);
                        return Step::Parked;
                    }
                }
            }
            Cont::NormChildren => return Step::Done(self.fibers[fid].term),
        };

        'red: loop {
            if at_unwind {
                // Unwind the spine. A DUP continuation here duplicates the (now
                // value/stuck) head via `dup_head` and resumes reduction; an APP
                // continuation rebuilds the stuck application and keeps unwinding.
                match spine.pop() {
                    None => return self.finish(fid, term),
                    Some(cont) => match cont.unpack() {
                        Term::Dp0(q) => {
                            let label = self.exec.heap.dup_label(q);
                            term = self.exec.dup_head(true, label, q, term);
                            at_unwind = false;
                        }
                        Term::Dp1(q) => {
                            let label = self.exec.heap.dup_label(q);
                            term = self.exec.dup_head(false, label, q, term);
                            at_unwind = false;
                        }
                        Term::App(app) => {
                            self.exec.heap.set(app.first(), term);
                            term = Term::App(app).pack();
                        }
                        _ => unreachable!("non-spine continuation"),
                    },
                }
                continue 'red;
            }

            if !self.exec.policy.should_continue() {
                // Budget spent: rebuild the spine without further interactions.
                while let Some(cont) = spine.pop() {
                    let slot = match cont.unpack() {
                        Term::App(p) => p.first(),
                        Term::Dp0(q) | Term::Dp1(q) => q.val(),
                        _ => unreachable!("non-spine continuation"),
                    };
                    self.exec.heap.set(slot, term);
                    term = cont;
                }
                return self.finish(fid, term);
            }

            match term.unpack() {
                Term::Var(slot) => {
                    if let Term::Sub(n) = self.exec.heap.node(slot).unpack() {
                        // binder consumed; reclaim its (now dead) lambda node.
                        self.exec.heap.free_pair(PairPtr(slot.0));
                        term = n;
                        continue 'red;
                    }
                    // free variable: unwind (a DUP cont applies DUP-VAR).
                    at_unwind = true;
                }
                Term::Dp0(q) => {
                    if let Term::Sub(n) = self.exec.heap.node(q.sub0()).unpack() {
                        self.exec.heap.free_dup(q);
                        term = n;
                        continue 'red;
                    }
                    spine.push(term);
                    term = self.exec.heap.node(q.val());
                    continue 'red;
                }
                Term::Dp1(q) => {
                    if let Term::Sub(n) = self.exec.heap.node(q.sub1()).unpack() {
                        self.exec.heap.free_dup(q);
                        term = n;
                        continue 'red;
                    }
                    spine.push(term);
                    term = self.exec.heap.node(q.val());
                    continue 'red;
                }
                Term::App(p) => {
                    spine.push(term);
                    term = self.exec.heap.node(p.first());
                    continue 'red;
                }
                // Strict fork points: reduce sub-terms as concurrent children.
                Term::Bop(ptr) => {
                    return self.park_fork(
                        fid,
                        term,
                        spine,
                        Cont::Bop(ptr),
                        &[ptr.second(), ptr.third()],
                        false,
                    );
                }
                Term::Lam(lam) => {
                    if let Some(Term::App(app)) = spine.last().map(|c| c.unpack()) {
                        spine.pop();
                        term = self.exec.app_lam(app, lam);
                        continue 'red;
                    }
                    at_unwind = true;
                }
                Term::Use(v) => {
                    if let Some(Term::App(app)) = spine.last().map(|c| c.unpack()) {
                        spine.pop();
                        term = self.exec.app_use(app, v);
                        continue 'red;
                    }
                    at_unwind = true;
                }
                Term::Sup(sup) => {
                    if let Some(Term::App(app)) = spine.last().map(|c| c.unpack()) {
                        spine.pop();
                        let slab = self.exec.heap.sup_label(sup);
                        term = self.exec.app_sup(app, slab, sup);
                        continue 'red;
                    }
                    at_unwind = true;
                }
                Term::Mat(id) => {
                    match spine.last().map(|c| c.unpack()) {
                        Some(Term::App(app)) => {
                            // force the scrutinee, then match on resume.
                            return self.park_fork(
                                fid,
                                term,
                                spine,
                                Cont::Mat(id, app),
                                &[app.second()],
                                false,
                            );
                        }
                        // duplicating a match value: share it to both sides.
                        Some(Term::Dp0(q)) | Some(Term::Dp1(q)) => {
                            spine.pop();
                            self.exec.subst(q.sub0(), term);
                            self.exec.subst(q.sub1(), term);
                            continue 'red;
                        }
                        _ => at_unwind = true,
                    }
                }
                Term::Wld => {
                    if let Some(Term::App(app)) = spine.last().map(|c| c.unpack()) {
                        spine.pop();
                        // (* a) => *, erasing the argument.
                        let arg = self.exec.heap.node(app.second());
                        self.exec.erase(arg);
                        self.exec.heap.free_pair(app);
                        term = self.exec.heap.wld();
                        continue 'red;
                    }
                    at_unwind = true;
                }
                Term::Era => {
                    if let Some(Term::App(app)) = spine.last().map(|c| c.unpack()) {
                        spine.pop();
                        term = self.exec.app_era(app);
                        continue 'red;
                    }
                    at_unwind = true;
                }
                Term::Pri(id) => {
                    let arity = self.exec.extensions.arity(id);
                    let n = spine.len();
                    let ready = arity <= n
                        && spine[n - arity..]
                            .iter()
                            .all(|c| matches!(c.unpack(), Term::App(_)));
                    if ready {
                        // collect the applications (innermost first = arg order)
                        // and fork a fiber to reduce each argument concurrently.
                        let mut apps = Vec::with_capacity(arity);
                        for _ in 0..arity {
                            let Term::App(app) = spine.pop().unwrap().unpack() else {
                                unreachable!("checked all-App above")
                            };
                            apps.push(app);
                        }
                        let slots: Vec<NodePtr> = apps.iter().map(|a| a.second()).collect();
                        return self.park_fork(fid, term, spine, Cont::Pri(id, apps), &slots, false);
                    }
                    // under-applied: an inert value — unwind (a DUP applies DUP-PRI).
                    at_unwind = true;
                }
                // numbers, constructors, and anything else are values/stuck:
                // unwind (a DUP continuation duplicates them via `dup_head`).
                _ => at_unwind = true,
            }
        }
    }
}

impl<P: ExecPolicy, X: Extensions> Future for Eval<'_, '_, P, X> {
    type Output = Node;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Node> {
        let this = self.get_mut();
        loop {
            // Drive every runnable fiber as far as it can go synchronously.
            while let Some(fid) = this.ready.pop_front() {
                if matches!(this.fibers[fid].state, FiberState::Done) {
                    continue;
                }
                match this.drive(fid) {
                    Step::Done(value) => this.complete(fid, value),
                    Step::Parked => {}
                }
            }

            if let Some(value) = this.done {
                return Poll::Ready(value);
            }

            // Poll each async-parked fiber's future with the real waker. Any that
            // resolves becomes runnable; if none does, we've exhausted all
            // synchronous progress and yield.
            let mut progressed = false;
            for fid in std::mem::take(&mut this.async_parked) {
                let FiberState::Async(mut fut) =
                    std::mem::replace(&mut this.fibers[fid].state, FiberState::Active)
                else {
                    continue;
                };
                match fut.as_mut().poll(cx) {
                    Poll::Ready(node) => {
                        this.fibers[fid].cont = Cont::AfterAsync(node);
                        this.ready.push_back(fid);
                        progressed = true;
                    }
                    Poll::Pending => {
                        this.fibers[fid].state = FiberState::Async(fut);
                        this.async_parked.push(fid);
                    }
                }
            }

            if !progressed {
                // Every live fiber is parked on async I/O (or we're done):
                // the inner futures registered our waker, so return Pending.
                return Poll::Pending;
            }
        }
    }
}
