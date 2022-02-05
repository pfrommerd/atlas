pub fn symbol_priority(sym: &str) -> u8 {
    match sym {
        "-" => 0,
        "+" => 0,
        "*" => 1,
        "/" => 1,
        _ => 2,
    }
}

pub fn transpile_infix(
    args: &Vec<Expr<'_>>,
    ops: &Vec<&str>,
    env: &SymbolMap,
    _span: Option<Span>,
    builder: ExprBuilder<'_>
) {
    if args.len() == 1 && ops.len() == 0 {
        // just transpile as per normal
        args[0].transpile(env, builder);
        return;
    }
    if args.len() < 2 {
        let mut eb = builder.init_error();
        eb.set_summary("Must provide at least two arguments to infix expression");
        return;
    }
    // First we find the rightmost, lowest-priority operation
    // to split on
    let mut lowest_priority: u8 = 255;
    let mut split_idx = 0;
    for (idx, op) in ops.iter().enumerate() {
        let p = symbol_priority(op);
        if p <= lowest_priority {
            lowest_priority = p;
            split_idx = idx;
        }
    }

    // Get the left and right arguments
    // TODO: Make more efficient by using immutable slices rather than
    // vectors
    let mut largs = args.clone();
    let rargs = largs.split_off(split_idx + 1);

    let mut lops = ops.clone();
    let mut rops = lops.split_off(split_idx);
    let op= rops.pop().unwrap();

    if let Some(sym) = env.lookup(op) {
        // Return a call expression
        let ib = builder.init_invoke();
        let mut cb = ib.init_app();
        let lx = cb.reborrow().init_lam();
        let mut lx = lx.init_id();
        lx.set_name(op);
        lx.set_disam(sym);

        let mut args = cb.reborrow().init_args(2);
        // get the builder for the args
        // and transpile left and right arguments
        let mut lb = args.reborrow().get(0);
        lb.set_pos(());
        transpile_infix(&largs, &lops, env, None,  lb.init_value());
        let mut rb = args.reborrow().get(1);
        rb.set_pos(());
        transpile_infix(&rargs, &rops, env, None,  rb.init_value());
    } else {
        let mut eb = builder.init_error();
        eb.set_summary("Symbol not found");
    }
}

impl<'src> Expr<'src> {
    pub fn transpile(&self, env: &SymbolMap, builder: ExprBuilder<'_>) {        
        match self {
            Expr::Identifier(_, ident) => {
                match env.lookup(ident) {
                    None => {
                        let mut eb = builder.init_error();
                        eb.set_summary("Unrecognized symbol");
                    },
                    Some(disam) => {
                        let mut sb = builder.init_id();
                        sb.set_name(ident);
                        sb.set_disam(disam);
                    }
                }
            },
            Expr::Infix(s, args, ops) => transpile_infix(args, ops, env, Some(*s), builder),
            Expr::Literal(_, lit) => lit.transpile(builder.init_literal()),
            Expr::IfElse(_, scrutinized, if_branch, else_branch) => 
                {
                    // yo dawg i heard you like reborrows so i reborrowed your reborrow
                    // so when you reborrow its already borrowed
                    let mut mb = builder.init_match();
                    scrutinized.transpile(env, mb.reborrow().init_expr());
                    let mut cb = mb.reborrow().init_cases(2);
                    let mut true_case = cb.reborrow().get(0);
                    true_case.reborrow().init_eq().set_bool(true);
                    if_branch.transpile(env, true_case.init_expr());
                    let mut false_case = cb.reborrow().get(1);
                    false_case.reborrow().init_eq().set_bool(false);
                    
                    if let Some(else_expr) = else_branch {
                        else_expr.transpile(env, false_case.init_expr())
                    } else {
                        // if else is omitted then default eval to unit makes sense?
                        false_case.init_expr().init_literal().set_unit(());
                    }
                    
                    let mut bb = mb.reborrow().init_binding();
                    bb.set_omitted(());
                },
            Expr::Tuple(_, _items) => todo!(),
            Expr::Builtin(_, name, args) => {
                let mut bb = builder.init_inline_builtin();
                bb.set_op(name);
                let mut ba = bb.reborrow().init_args(args.len() as u32);
                for (i,arg) in args.iter().enumerate() {
                    arg.transpile(env, ba.reborrow().get(i as u32));
                };
            },
            Expr::Record(s, fields) => {
                if let Some((hd, tl)) = fields.split_first() {
                    match hd {
                        Field::Simple(_, field_name, val) => {
                            
                            let ib =  builder.init_invoke();
                            let mut app_builder = ib.init_app();

                            let lam_b = app_builder.reborrow().init_lam();
                            let mut id_b = lam_b.init_id();
                            id_b.set_disam(env.lookup("__insert").unwrap());
                            id_b.set_name("__insert");
                            
                            let mut args_b = app_builder.init_args(3);
                            Expr::Record(*s, tl.to_vec()).transpile(env, args_b.reborrow().get(0).init_value());
                            args_b.reborrow().get(0).set_pos(());
                            args_b.reborrow().get(2).set_pos(());
                            args_b.reborrow().get(0).set_pos(());
                            args_b.reborrow().get(1).init_value().init_literal().set_string(field_name);
                            val.transpile(env, args_b.reborrow().get(2).init_value());
                        },
                        _ => todo!() // need to unpacking of record expansions -\_(o o)_/-
                    }
                } else {
                    builder.init_literal().set_empty_record(());
                }
            }
            Expr::Module(decls) => {
                let mut let_builder = builder.init_let();
                let mut child_env = SymbolMap::child(&env);
                decls.transpile(&mut child_env, let_builder.reborrow().init_binds());
                
                // collect the public decls as names in our record
                let pub_names = decls.declarations.iter().filter_map(extract_name).collect::<Vec<&str>>();

                let placeholder_span = Span::new(0, 0);
                let fields = 
                    pub_names.iter().map(|n| {Field::Simple(placeholder_span, n, Expr::Identifier(placeholder_span, n))});

                let record = Expr::Record(placeholder_span, fields.collect());
                record.transpile(&child_env, let_builder.reborrow().init_body())
            },
            Expr::Lambda(_, params, body) => {
                let mut lb = builder.init_lam();
                let mut pb = lb.reborrow().init_params(params.len() as u32);
                
                let mut new_env = SymbolMap::child(env);

                // doing param transpilation here for now since
                // it makes generating Symbol Maps easier
                for (i,param) in params.iter().enumerate() {
                    match param {
                        Parameter::Named(_, name) => {
                            let mut sym_builder = pb.reborrow().get(i as u32).init_symbol();
                            let disam = new_env.add(name);
                            sym_builder.set_disam(disam);
                            sym_builder.set_name(name);
                            pb.reborrow().get(i as u32).set_pos(());
                        },
                        _ => todo!(),
                    }
                };
                body.transpile(&new_env, lb.reborrow().init_body());
            },
            Expr::List(s, items) => {
                if let Some((hd, tl)) = items.split_first() {
                    let mut bb = builder.init_inline_builtin();
                    bb.set_op("__cons");
                    let mut args_builder = bb.reborrow().init_args(2);
                    let head = args_builder.reborrow().get(0);
                    hd.transpile(env, head);
                    let cons = args_builder.reborrow().get(1);
                    // i think this is O(n^2) ?
                    // could prob fix with slices or less recursion
                    Expr::List(*s, tl.to_vec()).transpile(env,cons);
                } else {
                    builder.init_literal().set_empty_list(());
                }
            }
            Expr::Prefix(_, _, _) => todo!(),
            // a call is an application of args followed by an invoke
            Expr::Call(_, fun, args) => {
                let ib = builder.init_invoke();
                let mut app_b = ib.init_app();
                fun.transpile(env, app_b.reborrow().init_lam());
                let mut args_b = app_b.reborrow().init_args(args.len() as u32);
                for (i, arg) in args.iter().enumerate() {
                    match arg {
                        Arg::Pos(_, arg_val) => {
                                args_b.reborrow().get(i as u32).set_pos(());
                                arg_val.transpile(env, args_b.reborrow().get(i as u32).init_value());
                            }
                        _ => todo!(),
                    }
                };
            },
            Expr::Scope(_, decls, val) => {
                if decls.is_empty() {
                    if let Some(e) = val.deref() {
                        e.transpile(&env, builder)
                    } else {
                        builder.init_literal().set_unit(());
                    }
                } else {
                    let mut let_builder = builder.init_let();
                    let mut child_env = SymbolMap::child(&env);
                    decls.transpile(&mut child_env, let_builder.reborrow().init_binds());
                    if let Some(e) = val.deref() {
                        e.transpile(&child_env, let_builder.reborrow().init_body())
                    } else {
                        let_builder.reborrow().init_body().init_literal().set_unit(());
                    }
                }
            },
            Expr::Project(_, _, _) => todo!(),
            Expr::Match(_, _, _) => todo!(),
        }
    }
}

impl<'src> LetBinding<'src> {
    pub fn new(b: (Pattern<'src>, Expr<'src>)) -> Self {
        LetBinding { binding: b }
    }

    // TODO: mutually recursive binds are not detected!!!
    pub fn transpile(&self, env: &mut SymbolMap, mut builder: BindBuilder<'_>) {
        match self {
            LetBinding{binding:(Pattern::Identifier(_, name), e)}=> {
                e.transpile(&env,builder.reborrow().init_value());
                let disam = env.add(name);
                let mut sym_builder = builder.reborrow().init_symbol();
                sym_builder.set_disam(disam);
                sym_builder.set_name(name);
            },
            _ => todo!()
        }
    }
}
impl<'src> Declaration<'src> {
    pub fn transpile<'a>(&self, env: &'a mut SymbolMap, mut builder: BindBuilder) {
        match self {
            Self::LetDeclare(_, _exported, binding) => {
                binding.transpile(env, builder);
            },
            // TODO: annotations?
            Declaration::FnDeclare(s, _b, name, params, body, _) => {
                let disam = env.add(name);
                let mut sym_builder = builder.reborrow().init_symbol();
                sym_builder.set_disam(disam);
                sym_builder.set_name(name);
                Expr::Lambda(*s, params.clone(), Box::new(body.clone())).transpile(&env, builder.init_value());
            },
        }
    }

    pub fn set_public(&mut self, is_public: bool) {
        let b = match self {
            Declaration::LetDeclare(_, b, _) => b,
            Declaration::FnDeclare(_, b, _, _, _, _) => b,
        };
        *b = is_public;
    }
}

fn extract_name<'a>(d: &'a Declaration) -> Option<&'a str> {
    match d {
        Declaration::LetDeclare(_, true, LetBinding{binding:(Pattern::Identifier(_, name),_)}) => {
            Some(name)
        },
        Declaration::FnDeclare(_, true, name, _, _, _) => Some(name),
        _ => None
    }
}
impl<'src> Declarations<'src> {
    pub fn new(span: Span, declarations: Vec<Declaration<'src>>) -> Self {
        Declarations { span, declarations }
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn transpile<'a>(&self, env: &'a mut SymbolMap, builder: BindsBuilder) {
        let mut bb = builder.init_binds(self.declarations.len() as u32);
        for (i,decl) in self.declarations.iter().enumerate() {
            decl.transpile(env,bb.reborrow().get(i as u32));
        }
    }
}