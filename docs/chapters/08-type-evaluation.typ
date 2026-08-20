#import "../style.typ": *

= Static type evaluation

Static type evaluation is an operational interpretation of a core term. It
computes types and constraints rather than ordinary values. Write

#judgement([$
  (Gamma; t; C) arrow.r_T
  (Gamma'; A; C')
$])

for one type interaction, where $C$ is a graph of conversion, universe,
quantity, row, and satisfaction constraints. A successful normal result is a
type expression $A$ together with a solved or generalized constraint graph.

Unlike $arrow.r_w$, the compatible closure of $arrow.r_T$ descends into every
statically reachable child. It checks lambda bodies and erased arguments. It
regularizes both alternatives of a superposition. Practical control-flow
extensions similarly visit every branch not excluded by the scrutinee's
already-known type.

== Core type interactions

#rulebox([T–VAR], [
  If $x:^q A$ is in $Gamma$, then $x$ type-evaluates to $A$ and records one use
  at quantity $q$. Multiple run-time uses cannot be merged; they require a DUP
  node in the input term.
])

#rulebox([T–LAM], [
  Introduce a fresh type metavariable $alpha : "Type"_i$ for an unannotated
  binder, or regularize its written annotation. Type-evaluate the entire body
  under $x:^q alpha$, accumulating every requirement placed on $alpha$. If the
  body yields $B$, the lambda yields
  $
    pi^q x:alpha dot B.
  $
  At the lambda boundary, unconstrained external metavariables are generalized
  as implicit grade-zero products.
])

#rulebox([T–APP], [
  Type-evaluate the function and argument independently. If the function type
  exposes $pi^q x:A dot B$, emit $A_u subset.eq A$ for the argument type $A_u$
  and return $B[x:=u]$, with occurrences of $u$ represented by its inferred
  type information. If no $Pi$ head is exposed, require a `Call` operator with
  an admissible dependent signature and use its result. A demonstrated absence
  or incompatible input is a mismatch; an unresolved row is not proven.
])

#rulebox([T–DUP], [
  Type-evaluate the duplicand to $A$, then type-evaluate the body with
  $x_0:^1 A$ and $x_1:^1 A$. Requirements inferred independently from the two
  projections are combined as a consistent meet on $A$. The result type is the
  body type; DUP does not create a union or branch.
])

#rulebox([T–SUP], [
  Type-evaluate both alternatives. If they produce $A$ and $B$, the
  superposition produces $A or B$. No run-time alternative is selected.
])

#rulebox([T–ERASE], [
  Type-evaluate the erased operand to validate all of its statically reachable
  syntax, then assign the erasure process `Never`. In APP–ERASE the enclosing
  application receives the erased lambda body's type, exactly as in the
  declarative rule.
])

#rulebox([T–PROJECT], [
  Type-evaluate $t$ to actual type $A$. Evaluate $E$ as a type-level expression
  under $arrow.r_T$ until it exposes the demanded type structure, emit
  $A subset.eq E$, and return $A$. The mode does not evaluate $t$ normally and
  does not return the expected type.
])

#rulebox([T–PROPERTY], [
  Type-evaluate the receiver to $A$. Constrain its property row to contain
  $p:(S,i)$ and return $S$, substituting the receiver type into any
  dependent `Self` occurrences. A fresh row tail preserves unrelated
  capabilities.
])

#rulebox([T–OPERATOR], [
  Type-evaluate both operands. Constrain the left type to contain operator $o$
  with dependent signature $pi^1 x:R dot F$. Check the right type against $R$
  and return $F$ specialized to that operand type. Implementations are checked
  against inferred signatures when present but are not evaluated to discover
  the signature.
])

#rulebox([T–TYPE], [
  Formation of $"Prop"$, $"Type"_i$, $Pi$, recursive types, and structural records
  follows the declarative universe rules. A record's universe is the least
  universe containing its layout entry types and property/operator signatures.
  Optional implementations must inhabit their inferred signatures.
])

The distinguished term `error` has type `Never`. It does not create an error
type and contributes no requirements when joined with a successful branch.

== Constraint interactions

Constraint solving is itself lazy graph reduction:

- Conversion constraints regularize only enough type structure to decide the
  next comparison.
- Satisfaction constraints emit row-presence and child-signature constraints.
- A row metavariable is refined monotonically with required keys, forbidden
  keys, and a tail relation.
- Repeated requirements meet. A provably inconsistent meet is a mismatch.
- Recursive comparisons carry active node pairs and close coinductively.
- Universe constraints choose the least levels satisfying cumulativity.

An inference metavariable is not assumed to be `Any`. It remains a variable
until constraints solve it or a permitted boundary generalizes it. A variable
that affects run-time computation cannot be generalized at grade zero.

== Outcomes

Type evaluation returns one of five disjoint outcomes:

#outcome([Success], [Every required conversion, satisfaction check, capability,
and universe constraint was solved or validly generalized.])

#outcome([Mismatch], [Regularization exposed incompatible rigid structure, a
missing closed-row entry, an invalid quantity, or an inconsistent meet.])

#outcome([Not proven], [A required comparison remained stuck on an unresolved
value, open constraint, or non-generalizable metavariable.])

#outcome([Budget exhausted/divergent], [The configured policy ended before a
required type WHNF or fixed point was reached.])

#outcome([Unsupported], [The term invokes a value-only or effectful extension
for which no type interaction is defined.])

Mismatch is the only negative proof. The remaining incomplete outcomes do not
imply that a projection would fail in a particular run-time execution.

== Declarative correspondence

The declarative judgment specifies which results are valid; $arrow.r_T$
specifies how Atlas searches for them. The intended correspondence is:

- if type evaluation succeeds with $A$, the generalized constraint solution
  yields a declarative derivation $Gamma tack.r t:A$; and
- if such a derivation exists within the supported fragment and all demanded
  computations terminate, type evaluation succeeds with a type satisfying its
  declarative result.

This edition defines the correspondence but does not provide a metatheoretic
proof.
