//! The [`Executor`]: interaction-calculus evaluation over a branded [`HeapScope`].
//!
//! v1 is a synchronous, single-task evaluator over the affine heap model. It
//! covers the affine core (APP-LAM / APP-USE / APP-ERA, binary ops, constructors
//! as data, and full normalization). The duplication / superposition / match
//! interactions and the parallel async driver are deferred to a later increment.

use crate::vm::heap::{Addr, BodyPtr, HeapScope, Spine, TermPtr};
use crate::vm::term::{BinaryOp, Term};
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
    AppLam, AppUse, AppEra,
    BopVal,
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

/// Drives reduction over a branded [`HeapScope`].
pub struct Executor<'e, 'h, P: ExecPolicy> {
    pub heap: &'e HeapScope<'h>,
    pub policy: P,
}

impl<'e, 'h, P: ExecPolicy> Executor<'e, 'h, P> {
    pub fn new(heap: &'e HeapScope<'h>, policy: P) -> Self {
        Executor { heap, policy }
    }

    /// Forge a fresh pointer to a node by address (used to descend into a child
    /// without consuming the parent's affine handle to it).
    fn at(&self, addr: Addr) -> TermPtr<'h> {
        unsafe { TermPtr::forge(addr) }
    }

    // ====================================================================
    // Erase: recursively reclaim a term and everything reachable from it.
    // ====================================================================

    pub fn erase(&self, term: Term<'h>) {
        match term {
            Term::App { func, arg }
            | Term::And { lhs: func, rhs: arg }
            | Term::Or { lhs: func, rhs: arg } => {
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
                self.erase(self.heap.pull(self.at(body.body_addr())));
            }
            Term::Use { body } => {
                self.erase(self.heap.pull(body));
            }
            Term::Ctr { arity, values, .. } => {
                let addrs = self.heap.free_pack(values);
                for a in addrs.iter().take(arity as usize) {
                    self.erase(self.heap.pull(self.at(*a)));
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

    // ====================================================================
    // WHNF
    // ====================================================================

    /// The boxed form of [`whnf_at`](Self::whnf_at), for use at recursive call
    /// sites (an `async fn` cannot directly recurse into itself).
    pub fn sub_whnf_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, TermPtr<'h>> {
        Box::pin(self.whnf_at(ptr))
    }

    /// Reduce the node at `ptr` to weak head normal form in place, returning a
    /// pointer to the result node (which may differ from `ptr` if the head
    /// interaction relocated it).
    pub async fn whnf_at(&self, ptr: TermPtr<'h>) -> TermPtr<'h> {
        let mut spine: Spine<'h> = Spine::new();
        let (mut slot, mut term) = self.heap.term(ptr);

        loop {
            if !self.policy.should_continue() {
                // Budget spent: write the head back and fold the spine.
                let mut cur = slot.finished(term);
                while let Some((cslot, cterm)) = spine.pop() {
                    let _ = cur;
                    cur = cslot.finished(cterm);
                }
                return cur;
            }

            // ---- reduction step ----
            match term {
                Term::App { func, arg } => {
                    let (fslot, fterm) = self.heap.term(self.at(func.addr()));
                    spine.push(slot, Term::App { func, arg });
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
                    if self.policy.should_continue() {
                        if let Some(t) = self.combine_bop(op, nl.addr(), nr.addr()) {
                            term = t; // reuse `slot`
                            continue;
                        }
                    }
                    // stuck (or budget): rebuild with reduced operands and unwind.
                    term = Term::Bop {
                        op,
                        lhs: nl,
                        rhs: nr,
                    };
                }
                other => term = other, // every other head is inert in v1.
            }

            // ---- unwind ----
            loop {
                match spine.pop() {
                    None => return slot.finished(term),
                    Some((cslot, cterm)) => match cterm {
                        Term::App { func, arg } => {
                            // Stuck head: persist it into the func node, rebuild the
                            // stuck application, and keep unwinding.
                            let _ = slot.finished(term);
                            slot = cslot;
                            term = Term::App { func, arg };
                        }
                        _ => unreachable!("non-spine continuation"),
                    },
                }
            }
        }
    }

    /// Combine a binary op whose operands are already in WHNF (read by address).
    /// On success the operand nodes are reclaimed; on `None` they are left intact
    /// for the caller to rebuild a stuck `Bop`.
    fn combine_bop(&self, op: BinaryOp, la: Addr, ra: Addr) -> Option<Term<'h>> {
        let l = self.heap.view(la);
        let r = self.heap.view(ra);
        if let (Term::U64(a), Term::U64(b)) = (l, r) {
            let _ = self.heap.pull(self.at(la));
            let _ = self.heap.pull(self.at(ra));
            self.policy.next_step(InteractionType::BopVal);
            return Some(match apply_op(op, a, b) {
                Some(v) => Term::U64(v),
                None => Term::Wld, // div/mod by zero erases to a wildcard
            });
        }
        None
    }

    // ====================================================================
    // Normalization
    // ====================================================================

    /// The boxed form of [`normalize_at`](Self::normalize_at), for recursive call
    /// sites.
    pub fn sub_normalize_at(&self, ptr: TermPtr<'h>) -> Reduce<'_, TermPtr<'h>> {
        Box::pin(self.normalize_at(ptr))
    }

    /// Reduce the node at `ptr` to full normal form in place, returning a pointer
    /// to the result node.
    pub async fn normalize_at(&self, ptr: TermPtr<'h>) -> TermPtr<'h> {
        let p = self.whnf_at(ptr).await;
        if !self.policy.should_continue() {
            return p;
        }
        let (slot, term) = self.heap.term(p);
        match term {
            Term::Lam { body } => {
                let nb = self.sub_normalize_at(self.at(body.body_addr())).await;
                let body = unsafe { BodyPtr::forge(body.binder_addr(), nb.into_addr()) };
                slot.finished(Term::Lam { body })
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
