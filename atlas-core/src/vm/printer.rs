// ========================================================================
// Readback / printing
// ========================================================================

pub struct Printer<'a, 'h, X: Extensions = NoExtensions> {
    heap: &'a Heap<'h>,
    extensions: &'a X,
    /// Keyed by raw addresses (the affine `NodePtr`/`DupPtr` are not map keys).
    var_names: MemoMap<u64, String>,
    dup_names: MemoMap<u64, String>,
    name_counter: Cell<usize>,
}

pub struct PrettyNode<'a, 'h, X: Extensions = NoExtensions> {
    printer: &'a Printer<'a, 'h, X>,
    target: Term<'h>,
}

impl<X: Extensions> fmt::Display for PrettyNode<'_, '_, X> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printer.fmt(f, self.target)
    }
}

impl<'a, 'h> Printer<'a, 'h, NoExtensions> {
    pub fn new(heap: &'a Heap<'h>) -> Self {
        const NO_EXT: &NoExtensions = &NoExtensions;
        Printer::with_extensions(heap, NO_EXT)
    }
}

impl<'a, 'h, X: Extensions> Printer<'a, 'h, X> {
    pub fn with_extensions(heap: &'a Heap<'h>, extensions: &'a X) -> Self {
        Printer {
            heap,
            extensions,
            var_names: MemoMap::new(),
            dup_names: MemoMap::new(),
            name_counter: Cell::new(0),
        }
    }

    pub fn pretty<'s>(&'s self, target: Term<'h>) -> PrettyNode<'s, 'h, X> {
        PrettyNode {
            printer: self,
            target,
        }
    }

    fn fresh_name(&self) -> String {
        let n = self.name_counter.get();
        self.name_counter.set(n + 1);
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{}{}", letter, n / 26)
        }
    }

    fn var_name(&self, binder_addr: u64) -> &str {
        self.var_names
            .get_or_insert_with(binder_addr, || self.fresh_name())
    }

    fn dup_name(&self, dup_idx: u64) -> &str {
        self.dup_names
            .get_or_insert_with(dup_idx, || self.fresh_name())
    }

    pub fn fmt(&self, f: &mut fmt::Formatter<'_>, t: Term<'h>) -> fmt::Result {
        self.fmt_prec(f, t, true)
    }

    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, t: Term<'h>, tail: bool) -> fmt::Result {
        match t {
            Term::Lam(p) => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\{} -> ", self.var_name(p.addr()))?;
                let (_, body) = self.heap.pair(p);
                self.fmt_prec(f, body, true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::App(p) => {
                let (func, arg) = self.heap.pair(p);
                write!(f, "(")?;
                self.fmt_prec(f, func, false)?;
                write!(f, " ")?;
                self.fmt_prec(f, arg, false)?;
                write!(f, ")")
            }
            Term::Var(p) => match self.heap.node(&p) {
                Term::Sub(n) => self.fmt_prec(f, self.heap.view(n), tail),
                _ => write!(f, "{}", self.var_name(p.addr())),
            },
            Term::Dp0(q) => self.fmt_dup(f, q, "0", tail),
            Term::Dp1(q) => self.fmt_dup(f, q, "1", tail),
            Term::Sup(p) => {
                let lab = self.heap.label(self.heap.sup_label(p));
                let (a, b) = self.heap.sup_args(p);
                write!(f, "&{}{{", lab)?;
                self.fmt_prec(f, a, false)?;
                write!(f, ", ")?;
                self.fmt_prec(f, b, false)?;
                write!(f, "}}")
            }
            Term::Num(n) => write!(f, "{}", n),
            Term::Char(c) => write!(f, "{:?}", c),
            Term::Boxed(id) => match self.heap.value_get(id) {
                Value::Str(s) => write!(f, "{:?}", s),
                Value::Bytes(b) => {
                    write!(f, "0x")?;
                    for byte in b.iter() {
                        write!(f, "{:02x}", byte)?;
                    }
                    Ok(())
                }
                Value::Float(x) => write!(f, "{:?}", x),
            },
            Term::Pri(id) => match self.extensions.name(id) {
                Some(name) => write!(f, "%{}", name),
                None => write!(f, "%{}", id.get()),
            },
            Term::Ctr(base) => self.fmt_ctr(f, base),
            Term::Use(v) => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\_ -> ")?;
                self.fmt_prec(f, self.heap.node(&v), true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::Wld => write!(f, "_"),
            Term::Era => write!(f, "*"),
            Term::Bop(p) => {
                let op = match self.heap.node(&p.first()) {
                    Term::OpMeta(op) => op,
                    _ => unreachable!("a Bop's first cell is its operator meta-cell"),
                };
                let (l, r) = (self.heap.node(&p.second()), self.heap.node(&p.third()));
                write!(f, "(")?;
                self.fmt_prec(f, l, false)?;
                write!(f, " {} ", op.symbol())?;
                self.fmt_prec(f, r, false)?;
                write!(f, ")")
            }
            Term::Mat(_) => write!(f, "?{{...}}"),
            _ => write!(f, "<?>"),
        }
    }

    fn fmt_dup<const F: bool>(
        &self,
        f: &mut fmt::Formatter<'_>,
        dp: DupPtr<'h, F>,
        suffix: &str,
        tail: bool,
    ) -> fmt::Result {
        let slot = if F { DupSlot::Sub0 } else { DupSlot::Sub1 };
        match self.heap.dup_sub(dp, slot) {
            Term::Sub(n) => self.fmt_prec(f, self.heap.view(n), tail),
            _ => write!(f, "{}.{}", self.dup_name(dp.index()), suffix),
        }
    }

    fn fmt_ctr(&self, f: &mut fmt::Formatter<'_>, base: CtrPtr<'h>) -> fmt::Result {
        let (name, arity) = self.heap.ctr_head(base);
        let nm = self.heap.name(name);
        let arity = arity.get();
        if nm == "Nil" && arity == 0 {
            return write!(f, "[]");
        }
        if nm == "Con" && arity == 2 {
            write!(f, "[")?;
            let mut cell = base;
            let mut first = true;
            loop {
                if !first {
                    write!(f, ", ")?;
                }
                first = false;
                let head = self.heap.node(&self.heap.ctr_field(cell, 0));
                self.fmt_prec(f, head, false)?;
                let tail = self.heap.node(&self.heap.ctr_field(cell, 1));
                match tail {
                    Term::Ctr(b)
                        if {
                            let (n, a) = self.heap.ctr_head(b);
                            self.heap.name(n) == "Con" && a.get() == 2
                        } =>
                    {
                        cell = b;
                    }
                    Term::Ctr(b) if self.heap.name(self.heap.ctr_head(b).0) == "Nil" => {
                        return write!(f, "]");
                    }
                    _ => {
                        write!(f, ", ")?;
                        self.fmt_prec(f, tail, false)?;
                        return write!(f, "]");
                    }
                }
            }
        }
        if arity == 0 {
            write!(f, "#{}", nm)
        } else {
            write!(f, "#{}{{", nm)?;
            for i in 0..arity {
                if i > 0 {
                    write!(f, ", ")?;
                }
                let field = self.heap.node(&self.heap.ctr_field(base, i));
                self.fmt_prec(f, field, false)?;
            }
            write!(f, "}}")
        }
    }
}
