#import "../style.typ": *

= Practical extensions and implementation correspondence

This chapter is informative. It explains how a practical Atlas evaluator can
extend the kernel without changing the preceding semantics.

== Rigid values and eliminators

Integers, floats, characters, booleans, byte strings, and host values are rigid
heads. Each has a stable structural descriptor. A primitive binary or unary
operation is an eliminator: runtime evaluation demands its operands according
to the operation, while type evaluation looks up and checks the corresponding
operator signature. Specialized arithmetic interactions are valid fast paths
when they are observationally equivalent to capability dispatch.

Products and sums may be represented by constructor selections and saturated
constructions. A construction carries its declared type descriptor and lazy
field values. Pattern matching demands the scrutinee at run time. In type mode,
it excludes variants made impossible by a known closed sum and joins every
remaining branch. Constructor availability is derived from layout or exposed
through existing operator/property mechanisms; it never adds a fourth type
record component.

`typeof` is a value-level observation of a demanded runtime head. It is not the
static type evaluator. Errors are first-class erasers operationally and receive
`Never` during type evaluation. Host primitives must publish enough signature
information for static evaluation or produce `unsupported`.

== Relation to the Rust evaluator

The current Rust core represents applications, lambdas, erased lambdas,
duplication projections, superpositions, constructions, matches, operations,
types, and primitives as heap nodes. Linked substitution and affine pointer
ownership implement the abstract substitution and erasure rules. The evaluator
uses a spine to locate the next WHNF interaction and recursively normalizes
children for full normalization.

Its present `TypeInfo` representation contains lazy product or sum child nodes.
That representation is an implementation precursor, not the normative
three-component record defined here. In particular, the complete property and
operator rows, dependent universes, projections, constraint graph, and static
type-evaluation mode remain implementation work.

The machine may specialize DUP–RIGID into separate rules for lambdas,
applications, operations, types, constructors, and atomic values. It may also
coordinate concurrent forcing of two duplication projections. These choices
must preserve the abstract ownership and read-back behavior.

== Conformance boundary

A conforming extension must define:

- its runtime head and demand behavior;
- how duplication and erasure distribute through its owned children;
- the descriptor exposed to a demanded projection;
- its static type interaction or an explicit `unsupported` outcome; and
- how alternatives join when the extension depends on unavailable run-time
  information.

An implementation must instantiate lowered core separately for static checking
and normal execution. A type-evaluated affine graph cannot be reused as the
starting graph for value reduction.
