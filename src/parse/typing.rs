enum Kind {
    Star,
    Arrow(Box<Kind>, Box<Kind>)
}

enum TypeDesc<'src> {
    Star,
    Arrow(Box<TypeDesc>, Box<TypeDesc>)
    Variant()
}

enum KindDesc {
}