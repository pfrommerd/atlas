#import "../style.typ": *

= Lazy runtime projections

A projection $t :> E$ contains a value expression and an expected type
expression. It is an assertion, not a cast. Value reduction does not inspect it
until the surrounding demand context requires its head.

== Runtime descriptors

Every run-time value head either carries or intrinsically determines a lazy
descriptor:

- a rigid data value carries the type record used to construct it;
- an atomic value determines a stable builtin record;
- a type value at universe level $i$ has type $"Type"_i$;
- a lambda exposes a lazy dependent-product descriptor synthesized from its
  binder and body when demanded; and
- a structural callable value exposes its `Call` operator entry.

Write $"descriptor"(v) => A$ when the descriptor of value head $v$
regularizes to actual type $A$. Descriptor synthesis may perform type
interactions local to the head, such as inferring a lambda signature, but it
does not normally evaluate the value's ordinary body or select run-time
branches.

Expected expression $E$ is evaluated as a first-class type computation under
the current value demand. It need only expose the part of type WHNF requested
by satisfaction. If it depends on a stuck value or diverges before exposing
that structure, the projection remains stuck or exhausts its budget.

== Projection interaction

#rulebox([PROJECT–OK], [
  If $t arrow.r_w^* v$, $"descriptor"(v) => A$,
  $E => E'$, and $A subset.eq E'$, then
  $
    v :> E arrow.r_v v.
  $
  The result is the same value, with the same observable sharing and ownership.
])

#rulebox([PROJECT–FAIL], [
  If both descriptors reach WHNF and exhibit an incompatible required entry,
  then
  $
    v :> E arrow.r_v "error".
  $
  Failure requires a demonstrated incompatibility. A stuck comparison is not a
  failure.
])

Implementations may preserve the value by borrowing immutable descriptor
metadata or by an interaction-calculus duplication whose checking projection
is consumed. In either case, successful projection is observationally
transparent; it may not rebuild, coerce, or consume the returned value.

== Superposition, duplication, and erasure

#rulebox([PROJECT–SUP], [
  A projection checks every possible run-time alternative. The expected type is
  duplicated with the superposition's label:
  $
    (&^ell {a,b}) :> E
    arrow.r_v
    delta^ell E_0,E_1 := E space "in" space
      &^ell {a :> E_0, b :> E_1}.
  $
])

Duplicating a projection follows DUP–RIGID: it duplicates the projected value
and expected expression consistently and produces two projections. Erasing an
unforced projection erases both operands still owned by it. Therefore an
annotation in an erased argument or untaken branch performs no run-time check.

Recursive child checks use the same active-pair hypothesis as all structural
satisfaction. Property and operator implementations are not forced when only
their signatures are being compared.

== Dynamic checking without a static pass

Normal execution never assumes that static type evaluation has run. A program
may execute successfully despite an invalid projection in unreachable code. A
bad projection fails precisely when demand reaches it. Conversely, a caller
may instantiate the same lowered expression separately and run static type
evaluation before deciding whether to execute it.
