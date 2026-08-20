#import "../style.typ": *

= WHNF and normalization

The local relation $arrow.r_v$ does not by itself say which redex is demanded.
Atlas value evaluation is weak-head and spine directed.

== Runtime weak-head reduction

A runtime head is one of the following:

- a lambda or erased lambda;
- a superposition;
- a rigid value, including a sort, dependent product, or type record;
- a variable or duplication projection whose binder has not been substituted;
- a projection whose value or expected type cannot yet expose a descriptor.

The principal demand contexts are

#judgement([$
  E ::= square | E space t | E dot p | E space o space t | E :> A.
$])

The hole $square$ is the next demanded head. Application demands only its
function until APP–LAM or APP–SUP exposes further work. A property or operator
elimination demands its receiver. A projection demands its value only when the
projection itself reaches a demanded position.

#definition([Runtime WHNF], [
  A term is in runtime WHNF when no $arrow.r_v$ interaction is available in its
  current demand context. Redexes inside a lambda body, rigid child, untaken
  alternative, or undemanded projection do not prevent WHNF.
])

Write $t arrow.r_w u$ for the compatible closure of $arrow.r_v$ under these
demand contexts, and $t arrow.r_w^* v$ for its reflexive transitive closure to
WHNF. A stuck variable, an unresolved duplicate, or a type computation that has
not exposed its outer form is a WHNF head, not a mismatch.

== Full normalization

Full normalization first computes runtime WHNF and then recursively normalizes
every owned child of the resulting head. For a lambda it normalizes the body;
for a superposition it normalizes both alternatives; for a rigid node it
normalizes every child. The process repeats whenever normalizing a child makes
a new head interaction available.

Full normalization preserves DUP and SUP nodes that cannot interact. Collapsing
or enumerating superpositions is a separate consumer operation and is not part
of either WHNF or normalization.

== Budgets and divergence

A finite reduction policy may stop either relation before a normal form is
reached. The residual term is a valid partially reduced graph. Exhausting a
budget never proves that a program diverges, and exhausting a type-computation
budget never proves a type mismatch.

== Static type evaluation is not normalization

Static type evaluation, written $arrow.r_T$, is introduced in Chapter 8. It is
not the full compatible closure of $arrow.r_w$. In particular, it:

- visits all statically reachable alternatives rather than selecting a value;
- processes erased arguments and function bodies to validate their types;
- replaces ordinary values with type descriptions and constraints; and
- may stop with _not proven_ even when one concrete value execution succeeds.

Conversely, runtime normalization need not inspect a projection in an erased
subterm. The two modes therefore differ both in their local rules and in their
notion of reachability.
