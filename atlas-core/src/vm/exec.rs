//! The [`Executor`]: interaction-calculus evaluation over a branded [`HeapScope`].
//!
//! v1 is a synchronous, single-task evaluator over the affine heap model. It
//! covers the affine core (APP-LAM / APP-USE / APP-ERA, binary ops, constructors
//! as data, and full normalization). The duplication / superposition / match
//! interactions and the parallel async driver are deferred to a later increment.

use crate::extension::{Extensions, Handle, NoExtensions, TermPtrLike};
use crate::vm::heap::{
    DupPtr, HeapScope, MatchPtr, PackPtr, PatKey, Spine, SupPtr, TermPtr, TermSlot,
};
use crate::vm::term::{BinaryOp, LabelId, Term};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

/// A boxed reduction future. Boxed so the (mutually) recursive async reduction
/// methods can call one another; the parallel driver will later add a `Send`
/// bound here.
type Reduce<'s, T> = Pin<Box<dyn Future<Output = T> + 's>>;

/// The kind of interaction performed in a single reduction step.
#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionType {
    AppLam, AppUse, AppEra, AppSup, AppMat, AppPri,
    DupLam, DupSup, DupCtr, DupApp, DupBop, DupNum, DupWld, DupVar, DupUse, DupPri, DupVal,
    BopVal, BopSup,
}

/// Controls how an [`Executor`] accounts for reduction steps and decides when to
/// stop. Taken through `&self` (atomics) so it can be shared.
pub trait ExecPolicy {
    fn next_step(&self, interaction: InteractionType);
    fn should_continue(&self) -> bool;
}

/// A policy that never limits reduction.
pub struct UnlimitedBudget;

impl ExecPolicy for UnlimitedBudget {
    #[inline(always)]
    fn next_step(&self, _: InteractionType) {}
    #[inline(always)]
    fn should_continue(&self) -> bool {
        true
    }
}

/// A policy that stops after a fixed number of interactions.
pub struct FiniteBudget {
    itrs: AtomicU64,
    budget: u64,
}

impl FiniteBudget {
    pub fn new(budget: u64) -> Self {
        FiniteBudget {
            itrs: AtomicU64::new(0),
            budget,
        }
    }
    pub fn interactions(&self) -> u64 {
        self.itrs.load(Ordering::Relaxed)
    }
}

impl ExecPolicy for FiniteBudget {
    #[inline]
    fn next_step(&self, _: InteractionType) {
        self.itrs.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    fn should_continue(&self) -> bool {
        self.itrs.load(Ordering::Relaxed) < self.budget
    }
}

const NO_EXTENSIONS: &NoExtensions = &NoExtensions;

/// Drives reduction over a branded [`HeapScope`]. The scope borrow's lifetime is
/// tied to the brand (`&'h HeapScope<'h>`), so [`Handle`]s minted for extensions
/// carry a single lifetime; the extension set is borrowed separately for `'e`.
pub struct Executor<'e, 'h, P: ExecPolicy, X: Extensions = NoExtensions> {
    pub heap: &'h HeapScope<'h>,
    pub extensions: &'e X,
    pub policy: P,
}

impl<'e, 'h, P: ExecPolicy> Executor<'e, 'h, P, NoExtensions> {
    pub fn new(heap: &'h HeapScope<'h>, policy: P) -> Self {
        Executor {
            heap,
            extensions: NO_EXTENSIONS,
            policy,
        }
    }
}

impl<'e, 'h, P: ExecPolicy, X: Extensions> Executor<'e, 'h, P, X> {
    pub fn with_extensions(heap: &'h HeapScope<'h>, policy: P, extensions: &'e X) -> Self {
        Executor {
            heap,
            extensions,
            policy,
        }
    }

    // ====================================================================
    // Erase: recursively reclaim a term and everything reachable from it.
    // ====================================================================

    pub fn erase(&self, term: Term<'h>) {
        match term {
            Term::App { func, arg }
            | Term::And {
                lhs: func,
                rhs: arg,
            }
            | Term::Or {
                lhs: func,
                rhs: arg,
            } => {
                self.erase(self.heap.pull(func));
                self.erase(self.heap.pull(arg));
            }
            Term::Bop { lhs, rhs, .. } => {
                self.erase(self.heap.pull(lhs));
                self.erase(self.heap.pull(rhs));
            }
            Term::Lam { body } => {
                // The binder slot is owned by the body's variable occurrence, so
                // erasing the body reclaims it exactly once.
                self.erase(self.heap.pull(self.heap.into_body(body)));
            }
            Term::Use { body } => {
                self.erase(self.heap.pull(body));
            }
            Term::Ctr { arity, values, .. } => {
                for f in self
                    .heap
                    .into_fields(values)
                    .into_iter()
                    .take(arity as usize)
                {
                    self.erase(self.heap.pull(f));
                }
            }
            Term::Box(v) => self.heap.value_drop(v),
            // Leaves and (v1-)inert heads.
            Term::Var
            | Term::Wld
            | Term::Err { .. }
            | Term::U64(_)
            | Term::I64(_)
            | Term::F32(_)
            | Term::F64(_)
            | Term::Char(_)
            | Term::Bool(_)
            | Term::Pri(_)
            | Term::Null => {}
            // Deferred-interaction heads: leave their cells (v1 does not produce
            // them through reduction).
            Term::Sup { .. } | Term::Dup { .. } | Term::Mat { .. } => {}
        }
    }

    /// Erase a single term named by a [`TermPtr`] or [`Handle`] (consuming it).
    pub async fn erase_handle(&self, h: impl TermPtrLike<'h>) {
        self.erase(self.heap.pull(h.into_ptr()));
    }

    /// Reclaim every node whose owning [`Handle`] was dropped without being
    /// consumed. Drains until empty, since erasing one term may itself drop
    /// further handles.
    pub async fn erase_dropped_handles(&self) {
        loop {
            let batch = self.heap.take_dropped();
            if batch.is_empty() {
                break;
            }
            for ptr in batch {
                self.erase(self.heap.pull(ptr));
            }
        }
    }

    // ====================================================================
    // WHNF
    // ====================================================================

    /// Reduce `x` (a [`TermPtr`] or [`Handle`]) to weak head normal form,
    /// returning the same kind of pointer naming the result node.
    pub async fn whnf_at<T: TermPtrLike<'h>>(&self, x: T) -> T {
        let r = self.whnf_at_ptr(x.into_ptr()).await;
        T::from_ptr(r, self.heap)
    }

    /// Reduce `x` (a [`TermPtr`] or [`Handle`]) to full normal form, returning the
    /// same kind of pointer naming the result node.
    pub async fn normalize_at<T: TermPtrLike<'h>>(&self, x: T) -> T {
        let r = self.normalize_at_ptr(x.into_ptr()).await;
        T::from_ptr(r, self.heap)
    }

    /// The boxed form of [`whnf_at_ptr`](Self::whnf_at_ptr), for use at recursive
    /// call sites (an `async fn` cannot directly recurse into itself).
    pub fn sub_whnf_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, TermPtr<'h>> {
        Box::pin(self.whnf_at_ptr(ptr))
    }

    /// Reduce the node at `ptr` to weak head normal form in place, returning a
    /// pointer to the result node (which may differ from `ptr` if the head
    /// interaction relocated it). The generic [`whnf_at`](Self::whnf_at) wraps this.
    pub async fn whnf_at_ptr(&self, ptr: TermPtr<'h>) -> TermPtr<'h> {
        let mut spine: Spine<'h> = Spine::new();
        let (mut slot, mut term) = self.heap.term(ptr);

        loop {
            if !self.policy.should_continue() {
                // Budget spent: write the head back and fold the spine.
                let (mut s, mut t) = (slot, term);
                loop {
                    match spine.unwind(s, t) {
                        Ok((ps, pt)) => {
                            s = ps;
                            t = pt;
                        }
                        Err(done) => return done,
                    }
                }
            }

            // ---- reduction step ----
            match term {
                Term::App { func, arg } => {
                    // Descend into `func`; the spine nulls it out of the stored
                    // frame and hands it back, so no alias to the child remains.
                    let child = spine.push(slot, Term::App { func, arg });
                    let (fslot, fterm) = self.heap.term(child);
                    slot = fslot;
                    term = fterm;
                    continue;
                }
                Term::Lam { body } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        let arg_term = self.heap.pull(arg);
                        let body_ptr = self.heap.substitute(body, arg_term);
                        self.heap.remove_slot(app_slot);
                        self.heap.remove_slot(slot);
                        self.policy.next_step(InteractionType::AppLam);
                        let (s, t) = self.heap.term(body_ptr);
                        slot = s;
                        term = t;
                        continue;
                    }
                    term = Term::Lam { body };
                }
                Term::Use { body } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.erase(self.heap.pull(arg));
                        self.heap.remove_slot(app_slot);
                        self.heap.remove_slot(slot);
                        self.policy.next_step(InteractionType::AppUse);
                        let (s, t) = self.heap.term(body);
                        slot = s;
                        term = t;
                        continue;
                    }
                    term = Term::Use { body };
                }
                Term::Wld => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.erase(self.heap.pull(arg));
                        self.heap.remove_slot(app_slot);
                        self.policy.next_step(InteractionType::AppEra);
                        term = Term::Wld; // reuse `slot`
                        continue;
                    }
                    term = Term::Wld;
                }
                Term::Bop { op, lhs, rhs } => {
                    // Reduce both operands concurrently.
                    let (nl, nr) = tokio::join!(self.sub_whnf_at(lhs), self.sub_whnf_at(rhs));
                    let (nl, nr) = if self.policy.should_continue() {
                        match self.combine_bop(op, nl, nr) {
                            Ok(t) => {
                                term = t; // reuse `slot`
                                continue;
                            }
                            Err(operands) => operands,
                        }
                    } else {
                        (nl, nr)
                    };
                    // stuck (or budget): rebuild with reduced operands and unwind.
                    term = Term::Bop {
                        op,
                        lhs: nl,
                        rhs: nr,
                    };
                }
                Term::Dup { label, ptr: dp } => {
                    match self.force_dup(label, dp).await {
                        Some(t) => {
                            term = t; // reuse `slot`
                            continue;
                        }
                        // Stuck: a dup over an unsubstituted binder. Leave the
                        // `Dup` as an inert head and unwind.
                        None => term = Term::Dup { label, ptr: dp },
                    }
                }
                Term::Sup { label, ptr: sup } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.heap.remove_slot(app_slot);
                        self.policy.next_step(InteractionType::AppSup);
                        term = self.app_sup(label, sup, arg); // reuse `slot`
                        continue;
                    }
                    term = Term::Sup { label, ptr: sup };
                }
                Term::Mat { matches, branches } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func, arg } = app_cont else {
                            unreachable!()
                        };
                        let na = self.sub_whnf_at(arg).await;
                        // Peek the WHNF scrutinee to select a branch; on a match,
                        // consume `na` for its fields.
                        let selected = self.match_index(&matches, &self.heap.view(&na));
                        match selected {
                            Some(idx) => {
                                self.heap.remove_slot(app_slot);
                                let scrut = self.heap.pull(na);
                                term = self.fire_mat(matches, branches, scrut, idx);
                                continue;
                            }
                            None => {
                                // `func` is the null placeholder from `pop`; thread
                                // it straight back with the reduced scrutinee.
                                spine.repush(app_slot, Term::App { func, arg: na });
                                term = Term::Mat { matches, branches };
                            }
                        }
                    } else {
                        term = Term::Mat { matches, branches };
                    }
                }
                Term::Pri(id) => {
                    let arity = self.extensions.arity(id);
                    // Pop up to `arity` application frames (innermost first). Each
                    // frame's continuation is the null placeholder; we keep it to
                    // rebuild an under-applied primitive's stuck application.
                    let mut frames: Vec<(TermSlot<'h>, TermPtr<'h>, TermPtr<'h>)> = Vec::new();
                    let mut saturated = true;
                    for _ in 0..arity {
                        match spine.pop() {
                            Some((s, Term::App { func, arg })) => frames.push((s, func, arg)),
                            Some((s, other)) => {
                                spine.repush(s, other);
                                saturated = false;
                                break;
                            }
                            None => {
                                saturated = false;
                                break;
                            }
                        }
                    }
                    if !saturated {
                        for (s, func, arg) in frames.into_iter().rev() {
                            spine.repush(s, Term::App { func, arg });
                        }
                        term = Term::Pri(id);
                    } else {
                        // Hand the (unforced) argument handles to the primitive; it
                        // forces what it needs and returns a result handle. Any
                        // argument it merely drops is reclaimed below, so it need
                        // not erase unused inputs by hand.
                        let mut args = Vec::with_capacity(frames.len());
                        for (app_slot, _func, arg) in frames {
                            args.push(Handle::new(arg, self.heap));
                            self.heap.remove_slot(app_slot);
                        }
                        self.policy.next_step(InteractionType::AppPri);
                        let result = self.extensions.apply(self, id, args).await.into_term_ptr();
                        self.erase_dropped_handles().await;
                        self.heap.remove_slot(slot); // drop the spent Pri node
                        let (s, t) = self.heap.term(result);
                        slot = s;
                        term = t;
                        continue;
                    }
                }
                other => term = other, // every other head is inert in v1.
            }

            // ---- unwind ----
            // The head is inert/stuck: fold it back up the spine, restoring each
            // parent's continuation from the slot just finalized.
            loop {
                match spine.unwind(slot, term) {
                    Ok((cslot, cterm)) => {
                        slot = cslot;
                        term = cterm;
                    }
                    Err(done) => return done,
                }
            }
        }
    }

    /// Combine a binary op whose operands `la`/`ra` are already in WHNF. On a
    /// reduction the operand nodes are consumed and the result term returned as
    /// `Ok`; otherwise the operands are handed back as `Err` so the caller can
    /// rebuild a stuck `Bop`.
    fn combine_bop(
        &self,
        op: BinaryOp,
        la: TermPtr<'h>,
        ra: TermPtr<'h>,
    ) -> Result<Term<'h>, (TermPtr<'h>, TermPtr<'h>)> {
        // BOP-SUP: a superposed operand distributes the op over both branches.
        if matches!(&*self.heap.view(&la), Term::Sup { .. }) {
            return Ok(self.bop_sup_left(op, la, ra));
        }
        if matches!(&*self.heap.view(&ra), Term::Sup { .. }) {
            return Ok(self.bop_sup_right(op, la, ra));
        }
        // BOP-VAL: two concrete numbers.
        let nums = match (&*self.heap.view(&la), &*self.heap.view(&ra)) {
            (Term::U64(a), Term::U64(b)) => Some((*a, *b)),
            _ => None,
        };
        if let Some((a, b)) = nums {
            self.heap.pull(la);
            self.heap.pull(ra);
            self.policy.next_step(InteractionType::BopVal);
            return Ok(match apply_op(op, a, b) {
                Some(v) => Term::U64(v),
                None => Term::Wld, // div/mod by zero erases to a wildcard
            });
        }
        Err((la, ra))
    }

    /// BOP-SUP (lhs superposed): `(&L{a,b} op r)` => `&L{(a op r0), (b op r1)}`.
    fn bop_sup_left(&self, op: BinaryOp, la: TermPtr<'h>, ra: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let Term::Sup { label, ptr: sup } = self.heap.pull(la) else {
            unreachable!("combine_bop checked lhs is a Sup")
        };
        let (a, b) = self.heap.free_sup(sup);
        let rhs = self.heap.pull(ra);
        let (d0, d1) = self.heap.alloc_dup(rhs);
        let b0 = self.heap.alloc(Term::Bop {
            op,
            lhs: a,
            rhs: self.dp_node(label, d0),
        });
        let b1 = self.heap.alloc(Term::Bop {
            op,
            lhs: b,
            rhs: self.dp_node(label, d1),
        });
        self.sup_term(label, b0, b1)
    }

    /// BOP-SUP (rhs superposed): `(l op &L{a,b})` => `&L{(l0 op a), (l1 op b)}`.
    fn bop_sup_right(&self, op: BinaryOp, la: TermPtr<'h>, ra: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let Term::Sup { label, ptr: sup } = self.heap.pull(ra) else {
            unreachable!("combine_bop checked rhs is a Sup")
        };
        let (a, b) = self.heap.free_sup(sup);
        let lhs = self.heap.pull(la);
        let (d0, d1) = self.heap.alloc_dup(lhs);
        let b0 = self.heap.alloc(Term::Bop {
            op,
            lhs: self.dp_node(label, d0),
            rhs: a,
        });
        let b1 = self.heap.alloc(Term::Bop {
            op,
            lhs: self.dp_node(label, d1),
            rhs: b,
        });
        self.sup_term(label, b0, b1)
    }

    // ====================================================================
    // Duplication / superposition / match
    // ====================================================================

    fn dp_node(&self, label: LabelId, dp: DupPtr<'h>) -> TermPtr<'h> {
        self.heap.alloc(Term::Dup { label, ptr: dp })
    }

    fn sup_term(&self, label: LabelId, a: TermPtr<'h>, b: TermPtr<'h>) -> Term<'h> {
        Term::Sup {
            label,
            ptr: self.heap.sup(a, b),
        }
    }

    /// Force one projection of a duplication. The first branch to acquire the
    /// cell lock reduces and fires the value (publishing the other projection)
    /// while holding the lock; the second wakes to a `None` value and reads its
    /// projection slot. Returns `None` when the dup is stuck (its value is an
    /// unsubstituted binder), leaving the cell untouched.
    fn force_dup(&self, label: LabelId, dp: DupPtr<'h>) -> Reduce<'_, Option<Term<'h>>> {
        Box::pin(async move {
            let mut guard = self.heap.dup_lock(dp).await;
            match self.heap.dup_take_value(&mut guard) {
                Some(seed) => {
                    // Reduce the duplicand to WHNF in place (kept addressable so a
                    // stuck dup can leave it untouched). A dup is stuck when its
                    // value won't reduce to a duplicable head: a bare binder `Var`,
                    // or another stuck `Dup` (transitively over a binder). These
                    // only arise under strong normalization.
                    let vp = self.sub_whnf_at(seed).await;
                    // Don't fire when the budget was spent mid-reduction: `vp` may
                    // not actually be WHNF (e.g. a `Bop` still awaiting BOP-SUP), and
                    // firing `dup_head` on a redex would both overshoot the budget
                    // and duplicate unreduced work. Leave the dup unfired (restore
                    // the duplicand); the next step resumes reducing it.
                    if !self.policy.should_continue()
                        || matches!(&*self.heap.view(&vp), Term::Var | Term::Dup { .. })
                    {
                        self.heap.dup_restore_value(&mut guard, vp);
                        drop(guard);
                        return None;
                    }
                    let head = self.heap.pull(vp);
                    let (s0, s1) = self.dup_head(label, dp, head).await;
                    // Fire: install both projections (Dp0 = left, Dp1 = right) as
                    // nodes, then read out this side.
                    let left = self.heap.alloc(s0);
                    let right = self.heap.alloc(s1);
                    self.heap.dup_fire(&mut guard, left, right);
                    let mine = self.heap.dup_project(dp, &mut guard);
                    drop(guard);
                    Some(mine)
                }
                None => {
                    // The other branch fired; read out this side
                    // and reclaim the (now fully projected) cell.
                    let mine = self.heap.dup_project(dp, &mut guard);
                    drop(guard);
                    self.heap.free_dup(dp);
                    Some(mine)
                }
            }
        })
    }

    /// Produce the two projections `(Dp0, Dp1)` of duplicating `head` (already in
    /// WHNF). Sub-terms are duplicated by allocating fresh dups over concrete
    /// values.
    fn dup_head(
        &self,
        label: LabelId,
        _dp: DupPtr<'h>,
        head: Term<'h>,
    ) -> Reduce<'_, (Term<'h>, Term<'h>)> {
        Box::pin(async move {
            match head {
                // copy leaves / atoms: duplicating a scalar value is a DUP-VAL.
                Term::U64(n) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::U64(n), Term::U64(n))
                }
                Term::I64(n) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::I64(n), Term::I64(n))
                }
                Term::F32(x) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::F32(x), Term::F32(x))
                }
                Term::F64(x) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::F64(x), Term::F64(x))
                }
                Term::Char(c) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::Char(c), Term::Char(c))
                }
                Term::Bool(b) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::Bool(b), Term::Bool(b))
                }
                Term::Wld => {
                    self.policy.next_step(InteractionType::DupWld);
                    (Term::Wld, Term::Wld)
                }
                // `Term::Var` (an unsubstituted binder) never reaches here: a dup
                // over one is left stuck by `force_dup`.
                Term::Pri(id) => {
                    self.policy.next_step(InteractionType::DupPri);
                    (Term::Pri(id), Term::Pri(id))
                }
                Term::Box(v) => {
                    self.policy.next_step(InteractionType::DupVal);
                    let v2 = self.heap.value_dup(&v);
                    (Term::Box(v), Term::Box(v2))
                }
                Term::App { func, arg } => {
                    self.policy.next_step(InteractionType::DupApp);
                    let f = self.heap.pull(func);
                    let x = self.heap.pull(arg);
                    let (df0, df1) = self.heap.alloc_dup(f);
                    let (dx0, dx1) = self.heap.alloc_dup(x);
                    let app0 = Term::App {
                        func: self.dp_node(label, df0),
                        arg: self.dp_node(label, dx0),
                    };
                    let app1 = Term::App {
                        func: self.dp_node(label, df1),
                        arg: self.dp_node(label, dx1),
                    };
                    (app0, app1)
                }
                Term::Bop { op, lhs, rhs } => {
                    // A stuck binary op (an operand is an unsubstituted binder, as
                    // under a duplicated lambda) distributes the dup into both
                    // operands, like DUP-APP.
                    self.policy.next_step(InteractionType::DupBop);
                    let l = self.heap.pull(lhs);
                    let r = self.heap.pull(rhs);
                    let (dl0, dl1) = self.heap.alloc_dup(l);
                    let (dr0, dr1) = self.heap.alloc_dup(r);
                    let bop0 = Term::Bop {
                        op,
                        lhs: self.dp_node(label, dl0),
                        rhs: self.dp_node(label, dr0),
                    };
                    let bop1 = Term::Bop {
                        op,
                        lhs: self.dp_node(label, dl1),
                        rhs: self.dp_node(label, dr1),
                    };
                    (bop0, bop1)
                }
                Term::Use { body } => {
                    self.policy.next_step(InteractionType::DupUse);
                    let b = self.heap.pull(body);
                    let (d0, d1) = self.heap.alloc_dup(b);
                    (
                        Term::Use {
                            body: self.dp_node(label, d0),
                        },
                        Term::Use {
                            body: self.dp_node(label, d1),
                        },
                    )
                }
                Term::Lam { body } => {
                    self.policy.next_step(InteractionType::DupLam);
                    let (orig_binder, body_ptr) = self.heap.open_body(body);
                    let body_term = self.heap.pull(body_ptr);
                    let (dg0, dg1) = self.heap.alloc_dup(body_term);
                    let (h0, occ0) = self.heap.fresh_binder();
                    let (h1, occ1) = self.heap.fresh_binder();
                    let lam0 = Term::Lam {
                        body: self.heap.close_body(h0, self.dp_node(label, dg0)),
                    };
                    let lam1 = Term::Lam {
                        body: self.heap.close_body(h1, self.dp_node(label, dg1)),
                    };
                    // x ← &L{ occ0, occ1 }
                    let sup_var = self.sup_term(label, occ0, occ1);
                    self.heap.fill_binder(orig_binder, sup_var);
                    (lam0, lam1)
                }
                Term::Ctr {
                    name,
                    arity,
                    values,
                } => {
                    self.policy.next_step(InteractionType::DupCtr);
                    let n = arity as usize;
                    let mut f0 = Vec::with_capacity(n);
                    let mut f1 = Vec::with_capacity(n);
                    for i in 0..n {
                        let field = self.heap.pull(self.heap.pack_field(&values, i));
                        let (di0, di1) = self.heap.alloc_dup(field);
                        f0.push(self.dp_node(label, di0));
                        f1.push(self.dp_node(label, di1));
                    }
                    self.heap.free_pack(values);
                    let p0 = self.heap.alloc_pack(f0);
                    let p1 = self.heap.alloc_pack(f1);
                    (
                        Term::Ctr {
                            name,
                            arity,
                            values: p0,
                        },
                        Term::Ctr {
                            name,
                            arity,
                            values: p1,
                        },
                    )
                }
                Term::Sup {
                    label: slab,
                    ptr: sup,
                } => {
                    self.policy.next_step(InteractionType::DupSup);
                    let (a, b) = self.heap.free_sup(sup);
                    if label == slab {
                        // same label annihilates
                        (self.heap.pull(a), self.heap.pull(b))
                    } else {
                        let ta = self.heap.pull(a);
                        let tb = self.heap.pull(b);
                        let (da0, da1) = self.heap.alloc_dup(ta);
                        let (db0, db1) = self.heap.alloc_dup(tb);
                        let s0 =
                            self.sup_term(slab, self.dp_node(label, da0), self.dp_node(label, db0));
                        let s1 =
                            self.sup_term(slab, self.dp_node(label, da1), self.dp_node(label, db1));
                        (s0, s1)
                    }
                }
                other => unreachable!("DUP of an unexpected head: {other:?}"),
            }
        })
    }

    /// APP-SUP: `(&L{f,g}) arg` => `!d&L=arg; &L{(f d0), (g d1)}`.
    fn app_sup(&self, label: LabelId, sup: SupPtr<'h>, arg: TermPtr<'h>) -> Term<'h> {
        let (f, g) = self.heap.free_sup(sup);
        let arg_term = self.heap.pull(arg);
        let (d0, d1) = self.heap.alloc_dup(arg_term);
        let fa = self.heap.alloc(Term::App {
            func: f,
            arg: self.dp_node(label, d0),
        });
        let gb = self.heap.alloc(Term::App {
            func: g,
            arg: self.dp_node(label, d1),
        });
        self.sup_term(label, fa, gb)
    }

    /// The branch index a WHNF scrutinee selects in a match table, if any.
    fn match_index(&self, matches: &MatchPtr<'h>, scrut: &Term<'h>) -> Option<usize> {
        let key = match scrut {
            Term::Ctr { name, .. } => PatKey::Ctr(name.addr()),
            Term::U64(k) => PatKey::Num(*k),
            _ => return None,
        };
        let data = self.heap.match_data(matches);
        data.cases
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, i)| *i)
            .or(data.default)
    }

    /// APP-MAT fire: branch `idx` was selected for the (already consumed) `scrut`.
    /// Apply the constructor's fields to the branch and reclaim everything else.
    fn fire_mat(
        &self,
        matches: MatchPtr<'h>,
        branches: PackPtr<'h>,
        scrut: Term<'h>,
        idx: usize,
    ) -> Term<'h> {
        // Take the selected branch and erase the rest.
        let mut selected = None;
        for (j, field) in self.heap.into_fields(branches).into_iter().enumerate() {
            if j == idx {
                selected = Some(field);
            } else {
                self.erase(self.heap.pull(field));
            }
        }
        let mut acc = selected.expect("selected branch index in range");
        if let Term::Ctr { arity, values, .. } = scrut {
            for field in self
                .heap
                .into_fields(values)
                .into_iter()
                .take(arity as usize)
            {
                acc = self.heap.alloc(Term::App {
                    func: acc,
                    arg: field,
                });
            }
        }
        self.heap.free_match(matches);
        self.policy.next_step(InteractionType::AppMat);
        self.heap.pull(acc)
    }

    // ====================================================================
    // Normalization
    // ====================================================================

    /// The boxed form of [`normalize_at_ptr`](Self::normalize_at_ptr), for
    /// recursive call sites.
    pub fn sub_normalize_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, TermPtr<'h>> {
        Box::pin(self.normalize_at_ptr(ptr))
    }

    /// Reduce the node at `ptr` to full normal form in place, returning a pointer
    /// to the result node. The generic [`normalize_at`](Self::normalize_at) wraps
    /// this.
    pub async fn normalize_at_ptr(&self, ptr: TermPtr<'h>) -> TermPtr<'h> {
        let p = self.whnf_at_ptr(ptr).await;
        if !self.policy.should_continue() {
            return p;
        }
        let (slot, term) = self.heap.term(p);
        match term {
            Term::Lam { body } => {
                let (binder, body_ptr) = self.heap.open_body(body);
                let nb = self.sub_normalize_at(body_ptr).await;
                slot.finished(Term::Lam {
                    body: self.heap.close_body(binder, nb),
                })
            }
            Term::Use { body } => {
                let nb = self.sub_normalize_at(body).await;
                slot.finished(Term::Use { body: nb })
            }
            Term::App { func, arg } => {
                let nf = self.sub_normalize_at(func).await;
                let na = self.sub_normalize_at(arg).await;
                slot.finished(Term::App { func: nf, arg: na })
            }
            Term::Sup { label, ptr } => {
                let (a, b) = self.heap.sup_args(&ptr);
                let na = self.sub_normalize_at(a).await;
                let nb = self.sub_normalize_at(b).await;
                self.heap.set_sup_args(&ptr, na, nb);
                slot.finished(Term::Sup { label, ptr })
            }
            Term::Bop { op, lhs, rhs } => {
                let nl = self.sub_normalize_at(lhs).await;
                let nr = self.sub_normalize_at(rhs).await;
                slot.finished(Term::Bop {
                    op,
                    lhs: nl,
                    rhs: nr,
                })
            }
            Term::Ctr {
                name,
                arity,
                values,
            } => {
                for i in 0..arity as usize {
                    let f = self.heap.pack_field(&values, i);
                    let nf = self.sub_normalize_at(f).await;
                    self.heap.set_pack_field(&values, i, nf);
                }
                slot.finished(Term::Ctr {
                    name,
                    arity,
                    values,
                })
            }
            other => slot.finished(other),
        }
    }
}

/// Apply a binary operator to two numbers. `None` for a failed operation (div/mod
/// by zero), which the caller turns into an erasure.
#[rustfmt::skip]
fn apply_op(op: BinaryOp, a: u64, b: u64) -> Option<u64> {
    Some(match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        BinaryOp::Div => return (b != 0).then(|| a / b),
        BinaryOp::Mod => return (b != 0).then(|| a % b),
        BinaryOp::Eq => (a == b) as u64,
        BinaryOp::Neq => (a != b) as u64,
        BinaryOp::Lt => (a < b) as u64,
        BinaryOp::Lte => (a <= b) as u64,
        BinaryOp::Gt => (a > b) as u64,
        BinaryOp::Gte => (a >= b) as u64,
        BinaryOp::And => (a != 0 && b != 0) as u64,
        BinaryOp::Or => (a != 0 || b != 0) as u64,
        BinaryOp::Xor => a ^ b,
        BinaryOp::Shl => a.wrapping_shl(b as u32),
        BinaryOp::Shr => a.wrapping_shr(b as u32),
        BinaryOp::Invalid => 0,
    })
}
