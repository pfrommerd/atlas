#import "../style.typ": *

= Kernel syntax and resources

Let $x, y, z$ range over variables, $ell, k$ over duplication labels, $a$ over
atomic layout tags, $p$ over property names, and $o$ over operator names. Terms
and types occupy one grammar:

#judgement([$
  t, A, B ::= x
    | lambda^q x : A dot t
    | t space u
$])
#judgement([$
  quad | delta^ell x_0, x_1 := t space "in" space u
    | &^ell {t, u}
    | "erase"(t)
$])
#judgement([$
  quad | pi^q x : A dot B
    | "Prop"
    | "Type"_i
    | mu X dot A
$])
#judgement([$
  quad | "Record"(L, P, O)
    | "Any"
    | "Never"
    | t dot p
    | t space o space u
    | t :> A
    | A + B
    | "error".
$])

The superscript $q$ is a quantity in $Q = {0, 1}$. A grade-one binder is a
run-time affine dependency. A grade-zero binder is erased: it may occur in
types and proofs but cannot influence a run-time-relevant result. The
application notation is left associative and dependent products extend as far
right as possible.

The term $delta^ell x_0, x_1 := t " in " u$ is explicit contraction. It owns
one occurrence of $t$ and binds two affine projections in $u$. The term
$&^ell{t,u}$ is a superposition: two alternatives occupying one position. A
label determines whether a duplication and superposition annihilate or commute.

The term $t :> A$ is a projection (also called an ascription). It asserts that
the actual type of $t$ structurally satisfies the expected type expression
$A$. It never converts $t$. The surface operation $A+B$ constructs the
consistent intersection of two requirements.

== Affine well-formedness

A quantitative context has entries $x :^q A$. Context addition is partial:

#judgement([$
  0 + q = q, quad q + 0 = q, quad 1 + 1 = "undefined".
$])

An ordinary rule may combine premises only when their run-time contexts add.
Thus an affine variable cannot occur in two premises. Duplication is the sole
rule that turns one run-time resource into two named resources. Erasure is the
sole operation that consumes an unused run-time resource.

#rulebox([DUP formation], [
  If $Gamma_1 tack.r t : A$ and
  $Gamma_2, x_0 :^1 A, x_1 :^1 A tack.r u : B$, then
  $Gamma_1 + Gamma_2 tack.r delta^ell x_0,x_1 := t " in " u : B$.
])

#rulebox([Erased dependency], [
  A variable bound at grade zero may occur while forming a type or proposition,
  but no grade-one result may inspect it. In particular, a proof cannot be
  eliminated into run-time data in $"Type"_i$.
])

Types written in judgments are meta-level annotations and do not themselves
consume a run-time occurrence. A first-class term whose value is a type is an
ordinary object-language term and remains affine unless it is explicitly
duplicated. This distinction permits erased dependent indices without making
all first-class type values implicitly shareable.

== Alpha equivalence and substitution

Terms are identified up to capture-avoiding renaming of binders. The notation
$t[x := u]$ denotes capture-avoiding substitution. Operational rules use this
notation to specify observable term rewriting; an implementation may realize it
with linked variables and destructive graph updates.

Recursive types use explicit graph identity. The binder in $mu X.A$ denotes a
back-edge to the enclosing type node, not a request to copy and unfold $A$
without bound.
