#import "../style.typ": *

= Status and scope

This book defines the normative kernel of the Atlas interaction calculus. It
specifies the intended language even where the current evaluator has not yet
implemented a rule. An implementation is conforming when its observable
results agree with the relations in this book; its heap layout, scheduler, and
choice of redex are not themselves normative.

Atlas combines three ideas. The first is an affine lambda calculus with
explicit duplication and superposition, descended from interaction nets and
interaction combinators @lafont1997. The second is a computational,
dependently typed core based on the Calculus of Constructions @coquand1988. The
third is a structural capability system whose checks are themselves graph
computations. Quantitative typing records which dependencies survive at run
time, following the general approach of quantitative type theory @atkey2018.

== Two meanings of evaluation

The same lowered term admits two independent interpretations.

- *Value reduction* is lazy program execution. It exposes weak-head normal
  form (WHNF) and checks a projection only if execution demands that projection.
- *Type evaluation* traverses the statically reachable term using a distinct
  family of interactions. It infers signatures, accumulates constraints, and
  proves every required projection without selecting a branch from unavailable
  run-time information.

These modes share syntax, type records, and the structural satisfaction
relation. They do not share a reduction strategy. Type evaluation never calls
value reduction to discover the result of an ordinary computation, and a graph
mutated by one mode is not subsequently continued in the other mode.

#definition([Type regularization], [
  Type regularization is the act of exposing the outer record, sort, dependent
  product, or recursive back-edge of a type expression. It is an observation
  boundary used by structural comparison; it is not another name for static
  type evaluation.
])

== Normative and informative material

Rules, grammars, judgments, and explicitly labelled definitions are normative.
Worked examples are normative tests of those rules. The final chapter's mapping
to the Rust evaluator and its discussion of literals, constructors, primitives,
and packed nodes are informative. Atlas source syntax and source-to-core
lowering are outside the scope of this edition.

No claim of logical consistency or strong normalization is made. Atlas admits
general computation and equi-recursive types at the type level. Consequently,
both conversion and type evaluation are semi-decision procedures.
