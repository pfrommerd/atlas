#import "../style.typ": *

= Computational dependent types

Atlas uses a computational form of the Calculus of Constructions. Types are
terms, dependent functions are fundamental, and universe checking is
cumulative. General recursion and recursive type records are admitted, so the
system is intended for program checking rather than proof-theoretic
normalization.

== Judgments and sorts

The declarative judgment

#judgement([$ Gamma tack.r_rho t : A $])

states that $t$ has type $A$ while consuming the usage vector $rho$ from the
quantitative context $Gamma$. Usage vectors add with the partial grade addition
defined in Chapter 2. A second judgment $Gamma tack.r A " sort" s$ abbreviates
that $A$ inhabits sort $s$.

The sort axioms are

#judgement([$
  Gamma tack.r "Prop" : "Type"_0
  quad "and" quad
  Gamma tack.r "Type"_i : "Type"_(i+1).
$])

Universes are cumulative: a term in $"Type"_i$ may be checked in
$"Type"_j$ when $i <= j$. This cumulativity is a universe rule, not structural
record satisfaction.

== Dependent products

Let $"level"("Prop") = 0$ and $"level"("Type"_i)=i$. Product formation is
impredicative in $"Prop"$:

#rulebox([PI–PROP], [
  If $Gamma tack.r A : s$ and
  $Gamma,x:^q A tack.r B : "Prop"$, then
  $Gamma tack.r pi^q x:A dot B : "Prop"$.
])

#rulebox([PI–TYPE], [
  If $Gamma tack.r A : s$ and
  $Gamma,x:^q A tack.r B : "Type"_j$, then
  $
    Gamma tack.r pi^q x:A dot B
    : "Type"_("max"("level"(s),j)).
  $
])

#rulebox([LAM], [
  If $Gamma,x:^q A tack.r t:B$, then
  $
    Gamma tack.r lambda^q x:A dot t : pi^q x:A dot B.
  $
  A grade-zero body is checked for type correctness but the bound value cannot
  affect a grade-one result.
])

#rulebox([APP], [
  If $Gamma_1 tack.r f : pi^q x:A dot B$ and
  $Gamma_2 tack.r u:A_u$, application emits the explicit obligation
  $A_u subset.eq A$. On success,
  $
    Gamma_1 + q Gamma_2 tack.r f space u : B[x:=u].
  $
  At grade zero, the argument contributes no run-time usage. At grade one, the
  contexts must add affinely.
])

The symbol $subset.eq$ denotes structural satisfaction, defined in Chapter 6;
it is not definitional equality. Application is therefore one of the explicit
places where Atlas asks for structural compatibility.

== Conversion

Write $A equiv B$ for definitional equality. It is the symmetric, transitive,
congruence closure of:

- interaction-calculus beta reduction;
- transparent definition and fixed-point unfolding;
- computation of type records and their type-level operators; and
- equi-recursive unfolding of $mu X.A$ through its graph back-edge.

Function eta-conversion is excluded. Conversion is checked by reducing only as
far as a demanded comparison requires. It may diverge.

#rulebox([CONV], [
  If $Gamma tack.r t:A$, $A equiv B$, and $Gamma tack.r B:s$, then
  $Gamma tack.r t:B$.
])

Structural satisfaction is deliberately absent from CONV. It is introduced by
APP, projections, operator application, and other explicitly specified
capability boundaries.

== Implicit generalization

Type evaluation may create signature metavariables. At a lambda or type-record
boundary, every unsolved metavariable that is not fixed by the surrounding
context is generalized as an implicit grade-zero product. For example, the
closed identity term receives the scheme

#judgement([$
  pi^0 A:"Type"_i dot pi^1 x:A dot A.
$])

The implicit argument does not change run-time arity. If a would-be generalized
variable is required by run-time computation rather than only by a type or
signature, it cannot be grade zero and the result is _not proven_ until an
explicit run-time dependency is supplied.

== Propositions and erasure

Inhabitants of $"Prop"$ are checked statically and have usage zero at run time.
A proof variable may refine a type or establish another proposition, but it may
not be scrutinized to construct grade-one data in $"Type"_i$. This quantitative
restriction, rather than a global axiom of proof irrelevance, justifies proof
erasure.
