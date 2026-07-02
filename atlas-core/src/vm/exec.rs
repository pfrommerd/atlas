//! The [`Executor`]: interaction-calculus evaluation over a branded [`HeapScope`].
//!
//! v1 is a synchronous, single-task evaluator over the affine heap model. It
//! covers the affine core (APP-LAM / APP-USE / APP-ERA, binary ops, constructors
//! as data, and full normalization). The duplication / superposition / match
//! interactions and the parallel async driver are deferred to a later increment.

use crate::extension::{Extensions, Handle, NoExtensions, TermPtrLike};
use crate::vm::heap::{
    Addr, Boxed, HeapScope, MatchPtr, RefPtr, Spine, SupPtr, TermPtr, TypeInfo, TypePtr, ValuePtr,
    Variant,
};
use crate::vm::term::{BinaryOp, LabelId, PrimId, Term, UnaryOp, VariantId};
use ordered_float::OrderedFloat;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A boxed reduction future. Boxed so the (mutually) recursive async reduction
/// methods can call one another; the parallel driver will later add a `Send`
/// bound here.
type Reduce<'s, T> = Pin<Box<dyn Future<Output = T> + 's>>;

/// The kind of interaction performed in a single reduction step.
#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionType {
    AppLam, AppUse, AppEra, AppErr, AppSup, AppMat, AppPri, AppCtr,
    TypeDef, Variant,
    DupLam, DupSup, DupCtr, DupType, DupApp, DupBop, DupUop, DupNum, DupWld, DupVar, DupUse, DupPri, DupVal, DupRef,
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
            Term::Ctn { ty, values, .. } => {
                for f in self.heap.into_fields(values) {
                    self.erase(self.heap.pull(f));
                }
                self.erase_type(ty);
            }
            Term::Partial { func, args, .. } => {
                self.erase(self.heap.pull(func));
                for f in self.heap.into_fields(args) {
                    self.erase(self.heap.pull(f));
                }
            }
            Term::Box(v) => self.heap.value_drop(v),
            Term::Ctr { ty, .. } => self.erase(self.heap.pull(ty)),
            // A type value owns its (lazy) sub-type children.
            Term::Type(t) => self.erase_type(t),
            // Leaves and (v1-)inert heads.
            Term::Var
            | Term::Wld
            | Term::Err { .. }
            | Term::Int(_)
            | Term::Float(_)
            | Term::Char(_)
            | Term::Bool(_)
            | Term::Pri(_)
            | Term::VarId(_)
            | Term::Null => {}
            // Deferred-interaction heads: leave their cells (v1 does not produce
            // them through reduction).
            Term::Sup { .. } | Term::Ref { .. } | Term::Mat { .. } => {}
        }
    }

    /// Reclaim a type value, erasing its owned (lazy) sub-type child nodes.
    fn erase_type(&self, ty: TypePtr<'h>) {
        let erase_children = |this: &Self, addrs: Vec<Addr>| {
            for a in addrs {
                this.erase(this.heap.pull(unsafe { TermPtr::forge(a) }));
            }
        };
        match self.heap.free_type(ty) {
            TypeInfo::Product { fields, .. } => erase_children(self, fields),
            TypeInfo::Sum { variants, .. } => {
                for v in variants {
                    erase_children(self, v.args);
                }
            }
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
                Term::Err {
                    immediate,
                    backtrace,
                } => {
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
                        term = Term::Err {
                            immediate,
                            backtrace,
                        }; // reuse `slot`
                        continue;
                    }
                    term = Term::Err {
                        immediate,
                        backtrace,
                    };
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
                Term::Ref { ptr: dp } => {
                    match self.force_dup(dp).await {
                        Some(t) => {
                            term = t; // reuse `slot`
                            continue;
                        }
                        // Stuck: a dup over an unsubstituted binder. Leave the
                        // `Ref` as an inert head and unwind.
                        None => term = Term::Ref { ptr: dp },
                    }
                }
                Term::Sup { ptr: sup } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.heap.remove_slot(app_slot);
                        self.policy.next_step(InteractionType::AppSup);
                        term = self.app_sup(sup, arg); // reuse `slot`
                        continue;
                    }
                    term = Term::Sup { ptr: sup };
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
                    if arity == 0 {
                        // a nullary primitive is a constant: fire immediately.
                        let result = self.fire_prim(id, vec![]).await;
                        self.heap.remove_slot(slot);
                        let (s, t) = self.heap.term(result);
                        slot = s;
                        term = t;
                        continue;
                    }
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        // Applying a primitive gathers its args through a `Partial`.
                        let func = self.heap.alloc(Term::Pri(id));
                        let args = self.heap.alloc_pack(None, vec![]);
                        term = Term::Partial {
                            func,
                            arity: arity as u8,
                            args,
                        }; // reuse `slot`
                        continue;
                    }
                    term = Term::Pri(id);
                }
                Term::Type(t) => {
                    // A bare type value can no longer be applied to build a value;
                    // one must first turn it into a constructor with `::New` (product)
                    // or `::Variant` (sum). Applying a type to an argument is an error.
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        self.erase(Term::Type(t));
                        self.policy.next_step(InteractionType::AppCtr);
                        term = err_term();
                        continue;
                    }
                    term = Term::Type(t);
                }
                Term::Partial { func, arity, args } => {
                    // Gather one argument; complete (build a `Ctn` or fire the
                    // primitive) once the arity is reached.
                    if self.policy.should_continue()
                        && matches!(spine.peek(), Some(Term::App { .. }))
                    {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        self.heap.remove_slot(app_slot);
                        let mut fields = self.heap.into_fields(args);
                        fields.push(arg);
                        if fields.len() == arity as usize {
                            self.policy.next_step(InteractionType::AppCtr);
                            let result = self.complete_partial(func, fields).await;
                            self.heap.remove_slot(slot); // drop the spent Partial node
                            let (s, t) = self.heap.term(result);
                            slot = s;
                            term = t;
                            continue;
                        }
                        let args = self.heap.alloc_pack(None, fields);
                        term = Term::Partial { func, arity, args }; // reuse `slot`
                        continue;
                    }
                    term = Term::Partial { func, arity, args };
                }
                Term::Ctr { ty, variant } => {
                    // Force `ty` to a type value. A nullary constructor completes to a
                    // `Ctn`; a constructor with args is a selector value that, when
                    // applied, gathers its args through a `Partial`. A not-yet-
                    // resolved `ty` (binder/dup/sup) leaves the selector stuck so a
                    // surrounding dup can distribute into it.
                    let nt = self.sub_whnf_at(ty).await;
                    if !self.policy.should_continue() {
                        term = Term::Ctr { ty: nt, variant };
                    } else {
                        match self.classify_type_arg(&nt) {
                            ArgClass::Type => {
                                let n = match &*self.heap.view(&nt) {
                                    Term::Type(t) => self.ctor_arity(t, variant),
                                    _ => None,
                                };
                                match n {
                                    Some(0) => {
                                        let Term::Type(t) = self.heap.pull(nt) else {
                                            unreachable!()
                                        };
                                        let values = self.heap.alloc_pack(variant, vec![]);
                                        self.policy.next_step(InteractionType::Variant);
                                        term = Term::Ctn {
                                            ty: t,
                                            arity: 0,
                                            values,
                                        }; // reuse `slot`
                                        continue;
                                    }
                                    Some(k) if matches!(spine.peek(), Some(Term::App { .. })) => {
                                        // An applied selector gathers args via a `Partial`.
                                        let func = self.heap.alloc(Term::Ctr { ty: nt, variant });
                                        let args = self.heap.alloc_pack(None, vec![]);
                                        term = Term::Partial {
                                            func,
                                            arity: k as u8,
                                            args,
                                        };
                                        continue;
                                    }
                                    Some(_) => term = Term::Ctr { ty: nt, variant },
                                    None => {
                                        // unknown variant / constructor-type mismatch.
                                        self.erase(self.heap.pull(nt));
                                        self.policy.next_step(InteractionType::Variant);
                                        term = err_term();
                                    }
                                }
                            }
                            ArgClass::Stuck => term = Term::Ctr { ty: nt, variant },
                            ArgClass::Err => {
                                self.erase(self.heap.pull(nt));
                                self.policy.next_step(InteractionType::Variant);
                                term = err_term();
                            }
                        }
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
        // TYPEOF: yield the operand's type as a first-class `Type` value.
        if let UnaryOp::TypeOf = op {
            // A construction owns its type: hand it back, erasing only the fields.
            if matches!(&*self.heap.view(&va), Term::Ctn { .. }) {
                let Term::Ctn { ty, values, .. } = self.heap.pull(va) else {
                    unreachable!()
                };
                for f in self.heap.into_fields(values) {
                    self.erase(self.heap.pull(f));
                }
                self.policy.next_step(InteractionType::UopVal);
                return Ok(Term::Type(ty));
            }
            // Other operands map to a builtin/opaque type (or stay stuck / error).
            enum TyOf {
                Builtin(&'static str),
                Err,
                Stuck,
            }
            let decision = match &*self.heap.view(&va) {
                Term::Err { .. } => TyOf::Err,
                Term::Int(_) => TyOf::Builtin("Int"),
                Term::Float(_) => TyOf::Builtin("Float"),
                Term::Bool(_) => TyOf::Builtin("Bool"),
                Term::Char(_) => TyOf::Builtin("Char"),
                Term::Box(b) => match self.heap.value_get(b) {
                    Boxed::Str(_) => TyOf::Builtin("String"),
                    Boxed::Bytes(_) => TyOf::Builtin("Bytes"),
                },
                Term::Type(_) => TyOf::Builtin("Type"),
                Term::Lam { .. }
                | Term::Use { .. }
                | Term::Pri(_)
                | Term::Mat { .. }
                | Term::Ctr { .. }
                | Term::Partial { .. } => TyOf::Builtin("Function"),
                // An unsubstituted binder or stuck dup: type is not yet known.
                Term::Var | Term::Ref { .. } => TyOf::Stuck,
                _ => TyOf::Err,
            };
            if let TyOf::Stuck = decision {
                return Err(va);
            }
            let ty = match decision {
                TyOf::Builtin(name) => Term::Type(self.heap.builtin_type(name)),
                TyOf::Err => err_term(),
                TyOf::Stuck => unreachable!(),
            };
            self.erase(self.heap.pull(va));
            self.policy.next_step(InteractionType::UopVal);
            return Ok(ty);
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

    /// UOP-SUP: `~&{a,b}` => `&{~a, ~b}`. A unary op over a superposition maps
    /// each part (no duplication is needed since the op has a single operand).
    fn uop_sup(&self, op: UnaryOp, va: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::UopSup);
        let Term::Sup { ptr: sup } = self.heap.pull(va) else {
            unreachable!("combine_uop checked operand is a Sup")
        };
        let parts = self.heap.free_sup(sup);
        let new_parts = parts
            .into_iter()
            .map(|(l, a)| (l, self.heap.alloc(Term::Uop { op, val: a })))
            .collect();
        self.sup_from(new_parts)
    }

    /// BOP-SUP (lhs superposed): `(&{a,b} op r)` => `&{(a op r0), (b op r1)}`,
    /// duplicating `r` across the sup's wires.
    fn bop_sup_left(&self, op: BinaryOp, la: TermPtr<'h>, ra: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let Term::Sup { ptr: sup } = self.heap.pull(la) else {
            unreachable!("combine_bop checked lhs is a Sup")
        };
        let parts = self.heap.free_sup(sup);
        let labels: Vec<LabelId> = parts.iter().map(|(l, _)| *l).collect();
        let rhs = self.heap.pull(ra);
        let rs = self.dup_n(rhs, &labels);
        let new_parts = parts
            .into_iter()
            .zip(rs)
            .map(|((l, a), r)| (l, self.heap.alloc(Term::Bop { op, lhs: a, rhs: r })))
            .collect();
        self.sup_from(new_parts)
    }

    /// BOP-SUP (rhs superposed): `(l op &{a,b})` => `&{(l0 op a), (l1 op b)}`,
    /// duplicating `l` across the sup's wires.
    fn bop_sup_right(&self, op: BinaryOp, la: TermPtr<'h>, ra: TermPtr<'h>) -> Term<'h> {
        self.policy.next_step(InteractionType::BopSup);
        let Term::Sup { ptr: sup } = self.heap.pull(ra) else {
            unreachable!("combine_bop checked rhs is a Sup")
        };
        let parts = self.heap.free_sup(sup);
        let labels: Vec<LabelId> = parts.iter().map(|(l, _)| *l).collect();
        let lhs = self.heap.pull(la);
        let ls = self.dup_n(lhs, &labels);
        let new_parts = parts
            .into_iter()
            .zip(ls)
            .map(|((l, a), lp)| (l, self.heap.alloc(Term::Bop { op, lhs: lp, rhs: a })))
            .collect();
        self.sup_from(new_parts)
    }

    // ====================================================================
    // Duplication / superposition / match
    // ====================================================================

    /// Allocate a `Ref` node naming wire `label` of dup `cell`.
    fn ref_node(&self, cell: Addr, label: LabelId) -> TermPtr<'h> {
        self.heap.alloc(Term::Ref {
            ptr: unsafe { RefPtr::forge(cell, label) },
        })
    }

    /// Allocate an N-way dup over `value` with the given wire labels, returning one
    /// `Ref` node per label (in label order).
    fn dup_n(&self, value: Term<'h>, labels: &[LabelId]) -> Vec<TermPtr<'h>> {
        let cell = self.heap.alloc_dup_n(value, labels);
        labels.iter().map(|l| self.ref_node(cell, *l)).collect()
    }

    /// Build a superposition node over its labelled parts.
    fn sup_from(&self, parts: Vec<(LabelId, TermPtr<'h>)>) -> Term<'h> {
        Term::Sup {
            ptr: self.heap.alloc_sup_n(parts),
        }
    }

    /// The field count of a constructor `t::variant`, or `None` if the constructor
    /// doesn't match the type. The product constructor (`variant == None`) applies
    /// only to a product type (yielding its field count); a sum variant
    /// (`variant == Some(name)`) applies only to a sum type that declares `name`.
    fn ctor_arity(&self, t: &TypePtr<'h>, variant: Option<VariantId>) -> Option<usize> {
        match (self.heap.type_info(t), variant) {
            (TypeInfo::Product { fields, .. }, None) => Some(fields.len()),
            (TypeInfo::Sum { variants, .. }, Some(name)) => variants
                .iter()
                .find(|v| v.name == name)
                .map(|v| v.args.len()),
            // `::New` on a sum, or a named variant on a product: mismatch.
            _ => None,
        }
    }

    /// Duplicate each sub-type child node of a type N ways, returning one projected
    /// address list per wire label. Mirrors DUP-CTR over a pack.
    fn dup_arg_addrs_n(&self, labels: &[LabelId], args: Vec<Addr>) -> Vec<Vec<Addr>> {
        let mut outs: Vec<Vec<Addr>> = labels.iter().map(|_| Vec::with_capacity(args.len())).collect();
        for a in args {
            let field = self.heap.pull(unsafe { TermPtr::forge(a) });
            let refs = self.dup_n(field, labels);
            for (out, r) in outs.iter_mut().zip(refs) {
                out.push(r.into_addr());
            }
        }
        outs
    }

    /// Deep-duplicate an (affine) type value into one fresh type entry per wire
    /// label, distributing the dup into each lazy sub-type child (mirrors DUP-CTR).
    fn dup_type_n(&self, labels: &[LabelId], ty: TypePtr<'h>) -> Vec<TypePtr<'h>> {
        match self.heap.free_type(ty) {
            TypeInfo::Product { name, fields } => self
                .dup_arg_addrs_n(labels, fields)
                .into_iter()
                .map(|fields| {
                    self.heap.alloc_type(TypeInfo::Product {
                        name: name.clone(),
                        fields,
                    })
                })
                .collect(),
            TypeInfo::Sum { name, variants } => {
                let mut per_label: Vec<Vec<Variant>> =
                    labels.iter().map(|_| Vec::with_capacity(variants.len())).collect();
                for v in variants {
                    let dup_args = self.dup_arg_addrs_n(labels, v.args);
                    for (out, args) in per_label.iter_mut().zip(dup_args) {
                        out.push(Variant { name: v.name, args });
                    }
                }
                per_label
                    .into_iter()
                    .map(|variants| {
                        self.heap.alloc_type(TypeInfo::Sum {
                            name: name.clone(),
                            variants,
                        })
                    })
                    .collect()
            }
        }
    }

    /// Force one projection of a duplication. The first branch to acquire the
    /// cell lock reduces and fires the value (filling every projection slot) while
    /// holding the lock; later branches wake to a `None` value and read their own
    /// slot. Returns `None` when the dup is stuck (its value is an unsubstituted
    /// binder), leaving the cell untouched. When the value is itself another dup's
    /// projection, the two dups *combine* (this dup's wires are spliced into the
    /// inner cell) and the projection is retried against the merged cell.
    fn force_dup(&self, dp: RefPtr<'h>) -> Reduce<'_, Option<Term<'h>>> {
        Box::pin(async move {
            loop {
                let (mut guard, cell) = self.heap.dup_lock(dp).await;
                let seed = match self.heap.dup_take_value(&mut guard) {
                    None => {
                        // Already fired: read out this wire, reclaim the cell once
                        // every wire has been projected.
                        let (mine, drained) = self.heap.dup_project(dp, &mut guard);
                        drop(guard);
                        if drained {
                            self.heap.free_dup(cell);
                        }
                        return Some(mine);
                    }
                    Some(seed) => seed,
                };
                // Reduce the duplicand to WHNF in place (kept addressable so a stuck
                // dup can leave it untouched). A dup is stuck only when its value is
                // a bare binder `Var` (these arise under strong normalization).
                let vp = self.sub_whnf_at(seed).await;
                if !self.policy.should_continue() || matches!(&*self.heap.view(&vp), Term::Var) {
                    self.heap.dup_restore_value(&mut guard, vp);
                    drop(guard);
                    return None;
                }
                // The value is another dup's projection: combine. Splice this dup's
                // wires into the inner cell (in place of the consumed wire) and
                // forward this cell to it, then retry the projection against the
                // merged cell. This flattens dup chains into a single fan.
                if matches!(&*self.heap.view(&vp), Term::Ref { .. }) {
                    let Term::Ref { ptr: inner } = self.heap.pull(vp) else {
                        unreachable!()
                    };
                    self.policy.next_step(InteractionType::DupRef);
                    let (mut inner_guard, inner_cell) = self.heap.dup_lock(inner).await;
                    let consumed = inner.label();
                    let pos = inner_guard
                        .slots
                        .iter()
                        .position(|(l, _)| *l == consumed)
                        .expect("combination: consumed wire missing in inner cell");
                    debug_assert!(inner_guard.slots[pos].1.is_none());
                    inner_guard.slots.remove(pos);
                    let outer_slots = std::mem::take(&mut guard.slots);
                    let added = outer_slots.len();
                    inner_guard.slots.extend(outer_slots);
                    inner_guard.remaining = inner_guard.remaining - 1 + added;
                    drop(inner_guard);
                    guard.fwd = Some(inner_cell);
                    guard.remaining = 0;
                    drop(guard);
                    continue;
                }
                let labels: Vec<LabelId> = guard.slots.iter().map(|(l, _)| *l).collect();
                // DUP-SUP: annihilate when the labels match (this dup met the sup it
                // spawned), else commute. With globally-unique wire labels the two
                // sets are equal or disjoint, never partially overlapping.
                if matches!(&*self.heap.view(&vp), Term::Sup { .. }) {
                    let Term::Sup { ptr: sup } = self.heap.pull(vp) else {
                        unreachable!()
                    };
                    let sup_labels = self.heap.sup_labels(&sup);
                    if same_label_set(&labels, &sup_labels) {
                        self.policy.next_step(InteractionType::DupSup);
                        let parts = self.heap.free_sup(sup);
                        self.heap.dup_fire(&mut guard, parts);
                        let (mine, drained) = self.heap.dup_project(dp, &mut guard);
                        drop(guard);
                        if drained {
                            self.heap.free_dup(cell);
                        }
                        return Some(mine);
                    }
                    debug_assert!(
                        disjoint_label_set(&labels, &sup_labels),
                        "DUP-SUP wire labels partially overlap"
                    );
                    let fills = self.dup_sup_commute(labels, sup).await;
                    self.heap.dup_fire(&mut guard, fills);
                    let (mine, drained) = self.heap.dup_project(dp, &mut guard);
                    drop(guard);
                    if drained {
                        self.heap.free_dup(cell);
                    }
                    return Some(mine);
                }
                let head = self.heap.pull(vp);
                let fills = self.dup_head_n(labels, head).await;
                self.heap.dup_fire(&mut guard, fills);
                let (mine, drained) = self.heap.dup_project(dp, &mut guard);
                drop(guard);
                if drained {
                    self.heap.free_dup(cell);
                }
                return Some(mine);
            }
        })
    }

    /// DUP-SUP commute (disjoint labels): duplicate each of the sup's parts across
    /// this dup's wires, so each dup wire `l` projects a superposition over the
    /// sup's wires. Returns one filled projection node per dup wire.
    fn dup_sup_commute(
        &self,
        labels: Vec<LabelId>,
        sup: SupPtr<'h>,
    ) -> Reduce<'_, Vec<(LabelId, TermPtr<'h>)>> {
        Box::pin(async move {
            let labels = &labels;
            self.policy.next_step(InteractionType::DupSup);
            let sup_parts = self.heap.free_sup(sup);
            // by_proj[i] gathers, for dup wire `labels[i]`, the sup-part copies.
            let mut by_proj: Vec<Vec<(LabelId, TermPtr<'h>)>> =
                labels.iter().map(|_| Vec::with_capacity(sup_parts.len())).collect();
            for (m, s) in sup_parts {
                let refs = self.dup_n(self.heap.pull(s), labels);
                for (slot, r) in by_proj.iter_mut().zip(refs) {
                    slot.push((m, r));
                }
            }
            labels
                .iter()
                .zip(by_proj)
                .map(|(l, parts)| (*l, self.heap.alloc(self.sup_from(parts))))
                .collect()
        })
    }

    /// Produce one projection of duplicating `head` (already in WHNF) per dup wire
    /// in `labels`. Sub-terms are duplicated by allocating fresh N-way dups over
    /// concrete values, carrying the same wire labels. Returns one filled
    /// projection node per wire. (DUP-SUP is handled in `force_dup`.)
    fn dup_head_n(
        &self,
        labels: Vec<LabelId>,
        head: Term<'h>,
    ) -> Reduce<'_, Vec<(LabelId, TermPtr<'h>)>> {
        Box::pin(async move {
            let labels = &labels;
            match head {
                // copy leaves / atoms: duplicating a scalar value is a DUP-VAL.
                Term::Int(n) => self.dup_copy(labels, InteractionType::DupVal, || Term::Int(n)),
                Term::Float(x) => self.dup_copy(labels, InteractionType::DupVal, || Term::Float(x)),
                Term::Char(c) => self.dup_copy(labels, InteractionType::DupVal, || Term::Char(c)),
                Term::Bool(b) => self.dup_copy(labels, InteractionType::DupVal, || Term::Bool(b)),
                Term::Wld => self.dup_copy(labels, InteractionType::DupWld, || Term::Wld),
                Term::VarId(v) => self.dup_copy(labels, InteractionType::DupVal, || Term::VarId(v)),
                // `Term::Var` (an unsubstituted binder) never reaches here: a dup
                // over one is left stuck by `force_dup`.
                Term::Pri(id) => self.dup_copy(labels, InteractionType::DupPri, || Term::Pri(id)),
                Term::Err { backtrace, .. } => {
                    // An error is a first-class eraser; duplicating it yields errors.
                    // Any (affine) backtrace cannot be shared, so it is dropped here.
                    let _ = backtrace;
                    self.dup_copy(labels, InteractionType::DupVal, err_term)
                }
                Term::Box(v) => {
                    // The boxed payload is affine: one fresh entry per extra wire.
                    self.policy.next_step(InteractionType::DupVal);
                    let n = labels.len();
                    let mut vals: Vec<ValuePtr<'h>> =
                        (1..n).map(|_| self.heap.value_dup(&v)).collect();
                    vals.push(v);
                    labels
                        .iter()
                        .zip(vals)
                        .map(|(l, val)| (*l, self.heap.alloc(Term::Box(val))))
                        .collect()
                }
                Term::Type(t) => {
                    // A type value is affine: deep-dup it into one fresh entry per
                    // wire, distributing the dup into each (lazy) sub-type child.
                    self.policy.next_step(InteractionType::DupType);
                    labels
                        .iter()
                        .zip(self.dup_type_n(labels, t))
                        .map(|(l, t)| (*l, self.heap.alloc(Term::Type(t))))
                        .collect()
                }
                Term::Partial { func, arity, args } => {
                    // Distribute the dup into the callable and each gathered argument.
                    self.policy.next_step(InteractionType::DupCtr);
                    let fs = self.dup_n(self.heap.pull(func), labels);
                    let mut arg_lists: Vec<Vec<TermPtr<'h>>> =
                        labels.iter().map(|_| Vec::new()).collect();
                    for field in self.heap.into_fields(args) {
                        let refs = self.dup_n(self.heap.pull(field), labels);
                        for (al, r) in arg_lists.iter_mut().zip(refs) {
                            al.push(r);
                        }
                    }
                    labels
                        .iter()
                        .zip(fs)
                        .zip(arg_lists)
                        .map(|((l, f), a)| {
                            let p = self.heap.alloc_pack(None, a);
                            (*l, self.heap.alloc(Term::Partial { func: f, arity, args: p }))
                        })
                        .collect()
                }
                Term::Ctr { ty, variant } => {
                    // A stuck constructor selector distributes the dup into its operand.
                    self.policy.next_step(InteractionType::DupCtr);
                    let ts = self.dup_n(self.heap.pull(ty), labels);
                    labels
                        .iter()
                        .zip(ts)
                        .map(|(l, t)| (*l, self.heap.alloc(Term::Ctr { ty: t, variant })))
                        .collect()
                }
                Term::App { func, arg } => {
                    self.policy.next_step(InteractionType::DupApp);
                    let fs = self.dup_n(self.heap.pull(func), labels);
                    let xs = self.dup_n(self.heap.pull(arg), labels);
                    labels
                        .iter()
                        .zip(fs)
                        .zip(xs)
                        .map(|((l, f), x)| (*l, self.heap.alloc(Term::App { func: f, arg: x })))
                        .collect()
                }
                Term::Bop { op, lhs, rhs } => {
                    // A stuck binary op (an operand is an unsubstituted binder, as
                    // under a duplicated lambda) distributes into both operands.
                    self.policy.next_step(InteractionType::DupBop);
                    let ls = self.dup_n(self.heap.pull(lhs), labels);
                    let rs = self.dup_n(self.heap.pull(rhs), labels);
                    labels
                        .iter()
                        .zip(ls)
                        .zip(rs)
                        .map(|((l, lp), rp)| {
                            (*l, self.heap.alloc(Term::Bop { op, lhs: lp, rhs: rp }))
                        })
                        .collect()
                }
                Term::Uop { op, val } => {
                    self.policy.next_step(InteractionType::DupUop);
                    let vs = self.dup_n(self.heap.pull(val), labels);
                    labels
                        .iter()
                        .zip(vs)
                        .map(|(l, v)| (*l, self.heap.alloc(Term::Uop { op, val: v })))
                        .collect()
                }
                Term::Use { body } => {
                    self.policy.next_step(InteractionType::DupUse);
                    let bs = self.dup_n(self.heap.pull(body), labels);
                    labels
                        .iter()
                        .zip(bs)
                        .map(|(l, b)| (*l, self.heap.alloc(Term::Use { body: b })))
                        .collect()
                }
                Term::Lam { body } => {
                    self.policy.next_step(InteractionType::DupLam);
                    let (orig_binder, body_ptr) = self.heap.open_body(body);
                    let body_refs = self.dup_n(self.heap.pull(body_ptr), labels);
                    let mut occ_parts: Vec<(LabelId, TermPtr<'h>)> =
                        Vec::with_capacity(labels.len());
                    let mut out = Vec::with_capacity(labels.len());
                    for (l, bref) in labels.iter().zip(body_refs) {
                        let (h, occ) = self.heap.fresh_binder();
                        let lam = Term::Lam {
                            body: self.heap.close_body(h, bref),
                        };
                        out.push((*l, self.heap.alloc(lam)));
                        occ_parts.push((*l, occ));
                    }
                    // x ← &{ occ per wire }: the binder occurrence superposes the
                    // wires, so the body-dup annihilates against it downstream.
                    let sup_var = self.sup_from(occ_parts);
                    self.heap.fill_binder(orig_binder, sup_var);
                    out
                }
                Term::Ctn { ty, arity, values } => {
                    self.policy.next_step(InteractionType::DupCtr);
                    let nfields = arity as usize;
                    let name = self.heap.pack_name(&values);
                    let mut field_lists: Vec<Vec<TermPtr<'h>>> =
                        labels.iter().map(|_| Vec::with_capacity(nfields)).collect();
                    for i in 0..nfields {
                        let field = self.heap.pull(self.heap.pack_field(&values, i));
                        let refs = self.dup_n(field, labels);
                        for (fl, r) in field_lists.iter_mut().zip(refs) {
                            fl.push(r);
                        }
                    }
                    self.heap.free_pack(values);
                    let tys = self.dup_type_n(labels, ty);
                    labels
                        .iter()
                        .zip(field_lists)
                        .zip(tys)
                        .map(|((l, fields), t)| {
                            let p = self.heap.alloc_pack(name, fields);
                            (*l, self.heap.alloc(Term::Ctn { ty: t, arity, values: p }))
                        })
                        .collect()
                }
                other => unreachable!("DUP of an unexpected head: {other:?}"),
            }
        })
    }

    /// Make `labels.len()` identical copies of an atom/leaf term (DUP-VAL family).
    fn dup_copy(
        &self,
        labels: &[LabelId],
        kind: InteractionType,
        mut make: impl FnMut() -> Term<'h>,
    ) -> Vec<(LabelId, TermPtr<'h>)> {
        self.policy.next_step(kind);
        labels
            .iter()
            .map(|l| (*l, self.heap.alloc(make())))
            .collect()
    }

    /// APP-SUP: `(&{f,g}) arg` => `&{(f a0), (g a1)}`, duplicating `arg` across the
    /// sup's wires.
    fn app_sup(&self, sup: SupPtr<'h>, arg: TermPtr<'h>) -> Term<'h> {
        let parts = self.heap.free_sup(sup);
        let labels: Vec<LabelId> = parts.iter().map(|(l, _)| *l).collect();
        let args = self.dup_n(self.heap.pull(arg), &labels);
        let new_parts = parts
            .into_iter()
            .zip(args)
            .map(|((l, f), a)| (l, self.heap.alloc(Term::App { func: f, arg: a })))
            .collect();
        self.sup_from(new_parts)
    }

    /// Whether a WHNF pattern key matches the (already-WHNF) scrutinee. A
    /// constructor scrutinee matches a `VarId` key naming the same variant; a
    /// value scrutinee matches an equal value key.
    fn key_matches(&self, scrut: &Term<'h>, key: &Term<'h>) -> bool {
        match (scrut, key) {
            (Term::Ctn { values, .. }, Term::VarId(v)) => self.heap.pack_name(values) == Some(*v),
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
            Term::Ctn { arity, values, .. } => {
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

    /// Apply a primitive to its (gathered, unforced) argument pointers: hand each
    /// as a [`Handle`] to the extension, which forces what it needs and returns a
    /// result; any argument it drops is reclaimed here.
    async fn fire_prim(&self, id: PrimId, arg_ptrs: Vec<TermPtr<'h>>) -> TermPtr<'h> {
        let args: Vec<Handle<'h>> = arg_ptrs
            .into_iter()
            .map(|p| Handle::new(p, self.heap))
            .collect();
        self.policy.next_step(InteractionType::AppPri);
        let result = self.extensions.apply(self, id, args).await.into_term_ptr();
        self.erase_dropped_handles().await;
        result
    }

    /// Complete a saturated [`Term::Partial`]: build the construction (for a
    /// constructor callable) or fire the primitive. `func` is the callable node and
    /// `fields` the gathered (full) argument list. Returns the result node.
    async fn complete_partial(&self, func: TermPtr<'h>, fields: Vec<TermPtr<'h>>) -> TermPtr<'h> {
        let arity = fields.len() as u8;
        // `func` may be a dup projection (after duplicating a partial); force it.
        let nf = self.sub_whnf_at(func).await;
        match self.heap.pull(nf) {
            Term::Ctr { ty, variant } => {
                let nt = self.sub_whnf_at(ty).await;
                match self.heap.pull(nt) {
                    Term::Type(t) => {
                        let values = self.heap.alloc_pack(variant, fields);
                        self.heap.alloc(Term::Ctn {
                            ty: t,
                            arity,
                            values,
                        })
                    }
                    other => {
                        self.erase(other);
                        for f in fields {
                            self.erase(self.heap.pull(f));
                        }
                        self.heap.alloc(err_term())
                    }
                }
            }
            Term::Pri(id) => self.fire_prim(id, fields).await,
            other => {
                self.erase(other);
                for f in fields {
                    self.erase(self.heap.pull(f));
                }
                self.heap.alloc(err_term())
            }
        }
    }

    /// Classify a reduced type operand (the `ty` of a [`Term::Ctr`]): a resolved
    /// type value, a not-yet-resolved head (a binder/dup/sup/redex) that leaves the
    /// selector stuck so a surrounding dup can distribute into it, or a concrete
    /// non-type (an error).
    fn classify_type_arg(&self, np: &TermPtr<'h>) -> ArgClass {
        match &*self.heap.view(np) {
            Term::Type(_) => ArgClass::Type,
            Term::Var
            | Term::Ref { .. }
            | Term::Sup { .. }
            | Term::App { .. }
            | Term::Bop { .. }
            | Term::Uop { .. }
            | Term::Partial { .. } => ArgClass::Stuck,
            _ => ArgClass::Err,
        }
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
            Term::Sup { ptr } => {
                for i in 0..self.heap.sup_len(&ptr) {
                    let a = unsafe { TermPtr::forge(self.heap.sup_part_addr(&ptr, i)) };
                    let na = self.sub_normalize_at(a).await;
                    self.heap.set_sup_part(&ptr, i, na.into_addr());
                }
                slot.finished(Term::Sup { ptr })
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
            Term::Ctn { ty, arity, values } => {
                for i in 0..arity as usize {
                    let f = self.heap.pack_field(&values, i);
                    let nf = self.sub_normalize_at(f).await;
                    self.heap.set_pack_field(&values, i, nf);
                }
                // The type's sub-types are left lazy (not normalized).
                slot.finished(Term::Ctn { ty, arity, values })
            }
            Term::Partial { func, arity, args } => {
                let nf = self.sub_normalize_at(func).await;
                for i in 0..self.heap.pack_len(&args) {
                    let a = self.heap.pack_field(&args, i);
                    let na = self.sub_normalize_at(a).await;
                    self.heap.set_pack_field(&args, i, na);
                }
                slot.finished(Term::Partial {
                    func: nf,
                    arity,
                    args,
                })
            }
            Term::Ctr { ty, variant } => {
                let nt = self.sub_normalize_at(ty).await;
                slot.finished(Term::Ctr { ty: nt, variant })
            }
            Term::Type(t) => {
                // Sub-types stay lazy (genuine redexes are not reduced), but the
                // administrative `Dup`/`Sup` nodes a `DUP-LAM` threads through them
                // must be resolved so a substituted binder actually reaches the
                // field (e.g. `(\T -> type { Cons(T), .. }) X`).
                let t = self.resolve_type_fields(t).await;
                slot.finished(Term::Type(t))
            }
            other => slot.finished(other),
        }
    }

    /// Resolve the administrative dup/sup bookkeeping inside a type's lazy
    /// sub-fields, rebuilding the (affine) type value. Genuine sub-type redexes
    /// are left untouched — only the substitution plumbing is settled.
    fn resolve_type_fields(&self, ty: TypePtr<'h>) -> Reduce<'_, TypePtr<'h>> {
        Box::pin(async move {
            match self.heap.free_type(ty) {
                TypeInfo::Product { name, fields } => {
                    let mut nf = Vec::with_capacity(fields.len());
                    for a in fields {
                        nf.push(self.resolve_lazy_field(a).await);
                    }
                    self.heap.alloc_type(TypeInfo::Product { name, fields: nf })
                }
                TypeInfo::Sum { name, variants } => {
                    let mut nv = Vec::with_capacity(variants.len());
                    for v in variants {
                        let mut na = Vec::with_capacity(v.args.len());
                        for a in v.args {
                            na.push(self.resolve_lazy_field(a).await);
                        }
                        nv.push(Variant {
                            name: v.name,
                            args: na,
                        });
                    }
                    self.heap.alloc_type(TypeInfo::Sum {
                        name,
                        variants: nv,
                    })
                }
            }
        })
    }

    /// Settle one lazy sub-field address: fire a *ready* administrative `Dup`
    /// (one whose duplicand is already a sup/value, so no redex is reduced),
    /// recurse through `Sup`s and nested types, and otherwise leave the field as
    /// written. Returns the (possibly relocated) field address.
    fn resolve_lazy_field(&self, addr: Addr) -> Reduce<'_, Addr> {
        Box::pin(async move {
            let ptr = unsafe { TermPtr::forge(addr) };
            enum K<'h> {
                Dup(RefPtr<'h>),
                Sup(Addr),
                Type,
                Leave,
            }
            let k = match &*self.heap.view(&ptr) {
                Term::Ref { ptr: dp } => K::Dup(*dp),
                Term::Sup { ptr: sp } => K::Sup(sp.addr()),
                Term::Type(_) => K::Type,
                _ => K::Leave,
            };
            match k {
                K::Dup(dp) if self.dup_is_ready(&dp) => {
                    let r = self.sub_whnf_at(ptr).await;
                    // If it didn't reduce past a dup (genuinely stuck — e.g. a
                    // fired chain bottoming at an unfilled binder, or the budget
                    // was spent), leave it lazy instead of retrying forever.
                    if matches!(&*self.heap.view(&r), Term::Ref { .. }) {
                        r.into_addr()
                    } else {
                        self.resolve_lazy_field(r.into_addr()).await
                    }
                }
                K::Dup(_) | K::Leave => addr,
                K::Sup(sup_addr) => {
                    let sp = unsafe { SupPtr::forge(sup_addr) };
                    for i in 0..self.heap.sup_len(&sp) {
                        let part = self.heap.sup_part_addr(&sp, i);
                        let n = self.resolve_lazy_field(part).await;
                        self.heap.set_sup_part(&sp, i, n);
                    }
                    addr
                }
                K::Type => {
                    let Term::Type(t) = self.heap.pull(ptr) else {
                        unreachable!()
                    };
                    let t = self.resolve_type_fields(t).await;
                    self.heap.alloc(Term::Type(t)).into_addr()
                }
            }
        })
    }

    /// Whether a `Dup`'s duplicand is settled enough to fire without reducing a
    /// genuine redex: a fired dup, or (transitively) a value/sup head. A nested
    /// `Dup` is followed — firing it just routes a projection, no work is forced —
    /// so chained duplications (a type fn reused across evaluations) resolve; an
    /// app/bop/binder duplicand stays lazy.
    fn dup_is_ready(&self, dp: &RefPtr<'h>) -> bool {
        // Follow a chain of dups iteratively (not recursively) so a deep chain
        // can't overflow the stack here.
        let mut value = self.heap.dup_value(dp);
        loop {
            let Some(v) = value else { return true }; // fired dup
            match &*self.heap.view_at(v) {
                Term::App { .. }
                | Term::Bop { .. }
                | Term::Uop { .. }
                | Term::Use { .. }
                | Term::Mat { .. }
                | Term::Var => return false,
                Term::Ref { ptr } => value = self.heap.dup_value(ptr),
                _ => return true,
            }
        }
    }
}

/// Whether two wire-label sets are equal (as sets). With globally-unique labels a
/// dup meets the sup it spawned exactly when their label sets coincide.
fn same_label_set(a: &[LabelId], b: &[LabelId]) -> bool {
    a.len() == b.len() && a.iter().all(|l| b.contains(l))
}

/// Whether two wire-label sets are disjoint (the DUP-SUP commute case).
fn disjoint_label_set(a: &[LabelId], b: &[LabelId]) -> bool {
    a.iter().all(|l| !b.contains(l))
}

/// Whether a WHNF scrutinee is a concrete value a match can fire on (a
/// constructor or a primitive value leaf). Any other head leaves the match inert.
fn is_matchable(scrut: &Term) -> bool {
    matches!(
        scrut,
        Term::Ctn { .. }
            | Term::Int(_)
            | Term::Float(_)
            | Term::Bool(_)
            | Term::Char(_)
            | Term::Box(_)
    )
}

/// The classification of a reduced type operand (see
/// [`Executor::classify_type_arg`]).
enum ArgClass {
    /// resolved to a type value.
    Type,
    /// not yet a value (a binder/dup/sup/redex): leave the selector stuck.
    Stuck,
    /// a concrete non-type value: a type error.
    Err,
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
        Term::Int(_) | Term::Float(_) | Term::Bool(_) | Term::Char(_) | Term::Box(_)
    )
}

/// Floor division of two `i64`s (rounds toward negative infinity, Python-style),
/// in contrast to Rust's truncating `/`. Caller guarantees `b != 0`.
fn floor_div_i64(a: i64, b: i64) -> i64 {
    let q = a.wrapping_div(b);
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
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
