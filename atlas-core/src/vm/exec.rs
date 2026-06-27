//! The [`Executor`]: interaction-calculus evaluation over a branded [`HeapScope`].
//!
//! v1 is a synchronous, single-task evaluator over the affine heap model. It
//! covers the affine core (APP-LAM / APP-USE / APP-ERA, binary ops, constructors
//! as data, and full normalization). The duplication / superposition / match
//! interactions and the parallel async driver are deferred to a later increment.

use crate::extension::{Extensions, Handle, NoExtensions, TermPtrLike};
use crate::vm::heap::{
    Boxed, DupPtr, HeapScope, MatchPtr, Spine, SupPtr, TermPtr, TermSlot, ValuePtr,
};
use crate::vm::term::{BinaryOp, LabelId, Term, UnaryOp};
use ordered_float::OrderedFloat;
use std::sync::Arc;
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
    AppLam, AppUse, AppEra, AppErr, AppSup, AppMat, AppPri,
    DupLam, DupSup, DupCtr, DupApp, DupBop, DupUop, DupNum, DupWld, DupVar, DupUse, DupPri, DupVal,
    BopVal, BopSup,
    UopVal, UopSup,
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
            Term::Uop { val, .. } => {
                self.erase(self.heap.pull(val));
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
            | Term::Int(_)
            | Term::Float(_)
            | Term::Char(_)
            | Term::Bool(_)
            | Term::Pri(_)
            | Term::Type(_)
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
                Term::Err { immediate, backtrace } => {
                    // An `Err` is a first-class eraser: when forced as the head of
                    // an application it annihilates the argument and bubbles up,
                    // exactly like `Wld`/era. It never stays stuck.
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.erase(self.heap.pull(arg));
                        self.heap.remove_slot(app_slot);
                        self.policy.next_step(InteractionType::AppErr);
                        term = Term::Err { immediate, backtrace }; // reuse `slot`
                        continue;
                    }
                    term = Term::Err { immediate, backtrace };
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
                Term::Uop { op, val } => {
                    let nv = self.sub_whnf_at(val).await;
                    if self.policy.should_continue() {
                        match self.combine_uop(op, nv) {
                            Ok(t) => {
                                term = t; // reuse `slot`
                                continue;
                            }
                            Err(operand) => term = Term::Uop { op, val: operand },
                        }
                    } else {
                        term = Term::Uop { op, val: nv };
                    }
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
                Term::Mat { matches } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func, arg } = app_cont else {
                            unreachable!()
                        };
                        let na = self.sub_whnf_at(arg).await;
                        // A concrete scrutinee fires the match (consuming `na`); an
                        // as-yet-inert head leaves the match stuck.
                        if is_matchable(&self.heap.view(&na)) {
                            self.heap.remove_slot(app_slot);
                            let scrut = self.heap.pull(na);
                            term = self.fire_mat(matches, scrut).await;
                            continue;
                        } else {
                            // `func` is the null placeholder from `pop`; thread it
                            // straight back with the reduced scrutinee.
                            spine.repush(app_slot, Term::App { func, arg: na });
                            term = Term::Mat { matches };
                        }
                    } else {
                        term = Term::Mat { matches };
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
        // BOP-VAL: both operands must be concrete primitive leaves. A type
        // mismatch or an op unsupported for the operand type reduces to `Err`;
        // if either operand is not yet a value, the op stays stuck and is rebuilt.
        let result = match (&*self.heap.view(&la), &*self.heap.view(&ra)) {
            // BOP-ERR: an `Err` operand bubbles up, erasing the other operand.
            (Term::Err { .. }, _) | (_, Term::Err { .. }) => Some(err_term()),
            (Term::Int(a), Term::Int(b)) => Some(apply_int(op, *a, *b)),
            (Term::Float(a), Term::Float(b)) => Some(apply_float(op, a.0, b.0)),
            // mixed Int/Float: promote the int and operate in float space.
            (Term::Int(a), Term::Float(b)) => Some(apply_float(op, *a as f64, b.0)),
            (Term::Float(a), Term::Int(b)) => Some(apply_float(op, a.0, *b as f64)),
            (Term::Bool(a), Term::Bool(b)) => Some(apply_bool(op, *a, *b)),
            (Term::Char(a), Term::Char(b)) => Some(apply_char(op, *a, *b)),
            // strings / byte arrays: equality and concatenation.
            (Term::Box(a), Term::Box(b)) => Some(self.apply_box(op, a, b)),
            // both are values but of mismatched type: an invalid op.
            (lt, rt) if is_value(lt) && is_value(rt) => Some(err_term()),
            // at least one operand is not a value yet: stay stuck.
            _ => None,
        };
        match result {
            Some(t) => {
                // Reclaim both operands (a no-op for scalar leaves; drops the
                // payload of a boxed string/bytes operand or an `Err` child).
                self.erase(self.heap.pull(la));
                self.erase(self.heap.pull(ra));
                self.policy.next_step(InteractionType::BopVal);
                Ok(t)
            }
            None => Err((la, ra)),
        }
    }

    /// Apply a binary op to two boxed values (strings / byte arrays). Supports
    /// `==` / `!=` (yielding `Bool`) and `+` concatenation (yielding a fresh
    /// boxed value). Mismatched box kinds or any other op yield `Err`.
    fn apply_box(&self, op: BinaryOp, a: &ValuePtr<'h>, b: &ValuePtr<'h>) -> Term<'h> {
        use BinaryOp::*;
        match (op, self.heap.value_get(a), self.heap.value_get(b)) {
            (Eq, Boxed::Str(x), Boxed::Str(y)) => Term::Bool(x == y),
            (Neq, Boxed::Str(x), Boxed::Str(y)) => Term::Bool(x != y),
            (Eq, Boxed::Bytes(x), Boxed::Bytes(y)) => Term::Bool(x == y),
            (Neq, Boxed::Bytes(x), Boxed::Bytes(y)) => Term::Bool(x != y),
            (Add, Boxed::Str(x), Boxed::Str(y)) => {
                let s = format!("{x}{y}");
                Term::Box(self.heap.value(Boxed::Str(Arc::from(s.as_str()))))
            }
            (Add, Boxed::Bytes(x), Boxed::Bytes(y)) => {
                let mut v = Vec::with_capacity(x.len() + y.len());
                v.extend_from_slice(x);
                v.extend_from_slice(y);
                Term::Box(self.heap.value(Boxed::Bytes(Arc::from(v.as_slice()))))
            }
            _ => err_term(),
        }
    }

    /// Combine a unary op whose operand `va` is already in WHNF. On a reduction
    /// the operand node is consumed and the result term returned as `Ok`;
    /// otherwise the operand is handed back as `Err` so the caller can rebuild a
    /// stuck `Uop`.
    fn combine_uop(&self, op: UnaryOp, va: TermPtr<'h>) -> Result<Term<'h>, TermPtr<'h>> {
        // UOP-SUP: a superposed operand distributes the op over both branches.
        if matches!(&*self.heap.view(&va), Term::Sup { .. }) {
            return Ok(self.uop_sup(op, va));
        }
        let result = match (op, &*self.heap.view(&va)) {
            // UOP-ERR: an `Err` operand bubbles up.
            (_, Term::Err { .. }) => Some(err_term()),
            (UnaryOp::Neg, Term::Int(a)) => Some(Term::Int(a.wrapping_neg())),
            (UnaryOp::Neg, Term::Float(a)) => Some(Term::Float(OrderedFloat(-a.0))),
            (UnaryOp::Not, Term::Bool(a)) => Some(Term::Bool(!a)),
            (UnaryOp::Not, Term::Int(a)) => Some(Term::Int(!a)),
            // operand is a value but the op is unsupported for its type.
            (_, t) if is_value(t) => Some(err_term()),
            // operand is not a value yet: stay stuck.
            _ => None,
        };
        match result {
            Some(t) => {
                // Reclaim the operand (a no-op for scalar leaves; drops a boxed
                // payload or an `Err` child).
                self.erase(self.heap.pull(va));
                self.policy.next_step(InteractionType::UopVal);
                Ok(t)
            }
            None => Err(va),
        }
    }

    /// UOP-SUP: `~&L{a,b}` => `&L{~a, ~b}`. A unary op over a superposition maps
    /// each branch (no duplication is needed since the op has a single operand).
    fn uop_sup(&self, op: UnaryOp, va: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::UopSup);
        let Term::Sup { label, ptr: sup } = self.heap.pull(va) else {
            unreachable!("combine_uop checked operand is a Sup")
        };
        let (a, b) = self.heap.free_sup(sup);
        let u0 = self.heap.alloc(Term::Uop { op, val: a });
        let u1 = self.heap.alloc(Term::Uop { op, val: b });
        self.sup_term(label, u0, u1)
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
                Term::Int(n) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::Int(n), Term::Int(n))
                }
                Term::Float(x) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::Float(x), Term::Float(x))
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
                Term::Type(t) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::Type(t), Term::Type(t))
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
                Term::Uop { op, val } => {
                    // A stuck unary op distributes the dup into its operand.
                    self.policy.next_step(InteractionType::DupUop);
                    let v = self.heap.pull(val);
                    let (dv0, dv1) = self.heap.alloc_dup(v);
                    let uop0 = Term::Uop {
                        op,
                        val: self.dp_node(label, dv0),
                    };
                    let uop1 = Term::Uop {
                        op,
                        val: self.dp_node(label, dv1),
                    };
                    (uop0, uop1)
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

    /// Whether a WHNF pattern key matches the (already-WHNF) scrutinee. A
    /// constructor scrutinee matches a `Type` key with the same type identity; a
    /// value scrutinee matches an equal value key.
    fn key_matches(&self, scrut: &Term<'h>, key: &Term<'h>) -> bool {
        match (scrut, key) {
            (Term::Ctr { name, .. }, Term::Type(t)) => name.addr() == t.addr(),
            (Term::Int(a), Term::Int(b)) => a == b,
            (Term::Float(a), Term::Float(b)) => a == b,
            (Term::Bool(a), Term::Bool(b)) => a == b,
            (Term::Char(a), Term::Char(b)) => a == b,
            (Term::Box(a), Term::Box(b)) => {
                match (self.heap.value_get(a), self.heap.value_get(b)) {
                    (Boxed::Str(x), Boxed::Str(y)) => x == y,
                    (Boxed::Bytes(x), Boxed::Bytes(y)) => x == y,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// APP-MAT fire for the (already consumed, concrete) `scrut`. Walk the cases,
    /// reducing each pattern key to WHNF and comparing it against the scrutinee.
    /// The first match's branch lambda is applied to the constructor's fields;
    /// every other key and branch (and the match table) is reclaimed. With no
    /// covering case or default, the scrutinee is erased and the match reduces to
    /// a runtime [`Term::Err`].
    async fn fire_mat(&self, matches: MatchPtr<'h>, scrut: Term<'h>) -> Term<'h> {
        // Copy the case/default node addresses out, then free the table.
        let (cases, default) = {
            let data = self.heap.match_data(&matches);
            (data.cases.clone(), data.default)
        };
        self.heap.free_match(matches);

        let mut selected: Option<TermPtr<'h>> = None;
        for (key_addr, branch_addr) in cases {
            let key_ptr = unsafe { TermPtr::forge(key_addr) };
            let branch_ptr = unsafe { TermPtr::forge(branch_addr) };
            let matched = if selected.is_none() {
                // Reduce the key to WHNF and compare it against the scrutinee.
                let key_ptr = self.sub_whnf_at(key_ptr).await;
                let m = self.key_matches(&scrut, &self.heap.view(&key_ptr));
                self.erase(self.heap.pull(key_ptr));
                m
            } else {
                // A branch is already chosen: just reclaim this key.
                self.erase(self.heap.pull(key_ptr));
                false
            };
            if matched {
                selected = Some(branch_ptr);
            } else {
                self.erase(self.heap.pull(branch_ptr));
            }
        }

        let branch = match (selected, default) {
            (Some(b), Some(d)) => {
                self.erase(self.heap.pull(unsafe { TermPtr::forge(d) }));
                b
            }
            (Some(b), None) => b,
            (None, Some(d)) => unsafe { TermPtr::forge(d) },
            (None, None) => {
                // A concrete value reached the match but no case or default covers
                // it: a runtime error.
                self.erase(scrut);
                self.policy.next_step(InteractionType::AppMat);
                return err_term();
            }
        };

        // Apply the constructor's fields to the selected branch lambda; a value
        // scrutinee carries no fields but may still own a boxed payload to reclaim.
        let mut acc = branch;
        match scrut {
            Term::Ctr { arity, values, .. } => {
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
            other => self.erase(other),
        }
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
            Term::Uop { op, val } => {
                let nv = self.sub_normalize_at(val).await;
                slot.finished(Term::Uop { op, val: nv })
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

/// Whether a WHNF scrutinee is a concrete value a match can fire on (a
/// constructor or a primitive value leaf). Any other head leaves the match inert.
fn is_matchable(scrut: &Term) -> bool {
    matches!(
        scrut,
        Term::Ctr { .. }
            | Term::Int(_)
            | Term::Float(_)
            | Term::Bool(_)
            | Term::Char(_)
            | Term::Box(_)
    )
}

/// A runtime error term, used for invalid operations (type mismatch, an op
/// unsupported for the operand type, or division/modulo by zero). The backtrace
/// is left unset for now.
fn err_term<'h>() -> Term<'h> {
    Term::Err {
        immediate: true,
        backtrace: None,
    }
}

/// Whether a WHNF term is a concrete primitive scalar leaf (the operands the
/// builtin ops act on).
fn is_value(t: &Term) -> bool {
    matches!(
        t,
        Term::Int(_)
            | Term::Float(_)
            | Term::Bool(_)
            | Term::Char(_)
            | Term::Box(_)
    )
}

/// Floor division of two `i64`s (rounds toward negative infinity, Python-style),
/// in contrast to Rust's truncating `/`. Caller guarantees `b != 0`.
fn floor_div_i64(a: i64, b: i64) -> i64 {
    let q = a.wrapping_div(b);
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) { q - 1 } else { q }
}

/// Apply a binary operator to two `Int`s. `/` is true division (always yields a
/// `Float`); `~/` is floor division (`Int`). Comparisons yield `Bool`; div / mod
/// by zero yields `Err`.
#[rustfmt::skip]
fn apply_int<'h>(op: BinaryOp, a: i64, b: i64) -> Term<'h> {
    use BinaryOp::*;
    match op {
        Add  => Term::Int(a.wrapping_add(b)),
        Sub  => Term::Int(a.wrapping_sub(b)),
        Mul  => Term::Int(a.wrapping_mul(b)),
        Div  => if b != 0 { Term::Float(OrderedFloat(a as f64 / b as f64)) } else { err_term() },
        IDiv => if b != 0 { Term::Int(floor_div_i64(a, b)) } else { err_term() },
        Mod  => if b != 0 { Term::Int(a.wrapping_rem(b)) } else { err_term() },
        And  => Term::Int(a & b),
        Or   => Term::Int(a | b),
        Xor  => Term::Int(a ^ b),
        Shl  => Term::Int(a.wrapping_shl(b as u32)),
        Shr  => Term::Int(a.wrapping_shr(b as u32)),
        Eq   => Term::Bool(a == b),
        Neq  => Term::Bool(a != b),
        Lt   => Term::Bool(a < b),
        Lte  => Term::Bool(a <= b),
        Gt   => Term::Bool(a > b),
        Gte  => Term::Bool(a >= b),
        Invalid => err_term(),
    }
}

/// Apply a binary operator to two `f64`s (used for `Float op Float` and any mixed
/// `Int`/`Float` after promoting the int). `/` and `~/` (floor) by zero yield
/// `Err` rather than `inf`. Comparisons yield `Bool`; bitwise/shift ops yield `Err`.
#[rustfmt::skip]
fn apply_float<'h>(op: BinaryOp, a: f64, b: f64) -> Term<'h> {
    use BinaryOp::*;
    match op {
        Add  => Term::Float(OrderedFloat(a + b)),
        Sub  => Term::Float(OrderedFloat(a - b)),
        Mul  => Term::Float(OrderedFloat(a * b)),
        Div  => if b != 0.0 { Term::Float(OrderedFloat(a / b)) } else { err_term() },
        IDiv => if b != 0.0 { Term::Float(OrderedFloat((a / b).floor())) } else { err_term() },
        Mod  => if b != 0.0 { Term::Float(OrderedFloat(a % b)) } else { err_term() },
        Eq   => Term::Bool(a == b),
        Neq  => Term::Bool(a != b),
        Lt   => Term::Bool(a < b),
        Lte  => Term::Bool(a <= b),
        Gt   => Term::Bool(a > b),
        Gte  => Term::Bool(a >= b),
        And | Or | Xor | Shl | Shr | Invalid => err_term(),
    }
}

/// Apply a binary operator to two `Bool`s. `&`/`|`/`^` are logical; `==`/`!=`
/// compare. Arithmetic / shift ops yield `Err`.
#[rustfmt::skip]
fn apply_bool<'h>(op: BinaryOp, a: bool, b: bool) -> Term<'h> {
    use BinaryOp::*;
    match op {
        And => Term::Bool(a && b),
        Or  => Term::Bool(a || b),
        Xor => Term::Bool(a ^ b),
        Eq  => Term::Bool(a == b),
        Neq => Term::Bool(a != b),
        _ => err_term(),
    }
}

/// Apply a binary operator to two `char`s. Only comparisons are supported (they
/// yield `Bool`); everything else yields `Err`.
#[rustfmt::skip]
fn apply_char<'h>(op: BinaryOp, a: char, b: char) -> Term<'h> {
    use BinaryOp::*;
    match op {
        Eq  => Term::Bool(a == b),
        Neq => Term::Bool(a != b),
        Lt  => Term::Bool(a < b),
        Lte => Term::Bool(a <= b),
        Gt  => Term::Bool(a > b),
        Gte => Term::Bool(a >= b),
        _ => err_term(),
    }
}
