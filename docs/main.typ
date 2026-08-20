#import "style.typ": *

#set document(
  title: "The Atlas Interaction Calculus",
  author: "The Atlas Project",
  keywords: ("interaction calculus", "dependent types", "structural typing"),
)

#show: book

#align(center)[
  #v(1.2in)
  #text(size: 25pt, weight: "bold", fill: atlas-blue)[The Atlas Interaction Calculus]
  #v(10pt)
  #text(size: 14pt)[Reduction, dependency, and structural typing]
  #v(24pt)
  #text(size: 10pt, fill: luma(90))[Normative kernel specification · first edition]
  #v(1fr)
  #text(size: 9pt)[The Atlas Project]
]

#pagebreak()

#outline(title: [Contents], indent: auto)

#include "chapters/01-status.typ"
#include "chapters/02-syntax.typ"
#include "chapters/03-interactions.typ"
#include "chapters/04-reduction-modes.typ"
#include "chapters/05-dependent-types.typ"
#include "chapters/06-structural-types.typ"
#include "chapters/07-projections.typ"
#include "chapters/08-type-evaluation.typ"
#include "chapters/09-examples.typ"
#include "chapters/10-extensions.typ"

= References

#bibliography("references.bib", style: "ieee")

