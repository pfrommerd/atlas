# Atlas typing

Atlas has a structural type system implemented by the interaction calculus
itself. Types are ordinary expressions, and checking a type is a form of
reduction rather than a separate compiler pass over a different type IR.

The same lowered core expression can be used in two independent ways:

1. **Normal evaluation** evaluates program values. It does not require a
   preceding static check. Type projections encountered during evaluation are
   checked dynamically.
2. **Type-only reduction** runs a different set of interactions over the core
   expression. It regularizes types and proves projections without evaluating
   ordinary program values.

Atlas source always lowers to core without performing type checking. A caller
may evaluate that core immediately, run type-only reduction, or do both on
separate instantiations of the lowered expression. The Atlas CLI will eventually
offer type checking before evaluation as an opt-in mode.

## Design principles

### Types are values

A type expression is a regular expression that evaluates to a type record. For
example, `Int`, a user-defined product, and `Attr "name"` are all first-class
type values. Types may be passed to functions, returned from functions, and
computed lazily.

A value's type is described by a structural record with three parts:

- **Layout** describes the data stored by values of the type: positional or
  named fields, the types of those fields, and the possible variants of a sum.
- **Properties** map names to typed associated values. A property may be a
  constant or a function. Structural comparison checks its signature, not the
  identity of its implementation.
- **Operators** map an operation to a typed implementation. Operators include
  arithmetic and comparison operations as well as the special `Call`
  capability used when a value appears in function position.

Concrete data types and requirement types use this same representation. A type
record with no runtime layout is zero-sized and acts like a structural trait.
There is no separate nominal trait kind.

Type values themselves also have a type record. Operations such as composing
types therefore use the same dispatch mechanism as operations on other values.

### Compatibility is structural satisfaction

An ascription:

```atlas
value : Expected
```

asks whether the actual type of `value` satisfies `Expected`. Satisfaction is
not exact equality. Every requirement stated by `Expected` must be present and
compatible, but the actual value may provide additional fields, properties, or
operators.

Property compatibility requires:

- the expected property name to exist;
- its value type to structurally satisfy the expected signature; and
- no equality between the two property implementations.

Operators are checked by their full signatures. A binary operator is not merely
an `Add` marker placed on both operands. It is a capability such as:

```text
Add<Rhs> -> Output
```

on the dispatching type. Similarly, implicit callability is represented as:

```text
Call<Argument> -> Result
```

This replaces a general "convert to lambda" coercion. An explicit `Call`
capability has a finite, checkable signature and does not introduce recursive
coercion search.

Read-only product fields are width-compatible: a record with `foo` and `bar`
satisfies a requirement that mentions only `foo`.

Sum types require polarity-aware comparison. A value's possible runtime
variants must be a subset of the variants a consumer is prepared to handle.
Constructor availability is a capability in the other direction: a type may
expose constructors beyond those requested by a constructor requirement.
Keeping these two views separate prevents an open set of runtime variants from
being passed to an exhaustively matched closed sum.

### Requirement composition

Zero-sized requirements compose with `+`:

```atlas
foo (x: Attr "foo" + Attr "bar") = x.foo + x.bar
```

On type values, `+` forms a structural intersection of requirements:

- disjoint entries are combined;
- repeated compatible entries are merged recursively;
- repeated incompatible entries make the composition fail; and
- composition does not add runtime layout merely because it combines
  requirements.

For example, two requirements for `foo` can compose when one property
signature satisfies the other. Requirements for incompatible `foo` signatures
cannot.

## WHNF regularization

All type checking goes through weak-head-normal-form regularization. There must
not be a second eager Rust-level structural checker, an Atlas-AST constraint
solver, or a metadata shortcut with different semantics.

A type record is in **type WHNF** when its outer record form and the keys of the
relevant layout, property, and operator entries are known. Child field types,
property signatures, operator signatures, and implementations remain lazy.
They are regularized only when a check demands them.

Structural satisfaction is represented by core check/projection terms. A check
interaction:

1. requests type WHNF for the actual and expected operands;
2. compares the required outer entries;
3. emits child checks for the entries that must be compared; and
4. succeeds only after those child checks regularize successfully.

Consequently, checking `Attr "foo"` does not normalize unrelated fields or
property bodies. The type reducer performs the minimum work necessary to prove
the requested structure.

Recursive types are equi-recursive structural records. Their runtime
representation must support explicit graph back-edges rather than relying on
unbounded unfolding of `fix`. Satisfaction is coinductive: while regularizing a
recursive comparison, an already-active `(actual, expected)` pair is treated as
the recursive hypothesis. This both accepts structurally equivalent recursive
records and terminates comparisons that revisit the same pair.

Type computations can still diverge before exposing type WHNF. Type-only
reduction is therefore a semi-decision procedure. A reduction budget ending is
"not proven", not proof that the types mismatch.

## Runtime projections

A type hint lowers to a core type-projection term containing the value
expression and expected type expression:

```atlas
sum (x: Int) (y: Int) = x + y
```

Conceptually lowers its binders using projections equivalent to:

```text
\x y -> (x : Int) + (y : Int)
```

In normal evaluation, forcing a projection:

1. regularizes the value enough to expose its actual type;
2. regularizes the expected type to type WHNF;
3. runs the ordinary structural-satisfaction interactions; and
4. returns the original value unchanged on success, or an `Err` term on
   failure.

A projection is an assertion about a value, not a conversion. It must preserve
the value and its ownership when successful.

Projection must have explicit interactions with the rest of the calculus:

- Erasing a projection erases both owned operands that remain live.
- Duplicating it preserves affine ownership and distributes checks consistently
  with the duplicated value.
- A projection over a superposition checks each possible branch.
- A projection whose value or expected type is stuck remains stuck.
- Recursive child projections use the same coinductive regularization rules as
  every other check.

Runtime projection failure produces the existing first-class `Err` term. It
does not create a special "error type".

Normal evaluation is valid without a type-only pass. A bad annotation may
therefore fail only if execution reaches it, while an unused bad branch may
never fail at runtime.

## Type-only reduction

Type-only reduction operates on lowered core, not on Atlas syntax. It must work
equally for:

- a core expression written directly;
- a core expression produced by Atlas lowering; and
- a core expression constructed by an embedding application.

It uses the same heap, term representation, scheduling model, and parallel
interaction machinery as normal evaluation, but selects type interactions
instead of value interactions. It does not call normal evaluation internally
to discover a value's result.

Representative type interactions include:

- literals regularize to their builtin type records;
- a lambda regularizes to a `Call` capability;
- application checks the function's `Call<Argument> -> Result` capability;
- field access requires and returns the corresponding property or layout entry;
- an operator checks its typed operator capability and produces its declared
  output type;
- an explicit projection introduces a structural-satisfaction check; and
- pattern matching and control flow join the types guaranteed by every
  reachable value-dependent branch.

When a condition cannot be decided from type information, type-only reduction
does not evaluate it to choose a branch. It regularizes all possible branches
and keeps only their common guarantees. For example, if one branch exposes
`foo` and the other exposes only `bar`, the joined result exposes neither. A
later `.foo` is rejected by type-only reduction even when a particular runtime
execution would take the branch containing `foo`.

This conservatism is intentional. When requested, type-only reduction requires
proof. It reports one of the following distinct outcomes:

- **success**: every required projection and capability was proven;
- **mismatch**: regularization demonstrated incompatible structure;
- **not proven**: a check remained stuck or depended on unavailable value
  information;
- **budget exhausted/divergent**: type WHNF was not reached within the
  reduction policy; or
- **unsupported**: the expression requires a value-only or effectful operation
  for which no type interaction exists.

The type-only pass and normal evaluation mutate their heap graphs differently.
When a caller wants to check and then evaluate, it must instantiate the lowered
core IR separately for each reduction. It must not attempt to continue normal
evaluation from the type-regularized affine graph.

## Lowering and elaboration

Atlas lowering must remain possible without type checking. Typed syntax lowers
to explicit core projections and capability operations, but lowering does not
try to prove them.

The current Atlas `Type` AST accepts only identifiers. It will need to accept
regular type expressions so syntax such as the following can reach core:

```atlas
x: Attr "foo" + Attr "bar"
```

Types should share the ordinary expression grammar wherever possible rather
than grow a second language that cannot represent computed types.

The initial sketch described rewriting every operator use:

```text
\x y -> x + y
```

into something resembling:

```text
\x y -> (x : Add) + (y : Add)
```

The final design should not perform this textual rewrite. It loses the operator
signature and can accidentally add uses of affine variables. Instead, the core
operator term itself emits an `Add<Rhs> -> Output` requirement during type-only
regularization and performs capability dispatch during normal evaluation.
Requirements arising from multiple uses are combined at the relevant binder by
the normal interaction machinery.

## Roadmap

### Phase 0: specify observable semantics

Before changing the term representation, add executable examples for:

- structural width satisfaction;
- property-signature compatibility;
- operator and `Call` signatures;
- requirement composition and conflicts;
- polarity-aware sum compatibility;
- recursive satisfaction;
- runtime projection success and failure; and
- the difference between normal and type-only control-flow behavior.

These examples become conformance tests as each later phase is implemented.

### Phase 1: core WHNF type foundation (MVP)

The first implementation milestone is deliberately core-only.

- Generalize the existing lazy `TypeInfo` product/sum representation into the
  uniform structural record described above.
- Give builtin types stable structural descriptions instead of minting
  semantically opaque fresh descriptions for each `typeof`.
- Define type WHNF and implement the interactions that expose one record layer
  without normalizing child expressions.
- Represent recursive records with stable graph back-edges.
- Implement lazy, coinductive structural-satisfaction interactions.
- Keep existing construction and `typeof` behavior working through the new
  records.

The MVP is complete when core tests can construct, inspect, compose, and compare
lazy recursive type records entirely through WHNF interactions. It does not yet
require Atlas annotation support.

### Phase 2: projections and capability dispatch

- Add the projection/ascription term to core syntax, core IR, heap terms,
  printing, erasure, duplication, and normal reduction.
- Add typed properties, layout access, operator entries, and `Call`.
- Implement `+` composition for type records.
- Route field access, calls, and operators through capability semantics.
  Existing specialized primitive interactions may remain as fast paths when
  they are observationally equivalent.
- Ensure property and operator implementations remain lazy during signature
  checks.

This phase provides useful dynamic typing even when no type-only pass is run.

### Phase 3: type-only interaction mode

- Introduce an explicit reduction mode or interaction table selected when an
  executor is created.
- Implement type interactions for every existing core term, with a clear
  `unsupported` result for terms that cannot yet be typed.
- Implement conservative joins for matches, conditionals, superpositions, and
  other value-dependent alternatives.
- Push and annihilate projections through WHNF regularization.
- Reuse the existing parallel scheduler and reduction budget.
- Expose a core API that accepts any lowered core expression and returns the
  structured type-reduction outcome.

There is no Atlas dependency in this phase.

### Phase 4: Atlas surface integration

- Expand type annotations from identifier-only syntax to regular type
  expressions.
- Lower typed binders to core projections.
- Lower structs, enums, recursive types, associated properties, and operator
  implementations into uniform core type records.
- Implement Atlas field access and calls in terms of the corresponding core
  capability operations.
- Preserve unconditional Atlas-to-core lowering: no checker state or proof is
  needed to produce core.

### Phase 5: CLI and embedding workflows

- Add an opt-in `--typecheck` CLI option, disabled by default.
- With `--typecheck`, lower to reusable core IR, instantiate and run type-only
  reduction, and instantiate a fresh graph for normal evaluation only after a
  successful check.
- Add a check-only REPL command and a persistent REPL toggle usable in both
  Atlas and core language modes.
- Expose the same choice through the embedding API: evaluate only, check only,
  or check then evaluate.
- Present mismatches, incomplete proofs, exhausted budgets, unsupported type
  interactions, and runtime errors distinctly.

### Phase 6: hardening and optimization

- Add richer recursive-type syntax, type functions, generic capability
  signatures, aliases, and associated declarations.
- Define variance for higher-order `Call` and operator signatures.
- Cache repeated WHNF comparisons without changing affine ownership.
- Canonicalize structural records where useful, while preserving lazy
  expression identity and recursive back-edges.
- Test deterministic results under parallel type interactions.
- Add source-aware projection traces and diagnostics.
- Define serialization for cyclic type records.
- Allow proven projections to be removed as an optimization, with an option to
  retain runtime verification.

## Conformance scenarios

The completed system should satisfy at least these cases:

1. A record with `foo: Int` and `bar: String` satisfies `Attr "foo" Int`.
2. The same record does not satisfy `Attr "foo" String`.
3. Two types may use different implementations of `foo` and still satisfy the
   same typed property requirement.
4. `Attr "foo" Int + Attr "bar" String` composes successfully.
5. `Attr "foo" Int + Attr "foo" String` fails during type regularization.
6. A callable value satisfies `Call<Int> -> String` only when its argument and
   result signatures are structurally compatible.
7. A product with extra fields satisfies a narrower product requirement.
8. A sum value cannot satisfy a consumer that omits one of its possible runtime
   variants, even if the type exposes all constructors the consumer requests.
9. Two equivalent recursive list records compare successfully without infinite
   unfolding; a recursive list and recursive tree eventually expose a mismatch.
10. Runtime projection returns the projected value unchanged on success and
    produces `Err` on failure.
11. Normal evaluation can succeed without first running type-only reduction.
12. Type-only reduction can reject an operation available in only one unknown
    branch even when one concrete normal evaluation would succeed.
13. Type-only reduction can inspect the signature of a divergent function body
    without evaluating that body.
14. Check-then-evaluate uses two independently instantiated graphs and produces
    the same runtime result as evaluation without the preceding check.

