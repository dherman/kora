extern crate syn;

#[macro_use]
extern crate quote;

use std::env;
use std::path::{PathBuf, Path as FilePath};

use syn::*;
use quote::Tokens;

struct Context {
    location_ident: Ident,
    location_expr: Expr,
    location_type: Ty,
    deref_self_expr: Expr,
}

impl Context {
    pub fn new() -> Self {
        Context {
            location_ident: Ident::from("location"),

            location_expr: Expr::from(ExprKind::Path(None, Path::from("location"))),

            location_type: Ty::Path(None, Path {
                global: false,
                segments: vec![PathSegment {
                    ident: Ident::from("Option"),
                    parameters: PathParameters::AngleBracketed(AngleBracketedParameterData {
                        lifetimes: vec![],
                        types: vec![Ty::Path(None, Path::from("Span"))],
                        bindings: vec![]
                    })
                }]
            }),

            deref_self_expr: Expr::from(ExprKind::Unary(
                UnOp::Deref,
                Box::new(Expr::from(ExprKind::Path(None, Path::from("self"))))
            ))
        }
    }

    fn expand_tracking_ref_data(&self, path: Path, data: &VariantData, mutability: Mutability) -> Arm {
        let location_pat = Pat::Ident(BindingMode::ByRef(mutability), self.location_ident.clone(), None);

        let expr = self.location_expr.clone();

        let (pat, expr) = match *data {
            VariantData::Struct(_) => (
                Pat::Struct(path, vec![FieldPat {
                    ident: self.location_ident.clone(),
                    pat: Box::new(location_pat),
                    is_shorthand: true
                }], true),
                if data.fields().iter().any(|field| field.ident.as_ref() == Some(&self.location_ident) && field.ty == self.location_type) {
                    expr
                } else {
                    panic!("Struct does not containt `location: Option<Span>`")
                }
            ),
            VariantData::Tuple(ref fields) => (
                Pat::TupleStruct(path, {
                    let mut v = Vec::with_capacity(fields.len());
                    v.push(location_pat);
                    v.extend(std::iter::repeat(Pat::Wild).take(fields.len() - 1));
                    v
                }, None),
                if data.fields()[0].ty == self.location_type {
                    expr
                } else {
                    Expr::from(ExprKind::MethodCall(
                        Ident::from(if mutability == Mutability::Immutable { "tracking_ref" } else { "tracking_mut" }),
                        vec![],
                        vec![expr]
                    ))
                }
            ),
            VariantData::Unit => panic!("Empty unit is not trackable")
        };

        Arm {
            attrs: vec![],
            pats: vec![pat],
            guard: None,
            body: Box::new(expr)
        }
    }

    pub fn expand_tracking_ref(&self, ast: &MacroInput, mutability: Mutability) -> Tokens {
        // Helper is provided for handling complex generic types correctly and effortlessly
        let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();

        let (impl_name, method) = if mutability == Mutability::Immutable {
            (Ident::from("TrackingRef"), quote! {
                tracking_ref(&self) -> &Option<Span>
            })
        } else {
            (Ident::from("TrackingMut"), quote! {
                tracking_mut(&mut self) -> &mut Option<Span>
            })
        };

        // Used in the quasi-quotation below as `#name`
        let name = &ast.ident;

        let body = Expr::from(ExprKind::Match(
            Box::new(self.deref_self_expr.clone()),
            match ast.body {
                Body::Struct(ref data) => {
                    vec![self.expand_tracking_ref_data(Path::from(name.clone()), data, mutability)]
                },
                Body::Enum(ref variants) => {
                    variants.iter().map(|var| {
                        let path = Path {
                            global: false,
                            segments: vec![
                                PathSegment::from(name.clone()),
                                PathSegment::from(var.ident.clone())
                            ]
                        };
                        self.expand_tracking_ref_data(path, &var.data, mutability)
                    }).collect()
                }
            }
        ));

        quote! {
            // The generated impl
            impl #impl_generics #impl_name for #name #ty_generics #where_clause {
                fn #method {
                    #body
                }
            }
        }
    }

    fn expand_untrack_data(&self, path: Path, data: &VariantData) -> Arm {
        let (pat, idents) = match *data {
            VariantData::Struct(ref fields) => {
                let mut field_pats = Vec::with_capacity(fields.len());
                let mut idents = Vec::with_capacity(fields.len());

                for field in fields {
                    let ident = field.ident.as_ref().unwrap();

                    field_pats.push(FieldPat {
                        ident: ident.clone(),
                        pat: Box::new(Pat::Ident(BindingMode::ByRef(Mutability::Mutable), ident.clone(), None)),
                        is_shorthand: true
                    });

                    idents.push(ident.clone());
                }

                (Pat::Struct(path, field_pats, false), idents)
            },
            VariantData::Tuple(ref fields) => {
                let mut pats = Vec::with_capacity(fields.len());
                let mut idents = Vec::with_capacity(fields.len());

                for i in 0..fields.len() {
                    let ident = Ident::from(format!("_f{}", i));

                    pats.push(Pat::Ident(BindingMode::ByRef(Mutability::Mutable), ident.clone(), None));

                    idents.push(ident);
                }

                (Pat::TupleStruct(path, pats, None), idents)
            },
            VariantData::Unit => {
                (Pat::Path(None, path), vec![])
            }
        };

        let expr = Expr::from(ExprKind::Block(BlockCheckMode::Default, Block {
            stmts: idents.into_iter().map(|ident| {
                Stmt::Semi(Box::new(Expr::from(ExprKind::MethodCall(
                    Ident::from("untrack"),
                    vec![],
                    vec![Expr::from(ExprKind::Path(None, Path::from(ident)))]
                ))))
            }).collect()
        }));

        Arm {
            attrs: vec![],
            pats: vec![pat],
            guard: None,
            body: Box::new(expr)
        }
    }

    pub fn expand_untrack(&self, ast: &MacroInput) -> Tokens {
        let mut generics = ast.generics.clone();

        let bound = TyParamBound::Trait(PolyTraitRef {
            bound_lifetimes: vec![],
            trait_ref: Path::from("Untrack")
        }, TraitBoundModifier::None);

        for ty in &mut generics.ty_params {
            ty.bounds.push(bound.clone());
        }

        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

        let name = &ast.ident;

        let body = Expr::from(ExprKind::Match(
            Box::new(self.deref_self_expr.clone()),
            match ast.body {
                Body::Struct(ref data) => {
                    vec![self.expand_untrack_data(Path::from(name.clone()), data)]
                },
                Body::Enum(ref variants) => {
                    variants.iter().map(|var| {
                        let path = Path {
                            global: false,
                            segments: vec![
                                PathSegment::from(name.clone()),
                                PathSegment::from(var.ident.clone())
                            ]
                        };
                        self.expand_untrack_data(path, &var.data)
                    }).collect()
                }
            }
        ));

        quote! {
            // The generated impl
            impl #impl_generics Untrack for #name #ty_generics #where_clause {
                fn untrack(&mut self) {
                    #body
                }
            }
        }
    }
}

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_path = FilePath::new(&out_dir);

    let src_path = {
        let mut buf = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        buf.push("src");
        buf
    };

    let context = Context::new();

    let registry = {
        let mut registry = Registry::new();

        registry.add_derive("TrackingRef", |input| {
            let tokens = context.expand_tracking_ref(&input, Mutability::Immutable);
            Ok(Expanded {
                new_items: parse_items(&tokens.to_string())?,
                original: Some(input),
            })
        });

        registry.add_derive("TrackingMut", |input| {
            let tokens = context.expand_tracking_ref(&input, Mutability::Mutable);
            Ok(Expanded {
                new_items: parse_items(&tokens.to_string())?,
                original: Some(input),
            })
        });

        registry.add_derive("Untrack", |input| {
            let tokens = context.expand_untrack(&input);
            Ok(Expanded {
                new_items: parse_items(&tokens.to_string())?,
                original: Some(input),
            })
        });

        registry
    };

    for entry in src_path.read_dir().unwrap() {
        let path = entry.unwrap().path();
        let file_name = path.file_name().unwrap();
        if file_name != "lib.rs" {
            let dest = out_path.join(file_name);
            registry.expand_file(&path, &dest).unwrap();
        }
    }
}