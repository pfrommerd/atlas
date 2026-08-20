#import "../style.typ": *

= Worked examples

These examples use compact names but follow the formal rules exactly.

== Sharing and superposition

Equal labels extract corresponding alternatives:

#judgement([$
  delta^A x_0,x_1 := &^A {1,2} space "in" space "add"(x_0,x_1)
  arrow.r_v "add"(1,2).
$])

With distinct labels, the interaction commutes and preserves all combinations:

#judgement([$
  delta^A x_0,x_1 := &^B {1,2} space "in" space "pair"(x_0,x_1)
  arrow.r_v^* &^B {"pair"(1,1), "pair"(2,2)}.
$])

The displayed final form suppresses the administrative equal-label
duplications created by the commuting rule.

Duplicating a lambda does not copy its body. DUP–LAM replaces the bound variable
with a superposition and places the body behind one duplication. Applying the
two resulting lambdas then substitutes two arguments into the two sides of that
same shared graph.

== WHNF is demand driven

The term

#judgement([$
  lambda^1 x:A dot ((lambda^1 y:B dot y) space z)
$])

is already in runtime WHNF: the beta redex is beneath a lambda. Full
normalization enters the body and reduces it to $lambda^1 x:A.z$. Static type
evaluation also visits the body, but it computes its type instead of performing
that ordinary value execution.

== Dynamic and static projection

Consider an erased function whose argument contains a failing projection:

#judgement([$
  (lambda^0 x:A dot 0) space (v :> E).
$])

Runtime APP–ERASE erases the argument without forcing its projection, so the
result is $0$. Static type evaluation visits the erased argument, infers the
actual type of $v$, and reports a mismatch if it does not satisfy $E$.

For a demanded successful projection,

#judgement([$
  "descriptor"(v) => A,
  quad A subset.eq E
  quad ==> quad
  v :> E arrow.r_v v.
$])

The result is $v$, not a value rebuilt with layout $E$.

== Implicit dependent inference

For $"id" = lambda^1 x dot x$, T–LAM introduces $alpha : "Type"_i$. T–VAR
returns $alpha$ and imposes no capability requirement. At the lambda boundary,
$alpha$ is generalized:

#judgement([$
  "id" : pi^0 A:"Type"_i dot pi^1 x:A dot A.
$])

Applying $"id"$ to an argument of actual type $"Int"$ satisfies the dependent input
and specializes the result to $"Int"$.

For $lambda^1 x dot x dot "foo"$, T–PROPERTY instead constrains $alpha$ to an
open property row containing $"foo":S$. Its principal input requirement says
“has at least readable property $"foo"$,” while the result remains the inferred
signature variable $S$.

== Width and variance

Let $A$ expose properties $"foo":"Int"$ and $"bar":"String"$, and let $E$ expose only
$"foo":"Int"$. RECORD and row width give $A subset.eq E$. If $E$ instead requires
$"foo":"String"$, the rigid atomic signatures demonstrate a mismatch. The
implementations of $"foo"$ are never compared.

Suppose an actual callable accepts `Any` and returns `Int`, while an expected
callable accepts `Number` and returns `Any`. Since
$"Number" subset.eq "Any"$ and $"Int" subset.eq "Any"$, PI–SAT accepts the actual
callable: its input is broader and its output narrower.

== Products, sums, meet, and join

A product with fields $"foo":"Int", "bar":"String"$ satisfies a product requirement
mentioning only $"foo":"Int"$. An unconstrained-layout property requirement is also
satisfied, while the concrete empty product remains a distinct layout.

For sums, a producer of $"Some"("Int")$ satisfies a consumer accepting
$"None" | "Some"("Int")$. The reverse does not hold, because the producer might emit
$"None"$ where the consumer is unprepared to handle it.

The composition of requirements $"Attr"("foo","Int")+"Attr"("bar","String")$ unions their
property keys. Composing $"Attr"("foo","Int")+"Attr"("foo","String")$ is inconsistent and
mismatches. Joining a branch with only $"foo"$ and a branch with only $"bar"$
retains neither property; joining sum branches unions their possible variants.

== Recursive satisfaction

Let

#judgement([$
  "List"(A) = mu X dot "Sum"({"Nil":"Unit", "Cons":"Product"(A,X)}).
$])

Comparing two independently allocated instances first compares their sum rows,
then their payloads. When the tail comparison revisits the original pair, the
active-pair hypothesis succeeds. Replacing the recursive tail by a structurally
different tree eventually exposes incompatible variant or product rows and
therefore mismatches.

== Proof erasure

A term may abstract over $h:^0 P$ and use $h$ while forming another proposition
or a dependent result type. It may not branch on $h$ to select grade-one data.
Consequently, erasing $h$ cannot alter normal program output.
