#import "../style.typ": *

= Structural type records

A structural type in record WHNF has exactly three components:

#judgement([$
  "Record"(L, P, O),
$])

where $L$ is layout, $P$ is a property row, and $O$ is an operator row. There is
no constructor component. Construction is an eliminator derived from a layout
or a practical extension represented through existing capabilities.

== Layouts and rows

Layouts are

#judgement([$
  L ::= star_L
    | "Atom"(a)
    | "Product"(R)
    | "Sum"(R).
$])

$star_L$ imposes no layout requirement. It is distinct from
$"Product"({})$, which describes a concrete product with zero stored fields.
This distinction lets a zero-sized requirement constrain capabilities without
claiming that satisfying values have zero-sized representations.

A row is a finite map followed by either a closed tail or a row variable:

#judgement([$
  R ::= {k_1 : A_1, ..., k_n : A_n | emptyset}
      | {k_1 : A_1, ..., k_n : A_n | r}.
$])

Row variables are ordinary inference variables with presence, absence, and
entry-type constraints. They permit principal open requirements such as “has at
least property $p$” without enumerating unrelated members.

== Properties and operators

A property entry is a pair $(S, i)$ of a signature $S$ and an
optional implementation $m$. An operator entry has the same shape, but its
signature is a dependent function describing the right operand and result.
Signatures may initially be metavariables; type evaluation infers them from
uses and implementations.

A readable stored field in a product generates a property with the field's
type and a layout accessor implementation. Therefore field and computed
property access use one namespace and one satisfaction rule. Physical layout
remains distinct from the public property row.

Requirement records omit implementations. Concrete records normally provide
them. Satisfaction compares signatures only; two values may satisfy the same
requirement with unrelated implementations.

== Structural satisfaction

Write $A subset.eq E$ when actual type $A$ satisfies expected type $E$. The
relation is reflexive and transitive, contains definitional equality, and has

#judgement([$
  "Never" subset.eq A subset.eq "Any".
$])

`Any` makes no guarantee. `Never` has no run-time inhabitants and represents an
unreachable result, not an error value.

#rulebox([RECORD], [
  $
    "Record"(L_A,P_A,O_A) subset.eq "Record"(L_E,P_E,O_E)
  $
  exactly when the three component judgments
  $L_A subset.eq_L L_E$, $P_A subset.eq_R P_E$, and
  $O_A subset.eq_R O_E$ hold. Implementations are not premises.
])

Layout satisfaction is polarity aware:

- Every layout satisfies $star_L$.
- Atomic layouts satisfy only the same atomic tag, modulo definitional equality.
- A product actual row must contain every expected field, and corresponding
  read-only field types satisfy covariantly. Extra actual fields are allowed.
- A sum actual row may contain only variants admitted by the expected row.
  Corresponding payload types satisfy covariantly. Thus the possible variants
  of a producer are a subset of those accepted by a consumer.

Property and operator rows use width satisfaction: every expected key must be
present in the actual row. Extra actual capabilities are allowed.

== Dependent capability variance

Dependent products compare contravariantly in their inputs and covariantly in
their outputs:

#rulebox([PI–SAT], [
  $
  (pi^q x:A dot B) subset.eq (pi^q x:E dot F)
  $
  when $E subset.eq A$ and, for every admissible $u$ satisfying $E$,
  $B[x:=u] subset.eq F[x:=u]$. Quantities must agree; changing a run-time
  dependency into an erased one, or conversely, is not structural subtyping.
])

Operators use PI–SAT on their full signatures. `Call` is a distinguished
operator name. A non-lambda value can appear in function position when its
operator row supplies a `Call` implementation whose signature satisfies the
required dependent product. Lambdas remain fundamentally typed by $Pi$; `Call`
is the structural bridge for other values.

== Recursive satisfaction

Recursive types are compared coinductively. A comparison carries a finite set
$H$ of active graph-node pairs. Comparing $(A,E)$ first regularizes both outer
nodes. If $(A,E)$ is already in $H$, the comparison succeeds by the recursive
hypothesis. Otherwise it adds the pair and emits only the child comparisons
required by the expected outer structure.

This rule accepts bisimilar recursive records without unbounded unfolding. A
type computation may still diverge before it exposes a record head; coinduction
does not turn such divergence into success.

== Meet, composition, and join

The meet $A and B$ is the greatest type, under $subset.eq$, satisfying both
requirements. The join $A or B$ is the least type guaranteed for either
alternative. They obey

#judgement([$
  A and "Any" = A, quad
  A or "Never" = A.
$])

For products and capability rows, meet unions keys and join retains common
keys. Repeated entries recursively meet or join their signatures. For sum
layouts, meet intersects possible variants and join unions them. Atomic layouts
with unequal tags join at an unconstrained layout and have no consistent meet.

The surface term $A+B$ computes a *consistent meet*. If repeated entries are
incompatible or the meet is uninhabited, composition produces a mismatch rather
than returning `Never`. Implementations are discarded from the composed
requirement. Branch joining, by contrast, is total through `Any` and `Never` and
never selects an implementation that is not guaranteed by every branch.

== Declarative eliminators

The structural operations enter the declarative typing relation only at
explicit boundaries.

#rulebox([PROPERTY–TYPE], [
  If $Gamma tack.r v:A$ and regularizing $A$ proves a property entry
  $p:(S,i)$, then $Gamma tack.r v dot p:S[v/"Self"]$. The implementation $i$
  is required for run-time dispatch but is not part of satisfaction.
])

#rulebox([OPERATOR–TYPE], [
  If $Gamma_1 tack.r v:A$, $A$ provides
  $o : pi^1 x:R dot F$, and $Gamma_2 tack.r u:A_u$ with
  $A_u subset.eq R$, then
  $
    Gamma_1 + Gamma_2 tack.r v space o space u : F[x:=u].
  $
])

#rulebox([CALL–TYPE], [
  If $v$ does not synthesize a foundational $Pi$ type but its actual structural
  type provides a `Call` entry with signature $pi^q x:R dot F$, application
  uses the same quantity and satisfaction premises as APP and returns
  $F[x:=u]$.
])

#rulebox([PROJECT–TYPE], [
  If $Gamma_1 tack.r t:A$, $Gamma_2 tack.r E:"Type"_i$, and
  $A subset.eq E$, then
  $
    Gamma_1 + Gamma_2 tack.r t :> E : A.
  $
  The result keeps the actual type; projection does not narrow it to $E$.
])

#rulebox([SUP–TYPE], [
  If $Gamma_1 tack.r a:A$ and $Gamma_2 tack.r b:B$, then
  $
    Gamma_1 + Gamma_2 tack.r &^ell {a,b} : A or B.
  $
  Both branches are owned and statically reachable. `Never` is neutral in the
  join.
])
