#let atlas-blue = rgb("#284b63")
#let atlas-pale = rgb("#f4f7f9")
#let atlas-rule = rgb("#9aabb7")

#let book(body) = {
  set page(
    paper: "us-letter",
    margin: (inside: 1.05in, outside: 0.85in, top: 0.8in, bottom: 0.9in),
    numbering: "1",
    number-align: center,
  )
  set text(font: "Libertinus Serif", size: 10.5pt)
  set par(justify: true, leading: 0.62em)
  set heading(numbering: "1.1")
  show heading.where(level: 1): it => {
    pagebreak(weak: true)
    set text(fill: atlas-blue)
    block(above: 1.2em, below: 0.8em)[#it]
  }
  show heading.where(level: 2): it => {
    set text(fill: atlas-blue)
    block(above: 1.0em, below: 0.5em)[#it]
  }
  show raw: set text(font: "DejaVu Sans Mono", size: 9pt)
  body
}

#let term(body) = text(font: "DejaVu Sans Mono", body)

#let rulebox(name, body) = block(
  width: 100%,
  fill: atlas-pale,
  stroke: (left: 2pt + atlas-blue, rest: 0.5pt + atlas-rule),
  inset: (x: 9pt, y: 7pt),
  radius: 2pt,
  breakable: false,
)[
  #text(fill: atlas-blue, weight: "bold")[#name]
  #v(3pt)
  #body
]

#let definition(name, body) = block(
  width: 100%,
  inset: (left: 8pt, right: 6pt, top: 4pt, bottom: 5pt),
  stroke: (left: 1.5pt + atlas-rule),
)[
  *Definition (#name).* #body
]

#let outcome(name, body) = [*#name:* #body]

#let judgement(body) = block(width: 100%, inset: 4pt)[#align(center)[#body]]
