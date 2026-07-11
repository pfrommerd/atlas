//! The [`Executor`]: interaction-calculus evaluation over a branded [`HeapScope`].
//!
//! v1 is a synchronous, single-task evaluator over the affine heap model. It
//! covers the affine core (APP-LAM / APP-USE / APP-ERA, binary ops, constructors
//! as data, and full normalization). The duplication / superposition / match
//! interactions and the parallel async driver are deferred to a later increment.

use crate::extension::{Extensions, Handle, NoExtensions, TermPtrLike};
use crate::vm::heap::{
    Addr, Boxed, DupDrop, DupPtr, HeapScope, MatchData, MatchPtr, Spine, SupPtr, TermPtr, TypeInfo,
    TypePtr, ValuePtr, Variant,
};
use crate::vm::term::{BinaryOp, LabelId, PrimId, Term, UnaryOp, VariantId};
use ordered_float::OrderedFloat;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A boxed reduction future. Boxed so the (mutually) recursive async reduction
/// methods can call one another; the parallel driver will later add a `Send`
/// bound here.
type Reduce<'s, T> = Pin<Box<dyn Future<Output = T> + 's>>;

enum DupForce<'h> {
    Term(Term<'h>),
    Rewritten,
    Stuck,
}

/// The kind of interaction performed in a single reduction step.
#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InteractionType {
    AppLam, AppUse, AppEra, AppErr, AppSup, AppMat, AppPri, AppCtr,
    TypeDef, Variant,
    DupLam, DupSup, DupCtr, DupType, DupApp, DupBop, DupUop, DupMat, DupNum, DupWld, DupVar, DupUse, DupPri, DupVal, DupErase, DupCollapse,
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
    extension_error: Mutex<Option<String>>,
}

impl<'e, 'h, P: ExecPolicy> Executor<'e, 'h, P, NoExtensions> {
    pub fn new(heap: &'h HeapScope<'h>, policy: P) -> Self {
        Executor {
            heap,
            extensions: NO_EXTENSIONS,
            policy,
            extension_error: Mutex::new(None),
        }
    }
}

impl<'e, 'h, P: ExecPolicy, X: Extensions> Executor<'e, 'h, P, X> {
    pub fn with_extensions(heap: &'h HeapScope<'h>, policy: P, extensions: &'e X) -> Self {
        Executor {
            heap,
            extensions,
            policy,
            extension_error: Mutex::new(None),
        }
    }

    /// Return and clear the first error raised by an extension primitive.
    pub fn take_extension_error(&self) -> Option<String> {
        self.extension_error.lock().unwrap().take()
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
            Term::Lam { body, .. } => {
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
            Term::Var { cell } => self.heap.drop_var(cell),
            Term::Wld
            | Term::Err { .. }
            | Term::Int(_)
            | Term::Float(_)
            | Term::Char(_)
            | Term::Bool(_)
            | Term::Pri(_)
            | Term::VarId(_)
            | Term::Null => {}
            // A dup projection: dropping one side rewrites the surviving
            // projection's parent directly; dropping both reclaims the duplicand.
            Term::Dup { ptr, .. } => match self.heap.dup_drop_side(ptr) {
                DupDrop::Recorded { dead } => self.erase_all(dead),
                DupDrop::Reclaim(p) => self.erase(self.heap.pull(p)),
            },
            // A superposition owns its cell (`SupPtr` is affine): reclaim both
            // branches.
            Term::Sup { ptr, .. } => {
                let (a, b) = self.heap.free_sup(ptr);
                self.erase(self.heap.pull(a));
                self.erase(self.heap.pull(b));
            }
            // A match table: same reclaim pattern as `fire_mat` — copy the
            // case/default addresses out, free the table, erase every key,
            // branch, and default.
            Term::Mat { matches } => {
                let (cases, default) = {
                    let data = self.heap.match_data(&matches);
                    (data.cases.clone(), data.default)
                };
                self.heap.free_match(matches);
                for (key, branch) in cases {
                    self.erase(self.heap.pull(unsafe { TermPtr::forge(key) }));
                    self.erase(self.heap.pull(unsafe { TermPtr::forge(branch) }));
                }
                if let Some(d) = default {
                    self.erase(self.heap.pull(unsafe { TermPtr::forge(d) }));
                }
            }
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

    /// Erase a batch of owned pointers (dead sup components handed back by the
    /// dup drop/flush paths).
    fn erase_all(&self, ptrs: Vec<TermPtr<'h>>) {
        for p in ptrs {
            self.erase(self.heap.pull(p));
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
                Term::Lam { var, body } => {
                    if matches!(spine.peek(), Some(Term::App { .. })) {
                        let (app_slot, app_cont) = spine.pop().unwrap();
                        let Term::App { func: _, arg } = app_cont else {
                            unreachable!()
                        };
                        let arg_term = self.heap.pull(arg);
                        let body_ptr = self.heap.substitute(var, body, arg_term);
                        self.heap.remove_slot(app_slot);
                        self.heap.remove_slot(slot);
                        self.policy.next_step(InteractionType::AppLam);
                        let (s, t) = self.heap.term(body_ptr);
                        slot = s;
                        term = t;
                        continue;
                    }
                    term = Term::Lam { var, body };
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
                Term::Dup { label, ptr: dp } => {
                    let cur = unsafe { slot.unchanged() };
                    match self.force_dup(label, dp).await {
                        DupForce::Term(t) => {
                            let (s, _) = self.heap.term(cur);
                            slot = s;
                            term = t;
                            continue;
                        }
                        DupForce::Rewritten => {
                            let (s, t) = self.heap.term(cur);
                            slot = s;
                            term = t;
                            continue;
                        }
                        // Stuck: a dup over an unsubstituted binder. Leave the
                        // `Dup` as an inert head and unwind.
                        DupForce::Stuck => {
                            let (s, _) = self.heap.term(cur);
                            slot = s;
                            term = Term::Dup { label, ptr: dp };
                        }
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
                Term::Var { .. } | Term::Dup { .. } => TyOf::Stuck,
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
        let (d0, d1) = self.alloc_dup_c(rhs);
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
        let (d0, d1) = self.alloc_dup_c(lhs);
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

    /// Allocate a child duplication, eagerly collapsing an inner dup that will
    /// never copy (see [`HeapScope::alloc_dup_collapsing`]) and accounting the
    /// collapse as an interaction.
    fn alloc_dup_c(&self, val: Term<'h>) -> (DupPtr<'h>, DupPtr<'h>) {
        let (a, b, collapsed, dead) = self.heap.alloc_dup_collapsing(val);
        if collapsed {
            self.policy.next_step(InteractionType::DupCollapse);
        }
        self.erase_all(dead);
        (a, b)
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

    /// Duplicate each sub-type child node of a type, returning the two projected
    /// address lists (one per dup side). Mirrors DUP-CTR over a pack.
    fn dup_arg_addrs(&self, label: LabelId, args: Vec<Addr>) -> (Vec<Addr>, Vec<Addr>) {
        let mut a0 = Vec::with_capacity(args.len());
        let mut a1 = Vec::with_capacity(args.len());
        for a in args {
            let field = self.heap.pull(unsafe { TermPtr::forge(a) });
            let (d0, d1) = self.alloc_dup_c(field);
            a0.push(self.dp_node(label, d0).into_addr());
            a1.push(self.dp_node(label, d1).into_addr());
        }
        (a0, a1)
    }

    /// Deep-duplicate an (affine) type value into two fresh type entries,
    /// distributing the dup into each lazy sub-type child (mirrors DUP-CTR).
    fn dup_type(&self, label: LabelId, ty: TypePtr<'h>) -> (TypePtr<'h>, TypePtr<'h>) {
        match self.heap.free_type(ty) {
            TypeInfo::Product { name, fields } => {
                let (f0, f1) = self.dup_arg_addrs(label, fields);
                (
                    self.heap.alloc_type(TypeInfo::Product {
                        name: name.clone(),
                        fields: f0,
                    }),
                    self.heap.alloc_type(TypeInfo::Product { name, fields: f1 }),
                )
            }
            TypeInfo::Sum { name, variants } => {
                let mut v0 = Vec::with_capacity(variants.len());
                let mut v1 = Vec::with_capacity(variants.len());
                for v in variants {
                    let (a0, a1) = self.dup_arg_addrs(label, v.args);
                    v0.push(Variant {
                        name: v.name,
                        args: a0,
                    });
                    v1.push(Variant {
                        name: v.name,
                        args: a1,
                    });
                }
                (
                    self.heap.alloc_type(TypeInfo::Sum {
                        name: name.clone(),
                        variants: v0,
                    }),
                    self.heap.alloc_type(TypeInfo::Sum { name, variants: v1 }),
                )
            }
        }
    }

    /// Build a sup term over two owned component nodes.
    fn sup_term(&self, label: LabelId, a: TermPtr<'h>, b: TermPtr<'h>) -> Term<'h> {
        let ptr = self.heap.sup(a, b);
        Term::Sup { label, ptr }
    }

    /// Force one projection of a duplication. The first branch to acquire the
    /// cell lock reduces and fires the value (publishing the other projection)
    /// while holding the lock; the second wakes to a `None` value and reads its
    /// projection slot. Returns `None` when the dup is stuck (its value is an
    /// unsubstituted binder), leaving the cell untouched.
    fn force_dup(&self, label: LabelId, dp: DupPtr<'h>) -> Reduce<'_, DupForce<'h>> {
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
                    // the duplicand); the next step resumes reducing it. Before
                    if !self.policy.should_continue()
                        || matches!(&*self.heap.view(&vp), Term::Var { .. } | Term::Dup { .. })
                    {
                        self.heap.dup_restore_value(&mut guard, vp);
                        drop(guard);
                        return DupForce::Stuck;
                    }
                    // DUP-SUP annihilation (same label): side `s` of the dup *is*
                    // component `s` of the sup. A bare variable component here
                    // means strong normalization reached a lambda binder without
                    // first firing its substitution, which violates the design
                    // invariant.
                    if matches!(&*self.heap.view(&vp), Term::Sup { label: slab, .. } if *slab == label)
                    {
                        let Term::Sup { .. } = &*self.heap.view(&vp) else {
                            unreachable!()
                        };
                        let Term::Sup { ptr: sup, .. } = self.heap.pull(vp) else {
                            unreachable!()
                        };
                        self.policy.next_step(InteractionType::DupSup);
                        let (a, b) = self.heap.free_sup(sup);
                        let (own, other) = if dp.side() { (a, b) } else { (b, a) };
                        let mine = self.heap.pull(own);
                        let other = self.heap.pull(other);
                        match self.heap.dup_rewrite_other(dp, &guard, other) {
                            Ok(waiter) => {
                                drop(guard);
                                if !waiter {
                                    self.heap.free_dup(dp);
                                }
                                return DupForce::Term(mine);
                            }
                            Err(other) => {
                                // The other side was dropped: no one will project it.
                                drop(guard);
                                self.heap.free_dup(dp);
                                self.erase(other);
                                return DupForce::Term(mine);
                            }
                        }
                    }
                    // The general copying step is elided when the other side was
                    // dropped: hand the reduced value through UNCOPIED (the whnf
                    // loop keeps reducing it) and reclaim the cell. Freeing from
                    // the winner cannot strand a waiter, since a dropped side
                    // never arrives at the eval lock.
                    if self.heap.dup_other_dropped(dp) {
                        self.policy.next_step(InteractionType::DupErase);
                        let mine = self.heap.pull(vp);
                        drop(guard);
                        self.heap.free_dup(dp);
                        return DupForce::Term(mine);
                    }
                    let head = self.heap.pull(vp);
                    let (s0, s1) = self.dup_head(label, dp, head).await;
                    // Fire: overwrite both projection parents unless the other
                    // side was dropped while `dup_head` ran — then discard the
                    // dead copy and only overwrite this side.
                    let (own, other) = if dp.side() { (s0, s1) } else { (s1, s0) };
                    match self.heap.dup_rewrite_other(dp, &guard, other) {
                        Ok(waiter) => {
                            drop(guard);
                            if !waiter {
                                self.heap.free_dup(dp);
                            }
                            DupForce::Term(own)
                        }
                        Err(other) => {
                            drop(guard);
                            self.heap.free_dup(dp);
                            self.erase(other);
                            DupForce::Term(own)
                        }
                    }
                }
                None => {
                    // The other branch fired and rewrote this parent node.
                    drop(guard);
                    self.heap.free_dup(dp);
                    DupForce::Rewritten
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
                    // A type value is affine: deep-dup it into two fresh entries,
                    // distributing the dup into each (lazy) sub-type child.
                    self.policy.next_step(InteractionType::DupType);
                    let (t0, t1) = self.dup_type(label, t);
                    (Term::Type(t0), Term::Type(t1))
                }
                Term::Partial { func, arity, args } => {
                    // Distribute the dup into the callable and each gathered argument.
                    self.policy.next_step(InteractionType::DupCtr);
                    let f = self.heap.pull(func);
                    let (df0, df1) = self.alloc_dup_c(f);
                    let mut a0 = Vec::new();
                    let mut a1 = Vec::new();
                    for field in self.heap.into_fields(args) {
                        let (d0, d1) = self.alloc_dup_c(self.heap.pull(field));
                        a0.push(self.dp_node(label, d0));
                        a1.push(self.dp_node(label, d1));
                    }
                    let p0 = self.heap.alloc_pack(None, a0);
                    let p1 = self.heap.alloc_pack(None, a1);
                    (
                        Term::Partial {
                            func: self.dp_node(label, df0),
                            arity,
                            args: p0,
                        },
                        Term::Partial {
                            func: self.dp_node(label, df1),
                            arity,
                            args: p1,
                        },
                    )
                }
                Term::Ctr { ty, variant } => {
                    // A stuck constructor selector distributes the dup into its operand.
                    self.policy.next_step(InteractionType::DupCtr);
                    let t = self.heap.pull(ty);
                    let (d0, d1) = self.alloc_dup_c(t);
                    (
                        Term::Ctr {
                            ty: self.dp_node(label, d0),
                            variant,
                        },
                        Term::Ctr {
                            ty: self.dp_node(label, d1),
                            variant,
                        },
                    )
                }
                Term::App { func, arg } => {
                    self.policy.next_step(InteractionType::DupApp);
                    let f = self.heap.pull(func);
                    let x = self.heap.pull(arg);
                    let (df0, df1) = self.alloc_dup_c(f);
                    let (dx0, dx1) = self.alloc_dup_c(x);
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
                    let (dl0, dl1) = self.alloc_dup_c(l);
                    let (dr0, dr1) = self.alloc_dup_c(r);
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
                    let (dv0, dv1) = self.alloc_dup_c(v);
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
                    let (d0, d1) = self.alloc_dup_c(b);
                    (
                        Term::Use {
                            body: self.dp_node(label, d0),
                        },
                        Term::Use {
                            body: self.dp_node(label, d1),
                        },
                    )
                }
                Term::Lam { var, body } => {
                    self.policy.next_step(InteractionType::DupLam);
                    let (orig_binder, body_ptr) = self.heap.open_body(var, body);
                    let body_term = self.heap.pull(body_ptr);
                    let (dg0, dg1) = self.alloc_dup_c(body_term);
                    let (h0, occ0) = self.heap.fresh_binder();
                    let (h1, occ1) = self.heap.fresh_binder();
                    let lam0 = Term::Lam {
                        var: h0,
                        body: self.dp_node(label, dg0),
                    };
                    let lam1 = Term::Lam {
                        var: h1,
                        body: self.dp_node(label, dg1),
                    };
                    // x ← &L{ occ0, occ1 }: the sup is the copies' shared
                    // binder (component i feeds copy i). If one dup half is
                    // dropped we currently leak the unselected sup half.
                    let sup_ptr = self.heap.sup(occ0, occ1);
                    self.heap.fill_binder(
                        orig_binder,
                        Term::Sup {
                            label,
                            ptr: sup_ptr,
                        },
                    );
                    (lam0, lam1)
                }
                Term::Ctn { ty, arity, values } => {
                    self.policy.next_step(InteractionType::DupCtr);
                    let n = arity as usize;
                    let name = self.heap.pack_name(&values);
                    let mut f0 = Vec::with_capacity(n);
                    let mut f1 = Vec::with_capacity(n);
                    for i in 0..n {
                        let field = self.heap.pull(self.heap.pack_field(&values, i));
                        let (di0, di1) = self.alloc_dup_c(field);
                        f0.push(self.dp_node(label, di0));
                        f1.push(self.dp_node(label, di1));
                    }
                    self.heap.free_pack(values);
                    let p0 = self.heap.alloc_pack(name, f0);
                    let p1 = self.heap.alloc_pack(name, f1);
                    // The type is affine, so deep-dup it for the two copies.
                    let (t0, t1) = self.dup_type(label, ty);
                    (
                        Term::Ctn {
                            ty: t0,
                            arity,
                            values: p0,
                        },
                        Term::Ctn {
                            ty: t1,
                            arity,
                            values: p1,
                        },
                    )
                }
                Term::VarId(v) => {
                    self.policy.next_step(InteractionType::DupVal);
                    (Term::VarId(v), Term::VarId(v))
                }
                Term::Err { backtrace, .. } => {
                    // An error is a first-class eraser; duplicating it yields two
                    // errors. Any (affine) backtrace cannot be shared, so it is
                    // dropped here.
                    self.policy.next_step(InteractionType::DupVal);
                    let _ = backtrace;
                    (err_term(), err_term())
                }
                Term::Mat { matches } => {
                    self.policy.next_step(InteractionType::DupMat);
                    let (cases, default) = {
                        let data = self.heap.match_data(&matches);
                        (data.cases.clone(), data.default)
                    };
                    self.heap.free_match(matches);

                    let mut cases0 = Vec::with_capacity(cases.len());
                    let mut cases1 = Vec::with_capacity(cases.len());
                    for (key_addr, branch_addr) in cases {
                        let key = self.heap.pull(unsafe { TermPtr::forge(key_addr) });
                        let branch = self.heap.pull(unsafe { TermPtr::forge(branch_addr) });
                        let (k0, k1) = self.alloc_dup_c(key);
                        let (b0, b1) = self.alloc_dup_c(branch);
                        cases0.push((
                            self.dp_node(label, k0).into_addr(),
                            self.dp_node(label, b0).into_addr(),
                        ));
                        cases1.push((
                            self.dp_node(label, k1).into_addr(),
                            self.dp_node(label, b1).into_addr(),
                        ));
                    }

                    let (default0, default1) = if let Some(default_addr) = default {
                        let default = self.heap.pull(unsafe { TermPtr::forge(default_addr) });
                        let (d0, d1) = self.alloc_dup_c(default);
                        (
                            Some(self.dp_node(label, d0).into_addr()),
                            Some(self.dp_node(label, d1).into_addr()),
                        )
                    } else {
                        (None, None)
                    };

                    (
                        Term::Mat {
                            matches: self.heap.alloc_match(MatchData {
                                cases: cases0,
                                default: default0,
                            }),
                        },
                        Term::Mat {
                            matches: self.heap.alloc_match(MatchData {
                                cases: cases1,
                                default: default1,
                            }),
                        },
                    )
                }
                Term::Sup {
                    label: slab,
                    ptr: sup,
                } => {
                    // Same-label annihilation is intercepted (and wired) in
                    // `force_dup`; only the different-label commute reaches here.
                    debug_assert!(label != slab);
                    self.policy.next_step(InteractionType::DupSup);
                    let (a, b) = self.heap.free_sup(sup);
                    let ta = self.heap.pull(a);
                    let tb = self.heap.pull(b);
                    let (da0, da1) = self.alloc_dup_c(ta);
                    let (db0, db1) = self.alloc_dup_c(tb);
                    let s0 =
                        self.sup_term(slab, self.dp_node(label, da0), self.dp_node(label, db0));
                    let s1 =
                        self.sup_term(slab, self.dp_node(label, da1), self.dp_node(label, db1));
                    (s0, s1)
                }
                other => unreachable!("DUP of an unexpected head: {other:?}"),
            }
        })
    }

    /// APP-SUP: `(&L{f,g}) arg` => `!d&L=arg; &L{(f d0), (g d1)}`.
    fn app_sup(&self, label: LabelId, sup: SupPtr<'h>, arg: TermPtr<'h>) -> Term<'h> {
        let (f, g) = self.heap.free_sup(sup);
        let arg_term = self.heap.pull(arg);
        let (d0, d1) = self.alloc_dup_c(arg_term);
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
            (None, Some(d)) => {
                let default = unsafe { TermPtr::forge(d) };
                let scrut_ptr = self.heap.alloc(scrut);
                let acc = self.heap.alloc(Term::App {
                    func: default,
                    arg: scrut_ptr,
                });
                self.policy.next_step(InteractionType::AppMat);
                return self.heap.pull(acc);
            }
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
        let result = match self.extensions.apply(self, id, args).await {
            Ok(result) => result.into_term_ptr(),
            Err(error) => {
                self.extension_error.lock().unwrap().get_or_insert(error);
                self.heap.alloc(Term::Wld)
            }
        };
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
            Term::Var { .. }
            | Term::Dup { .. }
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
            Term::Lam { var, body } => {
                let (binder, body_ptr) = self.heap.open_body(var, body);
                let nb = self.sub_normalize_at(body_ptr).await;
                self.heap.finish_slot(
                    slot,
                    Term::Lam {
                        var: binder,
                        body: nb,
                    },
                )
            }
            Term::Use { body } => {
                let nb = self.sub_normalize_at(body).await;
                self.heap.finish_slot(slot, Term::Use { body: nb })
            }
            Term::App { func, arg } => {
                let nf = self.sub_normalize_at(func).await;
                let na = self.sub_normalize_at(arg).await;
                self.heap.finish_slot(slot, Term::App { func: nf, arg: na })
            }
            Term::Sup { label, ptr } => {
                let (a, b) = self.heap.sup_args(&ptr);
                let na = self.sub_normalize_at(a).await;
                let nb = self.sub_normalize_at(b).await;
                self.heap.set_sup_args(&ptr, na, nb);
                self.heap.finish_slot(slot, Term::Sup { label, ptr })
            }
            Term::Bop { op, lhs, rhs } => {
                let nl = self.sub_normalize_at(lhs).await;
                let nr = self.sub_normalize_at(rhs).await;
                self.heap.finish_slot(
                    slot,
                    Term::Bop {
                        op,
                        lhs: nl,
                        rhs: nr,
                    },
                )
            }
            Term::Uop { op, val } => {
                let nv = self.sub_normalize_at(val).await;
                self.heap.finish_slot(slot, Term::Uop { op, val: nv })
            }
            Term::Ctn { ty, arity, values } => {
                for i in 0..arity as usize {
                    let f = self.heap.pack_field(&values, i);
                    let nf = self.sub_normalize_at(f).await;
                    self.heap.set_pack_field(&values, i, nf);
                }
                // The type's sub-types are left lazy (not normalized).
                self.heap.finish_slot(slot, Term::Ctn { ty, arity, values })
            }
            Term::Partial { func, arity, args } => {
                let nf = self.sub_normalize_at(func).await;
                for i in 0..self.heap.pack_len(&args) {
                    let a = self.heap.pack_field(&args, i);
                    let na = self.sub_normalize_at(a).await;
                    self.heap.set_pack_field(&args, i, na);
                }
                self.heap.finish_slot(
                    slot,
                    Term::Partial {
                        func: nf,
                        arity,
                        args,
                    },
                )
            }
            Term::Ctr { ty, variant } => {
                let nt = self.sub_normalize_at(ty).await;
                self.heap.finish_slot(slot, Term::Ctr { ty: nt, variant })
            }
            Term::Type(t) => {
                // Sub-types stay lazy (genuine redexes are not reduced), but the
                // administrative `Dup`/`Sup` nodes a `DUP-LAM` threads through them
                // must be resolved so a substituted binder actually reaches the
                // field (e.g. `(\T -> type { Cons(T), .. }) X`).
                let t = self.resolve_type_fields(t).await;
                self.heap.finish_slot(slot, Term::Type(t))
            }
            other => self.heap.finish_slot(slot, other),
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
                    self.heap.alloc_type(TypeInfo::Sum { name, variants: nv })
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
                Dup(DupPtr<'h>),
                Sup(Addr),
                Type,
                Leave,
            }
            let k = match &*self.heap.view(&ptr) {
                Term::Dup { ptr: dp, .. } => K::Dup(*dp),
                Term::Sup { ptr: sp, .. } => K::Sup(sp.addr()),
                Term::Type(_) => K::Type,
                _ => K::Leave,
            };
            match k {
                K::Dup(dp) if self.dup_is_ready(&dp) => {
                    let r = self.sub_whnf_at(ptr).await;
                    // If it didn't reduce past a dup (genuinely stuck — e.g. a
                    // fired chain bottoming at an unfilled binder, or the budget
                    // was spent), leave it lazy instead of retrying forever.
                    if matches!(&*self.heap.view(&r), Term::Dup { .. }) {
                        r.into_addr()
                    } else {
                        self.resolve_lazy_field(r.into_addr()).await
                    }
                }
                K::Dup(_) => addr,
                K::Leave => addr,
                K::Sup(sup_addr) => {
                    let sp = unsafe { SupPtr::forge(sup_addr) };
                    let (la, ra) = self.heap.sup_addrs(&sp);
                    let nla = self.resolve_lazy_field(la).await;
                    let nra = self.resolve_lazy_field(ra).await;
                    self.heap
                        .set_sup_args(&sp, unsafe { TermPtr::forge(nla) }, unsafe {
                            TermPtr::forge(nra)
                        });
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
    fn dup_is_ready(&self, dp: &DupPtr<'h>) -> bool {
        // Follow a chain of dups iteratively (not recursively) so a deep chain
        // can't overflow the stack here.
        let mut value = self.heap.dup_peek(dp);
        loop {
            let Some(v) = value else { return true }; // fired dup
            match &*self.heap.view_at(v) {
                Term::App { .. }
                | Term::Bop { .. }
                | Term::Uop { .. }
                | Term::Use { .. }
                | Term::Mat { .. }
                | Term::Var { .. } => return false,
                Term::Dup { ptr, .. } => value = self.heap.dup_peek(ptr),
                _ => return true,
            }
        }
    }
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
