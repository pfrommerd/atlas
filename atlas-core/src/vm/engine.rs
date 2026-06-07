//! The async, multi-stack reduction engine.
//!
//! [`Executor::eval_whnf`] / [`Executor::eval_normalize`] return an [`Eval`]
//! future you can `.await` on a tokio runtime. The engine is the *only* driver:
//! it does not recurse on the Rust stack to force strict sub-positions, but
//! keeps an explicit set of **fibers** — each a reduction stack of its own — and
//! a ready queue:
//!
//! - forcing a strict position (`Bop` operands, a `Mat` scrutinee, a primitive's
//!   arguments) **forks** child fibers and parks the parent until they finish,
//!   rather than recursing. Because the two operands of a `Bop` become *sibling*
//!   fibers, `(a + b)` drives `a` and `b` concurrently;
//! - an async primitive ([`PrimResult::Pending`]) parks its fiber on the future
//!   without blocking the worker;
//! - [`Future::poll`] runs every runnable fiber as far as it can go using only
//!   synchronous interactions, and returns [`Poll::Pending`] only once every
//!   remaining fiber is waiting on async I/O (their inner futures having been
//!   polled with the real waker, so the runtime wakes us).
//!
//! The interaction *rules* live on the [`Executor`]: the non-suspendable ones
//! ([`app_lam`](Executor::app_lam), [`dup_head`](Executor::dup_head),
//! [`combine_bop`](Executor::combine_bop), …) return a [`Node`], while the
//! suspendable ones ([`bop`](Executor::bop), [`mat`](Executor::mat),
//! [`pri`](Executor::pri)) take the whole [`Eval`] so they can fork and park.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::vm::exec::{ExecPolicy, Executor, Extensions, InteractionType, PrimFuture, PrimResult};
use crate::vm::heap::Heap;
use crate::vm::memory::{LOCKED, NodePtr, PairPtr, TriplePtr};
use crate::vm::term::{MatchId, Node, PrimId, Term};

/// The sub-position cells to normalize after a node reaches WHNF (the same set
/// the in-task normalize fiber and the parallel driver both recurse into).
fn norm_child_slots(heap: &Heap, term: Node) -> Vec<NodePtr> {
    match term.unpack() {
        Term::Lam(p) => vec![p.second()],
        Term::Use(c) => vec![c],
        Term::App(p) => vec![p.first(), p.second()],
        Term::Sup(t) | Term::Bop(t) => vec![t.second(), t.third()],
        Term::Ctr(c) => {
            let (_, arity) = heap.ctr_head(c);
            (0..arity.0).map(|i| c.field(i)).collect()
        }
        _ => vec![],
    }
}

/// Fully normalize the node at `slot` **in place**, spawning a tokio task per
/// independent sub-term so subtrees normalize across worker threads.
///
/// The shared [`Heap`], policy, and extensions are reached through `Arc`s (so
/// each task's future is `Send + 'static`); every cell mutation goes through the
/// atomic heap. Cross-task sharing — e.g. a DUP forced by two sibling subtrees —
/// is mediated by the lock-free DUP-claim in [`Eval`]: one task reduces the
/// shared value, the other observes the claim and retries.
pub fn par_normalize<P, X>(
    heap: Arc<Heap>,
    policy: Arc<P>,
    ext: Arc<X>,
    slot: NodePtr,
) -> Pin<Box<dyn Future<Output = ()> + Send>>
where
    P: ExecPolicy + Send + Sync + 'static,
    X: Extensions + Send + Sync + 'static,
{
    Box::pin(async move {
        // 1. reduce this node to WHNF (async: drives primitives and intra-WHNF
        //    concurrency), writing the result back into `slot`.
        let node = heap.node(slot);
        let whnf = {
            let exec = Executor::with_extensions(&heap, &*policy, &*ext);
            exec.eval_whnf(node).await
        };
        heap.set(slot, whnf);
        // 2. normalize each independent sub-term as its own task.
        let mut handles = Vec::new();
        for child in norm_child_slots(&heap, whnf) {
            handles.push(tokio::spawn(par_normalize(
                heap.clone(),
                policy.clone(),
                ext.clone(),
                child,
            )));
        }
        for h in handles {
            let _ = h.await;
        }
    })
}

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
    /// The fiber parked (forked children, or awaiting async); state is saved and
    /// the fiber will be re-queued by whatever it is waiting on.
    Parked,
    /// The fiber lost a DUP claim (another fiber is reducing the shared value);
    /// state is saved and it must be retried once the holder makes progress.
    Blocked,
}

/// What a suspendable interaction handler ([`Executor::bop`], [`Executor::mat`],
/// [`Executor::pri`]) tells the reduction loop to do next. The handler has
/// already mutated the fiber (forking children, parking on async, …) as needed.
enum Flow {
    /// Continue the reduction loop with this head term.
    Head(Node),
    /// The head is a value or stuck; unwind the spine carrying this node.
    Stuck(Node),
    /// The fiber parked (forked children, or awaiting async) — stop driving it.
    Park,
}

/// The hand-written future driving a reduction to completion. Awaitable on any
/// tokio runtime; see the [module docs](self).
pub struct Eval<'e, 'h, P: ExecPolicy, X: Extensions> {
    exec: &'e Executor<'h, P, X>,
    fibers: Vec<Fiber>,
    /// Fibers ready to run.
    ready: VecDeque<FiberId>,
    /// Fibers parked on an async future, re-polled each outer poll.
    async_parked: Vec<FiberId>,
    /// Fibers that lost a DUP claim, retried once the holder makes progress.
    blocked: Vec<FiberId>,
    /// Set once the root fiber finishes.
    done: Option<Node>,
}

impl<'h, P: ExecPolicy, X: Extensions> Executor<'h, P, X> {
    /// Reduce `node` to weak head normal form, asynchronously.
    pub fn eval_whnf(&self, node: Node) -> Eval<'_, 'h, P, X> {
        Eval::new(self, node, false)
    }

    /// Reduce `node` to strong (full) normal form, asynchronously, normalizing
    /// independent sub-terms as concurrent fibers.
    pub fn eval_normalize(&self, node: Node) -> Eval<'_, 'h, P, X> {
        Eval::new(self, node, true)
    }
}

impl<'e, 'h, P: ExecPolicy, X: Extensions> Eval<'e, 'h, P, X> {
    fn new(exec: &'e Executor<'h, P, X>, node: Node, normalize: bool) -> Self {
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
            blocked: Vec::new(),
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
        let slots = norm_child_slots(self.exec.heap, term);
        if slots.is_empty() {
            return Step::Done(term);
        }
        self.fibers[fid].term = term;
        self.fibers[fid].cont = Cont::NormChildren;
        self.fork(fid, &slots, true);
        Step::Parked
    }

    /// Run fiber `fid` until it finishes, forks, or parks on async.
    ///
    /// The fiber's spine lives in `self.fibers[fid].spine` and is manipulated in
    /// place, so a suspendable interaction handler ([`Executor::bop`] et al.) can
    /// fork or park the fiber and have its saved state survive across polls. The
    /// loop mirrors a textbook head-reduction: descend the spine, fire the rule
    /// at the head, and unwind once the head is a value or stuck.
    fn drive(&mut self, fid: FiberId) -> Step {
        let exec = self.exec;

        // Translate a suspendable handler's `Flow` into loop state, returning
        // early when the fiber parked or lost a DUP claim.
        macro_rules! flow {
            ($term:ident, $unwind:ident, $e:expr) => {
                match $e {
                    Flow::Head(t) => {
                        $term = t;
                        $unwind = false;
                    }
                    Flow::Stuck(t) => {
                        $term = t;
                        $unwind = true;
                    }
                    Flow::Park => return Step::Parked,
                }
            };
        }

        // Resume from where the fiber parked (or start fresh): a saved
        // continuation re-enters its own handler, which combines the now-WHNF
        // children (`Whnf`/`AfterAsync` just hand back the head term).
        let resume = match std::mem::replace(&mut self.fibers[fid].cont, Cont::Whnf) {
            Cont::Whnf => Flow::Head(self.fibers[fid].term),
            Cont::AfterAsync(n) => Flow::Head(n),
            Cont::Bop(ptr) => exec.bop(self, fid, ptr, true),
            Cont::Mat(id, app) => exec.mat(self, fid, id, app, true),
            Cont::Pri(id, apps) => exec.pri(self, fid, id, Some(apps)),
            Cont::NormChildren => return Step::Done(self.fibers[fid].term),
        };
        let mut term: Node;
        let mut at_unwind: bool;
        flow!(term, at_unwind, resume);

        loop {
            if at_unwind {
                // Unwind the spine. A DUP continuation here duplicates the (now
                // value/stuck) head via `dup_head` and resumes reduction; an APP
                // continuation rebuilds the stuck application and keeps unwinding.
                match self.fibers[fid].spine.pop() {
                    None => return self.finish(fid, term),
                    Some(cont) => match cont.unpack() {
                        Term::Dp0(q) => {
                            let label = exec.heap.dup_label(q);
                            term = exec.dup_head(true, label, q, term);
                            at_unwind = false;
                        }
                        Term::Dp1(q) => {
                            let label = exec.heap.dup_label(q);
                            term = exec.dup_head(false, label, q, term);
                            at_unwind = false;
                        }
                        Term::App(app) => {
                            exec.heap.set(app.first(), term);
                            term = Term::App(app).pack();
                        }
                        _ => unreachable!("non-spine continuation"),
                    },
                }
                continue;
            }

            if !exec.policy.should_continue() {
                // Budget spent: rebuild the spine without further interactions.
                while let Some(cont) = self.fibers[fid].spine.pop() {
                    let slot = match cont.unpack() {
                        Term::App(p) => p.first(),
                        Term::Dp0(q) | Term::Dp1(q) => q.val(),
                        _ => unreachable!("non-spine continuation"),
                    };
                    exec.heap.set(slot, term);
                    term = cont;
                }
                return self.finish(fid, term);
            }

            match term.unpack() {
                Term::Var(slot) => {
                    if let Term::Sub(n) = exec.heap.node(slot).unpack() {
                        // binder consumed; reclaim its (now dead) lambda node.
                        exec.heap.free_pair(PairPtr(slot.0));
                        term = n;
                        continue;
                    }
                    // free variable: unwind (a DUP cont applies DUP-VAR).
                    at_unwind = true;
                }
                Term::Dp0(q) => {
                    if let Term::Sub(n) = exec.heap.node(q.sub0()).unpack() {
                        exec.heap.free_dup(q);
                        term = n;
                        continue;
                    }
                    // Claim the shared value so only one fiber reduces it.
                    let val = exec.heap.take(q.val());
                    if val.raw() == LOCKED {
                        // another fiber holds the claim: use its result if it has
                        // fired, otherwise save the head and retry (DUP-claim).
                        if let Term::Sub(n) = exec.heap.node(q.sub0()).unpack() {
                            exec.heap.free_dup(q);
                            term = n;
                            continue;
                        }
                        self.fibers[fid].term = term;
                        return Step::Blocked;
                    }
                    // won the claim: reduce the taken value, fire DUP on unwind.
                    self.fibers[fid].spine.push(term);
                    term = val;
                    continue;
                }
                Term::Dp1(q) => {
                    if let Term::Sub(n) = exec.heap.node(q.sub1()).unpack() {
                        exec.heap.free_dup(q);
                        term = n;
                        continue;
                    }
                    let val = exec.heap.take(q.val());
                    if val.raw() == LOCKED {
                        if let Term::Sub(n) = exec.heap.node(q.sub1()).unpack() {
                            exec.heap.free_dup(q);
                            term = n;
                            continue;
                        }
                        self.fibers[fid].term = term;
                        return Step::Blocked;
                    }
                    self.fibers[fid].spine.push(term);
                    term = val;
                    continue;
                }
                Term::App(p) => {
                    self.fibers[fid].spine.push(term);
                    term = exec.heap.node(p.first());
                    continue;
                }
                // Strict fork point: reduce both operands as concurrent children.
                Term::Bop(ptr) => {
                    flow!(term, at_unwind, exec.bop(self, fid, ptr, false));
                    continue;
                }
                Term::Lam(lam) => {
                    if let Some(Term::App(app)) = self.fibers[fid].spine.last().map(|c| c.unpack())
                    {
                        self.fibers[fid].spine.pop();
                        term = exec.app_lam(app, lam);
                        continue;
                    }
                    at_unwind = true;
                }
                Term::Use(v) => {
                    if let Some(Term::App(app)) = self.fibers[fid].spine.last().map(|c| c.unpack())
                    {
                        self.fibers[fid].spine.pop();
                        term = exec.app_use(app, v);
                        continue;
                    }
                    at_unwind = true;
                }
                Term::Sup(sup) => {
                    if let Some(Term::App(app)) = self.fibers[fid].spine.last().map(|c| c.unpack())
                    {
                        self.fibers[fid].spine.pop();
                        let slab = exec.heap.sup_label(sup);
                        term = exec.app_sup(app, slab, sup);
                        continue;
                    }
                    at_unwind = true;
                }
                Term::Mat(id) => match self.fibers[fid].spine.last().map(|c| c.unpack()) {
                    // force the scrutinee, then match on resume.
                    Some(Term::App(app)) => {
                        flow!(term, at_unwind, exec.mat(self, fid, id, app, false));
                        continue;
                    }
                    // duplicating a match value: share it to both sides.
                    Some(Term::Dp0(q)) | Some(Term::Dp1(q)) => {
                        self.fibers[fid].spine.pop();
                        exec.subst(q.sub0(), term);
                        exec.subst(q.sub1(), term);
                        continue;
                    }
                    _ => at_unwind = true,
                },
                Term::Wld => {
                    if let Some(Term::App(app)) = self.fibers[fid].spine.last().map(|c| c.unpack())
                    {
                        self.fibers[fid].spine.pop();
                        // (* a) => *, erasing the argument.
                        let arg = exec.heap.node(app.second());
                        exec.erase(arg);
                        exec.heap.free_pair(app);
                        term = exec.heap.wld();
                        continue;
                    }
                    at_unwind = true;
                }
                Term::Era => {
                    if let Some(Term::App(app)) = self.fibers[fid].spine.last().map(|c| c.unpack())
                    {
                        self.fibers[fid].spine.pop();
                        term = exec.app_era(app);
                        continue;
                    }
                    at_unwind = true;
                }
                Term::Pri(id) => {
                    // A primitive fires once its top `arity` continuations are all
                    // applications; otherwise it is an inert value (unwind).
                    let arity = exec.extensions.arity(id);
                    let spine = &self.fibers[fid].spine;
                    let n = spine.len();
                    let ready = arity <= n
                        && spine[n - arity..]
                            .iter()
                            .all(|c| matches!(c.unpack(), Term::App(_)));
                    if ready {
                        flow!(term, at_unwind, exec.pri(self, fid, id, None));
                        continue;
                    }
                    at_unwind = true;
                }
                // numbers, constructors, and anything else are values/stuck:
                // unwind (a DUP continuation duplicates them via `dup_head`).
                _ => at_unwind = true,
            }
        }
    }
}

/// The suspendable interaction rules: a binary op, a match, and a primitive
/// application each must first force sub-terms. Rather than recurse, each takes
/// the whole [`Eval`] and either **forks** child fibers and parks itself (on
/// first encounter) or, once those children are WHNF, **resumes** and fires the
/// rule. Keeping them on [`Executor`] (beside the non-suspendable rules) leaves
/// [`Eval::drive`] a thin dispatcher.
impl<'h, P: ExecPolicy, X: Extensions> Executor<'h, P, X> {
    /// Binary op. On entry (`resuming == false`) forks both operands as sibling
    /// fibers and parks on [`Cont::Bop`]; on resume the operands are WHNF, so it
    /// combines them. A spent budget leaves `(lhs OP rhs)` stuck rather than
    /// charging the op without the policy's consent — this is what lets a
    /// single-step policy stop *between* operands.
    fn bop(
        &self,
        eval: &mut Eval<'_, 'h, P, X>,
        fid: FiberId,
        ptr: TriplePtr,
        resuming: bool,
    ) -> Flow {
        if !resuming {
            eval.fibers[fid].cont = Cont::Bop(ptr);
            eval.fork(fid, &[ptr.second(), ptr.third()], false);
            return Flow::Park;
        }
        if !self.policy.should_continue() {
            return Flow::Stuck(Term::Bop(ptr).pack());
        }
        match self.combine_bop(ptr) {
            Some(t) => Flow::Head(t),
            None => Flow::Stuck(Term::Bop(ptr).pack()),
        }
    }

    /// Match. On entry forks the scrutinee (held by application `app`, left on
    /// the spine) and parks on [`Cont::Mat`]; on resume the scrutinee is WHNF, so
    /// it applies the match — popping and freeing the application — or, if no arm
    /// matches (or the budget is spent), leaves the `(mat arg)` application stuck.
    fn mat(
        &self,
        eval: &mut Eval<'_, 'h, P, X>,
        fid: FiberId,
        id: MatchId,
        app: PairPtr,
        resuming: bool,
    ) -> Flow {
        if !resuming {
            eval.fibers[fid].cont = Cont::Mat(id, app);
            eval.fork(fid, &[app.second()], false);
            return Flow::Park;
        }
        if !self.policy.should_continue() {
            return Flow::Stuck(Term::Mat(id).pack());
        }
        let arg = self.heap.node(app.second());
        match self.app_mat(id, arg) {
            Some(t) => {
                eval.fibers[fid].spine.pop(); // the application that held the scrutinee
                self.heap.free_pair(app);
                Flow::Head(t)
            }
            None => Flow::Stuck(Term::Mat(id).pack()),
        }
    }

    /// Primitive application. On entry (`resume == None`) pops the top `arity`
    /// applications off the spine, forks a fiber per argument, and parks on
    /// [`Cont::Pri`]; on resume (`resume == Some(apps)`) the arguments are WHNF,
    /// so it runs the primitive — synchronously ([`PrimResult::Done`]) or by
    /// parking the fiber on the primitive's future ([`PrimResult::Pending`]). A
    /// spent budget rebuilds the application spine and leaves the prim inert.
    fn pri(
        &self,
        eval: &mut Eval<'_, 'h, P, X>,
        fid: FiberId,
        id: PrimId,
        resume: Option<Vec<PairPtr>>,
    ) -> Flow {
        let apps = match resume {
            None => {
                // collect the applications (innermost first = arg order) and fork
                // a fiber to reduce each argument concurrently.
                let arity = self.extensions.arity(id);
                let spine = &mut eval.fibers[fid].spine;
                let mut apps = Vec::with_capacity(arity);
                for _ in 0..arity {
                    let Term::App(app) = spine.pop().unwrap().unpack() else {
                        unreachable!("caller checked the top `arity` entries are applications")
                    };
                    apps.push(app);
                }
                let slots: Vec<NodePtr> = apps.iter().map(|a| a.second()).collect();
                eval.fibers[fid].cont = Cont::Pri(id, apps);
                eval.fork(fid, &slots, false);
                return Flow::Park;
            }
            Some(apps) => apps,
        };
        if !self.policy.should_continue() {
            // budget spent: rebuild the application spine, leave the prim inert.
            let spine = &mut eval.fibers[fid].spine;
            for app in apps.into_iter().rev() {
                spine.push(Term::App(app).pack());
            }
            return Flow::Stuck(Term::Pri(id).pack());
        }
        let args: Vec<Node> = apps.iter().map(|a| self.heap.node(a.second())).collect();
        self.policy.next_step(InteractionType::AppPri);
        match self.extensions.apply(self.heap, id, &args) {
            PrimResult::Done(t) => {
                for a in &apps {
                    self.heap.free_pair(*a);
                }
                Flow::Head(t)
            }
            PrimResult::Pending(fut) => {
                // The async result (a leaf node) does not depend on the args, so
                // reclaim them now.
                for a in &apps {
                    self.heap.free_pair(*a);
                }
                for arg in args {
                    self.erase(arg);
                }
                eval.fibers[fid].state = FiberState::Async(fut);
                eval.async_parked.push(fid);
                Flow::Park
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
            // `advanced` tracks *real* progress (completions, forks, async
            // parks) — a fiber merely re-blocking on a DUP claim is not progress.
            let mut advanced = false;
            while let Some(fid) = this.ready.pop_front() {
                if matches!(this.fibers[fid].state, FiberState::Done) {
                    continue;
                }
                match this.drive(fid) {
                    Step::Done(value) => {
                        this.complete(fid, value);
                        advanced = true;
                    }
                    Step::Parked => advanced = true,
                    Step::Blocked => this.blocked.push(fid),
                }
            }

            if let Some(value) = this.done {
                return Poll::Ready(value);
            }

            // Poll each async-parked fiber's future with the real waker. Any that
            // resolves becomes runnable.
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

            // Retry DUP-claim losers. If real progress was made this round, the
            // claim holder may have advanced/fired, so retry immediately.
            if !this.blocked.is_empty() {
                let blocked = std::mem::take(&mut this.blocked);
                this.ready.extend(blocked);
                if !advanced && !progressed {
                    // No local progress: the claim holder is elsewhere (another
                    // task) or waiting on I/O. If an async future holds our waker
                    // it will re-poll us; otherwise yield so the runtime can run
                    // the holder, then re-poll.
                    if this.async_parked.is_empty() {
                        cx.waker().wake_by_ref();
                    }
                    return Poll::Pending;
                }
                continue;
            }

            if !progressed {
                // Every live fiber is parked on async I/O (or we're done):
                // the inner futures registered our waker, so return Pending.
                return Poll::Pending;
            }
        }
    }
}
