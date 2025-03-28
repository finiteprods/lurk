use std::sync::Arc;

use p3_baby_bear::BabyBear;
use p3_field::{AbstractField, PrimeField32};
use rustc_hash::FxHashSet;

use crate::{
    core::{big_num::field_elts_to_biguint, error::EvalErr},
    func,
    lair::{
        chipset::{Chipset, NoChip},
        expr::{BlockE, CaseType, CasesE, CtrlE, FuncE, OpE, Var},
        toplevel::Toplevel,
        FxIndexMap, Name,
    },
};

use super::{
    chipset::{lurk_chip_map, LurkChip},
    ingress::{egress, ingress, preallocate_symbols, InternalTag, SymbolsDigests},
    lang::{Coroutine, Lang},
    misc::{
        big_num_lessthan, digest_equal, hash3, hash4, hash5, u64_add, u64_divrem, u64_iszero,
        u64_lessthan, u64_mul, u64_sub,
    },
    symbol::Symbol,
    tag::Tag,
    zstore::{lurk_zstore, ZStore},
};

fn native_lurk_funcs<F: PrimeField32>(
    digests: &SymbolsDigests<F>,
    coroutines: &FxIndexMap<Symbol, Coroutine<F>>,
) -> [FuncE<F>; 40] {
    [
        lurk_main(),
        preallocate_symbols(digests),
        eval(),
        eval_builtin_expr(digests),
        eval_bind_builtin(),
        eval_env_builtin(),
        eval_env_literal(),
        eval_apply_builtin(),
        eval_coroutine_expr(digests, coroutines),
        eval_opening_unop(digests),
        eval_hide(),
        eval_unop(digests),
        eval_binop_num(digests),
        eval_binop_misc(digests),
        eval_begin(),
        eval_list(),
        coerce_if_sym(),
        open_comm(),
        equal(digests),
        equal_inner(),
        car_cdr(digests),
        eval_let(),
        eval_letrec(),
        extend_env_with_mutuals(),
        eval_letrec_bindings(),
        apply(digests),
        env_lookup(),
        ingress(digests),
        egress(digests),
        hash3(),
        hash4(),
        hash5(),
        u64_add(),
        u64_sub(),
        u64_mul(),
        u64_divrem(),
        u64_lessthan(),
        u64_iszero(),
        digest_equal(),
        big_num_lessthan(),
    ]
}

/// Creates a `Toplevel` with the functions used for Lurk evaluation and returns,
/// along with it:
/// * A `ZStore` with the Lurk (and `Lang`) symbols already interned
/// * All the `Lang` symbols in a `FxHashSet`
#[allow(clippy::type_complexity)]
pub fn build_lurk_toplevel<C2: Chipset<BabyBear>>(
    lang: Lang<BabyBear, C2>,
) -> (
    Arc<Toplevel<BabyBear, LurkChip, C2>>,
    ZStore<BabyBear, LurkChip>,
    FxHashSet<Symbol>,
) {
    let mut zstore = lurk_zstore();
    let lang_symbols = lang.coroutines().keys().cloned().collect();
    let digests = SymbolsDigests::new(&lang_symbols, &mut zstore);
    let (coroutines, gadgets) = lang.into_parts();
    let mut func_expr_map: FxIndexMap<_, _> = native_lurk_funcs(&digests, &coroutines)
        .into_iter()
        .map(|func_expr| (func_expr.name, func_expr))
        .collect();
    for coroutine in coroutines.into_values() {
        let Coroutine { func_expr, .. } = coroutine;
        let name = func_expr.name;
        assert!(
            !func_expr_map.contains_key(&name),
            "Name conflict with native function {name}"
        );
        func_expr_map.insert(name, func_expr);
    }
    let funcs_exprs: Vec<_> = func_expr_map.into_values().collect();
    let lurk_chip_map = lurk_chip_map(gadgets);
    let toplevel = Toplevel::new(&funcs_exprs, lurk_chip_map);
    (toplevel, zstore, lang_symbols)
}

#[inline]
#[allow(clippy::type_complexity)]
pub fn build_lurk_toplevel_native() -> (
    Arc<Toplevel<BabyBear, LurkChip, NoChip>>,
    ZStore<BabyBear, LurkChip>,
    FxHashSet<Symbol>,
) {
    build_lurk_toplevel(Lang::empty())
}

pub fn lurk_main<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn lurk_main(full_expr_tag: [8], expr_digest: [8], env_digest: [8]): [16] {
            let _foo: [0] = call(preallocate_symbols,); // TODO: replace by `exec` - needs to be constrained to run though
            // Ingress on expr
            let (expr_tag, expr) = call(ingress, full_expr_tag, expr_digest);
            // Ingress on env
            let padding = [0; 7];
            let env_tag = Tag::Env;
            let full_env_tag: [8] = (env_tag, padding);
            let (_env_tag, env) = call(ingress, full_env_tag, env_digest);
            // Evaluate expr, env
            let (val_tag, val) = call(eval, expr_tag, expr, env);
            // Egress on val
            let (val_tag, val_digest: [8]) = call(egress, val_tag, val);
            let full_val_tag: [8] = (val_tag, padding);
            return (full_val_tag, val_digest)
        }
    )
}

/// When `coroutines` is empty, `eval_coroutine_expr` shouldn't be called.
/// ```ignore
/// fn eval_coroutine_expr(_head, _args_tag, _args, _env): [2] {
///     let zero = 0;
///     let one = 1;
///     assert_eq!(zero, one);
///     return (zero zero)
/// }
/// ```
///
/// Otherwise we first evaluate the Lurk arguments and then expand them according
/// to the coroutine's `FuncE` arity
/// ```ignore
/// partial fn eval_coroutine_expr(head, args_tag, args, env): [2] {
///     let (args_tag, args) = call(eval_list, args_tag, args, env);
///     match args_tag {
///         Tag::Err => {
///             return (args_tag, args)
///         }
///     };
///     match head {
///         ...
///         digests.ptr(symN) => {
///             let err_tag = Tag::Err; ─────────────────────────────i==0───┐
///             let err = EvalErr::InvalidForm;                             │
///             match args_tag {                                            │
///                 InternalTag::Nil => {                                   │
///                     return (err_tag, err)                               │
///                 }                                                       │
///             };                                                          │
///             let (arg_tag1, arg1, args_tag, args) = load(args); ──i==1──┐│
///             match args_tag {                                           ││
///                 InternalTag::Nil => {                                  ││
///                     return (err_tag, err)                              ││
///                 }                                                      ││
///             };                                                         ││
///             let (arg_tag2, arg2, args_tag, args) = load(args); ──i==2─┐││
///             ...                                                       │││
///             let (arg_tagM, argM, args_tag, _args) = load(args); ─init┐│││
///             match args_tag {                                         ││││
///                 InternalTag::Nil => {                                ││││
///                     let (res_tag, res) = call(                       ││││
///                         funcN,                                       ││││
///                         arg_tag1, arg1,                              ││││
///                         arg_tag2, arg2,                              ││││
///                         ...                                          ││││
///                         arg_tagM, argM,                              ││││
///                         env, // iff `funcN` arity is odd             ││││
///                     );                                               ││││
///                     return (res_tag, res)                            ││││
///                 }                                                    ││││
///             };                                                       ││││
///             return (err_tag, err) ───────────────────────────────────┴┴┴┘
///         }
///         ...
///     }
/// }
/// ```
pub fn eval_coroutine_expr<F: AbstractField>(
    digests: &SymbolsDigests<F>,
    coroutines: &FxIndexMap<Symbol, Coroutine<F>>,
) -> FuncE<F> {
    let (partial, input_params, body) = if coroutines.is_empty() {
        let head = Var::atom("_head");
        let args_tag = Var::atom("_args_tag");
        let args = Var::atom("_args");
        let env = Var::atom("_env");
        let zero = Var::atom("zero");
        let one = Var::atom("one");
        let declare_zero = OpE::Const(zero, F::zero());
        let declare_one = OpE::Const(one, F::one());
        let assert_eq_zero_one = OpE::AssertEq(zero, one, None);
        (
            false,
            [head, args_tag, args, env].into(),
            BlockE {
                ops: [declare_zero, declare_one, assert_eq_zero_one].into(),
                ctrl: CtrlE::return_vars([zero, zero]),
            },
        )
    } else {
        let head = Var::atom("head");
        let args_tag = Var::atom("args_tag");
        let args = Var::atom("args");
        let env = Var::atom("env");
        let res_tag = Var::atom("res_tag");
        let res = Var::atom("res");

        // we need `Box::leak` in order to create a `&'static str` from a `String`
        let mk_var = |string: String| Var::atom(Box::leak(string.into_boxed_str()));

        // the default case of the outer match is executed when the evaluation of
        // the Lurk arguments doesn't find an error
        let match_head = {
            let branches = coroutines
                .iter()
                .map(|(symbol, coroutine)| {
                    let Coroutine {
                        lurk_arity,
                        uses_env,
                        func_expr,
                    } = coroutine;
                    let func_name = func_expr.name;
                    assert_eq!(
                        2, func_expr.output_size,
                        "Output size of {func_name} is not 2"
                    );
                    let input_size = func_expr.input_params.total_size();
                    assert_eq!(
                        input_size,
                        2 * lurk_arity + usize::from(*uses_env),
                        "Input size mismatch for {func_name}"
                    );
                    let mut call_args = Vec::with_capacity(input_size);
                    for i in 1..=*lurk_arity {
                        call_args.push(mk_var(format!("arg_tag{i}")));
                        call_args.push(mk_var(format!("arg{i}")));
                    }
                    if *uses_env {
                        call_args.push(env);
                    }
                    let return_res = CtrlE::return_vars([res_tag, res]);
                    let block = if *lurk_arity == 0 {
                        // no argument to pop
                        let call_op = OpE::Call([res_tag, res].into(), func_name, call_args.into());
                        BlockE {
                            ops: [call_op].into(),
                            ctrl: return_res,
                        }
                    } else {
                        // at least one argument to pop
                        let err_tag = Var::atom("err_tag");
                        let err = Var::atom("err");
                        // we start from the block that pops the last Lurk argument
                        let mut block = {
                            let coroutine_call =
                                OpE::Call([res_tag, res].into(), func_name, call_args.into());
                            // inner block containing the coroutine call
                            let coroutine_call_block = BlockE {
                                ops: [coroutine_call].into(),
                                ctrl: return_res,
                            };

                            let args_ = Var::atom("_args");
                            let arg_tag = mk_var(format!("arg_tag{lurk_arity}"));
                            let arg = mk_var(format!("arg{lurk_arity}"));
                            let load_op = OpE::Load([arg_tag, arg, args_tag, args_].into(), args);

                            let cases = CasesE {
                                branches: vec![(
                                    [InternalTag::Nil.to_field()].into(),
                                    (coroutine_call_block, CaseType::Constrained),
                                )],
                                default: Some(
                                    (
                                        BlockE::no_op(CtrlE::return_vars([err_tag, err])),
                                        CaseType::Constrained,
                                    )
                                        .into(),
                                ),
                            };
                            BlockE {
                                ops: [load_op].into(),
                                ctrl: CtrlE::Match(args_tag, cases),
                            }
                        };

                        // now iterate over the arity in reverse order, building outer
                        // blocks on top of the inner ones as represented in the docstring
                        for i in (0..*lurk_arity).rev() {
                            let ops = if i == 0 {
                                let declare_err_tag = OpE::Const(err_tag, Tag::Err.to_field());
                                let declare_err = OpE::Const(err, EvalErr::InvalidForm.to_field());
                                [declare_err_tag, declare_err].into()
                            } else {
                                let arg_tag = mk_var(format!("arg_tag{i}"));
                                let arg = mk_var(format!("arg{i}"));
                                [OpE::Load([arg_tag, arg, args_tag, args].into(), args)].into()
                            };
                            block = BlockE {
                                ops,
                                ctrl: CtrlE::Match(
                                    args_tag,
                                    CasesE {
                                        branches: vec![(
                                            [InternalTag::Nil.to_field()].into(),
                                            (
                                                BlockE::no_op(CtrlE::return_vars([err_tag, err])),
                                                CaseType::Constrained,
                                            ),
                                        )],
                                        default: Some((block, CaseType::Constrained).into()),
                                    },
                                ),
                            };
                        }
                        block
                    };
                    (
                        [digests.symbol_ptr(symbol).to_field()].into(),
                        (block, CaseType::Constrained),
                    )
                })
                .collect();
            CtrlE::Match(head, CasesE::no_default(branches))
        };
        // the outer match to check for an error when evaluating the Lurk arguments
        let match_args_tag = {
            let case = (
                [Tag::Err.to_field()].into(),
                (
                    BlockE::no_op(CtrlE::return_vars([args_tag, args])),
                    CaseType::Constrained,
                ),
            );
            let cases = CasesE {
                branches: vec![case],
                default: Some((BlockE::no_op(match_head), CaseType::Constrained).into()),
            };
            CtrlE::Match(args_tag, cases)
        };
        let eval_args = OpE::Call(
            [args_tag, args].into(),
            Name("eval_list"),
            [args_tag, args, env].into(),
        );
        (
            true,
            [head, args_tag, args, env].into(),
            BlockE {
                ops: [eval_args].into(),
                ctrl: match_args_tag,
            },
        )
    };

    FuncE {
        name: Name("eval_coroutine_expr"),
        invertible: false,
        partial,
        input_params,
        output_size: 2,
        body,
    }
}

pub fn eval<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval(expr_tag, expr, env): [2] {
            match expr_tag {
                Tag::Builtin, Tag::Sym, Tag::Coroutine => {
                    let expr_digest: [8] = load(expr);
                    let (res_tag, res) = call(env_lookup, expr_tag, expr_digest, env);
                    match res_tag {
                        Tag::Fix => {
                            // Fixed points are closed expressions, so we can choose an empty environment
                            let nil_env = 0;
                            let (res_tag, res) = call(eval, res_tag, res, nil_env);
                            return (res_tag, res)
                        }
                    };
                    return (res_tag, res)
                }
                Tag::Cons => {
                    let (head_tag, head, rest_tag, rest) = load(expr);
                    match head_tag {
                        Tag::Builtin => {
                            let (res_tag, res) = call(eval_builtin_expr, head, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                        Tag::Coroutine => {
                            let (res_tag, res) = call(eval_coroutine_expr, head, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                    };
                    let (head_tag, head) = call(eval, head_tag, head, env);
                    match head_tag {
                        Tag::BigNum, Tag::Comm => {
                            let (head_tag, head) = call(open_comm, head);
                            let (res_tag, res) = call(apply, head_tag, head, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                        Tag::Err => {
                            return (head_tag, head)
                        }
                    };
                    let (res_tag, res) = call(apply, head_tag, head, rest_tag, rest, env);
                    return (res_tag, res)
                }
                Tag::Fix => {
                    let (body_tag, body, binds_tag, binds, mutual_env) = load(expr);
                    // extend `mutual_env` with the fixed points from the `letrec` bindings
                    // IMPORTANT: at this point this operation cannot return an error
                    let (_tag, ext_env) = call(extend_env_with_mutuals, binds_tag, binds, binds, mutual_env, mutual_env);
                    let (res_tag, res) = call(eval, body_tag, body, ext_env);
                    return (res_tag, res)
                }
                Tag::Env => {
                    let (res_tag, res) = call(eval_env_literal, expr, env);
                    return (res_tag, res)
                }
            };
            return (expr_tag, expr)
        }
    )
}

pub fn eval_builtin_expr<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn eval_builtin_expr(head, rest_tag, rest, env): [2] {
            let nil_tag = InternalTag::Nil;
            let cons_tag = Tag::Cons;
            let err_tag = Tag::Err;
            let invalid_form = EvalErr::InvalidForm;
            match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                "let", "letrec", "lambda" => {
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (fst_tag, fst, rest_tag, rest) = load(rest);
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "let" => {
                            // fst: bindings list
                            // rest: list-like body
                            let (res_tag, res) = call(eval_let, fst_tag, fst, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                        "letrec" => {
                            // analogous to `let`
                            let (res_tag, res) = call(eval_letrec, fst_tag, fst, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                        "lambda" => {
                            // fst: parameters list
                            // rest: list-like body

                            // A function (more precisely, a closure) is an object with a
                            // parameter list, a body and an environment
                            let res_tag = Tag::Fun;
                            let res = store(fst_tag, fst, rest_tag, rest, env);
                            return (res_tag, res)
                        }
                    }
                }
                "cons", "strcons", "type-eq", "type-eqq", "apply" => {
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (fst_tag, fst, rest_tag, rest) = load(rest);
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (snd_tag, snd, rest_tag, _rest) = load(rest);
                    let rest_not_nil = sub(rest_tag, nil_tag);
                    if rest_not_nil {
                        return (err_tag, invalid_form)
                    }
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "cons", "strcons" => {
                            let (res_tag, res) = call(eval_binop_misc, head, fst_tag, fst, snd_tag, snd, env);
                            return (res_tag, res)
                        }
                        "type-eq" => {
                            let (fst_tag, fst) = call(eval, fst_tag, fst, env);
                            match fst_tag {
                                Tag::Err => {
                                    return (fst_tag, fst)
                                }
                            };
                            let (snd_tag, snd) = call(eval, snd_tag, snd, env);
                            match snd_tag {
                                Tag::Err => {
                                    return (snd_tag, snd)
                                }
                            };
                            let fst_tag = call(coerce_if_sym, fst_tag);
                            let snd_tag = call(coerce_if_sym, snd_tag);
                            let type_not_eq = sub(fst_tag, snd_tag);
                            if type_not_eq {
                                let nil = digests.lurk_symbol_ptr("nil");
                                return (nil_tag, nil)
                            }
                            let t_tag = InternalTag::T;
                            let t = digests.lurk_symbol_ptr("t");
                            return (t_tag, t)
                        }
                        "type-eqq" => {
                            let (snd_tag, snd) = call(eval, snd_tag, snd, env);
                            match snd_tag {
                                Tag::Err => {
                                    return (snd_tag, snd)
                                }
                            };
                            let fst_tag = call(coerce_if_sym, fst_tag);
                            let snd_tag = call(coerce_if_sym, snd_tag);
                            let type_not_eqq = sub(fst_tag, snd_tag);
                            if type_not_eqq {
                                let nil = digests.lurk_symbol_ptr("nil");
                                return (nil_tag, nil)
                            }
                            let t_tag = InternalTag::T;
                            let t = digests.lurk_symbol_ptr("t");
                            return (t_tag, t)
                        }
                        "apply" => {
                            let (res_tag, res) = call(eval_apply_builtin, fst_tag, fst, snd_tag, snd, env);
                            return (res_tag, res)
                        }
                    }
                }
                "list" => {
                    let (expr_tag, expr) = call(eval_list, rest_tag, rest, env);
                    return (expr_tag, expr)
                }
                "+", "-", "*", "/", "%", "=", "<", ">", "<=", ">=" => {
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (fst_tag, fst, rest_tag, rest) = load(rest);
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (snd_tag, snd, rest_tag, _rest) = load(rest);
                    let rest_not_nil = sub(rest_tag, nil_tag);
                    if rest_not_nil {
                        return (err_tag, invalid_form)
                    }
                    let (res_tag, res) = call(eval_binop_num, head, fst_tag, fst, snd_tag, snd, env);
                    return (res_tag, res)
                }
                "eval" => {
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (expr_tag, expr, rest_tag, rest) = load(rest);
                    match rest_tag {
                        InternalTag::Nil => {
                            // Eval must be called twice, first with the original env and then
                            // with an empty env
                            let (res_tag, res) = call(eval, expr_tag, expr, env);
                            match res_tag {
                                Tag::Err => {
                                    return (res_tag, res)
                                }
                            };
                            let env = 0;
                            let (res_tag, res) = call(eval, res_tag, res, env);
                            return (res_tag, res)
                        }
                        Tag::Cons => {
                            let (env_expr_tag, env_expr, rest_tag, _rest) = load(rest);
                            let rest_not_nil = sub(rest_tag, nil_tag);
                            if rest_not_nil {
                                return (err_tag, invalid_form)
                            }
                            let (res_tag, res) = call(eval, expr_tag, expr, env);
                            match res_tag {
                                Tag::Err => {
                                    return (res_tag, res)
                                }
                            };
                            let (env_tag, new_env) = call(eval, env_expr_tag, env_expr, env);
                            match env_tag {
                                Tag::Err => {
                                    return (env_tag, new_env)
                                }
                                Tag::Env => {
                                    let (res_tag, res) = call(eval, res_tag, res, new_env);
                                    return (res_tag, res)
                                }
                            };
                            let err = EvalErr::NotEnv;
                            return (err_tag, err)
                        }
                    };
                    let not_env = EvalErr::NotEnv;
                    return (err_tag, not_env)
                }
                "quote" => {
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (expr_tag, expr, rest_tag, _rest) = load(rest);
                    let rest_not_nil = sub(rest_tag, nil_tag);
                    if rest_not_nil {
                        return (err_tag, invalid_form)
                    }
                    return (expr_tag, expr)
                }
                "begin" => {
                    let (expr_tag, expr) = call(eval_begin, rest_tag, rest, env);
                    return (expr_tag, expr)
                }
                "current-env", "empty-env", "fail" => {
                    let rest_not_nil = sub(rest_tag, nil_tag);
                    if rest_not_nil {
                        return (err_tag, invalid_form)
                    }
                    let env_tag = Tag::Env;
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "current-env" => {
                            return (env_tag, env)
                        }
                        "empty-env" => {
                            let env = 0;
                            return (env_tag, env)
                        }
                        "fail" => {
                            let zero = 0;
                            let one = 1;
                            assert_eq!(zero, one, |_, _| "Explicit fail encountered".to_string());
                            return (zero, zero)
                        }
                    }
                }
                "bind" => {
                    let (res_tag, res) = call(eval_bind_builtin, rest_tag, rest, env);
                    return (res_tag, res)
                }
                "env" => {
                    let (res_tag, res) = call(eval_env_builtin, rest_tag, rest, env);
                    return (res_tag, res)
                }
                "breakpoint" => {
                    breakpoint;
                    match rest_tag {
                        InternalTag::Nil => {
                            let nil = digests.lurk_symbol_ptr("nil");
                            return (nil_tag, nil)
                        }
                        Tag::Cons => {
                            let (expr_tag, expr, rest_tag, _rest) = load(rest);
                            let rest_not_nil = sub(rest_tag, nil_tag);
                            if rest_not_nil {
                                return (err_tag, invalid_form)
                            }
                            let (val_tag, val) = call(eval, expr_tag, expr, env);
                            return (val_tag, val)
                        }
                    }
                }
                "if" => {
                    // An if expression is a list of 3 or 4 elements
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (expr_tag, expr, rest_tag, rest) = load(rest);
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    let (t_branch_tag, t_branch, rest_tag, rest) = load(rest);
                    match rest_tag {
                        InternalTag::Nil => {
                            let (val_tag, val) = call(eval, expr_tag, expr, env);
                            match val_tag {
                                InternalTag::Nil, Tag::Err => {
                                    return (val_tag, val)
                                }
                            };
                            let (res_tag, res) = call(eval, t_branch_tag, t_branch, env);
                            return (res_tag, res)
                        }
                        Tag::Cons => {
                            let (f_branch_tag, f_branch, rest_tag, _rest) = load(rest);
                            let rest_not_nil = sub(rest_tag, nil_tag);
                            if rest_not_nil {
                                return (err_tag, invalid_form)
                            }
                            let (val_tag, val) = call(eval, expr_tag, expr, env);
                            match val_tag {
                                InternalTag::Nil => {
                                    let (res_tag, res) = call(eval, f_branch_tag, f_branch, env);
                                    return (res_tag, res)
                                }
                                Tag::Err => {
                                    return (val_tag, val)
                                }
                            };
                            let (res_tag, res) = call(eval, t_branch_tag, t_branch, env);
                            return (res_tag, res)
                        }
                    };
                    return (err_tag, invalid_form)
                }
                "eq" => {
                    let one = 1;
                    let res: [2] = call(equal, rest_tag, rest, env, one);
                    return res
                }
                "eqq" => {
                    let zero = 0;
                    let res: [2] = call(equal, rest_tag, rest, env, zero);
                    return res
                }
                "hide" => {
                    let (res_tag, res) = call(eval_hide, rest_tag, rest, env);
                    return (res_tag, res)
                }
                "car", "cdr" => {
                    let (car_tag, car, cdr_tag, cdr) = call(car_cdr, rest_tag, rest, env);
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "car" => {
                            return (car_tag, car)
                        }
                        "cdr" => {
                            return (cdr_tag, cdr)
                        }
                    }
                }
                "u64", "char", "atom", "emit", "bignum", "comm" => {
                    let (res_tag, res) = call(eval_unop, head, rest_tag, rest, env);
                    return (res_tag, res)
                }
                "commit", "open", "secret" => {
                    let (res_tag, res) = call(eval_opening_unop, head, rest_tag, rest, env);
                    return (res_tag, res)
                }
            }
        }
    )
}

/// If `rest` is of form `(sym val env)`, evaluate those to `sym'`, `val'` and `env'`
/// respectively, early returning errors if found. Then return an environment with
/// `(sym' . val')` as the head binding and `env'` as the tail.
pub fn eval_bind_builtin<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_bind_builtin(rest_tag, rest, env): [2] {
            let nil_tag = InternalTag::Nil;
            let cons_tag = Tag::Cons;
            let err_tag = Tag::Err;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (fst_tag, fst, rest_tag, rest) = load(rest);
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (snd_tag, snd, rest_tag, rest) = load(rest);
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (trd_tag, trd, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form)
            }
            let (fst_tag, fst) = call(eval, fst_tag, fst, env);
            match fst_tag {
                Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                    let (snd_tag, snd) = call(eval, snd_tag, snd, env);
                    let not_err = sub(snd_tag, err_tag);
                    if !not_err {
                        return (snd_tag, snd)
                    }
                    let (trd_tag, trd) = call(eval, trd_tag, trd, env);
                    match trd_tag {
                        Tag::Env => {
                            let env = store(fst_tag, fst, snd_tag, snd, trd);
                            return (trd_tag, env) // trd_tag is Tag::Env
                        }
                        Tag::Err => {
                            return (trd_tag, trd)
                        }
                    };
                    let err = EvalErr::NotEnv;
                    return (err_tag, err)
                }
                Tag::Err => {
                    return (fst_tag, fst)
                }
            };
            let err = EvalErr::IllegalBindingVar;
            return (err_tag, err)
        }
    )
}

/// If `rest` is of form `(b1 b2 ... bn)`, evaluate those, early returning errors
/// if found. Then expect each evaluated term `i` to be of form `(symi . vali)`
/// and return an environment with bindings `(sym1 . val1)`, `(sym2 . val2)`, ...,
/// `(symn . valn)`.
pub fn eval_env_builtin<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_env_builtin(rest_tag, rest, env): [2] {
            let env_tag = Tag::Env;
            let err_tag = Tag::Err;
            match rest_tag {
                InternalTag::Nil => {
                    let env = 0;
                    return (env_tag, env)
                }
                Tag::Cons => {
                    let (head_tag, head, rest_tag, rest) = load(rest);
                    let (head_tag, head) = call(eval, head_tag, head, env);
                    match head_tag {
                        Tag::Cons => {
                            let (sym_tag, sym, val_tag, val) = load(head);
                            match sym_tag {
                                Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                                    let (tail_env_tag, tail_env) = call(eval_env_builtin, rest_tag, rest, env);
                                    match tail_env_tag {
                                        Tag::Env => {
                                            let env = store(sym_tag, sym, val_tag, val, tail_env);
                                            return (env_tag, env)
                                        }
                                        Tag::Err => {
                                            return (tail_env_tag, tail_env)
                                        }
                                    }
                                }
                            };
                            let err = EvalErr::IllegalBindingVar;
                            return (err_tag, err)
                        }
                        Tag::Err => {
                            return (head_tag, head)
                        }
                    };
                    let err = EvalErr::NotCons;
                    return (err_tag, err)
                }
            };
            let err = EvalErr::InvalidForm;
            return (err_tag, err)
        }
    )
}

pub fn eval_env_literal<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_env_literal(env_literal, env): [2] {
            let env_tag = Tag::Env;
            let err_tag = Tag::Err;
            let nil_env = 0;
            if !env_literal {
                return (env_tag, nil_env)
            }
            let (sym_tag, sym, val_tag, val, rest) = load(env_literal);
            match sym_tag {
                Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                    let (val_tag, val) = call(eval, val_tag, val, env);
                    match val_tag {
                        Tag::Err => {
                            return (val_tag, val)
                        }
                    };
                    let (tail_env_tag, tail_env) = call(eval_env_literal, rest, env);
                    match tail_env_tag {
                        Tag::Env => {
                            let env = store(sym_tag, sym, val_tag, val, tail_env);
                            return (env_tag, env)
                        }
                        Tag::Err => {
                            return (tail_env_tag, tail_env)
                        }
                    };
                    let err = EvalErr::InvalidForm;
                    return (err_tag, err)
                }
            };
            let err = EvalErr::IllegalBindingVar;
            return (err_tag, err)
        }
    )
}

pub fn eval_apply_builtin<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_apply_builtin(fst_tag, fst, snd_tag, snd, env): [2] {
            let (fst_tag, fst) = call(eval, fst_tag, fst, env);
            match fst_tag {
                Tag::Err => {
                    return (fst_tag, fst)
                }
            };
            let (snd_tag, snd) = call(eval, snd_tag, snd, env);
            match snd_tag {
                Tag::Err => {
                    return (snd_tag, snd)
                }
            };
            let (res_tag, res) = call(apply, fst_tag, fst, snd_tag, snd, env);
            return (res_tag, res)
        }
    )
}

pub fn coerce_if_sym<F: AbstractField>() -> FuncE<F> {
    func!(
        fn coerce_if_sym(tag): [1] {
            match tag {
                InternalTag::Nil, InternalTag::T => {
                    let sym_tag = Tag::Sym;
                    return sym_tag
                }
            };
            return tag
        }
    )
}

pub fn open_comm<F: PrimeField32>() -> FuncE<F> {
    func!(
        fn open_comm(hash_ptr): [2] {
            let comm_hash: [8] = load(hash_ptr);
            let (_secret: [8], payload_tag, padding: [7], val_digest: [8]) = preimg(
                hash3,
                comm_hash,
                |fs| format!("Preimage not found for #{:#x}", field_elts_to_biguint(fs))
            );
            let (payload_tag, payload_ptr) = call(ingress, payload_tag, padding, val_digest);
            return (payload_tag, payload_ptr)
        }
    )
}

pub fn car_cdr<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn car_cdr(rest_tag, rest, env): [4] {
            let nil = digests.lurk_symbol_ptr("nil");
            let nil_tag = InternalTag::Nil;
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form, err_tag, invalid_form)
            }
            let (expr_tag, expr, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form, err_tag, invalid_form)
            }
            let (val_tag, val) = call(eval, expr_tag, expr, env);
            match val_tag {
                Tag::Err => {
                    return (val_tag, val, val_tag, val)
                }
                Tag::Cons => {
                    let (car_tag, car, cdr_tag, cdr) = load(val);
                    return (car_tag, car, cdr_tag, cdr)
                }
                InternalTag::Nil => {
                    return (nil_tag, nil, nil_tag, nil)
                }
                Tag::Str => {
                    let empty = 0;
                    let not_empty = sub(val, empty);
                    if not_empty {
                        let (car_tag, car, cdr_tag, cdr) = load(val);
                        return (car_tag, car, cdr_tag, cdr)
                    }
                    let str_tag = Tag::Str;
                    return (nil_tag, nil, str_tag, empty)
                }
            };
            let not_cons = EvalErr::NotCons;
            return (err_tag, not_cons, err_tag, not_cons)

        }
    )
}

pub fn equal<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn equal(rest_tag, rest, env, eval_first): [2] {
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let nil_tag = InternalTag::Nil;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (exp1_tag, exp1, rest_tag, rest) = load(rest);
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (exp2_tag, exp2, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form)
            }
            let (val2_tag, val2) = call(eval, exp2_tag, exp2, env);
            match val2_tag {
                Tag::Err => {
                    return (val2_tag, val2)
                }
            };
            if eval_first {
                let (val1_tag, val1) = call(eval, exp1_tag, exp1, env);
                match val1_tag {
                    Tag::Err => {
                        return (val1_tag, val1)
                    }
                };
                let is_equal_inner = call(equal_inner, val1_tag, val1, val2_tag, val2);
                if is_equal_inner {
                    let t_tag = InternalTag::T;
                    let t = digests.lurk_symbol_ptr("t");
                    return (t_tag, t)
                }
                return (nil_tag, is_equal_inner) // `is_equal_inner` is zero
            }
            let is_equal_inner = call(equal_inner, exp1_tag, exp1, val2_tag, val2);
            if is_equal_inner {
                let t_tag = InternalTag::T;
                let t = digests.lurk_symbol_ptr("t");
                return (t_tag, t)
            }
            return (nil_tag, is_equal_inner) // `is_equal_inner` is zero
        }
    )
}

pub fn equal_inner<F: AbstractField>() -> FuncE<F> {
    func!(
        fn equal_inner(a_tag, a, b_tag, b): [1] {
            let not_eq_tag = sub(a_tag, b_tag);
            let zero = 0;
            let one = 1;
            if not_eq_tag {
                return zero
            }
            let not_eq = sub(a, b);
            if !not_eq {
                return one
            }
            match a_tag {
                // The Nil and Err cases are impossible
                Tag::Num => {
                    return zero
                }
                Tag::Char => {
                    let a_bytes: [4] = load(a);
                    let b_bytes: [4] = load(b);
                    let diff = sub(a_bytes, b_bytes);
                    if diff {
                        return zero
                    }
                    return one
                }
                Tag::Key, Tag::Sym, Tag::Builtin, Tag::Coroutine, Tag::U64, Tag::BigNum, Tag::Comm => {
                    let a_digest: [8] = load(a);
                    let b_digest: [8] = load(b);
                    let diff = sub(a_digest, b_digest);
                    if diff {
                        return zero
                    }
                    return one
                }
                Tag::Str => {
                    let a_and_b = mul(a, b);
                    if !a_and_b {
                        return zero
                    }
                    let (a_fst: [2], a_snd: [2]) = load(a);
                    let (b_fst: [2], b_snd: [2]) = load(b);
                    let fst_eq = call(equal_inner, a_fst, b_fst);
                    let snd_eq = call(equal_inner, a_snd, b_snd);
                    let eq = mul(fst_eq, snd_eq);
                    return eq
                }
                Tag::Cons => {
                    let (a_fst: [2], a_snd: [2]) = load(a);
                    let (b_fst: [2], b_snd: [2]) = load(b);
                    let fst_eq = call(equal_inner, a_fst, b_fst);
                    let snd_eq = call(equal_inner, a_snd, b_snd);
                    let eq = mul(fst_eq, snd_eq);
                    return eq
                }
                Tag::Fun, Tag::Fix => {
                    let trd_tag = Tag::Env;
                    let (a_fst: [2], a_snd: [2], a_trd) = load(a);
                    let (b_fst: [2], b_snd: [2], b_trd) = load(b);
                    let fst_eq = call(equal_inner, a_fst, b_fst);
                    let snd_eq = call(equal_inner, a_snd, b_snd);
                    let trd_eq = call(equal_inner, trd_tag, a_trd, trd_tag, b_trd);
                    let eq = mul(fst_eq, snd_eq);
                    let eq = mul(eq, trd_eq);
                    return eq
                }
                Tag::Env => {
                    let a_and_b = mul(a, b);
                    if !a_and_b {
                        return zero
                    }
                    let (a_fst: [2], a_snd: [2], a_trd) = load(a);
                    let (b_fst: [2], b_snd: [2], b_trd) = load(b);
                    let fst_eq = call(equal_inner, a_fst, b_fst);
                    let snd_eq = call(equal_inner, a_snd, b_snd);
                    let trd_eq = call(equal_inner, a_tag, a_trd, a_tag, b_trd); // `a_tag` is `Tag::Env`
                    let eq = mul(fst_eq, snd_eq);
                    let eq = mul(eq, trd_eq);
                    return eq
                }
            }
        }
    )
}

pub fn eval_list<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_list(rest_tag, rest, env): [2] {
            match rest_tag {
                InternalTag::Nil => {
                    return (rest_tag, rest)
                }
                Tag::Cons => {
                    let (head_tag, head, rest_tag, rest) = load(rest);
                    let (head_tag, head) = call(eval, head_tag, head, env);
                    match head_tag {
                        Tag::Err => {
                            return (head_tag, head)
                        }
                    };
                    let (rest_tag, rest) = call(eval_list, rest_tag, rest, env);
                    match rest_tag {
                        Tag::Err => {
                            return (rest_tag, rest)
                        }
                    };
                    let cons_tag = Tag::Cons;
                    let cons = store(head_tag, head, rest_tag, rest);
                    return (cons_tag, cons)
                }
            };
            let err_tag = Tag::Err;
            let err = EvalErr::InvalidForm;
            return (err_tag, err)
        }
    )
}

pub fn eval_begin<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_begin(rest_tag, rest, env): [2] {
            match rest_tag {
                InternalTag::Nil => {
                    return (rest_tag, rest)
                }
                Tag::Cons => {
                    let (head_tag, head, rest_tag, rest) = load(rest);
                    let (head_tag, head) = call(eval, head_tag, head, env);
                    match head_tag {
                        Tag::Err => {
                            return (head_tag, head)
                        }
                    };
                    let nil_tag = InternalTag::Nil;
                    let rest_not_nil = sub(nil_tag, rest_tag);
                    if rest_not_nil {
                        let (res_tag, res) = call(eval_begin, rest_tag, rest, env);
                        return (res_tag, res)
                    }
                    return (head_tag, head)
                }
            };
            let err_tag = Tag::Err;
            let err = EvalErr::InvalidForm;
            return (err_tag, err)
        }
    )
}

pub fn eval_binop_num<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn eval_binop_num(head, exp1_tag, exp1, exp2_tag, exp2, env): [2] {
            let err_tag = Tag::Err;
            let num_tag = Tag::Num;
            let u64_tag = Tag::U64;
            let nil_tag = InternalTag::Nil;
            let err_div_zero = EvalErr::DivByZero;
            let t = digests.lurk_symbol_ptr("t");
            let nil = digests.lurk_symbol_ptr("nil");
            let (val1_tag, val1) = call(eval, exp1_tag, exp1, env);
            match val1_tag {
                Tag::Err => {
                    return (val1_tag, val1)
                }
            };
            let (val2_tag, val2) = call(eval, exp2_tag, exp2, env);
            match val2_tag {
                Tag::Err => {
                    return (val2_tag, val2)
                }
            };
            let t_tag = InternalTag::T;
            let tags: [2] = (val1_tag, val2_tag);
            match tags {
                [Tag::U64, Tag::U64] => {
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "+" => {
                            let res = call(u64_add, val1, val2);
                            return (u64_tag, res)
                        }
                        "-" => {
                            let res = call(u64_sub, val1, val2);
                            return (u64_tag, res)
                        }
                        "*" => {
                            let res = call(u64_mul, val1, val2);
                            return (u64_tag, res)
                        }
                        "/", "%" => {
                            let is_zero = call(u64_iszero, val2);
                            if is_zero {
                                return (err_tag, err_div_zero)
                            }
                            let (quot, rem) = call(u64_divrem, val1, val2);
                            match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                                "/" => {
                                    return (u64_tag, quot)
                                }
                                "%" => {
                                    return (u64_tag, rem)
                                }
                            }
                        }
                        "<" => {
                            let res = call(u64_lessthan, val1, val2);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                        ">=" => {
                            let res = call(u64_lessthan, val1, val2);
                            if res {
                                return (nil_tag, nil)
                            }
                            return (t_tag, t)

                        }
                        ">" => {
                            let res = call(u64_lessthan, val2, val1);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                        "<=" => {
                            let res = call(u64_lessthan, val2, val1);
                            if res {
                                return (nil_tag, nil)
                            }
                            return (t_tag, t)
                        }
                        "=" => {
                            let res = call(digest_equal, val1, val2);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                    }
                }
                [Tag::Num, Tag::Num] => {
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "+" => {
                            let res = add(val1, val2);
                            return (num_tag, res)
                        }
                        "-" => {
                            let res = sub(val1, val2);
                            return (num_tag, res)
                        }
                        "*" => {
                            let res = mul(val1, val2);
                            return (num_tag, res)
                        }
                        "/" => {
                            if !val2 {
                                return (err_tag, err_div_zero)
                            }
                            let res = div(val1, val2);
                            return (num_tag, res)
                        }
                        "=" => {
                            let diff = sub(val1, val2);
                            if diff {
                                return (nil_tag, nil)
                            }
                            return (t_tag, t)
                        }
                        "%", "<", ">", "<=", ">=" => {
                            let err = EvalErr::NotU64;
                            return (err_tag, err)
                        }
                    }
                }
                [Tag::BigNum, Tag::BigNum] => {
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "<" => {
                            let res = call(big_num_lessthan, val1, val2);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                        ">=" => {
                            let res = call(big_num_lessthan, val1, val2);
                            if res {
                                return (nil_tag, nil)
                            }
                            return (t_tag, t)

                        }
                        ">" => {
                            let res = call(big_num_lessthan, val2, val1);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                        "<=" => {
                            let res = call(big_num_lessthan, val2, val1);
                            if res {
                                return (nil_tag, nil)
                            }
                            return (t_tag, t)
                        }
                        "=" => {
                            let res = call(digest_equal, val2, val1);
                            if res {
                                return (t_tag, t)
                            }
                            return (nil_tag, nil)
                        }
                        "+", "-", "*", "/", "%" => {
                            let err = EvalErr::InvalidArg;
                            return (err_tag, err)
                        }
                    }
                }
            };
            let err = EvalErr::InvalidArg;
            return (err_tag, err)
        }
    )
}

pub fn eval_binop_misc<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn eval_binop_misc(head, exp1_tag, exp1, exp2_tag, exp2, env): [2] {
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let (val1_tag, val1) = call(eval, exp1_tag, exp1, env);
            match val1_tag {
                Tag::Err => {
                    return (val1_tag, val1)
                }
            };
            let (val2_tag, val2) = call(eval, exp2_tag, exp2, env);
            match val2_tag {
                Tag::Err => {
                    return (val2_tag, val2)
                }
            };
            match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                "cons" => {
                    let cons = store(val1_tag, val1, val2_tag, val2);
                    return (cons_tag, cons)
                }
                "strcons" => {
                    let char_tag = Tag::Char;
                    let str_tag = Tag::Str;
                    let strcons = store(val1_tag, val1, val2_tag, val2);
                    let not_char = sub(val1_tag, char_tag);
                    let not_str = sub(val2_tag, str_tag);
                    if not_char {
                        let err = EvalErr::NotChar;
                        return (err_tag, err)
                    }
                    if not_str {
                        let err = EvalErr::NotString;
                        return (err_tag, err)
                    }
                    return (str_tag, strcons)
                }
            }
        }
    )
}

pub fn eval_unop<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn eval_unop(head, rest_tag, rest, env): [2] {
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let nil_tag = InternalTag::Nil;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (expr_tag, expr, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form)
            }
            let (val_tag, val) = call(eval, expr_tag, expr, env);
            match val_tag {
                Tag::Err => {
                    return (val_tag, val)
                }
            };

            match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                "atom" => {
                    let val_not_cons = sub(val_tag, cons_tag);
                    if val_not_cons {
                        let t_tag = InternalTag::T;
                        let t = digests.lurk_symbol_ptr("t");
                        return (t_tag, t)
                    }
                    let nil = digests.lurk_symbol_ptr("nil");
                    return (nil_tag, nil)
                }
                "emit" => {
                    emit(val_tag, val);
                    return (val_tag, val)
                }
                "u64" => {
                    match val_tag {
                        Tag::U64 => {
                            return (val_tag, val)
                        }
                        Tag::Char => {
                            let bytes: [4] = load(val);
                            let padding = [0; 4];
                            let val = store(bytes, padding);
                            let val_tag = Tag::U64;
                            return (val_tag, val)
                        }
                    };
                    let err = EvalErr::CantCastToU64;
                    return(err_tag, err)
                }
                "char" => {
                    match val_tag {
                        Tag::Char => {
                            return (val_tag, val)
                        }
                        Tag::U64 => {
                            let (bytes: [4], _ignored: [4]) = load(val);
                            let val = store(bytes);
                            let val_tag = Tag::Char;
                            return (val_tag, val)
                        }
                    };
                    let err = EvalErr::CantCastToChar;
                    return(err_tag, err)
                }
                "bignum" => {
                    match val_tag {
                        Tag::BigNum => {
                            return (val_tag, val)
                        }
                        Tag::Comm => {
                            let big_num_tag = Tag::BigNum;
                            return (big_num_tag, val)
                        }
                    };
                    let err = EvalErr::CantCastToBigNum;
                    return(err_tag, err)
                }
                "comm" => {
                    match val_tag {
                        Tag::BigNum => {
                            let comm_tag = Tag::Comm;
                            return (comm_tag, val)
                        }
                        Tag::Comm => {
                            return (val_tag, val)
                        }
                    };
                    let err = EvalErr::CantCastToComm;
                    return(err_tag, err)
                }
             }
        }
    )
}

pub fn eval_opening_unop<F: PrimeField32>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn eval_opening_unop(head, rest_tag, rest, env): [2] {
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let nil_tag = InternalTag::Nil;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (expr_tag, expr, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form)
            }
            let (val_tag, val) = call(eval, expr_tag, expr, env);
            match val_tag {
                Tag::Err => {
                    return (val_tag, val)
                }
            };

            match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                "commit" => {
                    let (val_tag, val_digest: [8]) = call(egress, val_tag, val);
                    let padding = [0; 7];
                    let zero = 0;
                    let comm_hash: [8] = call(hash3, zero, padding, val_tag, padding, val_digest);
                    let comm_tag = Tag::Comm;
                    let comm_ptr = store(comm_hash);
                    return (comm_tag, comm_ptr)
                }
            };
            // default case must be `open` or `secret`
            match val_tag {
                Tag::Comm, Tag::BigNum => {
                    let comm_hash: [8] = load(val);
                    let (secret: [8], tag, padding: [7], val_digest: [8]) = preimg(
                        hash3,
                        comm_hash,
                        |fs| format!("Preimage not found for #{:#x}", field_elts_to_biguint(fs))
                    );
                    match head [|name| digests.builtin_symbol_ptr(name).to_field()] {
                        "open" => {
                            let (tag, ptr) = call(ingress, tag, padding, val_digest);
                            return (tag, ptr)
                        }
                        "secret" => {
                            let ptr = store(secret);
                            let big_num_tag = Tag::BigNum;
                            return (big_num_tag, ptr)
                        }
                    }
                }
            };
            let cant_open = EvalErr::CantOpen;
            return (err_tag, cant_open)
        }
    )
}

pub fn eval_hide<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_hide(rest_tag, rest, env): [2] {
            let err_tag = Tag::Err;
            let cons_tag = Tag::Cons;
            let nil_tag = InternalTag::Nil;
            let invalid_form = EvalErr::InvalidForm;
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (exp1_tag, exp1, rest_tag, rest) = load(rest);
            let rest_not_cons = sub(rest_tag, cons_tag);
            if rest_not_cons {
                return (err_tag, invalid_form)
            }
            let (exp2_tag, exp2, rest_tag, _rest) = load(rest);
            let rest_not_nil = sub(rest_tag, nil_tag);
            if rest_not_nil {
                return (err_tag, invalid_form)
            }
            let (val1_tag, val1) = call(eval, exp1_tag, exp1, env);
            match val1_tag {
                Tag::Err => {
                    return (val1_tag, val1)
                }
            };
            let (val2_tag, val2) = call(eval, exp2_tag, exp2, env);
            match val2_tag {
                Tag::Err => {
                    return (val2_tag, val2)
                }
            };
            match val1_tag {
                Tag::BigNum => {
                    let secret: [8] = load(val1);
                    let (val2_tag, val2_digest: [8]) = call(egress, val2_tag, val2);
                    let padding = [0; 7];
                    let comm_hash: [8] = call(hash3, secret, val2_tag, padding, val2_digest);
                    let comm_ptr = store(comm_hash);
                    let comm_tag = Tag::Comm;
                    return (comm_tag, comm_ptr)
                }
            };
            let not_comm = EvalErr::NotBigNum;
            return (err_tag, not_comm)
        }
    )
}

pub fn eval_let<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_let(binds_tag, binds, body_tag, body, env): [2] {
            let err_tag = Tag::Err;
            let invalid_form = EvalErr::InvalidForm;
            match binds_tag {
                InternalTag::Nil => {
                    let (res_tag, res) = call(eval_begin, body_tag, body, env);
                    return (res_tag, res)
                }
                Tag::Cons => {
                    let cons_tag = Tag::Cons;
                    let nil_tag = InternalTag::Nil;
                    // `binds` is a list of bindings
                    let (bind_tag, bind, rest_binds_tag, rest_binds) = load(binds);
                    let bind_not_cons = sub(bind_tag, cons_tag);
                    if bind_not_cons {
                        return (err_tag, invalid_form)
                    }
                    // each binding is in turn a 2 element list
                    let (param_tag, param, rest_tag, rest) = load(bind);
                    let rest_not_cons = sub(rest_tag, cons_tag);
                    if rest_not_cons {
                        return (err_tag, invalid_form)
                    }
                    match param_tag {
                        Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                            let (expr_tag, expr, rest_tag, _rest) = load(rest);
                            let rest_not_nil = sub(rest_tag, nil_tag);
                            if rest_not_nil {
                                return (err_tag, invalid_form)
                            }

                            let (val_tag, val) = call(eval, expr_tag, expr, env);
                            match val_tag {
                                Tag::Err => {
                                    return (val_tag, val)
                                }
                            };
                            let ext_env = store(param_tag, param, val_tag, val, env);
                            let rest_binds_not_nil = sub(nil_tag, rest_binds_tag);
                            if rest_binds_not_nil {
                                let (res_tag, res) = call(eval_let, rest_binds_tag, rest_binds, body_tag, body, ext_env);
                                return (res_tag, res)
                            }
                            let (res_tag, res) = call(eval_begin, body_tag, body, ext_env);
                            return (res_tag, res)
                        }
                    };
                    let err = EvalErr::IllegalBindingVar;
                    return (err_tag, err)

                }
            };
            return (err_tag, invalid_form)
        }
    )
}

/// Extends the original environment of a `letrec` with fixed points from its list of mutual bindings
pub fn extend_env_with_mutuals<F: AbstractField>() -> FuncE<F> {
    func!(
        fn extend_env_with_mutuals(binds_tag, binds, mutual_binds, mutual_env, ext_env): [2] {
            let err_tag = Tag::Err;
            let env_tag = Tag::Env;
            let invalid_form_err = EvalErr::InvalidForm;
            match binds_tag {
                InternalTag::Nil => {
                    return (env_tag, ext_env)
                }
                Tag::Cons => {
                    let cons_tag = Tag::Cons;
                    let (binding_tag, binding, binds_tag, binds) = load(binds);
                    let binding_not_cons = sub(binding_tag, cons_tag);
                    if binding_not_cons {
                        return (err_tag, invalid_form_err)
                    }
                    let (var_tag, var, rest_tag, rest) = load(binding);
                    let rest_tag_not_cons = sub(rest_tag, cons_tag);
                    if rest_tag_not_cons {
                        return (err_tag, invalid_form_err)
                    }
                    let (expr_tag, expr, rest_tag, _rest) = load(rest);
                    let nil_tag = InternalTag::Nil;
                    let rest_tag_not_nil = sub(rest_tag, nil_tag);
                    if rest_tag_not_nil {
                        return (err_tag, invalid_form_err)
                    }
                    match var_tag {
                        Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                            let fix_tag = Tag::Fix;
                            // IMPORTANT: At this point, `mutual_binds` must be a cons
                            let fix = store(expr_tag, expr, cons_tag, mutual_binds, mutual_env);
                            let ext_env = store(var_tag, var, fix_tag, fix, ext_env);
                            let (res_tag, ext_env) = call(extend_env_with_mutuals, binds_tag, binds, mutual_binds, mutual_env, ext_env);
                            return (res_tag, ext_env)
                        }
                    };
                    let illegal_binding_var_err = EvalErr::IllegalBindingVar;
                    return (err_tag, illegal_binding_var_err)
                }
            };
            return (err_tag, invalid_form_err)
        }
    )
}

/// Evaluates the mutual fixed points in an environment extended by `letrec` bindings
pub fn eval_letrec_bindings<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_letrec_bindings(init_env, ext_env): [2] {
            let not_eq = sub(ext_env, init_env);
            if !not_eq {
                let env_tag = Tag::Env;
                return (env_tag, init_env)
            }
            let (_var_tag, _var, val_tag, val, ext_env) = load(ext_env);
            let fix_tag = Tag::Fix;
            // Safety check: letrec bindings must be fixed points
            assert_eq!(fix_tag, val_tag);
            let nil_env = 0;
            // Fixed points are closed expressions, so we can choose an empty environment
            let (res_tag, res) = call(eval, val_tag, val, nil_env);
            match res_tag {
                Tag::Err => {
                    return (res_tag, res)
                }
            };
            let (res_tag, res) = call(eval_letrec_bindings, init_env, ext_env);
            return (res_tag, res)
        }
    )
}

pub fn eval_letrec<F: AbstractField>() -> FuncE<F> {
    func!(
        partial fn eval_letrec(binds_tag, binds, body_tag, body, env): [2] {
            // extend `env` with the bindings from the mutual env
            let (ext_env_tag, ext_env) = call(extend_env_with_mutuals, binds_tag, binds, binds, env, env);
            match ext_env_tag {
                Tag::Err => {
                    return (ext_env_tag, ext_env)
                }
            };
            // preemptively evaluate each binding value for side-effects, error detection and memoization
            let (res_tag, res) = call(eval_letrec_bindings, env, ext_env);
            match res_tag {
                Tag::Err => {
                    return (res_tag, res)
                }
            };
            // no error found... evaluate the body with the extended env
            let (res_tag, res) = call(eval_begin, body_tag, body, ext_env);
            return (res_tag, res)
        }
    )
}

pub fn apply<F: AbstractField>(digests: &SymbolsDigests<F>) -> FuncE<F> {
    func!(
        partial fn apply(head_tag, head, args_tag, args, args_env): [2] {
            // Constants, tags, etc
            let err_tag = Tag::Err;
            let fun_tag = Tag::Fun;
            // Expression must be a function
            let head_not_fun = sub(head_tag, fun_tag);
            if head_not_fun {
                let err = EvalErr::ApplyNonFunc;
                return (err_tag, err)
            }

            let (params_tag, params, body_tag, body, func_env) = load(head);

            match params_tag {
                InternalTag::Nil => {
                    let (res_tag, res) = call(eval_begin, body_tag, body, func_env);
                    match res_tag {
                        Tag::Err => {
                            return (res_tag, res)
                        }
                    };
                    match args_tag {
                        InternalTag::Nil => {
                            return (res_tag, res)
                        }
                        Tag::Cons => {
                            // Oversaturated application
                            let (app_res_tag, app_res) = call(apply, res_tag, res, args_tag, args, args_env);
                            return (app_res_tag, app_res)
                        }
                    };
                    let err = EvalErr::ArgsNotList;
                    return (err_tag, err)
                }
                Tag::Cons => {
                    // check if the only params left are "&rest <var>"
                    let (param_tag, param, rest_params_tag, rest_params) = load(params);
                    match param_tag {
                        Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                            let rest_sym = digests.lurk_symbol_ptr("&rest");
                            let is_not_rest_sym = sub(param, rest_sym);
                            if !is_not_rest_sym {
                                // check whether the next param in the list is a variable
                                match rest_params_tag {
                                    InternalTag::Nil => {
                                        let err = EvalErr::ParamInvalidRest;
                                        return (err_tag, err)
                                    }
                                    Tag::Cons => {
                                        let (param_tag, param, rest_params_tag, rest_params) = load(rest_params);
                                        match param_tag {
                                            Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                                                // check that there are no remaining arguments after the variable
                                                match rest_params_tag {
                                                    InternalTag::Nil => {
                                                        // evaluate all the remaining arguments and collect into a list
                                                        let (arg_tag, arg) = call(eval_list, args_tag, args, args_env);
                                                        match arg_tag {
                                                            Tag::Err => {
                                                                return (arg_tag, arg)
                                                            }
                                                        };

                                                        // and store it in the environment
                                                        let ext_env = store(param_tag, param, arg_tag, arg, func_env);
                                                        let ext_fun = store(rest_params_tag, rest_params, body_tag, body, ext_env);
                                                        let nil_tag = InternalTag::Nil;
                                                        let nil = digests.lurk_symbol_ptr("nil");
                                                        let (res_tag, res) = call(apply, fun_tag, ext_fun, nil_tag, nil, args_env);

                                                        return (res_tag, res)
                                                    }
                                                };
                                                let err = EvalErr::ParamInvalidRest;
                                                return (err_tag, err)
                                            }
                                        };
                                        let err = EvalErr::IllegalBindingVar;
                                        return (err_tag, err)
                                    }
                                };
                                let err = EvalErr::ParamsNotList;
                                return (err_tag, err)
                            }
                            // --- DUPLICATED APPLY BLOCK START ---
                            match args_tag {
                                InternalTag::Nil => {
                                    // Undersaturated application
                                    return (head_tag, head)
                                }
                                Tag::Cons => {
                                    let (arg_tag, arg, rest_args_tag, rest_args) = load(args);
                                    match param_tag {
                                        Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                                            // evaluate the argument
                                            let (arg_tag, arg) = call(eval, arg_tag, arg, args_env);
                                            match arg_tag {
                                                Tag::Err => {
                                                    return (arg_tag, arg)
                                                }
                                            };
                                            // and store it in the environment
                                            let ext_env = store(param_tag, param, arg_tag, arg, func_env);
                                            let ext_fun = store(rest_params_tag, rest_params, body_tag, body, ext_env);
                                            let (res_tag, res) = call(apply, fun_tag, ext_fun, rest_args_tag, rest_args, args_env);

                                            return (res_tag, res)
                                        }
                                    };
                                    let err = EvalErr::IllegalBindingVar;
                                    return (err_tag, err)
                                }
                            };
                            let err = EvalErr::ArgsNotList;
                            return (err_tag, err)
                            // --- DUPLICATED APPLY BLOCK END ---
                        }
                    };
                    // --- DUPLICATED APPLY BLOCK START ---
                    match args_tag {
                        InternalTag::Nil => {
                            // Undersaturated application
                            return (head_tag, head)
                        }
                        Tag::Cons => {
                            let (arg_tag, arg, rest_args_tag, rest_args) = load(args);
                            match param_tag {
                                Tag::Sym, Tag::Builtin, Tag::Coroutine => {
                                    // evaluate the argument
                                    let (arg_tag, arg) = call(eval, arg_tag, arg, args_env);
                                    match arg_tag {
                                        Tag::Err => {
                                            return (arg_tag, arg)
                                        }
                                    };
                                    // and store it in the environment
                                    let ext_env = store(param_tag, param, arg_tag, arg, func_env);
                                    let ext_fun = store(rest_params_tag, rest_params, body_tag, body, ext_env);
                                    let (res_tag, res) = call(apply, fun_tag, ext_fun, rest_args_tag, rest_args, args_env);

                                    return (res_tag, res)
                                }
                            };
                            let err = EvalErr::IllegalBindingVar;
                            return (err_tag, err)
                        }
                    };
                    let err = EvalErr::ArgsNotList;
                    return (err_tag, err)
                    // --- DUPLICATED APPLY BLOCK END ---
                }
            };
            let err = EvalErr::ParamsNotList;
            return (err_tag, err)
        }
    )
}

pub fn env_lookup<F: AbstractField>() -> FuncE<F> {
    func!(
        fn env_lookup(x_tag_digest: [9], env): [2] {
            if !env {
                let err_tag = Tag::Err;
                let err = EvalErr::UnboundVar;
                return (err_tag, err)
            }
            let (y_tag, y, val_tag, val, tail_env) = load(env);
            let y_digest: [8] = load(y);
            let y_tag_digest: [9] = (y_tag, y_digest);
            let not_eq = sub(x_tag_digest, y_tag_digest);
            if !not_eq {
                return (val_tag, val)
            }
            let (res_tag, res) = call(env_lookup, x_tag_digest, tail_env);
            return (res_tag, res)
        }
    )
}

#[cfg(test)]
mod test {
    use expect_test::{expect, Expect};
    use p3_baby_bear::BabyBear as F;
    use p3_field::AbstractField;
    use rustc_hash::FxHashSet;
    use strum::IntoEnumIterator;

    use crate::{
        air::debug::debug_constraints_collecting_queries,
        core::{state::State, zstore::ZPtr},
        lair::{
            execute::{QueryRecord, Shard},
            func_chip::FuncChip,
            List,
        },
    };

    use super::*;

    #[test]
    fn test_widths() {
        let (toplevel, ..) = &build_lurk_toplevel_native();

        let lurk_main = FuncChip::from_name("lurk_main", toplevel);
        let preallocate_symbols = FuncChip::from_name("preallocate_symbols", toplevel);
        let eval_coroutine_expr = FuncChip::from_name("eval_coroutine_expr", toplevel);
        let eval = FuncChip::from_name("eval", toplevel);
        let eval_builtin_expr = FuncChip::from_name("eval_builtin_expr", toplevel);
        let eval_bind_builtin = FuncChip::from_name("eval_bind_builtin", toplevel);
        let eval_env_builtin = FuncChip::from_name("eval_env_builtin", toplevel);
        let eval_apply_builtin = FuncChip::from_name("eval_apply_builtin", toplevel);
        let eval_opening_unop = FuncChip::from_name("eval_opening_unop", toplevel);
        let eval_hide = FuncChip::from_name("eval_hide", toplevel);
        let eval_unop = FuncChip::from_name("eval_unop", toplevel);
        let eval_binop_num = FuncChip::from_name("eval_binop_num", toplevel);
        let eval_binop_misc = FuncChip::from_name("eval_binop_misc", toplevel);
        let eval_begin = FuncChip::from_name("eval_begin", toplevel);
        let eval_list = FuncChip::from_name("eval_list", toplevel);
        let eval_let = FuncChip::from_name("eval_let", toplevel);
        let eval_letrec = FuncChip::from_name("eval_letrec", toplevel);
        let extend_env_with_mutuals = FuncChip::from_name("extend_env_with_mutuals", toplevel);
        let eval_letrec_bindings = FuncChip::from_name("eval_letrec_bindings", toplevel);
        let coerce_if_sym = FuncChip::from_name("coerce_if_sym", toplevel);
        let open_comm = FuncChip::from_name("open_comm", toplevel);
        let equal = FuncChip::from_name("equal", toplevel);
        let equal_inner = FuncChip::from_name("equal_inner", toplevel);
        let car_cdr = FuncChip::from_name("car_cdr", toplevel);
        let apply = FuncChip::from_name("apply", toplevel);
        let env_lookup = FuncChip::from_name("env_lookup", toplevel);
        let ingress = FuncChip::from_name("ingress", toplevel);
        let egress = FuncChip::from_name("egress", toplevel);
        let hash3 = FuncChip::from_name("hash3", toplevel);
        let hash4 = FuncChip::from_name("hash4", toplevel);
        let hash5 = FuncChip::from_name("hash5", toplevel);
        let u64_add = FuncChip::from_name("u64_add", toplevel);
        let u64_sub = FuncChip::from_name("u64_sub", toplevel);
        let u64_mul = FuncChip::from_name("u64_mul", toplevel);
        let u64_divrem = FuncChip::from_name("u64_divrem", toplevel);
        let u64_lessthan = FuncChip::from_name("u64_lessthan", toplevel);
        let u64_iszero = FuncChip::from_name("u64_iszero", toplevel);
        let digest_equal = FuncChip::from_name("digest_equal", toplevel);
        let big_num_lessthan = FuncChip::from_name("big_num_lessthan", toplevel);

        let expect_eq = |computed: usize, expected: Expect| {
            expected.assert_eq(&computed.to_string());
        };
        expect_eq(lurk_main.width(), expect!["97"]);
        expect_eq(preallocate_symbols.width(), expect!["188"]);
        expect_eq(eval_coroutine_expr.width(), expect!["10"]);
        expect_eq(eval.width(), expect!["79"]);
        expect_eq(eval_builtin_expr.width(), expect!["148"]);
        expect_eq(eval_bind_builtin.width(), expect!["110"]);
        expect_eq(eval_env_builtin.width(), expect!["81"]);
        expect_eq(eval_apply_builtin.width(), expect!["79"]);
        expect_eq(eval_opening_unop.width(), expect!["97"]);
        expect_eq(eval_hide.width(), expect!["115"]);
        expect_eq(eval_unop.width(), expect!["78"]);
        expect_eq(eval_binop_num.width(), expect!["107"]);
        expect_eq(eval_binop_misc.width(), expect!["70"]);
        expect_eq(eval_begin.width(), expect!["68"]);
        expect_eq(eval_list.width(), expect!["72"]);
        expect_eq(eval_let.width(), expect!["94"]);
        expect_eq(eval_letrec.width(), expect!["66"]);
        expect_eq(extend_env_with_mutuals.width(), expect!["53"]);
        expect_eq(eval_letrec_bindings.width(), expect!["66"]);
        expect_eq(coerce_if_sym.width(), expect!["9"]);
        expect_eq(open_comm.width(), expect!["50"]);
        expect_eq(equal.width(), expect!["86"]);
        expect_eq(equal_inner.width(), expect!["58"]);
        expect_eq(car_cdr.width(), expect!["61"]);
        expect_eq(apply.width(), expect!["114"]);
        expect_eq(env_lookup.width(), expect!["52"]);
        expect_eq(ingress.width(), expect!["104"]);
        expect_eq(egress.width(), expect!["81"]);
        expect_eq(hash3.width(), expect!["493"]);
        expect_eq(hash4.width(), expect!["655"]);
        expect_eq(hash5.width(), expect!["815"]);
        expect_eq(u64_add.width(), expect!["53"]);
        expect_eq(u64_sub.width(), expect!["53"]);
        expect_eq(u64_mul.width(), expect!["85"]);
        expect_eq(u64_divrem.width(), expect!["166"]);
        expect_eq(u64_lessthan.width(), expect!["44"]);
        expect_eq(u64_iszero.width(), expect!["26"]);
        expect_eq(digest_equal.width(), expect!["38"]);
        expect_eq(big_num_lessthan.width(), expect!["78"]);
    }

    #[test]
    fn test_ingress_egress() {
        let (toplevel, zstore, lang_symbols) = &build_lurk_toplevel_native();

        let ingress = toplevel.func_by_name("ingress");
        let egress = toplevel.func_by_name("egress");
        let hash4_chip = FuncChip::from_name("hash4", toplevel);

        let state = State::init_lurk_state().rccell();

        let assert_ingress_egress_correctness = |code| {
            let zstore = &mut zstore.clone();
            let ZPtr { tag, digest } = zstore.read_with_state(code, state.clone(), lang_symbols);
            let tag = tag.to_field();

            let digest: List<_> = digest.into();

            let mut queries = QueryRecord::new(toplevel);
            queries.inject_inv_queries("hash4", toplevel, &zstore.hashes4);

            let mut ingress_args = [F::zero(); 16];
            ingress_args[0] = tag;
            ingress_args[8..].copy_from_slice(&digest);

            let ingress_out = toplevel
                .execute(ingress, &ingress_args, &mut queries, None)
                .unwrap();

            let egress_out = toplevel
                .execute(egress, &ingress_out, &mut queries, None)
                .unwrap();

            assert_eq!(tag, egress_out[0]);
            assert_eq!(
                digest.as_ref(),
                &egress_out[1..],
                "ingress -> egress doesn't roundtrip"
            );

            let shards = Shard::new(queries.clone());
            assert_eq!(shards.len(), 1);
            let shard = &shards[0];
            let hash4_trace = hash4_chip.generate_trace(shard);
            debug_constraints_collecting_queries(&hash4_chip, &[], None, &hash4_trace);
        };

        assert_ingress_egress_correctness("~()");
        assert_ingress_egress_correctness("~:()");
        assert_ingress_egress_correctness("()");
        assert_ingress_egress_correctness("'c'");
        assert_ingress_egress_correctness("5u64");
        assert_ingress_egress_correctness("\"\"");
        assert_ingress_egress_correctness("\"a\"");
        assert_ingress_egress_correctness("\"test string\"");
        assert_ingress_egress_correctness(".a");
        assert_ingress_egress_correctness("TestSymbol");
        assert_ingress_egress_correctness(":keyword");
        assert_ingress_egress_correctness("(+ 1 2)");
        assert_ingress_egress_correctness("(a 'b c)");
    }

    #[test]
    fn test_strum() {
        assert_eq!(2, InternalTag::iter().count());
    }

    #[test]
    fn test_disjoint_internal_tags() {
        let tag_fields: FxHashSet<F> = Tag::iter().map(Tag::to_field).collect();
        for internal_tag in InternalTag::iter() {
            assert!(!tag_fields.contains(&internal_tag.to_field()));
        }
    }
}
